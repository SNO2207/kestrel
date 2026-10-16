//! Bringing folders of the index back in line with what is on disk.
//!
//! A [`Sync`] takes fresh listings of any number of folders and applies the differences as one
//! batch. Matching by file id across the whole batch is what lets a folder moved from one
//! listed folder to another keep its contents, instead of being dropped from one place and
//! re-added empty in the other.
//!
//! A batch does not borrow the index between listings, so the caller can release its lock
//! while reading the disk and apply each round of listings as it arrives.

use std::collections::HashMap;

use crate::fold::fold;
use crate::index::{DirItem, EntryId, Index, attr};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub inserted: usize,
    pub moved: usize,
    pub removed: usize,
    pub updated: usize,
}

#[derive(Default)]
pub struct Sync {
    /// Files listed on disk that the index lacks, placed once every listing has been seen.
    pending: Vec<(EntryId, DirItem)>,
    /// Entries whose folder was listed without them: removed unless they turn up elsewhere.
    missing: Vec<EntryId>,
    /// Folders added as new rather than moved, since [`Sync::take_new_dirs`] was last called.
    new_dirs: Vec<EntryId>,
    stats: SyncStats,
}

fn is_dir(item: &DirItem) -> bool {
    item.meta.attrs & attr::DIRECTORY != 0
}

impl Sync {
    /// Reconciles the folder `dir` with `items`, its complete contents as just read from disk.
    pub fn dir(&mut self, index: &mut Index, dir: EntryId, items: Vec<DirItem>) {
        let mut known: HashMap<Vec<u8>, EntryId> =
            index.children(dir).map(|child| (index.lower_bytes(child).to_vec(), child)).collect();

        for item in items {
            match known.remove(fold(&item.name).as_bytes()) {
                // The same entry, perhaps renamed in case only or saved under a new file id.
                Some(id) if index.is_dir(id) == is_dir(&item) => {
                    let id = if index.name(id) == item.name { id } else { index.rename(id, dir, &item.name) };
                    index.update_meta(id, item.meta);
                    self.stats.updated += 1;
                }
                // A file became a folder or the other way round.
                Some(id) => {
                    self.missing.push(id);
                    self.place(index, dir, item);
                }
                None => self.place(index, dir, item),
            }
        }
        self.missing.extend(known.into_values());
    }

    /// Adds an item the index lacks, as a move where the evidence allows.
    fn place(&mut self, index: &mut Index, dir: EntryId, item: DirItem) {
        if !is_dir(&item) {
            // A file with a known id may be a second hard link rather than a move, so decide
            // once it is known whether the original went missing.
            self.pending.push((dir, item));
            return;
        }
        // Folders cannot be hard-linked, so a folder whose id lives elsewhere has moved here,
        // and moving its entry keeps everything beneath it. A new folder is added at once so
        // that a later listing of its contents in the same batch has somewhere to go.
        let elsewhere = index
            .entries_with_file_id(item.meta.file_id)
            .into_iter()
            .find(|&old| index.is_dir(old) && (index.parent(old) != dir || !index.name(old).eq_ignore_ascii_case(&item.name)));
        match elsewhere {
            Some(old) => {
                let moved = index.rename(old, dir, &item.name);
                index.update_meta(moved, item.meta);
                self.stats.moved += 1;
            }
            None => {
                let new = index.insert(dir, &item.name, item.meta);
                // A junction or link leads to a folder indexed where it really is: reading it
                // would take that folder's subfolders for moves and re-parent them under it.
                if !item.alias {
                    self.new_dirs.push(new);
                }
                self.stats.inserted += 1;
            }
        }
    }

    /// The folders added as new since this was last called. A new folder is not necessarily
    /// empty: it may have been moved in, contents and all, from somewhere the index does not
    /// cover or no longer holds it (a batch of changes can end between a folder leaving one
    /// place and arriving in another). Nothing else will name its contents, so a caller that
    /// follows changes lists these folders too. Junctions and links are never among them.
    pub fn take_new_dirs(&mut self) -> Vec<EntryId> {
        std::mem::take(&mut self.new_dirs)
    }

    /// Places the remaining new files and removes entries that did not reappear anywhere.
    pub fn finish(mut self, index: &mut Index) -> SyncStats {
        let mut missing_by_id: HashMap<u64, Vec<EntryId>> = HashMap::new();
        for &id in &self.missing {
            if !index.is_deleted(id) && index.file_id(id) != 0 {
                missing_by_id.entry(index.file_id(id)).or_default().push(id);
            }
        }
        // The names in each folder new files go into, read when first needed. A batch whose
        // caller released the lock between listings (the live index does, while it reads the
        // disk) may find that another reconciliation, such as a folder being opened, added the
        // same file meanwhile; adding it again would list it twice.
        let mut present: HashMap<EntryId, HashMap<Vec<u8>, EntryId>> = HashMap::new();
        for (dir, item) in std::mem::take(&mut self.pending) {
            let key = fold(&item.name).into_bytes();
            if !index.is_deleted(dir) {
                let names = present
                    .entry(dir)
                    .or_insert_with(|| index.children(dir).map(|child| (index.lower_bytes(child).to_vec(), child)).collect());
                if let Some(&existing) = names.get(&key)
                    && !index.is_deleted(existing)
                    && index.is_dir(existing) == is_dir(&item)
                {
                    index.update_meta(existing, item.meta);
                    self.stats.updated += 1;
                    continue;
                }
            }
            let moved_from = missing_by_id.get_mut(&item.meta.file_id).and_then(|ids| {
                let at = ids.iter().position(|&id| !index.is_deleted(id) && index.is_dir(id) == is_dir(&item))?;
                Some(ids.swap_remove(at))
            });
            let placed = match moved_from {
                Some(old) => {
                    let moved = index.rename(old, dir, &item.name);
                    index.update_meta(moved, item.meta);
                    self.stats.moved += 1;
                    moved
                }
                None => {
                    self.stats.inserted += 1;
                    index.insert(dir, &item.name, item.meta)
                }
            };
            if let Some(names) = present.get_mut(&dir) {
                names.insert(key, placed);
            }
        }
        for id in self.missing {
            if !index.is_deleted(id) {
                index.remove(id);
                self.stats.removed += 1;
            }
        }
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexBuilder, Meta};

    fn file(name: &str, size: u64, file_id: u64) -> DirItem {
        DirItem { name: name.into(), meta: Meta { size, alloc: size, file_id, ..Meta::default() }, alias: false }
    }

    fn folder(name: &str, file_id: u64) -> DirItem {
        DirItem { name: name.into(), meta: Meta { attrs: attr::DIRECTORY, file_id, ..Meta::default() }, alias: false }
    }

    /// C:\ { a (dir 10) { deep (dir 11) { x.txt 5 (21) } }, b (dir 12), one.txt 3 (20) }
    fn sample() -> Index {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let a = b.push(root, "a", folder("a", 10).meta);
        let deep = b.push(a, "deep", folder("deep", 11).meta);
        b.push(deep, "x.txt", file("x.txt", 5, 21).meta);
        b.push(root, "b", folder("b", 12).meta);
        b.push(root, "one.txt", file("one.txt", 3, 20).meta);
        b.finish()
    }

    fn children(index: &Index, path: &str) -> Vec<String> {
        let dir = index.lookup(path).unwrap();
        let mut names: Vec<String> = index.children(dir).map(|c| index.name(c).to_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_folder_moved_between_listed_folders_keeps_its_contents() {
        let mut index = sample();
        let (a, b) = (index.lookup(r"C:\a").unwrap(), index.lookup(r"C:\b").unwrap());
        let mut sync = Sync::default();
        // `deep` moved from a to b; b happens to be listed first.
        sync.dir(&mut index, b, vec![folder("deep", 11)]);
        sync.dir(&mut index, a, vec![]);
        let stats = sync.finish(&mut index);
        assert_eq!((stats.moved, stats.removed, stats.inserted), (1, 0, 0));
        assert_eq!(index.path(index.lookup(r"C:\b\deep\x.txt").unwrap()), r"C:\b\deep\x.txt");
        assert!(index.lookup(r"C:\a\deep").is_none());
        assert_eq!(index.size(index.lookup(r"C:\b").unwrap()), 5);
        assert_eq!(index.size(index.lookup(r"C:\a").unwrap()), 0);
    }

    #[test]
    fn a_folder_moved_in_from_an_unlisted_folder_still_keeps_its_contents() {
        let mut index = sample();
        let b = index.lookup(r"C:\b").unwrap();
        let mut sync = Sync::default();
        sync.dir(&mut index, b, vec![folder("deep", 11)]);
        sync.finish(&mut index);
        assert!(index.lookup(r"C:\b\deep\x.txt").is_some());
    }

    #[test]
    fn files_are_added_removed_renamed_and_updated() {
        let mut index = sample();
        let root = index.roots()[0];
        let mut sync = Sync::default();
        // one.txt grew and was renamed in case only; new.txt appeared; nothing else changed.
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12), file("ONE.txt", 30, 20), file("new.txt", 7, 22)]);
        let stats = sync.finish(&mut index);
        assert_eq!((stats.inserted, stats.removed, stats.moved), (1, 0, 0));
        assert_eq!(children(&index, r"C:\"), ["ONE.txt", "a", "b", "new.txt"]);
        assert_eq!(index.size(index.roots()[0]), 5 + 30 + 7);
        assert_eq!(index.entries_with_file_id(22).len(), 1);

        let root = index.roots()[0];
        let mut sync = Sync::default();
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12)]);
        assert_eq!(sync.finish(&mut index).removed, 2);
        assert_eq!(index.size(index.roots()[0]), 5);
    }

    #[test]
    fn a_file_moved_between_listed_folders_is_a_move_and_a_new_hard_link_is_not() {
        let mut index = sample();
        let (root, b) = (index.roots()[0], index.lookup(r"C:\b").unwrap());
        let mut sync = Sync::default();
        sync.dir(&mut index, b, vec![file("one.txt", 3, 20)]);
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12)]);
        assert_eq!(sync.finish(&mut index).moved, 1);
        assert_eq!(children(&index, r"C:\b"), ["one.txt"]);

        // A second link to x.txt appears in b while the original stays in a\deep.
        let b = index.lookup(r"C:\b").unwrap();
        let mut sync = Sync::default();
        sync.dir(&mut index, b, vec![file("one.txt", 3, 20), file("x-link.txt", 5, 21)]);
        let stats = sync.finish(&mut index);
        assert_eq!((stats.inserted, stats.moved), (1, 0));
        assert!(index.lookup(r"C:\a\deep\x.txt").is_some());
        assert_eq!(index.entries_with_file_id(21).len(), 2);
    }

    /// The live index releases the lock between reading folders and placing new files; a
    /// folder opened meanwhile adds the same new files, which must then not be added twice.
    #[test]
    fn a_new_file_another_batch_added_meanwhile_is_not_added_twice() {
        let mut index = sample();
        let b = index.lookup(r"C:\b").unwrap();
        let mut slow = Sync::default();
        slow.dir(&mut index, b, vec![file("new.txt", 7, 30), file("Other.txt", 2, 31)]);
        let mut quick = Sync::default();
        quick.dir(&mut index, b, vec![file("NEW.txt", 7, 30)]);
        assert_eq!(quick.finish(&mut index).inserted, 1);
        let stats = slow.finish(&mut index);
        assert_eq!((stats.inserted, stats.updated), (1, 1));
        assert_eq!(children(&index, r"C:\b"), ["NEW.txt", "Other.txt"]);
        assert_eq!(index.entries_with_file_id(30).len(), 1);
        assert_eq!(index.size(index.lookup(r"C:\b").unwrap()), 9);
    }

    #[test]
    fn a_file_replaced_by_a_folder_of_the_same_name_is_swapped() {
        let mut index = sample();
        let root = index.roots()[0];
        let mut sync = Sync::default();
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12), folder("one.txt", 30)]);
        sync.finish(&mut index);
        assert!(index.is_dir(index.lookup(r"C:\one.txt").unwrap()));
    }

    #[test]
    fn a_folder_renamed_in_place_keeps_its_contents() {
        let mut index = sample();
        let root = index.roots()[0];
        let mut sync = Sync::default();
        sync.dir(&mut index, root, vec![folder("renamed", 10), folder("b", 12), file("one.txt", 3, 20)]);
        let stats = sync.finish(&mut index);
        assert_eq!((stats.moved, stats.removed, stats.inserted), (1, 0, 0));
        assert!(index.lookup(r"C:\renamed\deep\x.txt").is_some());
        assert!(index.lookup(r"C:\a").is_none());
    }

    #[test]
    fn folders_added_as_new_are_reported_and_moved_ones_are_not() {
        let mut index = sample();
        let (root, b) = (index.roots()[0], index.lookup(r"C:\b").unwrap());
        let mut sync = Sync::default();
        // `deep` moved from a to b (it keeps its contents); `fresh` is unknown.
        sync.dir(&mut index, b, vec![folder("deep", 11), folder("fresh", 30), file("f.txt", 1, 31)]);
        let fresh = index.lookup(r"C:\b\fresh").unwrap();
        assert_eq!(sync.take_new_dirs(), [fresh]);
        assert!(sync.take_new_dirs().is_empty(), "each new folder is reported once");
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12), file("one.txt", 3, 20)]);
        assert!(sync.take_new_dirs().is_empty());
        sync.finish(&mut index);
    }

    #[test]
    fn a_new_junction_is_added_but_not_reported_for_listing() {
        let mut index = sample();
        let b = index.lookup(r"C:\b").unwrap();
        let mut sync = Sync::default();
        let link = DirItem { alias: true, ..folder("link", 40) };
        sync.dir(&mut index, b, vec![link, folder("fresh", 41)]);
        let fresh = index.lookup(r"C:\b\fresh").unwrap();
        assert_eq!(sync.take_new_dirs(), [fresh], "only the real folder is listed next");
        sync.finish(&mut index);
        assert!(index.lookup(r"C:\b\link").is_some_and(|link| index.is_dir(link)));
    }

    #[test]
    fn a_new_folder_can_be_filled_in_the_same_batch() {
        let mut index = sample();
        let root = index.roots()[0];
        let mut sync = Sync::default();
        sync.dir(&mut index, root, vec![folder("a", 10), folder("b", 12), file("one.txt", 3, 20), folder("fresh", 30)]);
        let fresh = index.lookup(r"C:\fresh").expect("a new folder is added straight away");
        sync.dir(&mut index, fresh, vec![file("inside.txt", 9, 31)]);
        sync.finish(&mut index);
        assert_eq!(index.size(index.lookup(r"C:\fresh").unwrap()), 9);
    }
}
