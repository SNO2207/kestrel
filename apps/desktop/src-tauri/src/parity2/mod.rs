//! Explorer parity, second round: what Windows' File Explorer does that the first round left out.
//!
//! - [`libraries`]: the shell's Libraries, read from their `.library-ms` files, each listed as one
//!   folder holding the items of every folder it includes (`Engine::list_merged`).
//! - [`network`]: Map network drive and Disconnect network drive (Windows' own dialogs), the
//!   shell's "Add a network location" wizard, the Network location's computers (found with
//!   `WNetOpenEnum` off the window's thread, with a time limit, and kept for a minute), the
//!   shares of a computer, and the network locations the wizard made.
//! - [`tabs`]: a tab dragged out of its strip opens in a new window where it was dropped, and one
//!   dropped on another window's strip moves there.
//! - [`privacy`]: "Clear File Explorer history", for Kestrel's own records (`kestrel_db::parity2`).
//!
//! Adding files to a zip by dropping or pasting them is the archive module's (`archive::add`).
//! The front end's half is `src/parity2.ts`, `src/lib/parity2Api.ts` and the components
//! `Parity2Layer`, `LibrariesPage`, `NetworkPage`, `TabMenu` and `ItemCheck`.

pub mod libraries;
pub mod network;
pub mod privacy;
pub mod tabs;

use std::ptr::null_mut;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
use windows_sys::core::GUID;

/// A NUL-terminated wide string.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Text from a NUL-terminated wide string.
///
/// # Safety
/// `text` must be null or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the caller promises a NUL-terminated string; nothing past the NUL is read.
    unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    }
}

/// The path of a known folder, or `None` if Windows doesn't know it here.
pub fn known_folder(id: &GUID) -> Option<String> {
    let mut raw = null_mut();
    // SAFETY: `raw` receives a CoTaskMem-allocated, NUL-terminated string on success and is
    // freed below in every case, as the API requires.
    unsafe {
        let hr = SHGetKnownFolderPath(id, 0, null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| from_wide(raw));
        CoTaskMemFree(raw.cast());
        path
    }
}

/// A GUID written as `{FDD39AD0-238F-46AF-ADB4-6C85480369C7}`, braces optional.
pub fn parse_guid(text: &str) -> Option<GUID> {
    let text = text.trim().trim_start_matches('{').trim_end_matches('}');
    let groups: Vec<&str> = text.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    if groups.len() != 5 || groups.iter().zip(lengths).any(|(group, len)| group.len() != len || !group.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    u128::from_str_radix(&groups.concat(), 16).ok().map(GUID::from_u128)
}

/// Whether development hooks may answer: only in development builds.
pub fn dev_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_are_read_with_or_without_braces() {
        let documents = parse_guid("{FDD39AD0-238F-46AF-ADB4-6C85480369C7}").unwrap();
        assert_eq!(documents.data1, 0xFDD39AD0);
        assert_eq!(documents.data2, 0x238F);
        assert_eq!(documents.data3, 0x46AF);
        assert_eq!(documents.data4, [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7]);
        assert_eq!(parse_guid("fdd39ad0-238f-46af-adb4-6c85480369c7").map(|g| g.data1), Some(0xFDD39AD0));
        for bad in ["", "{}", "FDD39AD0-238F-46AF-ADB4", "XDD39AD0-238F-46AF-ADB4-6C85480369C7", "{FDD39AD0238F46AFADB46C85480369C7}"] {
            assert!(parse_guid(bad).is_none(), "{bad}");
        }
    }
}
