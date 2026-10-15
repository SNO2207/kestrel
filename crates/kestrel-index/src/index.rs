use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;

use crate::fold::{fold, fold_into};
use crate::natural::natural_cmp;

pub type EntryId = u32;
pub const NO_ENTRY: EntryId = u32::MAX;

/// Windows file attribute bits stored per entry, plus one private bit.
pub mod attr {
    pub const READONLY: u32 = 0x1;
    pub const HIDDEN: u32 = 0x2;
    pub const SYSTEM: u32 = 0x4;
    pub const DIRECTORY: u32 = 0x10;
    pub const REPARSE_POINT: u32 = 0x400;
    pub const COMPRESSED: u32 = 0x800;
    pub const OFFLINE: u32 = 0x1000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    /// Tombstone. Windows never sets bit 31, so it is free for the index.
    pub const DELETED: u32 = 0x8000_0000;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// Logical size in bytes. Ignored for directories, whose size is the sum of their subtree.
    pub size: u64,
    /// Bytes allocated on disk. Zero for cloud-only placeholders.
    pub alloc: u64,
    /// Last write time as a Windows FILETIME (100 ns ticks since 1601-01-01 UTC).
    pub mtime: i64,
    pub attrs: u32,
    pub file_id: u64,
}

/// One entry of a directory as read from disk.
#[derive(Clone, Debug)]
pub struct DirItem {
    pub name: String,
    pub meta: Meta,
    /// A junction or symbolic link to a folder: another name for a folder that lives
    /// elsewhere. It is indexed as an entry of its own but never read through, or the folders
    /// it leads to would be taken for moves and re-parented under it.
    pub alias: bool,
}

/// How far an index has caught up with a volume's change journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalCursor {
    /// The volume's root, such as `C:\`.
    pub root: String,
    /// Identifies the journal instance; a different id means the journal was recreated and
    /// the changes since the cursor are unknown.
    pub journal_id: u64,
    /// The first change not yet applied.
    pub next_usn: i64,
}

/// Interned lower-case file extensions. Id 0 means "no extension".
#[derive(Default)]
pub(crate) struct ExtTable {
    ids: HashMap<Box<str>, u16>,
    pub(crate) names: Vec<Box<str>>,
}

impl ExtTable {
    pub(crate) fn new() -> Self {
        Self { ids: HashMap::new(), names: vec!["".into()] }
    }

    pub(crate) fn from_names(names: Vec<Box<str>>) -> Self {
        let ids = names.iter().enumerate().skip(1).map(|(i, n)| (n.clone(), i as u16)).collect();
        Self { ids, names }
    }

    fn intern(&mut self, folded_name: &[u8]) -> u16 {
        let Some(ext) = extension_of(folded_name) else { return 0 };
        if let Some(&id) = self.ids.get(ext) {
            return id;
        }
        if self.names.len() > u16::MAX as usize {
            return 0;
        }
        let id = self.names.len() as u16;
        self.names.push(ext.into());
        self.ids.insert(ext.into(), id);
        id
    }
}

/// The extension of a folded file name: the part after the last dot, if it looks like one.
fn extension_of(folded_name: &[u8]) -> Option<&str> {
    let dot = folded_name.iter().rposition(|&b| b == b'.')?;
    let ext = &folded_name[dot + 1..];
    let plausible = dot > 0
        && (1..=16).contains(&ext.len())
        && ext.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'~'));
    // Every accepted byte is ASCII, so the slice is valid UTF-8.
    plausible.then(|| std::str::from_utf8(ext).unwrap())
}

pub struct Index {
    /// Display names, each followed by a NUL.
    pub(crate) names: Vec<u8>,
    /// Folded names, byte-for-byte aligned with `names`.
    pub(crate) lower: Vec<u8>,
    /// Start of each name; one extra element marks the end of the arena.
    pub(crate) name_off: Vec<u32>,
    pub(crate) parent: Vec<EntryId>,
    pub(crate) first_child: Vec<EntryId>,
    pub(crate) next_sibling: Vec<EntryId>,
    /// One past the last descendant, for entries laid out by the builder.
    pub(crate) subtree_end: Vec<EntryId>,
    pub(crate) size: Vec<u64>,
    pub(crate) alloc: Vec<u64>,
    pub(crate) mtime: Vec<i64>,
    pub(crate) attrs: Vec<u32>,
    pub(crate) ext: Vec<u16>,
    pub(crate) file_id: Vec<u64>,
    /// Position of each built entry in natural name order.
    pub(crate) name_rank: Vec<u32>,
    /// `(record number << 32) | entry` for every entry with a file id, sorted, so an entry can
    /// be found from the file reference a change journal reports. The low 32 bits of a file
    /// reference are the record number; the full id is checked against `file_id` on lookup.
    pub(crate) file_ids: Vec<u64>,
    /// Entries `file_ids` does not cover: those added, or given a new id, after it was built.
    pub(crate) file_ids_added: HashMap<u64, Vec<EntryId>>,
    /// Entries below this id are covered by `file_ids`.
    pub(crate) file_ids_covered: u32,
    pub(crate) journal: Vec<JournalCursor>,
    pub(crate) exts: ExtTable,
    pub(crate) roots: Vec<EntryId>,
    /// Entries below this id were laid out depth-first by the builder.
    pub(crate) base_len: u32,
    /// Set once a directory has moved, after which `subtree_end` no longer bounds subtrees.
    pub(crate) moved: bool,
    pub(crate) live: u32,
}

impl Index {
    /// Total number of entries, including tombstones.
    pub fn len(&self) -> u32 {
        self.parent.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Number of entries that have not been deleted.
    pub fn live_count(&self) -> u32 {
        self.live
    }

    pub fn roots(&self) -> &[EntryId] {
        &self.roots
    }

    pub fn name(&self, id: EntryId) -> &str {
        // SAFETY: the arena is only ever extended with whole `&str` values.
        unsafe { std::str::from_utf8_unchecked(self.name_bytes(id)) }
    }

    pub fn name_bytes(&self, id: EntryId) -> &[u8] {
        &self.names[self.name_range(id)]
    }

    /// The folded name, for matching against a folded query.
    pub fn lower_bytes(&self, id: EntryId) -> &[u8] {
        &self.lower[self.name_range(id)]
    }

    fn name_range(&self, id: EntryId) -> Range<usize> {
        let i = id as usize;
        self.name_off[i] as usize..self.name_off[i + 1] as usize - 1
    }

    /// The whole folded arena. Names are separated by NUL and ordered by entry id.
    pub fn lower_arena(&self) -> &[u8] {
        &self.lower
    }

    /// Arena offset where each name starts, plus a final element marking the arena end.
    pub fn name_offsets(&self) -> &[u32] {
        &self.name_off
    }

    pub fn parent(&self, id: EntryId) -> EntryId {
        self.parent[id as usize]
    }

    /// Logical size. For a directory this is the total of everything beneath it.
    pub fn size(&self, id: EntryId) -> u64 {
        self.size[id as usize]
    }

    /// Size on disk. For a directory this is the total of everything beneath it.
    pub fn alloc(&self, id: EntryId) -> u64 {
        self.alloc[id as usize]
    }

    pub fn mtime(&self, id: EntryId) -> i64 {
        self.mtime[id as usize]
    }

    pub fn attrs(&self, id: EntryId) -> u32 {
        self.attrs[id as usize]
    }

    pub fn file_id(&self, id: EntryId) -> u64 {
        self.file_id[id as usize]
    }

    pub fn is_dir(&self, id: EntryId) -> bool {
        self.attrs[id as usize] & attr::DIRECTORY != 0
    }

    pub fn is_deleted(&self, id: EntryId) -> bool {
        self.attrs[id as usize] & attr::DELETED != 0
    }

    pub fn ext(&self, id: EntryId) -> u16 {
        self.ext[id as usize]
    }

    pub fn ext_name(&self, ext: u16) -> &str {
        &self.exts.names[ext as usize]
    }

    /// Number of distinct extensions, including the empty one with id 0.
    pub fn ext_count(&self) -> usize {
        self.exts.names.len()
    }

    /// Looks up an extension given in lower case without the dot.
    pub fn ext_id(&self, ext: &str) -> Option<u16> {
        self.exts.ids.get(ext).copied()
    }

    /// Live entries with this file id. Usually one; hard links give several.
    pub fn entries_with_file_id(&self, file_id: u64) -> Vec<EntryId> {
        if file_id == 0 {
            return Vec::new();
        }
        let record = file_id as u32 as u64;
        let start = self.file_ids.partition_point(|&packed| packed >> 32 < record);
        let mut found: Vec<EntryId> = self.file_ids[start..]
            .iter()
            .take_while(|&&packed| packed >> 32 == record)
            .map(|&packed| packed as u32)
            .collect();
        found.extend(self.file_ids_added.get(&file_id).into_iter().flatten());
        found.retain(|&id| self.file_id[id as usize] == file_id && !self.is_deleted(id));
        found.sort_unstable();
        found.dedup();
        found
    }

    /// The sorted file id lookup over every live entry.
    pub(crate) fn sorted_file_ids(&self) -> Vec<u64> {
        let mut packed: Vec<u64> = (0..self.len())
            .filter(|&id| self.file_id[id as usize] != 0 && !self.is_deleted(id))
            .map(|id| (self.file_id[id as usize] as u32 as u64) << 32 | id as u64)
            .collect();
        rayon::slice::ParallelSliceMut::par_sort_unstable(packed.as_mut_slice());
        packed
    }

    pub(crate) fn rebuild_file_ids(&mut self) {
        self.file_ids = self.sorted_file_ids();
        self.file_ids_added.clear();
        self.file_ids_covered = self.len();
    }

    fn note_file_id(&mut self, id: EntryId, file_id: u64) {
        if file_id != 0 && id >= self.file_ids_covered {
            self.file_ids_added.entry(file_id).or_default().push(id);
        }
    }

    /// Records a file id for an entry created without one, such as a crawl root.
    pub fn set_file_id(&mut self, id: EntryId, file_id: u64) {
        self.file_id[id as usize] = file_id;
        // The sorted lookup may hold this entry under its old id; the extra mapping wins.
        if file_id != 0 {
            self.file_ids_added.entry(file_id).or_default().push(id);
        }
    }

    /// Where this index stands in each volume's change journal.
    pub fn journal(&self) -> &[JournalCursor] {
        &self.journal
    }

    pub fn set_journal(&mut self, cursors: Vec<JournalCursor>) {
        self.journal = cursors;
    }

    /// Advances the cursor for `root`, adding one if there is none.
    pub fn advance_journal(&mut self, root: &str, journal_id: u64, next_usn: i64) {
        match self.journal.iter_mut().find(|c| c.root.eq_ignore_ascii_case(root)) {
            Some(cursor) => {
                cursor.journal_id = journal_id;
                cursor.next_usn = next_usn;
            }
            None => self.journal.push(JournalCursor { root: root.to_owned(), journal_id, next_usn }),
        }
    }

    /// Live direct children, in no particular order.
    pub fn children(&self, id: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        let mut next = self.first_child[id as usize];
        std::iter::from_fn(move || {
            while next != NO_ENTRY {
                let cur = next;
                next = self.next_sibling[cur as usize];
                if !self.is_deleted(cur) {
                    return Some(cur);
                }
            }
            None
        })
    }

    /// The id range holding every descendant of `dir`, when the layout still guarantees one.
    /// Entries appended after the build are not covered; see [`Index::base_len`].
    pub fn subtree_range(&self, dir: EntryId) -> Option<Range<EntryId>> {
        (!self.moved && dir < self.base_len).then(|| dir + 1..self.subtree_end[dir as usize])
    }

    /// Entries with ids at or above this were appended after the last build.
    pub fn base_len(&self) -> u32 {
        self.base_len
    }

    /// Whether `id` lies somewhere beneath `dir`.
    pub fn is_under(&self, id: EntryId, dir: EntryId) -> bool {
        if id < self.base_len
            && let Some(range) = self.subtree_range(dir)
        {
            return range.contains(&id);
        }
        let mut cur = self.parent[id as usize];
        while cur != NO_ENTRY {
            if cur == dir {
                return true;
            }
            cur = self.parent[cur as usize];
        }
        false
    }

    pub fn path(&self, id: EntryId) -> String {
        let mut out = String::new();
        self.write_path(id, &mut out);
        out
    }

    /// Appends the full path of `id`, such as `C:\Users\me\file.txt`.
    pub fn write_path(&self, id: EntryId, out: &mut String) {
        // SAFETY: both arenas hold only whole `&str` values and `\` is ASCII.
        self.write_path_from(&self.names, id, unsafe { out.as_mut_vec() });
    }

    /// Appends the folded full path of `id`, for matching path queries.
    pub fn write_lower_path(&self, id: EntryId, out: &mut Vec<u8>) {
        self.write_path_from(&self.lower, id, out);
    }

    fn write_path_from(&self, arena: &[u8], id: EntryId, out: &mut Vec<u8>) {
        let mut chain = [NO_ENTRY; 64];
        let mut depth = 0;
        let mut cur = id;
        while cur != NO_ENTRY && depth < chain.len() {
            chain[depth] = cur;
            depth += 1;
            cur = self.parent[cur as usize];
        }
        for (n, &e) in chain[..depth].iter().rev().enumerate() {
            if n > 0 {
                out.push(b'\\');
            }
            out.extend_from_slice(&arena[self.name_range(e)]);
        }
        // A bare drive root is written `C:\`, not `C:`.
        if depth == 1 && out.last() == Some(&b':') {
            out.push(b'\\');
        }
    }

    /// Finds the entry for an absolute path. Case-insensitive; accepts either slash.
    pub fn lookup(&self, path: &str) -> Option<EntryId> {
        let folded = fold(path.strip_prefix(r"\\?\").unwrap_or(path));
        let folded = folded.replace('/', "\\");
        // Roots may themselves be multi-component paths, so match the longest root prefix.
        let (root, rest) = self
            .roots
            .iter()
            .filter_map(|&r| {
                let rest = folded.as_bytes().strip_prefix(self.lower_bytes(r))?;
                (rest.is_empty() || rest[0] == b'\\').then_some((r, rest))
            })
            .min_by_key(|(_, rest)| rest.len())?;
        let mut cur = root;
        for part in rest.split(|&b| b == b'\\').filter(|p| !p.is_empty()) {
            cur = self.children(cur).find(|&c| self.lower_bytes(c) == part)?;
        }
        Some(cur)
    }

    /// Orders two entries by natural name order, ties broken by id.
    pub fn cmp_name(&self, a: EntryId, b: EntryId) -> Ordering {
        let ranked = self.name_rank.len() as u32;
        if a < ranked && b < ranked {
            return self.name_rank[a as usize].cmp(&self.name_rank[b as usize]);
        }
        natural_cmp(self.lower_bytes(a), self.lower_bytes(b)).then(a.cmp(&b))
    }

    /// Appends an entry without linking it to its parent or touching aggregate sizes.
    pub(crate) fn push_raw(&mut self, parent: EntryId, name: &str, meta: Meta) -> EntryId {
        let id = self.len();
        self.names.extend_from_slice(name.as_bytes());
        self.names.push(0);
        fold_into(name, &mut self.lower);
        self.lower.push(0);
        let folded_start = self.name_off[id as usize] as usize;
        let ext = if meta.attrs & attr::DIRECTORY != 0 {
            0
        } else {
            self.exts.intern(&self.lower[folded_start..self.lower.len() - 1])
        };
        self.name_off.push(self.names.len() as u32);
        self.parent.push(parent);
        self.first_child.push(NO_ENTRY);
        self.next_sibling.push(NO_ENTRY);
        self.subtree_end.push(id + 1);
        self.size.push(meta.size);
        self.alloc.push(meta.alloc);
        self.mtime.push(meta.mtime);
        self.attrs.push(meta.attrs & !attr::DELETED);
        self.ext.push(ext);
        self.file_id.push(meta.file_id);
        self.note_file_id(id, meta.file_id);
        self.live += 1;
        id
    }

    fn link(&mut self, id: EntryId, parent: EntryId) {
        self.parent[id as usize] = parent;
        self.next_sibling[id as usize] = self.first_child[parent as usize];
        self.first_child[parent as usize] = id;
    }

    /// Adds `size`/`alloc` deltas to every ancestor of `id`.
    fn adjust_ancestors(&mut self, id: EntryId, size: i64, alloc: i64) {
        let mut cur = self.parent[id as usize];
        while cur != NO_ENTRY {
            let c = cur as usize;
            self.size[c] = self.size[c].wrapping_add_signed(size);
            self.alloc[c] = self.alloc[c].wrapping_add_signed(alloc);
            cur = self.parent[c];
        }
    }

    /// Adds a new entry under `parent`.
    pub fn insert(&mut self, parent: EntryId, name: &str, mut meta: Meta) -> EntryId {
        if meta.attrs & attr::DIRECTORY != 0 {
            meta.size = 0;
            meta.alloc = 0;
        }
        let id = self.push_raw(NO_ENTRY, name, meta);
        self.link(id, parent);
        self.adjust_ancestors(id, meta.size as i64, meta.alloc as i64);
        id
    }

    /// Tombstones `id` and everything beneath it.
    pub fn remove(&mut self, id: EntryId) {
        if self.is_deleted(id) {
            return;
        }
        let (size, alloc) = (self.size[id as usize] as i64, self.alloc[id as usize] as i64);
        self.adjust_ancestors(id, -size, -alloc);
        let mut stack = vec![id];
        while let Some(e) = stack.pop() {
            stack.extend(self.children(e));
            self.attrs[e as usize] |= attr::DELETED;
            self.live -= 1;
        }
    }

    /// Renames and/or moves an entry. The entry gets a new id, which is returned; its children
    /// are carried over.
    pub fn rename(&mut self, id: EntryId, new_parent: EntryId, new_name: &str) -> EntryId {
        let i = id as usize;
        let meta = Meta {
            size: self.size[i],
            alloc: self.alloc[i],
            mtime: self.mtime[i],
            attrs: self.attrs[i],
            file_id: self.file_id[i],
        };
        self.adjust_ancestors(id, -(meta.size as i64), -(meta.alloc as i64));

        let new = self.push_raw(NO_ENTRY, new_name, meta);
        let first = std::mem::replace(&mut self.first_child[i], NO_ENTRY);
        self.first_child[new as usize] = first;
        let mut child = first;
        while child != NO_ENTRY {
            self.parent[child as usize] = new;
            child = self.next_sibling[child as usize];
        }
        self.moved |= first != NO_ENTRY;

        self.attrs[i] |= attr::DELETED;
        self.live -= 1;
        self.link(new, new_parent);
        self.adjust_ancestors(new, meta.size as i64, meta.alloc as i64);
        new
    }

    /// Updates the metadata of an existing entry, keeping aggregate sizes consistent.
    pub fn update_meta(&mut self, id: EntryId, meta: Meta) {
        let i = id as usize;
        if !self.is_dir(id) {
            let size = meta.size as i64 - self.size[i] as i64;
            let alloc = meta.alloc as i64 - self.alloc[i] as i64;
            self.size[i] = meta.size;
            self.alloc[i] = meta.alloc;
            self.adjust_ancestors(id, size, alloc);
        }
        self.mtime[i] = meta.mtime;
        let keep = attr::DELETED | attr::DIRECTORY;
        self.attrs[i] = (self.attrs[i] & keep) | (meta.attrs & !keep);
        // An editor that saves by writing a new file and renaming it over the old one leaves
        // the same name with a new file id.
        if meta.file_id != 0 && meta.file_id != self.file_id[i] {
            self.set_file_id(id, meta.file_id);
        }
    }

    /// Approximate heap memory used by the index, in bytes.
    pub fn memory_bytes(&self) -> usize {
        let n = self.parent.capacity();
        self.names.capacity()
            + self.lower.capacity()
            + self.name_off.capacity() * 4
            + n * (4 * 4 + 8 * 4 + 4 + 2)
            + self.name_rank.capacity() * 4
            + self.file_ids.capacity() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexBuilder;

    fn file(size: u64) -> Meta {
        Meta { size, alloc: size, ..Meta::default() }
    }

    fn dir() -> Meta {
        Meta { attrs: attr::DIRECTORY, ..Meta::default() }
    }

    /// C:\ { a.txt(10), Docs { b.TXT(20), Sub { c.rs(5) } }, Empty }
    fn sample() -> Index {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        // Pushed out of order on purpose: the builder must cope with forward parent references.
        let c = b.push(NO_ENTRY, "c.rs", file(5));
        let docs = b.push(root, "Docs", dir());
        let sub = b.push(docs, "Sub", dir());
        b.set_parent(c, sub);
        b.push(docs, "b.TXT", file(20));
        b.push(root, "a.txt", file(10));
        b.push(root, "Empty", dir());
        b.finish()
    }

    #[test]
    fn builds_paths_and_sizes() {
        let idx = sample();
        assert_eq!(idx.live_count(), 7);
        let root = idx.roots()[0];
        assert_eq!(idx.path(root), r"C:\");
        assert_eq!(idx.size(root), 35);
        let docs = idx.lookup(r"c:\docs").unwrap();
        assert_eq!(idx.size(docs), 25);
        let c = idx.lookup("C:/Docs/Sub/c.rs").unwrap();
        assert_eq!(idx.path(c), r"C:\Docs\Sub\c.rs");
        assert_eq!(idx.ext_name(idx.ext(c)), "rs");
        assert_eq!(idx.ext_name(idx.ext(idx.lookup(r"C:\Docs\b.TXT").unwrap())), "txt");
        assert!(idx.lookup(r"C:\missing").is_none());
    }

    #[test]
    fn subtrees_are_contiguous() {
        let idx = sample();
        let docs = idx.lookup(r"C:\Docs").unwrap();
        let range = idx.subtree_range(docs).unwrap();
        assert_eq!(range.len(), 3);
        for id in 0..idx.len() {
            let under = idx.path(id).starts_with(r"C:\Docs\");
            assert_eq!(range.contains(&id), under, "{}", idx.path(id));
            assert_eq!(idx.is_under(id, docs), under);
        }
    }

    #[test]
    fn insert_remove_update_keep_sizes_consistent() {
        let mut idx = sample();
        let root = idx.roots()[0];
        let sub = idx.lookup(r"C:\Docs\Sub").unwrap();
        let new = idx.insert(sub, "new.bin", file(100));
        assert_eq!(idx.size(root), 135);
        assert!(idx.is_under(new, idx.lookup(r"C:\Docs").unwrap()));

        idx.update_meta(new, file(40));
        assert_eq!(idx.size(sub), 45);

        idx.remove(idx.lookup(r"C:\Docs").unwrap());
        assert_eq!(idx.size(root), 10);
        assert_eq!(idx.live_count(), 3);
        assert!(idx.lookup(r"C:\Docs\Sub\new.bin").is_none());
    }

    #[test]
    fn rename_moves_a_directory_with_its_children() {
        let mut idx = sample();
        let docs = idx.lookup(r"C:\Docs").unwrap();
        let empty = idx.lookup(r"C:\Empty").unwrap();
        let moved = idx.rename(docs, empty, "Papers");
        assert_eq!(idx.path(moved), r"C:\Empty\Papers");
        assert_eq!(idx.size(empty), 25);
        assert_eq!(idx.size(idx.roots()[0]), 35);
        let c = idx.lookup(r"C:\Empty\Papers\Sub\c.rs").unwrap();
        assert!(idx.is_under(c, empty));
        assert!(idx.lookup(r"C:\Docs").is_none());
        assert_eq!(idx.live_count(), 7);
    }

    #[test]
    fn name_order_is_natural() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        for name in ["f10", "F2", "f1"] {
            b.push(root, name, file(0));
        }
        let idx = b.finish();
        let ordered: Vec<_> = ["f1", "F2", "f10"]
            .iter()
            .map(|name| idx.lookup(&format!(r"C:\{name}")).unwrap())
            .collect();
        assert!(ordered.windows(2).all(|w| idx.cmp_name(w[0], w[1]).is_lt()));
    }
}
