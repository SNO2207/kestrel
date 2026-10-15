use rayon::prelude::*;

use crate::index::{EntryId, ExtTable, Index, Meta, NO_ENTRY, attr};
use crate::natural::natural_cmp;

/// Collects entries in any order and lays them out into an [`Index`].
///
/// Entries may reference parents that are pushed later. [`IndexBuilder::finish`] reorders
/// everything depth-first so that each directory's descendants occupy one contiguous id range
/// and every parent precedes its children.
#[derive(Default)]
pub struct IndexBuilder {
    names: Vec<u8>,
    name_off: Vec<u32>,
    parent: Vec<u32>,
    meta: Vec<Meta>,
    roots: Vec<u32>,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self { name_off: vec![0], ..Self::default() }
    }

    pub fn len(&self) -> u32 {
        self.parent.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Adds a root such as `C:` or `C:\Users\me` (no trailing backslash).
    pub fn add_root(&mut self, name: &str) -> u32 {
        let id = self.push(NO_ENTRY, name, Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        self.roots.push(id);
        id
    }

    /// Adds an entry and returns its temporary id. `parent` may be set later with
    /// [`IndexBuilder::set_parent`]; entries left without a reachable root are dropped.
    pub fn push(&mut self, parent: u32, name: &str, meta: Meta) -> u32 {
        let id = self.len();
        self.names.extend_from_slice(name.as_bytes());
        self.name_off.push(self.names.len() as u32);
        self.parent.push(parent);
        self.meta.push(meta);
        id
    }

    pub fn set_parent(&mut self, id: u32, parent: u32) {
        self.parent[id as usize] = parent;
    }

    fn name(&self, id: u32) -> &str {
        let i = id as usize;
        let bytes = &self.names[self.name_off[i] as usize..self.name_off[i + 1] as usize];
        // SAFETY: `push` only ever appends whole `&str` values.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    fn is_dir(&self, id: u32) -> bool {
        self.meta[id as usize].attrs & attr::DIRECTORY != 0
    }

    /// Children of every entry in compressed sparse row form: `starts[p]..starts[p + 1]` indexes
    /// into the returned child list.
    fn child_lists(&self) -> (Vec<u32>, Vec<u32>) {
        let n = self.parent.len();
        let mut starts = vec![0u32; n + 1];
        let linked = |p: u32| (p as usize) < n;
        for &p in &self.parent {
            if linked(p) {
                starts[p as usize + 1] += 1;
            }
        }
        for i in 0..n {
            starts[i + 1] += starts[i];
        }
        let mut fill = starts.clone();
        let mut children = vec![0u32; starts[n] as usize];
        for (id, &p) in self.parent.iter().enumerate() {
            if linked(p) {
                children[fill[p as usize] as usize] = id as u32;
                fill[p as usize] += 1;
            }
        }
        (starts, children)
    }

    /// Depth-first order of every entry reachable from a root: a directory, then its files,
    /// then each subdirectory's subtree.
    fn layout(&self) -> Vec<u32> {
        let (starts, children) = self.child_lists();
        let kids = |d: u32| &children[starts[d as usize] as usize..starts[d as usize + 1] as usize];
        let mut order = Vec::with_capacity(self.parent.len());
        let mut stack: Vec<u32> = self.roots.iter().rev().copied().collect();
        while let Some(dir) = stack.pop() {
            order.push(dir);
            order.extend(kids(dir).iter().filter(|&&c| !self.is_dir(c)));
            stack.extend(kids(dir).iter().rev().filter(|&&c| self.is_dir(c)));
        }
        order
    }

    pub fn finish(self) -> Index {
        let order = self.layout();
        let n = order.len();
        let mut new_id = vec![NO_ENTRY; self.parent.len()];
        for (new, &old) in order.iter().enumerate() {
            new_id[old as usize] = new as u32;
        }

        let mut idx = Index {
            names: Vec::with_capacity(self.names.len() + n),
            lower: Vec::with_capacity(self.names.len() + n),
            name_off: Vec::with_capacity(n + 1),
            parent: Vec::with_capacity(n),
            first_child: Vec::with_capacity(n),
            next_sibling: Vec::with_capacity(n),
            subtree_end: Vec::with_capacity(n),
            size: Vec::with_capacity(n),
            alloc: Vec::with_capacity(n),
            mtime: Vec::with_capacity(n),
            attrs: Vec::with_capacity(n),
            ext: Vec::with_capacity(n),
            file_id: Vec::with_capacity(n),
            name_rank: Vec::new(),
            file_ids: Vec::new(),
            file_ids_added: Default::default(),
            // Nothing is recorded per entry while building; the sorted lookup comes at the end.
            file_ids_covered: u32::MAX,
            journal: Vec::new(),
            exts: ExtTable::new(),
            roots: self.roots.iter().map(|&r| new_id[r as usize]).collect(),
            base_len: n as u32,
            moved: false,
            live: 0,
        };
        idx.name_off.push(0);

        for &old in &order {
            let mut meta = self.meta[old as usize];
            if self.is_dir(old) {
                meta.size = 0;
                meta.alloc = 0;
            }
            let parent = match self.parent[old as usize] {
                NO_ENTRY => NO_ENTRY,
                p => new_id[p as usize],
            };
            idx.push_raw(parent, self.name(old), meta);
        }

        // Parents precede children, so one backwards pass links sibling lists in ascending id
        // order and rolls sizes and subtree bounds up to every ancestor.
        for i in (0..n).rev() {
            let p = idx.parent[i];
            if p == NO_ENTRY {
                continue;
            }
            let p = p as usize;
            idx.next_sibling[i] = idx.first_child[p];
            idx.first_child[p] = i as EntryId;
            idx.size[p] += idx.size[i];
            idx.alloc[p] += idx.alloc[i];
            idx.subtree_end[p] = idx.subtree_end[p].max(idx.subtree_end[i]);
        }

        idx.name_rank = rank_names(&idx);
        idx.rebuild_file_ids();
        idx
    }

    pub fn meta(&self, id: u32) -> Meta {
        self.meta[id as usize]
    }

    pub fn meta_mut(&mut self, id: u32) -> &mut Meta {
        &mut self.meta[id as usize]
    }

    /// Sets the file id of an entry already pushed, such as a root.
    pub fn set_file_id(&mut self, id: u32, file_id: u64) {
        self.meta[id as usize].file_id = file_id;
    }
}

/// The position of each entry when all entries are sorted by natural name order.
fn rank_names(idx: &Index) -> Vec<u32> {
    let mut sorted: Vec<u32> = (0..idx.len()).collect();
    sorted.par_sort_unstable_by(|&a, &b| {
        natural_cmp(idx.lower_bytes(a), idx.lower_bytes(b)).then(a.cmp(&b))
    });
    let mut rank = vec![0u32; sorted.len()];
    for (pos, &id) in sorted.iter().enumerate() {
        rank[id as usize] = pos as u32;
    }
    rank
}
