//! Drag and drop of files, in both directions.
//!
//! Out: a drag that starts on selected rows becomes a native shell drag (`SHDoDragDrop` over a
//! shell data object), so Explorer, the desktop, mail and editors receive real files, with the
//! shell's drag image and Windows' own copy, move and link rules. The drag is modal and must run
//! on the window's thread, so the request is posted to the window as a message: run from inside
//! the event loop's handler instead, it would hold up every window event until it ended,
//! including the drop events of a drag that ends in Kestrel itself. Windows' drag loop itself
//! services the window's other messages only as mouse or keyboard input arrives, so while a
//! drag out of Kestrel is held perfectly still, the window's own updates wait for the next
//! movement.
//!
//! In: Tauri's drop handling reports where files are dragged and dropped; the front end works
//! out which folder is under the cursor and calls `drop_items`, which copies or moves the files
//! through the shell's copy engine, choosing between the two as Explorer does.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{DragDropEvent, State, Window, WindowEvent};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_USEDEFAULTCURSORS};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFindLastID, ILFree, SHCreateDataObject, SHDoDragDrop};
use windows::core::{BOOL, HRESULT, HSTRING, implement};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, PostMessageW, RegisterWindowMessageW, SM_SWAPBUTTON, WM_KEYDOWN, WM_MOUSEMOVE,
};

use crate::engine::Engine;
use crate::fileops::{self, Done, Options, Request, Transfer};
use crate::undo::Journal;

/// Most items one drag carries. Each becomes a shell item, so dragging a million search
/// results would stall the window for seconds before the drag began.
const MAX_DRAG_ITEMS: u64 = 20_000;
/// How long the keys read at a drop stay attached to it. The front end asks for the drop to be
/// carried out a few milliseconds later.
const DROP_KEYS_VALID: Duration = Duration::from_secs(5);
/// Identifies this module's handler among the window's subclasses.
const SUBCLASS_ID: usize = 0x4B44_5244;

/// The modifier keys that decide between copying and moving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Keys {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
}

impl Keys {
    /// The keys held right now, read from the keyboard itself rather than from a window's
    /// message queue, which an OLE drag loop does not keep current.
    fn now() -> Self {
        // SAFETY: plain FFI calls; a negative result means the key is down.
        let down = |key: u16| unsafe { GetAsyncKeyState(i32::from(key)) } < 0;
        Self { ctrl: down(VK_CONTROL), shift: down(VK_SHIFT) }
    }
}

// ---------- deciding what a drop does ----------

/// The volume a path is on, to tell whether a move stays on one drive: `c:` for drive paths and
/// `\\server\share` for network paths, lowercased. `None` when the path names neither.
pub fn volume(path: &str) -> Option<String> {
    let path = path.replace('/', "\\");
    let path = match path.strip_prefix(r"\\?\") {
        Some(rest) => rest.strip_prefix(r"UNC\").map_or_else(|| rest.to_owned(), |unc| format!(r"\\{unc}")),
        None => path,
    };
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(path[..2].to_ascii_lowercase());
    }
    let mut parts = path.strip_prefix(r"\\")?.split('\\').filter(|part| !part.is_empty());
    let (server, share) = (parts.next()?, parts.next()?);
    Some(format!(r"\\{server}\{share}").to_lowercase())
}

/// Whether dropping `sources` on `destination` copies or moves them, by Explorer's rules: Ctrl
/// copies and Shift moves; with neither, items move within a drive and are copied to another.
/// Explorer makes shortcuts when both are held; Kestrel doesn't make shortcuts, and copying is
/// the reading of that gesture that loses nothing.
pub fn transfer_kind(sources: &[String], destination: &str, keys: Keys) -> Transfer {
    if keys.ctrl {
        return Transfer::Copy;
    }
    if keys.shift {
        return Transfer::Move;
    }
    let target = volume(destination);
    if target.is_some() && sources.iter().all(|source| volume(source) == target) {
        Transfer::Move
    } else {
        Transfer::Copy
    }
}

/// A path in a form that compares equal for the same folder: backslashes, no trailing
/// separator, lowercase.
fn comparable(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// What dropping `sources` on `destination` does: how, and with which of the items. Items
/// already in the destination stay where they are, and a folder can't go into itself or one of
/// its own subfolders, so those are left out. `None` means nothing is left to do.
pub fn plan(sources: &[String], destination: &str, keys: Keys) -> Option<(Transfer, Vec<String>)> {
    let target = comparable(destination);
    let taking_part = |source: &&String| {
        let source = comparable(source);
        let parent = Path::new(&source).parent().and_then(Path::to_str).map(comparable);
        parent.as_deref() != Some(target.as_str())
            && target != source
            && !target.starts_with(&format!("{source}\\"))
    };
    let sources: Vec<String> = sources.iter().filter(taking_part).cloned().collect();
    if sources.is_empty() {
        return None;
    }
    Some((transfer_kind(&sources, destination, keys), sources))
}

fn effect_name(kind: Transfer) -> &'static str {
    match kind {
        Transfer::Copy => "copy",
        Transfer::Move => "move",
    }
}

// ---------- dropping in ----------

/// Keys read at the moment of the last drop on the window, and when.
static DROP_KEYS: Mutex<Option<(Instant, Keys)>> = Mutex::new(None);

/// Records which modifier keys are held when files are dropped on the window. Called for every
/// window event, at the drop itself; the front end asks for the drop to be carried out a moment
/// later, by which time the keys may have been let go.
pub fn on_window_event(event: &WindowEvent) {
    if let WindowEvent::DragDrop(DragDropEvent::Drop { .. }) = event {
        *DROP_KEYS.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), Keys::now()));
    }
}

/// The keys held at the last drop, or failing that, the keys held now.
fn keys_at_drop() -> Keys {
    match DROP_KEYS.lock().unwrap_or_else(PoisonError::into_inner).take() {
        Some((at, keys)) if at.elapsed() < DROP_KEYS_VALID => keys,
        _ => Keys::now(),
    }
}

/// Keys named by the end-to-end tests, whose synthetic drops can't hold real ones. Only debug
/// builds listen; a release build always reads the keyboard.
fn test_keys(keys: Option<Keys>) -> Option<Keys> {
    if cfg!(debug_assertions) { keys } else { None }
}

#[derive(Debug, Serialize)]
pub struct Dropped {
    /// `copy`, `move`, or `none` when there was nothing to do or the user cancelled.
    pub effect: &'static str,
    /// Names of the items now in the destination, to select there.
    pub names: Vec<String>,
}

impl Dropped {
    const NOTHING: Dropped = Dropped { effect: "none", names: Vec::new() };
}

/// What a drop on `destination` would do with the keys held now: `copy`, `move` or `none`.
/// The front end shows it while files are dragged over a folder.
#[tauri::command(async)]
pub fn drop_effect(paths: Vec<String>, destination: String, keys: Option<Keys>) -> &'static str {
    let keys = test_keys(keys).unwrap_or_else(Keys::now);
    plan(&paths, &destination, keys).map_or("none", |(kind, _)| effect_name(kind))
}

/// Copies or moves files dropped on the window into `destination`, deciding which by the keys
/// held at the drop and the drives involved. The operation goes into the undo journal, as a
/// paste does.
#[tauri::command(async)]
pub fn drop_items(
    window: Window,
    journal: State<'_, Journal>,
    paths: Vec<String>,
    destination: String,
    keys: Option<Keys>,
) -> Result<Dropped, String> {
    let keys = test_keys(keys).unwrap_or_else(keys_at_drop);
    let Some((kind, sources)) = plan(&paths, &destination, keys) else { return Ok(Dropped::NOTHING) };

    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    let request = Request::Transfer { sources, destination, kind };
    let outcome = fileops::execute(request, Options { owner, quiet: false })?;
    journal.record(&outcome);
    // The names the items really got there, "name - Copy" and conflict renames included. A
    // cancelled drop may still have done part of its work, which is selected all the same.
    let names: Vec<String> = outcome
        .results()
        .filter_map(|done| match done {
            Done::Copied { to, .. } | Done::Moved { to, .. } => Some(fileops::file_name(to).to_owned()),
            _ => None,
        })
        .collect();
    let effect = if names.is_empty() { "none" } else { effect_name(kind) };
    Ok(Dropped { effect, names })
}

// ---------- dragging out ----------

/// An item id list from the shell, freed when dropped.
struct IdList(*mut ITEMIDLIST);

impl IdList {
    fn from_path(path: &str) -> Result<Self, String> {
        // The shell's parser rejects forward slashes.
        let path = path.replace('/', "\\");
        // SAFETY: the path is a valid wide string for the duration of the call.
        let list = unsafe { ILCreateFromPathW(&HSTRING::from(path.as_str())) };
        if list.is_null() { Err(format!("Can't find {path}")) } else { Ok(Self(list)) }
    }
}

impl Drop for IdList {
    fn drop(&mut self) {
        // SAFETY: the list came from ILCreateFromPathW and is freed exactly once.
        unsafe { ILFree(Some(self.0.cast_const())) };
    }
}

/// The folder every path is directly inside, if they share one.
fn common_parent(paths: &[String]) -> Option<String> {
    let parent = |path: &String| Path::new(path).parent().and_then(Path::to_str).map(str::to_owned);
    let first = parent(paths.first()?)?;
    paths.iter().all(|path| parent(path).is_some_and(|other| comparable(&other) == comparable(&first))).then_some(first)
}

/// A shell data object for files, like the one Explorer makes when files are dragged or
/// copied: drop targets can read it as shell items or as a plain `CF_HDROP` file list. COM must
/// be initialised on the calling thread.
pub fn data_object(paths: &[String]) -> Result<IDataObject, String> {
    let items = paths.iter().map(|path| IdList::from_path(path)).collect::<Result<Vec<_>, _>>()?;
    // Items in one folder are described relative to it, as Explorer does. Items from several
    // folders, as search results can be, are described by their full id lists relative to the
    // desktop, whose own id list is empty: just the two-byte terminator.
    let folder = common_parent(paths).map(|parent| IdList::from_path(&parent)).transpose()?;
    let desktop = [0u16; 1];
    // SAFETY: each item's id list is alive until the end of this function, and so is the last
    // id inside it.
    let children: Vec<*const ITEMIDLIST> = items
        .iter()
        .map(|item| if folder.is_some() { unsafe { ILFindLastID(item.0) }.cast_const() } else { item.0.cast_const() })
        .collect();
    let parent = folder.as_ref().map_or(desktop.as_ptr().cast::<ITEMIDLIST>(), |folder| folder.0.cast_const());
    // SAFETY: the parent and child id lists outlive the call, which copies them into the data
    // object.
    unsafe { SHCreateDataObject(Some(parent), Some(children.as_slice()), None::<&IDataObject>) }.map_err(|e| e.message())
}

/// How the end-to-end tests run a drag out, since they can't hold a mouse button down. Only
/// debug builds accept one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rehearsal {
    /// Build the data object on the window's thread but start no drag.
    Prepare,
    /// Run the shell's drag loop with a drop source that calls it off at once, so nothing can
    /// be dropped anywhere.
    Cancel,
    /// As `Cancel`, but keep the drag loop running for `HOLD` first, fed with mouse moves as a
    /// real drag is, so a test can check that the window goes on delivering events while a
    /// drag is in progress.
    Hold,
}

/// How long a `Hold` rehearsal keeps its drag loop running.
const HOLD: Duration = Duration::from_millis(2500);

/// A drop source that calls the drag off the first time it is asked whether to go on after
/// `until`. It never lets anything drop.
#[implement(IDropSource)]
struct CalledOff {
    until: Instant,
}

impl IDropSource_Impl for CalledOff_Impl {
    fn QueryContinueDrag(&self, _escape_pressed: BOOL, _keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if Instant::now() < self.until { windows::Win32::Foundation::S_OK } else { DRAGDROP_S_CANCEL }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// A drag waiting for the window's thread to pick it up.
struct DragJob {
    paths: Vec<String>,
    rehearsal: Option<Rehearsal>,
    done: mpsc::Sender<Result<&'static str, String>>,
}

static JOB: Mutex<Option<DragJob>> = Mutex::new(None);
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The message that tells the window to start the waiting drag. Registered by name, so it can't
/// collide with a message anything else sends the window.
fn drag_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        let name: Vec<u16> = "Kestrel.StartDrag".encode_utf16().chain([0]).collect();
        // SAFETY: `name` is NUL-terminated.
        unsafe { RegisterWindowMessageW(name.as_ptr()) }
    })
}

/// Lets the window start drags. Called once, on the thread that created the window.
pub fn install(window: &Window) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0;
    if drag_message() == 0 {
        return Err("Couldn't register the drag message".into());
    }
    // SAFETY: `hwnd` is the live main window, this runs on the thread that created it as
    // SetWindowSubclass requires, and `subclass` has the signature it expects.
    if unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0) } == 0 {
        return Err("Couldn't hook the window for dragging".into());
    }
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == drag_message() {
        let job = JOB.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(job) = job {
            let _ = job.done.send(run_drag(hwnd, &job.paths, job.rehearsal));
        }
        return 0;
    }
    // SAFETY: passes the message on unchanged to the next handler in the window's chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// Whether the primary mouse button is down. `GetAsyncKeyState` reads physical buttons, so a
/// swapped mouse is checked by its right button.
fn primary_button_down() -> bool {
    // SAFETY: plain FFI calls.
    unsafe {
        let button = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
        GetAsyncKeyState(i32::from(button)) < 0
    }
}

/// Runs a drag on the window's thread and returns once the files are dropped or the drag is
/// abandoned.
fn run_drag(hwnd: HWND, paths: &[String], rehearsal: Option<Rehearsal>) -> Result<&'static str, String> {
    // The drag must begin while the button that started it is still down. If the press ended
    // before this message arrived, a drag started now would drop the files at once wherever
    // the cursor rests.
    if rehearsal.is_none() && !primary_button_down() {
        return Ok("none");
    }
    let data = data_object(paths)?;
    let source: Option<IDropSource> = match rehearsal {
        None => None,
        Some(Rehearsal::Prepare) => return Ok("none"),
        Some(rehearsal @ (Rehearsal::Cancel | Rehearsal::Hold)) => {
            // The drag loop consults its drop source only as input arrives, and in a test none
            // does; a queued Escape makes it ask. A held drag is fed mouse moves meanwhile, as
            // a real one would be, without which the loop would not service other messages.
            let post = move |window: usize, message: u32, wparam: usize| {
                // SAFETY: posting to a window from any thread is allowed; the message carries no
                // pointers, and a window that has gone away just makes the call fail.
                unsafe { PostMessageW(window as HWND, message, wparam, 0) };
            };
            let window = hwnd as usize;
            let until = Instant::now() + if rehearsal == Rehearsal::Hold { HOLD } else { Duration::ZERO };
            if rehearsal == Rehearsal::Hold {
                std::thread::spawn(move || {
                    while Instant::now() < until + Duration::from_millis(100) {
                        post(window, WM_MOUSEMOVE, 0);
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    post(window, WM_KEYDOWN, usize::from(VK_ESCAPE));
                });
            } else {
                post(window, WM_KEYDOWN, usize::from(VK_ESCAPE));
            }
            Some(CalledOff { until }.into())
        }
    };
    let allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
    // SAFETY: this is the window's thread, which the windowing library initialised for OLE,
    // and the data object outlives the call. Without a drop source of our own the shell
    // supplies one, which follows the mouse buttons and Escape as Explorer's drags do; the
    // shell also draws the drag image.
    let owner = windows::Win32::Foundation::HWND(hwnd);
    let effect = unsafe { SHDoDragDrop(Some(owner), &data, source.as_ref(), allowed) }.map_err(|e| e.message())?;
    Ok(drag_effect_name(effect))
}

/// Names the effect a drop target reported. Some report several flags at once; anything that
/// isn't a plain move or link counts as a copy, since nothing is ever deleted on that basis.
fn drag_effect_name(effect: DROPEFFECT) -> &'static str {
    match effect {
        DROPEFFECT_NONE => "none",
        DROPEFFECT_MOVE => "move",
        DROPEFFECT_LINK => "link",
        _ => "copy",
    }
}

#[derive(Debug, Serialize)]
pub struct DragOut {
    /// How the drag ended: `copy`, `move`, `link`, or `none`.
    pub effect: &'static str,
    /// How many items it carried.
    pub items: usize,
}

/// Starts a native drag of the rows in `ranges` while the mouse button that began it is still
/// down, and returns when it ends. Kestrel never deletes anything because of how a drag ended:
/// a target that moves files does so itself, as Explorer does.
#[tauri::command(async)]
pub fn start_drag(
    window: Window,
    engine: State<'_, Arc<Engine>>,
    set: u32,
    ranges: Vec<(u32, u32)>,
    rehearsal: Option<Rehearsal>,
) -> Result<DragOut, String> {
    let requested: u64 = ranges.iter().map(|&(start, end)| u64::from(end.saturating_sub(start)) + 1).sum();
    if requested > MAX_DRAG_ITEMS {
        return Err(format!("Drag at most {MAX_DRAG_ITEMS} items at a time."));
    }
    let paths = engine.paths(set, &ranges)?;
    if paths.is_empty() {
        return Ok(DragOut { effect: "none", items: 0 });
    }
    if !INSTALLED.load(Ordering::Acquire) {
        return Err("Dragging files out isn't available.".into());
    }
    let items = paths.len();
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0;
    let (done, outcome) = mpsc::channel();
    // Rehearsals are for the end-to-end tests; release builds always drag for real.
    let rehearsal = if cfg!(debug_assertions) { rehearsal } else { None };
    // A drag still waiting is replaced: its request then ends with an error.
    *JOB.lock().unwrap_or_else(PoisonError::into_inner) = Some(DragJob { paths, rehearsal, done });
    // SAFETY: posting to a window of another thread is allowed; the message carries no pointers.
    if unsafe { PostMessageW(hwnd, drag_message(), 0, 0) } == 0 {
        JOB.lock().unwrap_or_else(PoisonError::into_inner).take();
        return Err("Couldn't start the drag.".into());
    }
    let effect = outcome.recv().map_err(|_| "The drag was replaced by another.".to_owned())??;
    Ok(DragOut { effect, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Ole::{CF_HDROP, ReleaseStgMedium};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    const NONE: Keys = Keys { ctrl: false, shift: false };
    const CTRL: Keys = Keys { ctrl: true, shift: false };
    const SHIFT: Keys = Keys { ctrl: false, shift: true };
    const BOTH: Keys = Keys { ctrl: true, shift: true };

    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|&path| path.to_owned()).collect()
    }

    /// A scratch folder under the temp directory; every test stays inside its own.
    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-dragdrop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    /// Initialises COM for the test's thread and undoes it afterwards.
    struct Com;

    impl Com {
        fn init() -> Self {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
            Com
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    /// Reads the plain file list a drop target sees in a data object.
    fn hdrop_paths(data: &IDataObject) -> Vec<String> {
        let format = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        unsafe {
            let mut medium = data.GetData(&format).unwrap();
            let drop = HDROP(medium.u.hGlobal.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            let paths = (0..count)
                .map(|i| {
                    let mut buffer = vec![0u16; DragQueryFileW(drop, i, None) as usize + 1];
                    let len = DragQueryFileW(drop, i, Some(&mut buffer)) as usize;
                    String::from_utf16_lossy(&buffer[..len])
                })
                .collect();
            ReleaseStgMedium(&mut medium);
            paths
        }
    }

    #[test]
    fn tells_volumes_apart() {
        assert_eq!(volume(r"C:\Users\me").as_deref(), Some("c:"));
        assert_eq!(volume("d:/data").as_deref(), Some("d:"));
        assert_eq!(volume(r"\\?\E:\long\path").as_deref(), Some("e:"));
        assert_eq!(volume(r"\\Server\Share\folder").as_deref(), Some(r"\\server\share"));
        assert_eq!(volume(r"\\?\UNC\server\share\x").as_deref(), Some(r"\\server\share"));
        assert_eq!(volume(r"\\server"), None);
        assert_eq!(volume("relative"), None);
    }

    #[test]
    fn moves_within_a_drive_and_copies_across() {
        let on_c = strings(&[r"C:\a\one.txt", r"c:\b\two.txt"]);
        assert_eq!(transfer_kind(&on_c, r"C:\dest", NONE), Transfer::Move);
        assert_eq!(transfer_kind(&on_c, r"D:\dest", NONE), Transfer::Copy);
        // One item from another drive makes the whole drop a copy, so nothing leaves that drive.
        let mixed = strings(&[r"C:\a\one.txt", r"D:\b\two.txt"]);
        assert_eq!(transfer_kind(&mixed, r"C:\dest", NONE), Transfer::Copy);
        let share = strings(&[r"\\nas\files\a.txt"]);
        assert_eq!(transfer_kind(&share, r"\\NAS\Files\b", NONE), Transfer::Move);
        assert_eq!(transfer_kind(&share, r"\\nas\other", NONE), Transfer::Copy);
        assert_eq!(transfer_kind(&share, r"C:\dest", NONE), Transfer::Copy);
    }

    #[test]
    fn modifier_keys_override_the_drive_rule() {
        let on_c = strings(&[r"C:\a\one.txt"]);
        assert_eq!(transfer_kind(&on_c, r"C:\dest", CTRL), Transfer::Copy);
        assert_eq!(transfer_kind(&on_c, r"D:\dest", SHIFT), Transfer::Move);
        assert_eq!(transfer_kind(&on_c, r"C:\dest", BOTH), Transfer::Copy);
        assert_eq!(transfer_kind(&on_c, r"D:\dest", BOTH), Transfer::Copy);
    }

    #[test]
    fn plans_leave_out_what_cannot_move() {
        let sources = strings(&[r"C:\work\a.txt", r"C:\work\sub", r"C:\other\b.txt"]);
        // Dropping on their own folder leaves the items from there alone.
        assert_eq!(plan(&sources, r"C:\work\", NONE), Some((Transfer::Move, strings(&[r"C:\other\b.txt"]))));
        assert_eq!(plan(&strings(&[r"C:\work\a.txt"]), r"c:\WORK", CTRL), None);
        // A folder can't go into itself or anything inside it.
        assert_eq!(plan(&strings(&[r"C:\work\sub"]), r"C:\work\sub", NONE), None);
        assert_eq!(plan(&strings(&[r"C:\work\sub"]), r"C:\work\sub\deeper", NONE), None);
        // A sibling whose name merely starts the same is a fine destination.
        assert_eq!(
            plan(&strings(&[r"C:\work\sub"]), r"C:\work\sub-two", NONE),
            Some((Transfer::Move, strings(&[r"C:\work\sub"])))
        );
        assert_eq!(plan(&strings(&[r"C:\work\a.txt"]), r"D:\backup", NONE), Some((Transfer::Copy, strings(&[r"C:\work\a.txt"]))));
        assert_eq!(plan(&[], r"C:\x", NONE), None);
    }

    #[test]
    fn data_object_carries_files_from_one_folder() {
        let dir = sandbox("one-folder");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join("b c.txt"), b"b").unwrap();
        let paths = vec![text(&dir.join("a.txt")), text(&dir.join("b c.txt")), text(&dir.join("sub"))];
        assert_eq!(common_parent(&paths).as_deref(), Some(text(&dir).as_str()));

        let _com = Com::init();
        let data = data_object(&paths).unwrap();
        let read: Vec<String> = hdrop_paths(&data).iter().map(|path| path.to_lowercase()).collect();
        let expected: Vec<String> = paths.iter().map(|path| path.to_lowercase()).collect();
        assert_eq!(read, expected);
        drop(data);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_object_carries_files_from_several_folders() {
        let dir = sandbox("several-folders");
        fs::create_dir_all(dir.join("one")).unwrap();
        fs::create_dir_all(dir.join("two/deeper")).unwrap();
        fs::write(dir.join("one/x.txt"), b"x").unwrap();
        fs::write(dir.join("two/deeper/y.txt"), b"y").unwrap();
        let paths = vec![text(&dir.join("one").join("x.txt")), text(&dir.join("two").join("deeper").join("y.txt"))];
        assert_eq!(common_parent(&paths), None);

        let _com = Com::init();
        let data = data_object(&paths).unwrap();
        let read: Vec<String> = hdrop_paths(&data).iter().map(|path| path.to_lowercase()).collect();
        let expected: Vec<String> = paths.iter().map(|path| path.to_lowercase()).collect();
        assert_eq!(read, expected);
        drop(data);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_object_refuses_missing_files() {
        let dir = sandbox("missing");
        let _com = Com::init();
        assert!(data_object(&[text(&dir.join("not-here.txt"))]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_drop_without_fresh_keys_reads_the_keyboard() {
        *DROP_KEYS.lock().unwrap() = Some((Instant::now().checked_sub(DROP_KEYS_VALID * 2).unwrap(), BOTH));
        // The stale record is discarded; what is read instead depends on the real keyboard, so
        // only the discarding is checked.
        let _ = keys_at_drop();
        assert!(DROP_KEYS.lock().unwrap().is_none());
        *DROP_KEYS.lock().unwrap() = Some((Instant::now(), SHIFT));
        assert_eq!(keys_at_drop(), SHIFT);
    }
}
