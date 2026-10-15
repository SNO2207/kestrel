//! Index source that needs no elevation: a parallel crawl of a directory tree.
//!
//! Each directory is read with `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`, which
//! returns names, sizes, timestamps, attributes and file ids for many entries per system call,
//! so no file is ever opened or stat'ed individually.

use std::io;
use std::mem::MaybeUninit;
use std::ptr::{addr_of, null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO,
    FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    FileIdType, GetFileInformationByHandle, GetFileInformationByHandleEx, OPEN_EXISTING,
    OpenFileById,
};

use crate::builder::IndexBuilder;
pub use crate::index::DirItem;
use crate::index::{Meta, NO_ENTRY, attr};

/// Reparse tags with this bit set are aliases for another name (junctions, symlinks). They are
/// listed but never descended into, which avoids cycles and double counting. Other reparse
/// directories, such as OneDrive placeholders, are real directories and are walked.
const NAME_SURROGATE_BIT: u32 = 0x2000_0000;

const BUFFER_WORDS: usize = 8192;

#[derive(Clone, Copy, Debug, Default)]
pub struct WalkStats {
    pub dirs: u64,
    pub files: u64,
    /// Directories that could not be opened, usually for lack of permission.
    pub unreadable: u64,
}

struct Item {
    name_len: u32,
    /// Walk-local directory number, or `NO_ENTRY` for anything that is not descended into.
    node: u32,
    meta: Meta,
}

/// Everything found in one directory.
struct Batch {
    dir_node: u32,
    names: String,
    items: Vec<Item>,
}

struct Shared {
    next_node: AtomicU32,
    batches: Mutex<Vec<Batch>>,
    unreadable: AtomicU64,
}

/// The extended-length form of a directory path, ending with a backslash. This lifts the
/// 260-character limit and covers UNC shares.
fn extended_path(dir: &str) -> Vec<u16> {
    let dir = dir.trim_end_matches(['\\', '/']);
    let text = match dir.strip_prefix(r"\\") {
        Some(_) if dir.starts_with(r"\\?\") => format!(r"{dir}\"),
        Some(share) => format!(r"\\?\UNC\{share}\"),
        None => format!(r"\\?\{dir}\"),
    };
    text.encode_utf16().collect()
}

/// The file id (on NTFS, the file reference number) of a file or directory.
pub fn file_id_of(path: &str) -> Option<u64> {
    let mut wide = extended_path(path);
    // A drive root such as `\\?\C:\` must keep its separator; other paths must not have one.
    if !path.trim_end_matches(['\\', '/']).ends_with(':') {
        wide.pop();
    }
    wide.push(0);
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    // SAFETY: `wide` is NUL-terminated; the handle is closed before returning.
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
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
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let ok = GetFileInformationByHandle(handle, &mut info) != 0;
        CloseHandle(handle);
        ok.then(|| (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64)
    }
}

/// Lists a single directory without descending into it.
pub fn list(dir: &str) -> io::Result<Vec<DirItem>> {
    let mut items = Vec::new();
    read_dir(&mut extended_path(dir), |name, meta, _| {
        items.push(DirItem { name: String::from_utf16_lossy(name), meta });
    })
    .map_err(|code| io::Error::from_raw_os_error(code as i32))?;
    Ok(items)
}

/// A volume whose folders can be listed by file id.
///
/// A change journal names folders by file id. Opening one by its id finds it wherever it is
/// now, whereas the path an index holds for it is stale whenever a folder above it was moved
/// or renamed in the same batch of changes, or since.
pub struct Volume {
    /// Any handle on the volume serves as the hint `OpenFileById` needs.
    handle: HANDLE,
}

// SAFETY: the handle is only used through thread-safe Win32 calls.
unsafe impl Send for Volume {}
unsafe impl Sync for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: opened by `Volume::open` and closed exactly once.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Volume {
    /// Opens the volume whose root is `root`, such as `C:\`. Needs no special rights.
    pub fn open(root: &str) -> io::Result<Self> {
        let wide: Vec<u16> = format!("{}\\", root.trim_end_matches(['\\', '/'])).encode_utf16().chain([0]).collect();
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Lists the folder with this file id, wherever it is on the volume now. Fails if it no
    /// longer exists (the sequence number in the id tells a reused record apart) or cannot be
    /// read.
    pub fn list(&self, file_id: u64) -> io::Result<Vec<DirItem>> {
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: file_id as i64 },
        };
        // SAFETY: `descriptor` is a valid FILE_ID_DESCRIPTOR that outlives the call.
        let handle = unsafe {
            OpenFileById(
                self.handle,
                &descriptor,
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut items = Vec::new();
        read_handle(handle, |name, meta, _| {
            items.push(DirItem { name: String::from_utf16_lossy(name), meta });
        })
        .map_err(|code| io::Error::from_raw_os_error(code as i32))?;
        Ok(items)
    }
}

/// Crawls `root` (for example `C:\`) and adds it and everything beneath it to `builder`.
pub fn walk(root: &str, builder: &mut IndexBuilder) -> io::Result<WalkStats> {
    let root_name = root.trim_end_matches(['\\', '/']);
    let mut path = extended_path(root_name);

    // Fail up front if the root itself cannot be read, rather than returning an empty index.
    read_dir(&mut path, |_, _, _| {}).map_err(|code| io::Error::from_raw_os_error(code as i32))?;

    let shared = Shared {
        next_node: AtomicU32::new(1),
        batches: Mutex::new(Vec::new()),
        unreadable: AtomicU64::new(0),
    };
    rayon::scope(|scope| visit(scope, &shared, path, 0));

    let batches = shared.batches.into_inner().unwrap();
    let mut node_entry = vec![NO_ENTRY; shared.next_node.into_inner() as usize];
    node_entry[0] = builder.add_root(root_name);
    // Change journals name the root as the parent of its top-level entries.
    if let Some(file_id) = file_id_of(root_name) {
        builder.set_file_id(node_entry[0], file_id);
    }
    let mut stats =
        WalkStats { dirs: 1, files: 0, unreadable: shared.unreadable.into_inner() };

    // Directories finish in arbitrary order, so entries are pushed first and parented second,
    // once every directory has an id.
    let mut first_ids = Vec::with_capacity(batches.len());
    for batch in &batches {
        first_ids.push(builder.len());
        let mut offset = 0;
        for item in &batch.items {
            let end = offset + item.name_len as usize;
            let id = builder.push(NO_ENTRY, &batch.names[offset..end], item.meta);
            offset = end;
            if item.meta.attrs & attr::DIRECTORY != 0 {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            if item.node != NO_ENTRY {
                node_entry[item.node as usize] = id;
            }
        }
    }
    for (batch, first) in batches.iter().zip(first_ids) {
        let parent = node_entry[batch.dir_node as usize];
        for id in first..first + batch.items.len() as u32 {
            builder.set_parent(id, parent);
        }
    }
    Ok(stats)
}

/// Reads one directory and spawns a task for each subdirectory. `path` ends with a backslash.
fn visit<'s>(scope: &rayon::Scope<'s>, shared: &'s Shared, mut path: Vec<u16>, node: u32) {
    let mut batch = Batch { dir_node: node, names: String::new(), items: Vec::new() };
    let mut subdirs: Vec<(Vec<u16>, u32)> = Vec::new();

    let result = read_dir(&mut path, |name, meta, reparse_tag| {
        let start = batch.names.len();
        batch.names.extend(char::decode_utf16(name.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
        let name_len = (batch.names.len() - start) as u32;

        let is_dir = meta.attrs & attr::DIRECTORY != 0;
        let is_alias = meta.attrs & attr::REPARSE_POINT != 0 && reparse_tag & NAME_SURROGATE_BIT != 0;
        let node = if is_dir && !is_alias {
            let node = shared.next_node.fetch_add(1, Ordering::Relaxed);
            subdirs.push((name.to_vec(), node));
            node
        } else {
            NO_ENTRY
        };
        batch.items.push(Item { name_len, node, meta });
    });

    if result.is_err() {
        shared.unreadable.fetch_add(1, Ordering::Relaxed);
    }
    for (name, child_node) in subdirs {
        let mut child = Vec::with_capacity(path.len() + name.len() + 2);
        child.extend_from_slice(&path);
        child.extend_from_slice(&name);
        child.push(b'\\' as u16);
        scope.spawn(move |scope| visit(scope, shared, child, child_node));
    }
    if !batch.items.is_empty() {
        shared.batches.lock().unwrap().push(batch);
    }
}

/// Enumerates the directory at `path`, calling `each(name, meta, reparse_tag)` for every entry
/// except `.` and `..`. Returns the Win32 error code on failure.
fn read_dir(path: &mut Vec<u16>, each: impl FnMut(&[u16], Meta, u32)) -> Result<(), u32> {
    path.push(0);
    // SAFETY: `path` is NUL-terminated for the duration of the call.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    path.pop();
    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: plain FFI call with no arguments.
        return Err(unsafe { GetLastError() });
    }
    read_handle(handle, each)
}

/// Enumerates the open directory `handle` as [`read_dir`] does, then closes it.
fn read_handle(handle: HANDLE, mut each: impl FnMut(&[u16], Meta, u32)) -> Result<(), u32> {
    // 64 KiB, 8-byte aligned and deliberately uninitialised: zeroing it per directory would
    // cost more than the enumeration itself.
    let mut buffer = MaybeUninit::<[u64; BUFFER_WORDS]>::uninit();
    let buffer_bytes = (BUFFER_WORDS * 8) as u32;

    let mut class = FileIdBothDirectoryRestartInfo;
    let result = loop {
        // SAFETY: the buffer is valid for `buffer_bytes` and outlives the call.
        let ok = unsafe {
            GetFileInformationByHandleEx(handle, class, buffer.as_mut_ptr().cast(), buffer_bytes)
        };
        if ok == 0 {
            // SAFETY: plain FFI call with no arguments.
            let code = unsafe { GetLastError() };
            break if code == ERROR_NO_MORE_FILES { Ok(()) } else { Err(code) };
        }
        class = FileIdBothDirectoryInfo;

        let mut cursor = buffer.as_ptr().cast::<u8>();
        loop {
            let info = cursor.cast::<FILE_ID_BOTH_DIR_INFO>();
            // SAFETY: on success the kernel wrote a chain of 8-byte-aligned records into the
            // buffer, each holding `FileNameLength` bytes of name and linked by
            // `NextEntryOffset`, with zero marking the last one.
            let (next, name, meta, reparse_tag) = unsafe {
                let name_len = (*info).FileNameLength as usize / 2;
                let name = std::slice::from_raw_parts(addr_of!((*info).FileName).cast::<u16>(), name_len);
                let meta = Meta {
                    size: (*info).EndOfFile as u64,
                    alloc: (*info).AllocationSize as u64,
                    mtime: (*info).LastWriteTime,
                    attrs: (*info).FileAttributes,
                    file_id: (*info).FileId as u64,
                };
                // For reparse points the EaSize field carries the reparse tag instead.
                ((*info).NextEntryOffset as usize, name, meta, (*info).EaSize)
            };
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                each(name, meta, reparse_tag);
            }
            if next == 0 {
                break;
            }
            // SAFETY: `next` is the kernel-provided distance to the following record.
            cursor = unsafe { cursor.add(next) };
        }
    };
    // SAFETY: `handle` was returned by CreateFileW and is closed exactly once.
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_a_real_directory_tree() {
        let root = std::env::temp_dir().join(format!("kestrel-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("one.txt"), b"12345").unwrap();
        fs::write(root.join("sub/Two.RS"), b"1234567").unwrap();
        fs::write(root.join("sub/deeper/ünï.bin"), [0u8; 100]).unwrap();

        let mut builder = IndexBuilder::new();
        let root_str = root.to_str().unwrap();
        let stats = walk(root_str, &mut builder).unwrap();
        let idx = builder.finish();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!((stats.dirs, stats.files, stats.unreadable), (3, 3, 0));
        assert_eq!(idx.size(idx.roots()[0]), 112);
        let two = idx.lookup(&format!(r"{root_str}\sub\two.rs")).unwrap();
        assert_eq!(idx.name(two), "Two.RS");
        assert_eq!(idx.size(two), 7);
        assert_ne!(idx.file_id(two), 0);
        assert!(idx.mtime(two) > 0);
        let deep = idx.lookup(&format!(r"{root_str}\sub\deeper\ünï.bin")).unwrap();
        assert_eq!(idx.path(deep), format!(r"{root_str}\sub\deeper\ünï.bin"));
    }

    #[test]
    fn a_folder_is_listed_by_file_id_wherever_it_has_moved() {
        let root = std::env::temp_dir().join(format!("kestrel-walk-id-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("before/inner")).unwrap();
        fs::write(root.join("before/inner/a.txt"), b"a").unwrap();
        // Extended-length paths take backslashes only.
        let id = file_id_of(root.join("before").join("inner").to_str().unwrap()).unwrap();
        fs::create_dir(root.join("after")).unwrap();
        fs::rename(root.join("before/inner"), root.join("after/renamed")).unwrap();

        let volume = Volume::open(r"C:\").unwrap();
        let names: Vec<String> = volume.list(id).unwrap().into_iter().map(|item| item.name).collect();
        assert_eq!(names, ["a.txt"]);
        // A deleted folder's id no longer opens, even if its record is reused later.
        fs::remove_dir_all(root.join("after/renamed")).unwrap();
        assert!(volume.list(id).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_root_is_an_error() {
        let mut builder = IndexBuilder::new();
        assert!(walk(r"C:\this\path\does\not\exist-kestrel", &mut builder).is_err());
    }
}
