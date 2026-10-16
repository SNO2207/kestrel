//! Explorer parity: what Windows' own File Explorer does that the rest of Kestrel did not.
//!
//! - [`shellnew`]: New > every file type Windows offers, read from the registry's `ShellNew`
//!   entries, created from the template (or empty, or from the bytes registered) and then
//!   renamed in place by the front end.
//! - [`links`]: shortcuts (`.lnk`) through `IShellLink`: New > Shortcut, Alt+drop, and reading
//!   the targets of Windows' Recent items.
//! - [`recent`]: Home's recent files: Windows' Recent items plus the files opened through
//!   Kestrel (kept in `kestrel_db::parity`).
//! - [`terminal`]: Open in Terminal, in Windows Terminal (the projects feature's launcher).
//! - [`details`]: the details pane's properties of one item, read through the Windows property
//!   system, and the summary of a selection.
//! - [`syncroots`]: the folders cloud providers such as OneDrive keep in sync, for the sync
//!   status glyphs.
//! - [`appwindows`]: Ctrl+N's other windows in the same process: their labels, which of them the
//!   quick launcher and a second launch of Kestrel bring forward, and their preview webviews.

pub mod appwindows;
pub mod details;
pub mod links;
pub mod recent;
pub mod shellnew;
pub mod syncroots;
pub mod terminal;

use std::ptr::null_mut;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
use windows_sys::core::GUID;

/// A NUL-terminated wide string.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as i64)
}

/// The path of a known folder, such as the Templates or Recent folder.
pub fn known_folder(id: &GUID) -> Option<String> {
    let mut raw = null_mut();
    // SAFETY: `raw` receives a CoTaskMem-allocated, NUL-terminated string on success and is
    // freed below in every case, as the API requires.
    unsafe {
        let hr = SHGetKnownFolderPath(id, 0, null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(raw, len))
        });
        CoTaskMemFree(raw.cast());
        path
    }
}

/// Runs `work` on a thread of its own in a single-threaded COM apartment, as shell objects
/// (shortcuts, property handlers) expect, and waits for it.
pub fn in_apartment<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    std::thread::Builder::new()
        .name("kestrel-parity-com".into())
        .spawn(move || {
            // SAFETY: initialises COM for this new thread, balanced below once `work` is done
            // and every interface it made has been dropped.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().map_err(|e| e.message())?;
            let result = work();
            // SAFETY: balances the successful CoInitializeEx above, on the same thread.
            unsafe { CoUninitialize() };
            result
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "The shell crashed while answering".to_owned())?
}

/// File attributes, read without opening the file (so a cloud file is not downloaded), or
/// `None` if there is nothing at `path`.
pub fn attributes(path: &str) -> Option<u32> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
    let wide = wide(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    (attrs != INVALID_FILE_ATTRIBUTES).then_some(attrs)
}

/// Attributes of a file whose contents live elsewhere: reading it downloads it first.
pub const CLOUD_ONLY: u32 = 0x1000 | 0x4_0000 | 0x40_0000;
const DIRECTORY: u32 = 0x10;
