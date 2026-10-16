//! Reading files for their hashes: the two ends of a file, then all of it, with BLAKE3.
//!
//! A file is opened only once the duplicate finder knows from its folder's listing that its
//! contents are on this device; the handle is then checked again before a byte is read, so a
//! file that became a cloud placeholder, or another file under the same name, is left alone.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::tree::PLACEHOLDER;

/// Bytes read from each end of a file for its partial hash.
pub const EDGE: u64 = 64 * 1024;
/// Bytes read at a time for a full hash.
const CHUNK: usize = 1 << 20;

/// Every path opened for hashing, so tests can check what never was.
#[cfg(test)]
pub static OPENED: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

fn refused(why: &str) -> io::Error {
    io::Error::other(why.to_owned())
}

/// The attributes, size and file id of an open file, and its reparse tag if it has one.
#[cfg(windows)]
pub(crate) fn facts(file: &File) -> io::Result<(u32, u64, u64, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };
    // SAFETY: all zeroes is a valid BY_HANDLE_FILE_INFORMATION and FILE_ATTRIBUTE_TAG_INFO, each
    // writable and of the size given; the handle is open for both calls.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut tag: FILE_ATTRIBUTE_TAG_INFO = std::mem::zeroed();
        let tagged = GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        ) != 0;
        let size = (info.nFileSizeHigh as u64) << 32 | info.nFileSizeLow as u64;
        let id = (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64;
        Ok((info.dwFileAttributes, size, id, if tagged { tag.ReparseTag } else { 0 }))
    }
}

/// Reparse tags of links (symbolic links, junctions) carry this bit (`IsReparseTagNameSurrogate`).
const NAME_SURROGATE: u32 = 0x2000_0000;

/// Opens the file at `path` for reading, if it is still the file with this id and size, its
/// contents are on this device, and it isn't a link.
///
/// The file itself is looked at first, never what a link leads to, without reading a byte.
/// A reparse point that isn't a link (every file OneDrive keeps on this device, every file
/// Windows compresses) is then opened as any program opens it, through the filter that keeps
/// its contents: read as a reparse point, it would show nothing, or the wrong bytes.
#[cfg(windows)]
pub fn open(path: &str, file_id: u64, size: u64) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use kestrel_index::attr;
    #[cfg(test)]
    OPENED.lock().push(path.to_owned());
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let itself = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(share)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let (attrs, _, id, tag) = facts(&itself)?;
    if attrs & PLACEHOLDER != 0 {
        return Err(refused("its contents aren't on this device"));
    }
    if attrs & attr::REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0 {
        return Err(refused("it's a link"));
    }
    let file = if attrs & attr::REPARSE_POINT == 0 {
        itself
    } else {
        std::fs::OpenOptions::new().read(true).share_mode(share).custom_flags(FILE_FLAG_SEQUENTIAL_SCAN).open(path)?
    };
    // The file as read: still no placeholder, and the same file as looked at (a link swapped in
    // meanwhile would lead to another), of the size and id the search knows.
    let (attrs, found_size, found_id, _) = facts(&file)?;
    if attrs & PLACEHOLDER != 0 {
        return Err(refused("its contents aren't on this device"));
    }
    if found_id != id || found_size != size || (file_id != 0 && found_id != file_id) {
        return Err(refused("it changed"));
    }
    Ok(file)
}

/// The hash of a file's first and last 64 KB and its size; of the whole file when it is no
/// longer than both ends together, in which case it is also its full hash (see [`is_whole`]).
pub fn partial(file: &mut File, size: u64, read: &AtomicU64) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    if is_whole(size) {
        let mut all = Vec::with_capacity(size as usize);
        file.read_to_end(&mut all)?;
        if all.len() as u64 != size {
            return Err(refused("it changed"));
        }
        read.fetch_add(size, Ordering::Relaxed);
        hasher.update(&all);
        return Ok(*hasher.finalize().as_bytes());
    }
    let mut edge = vec![0u8; EDGE as usize];
    hasher.update(&size.to_le_bytes());
    file.read_exact(&mut edge)?;
    hasher.update(&edge);
    file.seek(SeekFrom::Start(size - EDGE))?;
    file.read_exact(&mut edge)?;
    hasher.update(&edge);
    read.fetch_add(2 * EDGE, Ordering::Relaxed);
    Ok(*hasher.finalize().as_bytes())
}

/// Whether a file this size is read whole for its partial hash.
pub fn is_whole(size: u64) -> bool {
    size <= 2 * EDGE
}

/// The BLAKE3 hash of the whole file, or `None` if `cancel` was set meanwhile. Each chunk read
/// is added to `read`.
pub fn full(file: &mut File, size: u64, read: &AtomicU64, cancel: &AtomicBool) -> io::Result<Option<[u8; 32]>> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        // Large reads are hashed on several cores.
        hasher.update_rayon(&buffer[..n]);
        total += n as u64;
        read.fetch_add(n as u64, Ordering::Relaxed);
    }
    if total != size {
        return Err(refused("it changed"));
    }
    Ok(Some(*hasher.finalize().as_bytes()))
}
