//! Text from the clipboard, for the terminal's Paste. Reading it here rather than through the
//! web view's clipboard API needs no permission prompt; Ctrl+V pastes through the web view's
//! own paste event, which needs none either.

use std::ptr::null_mut;

use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

/// `CF_UNICODETEXT`.
const UNICODE_TEXT: u32 = 13;
/// Text pasted at most, in UTF-16 units.
const MAX_TEXT: usize = 4 << 20;

/// The clipboard's text, or `None` when it holds none (or another program keeps it open).
pub fn text() -> Option<String> {
    // SAFETY: plain FFI.
    if unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) } == 0 {
        return None;
    }
    let mut opened = false;
    for _ in 0..10 {
        // SAFETY: no owner window is needed to read.
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return None;
    }
    // SAFETY: the clipboard is open on this thread; the block is locked only while it is read,
    // never past its size, and the clipboard is closed in every case.
    unsafe {
        let block = GetClipboardData(UNICODE_TEXT);
        let text = if block.is_null() {
            None
        } else {
            let data = GlobalLock(block).cast::<u16>();
            if data.is_null() {
                None
            } else {
                let units = (GlobalSize(block) / 2).min(MAX_TEXT);
                let slice = std::slice::from_raw_parts(data, units);
                let length = slice.iter().position(|&unit| unit == 0).unwrap_or(units);
                let text = String::from_utf16_lossy(&slice[..length]);
                GlobalUnlock(block);
                Some(text)
            }
        };
        CloseClipboard();
        text
    }
}
