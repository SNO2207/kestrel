//! The media library: every photo and video in the owner's media scopes, read once in the
//! background for what Windows knows of it (dimensions, when it was taken, the camera, where,
//! how long), a thumbnail for the Gallery, and the text in it.
//!
//! - [`pipeline`]: the background pass. Items go through [`pipeline::Stage`]s (metadata,
//!   thumbnail, text recognition, and any added later, such as embeddings), fed by one decode
//!   of each file, the heavy work at background priority ([`priority`]), pausing on request,
//!   while a full-screen app runs or while battery saver is on. Work is keyed by volume, file id, size and last write time, so
//!   a rename or a move is never read again and a changed file always is.
//! - [`scan`]: finding photos and videos in the index under the scopes.
//! - [`scopes`]: the default scopes, wherever Windows keeps the owner's folders.
//! - [`props`], [`decode`], [`ocr`]: the property system, WIC and Media Foundation, and
//!   Windows.Media.Ocr.
//! - [`query`]: answering the media clauses of a search (`width:`, `taken:`, `text:`...).
//! - [`fixtures`]: photos with EXIF, a video and an image with text, made for the tests.
//!
//! Nothing here ever opens a file whose contents are only in the cloud: that would download it.

pub mod conditions;
pub mod decode;
pub mod fixtures;
pub mod ocr;
pub mod pipeline;
pub mod priority;
pub mod props;
pub mod query;
pub mod scan;
pub mod scopes;

mod com;
#[cfg(test)]
mod files_tests;

pub use com::Apartment;
pub use decode::Bitmap;
pub use kestrel_db::media::Facts;
pub use pipeline::{Input, Item, Needs, Pipeline, Progress, Stage};

/// FILETIME ticks at the Unix epoch.
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// Attributes of a file whose contents are not on this computer: reading it downloads it.
/// `RECALL_ON_OPEN` doubles as `FILE_ATTRIBUTE_EA`, which a few local files carry too; they
/// are passed over all the same, since telling them apart would mean opening them.
pub const CLOUD_ONLY: u32 = 0x1000 | 0x4_0000 | 0x40_0000;

/// What the library holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Photo,
    Video,
}

impl Kind {
    /// As stored in the database.
    pub fn code(self) -> u8 {
        match self {
            Kind::Photo => kestrel_db::media::PHOTO,
            Kind::Video => kestrel_db::media::VIDEO,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            kestrel_db::media::PHOTO => Some(Kind::Photo),
            kestrel_db::media::VIDEO => Some(Kind::Video),
            _ => None,
        }
    }
}

/// Photo extensions: whatever WIC may decode. Camera raw and HEIC files need the codecs from
/// the Microsoft Store; without them their facts are read and decoding is reported as failed.
pub const PHOTO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "jxr", "wdp", "jxl",
    "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf",
];

/// Video extensions Media Foundation reads. `.ts` and `.mts` are left out: on a computer with code
/// on it they are far more often TypeScript (`.d.mts` declarations turned up in the owner's
/// Documents) than camera video.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "avi", "wmv", "asf", "mkv", "webm", "mpg", "mpeg", "3gp", "3g2", "m2ts"];

/// What a file is by its extension (given in lower case, without the dot).
pub fn kind_of_extension(ext: &str) -> Option<Kind> {
    if PHOTO_EXTENSIONS.contains(&ext) {
        Some(Kind::Photo)
    } else if VIDEO_EXTENSIONS.contains(&ext) {
        Some(Kind::Video)
    } else {
        None
    }
}

/// What a file is by its name.
pub fn kind_of_path(path: &str) -> Option<Kind> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    kind_of_extension(&ext.to_ascii_lowercase())
}

/// The volume a path is on, which file ids are unique within: `c:` for drive paths and
/// `\\server\share` for network paths, in lower case, as the rest of the app names volumes.
pub fn volume_of(path: &str) -> Option<String> {
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

/// A file's attributes, size and last write time, read from its directory entry without
/// opening it (so a cloud file is never downloaded), or `None` if there is nothing there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnDisk {
    pub attrs: u32,
    pub size: u64,
    /// FILETIME.
    pub mtime: i64,
}

impl OnDisk {
    pub fn read(path: &str) -> Option<Self> {
        use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA};
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        // SAFETY: WIN32_FILE_ATTRIBUTE_DATA is plain data; all-zero is a valid value.
        let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
        // SAFETY: `wide` is NUL-terminated and `data` is the structure this information level
        // fills. Only the directory entry is read, which never recalls a cloud file.
        let found = unsafe { GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&raw mut data).cast()) } != 0;
        found.then(|| OnDisk {
            attrs: data.dwFileAttributes,
            size: (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64,
            mtime: ((data.ftLastWriteTime.dwHighDateTime as u64) << 32 | data.ftLastWriteTime.dwLowDateTime as u64) as i64,
        })
    }

    pub fn is_cloud_only(self) -> bool {
        self.attrs & CLOUD_ONLY != 0
    }

    pub fn is_dir(self) -> bool {
        self.attrs & 0x10 != 0
    }
}

/// Milliseconds since the Unix epoch, from a FILETIME, as the front end's rows carry times.
pub fn unix_ms(filetime: i64) -> i64 {
    (filetime - FILETIME_UNIX_EPOCH) / 10_000
}

/// Seconds since the Unix epoch, now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_extensions() {
        assert_eq!(kind_of_path(r"C:\Pictures\IMG_0001.JPG"), Some(Kind::Photo));
        assert_eq!(kind_of_path(r"C:\Videos\clip.Mp4"), Some(Kind::Video));
        assert_eq!(kind_of_path(r"C:\code\app.ts"), None, "TypeScript, not a transport stream");
        assert_eq!(kind_of_path(r"C:\code\redirects.d.mts"), None, "TypeScript too");
        assert_eq!(kind_of_path(r"C:\Pictures\.jpg"), None);
        assert_eq!(kind_of_path(r"C:\Pictures\notes.txt"), None);
        assert_eq!(Kind::from_code(Kind::Video.code()), Some(Kind::Video));
    }

    #[test]
    fn volumes_are_named_as_the_app_names_them() {
        assert_eq!(volume_of(r"C:\Users\me\a.jpg").as_deref(), Some("c:"));
        assert_eq!(volume_of(r"\\?\D:\x").as_deref(), Some("d:"));
        assert_eq!(volume_of(r"\\NAS\Photos\2024\a.jpg").as_deref(), Some(r"\\nas\photos"));
        assert_eq!(volume_of(r"\\?\UNC\nas\photos\a.jpg").as_deref(), Some(r"\\nas\photos"));
        assert_eq!(volume_of("relative"), None);
    }

    #[test]
    fn files_are_read_from_their_directory_entry() {
        let dir = std::env::temp_dir().join(format!("kestrel-media-ondisk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"12345").unwrap();
        let found = OnDisk::read(file.to_str().unwrap()).unwrap();
        assert_eq!(found.size, 5);
        assert!(!found.is_cloud_only() && !found.is_dir());
        assert!(unix_ms(found.mtime) > 1_600_000_000_000);
        assert!(OnDisk::read(dir.to_str().unwrap()).unwrap().is_dir());
        assert_eq!(OnDisk::read(dir.join("missing.jpg").to_str().unwrap()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
