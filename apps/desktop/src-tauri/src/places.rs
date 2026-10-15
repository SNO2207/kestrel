//! Well-known folders and drives for the sidebar.

use std::ptr::null_mut;

use serde::Serialize;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_Videos, SHGetKnownFolderPath,
};
use windows_sys::core::GUID;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

#[derive(Serialize)]
pub struct Folder {
    pub name: &'static str,
    /// Stable key the UI maps to a glyph.
    pub kind: &'static str,
    pub path: String,
}

#[derive(Serialize)]
pub struct Drive {
    /// Root path such as `C:\`.
    pub path: String,
    pub label: String,
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
}

#[derive(Serialize)]
pub struct Places {
    pub home: String,
    pub folders: Vec<Folder>,
    pub drives: Vec<Drive>,
}

fn known_folder(id: &GUID) -> Option<String> {
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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Root paths of every mounted fixed disk, such as `C:\`.
pub fn fixed_drive_roots() -> Vec<String> {
    drives().into_iter().filter(|d| d.kind == "fixed").map(|d| d.path).collect()
}

pub fn drives() -> Vec<Drive> {
    // SAFETY: plain FFI call with no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask >> bit & 1 != 0)
        .filter_map(|bit| {
            let path = format!("{}:\\", (b'A' + bit) as char);
            let root = wide(&path);
            // SAFETY: `root` is NUL-terminated and the out-pointers are valid for each call.
            unsafe {
                let kind = match GetDriveTypeW(root.as_ptr()) {
                    DRIVE_FIXED => "fixed",
                    DRIVE_REMOVABLE => "removable",
                    DRIVE_REMOTE => "network",
                    DRIVE_CDROM => "optical",
                    _ => return None,
                };
                let mut label = [0u16; 64];
                GetVolumeInformationW(
                    root.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    0,
                );
                let label_len = label.iter().position(|&c| c == 0).unwrap_or(0);
                let (mut total, mut free) = (0u64, 0u64);
                GetDiskFreeSpaceExW(root.as_ptr(), null_mut(), &mut total, &mut free);
                Some(Drive {
                    path,
                    label: String::from_utf16_lossy(&label[..label_len]),
                    kind,
                    total,
                    free,
                })
            }
        })
        .collect()
}

pub fn places() -> Places {
    let folders = [
        ("Desktop", "desktop", &FOLDERID_Desktop),
        ("Documents", "documents", &FOLDERID_Documents),
        ("Downloads", "downloads", &FOLDERID_Downloads),
        ("Pictures", "pictures", &FOLDERID_Pictures),
        ("Music", "music", &FOLDERID_Music),
        ("Videos", "videos", &FOLDERID_Videos),
    ]
    .into_iter()
    .filter_map(|(name, kind, id)| Some(Folder { name, kind, path: known_folder(id)? }))
    .collect();
    Places { home: known_folder(&FOLDERID_Profile).unwrap_or_default(), folders, drives: drives() }
}
