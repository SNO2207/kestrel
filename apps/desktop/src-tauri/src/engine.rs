//! The index, the result sets the UI is looking at, and the operations over them.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as Atomic};

use kestrel_index::sync::Sync;
use std::time::Instant;

use kestrel_index::walk::{self, DirItem};
use kestrel_index::{EntryId, Index, IndexBuilder, attr, fold, natural_cmp};
use kestrel_query::group::attribute_key;
use kestrel_query::{Clause, ClauseKind, Clock, Group, Grouping, Query, Sort, SortKey, compile, parse, search, sort};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::visibility::Visibility;

/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Result sets kept alive at once. The UI releases sets it no longer shows, so this only
/// bounds leaks.
const MAX_SETS: usize = 64;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Name,
    Size,
    Modified,
    Type,
    /// The details view's extra columns (`layout.rs`): allocated size, attribute letters,
    /// extension and the folder an item is in.
    Alloc,
    Attributes,
    Extension,
    Path,
}

#[derive(Clone, Copy, Deserialize)]
pub struct SortSpec {
    pub key: SortField,
    pub desc: bool,
}

impl SortSpec {
    fn resolve(self, folders_first: bool) -> Sort {
        let key = match self.key {
            SortField::Name => SortKey::Name,
            SortField::Size => SortKey::Size,
            SortField::Modified => SortKey::Modified,
            // The Type column is the extension's, so the two sort alike.
            SortField::Type | SortField::Extension => SortKey::Type,
            SortField::Alloc => SortKey::Alloc,
            SortField::Attributes => SortKey::Attributes,
            SortField::Path => SortKey::Path,
        };
        Sort { key, descending: self.desc, folders_first }
    }
}

#[derive(Serialize)]
pub struct Listing {
    /// Handle for fetching rows.
    pub set: u32,
    pub total: u32,
    /// The folder shown, with its on-disk capitalisation. Empty for search results.
    pub path: String,
    /// Largest size in the set, for scaling the size bars.
    pub max_size: u64,
    pub took_us: u64,
    /// For a folder inside an archive, the archive's path on disk; such a listing is read-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
}

/// Selected rows and the focused row of a result set, as inclusive ranges of positions.
#[derive(Default, Deserialize, Serialize)]
pub struct Marks {
    pub ranges: Vec<(u32, u32)>,
    pub focus: Option<u32>,
}

/// What identifies a row across two result sets.
#[derive(PartialEq, Eq, Hash)]
enum RowKey {
    Entry(EntryId),
    Path(String),
}

/// The rows some `Marks` refer to, captured as identities before the rows move.
struct Kept {
    rows: HashSet<RowKey>,
    focus: Option<RowKey>,
    by_path: bool,
}

#[derive(Serialize)]
pub struct Row {
    pub name: String,
    pub path: String,
    /// Containing folder, sent for search results only.
    pub location: Option<String>,
    pub dir: bool,
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime: i64,
    pub ext: String,
    pub hidden: bool,
    /// Bytes allocated on disk: the details view's Allocated size column.
    pub alloc: u64,
    /// The file attributes: the details view's Attributes column, and a cloud file's sync
    /// status (`parity::syncroots`).
    pub attrs: u32,
}

#[derive(Clone, Serialize)]
pub struct IndexStatus {
    /// `loading`, `scanning` (first index being built) or `ready`.
    pub state: &'static str,
    pub entries: u32,
    /// A background rescan is replacing a loaded index.
    pub refreshing: bool,
    /// Bumped whenever the index is replaced; result sets from older generations are gone.
    pub generation: u64,
}

enum Rows {
    Indexed(Vec<EntryId>),
    /// A folder outside the index, read directly from disk.
    Live { dir: String, items: Vec<DirItem> },
}

struct ResultSet {
    rows: Rows,
    is_search: bool,
    /// The sort the rows follow, and how they are grouped; see `layout.rs`.
    arranged: Arranged,
}

#[derive(Default)]
struct Sets {
    next: u32,
    map: HashMap<u32, ResultSet>,
}

pub struct Engine {
    index: RwLock<Index>,
    generation: AtomicU64,
    sets: Mutex<Sets>,
    /// Cancels the search in flight when a newer one starts.
    cancel: Mutex<Arc<AtomicBool>>,
    status: Mutex<IndexStatus>,
    /// The index has changed since it was last saved.
    dirty: AtomicBool,
    /// Bumped whenever the index changes or is replaced, so features that derive data from
    /// it (such as project discovery) can tell when to look again.
    revision: AtomicU64,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            index: RwLock::new(IndexBuilder::new().finish()),
            generation: AtomicU64::new(0),
            sets: Mutex::default(),
            cancel: Mutex::default(),
            status: Mutex::new(IndexStatus {
                state: "loading",
                entries: 0,
                refreshing: false,
                generation: 0,
            }),
            dirty: AtomicBool::new(false),
            revision: AtomicU64::new(0),
        }
    }
}

/// Normalises a path typed or clicked by the user: backslashes, `C:` becomes `C:\`, and no
/// trailing separator elsewhere.
pub fn normalize(path: &str) -> String {
    let mut path = path.trim().replace('/', "\\");
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    if path.len() == 2 && path.ends_with(':') {
        path.push('\\');
    }
    path
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

fn unix_ms(filetime: i64) -> i64 {
    (filetime - FILETIME_UNIX_EPOCH) / 10_000
}

fn is_hidden(attrs: u32) -> bool {
    attrs & attr::HIDDEN != 0
}

impl Engine {
    pub fn status(&self) -> IndexStatus {
        self.status.lock().clone()
    }

    /// Updates the status and returns the new value for broadcasting.
    pub fn set_status(&self, state: &'static str, refreshing: bool) -> IndexStatus {
        let mut status = self.status.lock();
        status.state = state;
        status.refreshing = refreshing;
        status.entries = self.index.read().live_count();
        status.generation = self.generation.load(Atomic::Relaxed);
        status.clone()
    }

    /// Replaces the index. Every existing result set refers to the old one and is dropped.
    pub fn install(&self, index: Index) {
        *self.index.write() = index;
        self.generation.fetch_add(1, Atomic::Relaxed);
        self.revision.fetch_add(1, Atomic::Relaxed);
        self.sets.lock().map.clear();
    }

    /// Changes whenever the index does; see [`Engine::mark_dirty`] and [`Engine::install`].
    pub fn revision(&self) -> u64 {
        self.revision.load(Atomic::Relaxed)
    }

    #[cfg(test)]
    fn with_index<T>(&self, f: impl FnOnce(&Index) -> T) -> T {
        f(&self.index.read())
    }

    /// The index, for the background work that keeps it current.
    pub fn index(&self) -> &RwLock<Index> {
        &self.index
    }

    pub fn live_count(&self) -> u32 {
        self.index.read().live_count()
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Atomic::Relaxed);
        self.revision.fetch_add(1, Atomic::Relaxed);
    }

    /// Whether the index changed since this was last asked, clearing the flag.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Atomic::Relaxed)
    }

    /// Frees a result set the UI has stopped showing. The UI calls this only after switching
    /// to the replacement, so a row request still in flight for the old set is never mistaken
    /// for a sign that the index was replaced.
    pub fn release(&self, set: u32) {
        self.sets.lock().map.remove(&set);
    }

    fn store(&self, set: ResultSet) -> u32 {
        let mut sets = self.sets.lock();
        while sets.map.len() >= MAX_SETS {
            let oldest = *sets.map.keys().min().unwrap();
            sets.map.remove(&oldest);
        }
        sets.next += 1;
        let id = sets.next;
        sets.map.insert(id, set);
        id
    }

    /// Lists a folder from disk and brings the index into line with what was found, so the
    /// listing is always current even when the index is stale. Hidden items are included: the
    /// commands use [`Engine::list_dir_with`], and this stays for callers that want everything.
    #[allow(dead_code)]
    pub fn list_dir(&self, path: &str, spec: SortSpec) -> Result<Listing, String> {
        self.list_dir_with(path, spec, Visibility::ALL)
    }

    /// [`Engine::list_dir`], leaving out the entries `visibility` excludes.
    pub fn list_dir_with(&self, path: &str, spec: SortSpec, visibility: Visibility) -> Result<Listing, String> {
        let started = Instant::now();
        let path = normalize(path);
        let items = walk::list(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => crate::access::LOCKED.to_owned(),
            _ => format!("Can't open {path}: {e}"),
        })?;
        let sort_spec = spec.resolve(true);

        let mut index = self.index.write();
        // A junction or folder link is listed through to the folder it leads to, which the
        // index holds where it really is: reconciling that listing with the link's entry would
        // re-parent the real folder's subfolders under the link. Such folders are shown live.
        let indexed = index
            .lookup(&path)
            .filter(|&dir| index.is_dir(dir))
            .filter(|&dir| index.attrs(dir) & attr::REPARSE_POINT == 0 || !walk::is_alias(&path));
        let (rows, shown_path, max_size) = match indexed {
            Some(dir) => {
                let mut sync = Sync::default();
                sync.dir(&mut index, dir, items);
                sync.finish(&mut index);
                let mut ids: Vec<EntryId> =
                    index.children(dir).filter(|&id| visibility.shows(index.attrs(id))).collect();
                sort(&index, &mut ids, sort_spec);
                let max = ids.iter().map(|&id| index.size(id)).max().unwrap_or(0);
                (Rows::Indexed(ids), index.path(dir), max)
            }
            None => {
                let mut items = items;
                items.retain(|item| visibility.shows(item.meta.attrs));
                sort_live(&mut items, sort_spec);
                let max = items.iter().map(|i| i.meta.size).max().unwrap_or(0);
                (Rows::Live { dir: path.clone(), items }, path, max)
            }
        };
        drop(index);

        let total = rows.len();
        let set = self.store(ResultSet { rows, is_search: false, arranged: Arranged::sorted(sort_spec) });
        Ok(Listing { set, total, path: shown_path, max_size, took_us: started.elapsed().as_micros() as u64, archive: None })
    }

    /// Shows items that are not on disk as a folder at `dir`: the contents of a folder inside
    /// the archive at `archive`. Rows are paged, sorted, grouped and selected as any folder's are.
    pub fn show_items(&self, dir: String, mut items: Vec<DirItem>, spec: SortSpec, visibility: Visibility, archive: String) -> Listing {
        let started = Instant::now();
        let sort_spec = spec.resolve(true);
        items.retain(|item| visibility.shows(item.meta.attrs));
        sort_live(&mut items, sort_spec);
        let max_size = items.iter().map(|item| item.meta.size).max().unwrap_or(0);
        let total = items.len() as u32;
        let set = self.store(ResultSet { rows: Rows::Live { dir: dir.clone(), items }, is_search: false, arranged: Arranged::sorted(sort_spec) });
        Listing { set, total, path: dir, max_size, took_us: started.elapsed().as_micros() as u64, archive: Some(archive) }
    }

    /// Runs a query, optionally restricted to the folder `scope`. Hidden items are included,
    /// as for [`Engine::list_dir`].
    #[allow(dead_code)]
    pub fn search(&self, text: &str, scope: Option<&str>, spec: SortSpec) -> Result<Listing, String> {
        self.search_with(text, scope, spec, Visibility::ALL)
    }

    /// [`Engine::search`], leaving out the entries `visibility` excludes.
    pub fn search_with(
        &self,
        text: &str,
        scope: Option<&str>,
        spec: SortSpec,
        visibility: Visibility,
    ) -> Result<Listing, String> {
        self.search_resolving(text, scope, spec, visibility, |_, _| Ok(()))
    }

    /// [`Engine::search_with`], with `resolve` filling in, while the index is held, what the
    /// index can't know by itself, such as which entries carry a tag.
    pub fn search_resolving(
        &self,
        text: &str,
        scope: Option<&str>,
        spec: SortSpec,
        visibility: Visibility,
        resolve: impl FnOnce(&mut Query, &Index) -> Result<(), String>,
    ) -> Result<Listing, String> {
        let started = Instant::now();
        let mut query = parse(text, Clock::local()).map_err(|e| e.to_string())?;
        if let Some(scope) = scope {
            query.clauses.push(Clause { negate: false, kind: ClauseKind::Within(normalize(scope)) });
        }

        let cancel = Arc::new(AtomicBool::new(false));
        std::mem::replace(&mut *self.cancel.lock(), cancel.clone()).store(true, Atomic::Relaxed);

        let index = self.index.read();
        resolve(&mut query, &index)?;
        let compiled = compile(&query, &index).map_err(|e| e.to_string())?;
        let mut ids = search(&index, &compiled, &cancel).ok_or("Superseded by a newer search")?;
        if visibility != Visibility::ALL {
            ids.retain(|&id| visibility.shows(index.attrs(id)));
        }
        sort(&index, &mut ids, spec.resolve(false));
        let max_size = ids.iter().map(|&id| index.size(id)).max().unwrap_or(0);
        let rows = Rows::Indexed(ids);
        drop(index);

        let total = rows.len();
        let set = self.store(ResultSet { rows, is_search: true, arranged: Arranged::sorted(spec.resolve(false)) });
        Ok(Listing { set, total, path: String::new(), max_size, took_us: started.elapsed().as_micros() as u64, archive: None })
    }

    /// Finds where the rows marked in the set `from` sit in the set `to`, so a selection can
    /// follow its files from one listing of a folder to the next.
    pub fn carry(&self, from: u32, to: u32, marks: &Marks) -> Marks {
        let index = self.index.read();
        let sets = self.sets.lock();
        let (Some(old), Some(new)) = (sets.map.get(&from), sets.map.get(&to)) else {
            return Marks::default();
        };
        // Entries keep their ids across listings of the same index; anything else is matched
        // by path.
        let by_path = !matches!((&old.rows, &new.rows), (Rows::Indexed(_), Rows::Indexed(_)));
        new.rows.locate(&index, &old.rows.capture(&index, marks, by_path))
    }

    /// Re-sorts a result set in place and reports where the marked rows moved to.
    pub fn sort_set(&self, set: u32, spec: SortSpec, marks: &Marks) -> Result<Marks, String> {
        let index = self.index.read();
        let mut sets = self.sets.lock();
        let entry = sets.map.get_mut(&set).ok_or("stale")?;
        let kept = entry.rows.capture(&index, marks, false);
        // A grouped set stays grouped, now sorted this way within each group.
        let grouping = entry.arranged.groups.as_ref().map(|(grouping, _)| *grouping);
        entry.ungroup();
        entry.sort_by(&index, spec.resolve(!entry.is_search));
        entry.group_by(&index, grouping);
        Ok(entry.rows.locate(&index, &kept))
    }

    /// The positions of the rows with the given names, as inclusive ranges. Used to select
    /// items that an operation just created.
    pub fn locate_names(&self, set: u32, names: &[String]) -> Vec<(u32, u32)> {
        let wanted: HashSet<String> = names.iter().map(|name| fold(name)).collect();
        let index = self.index.read();
        let sets = self.sets.lock();
        let Some(set) = sets.map.get(&set) else { return Vec::new() };
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for row in 0..set.rows.len() {
            let found = match &set.rows {
                Rows::Indexed(ids) => {
                    // Names in the index are already folded.
                    std::str::from_utf8(index.lower_bytes(ids[row as usize])).is_ok_and(|n| wanted.contains(n))
                }
                Rows::Live { items, .. } => wanted.contains(&fold(&items[row as usize].name)),
            };
            if found {
                match ranges.last_mut() {
                    Some(last) if last.1 + 1 == row => last.1 = row,
                    _ => ranges.push((row, row)),
                }
            }
        }
        ranges
    }

    /// The full paths of the rows in the given inclusive ranges.
    pub fn paths(&self, set: u32, ranges: &[(u32, u32)]) -> Result<Vec<String>, String> {
        let index = self.index.read();
        let sets = self.sets.lock();
        let rows = &sets.map.get(&set).ok_or("stale")?.rows;
        Ok(rows.clamp(ranges).map(|row| rows.path(&index, row)).collect())
    }

    pub fn rows(&self, set: u32, start: u32, count: u32) -> Result<Vec<Row>, String> {
        let index = self.index.read();
        let sets = self.sets.lock();
        let set = sets.map.get(&set).ok_or("stale")?;
        let range = |len: usize| {
            let start = (start as usize).min(len);
            start..(start + count as usize).min(len)
        };
        Ok(match &set.rows {
            Rows::Indexed(ids) => ids[range(ids.len())]
                .iter()
                .map(|&id| {
                    let path = index.path(id);
                    let location = set.is_search.then(|| match index.parent(id) {
                        kestrel_index::NO_ENTRY => String::new(),
                        parent => index.path(parent),
                    });
                    Row {
                        name: index.name(id).to_owned(),
                        path,
                        location,
                        dir: index.is_dir(id),
                        size: index.size(id),
                        mtime: unix_ms(index.mtime(id)),
                        ext: index.ext_name(index.ext(id)).to_owned(),
                        hidden: is_hidden(index.attrs(id)),
                        alloc: index.alloc(id),
                        attrs: index.attrs(id) & !attr::DELETED,
                    }
                })
                .collect(),
            Rows::Live { dir, items } => items[range(items.len())]
                .iter()
                .map(|item| {
                    let is_dir = item.meta.attrs & attr::DIRECTORY != 0;
                    let ext = match item.name.rsplit_once('.') {
                        Some((stem, ext)) if !is_dir && !stem.is_empty() => ext.to_ascii_lowercase(),
                        _ => String::new(),
                    };
                    Row {
                        name: item.name.clone(),
                        path: join(dir, &item.name),
                        location: None,
                        dir: is_dir,
                        size: item.meta.size,
                        mtime: unix_ms(item.meta.mtime),
                        ext,
                        hidden: is_hidden(item.meta.attrs),
                        alloc: item.meta.alloc,
                        attrs: item.meta.attrs,
                    }
                })
                .collect(),
        })
    }

    /// The position of the first row at or after `from` whose name starts with `prefix`,
    /// wrapping around. Used for type-to-select.
    pub fn find_prefix(&self, set: u32, prefix: &str, from: u32) -> Option<u32> {
        let prefix = fold(prefix);
        let index = self.index.read();
        let sets = self.sets.lock();
        let set = sets.map.get(&set)?;
        let len = set.rows.len();
        (0..len).map(|step| (from + step) % len).find(|&row| match &set.rows {
            Rows::Indexed(ids) => index.lower_bytes(ids[row as usize]).starts_with(prefix.as_bytes()),
            Rows::Live { items, .. } => fold(&items[row as usize].name).starts_with(&prefix),
        })
    }
}

impl Rows {
    fn len(&self) -> u32 {
        match self {
            Rows::Indexed(ids) => ids.len() as u32,
            Rows::Live { items, .. } => items.len() as u32,
        }
    }

    fn path(&self, index: &Index, row: u32) -> String {
        match self {
            Rows::Indexed(ids) => index.path(ids[row as usize]),
            Rows::Live { dir, items } => join(dir, &items[row as usize].name),
        }
    }

    /// The rows covered by `ranges`, ignoring anything past the end of the set.
    fn clamp<'a>(&self, ranges: &'a [(u32, u32)]) -> impl Iterator<Item = u32> + 'a {
        let len = self.len();
        ranges.iter().flat_map(move |&(start, end)| start..end.saturating_add(1).min(len))
    }

    fn key(&self, index: &Index, row: u32, by_path: bool) -> RowKey {
        match self {
            Rows::Indexed(ids) if !by_path => RowKey::Entry(ids[row as usize]),
            _ => RowKey::Path(self.path(index, row).to_lowercase()),
        }
    }

    fn capture(&self, index: &Index, marks: &Marks, by_path: bool) -> Kept {
        Kept {
            rows: self.clamp(&marks.ranges).map(|row| self.key(index, row, by_path)).collect(),
            focus: marks.focus.filter(|&row| row < self.len()).map(|row| self.key(index, row, by_path)),
            by_path,
        }
    }

    /// The positions in this set of the rows captured in `kept`.
    fn locate(&self, index: &Index, kept: &Kept) -> Marks {
        let mut marks = Marks::default();
        if kept.rows.is_empty() && kept.focus.is_none() {
            return marks;
        }
        for row in 0..self.len() {
            let key = self.key(index, row, kept.by_path);
            if kept.focus.as_ref() == Some(&key) {
                marks.focus = Some(row);
            }
            if kept.rows.contains(&key) {
                match marks.ranges.last_mut() {
                    Some(last) if last.1 + 1 == row => last.1 = row,
                    _ => marks.ranges.push((row, row)),
                }
            }
        }
        marks
    }
}

// ---------- grouping and the filter box (`layout.rs`) ----------

/// A group of a result set, as the list draws its header.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct GroupHeader {
    pub label: String,
    pub start: u32,
    pub count: u32,
}

/// How a set's rows are grouped, and the plain sorted order behind the groups, so changing the
/// grouping (or the sort within it) never has to sort a million rows again from scratch.
#[derive(Default)]
struct Arranged {
    /// The sort the rows follow (or, while an indexed set is grouped, `plain` follows), when
    /// known.
    sorted: Option<Sort>,
    groups: Option<(Grouping, Vec<Group>)>,
    /// While an indexed set is grouped, its rows in plain sorted order.
    plain: Option<Vec<EntryId>>,
}

impl Arranged {
    fn sorted(sort: Sort) -> Self {
        Self { sorted: Some(sort), ..Self::default() }
    }

    fn starts(&self) -> Vec<u32> {
        self.groups.as_ref().map_or_else(Vec::new, |(_, groups)| groups.iter().map(|g| g.start).collect())
    }
}

impl ResultSet {
    /// Puts the rows back in plain sorted order, out of any groups.
    fn ungroup(&mut self) {
        let was_grouped = self.arranged.groups.take().is_some();
        match (&mut self.rows, self.arranged.plain.take()) {
            (Rows::Indexed(ids), Some(plain)) => *ids = plain,
            // Grouping rearranged a folder listed from disk; it is sorted again, which is cheap.
            (Rows::Live { .. }, _) if was_grouped => self.arranged.sorted = None,
            _ => {}
        }
    }

    /// Sorts the (ungrouped) rows, unless they already are sorted that way.
    fn sort_by(&mut self, index: &Index, spec: Sort) {
        if self.arranged.sorted == Some(spec) {
            return;
        }
        match &mut self.rows {
            Rows::Indexed(ids) => sort(index, ids, spec),
            Rows::Live { items, .. } => sort_live(items, spec),
        }
        self.arranged.sorted = Some(spec);
    }

    /// Groups the (ungrouped, sorted) rows, keeping the plain order of an indexed set aside.
    fn group_by(&mut self, index: &Index, grouping: Option<Grouping>) {
        let Some(grouping) = grouping else { return };
        match &self.rows {
            Rows::Indexed(ids) => self.arranged.plain = Some(ids.clone()),
            Rows::Live { .. } => self.arranged.sorted = None,
        }
        self.arranged.groups = Some((grouping, self.rows.group(index, grouping)));
    }
}

impl Engine {
    /// Sorts a set by `spec` and groups it by `grouping`, or leaves it ungrouped with `None`,
    /// and reports where the marked rows went and where each group starts.
    pub fn group_set(
        &self,
        set: u32,
        spec: SortSpec,
        grouping: Option<Grouping>,
        marks: &Marks,
    ) -> Result<(Marks, Vec<u32>), String> {
        let index = self.index.read();
        let mut sets = self.sets.lock();
        let entry = sets.map.get_mut(&set).ok_or("stale")?;
        let kept = entry.rows.capture(&index, marks, false);
        entry.ungroup();
        entry.sort_by(&index, spec.resolve(!entry.is_search));
        entry.group_by(&index, grouping);
        Ok((entry.rows.locate(&index, &kept), entry.arranged.starts()))
    }

    /// The groups `from` to `from + count` of a grouped set, for the headers in view.
    pub fn group_headers(&self, set: u32, from: u32, count: u32) -> Result<Vec<GroupHeader>, String> {
        let sets = self.sets.lock();
        let entry = sets.map.get(&set).ok_or("stale")?;
        let Some((_, groups)) = &entry.arranged.groups else { return Ok(Vec::new()) };
        let total = entry.rows.len();
        let from = (from as usize).min(groups.len());
        let to = (from + count as usize).min(groups.len());
        Ok((from..to)
            .map(|i| {
                let end = groups.get(i + 1).map_or(total, |next| next.start);
                GroupHeader { label: groups[i].label.clone(), start: groups[i].start, count: end - groups[i].start }
            })
            .collect())
    }

    /// A new set holding the rows of `base` whose names pass `filter`, sorted by `spec` and
    /// grouped by `grouping`. `base` is left as it is, so the filter can change and be applied
    /// to it again.
    pub fn filter_set(
        &self,
        base: u32,
        filter: &kestrel_query::NameFilter,
        spec: SortSpec,
        grouping: Option<Grouping>,
    ) -> Result<(Listing, Vec<u32>), String> {
        use rayon::prelude::*;
        let started = Instant::now();
        let index = self.index.read();
        let sets = self.sets.lock();
        let entry = sets.map.get(&base).ok_or("stale")?;
        let is_search = entry.is_search;
        // Taken from the plain sorted order, so the narrowed rows are sorted already.
        let rows = match (&entry.rows, &entry.arranged.plain) {
            (Rows::Indexed(_), Some(plain)) | (Rows::Indexed(plain), None) => {
                Rows::Indexed(plain.par_iter().copied().filter(|&id| filter.matches(index.lower_bytes(id))).collect())
            }
            (Rows::Live { dir, items }, _) => Rows::Live {
                dir: dir.clone(),
                items: items.iter().filter(|item| filter.matches(fold(&item.name).as_bytes())).cloned().collect(),
            },
        };
        let mut narrowed = ResultSet { rows, is_search, arranged: Arranged { sorted: entry.arranged.sorted, ..Arranged::default() } };
        drop(sets);
        narrowed.sort_by(&index, spec.resolve(!is_search));
        narrowed.group_by(&index, grouping);
        let max_size = match &narrowed.rows {
            Rows::Indexed(ids) => ids.iter().map(|&id| index.size(id)).max().unwrap_or(0),
            Rows::Live { items, .. } => items.iter().map(|item| item.meta.size).max().unwrap_or(0),
        };
        drop(index);
        let starts = narrowed.arranged.starts();
        let total = narrowed.rows.len();
        let set = self.store(narrowed);
        let took_us = started.elapsed().as_micros() as u64;
        Ok((Listing { set, total, path: String::new(), max_size, took_us, archive: None }, starts))
    }
}

impl Rows {
    /// Groups the rows, keeping their order within each group.
    fn group(&mut self, index: &Index, grouping: Grouping) -> Vec<Group> {
        let edges = kestrel_query::DateEdges::local();
        match self {
            Rows::Indexed(ids) => kestrel_query::group::group(index, ids, grouping, &edges),
            Rows::Live { items, .. } => {
                let exts: Vec<String> = items.iter().map(live_extension).collect();
                let mut order: Vec<u32> = (0..items.len() as u32).collect();
                let groups = kestrel_query::group::group_rows(
                    &mut order,
                    |&i| kestrel_query::group::Facts {
                        name: &items[i as usize].name,
                        ext: &exts[i as usize],
                        dir: items[i as usize].meta.attrs & attr::DIRECTORY != 0,
                        size: items[i as usize].meta.size,
                        mtime: items[i as usize].meta.mtime,
                    },
                    grouping,
                    &edges,
                );
                let arranged = order.iter().map(|&i| items[i as usize].clone()).collect();
                *items = arranged;
                groups
            }
        }
    }
}

/// The lower-case extension of an item listed from disk, as the Type column shows it.
fn live_extension(item: &DirItem) -> String {
    match item.name.rsplit_once('.') {
        Some((stem, ext)) if item.meta.attrs & attr::DIRECTORY == 0 && !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn sort_live(items: &mut [DirItem], spec: Sort) {
    let is_dir = |item: &DirItem| item.meta.attrs & attr::DIRECTORY != 0;
    let by_name = |a: &DirItem, b: &DirItem| {
        natural_cmp(fold(&a.name).as_bytes(), fold(&b.name).as_bytes())
    };
    let extension = |item: &DirItem| {
        item.name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default()
    };
    items.sort_by(|a, b| {
        let by_key = match spec.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortKey::Type => extension(a).cmp(&extension(b)),
            SortKey::Alloc => a.meta.alloc.cmp(&b.meta.alloc),
            SortKey::Attributes => attribute_key(a.meta.attrs).cmp(&attribute_key(b.meta.attrs)),
            // Everything listed from disk is in the same folder.
            SortKey::Path => Ordering::Equal,
        };
        let ordering = by_key.then_with(|| by_name(a, b));
        let ordering = if spec.descending { ordering.reverse() } else { ordering };
        if spec.folders_first { is_dir(b).cmp(&is_dir(a)).then(ordering) } else { ordering }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const BY_NAME: SortSpec = SortSpec { key: SortField::Name, desc: false };

    fn names(engine: &Engine, listing: &Listing) -> Vec<String> {
        engine.rows(listing.set, 0, 100).unwrap().into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("c:"), r"c:\");
        assert_eq!(normalize(r"C:\"), r"C:\");
        assert_eq!(normalize("C:/Users/me/"), r"C:\Users\me");
    }

    #[test]
    fn listing_heals_a_stale_index() {
        let root = std::env::temp_dir().join(format!("kestrel-engine-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("old.txt"), b"1234").unwrap();
        fs::write(root.join("sub/inner.txt"), b"12").unwrap();
        let root_str = root.to_str().unwrap().to_owned();

        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&root_str, &mut builder).unwrap();
        engine.install(builder.finish());

        // The disk changes behind the index's back.
        fs::remove_file(root.join("old.txt")).unwrap();
        fs::write(root.join("new.txt"), b"123456").unwrap();

        let listing = engine.list_dir(&root_str, BY_NAME).unwrap();
        assert_eq!(names(&engine, &listing), ["sub", "new.txt"]);
        assert_eq!(listing.max_size, 6);
        // The folder's size reflects the reconciled contents: inner.txt (2) + new.txt (6).
        assert_eq!(engine.with_index(|index| index.size(index.lookup(&root_str).unwrap())), 8);

        let found = engine.search("new", Some(&root_str), BY_NAME).unwrap();
        assert_eq!(names(&engine, &found), ["new.txt"]);
        assert!(engine.rows(listing.set, 0, 1).is_ok(), "a set stays readable until released");
        engine.release(listing.set);
        assert!(engine.rows(listing.set, 0, 1).is_err());
        assert_eq!(engine.search("old", Some(&root_str), BY_NAME).unwrap().total, 0);

        assert_eq!(engine.find_prefix(found.set, "NE", 0), Some(0));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folders_outside_the_index_are_listed_live() {
        let root = std::env::temp_dir().join(format!("kestrel-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("b-dir")).unwrap();
        fs::write(root.join("a.TXT"), b"123").unwrap();

        let engine = Engine::default();
        let listing = engine.list_dir(root.to_str().unwrap(), BY_NAME).unwrap();
        let rows = engine.rows(listing.set, 0, 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["b-dir", "a.TXT"]);
        assert_eq!(rows[1].ext, "txt");
        assert_eq!(rows[1].path, format!(r"{}\a.TXT", root.to_str().unwrap()));
        fs::remove_dir_all(&root).unwrap();
    }

    /// Opening a junction lists the folder it leads to. That listing used to be reconciled with
    /// the junction's own entry, which moved the real folder's subfolders under the link.
    #[test]
    fn opening_a_junction_lists_it_live_and_leaves_the_index_alone() {
        let root = std::env::temp_dir().join(format!("kestrel-engine-junction-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real/sub")).unwrap();
        fs::write(root.join("real/sub/inner.txt"), b"inner").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(root.join("link"))
            .arg(root.join("real"))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
        let root_str = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&root_str, &mut builder).unwrap();
        engine.install(builder.finish());
        assert!(walk::is_alias(&format!(r"{root_str}\link")));
        assert!(!walk::is_alias(&format!(r"{root_str}\real")));

        let listing = engine.list_dir(&format!(r"{root_str}\link"), BY_NAME).unwrap();
        assert_eq!(names(&engine, &listing), ["sub"]);
        engine.with_index(|index| {
            let sub = index.lookup(&format!(r"{root_str}\real\sub")).expect("the real folder keeps its subfolder");
            assert_eq!(index.children(sub).count(), 1);
            let link = index.lookup(&format!(r"{root_str}\link")).unwrap();
            assert_eq!(index.children(link).count(), 0, "the junction gains no children");
        });
        fs::remove_dir(root.join("link")).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    /// Creates `count` small files named `f0.txt`, `f1.txt`... in a fresh folder and indexes it.
    fn indexed_folder(name: &str, count: usize) -> (Engine, std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("kestrel-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for i in 0..count {
            fs::write(root.join(format!("f{i}.txt")), vec![b'x'; i + 1]).unwrap();
        }
        let root_str = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&root_str, &mut builder).unwrap();
        engine.install(builder.finish());
        (engine, root, root_str)
    }

    #[test]
    fn selection_follows_its_files_through_refresh_and_sort() {
        let (engine, root, root_str) = indexed_folder("carry", 5);
        let listing = engine.list_dir(&root_str, BY_NAME).unwrap();
        assert_eq!(engine.paths(listing.set, &[(1, 2), (9, 20)]).unwrap().len(), 2);
        let wanted = ["F3.TXT".to_owned(), "f0.txt".to_owned(), "f4.txt".to_owned(), "nope".to_owned()];
        assert_eq!(engine.locate_names(listing.set, &wanted), [(0, 0), (3, 4)]);

        // f1 and f2 are selected and f2 has focus; then f0 disappears and a0 appears.
        let marks = Marks { ranges: vec![(1, 2)], focus: Some(2) };
        fs::remove_file(root.join("f0.txt")).unwrap();
        fs::write(root.join("a0.txt"), b"x").unwrap();
        let refreshed = engine.list_dir(&root_str, BY_NAME).unwrap();
        assert_eq!(names(&engine, &refreshed), ["a0.txt", "f1.txt", "f2.txt", "f3.txt", "f4.txt"]);
        let marks = engine.carry(listing.set, refreshed.set, &marks);
        assert_eq!((marks.ranges.clone(), marks.focus), (vec![(1, 2)], Some(2)));

        // Largest first: f4, f3, f2, f1, a0.
        let by_size = SortSpec { key: SortField::Size, desc: true };
        let marks = engine.sort_set(refreshed.set, by_size, &marks).unwrap();
        assert_eq!(names(&engine, &refreshed), ["f4.txt", "f3.txt", "f2.txt", "f1.txt", "a0.txt"]);
        assert_eq!((marks.ranges.clone(), marks.focus), (vec![(2, 3)], Some(2)));

        // A selected file that is deleted simply drops out of the selection.
        fs::remove_file(root.join("f2.txt")).unwrap();
        let after = engine.list_dir(&root_str, BY_NAME).unwrap();
        let marks = engine.carry(refreshed.set, after.set, &marks);
        assert_eq!((marks.ranges, marks.focus), (vec![(1, 1)], None));
        // Carrying from a set that no longer exists selects nothing rather than failing.
        engine.release(refreshed.set);
        assert!(engine.carry(refreshed.set, after.set, &Marks { ranges: vec![(0, 9)], focus: None }).ranges.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
