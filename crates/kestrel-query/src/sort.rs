use std::cmp::Ordering;

use kestrel_index::{EntryId, Index, NO_ENTRY};
use rayon::prelude::*;

use crate::group::attribute_key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    /// By extension, then name.
    Type,
    /// Bytes allocated on disk.
    Alloc,
    /// By the attribute letters Explorer shows ("A", "HSA", "RD"...).
    Attributes,
    /// By the folder an item is in, then name.
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
    /// Keep folders ahead of files regardless of direction, as Explorer does in folder views.
    pub folders_first: bool,
}

/// Below this many results a parallel sort costs more than it saves.
const PARALLEL_THRESHOLD: usize = 8192;

pub fn sort(index: &Index, ids: &mut [EntryId], sort: Sort) {
    match sort.key {
        // Keys that are expensive to work out are worked out once per entry, not per comparison.
        SortKey::Attributes => return sort_keyed(index, ids, sort, |id| attribute_key(index.attrs(id))),
        SortKey::Path => {
            let ranks = folder_ranks(index, ids);
            let rank = |id: EntryId| match index.parent(id) {
                NO_ENTRY => 0,
                folder => u64::from(ranks[folder as usize]),
            };
            return sort_keyed(index, ids, sort, rank);
        }
        _ => {}
    }
    // Extension ids are assigned in discovery order, so rank them alphabetically once.
    let ext_rank: Vec<u16> = if sort.key == SortKey::Type { ext_ranks(index) } else { Vec::new() };

    let compare = |&a: &EntryId, &b: &EntryId| -> Ordering {
        let by_key = match sort.key {
            SortKey::Name | SortKey::Attributes | SortKey::Path => Ordering::Equal,
            SortKey::Size => index.size(a).cmp(&index.size(b)),
            SortKey::Modified => index.mtime(a).cmp(&index.mtime(b)),
            SortKey::Type => ext_rank[index.ext(a) as usize].cmp(&ext_rank[index.ext(b) as usize]),
            SortKey::Alloc => index.alloc(a).cmp(&index.alloc(b)),
        };
        finish(index, a, b, by_key, sort)
    };
    if ids.len() >= PARALLEL_THRESHOLD {
        ids.par_sort_unstable_by(compare);
    } else {
        ids.sort_unstable_by(compare);
    }
}

/// Breaks ties by name, applies the direction, and keeps folders first when asked to.
fn finish(index: &Index, a: EntryId, b: EntryId, by_key: Ordering, sort: Sort) -> Ordering {
    let ordering = by_key.then_with(|| index.cmp_name(a, b));
    let ordering = if sort.descending { ordering.reverse() } else { ordering };
    if sort.folders_first { index.is_dir(b).cmp(&index.is_dir(a)).then(ordering) } else { ordering }
}

/// Sorts by a key computed once per entry.
fn sort_keyed(index: &Index, ids: &mut [EntryId], sort: Sort, key: impl Fn(EntryId) -> u64 + Sync) {
    let mut keyed: Vec<(u64, EntryId)> = ids.par_iter().map(|&id| (key(id), id)).collect();
    let compare = |a: &(u64, EntryId), b: &(u64, EntryId)| finish(index, a.1, b.1, a.0.cmp(&b.0), sort);
    if keyed.len() >= PARALLEL_THRESHOLD {
        keyed.par_sort_unstable_by(compare);
    } else {
        keyed.sort_unstable_by(compare);
    }
    for (slot, (_, id)) in ids.iter_mut().zip(keyed) {
        *slot = id;
    }
}

/// Every extension id's position in alphabetical order of the extensions' names.
pub(crate) fn ext_ranks(index: &Index) -> Vec<u16> {
    let mut by_name: Vec<u16> = (0..index.ext_count()).map(|e| e as u16).collect();
    by_name.sort_unstable_by_key(|&e| index.ext_name(e));
    let mut rank = vec![0; by_name.len()];
    for (position, &ext) in by_name.iter().enumerate() {
        rank[ext as usize] = position as u16;
    }
    rank
}

/// The position of each folder holding one of `ids` in the order of their paths, by entry id
/// (from 1; 0 for the rest): folders are compared a part at a time, each part in natural order
/// of its folded name, so a folder comes just before everything beneath it.
///
/// Worked out over the tree of those folders and the folders above them, ordering only each
/// folder's own subfolders, by the names the index keeps folded: a broad search's results lie
/// in hundreds of thousands of folders, and writing out and sorting every one's whole path took
/// ten times as long as sorting the results by name.
fn folder_ranks(index: &Index, ids: &[EntryId]) -> Vec<u32> {
    use std::sync::atomic::{AtomicBool, Ordering as Atomic};
    let len = index.len() as usize;
    // The folders holding results, marked side by side, then read off in id order.
    let holding: Vec<AtomicBool> = (0..len).map(|_| AtomicBool::new(false)).collect();
    ids.par_iter().with_min_len(1 << 14).for_each(|&id| match index.parent(id) {
        NO_ENTRY => {}
        folder => holding[folder as usize].store(true, Atomic::Relaxed),
    });
    let folders: Vec<EntryId> =
        (0..len).into_par_iter().with_min_len(1 << 14).filter(|&at| holding[at].load(Atomic::Relaxed)).map(|at| at as EntryId).collect();
    drop(holding);
    // Every folder above them too, each under its parent; the topmost on their own.
    let mut in_tree = vec![false; len];
    let mut edges: Vec<(EntryId, EntryId)> = Vec::with_capacity(folders.len());
    let mut tops: Vec<EntryId> = Vec::new();
    for &folder in &folders {
        let mut at = folder;
        while at != NO_ENTRY && !in_tree[at as usize] {
            in_tree[at as usize] = true;
            match index.parent(at) {
                NO_ENTRY => tops.push(at),
                parent => edges.push((parent, at)),
            }
            at = index.parent(at);
        }
    }
    // Natural name order, as the index keeps it ranked.
    edges.par_sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| index.cmp_name(a.1, b.1)));
    tops.sort_unstable_by(|&a, &b| index.cmp_name(a, b));
    // Where each folder's subfolders start among the edges, plus one (0 for none).
    let mut first = vec![0u32; len];
    for (at, &(parent, _)) in edges.iter().enumerate().rev() {
        first[parent as usize] = at as u32 + 1;
    }
    // Numbered in preorder: each folder, then what is beneath it, in order.
    let mut ranks = vec![0u32; len];
    let mut next = 1;
    let mut stack: Vec<EntryId> = tops.into_iter().rev().collect();
    let mut below = Vec::new();
    while let Some(folder) = stack.pop() {
        ranks[folder as usize] = next;
        next += 1;
        if let Some(start) = (first[folder as usize] as usize).checked_sub(1) {
            below.clear();
            below.extend(edges[start..].iter().take_while(|edge| edge.0 == folder).map(|&(_, child)| child));
            stack.extend(below.iter().rev());
        }
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta, attr};

    fn names(index: &Index, key: SortKey, descending: bool, folders_first: bool) -> Vec<String> {
        let root = index.roots()[0];
        let mut ids: Vec<EntryId> = index.children(root).collect();
        sort(index, &mut ids, Sort { key, descending, folders_first });
        ids.iter().map(|&id| index.name(id).to_owned()).collect()
    }

    #[test]
    fn orders() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let file = |size, mtime| Meta { size, mtime, ..Meta::default() };
        b.push(root, "b10.txt", file(5, 30));
        b.push(root, "B2.txt", file(50, 10));
        b.push(root, "a.zip", file(20, 20));
        b.push(root, "zeta", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        let idx = b.finish();

        assert_eq!(names(&idx, SortKey::Name, false, true), ["zeta", "a.zip", "B2.txt", "b10.txt"]);
        assert_eq!(names(&idx, SortKey::Name, true, true), ["zeta", "b10.txt", "B2.txt", "a.zip"]);
        assert_eq!(names(&idx, SortKey::Name, false, false), ["a.zip", "B2.txt", "b10.txt", "zeta"]);
        assert_eq!(names(&idx, SortKey::Size, true, false), ["B2.txt", "a.zip", "b10.txt", "zeta"]);
        assert_eq!(names(&idx, SortKey::Modified, false, true), ["zeta", "B2.txt", "a.zip", "b10.txt"]);
        assert_eq!(names(&idx, SortKey::Type, false, false), ["zeta", "B2.txt", "b10.txt", "a.zip"]);
    }

    #[test]
    fn orders_by_the_extra_columns() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let file = |alloc, attrs| Meta { size: 1, alloc, attrs, ..Meta::default() };
        // Archive only; read-only and archive; hidden, system and archive; nothing at all.
        b.push(root, "plain.txt", file(4096, 0x20));
        b.push(root, "locked.txt", file(0, 0x21));
        b.push(root, "secret.sys", file(8192, 0x26));
        b.push(root, "bare.bin", file(12288, 0));
        let idx = b.finish();

        assert_eq!(names(&idx, SortKey::Alloc, false, false), ["locked.txt", "plain.txt", "secret.sys", "bare.bin"]);
        assert_eq!(names(&idx, SortKey::Alloc, true, false), ["bare.bin", "secret.sys", "plain.txt", "locked.txt"]);
        // "" < "A" < "HSA" < "RA", as the letters read.
        assert_eq!(names(&idx, SortKey::Attributes, false, false), ["bare.bin", "plain.txt", "secret.sys", "locked.txt"]);
        assert_eq!(names(&idx, SortKey::Attributes, true, false), ["locked.txt", "secret.sys", "plain.txt", "bare.bin"]);
    }

    #[test]
    fn orders_search_results_by_folder_path_then_name() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let dir = Meta { attrs: attr::DIRECTORY, ..Meta::default() };
        let zebra = b.push(root, "Zebra", dir);
        let apple = b.push(root, "apple", dir);
        let apple10 = b.push(apple, "folder10", dir);
        let apple9 = b.push(apple, "folder9", dir);
        for parent in [zebra, apple, apple10, apple9] {
            b.push(parent, "b.txt", Meta::default());
            b.push(parent, "a.txt", Meta::default());
        }
        let idx = b.finish();
        let mut ids: Vec<EntryId> = (0..idx.len()).filter(|&id| idx.name(id).ends_with(".txt")).collect();
        sort(&idx, &mut ids, Sort { key: SortKey::Path, descending: false, folders_first: false });
        let paths: Vec<String> = ids.iter().map(|&id| idx.path(id)).collect();
        assert_eq!(
            paths,
            [
                r"C:\apple\a.txt",
                r"C:\apple\b.txt",
                r"C:\apple\folder9\a.txt",
                r"C:\apple\folder9\b.txt",
                r"C:\apple\folder10\a.txt",
                r"C:\apple\folder10\b.txt",
                r"C:\Zebra\a.txt",
                r"C:\Zebra\b.txt",
            ]
        );
        sort(&idx, &mut ids, Sort { key: SortKey::Path, descending: true, folders_first: false });
        assert_eq!(idx.path(ids[0]), r"C:\Zebra\b.txt");
    }

    /// Folders are compared a part at a time: what is in a folder comes before a folder beside
    /// it whose name merely starts the same way.
    #[test]
    fn a_folders_contents_come_before_its_neighbours() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let dir = Meta { attrs: attr::DIRECTORY, ..Meta::default() };
        let apple = b.push(root, "apple", dir);
        let deeper = b.push(apple, "x", dir);
        let neighbour = b.push(root, "apple-x", dir);
        for parent in [neighbour, deeper, apple] {
            b.push(parent, "f.txt", Meta::default());
        }
        let idx = b.finish();
        let mut ids: Vec<EntryId> = (0..idx.len()).filter(|&id| idx.name(id) == "f.txt").collect();
        sort(&idx, &mut ids, Sort { key: SortKey::Path, descending: false, folders_first: false });
        let paths: Vec<String> = ids.iter().map(|&id| idx.path(id)).collect();
        assert_eq!(paths, [r"C:\apple\f.txt", r"C:\apple\x\f.txt", r"C:\apple-x\f.txt"]);
    }

    /// Sorting a broad search's results by folder takes about as long as sorting them by name,
    /// not ten times as long: they lie in hundreds of thousands of folders.
    #[test]
    fn sorting_by_folder_costs_about_what_sorting_by_name_does() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let dir = Meta { attrs: attr::DIRECTORY, ..Meta::default() };
        let mut files = Vec::new();
        for top in 0..40 {
            let top = b.push(root, &format!("Projects and other things {top}"), dir);
            for mid in 0..50 {
                let mid = b.push(top, &format!("some folder with a long name {mid}"), dir);
                for leaf in 0..50 {
                    let leaf = b.push(mid, &format!("leaf {leaf}"), dir);
                    for n in 0..4 {
                        files.push(b.push(leaf, &format!("file {n}.txt"), Meta::default()));
                    }
                }
            }
        }
        let idx = b.finish();
        let timed = |key: SortKey| {
            (0..3)
                .map(|_| {
                    let mut ids = files.clone();
                    let started = std::time::Instant::now();
                    sort(&idx, &mut ids, Sort { key, descending: false, folders_first: false });
                    started.elapsed()
                })
                .min()
                .unwrap()
        };
        let (by_name, by_path) = (timed(SortKey::Name), timed(SortKey::Path));
        assert!(by_path < by_name * 3, "by folder {by_path:?}, by name {by_name:?}");
    }
}
