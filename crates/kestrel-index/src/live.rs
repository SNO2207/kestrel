//! Keeping an index current from each volume's change journal.
//!
//! The journal says which folders changed; those folders are re-read from disk and the
//! differences applied with [`Sync`]. This needs no administrator rights, and it means a
//! restart costs a snapshot load plus the changes made while the app was closed, rather than a
//! full rescan.

use std::collections::HashSet;
use std::io;

use kestrel_ntfs::journal::{Journal, is_gap, reason};
use parking_lot::RwLock;
use rayon::prelude::*;

use crate::index::{DirItem, EntryId, Index, JournalCursor};
use crate::sync::{Sync, SyncStats};
use crate::walk;

/// The outcome of bringing an index up to date with one volume.
#[derive(Debug)]
pub enum Outcome {
    /// The index now reflects every change up to the present.
    Current { changes: usize, folders: usize, stats: SyncStats },
    /// The changes since the index was last current can no longer be recovered, so the volume
    /// has to be scanned again.
    Lost(&'static str),
}

/// Follows the change journal of one volume.
pub struct Tracker {
    root: String,
    journal: Journal,
    /// Opens the folders the journal names by their file ids; without it they are read by
    /// path.
    volume: Option<walk::Volume>,
}

/// The root of the volume holding `path`, in the form cursors are stored under, such as
/// `C:\`. A journal belongs to a whole volume even when only a folder of it is indexed.
fn volume_root(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return format!("{}:\\", (bytes[0] as char).to_ascii_uppercase());
    }
    format!("{}\\", path.trim_end_matches(['\\', '/']))
}

impl Tracker {
    pub fn open(root: &str) -> io::Result<Self> {
        Ok(Self { root: volume_root(root), journal: Journal::open(root)?, volume: walk::Volume::open(root).ok() })
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    /// The journal position to record with an index whose scan of this volume starts now.
    /// Changes made during the scan are replayed afterwards, which is harmless because
    /// re-reading a folder is idempotent.
    pub fn position(&self) -> io::Result<JournalCursor> {
        let state = self.journal.state()?;
        Ok(JournalCursor { root: self.root.clone(), journal_id: state.id, next_usn: state.next_usn })
    }

    /// Applies every change since the index's cursor for this volume and advances the cursor.
    pub fn catch_up(&self, index: &RwLock<Index>) -> io::Result<Outcome> {
        let cursor = index.read().journal().iter().find(|c| c.root.eq_ignore_ascii_case(&self.root)).cloned();
        let Some(cursor) = cursor else { return Ok(Outcome::Lost("the index has no journal position for this drive")) };
        let state = self.journal.state()?;
        if cursor.journal_id != state.id {
            return Ok(Outcome::Lost("the drive's change journal was recreated"));
        }
        if cursor.next_usn < state.first_usn {
            return Ok(Outcome::Lost("more changed than the journal holds"));
        }
        let (changes, next) = match self.journal.read(state.id, cursor.next_usn) {
            Err(error) if is_gap(&error) => return Ok(Outcome::Lost("more changed than the journal holds")),
            other => other?,
        };

        // Each folder is re-read once however many of its files changed, in first-seen order.
        let mut seen = HashSet::new();
        let folders: Vec<u64> = changes
            .iter()
            .filter(|change| change.reason & reason::VISIBLE != 0)
            .map(|change| change.parent_id)
            .filter(|&parent| seen.insert(parent))
            .collect();
        let (listed, stats) = refresh(index, folders, self.volume.as_ref());
        index.write().advance_journal(&self.root, state.id, next);
        Ok(Outcome::Current { changes: changes.len(), folders: listed, stats })
    }
}

/// Re-reads the folders with these file ids, by the paths the index holds for them, and
/// applies the differences as one batch. Returns how many folders were read.
pub fn refresh_folders(index: &RwLock<Index>, folder_ids: Vec<u64>) -> (usize, SyncStats) {
    refresh(index, folder_ids, None)
}

/// The live directory entry with this file id. Folders cannot be hard-linked, so there is at
/// most one on a real disk.
fn find_dir(index: &Index, file_id: u64) -> Option<EntryId> {
    index.entries_with_file_id(file_id).into_iter().find(|&id| index.is_dir(id))
}

/// Re-reads the folders with these file ids and applies the differences as one batch.
///
/// Folders are read in rounds. A folder created during the changes is unknown to the index
/// until its parent has been applied, so each round picks up the folders the previous one
/// made known, and a folder that turns up as new is read too, since it may have arrived with
/// contents that nothing else names. The index lock is released while the disk is read.
///
/// With a `volume`, folders are opened by file id. Their paths in the index are stale while a
/// folder above them has moved in the same batch (the move is applied only once its new parent
/// is read), and listing such a path failed, which silently lost the changes inside it.
fn refresh(index: &RwLock<Index>, mut waiting: Vec<u64>, volume: Option<&walk::Volume>) -> (usize, SyncStats) {
    let mut sync = Sync::default();
    let mut read: HashSet<u64> = HashSet::new();
    loop {
        let mut targets: Vec<(u64, EntryId, String)> = Vec::new();
        {
            let index = index.read();
            waiting.retain(|&file_id| {
                if read.contains(&file_id) {
                    return false;
                }
                let Some(dir) = find_dir(&index, file_id) else { return true };
                read.insert(file_id);
                targets.push((file_id, dir, index.path(dir)));
                false
            });
        }
        if targets.is_empty() {
            break;
        }
        // Folders that cannot be read, usually for lack of permission, are left as they are.
        let listings: Vec<(u64, EntryId, Vec<DirItem>)> = targets
            .into_par_iter()
            .filter_map(|(file_id, dir, path)| {
                let by_id = volume.map(|volume| volume.list(file_id));
                let items = match by_id {
                    Some(Ok(items)) => Ok(items),
                    _ => walk::list(&path),
                };
                items.ok().map(|items| (file_id, dir, items))
            })
            .collect();
        let mut index = index.write();
        for (file_id, dir, items) in listings {
            // A folder moved by a listing applied before its own has a new entry by now.
            let dir = if index.is_deleted(dir) { find_dir(&index, file_id) } else { Some(dir) };
            if let Some(dir) = dir {
                sync.dir(&mut index, dir, items);
            }
        }
        for dir in sync.take_new_dirs() {
            let file_id = index.file_id(dir);
            if !index.is_deleted(dir) && file_id != 0 && !read.contains(&file_id) {
                waiting.push(file_id);
            }
        }
    }
    (read.len(), sync.finish(&mut index.write()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexBuilder;
    use std::fs;

    /// Builds an index of a scratch folder on the system drive with a journal position taken
    /// first, as a real scan would.
    fn scanned(dir: &std::path::Path) -> (RwLock<Index>, Tracker) {
        let tracker = Tracker::open(r"C:\").unwrap();
        let cursor = tracker.position().unwrap();
        let mut builder = IndexBuilder::new();
        walk::walk(dir.to_str().unwrap(), &mut builder).unwrap();
        let mut index = builder.finish();
        index.set_journal(vec![cursor]);
        (RwLock::new(index), tracker)
    }

    fn names(index: &Index, path: &str) -> Vec<String> {
        let dir = index.lookup(path).unwrap_or_else(|| panic!("{path} is not in the index"));
        let mut names: Vec<String> = index.children(dir).map(|c| index.name(c).to_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn changes_made_after_a_scan_are_applied_from_the_journal() {
        let root = std::env::temp_dir().join(format!("kestrel-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("keep/deep")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("keep/deep/data.bin"), [0u8; 1000]).unwrap();
        fs::write(root.join("old.txt"), b"old").unwrap();
        fs::write(root.join("grows.txt"), b"1").unwrap();
        let (index, tracker) = scanned(&root);
        let base = root.to_str().unwrap().to_owned();

        // Changes the app was not running to see.
        fs::remove_file(root.join("old.txt")).unwrap();
        fs::write(root.join("grows.txt"), [b'x'; 500]).unwrap();
        fs::rename(root.join("keep"), root.join("target/moved")).unwrap();
        fs::create_dir_all(root.join("brand/new/tree")).unwrap();
        fs::write(root.join("brand/new/tree/leaf.txt"), b"leaf").unwrap();

        let outcome = tracker.catch_up(&index).unwrap();
        assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
        let index = index.read();
        assert_eq!(names(&index, &base), ["brand", "grows.txt", "target"]);
        // The moved folder kept its contents and their sizes.
        assert_eq!(names(&index, &format!(r"{base}\target\moved\deep")), ["data.bin"]);
        assert_eq!(index.size(index.lookup(&format!(r"{base}\target")).unwrap()), 1000);
        assert_eq!(index.size(index.lookup(&format!(r"{base}\grows.txt")).unwrap()), 500);
        // A folder tree created from nothing was filled in, however deep.
        assert_eq!(names(&index, &format!(r"{base}\brand\new\tree")), ["leaf.txt"]);
        assert_eq!(index.size(index.lookup(&base).unwrap()), 1000 + 500 + 4);
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A scratch folder under %TEMP% with the given subfolders, removed first if it exists.
    fn scratch(name: &str, folders: &[&str]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("kestrel-live-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for folder in folders {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        root
    }

    /// Whether any live entry of the index has this name.
    fn anywhere(index: &Index, name: &str) -> bool {
        (0..index.len()).any(|id| !index.is_deleted(id) && index.name(id) == name)
    }

    /// The e2e failure: a folder moved and a file inside it deleted between two journal reads.
    /// The folder of the deleted file was listed at the path the index still had for it, from
    /// before the move, which no longer existed, so the deletion was never applied.
    #[test]
    fn a_change_inside_a_folder_moved_in_the_same_batch_is_applied() {
        let root = scratch("moved-batch", &["nested/deeper"]);
        fs::write(root.join("nested/deeper/final.txt"), b"hello").unwrap();
        let (index, tracker) = scanned(&root);
        let base = root.to_str().unwrap().to_owned();

        fs::create_dir(root.join("elsewhere")).unwrap();
        fs::rename(root.join("nested"), root.join("elsewhere/nested")).unwrap();
        fs::remove_file(root.join("elsewhere/nested/deeper/final.txt")).unwrap();
        fs::write(root.join("elsewhere/nested/deeper/added.txt"), b"new").unwrap();

        assert!(matches!(tracker.catch_up(&index).unwrap(), Outcome::Current { .. }));
        let index = index.read();
        assert_eq!(names(&index, &format!(r"{base}\elsewhere\nested\deeper")), ["added.txt"]);
        assert!(!anywhere(&index, "final.txt"), "the deleted file is gone from the index");
        assert!(index.lookup(&format!(r"{base}\nested")).is_none());
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A journal read can end between creating a folder and moving another into it, and the
    /// listing that follows already shows the move. The moved folder must keep its contents.
    #[test]
    fn a_folder_moved_into_a_new_folder_keeps_its_contents_when_only_the_parent_is_named() {
        let root = scratch("moved-new", &["nested/deeper"]);
        fs::write(root.join("nested/deeper/kept.txt"), b"kept").unwrap();
        let (index, _tracker) = scanned(&root);
        let base = root.to_str().unwrap().to_owned();

        fs::create_dir(root.join("elsewhere")).unwrap();
        fs::rename(root.join("nested"), root.join("elsewhere/nested")).unwrap();
        // Only the record of `elsewhere` being created, whose parent is the root, was read.
        let root_id = walk::file_id_of(&base).unwrap();
        refresh_folders(&index, vec![root_id]);

        let index = index.read();
        assert_eq!(names(&index, &format!(r"{base}\elsewhere\nested\deeper")), ["kept.txt"]);
        assert!(index.lookup(&format!(r"{base}\nested")).is_none());
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A folder moved in from somewhere the index does not cover arrives with its contents,
    /// although the journal names only the folder it was moved into.
    #[test]
    fn a_folder_moved_in_from_outside_the_index_arrives_with_its_contents() {
        let root = scratch("moved-in", &["indexed", "outside/incoming/deep"]);
        fs::write(root.join("outside/incoming/deep/inside.txt"), b"inside").unwrap();
        let (index, tracker) = scanned(&root.join("indexed"));
        let base = root.join("indexed").to_str().unwrap().to_owned();

        fs::rename(root.join("outside/incoming"), root.join("indexed/incoming")).unwrap();
        assert!(matches!(tracker.catch_up(&index).unwrap(), Outcome::Current { .. }));

        let index = index.read();
        assert_eq!(names(&index, &format!(r"{base}\incoming\deep")), ["inside.txt"]);
        assert_eq!(index.size(index.lookup(&base).unwrap()), 6);
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Makes `link` a directory junction to `target`, as `mklink /J` (and `npm link`, pnpm and
    /// Yarn workspaces) do. Junctions need no special rights.
    pub(crate) fn junction(link: &std::path::Path, target: &std::path::Path) {
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    }

    /// Whether every live entry hangs under live folders all the way up to a root.
    fn every_entry_has_a_live_parent(index: &Index) -> bool {
        (0..index.len()).filter(|&id| !index.is_deleted(id)).all(|id| {
            let parent = index.parent(id);
            parent == crate::NO_ENTRY || !index.is_deleted(parent)
        })
    }

    /// A junction made in an indexed folder is a name for another folder, which the index
    /// already holds where it really is. Following it took the real folders' entries for moves
    /// and re-parented them under the link.
    #[test]
    fn a_new_junction_is_indexed_but_never_followed() {
        let root = scratch("junction", &["real/sub"]);
        fs::write(root.join("real/sub/inner.txt"), b"inner").unwrap();
        let (index, tracker) = scanned(&root);
        let base = root.to_str().unwrap().to_owned();

        junction(&root.join("link"), &root.join("real"));
        assert!(matches!(tracker.catch_up(&index).unwrap(), Outcome::Current { .. }));

        let index = index.read();
        assert_eq!(names(&index, &base), ["link", "real"]);
        assert_eq!(names(&index, &format!(r"{base}\real")), ["sub"]);
        assert_eq!(names(&index, &format!(r"{base}\real\sub")), ["inner.txt"]);
        assert!(names(&index, &format!(r"{base}\link")).is_empty(), "the junction is listed, not walked");
        assert_eq!(index.size(index.lookup(&base).unwrap()), 5, "nothing is counted twice");
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A junction to a folder above it, read by path: following it moved the folder's own
    /// children under the link and left them hanging under a deleted entry.
    #[test]
    fn a_junction_to_its_own_parent_leaves_the_tree_whole() {
        let root = scratch("junction-loop", &["proj/sub"]);
        fs::write(root.join("proj/sub/x.txt"), b"x").unwrap();
        let proj = root.join("proj");
        let (index, _tracker) = scanned(&proj);
        let base = proj.to_str().unwrap().to_owned();

        junction(&proj.join("back"), &proj);
        refresh_folders(&index, vec![walk::file_id_of(&base).unwrap()]);

        let index = index.read();
        assert_eq!(names(&index, &base), ["back", "sub"]);
        assert_eq!(names(&index, &format!(r"{base}\sub")), ["x.txt"]);
        assert!(names(&index, &format!(r"{base}\back")).is_empty());
        assert!(every_entry_has_a_live_parent(&index));
        drop(index);
        // Removing the junction leaves its target alone.
        fs::remove_dir(proj.join("back")).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cursors_are_kept_per_volume() {
        assert_eq!(volume_root(r"c:"), r"C:\");
        assert_eq!(volume_root(r"C:\Users\me\Projects"), r"C:\");
        assert_eq!(Tracker::open(r"C:\Windows").unwrap().position().unwrap().root, r"C:\");
    }

    #[test]
    fn an_index_without_a_position_needs_a_rescan() {
        let index = RwLock::new(IndexBuilder::new().finish());
        let tracker = Tracker::open(r"C:\").unwrap();
        assert!(matches!(tracker.catch_up(&index).unwrap(), Outcome::Lost(_)));
    }
}
