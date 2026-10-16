//! The folders cloud providers keep in sync (OneDrive above all), so the views can show each
//! item's sync status as Explorer's Status column does: online-only, available on this
//! device, or always kept on this device. The status itself comes from each item's attributes,
//! which the listing already has; this only says which items have a status at all.
//!
//! Sync roots are named by OneDrive's environment variables and, for every provider that uses
//! the Windows cloud files API, under `SyncRootManager` in the registry. Both are only read.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
};

use super::wide;

const SYNC_ROOT_MANAGER: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager";

struct Key(HKEY);

impl Key {
    fn open(parent: HKEY, path: &str) -> Option<Key> {
        let wide = wide(path);
        let mut key: HKEY = null_mut();
        // SAFETY: `wide` is NUL-terminated and `key` receives the handle on success.
        let status = unsafe { RegOpenKeyExW(parent, wide.as_ptr(), 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS).then_some(Key(key))
    }

    fn subkeys(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut buffer = [0u16; 512];
        for index in 0.. {
            let mut len = buffer.len() as u32;
            // SAFETY: `buffer` holds `len` characters, which bounds what the call writes.
            let status =
                unsafe { RegEnumKeyExW(self.0, index, buffer.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut()) };
            if status != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&buffer[..len as usize]));
        }
        names
    }

    /// Every string value's data.
    fn strings(&self) -> Vec<String> {
        let mut values = Vec::new();
        let mut name = [0u16; 512];
        let mut data = [0u16; 1024];
        for index in 0.. {
            let mut name_len = name.len() as u32;
            let mut data_len = (data.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: both buffers hold the lengths passed, which bound what the call writes.
            let status = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    null(),
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut data_len,
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            if kind == REG_SZ || kind == REG_EXPAND_SZ {
                let units = &data[..(data_len as usize / 2).min(data.len())];
                let end = units.iter().position(|&unit| unit == 0).unwrap_or(units.len());
                values.push(String::from_utf16_lossy(&units[..end]));
            }
        }
        values
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// Folders kept in sync by a cloud provider, without duplicates (compared without case).
pub fn sync_roots() -> Vec<String> {
    let mut roots: Vec<String> = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    if let Some(manager) = Key::open(HKEY_LOCAL_MACHINE, SYNC_ROOT_MANAGER) {
        for provider in manager.subkeys() {
            if let Some(users) = Key::open(manager.0, &format!(r"{provider}\UserSyncRoots")) {
                roots.extend(users.strings());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .map(|root| root.trim().trim_end_matches('\\').to_owned())
        .filter(|root| root.len() > 2 && std::path::Path::new(root).is_absolute())
        .filter(|root| seen.insert(root.to_lowercase()))
        .collect()
}

/// The cloud-synced folders, for the sync status glyphs.
#[tauri::command(async)]
pub fn sync_roots_list() -> Vec<String> {
    sync_roots()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_roots_are_absolute_and_unique() {
        let roots = sync_roots();
        let mut lower: Vec<String> = roots.iter().map(|root| root.to_lowercase()).collect();
        lower.sort();
        lower.dedup();
        assert_eq!(lower.len(), roots.len());
        assert!(roots.iter().all(|root| std::path::Path::new(root).is_absolute() && !root.ends_with('\\')));
        if let Ok(onedrive) = std::env::var("OneDrive") {
            assert!(lower.contains(&onedrive.trim_end_matches('\\').to_lowercase()), "{roots:?}");
        }
    }
}
