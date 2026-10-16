//! What a file is, asked without opening it, and opening it only when its contents are on
//! this PC.

use std::fs::File;

/// `FILE_ATTRIBUTE_OFFLINE`, `FILE_ATTRIBUTE_RECALL_ON_OPEN` and
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`: the contents are elsewhere (OneDrive's online-only
/// files), and reading them downloads them. Such a file is never opened. (`RECALL_ON_OPEN`
/// shares its bit with `FILE_ATTRIBUTE_EA`, which some system files carry; they are passed
/// over too, which costs a search of system files little.)
pub const CLOUD_ONLY: u32 = 0x1000 | 0x4_0000 | 0x40_0000;
pub const DIRECTORY: u32 = 0x10;

/// A file's attributes, size and last write time (FILETIME ticks), from its directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub attrs: u32,
    pub size: u64,
    pub mtime: i64,
}

/// The path in the form that lifts the 260-character limit.
fn extended_text(path: &str) -> String {
    if path.starts_with(r"\\?\") {
        path.to_owned()
    } else if let Some(share) = path.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{share}")
    } else {
        format!(r"\\?\{path}")
    }
}

fn extended(path: &str) -> Vec<u16> {
    extended_text(path).encode_utf16().chain([0]).collect()
}

/// The file's attributes without opening it (`GetFileAttributesExW` reads the directory
/// entry), or `None` when it can't be found.
#[cfg(windows)]
pub fn attributes(path: &str) -> Option<Info> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA};
    let wide = extended(path);
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    // SAFETY: `wide` is NUL-terminated and `data` is the structure this information level fills.
    let ok = unsafe { GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&mut data as *mut WIN32_FILE_ATTRIBUTE_DATA).cast()) };
    if ok == 0 {
        return None;
    }
    let time = data.ftLastWriteTime;
    Some(Info {
        attrs: data.dwFileAttributes,
        size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
        mtime: ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)) as i64,
    })
}

#[cfg(not(windows))]
pub fn attributes(path: &str) -> Option<Info> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Info { attrs: if meta.is_dir() { DIRECTORY } else { 0 }, size: meta.len(), mtime: 0 })
}

/// Opens a file to read it, unless its contents are only in the cloud or it is a folder. The
/// attributes are read again here, just before opening, whatever the caller saw earlier.
pub fn open(path: &str) -> Option<File> {
    let info = attributes(path)?;
    if info.attrs & (CLOUD_ONLY | DIRECTORY) != 0 {
        return None;
    }
    // Shared for reading, writing and deleting, so the search never gets in anyone's way.
    File::open(extended_text(path)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_extended_for_long_names() {
        let text = |path: &str| String::from_utf16_lossy(&extended(path)).trim_end_matches('\0').to_owned();
        assert_eq!(text(r"C:\a\b.txt"), r"\\?\C:\a\b.txt");
        assert_eq!(text(r"\\server\share\x"), r"\\?\UNC\server\share\x");
        assert_eq!(text(r"\\?\C:\x"), r"\\?\C:\x");
    }

    #[test]
    fn attributes_are_read_without_opening() {
        let dir = std::env::temp_dir().join(format!("kestrel-content-fsinfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"12345").unwrap();
        let info = attributes(file.to_str().unwrap()).unwrap();
        assert_eq!(info.size, 5);
        assert_eq!(info.attrs & DIRECTORY, 0);
        assert!(attributes(dir.to_str().unwrap()).unwrap().attrs & DIRECTORY != 0);
        assert!(attributes(dir.join("missing").to_str().unwrap()).is_none());
        assert!(open(file.to_str().unwrap()).is_some());
        assert!(open(dir.to_str().unwrap()).is_none(), "folders are not opened");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
