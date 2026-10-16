//! Small walks over the index that the rules share.

use kestrel_index::{EntryId, Index, attr};

use crate::{Item, unix_ms};

/// Attributes of a cloud file whose contents are not on this device: reading it downloads it.
/// OneDrive's online-only files carry both (0x401620), in listings and in the file table alike.
///
/// Not `FILE_ATTRIBUTE_RECALL_ON_OPEN` (0x40000), which is also `FILE_ATTRIBUTE_EA`, carried by
/// thousands of ordinary files with extended attributes; nor nothing allocated, which in an
/// index read from the file table is true of a hard link's second name (its allocation is
/// charged to the first), of files small enough to live in the table, and of files Windows
/// keeps compressed in a stream of their own (`kestrel_analyze::is_cloud_only` says the same).
pub const PLACEHOLDER: u32 = attr::OFFLINE | attr::RECALL_ON_DATA_ACCESS;

/// Whether attributes describe a file whose contents are only in the cloud.
pub fn is_placeholder(attrs: u32) -> bool {
    attrs & PLACEHOLDER != 0
}

/// The live entry at `path`.
pub fn find(index: &Index, path: &str) -> Option<EntryId> {
    if path.trim().is_empty() {
        return None;
    }
    index.lookup(path).filter(|&id| !index.is_deleted(id))
}

/// The live folder at `path` that is a real folder, not a link to one.
pub fn find_dir(index: &Index, path: &str) -> Option<EntryId> {
    find(index, path).filter(|&id| index.is_dir(id) && index.attrs(id) & attr::REPARSE_POINT == 0)
}

/// The folded name of an entry.
pub fn name(index: &Index, id: EntryId) -> &str {
    std::str::from_utf8(index.lower_bytes(id)).unwrap_or("")
}

/// The child of `dir` with this folded name.
pub fn child(index: &Index, dir: EntryId, folded: &str) -> Option<EntryId> {
    index.children(dir).find(|&c| index.lower_bytes(c) == folded.as_bytes())
}

/// The newest last write time of the entry and everything beneath it (folders' own times
/// included: they change when something inside is added or removed).
pub fn newest(index: &Index, id: EntryId) -> i64 {
    let mut newest = index.mtime(id);
    if !index.is_dir(id) {
        return newest;
    }
    let mut stack = vec![id];
    while let Some(dir) = stack.pop() {
        for c in index.children(dir) {
            newest = newest.max(index.mtime(c));
            if index.is_dir(c) {
                stack.push(c);
            }
        }
    }
    newest
}

/// The item for an entry whose newest change is `modified` (a FILETIME).
pub fn item(index: &Index, id: EntryId, modified: i64) -> Item {
    Item { path: index.path(id), dir: index.is_dir(id), bytes: index.alloc(id), modified: unix_ms(modified), file_id: index.file_id(id) }
}

/// The item for an entry, with its newest change worked out.
pub fn item_of(index: &Index, id: EntryId) -> Item {
    item(index, id, newest(index, id))
}

/// Every live entry with one of these extensions (lower case, no dot), in id order.
pub fn with_extensions(index: &Index, extensions: &[&str]) -> Vec<EntryId> {
    use rayon::prelude::*;
    let ids: Vec<u16> = extensions.iter().filter_map(|ext| index.ext_id(ext)).collect();
    if ids.is_empty() {
        return Vec::new();
    }
    (0..index.len())
        .into_par_iter()
        .with_min_len(1 << 14)
        .filter(|&id| ids.contains(&index.ext(id)) && !index.is_deleted(id) && !index.is_dir(id))
        .collect()
}
