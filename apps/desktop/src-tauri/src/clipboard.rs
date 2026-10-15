//! File lists on the Windows clipboard, in the same formats Explorer uses, so copy and paste
//! work in both directions between Kestrel and every other application.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

/// Files on the clipboard and whether pasting should move them (they were cut).
#[derive(Debug, PartialEq, Eq)]
pub struct FileClip {
    pub paths: Vec<String>,
    pub cut: bool,
}

/// Closes the clipboard when dropped.
struct Clipboard;

impl Clipboard {
    fn open(owner: Option<isize>) -> Result<Self, String> {
        let owner = owner.map_or(null_mut(), |hwnd| hwnd as HWND);
        // Another program may hold the clipboard for a moment, so retry briefly.
        for _ in 0..10 {
            // SAFETY: `owner` is null or a live window handle.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err("The clipboard is in use by another program".into())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: only constructed after OpenClipboard succeeded on this thread.
        unsafe { CloseClipboard() };
    }
}

fn drop_effect_format() -> u32 {
    let name: Vec<u16> = "Preferred DropEffect".encode_utf16().chain([0]).collect();
    // SAFETY: `name` is NUL-terminated.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Copies `bytes` into a movable global block and hands it to the clipboard.
unsafe fn put(format: u32, bytes: &[u8]) -> Result<(), String> {
    // SAFETY: the block is allocated with the requested size, locked before the copy and
    // unlocked after. On success the clipboard owns it; on failure it is freed here.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if block.is_null() {
            return Err("Out of memory".into());
        }
        let target = GlobalLock(block).cast::<u8>();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        GlobalUnlock(block);
        if SetClipboardData(format, block as HANDLE).is_null() {
            GlobalFree(block);
            return Err("Couldn't write to the clipboard".into());
        }
    }
    Ok(())
}

/// Puts `paths` on the clipboard as files. `cut` marks them to be moved when pasted.
pub fn set_files(owner: Option<isize>, paths: &[String], cut: bool) -> Result<(), String> {
    // A DROPFILES header followed by NUL-terminated wide paths and a final extra NUL.
    let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, fWide: 1, ..Default::default() };
    let mut bytes = Vec::new();
    // SAFETY: DROPFILES is a plain C struct with no padding-sensitive invariants.
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts((&raw const header).cast::<u8>(), size_of::<DROPFILES>())
    });
    for unit in paths.iter().flat_map(|path| path.encode_utf16().chain([0])).chain([0]) {
        bytes.extend_from_slice(&unit.to_ne_bytes());
    }
    let effect = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };

    let _clipboard = Clipboard::open(owner)?;
    // SAFETY: the clipboard is open on this thread for the duration of these calls.
    unsafe {
        EmptyClipboard();
        put(CF_HDROP, &bytes)?;
        put(drop_effect_format(), &effect.to_ne_bytes())
    }
}

/// Whether the clipboard currently holds files.
pub fn has_files() -> bool {
    // SAFETY: plain FFI call.
    unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 }
}

/// Reads the files on the clipboard, if it holds any.
pub fn files(owner: Option<isize>) -> Result<Option<FileClip>, String> {
    if !has_files() {
        return Ok(None);
    }
    let _clipboard = Clipboard::open(owner)?;
    // SAFETY: the clipboard is open, so the handles it returns stay valid until it is closed;
    // they are owned by the clipboard and must not be freed here.
    unsafe {
        let drop = GetClipboardData(CF_HDROP);
        if drop.is_null() {
            return Ok(None);
        }
        let count = DragQueryFileW(drop as _, u32::MAX, null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop as _, i, null_mut(), 0) as usize;
            let mut buffer = vec![0u16; len + 1];
            DragQueryFileW(drop as _, i, buffer.as_mut_ptr(), buffer.len() as u32);
            paths.push(String::from_utf16_lossy(&buffer[..len]));
        }

        let effect = GetClipboardData(drop_effect_format());
        let cut = !effect.is_null() && {
            let value = GlobalLock(effect).cast::<u32>();
            let cut = !value.is_null() && value.read_unaligned() & DROPEFFECT_MOVE != 0;
            GlobalUnlock(effect);
            cut
        };
        Ok(Some(FileClip { paths, cut }))
    }
}

/// Empties the clipboard, as Explorer does once cut files have been pasted.
pub fn clear(owner: Option<isize>) -> Result<(), String> {
    let _clipboard = Clipboard::open(owner)?;
    // SAFETY: the clipboard is open on this thread.
    unsafe { EmptyClipboard() };
    Ok(())
}
