use std::cmp::Ordering;

use kestrel_index::{EntryId, Index};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    /// By extension, then name.
    Type,
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
    // Extension ids are assigned in discovery order, so rank them alphabetically once.
    let ext_rank: Vec<u16> = if sort.key == SortKey::Type {
        let mut by_name: Vec<u16> = (0..index.ext_count()).map(|e| e as u16).collect();
        by_name.sort_unstable_by_key(|&e| index.ext_name(e));
        let mut rank = vec![0; by_name.len()];
        for (position, &ext) in by_name.iter().enumerate() {
            rank[ext as usize] = position as u16;
        }
        rank
    } else {
        Vec::new()
    };

    let compare = |&a: &EntryId, &b: &EntryId| -> Ordering {
        let by_key = match sort.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Size => index.size(a).cmp(&index.size(b)),
            SortKey::Modified => index.mtime(a).cmp(&index.mtime(b)),
            SortKey::Type => ext_rank[index.ext(a) as usize].cmp(&ext_rank[index.ext(b) as usize]),
        };
        let ordering = by_key.then_with(|| index.cmp_name(a, b));
        let ordering = if sort.descending { ordering.reverse() } else { ordering };
        if sort.folders_first {
            index.is_dir(b).cmp(&index.is_dir(a)).then(ordering)
        } else {
            ordering
        }
    };
    if ids.len() >= PARALLEL_THRESHOLD {
        ids.par_sort_unstable_by(compare);
    } else {
        ids.sort_unstable_by(compare);
    }
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
}
