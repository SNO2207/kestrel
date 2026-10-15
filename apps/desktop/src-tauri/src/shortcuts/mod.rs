//! Shortcuts back to the places the user cares about: pinned places, frequent folders and Go
//! to folder, saved searches, tags and workspaces. The data lives in `kestrel_db::shortcuts`;
//! this module holds the commands, and the work that needs the index or the disk: finding a
//! moved item by its file id, ranking folders for Go to folder, resolving `tag:` searches.

pub mod frecency;
pub mod goto;
pub mod pins;
pub mod provider;
pub mod searches;
pub mod tags;
pub mod workspaces;

use std::path::Path;
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_index::{EntryId, Index, walk};
use tauri::State;

pub type Data<'a> = State<'a, Arc<Db>>;

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The volume a path is on, such as `c:` or `\\server\share`; file ids are unique only within one.
pub fn volume(path: &str) -> String {
    crate::dragdrop::volume(path).unwrap_or_default()
}

/// What identifies an item on disk beyond its path: its volume and file id (0 if unreadable).
pub fn identity(path: &str) -> (String, u64) {
    (volume(path), walk::file_id_of(path).unwrap_or(0))
}

/// A path on a network share, such as `\\server\share\file`.
pub fn is_network(path: &str) -> bool {
    path.starts_with(r"\\") && !path.starts_with(r"\\?\")
}

/// Whether `path` exists. A network path is taken to exist without asking, because asking an
/// unreachable server can take most of a minute.
pub fn exists(path: &str) -> bool {
    is_network(path) || Path::new(path).exists()
}

/// The live index entries that are this item: those with its file id on its volume.
pub fn entries_by_id(index: &Index, volume: &str, file_id: u64) -> Vec<EntryId> {
    if file_id == 0 {
        return Vec::new();
    }
    let mut found = index.entries_with_file_id(file_id);
    found.retain(|&id| crate::dragdrop::volume(&index.path(id)).as_deref() == Some(volume));
    found
}

/// Where an item known by `path`, `volume` and `file_id` is now: found by its file id in the
/// index, at its path, or renamed within the folder it was in, which covers folders outside
/// the index. `None` if it is nowhere to be found.
pub fn locate(index: &Index, path: &str, volume: &str, file_id: u64) -> Option<String> {
    if let Some(&entry) = entries_by_id(index, volume, file_id).first() {
        return Some(index.path(entry));
    }
    if exists(path) {
        return Some(path.to_owned());
    }
    if file_id == 0 {
        return None;
    }
    let parent = Path::new(path).parent()?.to_str()?;
    let items = walk::list(parent).ok()?;
    let item = items.into_iter().find(|item| item.meta.file_id == file_id)?;
    Some(Path::new(parent).join(item.name).to_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::IndexBuilder;
    use std::fs;

    #[test]
    fn items_are_found_after_a_rename_by_their_file_id() {
        let root = std::env::temp_dir().join(format!("kestrel-shortcuts-locate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("inner")).unwrap();
        fs::write(root.join("inner").join("before.txt"), b"x").unwrap();
        let before = root.join("inner").join("before.txt").to_str().unwrap().to_owned();
        let (vol, file_id) = identity(&before);
        assert_ne!(file_id, 0);
        assert!(vol.ends_with(':'), "{vol}");

        // Renamed within its folder, with no index: found by listing the folder.
        let after = root.join("inner").join("after.txt").to_str().unwrap().to_owned();
        fs::rename(&before, &after).unwrap();
        let empty = IndexBuilder::new().finish();
        assert_eq!(locate(&empty, &before, &vol, file_id).as_deref(), Some(after.as_str()));

        // Moved to another folder: only the index can tell where.
        fs::create_dir_all(root.join("other")).unwrap();
        let moved = root.join("other").join("moved.txt").to_str().unwrap().to_owned();
        fs::rename(&after, &moved).unwrap();
        assert_eq!(locate(&empty, &before, &vol, file_id), None);
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let index = builder.finish();
        assert_eq!(locate(&index, &before, &vol, file_id).as_deref(), Some(moved.as_str()));
        // The same id on another volume is something else.
        assert!(entries_by_id(&index, "q:", file_id).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
