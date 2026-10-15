//! What a drive says about itself: its size and free space, its cluster size, and, when
//! Windows tells an ordinary account, the size of NTFS's own file table. The disk usage hub sets
//! these beside what the index holds, to explain the difference.

use std::ptr::{null, null_mut};

use serde::Serialize;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FindClose,
    FindFirstFileW, GetDiskFreeSpaceExW, GetDiskFreeSpaceW, GetVolumeInformationW, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

#[derive(Clone, Debug, Serialize)]
pub struct Volume {
    /// Such as `C:\`.
    pub root: String,
    pub label: String,
    pub file_system: String,
    pub total: u64,
    pub free: u64,
    /// Bytes per cluster: every file's size on disk is a whole number of them.
    pub cluster: u64,
    /// Bytes of NTFS's master file table in use, when the drive says.
    pub mft: Option<u64>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn text(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Reads what the drive with root `root` (`C:\` or `C:`) says about itself. `None` if it can't
/// be read.
pub fn read(root: &str) -> Option<Volume> {
    let root = format!("{}\\", root.trim_end_matches(['\\', '/']));
    let path = wide(&root);
    let (mut total, mut free) = (0u64, 0u64);
    let mut label = [0u16; 128];
    let mut file_system = [0u16; 32];
    let (mut sectors_per_cluster, mut bytes_per_sector, mut free_clusters, mut clusters) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `path` is NUL-terminated and every out-pointer is valid for the call it is passed to,
    // with the buffer lengths given.
    unsafe {
        if GetDiskFreeSpaceExW(path.as_ptr(), null_mut(), &mut total, &mut free) == 0 {
            return None;
        }
        GetVolumeInformationW(
            path.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            null_mut(),
            null_mut(),
            null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        );
        GetDiskFreeSpaceW(path.as_ptr(), &mut sectors_per_cluster, &mut bytes_per_sector, &mut free_clusters, &mut clusters);
    }
    let file_system = text(&file_system);
    Some(Volume {
        mft: if file_system == "NTFS" { mft_bytes(&root) } else { None },
        root,
        label: text(&label),
        file_system,
        total,
        free,
        cluster: sectors_per_cluster as u64 * bytes_per_sector as u64,
    })
}

/// The bytes of the master file table in use, with no rights beyond the ordinary.
fn mft_bytes(root: &str) -> Option<u64> {
    // Opening the volume itself (`\\.\C:`) takes administrator rights to be any use, but NTFS
    // answers this question on a handle to the root folder too, which anyone may open to read
    // its attributes (the same trick that reads the change journal unprivileged).
    let folder = wide(root);
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    // SAFETY: `folder` is NUL-terminated; the handle is closed before returning, and `data` is a
    // valid out-buffer of the size passed.
    unsafe {
        let handle = CreateFileW(
            folder.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut data: NTFS_VOLUME_DATA_BUFFER = std::mem::zeroed();
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            null(),
            0,
            (&raw mut data).cast(),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut returned,
            null_mut(),
        ) != 0;
        CloseHandle(handle);
        (ok && data.MftValidDataLength > 0).then_some(data.MftValidDataLength as u64)
    }
}

/// The size a folder lists for a file, without opening the file: the page file and the
/// hibernation file are held open by Windows and can't be opened, but can be listed.
pub fn listed_size(path: &str) -> Option<u64> {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated and `found` is a valid out-buffer; the search handle is
    // closed before returning.
    unsafe {
        let mut found: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = FindFirstFileW(path.as_ptr(), &mut found);
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        FindClose(handle);
        Some((found.nFileSizeHigh as u64) << 32 | found.nFileSizeLow as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_drive_describes_itself() {
        let volume = read("C:").expect("C: can be read");
        assert_eq!(volume.root, r"C:\");
        assert!(volume.total > volume.free && volume.free > 0);
        assert!(volume.cluster >= 512 && volume.cluster.is_power_of_two());
        assert_eq!(volume.file_system, "NTFS");
        // The file table's size needs no rights on NTFS; it is at least a few megabytes.
        let mft = volume.mft.expect("NTFS tells an ordinary account its file table's size");
        assert!(mft > 1 << 20, "{mft}");
        assert!(read(r"Q:\nowhere").is_none() || std::path::Path::new(r"Q:\").exists());
    }

    #[test]
    fn listed_sizes_need_no_opening() {
        let dir = std::env::temp_dir().join(format!("kestrel-disk-volume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("listed.bin");
        std::fs::write(&file, vec![7u8; 12_345]).unwrap();
        assert_eq!(listed_size(file.to_str().unwrap()), Some(12_345));
        assert_eq!(listed_size(dir.join("missing.bin").to_str().unwrap()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
