//! Keeping the helper alive while the shell finishes what a command started.
//!
//! Many commands return before they are done. Properties opens its dialog on a thread of its
//! own and returns at once; "Compressed (zipped) folder" zips in the background; Copy leaves
//! a data object on the clipboard that lives in this process. If the helper exited when the
//! command returned, the dialog would vanish, the zip would be cut short and the clipboard
//! would empty. So the helper:
//! - registers a process reference, which background work that knows about it (anything
//!   started with `SHCreateThread(CTF_PROCESS_REF)`) holds while it runs;
//! - watches for windows the command opens, since Properties takes no reference, and waits
//!   for them to close;
//! - renders the clipboard's contents so they outlive the process.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::UI::Shell::SHSetInstanceExplorer;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::System::DataExchange::GetClipboardOwner;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::Win32::System::Ole::OleFlushClipboard;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetWindowThreadProcessId, IsWindowVisible, MSG,
    MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT,
    TranslateMessage, WM_NULL,
};
use windows_sys::w;

/// How long after a command returns, and after its last window closes, to keep watching for
/// another window. Properties opens its dialog a moment after returning.
pub const SETTLE: Duration = Duration::from_millis(1500);
/// How long to keep waiting for a reference to be released while the process shows no window.
/// Work that runs this long normally shows a progress dialog; a reference held longer than
/// this without one has most likely been leaked by an extension.
const IDLE_LIMIT: Duration = Duration::from_secs(60);
/// How often the wait looks for windows.
const POLL: u32 = 100;

const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002_u32 as i32);
const E_POINTER: HRESULT = HRESULT(0x8000_4003_u32 as i32);

/// References held on the process: one by `Reference` itself, plus any the shell takes.
static COUNT: AtomicU32 = AtomicU32::new(0);
/// The thread that waits, woken with a message when the last reference goes.
static WAITER: AtomicU32 = AtomicU32::new(0);

/// A COM object with nothing but a reference count. It is a static, so it is never freed.
#[repr(C)]
struct Counter {
    vtable: &'static IUnknown_Vtbl,
}

static VTABLE: IUnknown_Vtbl = IUnknown_Vtbl { QueryInterface: query_interface, AddRef: add_ref, Release: release };
static COUNTER: Counter = Counter { vtable: &VTABLE };

unsafe extern "system" fn query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM guarantees `iid` points to a GUID and `out` to writable storage.
    unsafe {
        if !iid.is_null() && *iid == IUnknown::IID {
            add_ref(this);
            *out = this;
            HRESULT(0)
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
    COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
    let left = COUNT.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
    if left == 0 {
        // SAFETY: plain FFI call; a thread that has exited simply doesn't get the message.
        unsafe { PostThreadMessageW(WAITER.load(Ordering::SeqCst), WM_NULL, 0, 0) };
    }
    left
}

/// The process reference the shell's background work holds on to.
pub struct Reference(IUnknown);

impl Reference {
    /// Registers the reference. Call once, on the thread that will wait.
    pub fn install() -> Self {
        // SAFETY: plain FFI call.
        WAITER.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        COUNT.store(1, Ordering::SeqCst);
        // SAFETY: `COUNTER` is a static COM object whose first field is its vtable, and the
        // count of 1 just stored is the reference this `IUnknown` owns.
        let unknown = unsafe { IUnknown::from_raw(std::ptr::from_ref(&COUNTER).cast_mut().cast()) };
        // Older shell code asks shell32 for the reference, newer code asks shcore, so it is
        // registered with both. Neither keeps a count of its own: the object is a static.
        // SAFETY: the pointer stays valid for the life of the process. SetProcessReference is
        // exported by name from shcore.dll with this signature (documented in shlwapi.h).
        unsafe {
            SHSetInstanceExplorer(&unknown);
            let shcore = LoadLibraryExW(w!("shcore.dll"), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if !shcore.is_null()
                && let Some(set) = GetProcAddress(shcore, c"SetProcessReference".as_ptr().cast())
            {
                let set: unsafe extern "system" fn(*mut c_void) = std::mem::transmute(set);
                set(unknown.as_raw());
            }
        }
        Self(unknown)
    }

    /// Drops this reference and pumps messages until the shell has released its own and no
    /// window of this process has been open for `settle`, so dialogs and background work a
    /// command started can finish. `settle` is zero when no command ran.
    pub fn wait(self, settle: Duration) {
        drop(self.0);
        let mut last_window = Instant::now();
        let mut last_busy = Instant::now();
        loop {
            pump();
            let window = shows_a_window();
            let held = COUNT.load(Ordering::SeqCst) > 0;
            if window {
                last_window = Instant::now();
            }
            if window || held {
                last_busy = Instant::now();
            }
            if !held && !window && last_busy.elapsed() >= settle {
                break;
            }
            if held && !window && last_window.elapsed() >= IDLE_LIMIT {
                break;
            }
            // SAFETY: waits on no handles, only for messages, for a short while.
            unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, POLL, QS_ALLINPUT) };
        }
    }
}

/// Dispatches every message waiting for this thread.
fn pump() {
    // SAFETY: MSG is a plain struct the calls fill in.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Whether any window of this process is visible, such as a dialog a command opened.
fn shows_a_window() -> bool {
    unsafe extern "system" fn visit(hwnd: HWND, found: LPARAM) -> i32 {
        let mut process = 0;
        // SAFETY: `hwnd` comes from EnumWindows and `found` is the bool passed below.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process);
            if process == GetCurrentProcessId() && IsWindowVisible(hwnd) != 0 {
                *(found as *mut bool) = true;
                return 0;
            }
        }
        1
    }
    let mut found = false;
    // SAFETY: the callback only runs during the call, while `found` is alive.
    unsafe { EnumWindows(Some(visit), (&raw mut found) as LPARAM) };
    found
}

/// Renders what this process put on the clipboard, so it stays there after the process exits.
pub fn flush_clipboard() {
    // SAFETY: plain FFI calls; OleFlushClipboard acts only on data this thread placed there.
    unsafe {
        let owner = GetClipboardOwner();
        if owner.is_null() {
            return;
        }
        let mut process = 0;
        GetWindowThreadProcessId(owner, &mut process);
        if process == GetCurrentProcessId() {
            OleFlushClipboard();
        }
    }
}
