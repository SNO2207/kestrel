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
        Ok(Self { root: volume_root(root), journal: Journal::open(root)? })
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
        let (listed, stats) = refresh_folders(index, folders);
        index.write().advance_journal(&self.root, state.id, next);
        Ok(Outcome::Current { changes: changes.len(), folders: listed, stats })
    }
}

/// Re-reads the folders with these file ids and applies the differences as one batch.
/// Returns how many folders were read.
///
/// Folders are read in rounds. A folder created during the changes is unknown to the index
/// until its parent has been applied, so each round picks up the folders the previous one
/// made known. The index lock is released while the disk is read.
pub fn refresh_folders(index: &RwLock<Index>, mut folder_ids: Vec<u64>) -> (usize, SyncStats) {
    let mut sync = Sync::default();
    let mut listed = 0;
    loop {
        let mut targets: Vec<(EntryId, String)> = Vec::new();
        {
            let index = index.read();
            folder_ids.retain(|&file_id| {
                let found = index.entries_with_file_id(file_id).into_iter().find(|&id| index.is_dir(id));
                if let Some(dir) = found {
                    targets.push((dir, index.path(dir)));
                }
                found.is_none()
            });
        }
        if targets.is_empty() {
            break;
        }
        listed += targets.len();
        // Folders that cannot be read, usually for lack of permission, are left as they are.
        let listings: Vec<(EntryId, Vec<DirItem>)> = targets
            .into_par_iter()
            .filter_map(|(dir, path)| walk::list(&path).ok().map(|items| (dir, items)))
            .collect();
        let mut index = index.write();
        for (dir, items) in listings {
            if !index.is_deleted(dir) {
                sync.dir(&mut index, dir, items);
            }
        }
    }
    (listed, sync.finish(&mut index.write()))
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
