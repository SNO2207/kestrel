//! Browsing inside archives: listing a folder in one, read-only, and getting single items out of
//! one for the preview, for opening, and for copying or dragging elsewhere.
//!
//! An archive's entries are read once into a [`Tree`] of folders, kept for the few archives
//! looked at last and read again when the archive changes. An archive inside an archive is
//! copied out to the cache folder (under the data folder) and browsed from there. Every item
//! taken out goes through the same extraction as "Extract all", with all its checks, and gets
//! the outermost archive's Mark of the Web.
//!
//! A compressed tar (or a RAR, which arrives as one) has no list of contents: listing it means
//! decompressing all of it, which for a large one takes minutes. So an archive is read by one
//! caller at a time while others wait for that reading ([`READING`]); a listing stops when the
//! window that asked no longer wants it (its job is cancelled, see `list_dir`); the palette
//! only looks inside archives already at hand ([`Reading::AtHand`]); taking one item out of a
//! stream reuses the listing already read and stops reading once it has the item; and a
//! preview gives way when another item is wanted, and is refused for an item too far into a
//! stream.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use kestrel_index::{DirItem, Meta, attr};
use parking_lot::Mutex;

use super::address::{self, Format};
use super::extract::{self, Conflicts, Extracted};
use super::job::{Failure, Job, size_text};
use super::motw::{self, Zone};
use super::names;
use super::reader::{self, Entry, Kind};
use super::time;

/// Archive listings kept in memory.
const KEEP_TREES: usize = 8;
/// The largest item copied out of an archive just to preview it.
const PREVIEW_LIMIT: u64 = 512 << 20;
/// How far into a stream (a compressed tar, a RAR) an item may lie for the preview: reaching it
/// means decompressing everything before it, which beyond this takes seconds. Tests use a
/// lower limit, to reach it quickly.
const STREAM_PREVIEW_LIMIT: u64 = if cfg!(test) { 1 << 20 } else { 256 << 20 };
/// How often a caller waiting for another's reading of the same archive looks for a cancel.
const WAIT_STEP: Duration = Duration::from_millis(50);
/// The folder of `%TEMP%\Kestrel` that copies of password-protected items are opened from.
const PROTECTED: &str = "protected";
/// Copies opened from archives, left unchanged this long, are removed at start.
const OPENED_KEPT: Duration = Duration::from_secs(7 * 86_400);
/// Copies of password-protected items left from an earlier run (one that didn't close
/// normally) are removed at start once this old.
const PROTECTED_KEPT: Duration = Duration::from_secs(3600);

/// One file or folder in an archive.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub dir: bool,
    /// For a folder, everything beneath it.
    pub size: u64,
    pub mtime: i64,
    pub attrs: u32,
    pub encrypted: bool,
    pub parent: usize,
    pub children: Vec<usize>,
    /// The position of the entry that last described it; `None` for a folder only implied.
    pub entry: Option<usize>,
}

/// An archive's entries as folders and files. Folders the archive only implies, by naming items
/// inside them, are made up; node 0 is the archive itself.
pub struct Tree {
    pub nodes: Vec<Node>,
    pub format: Format,
    by_name: HashMap<(usize, String), usize>,
    /// The entries as read, kept for a stream (a compressed tar, a RAR): taking an item out
    /// needs them, and reading them again means decompressing the whole archive once more.
    /// Empty for zip and 7z, whose list of contents is at hand.
    entries: Vec<Entry>,
}

impl Tree {
    pub fn build(entries: Vec<Entry>, format: Format) -> Tree {
        let root =
            Node { name: String::new(), dir: true, size: 0, mtime: 0, attrs: 0, encrypted: false, parent: 0, children: Vec::new(), entry: None };
        let mut tree = Tree { nodes: vec![root], format, by_name: HashMap::new(), entries: Vec::new() };
        for (position, entry) in entries.iter().enumerate() {
            if entry.kind == Kind::Other {
                continue;
            }
            let parts = names::shown_path(&entry.name);
            let Some(last) = parts.len().checked_sub(1) else { continue };
            let mut at = 0;
            for (depth, part) in parts.iter().enumerate() {
                let dir = depth < last || entry.kind == Kind::Dir;
                at = match tree.by_name.get(&(at, names::fold(part))) {
                    // A file and a folder of the same name: the folder wins, as it holds more.
                    Some(&existing) => {
                        if dir {
                            tree.nodes[existing].dir = true;
                        }
                        existing
                    }
                    None => {
                        let node = tree.nodes.len();
                        tree.nodes.push(Node {
                            name: part.clone(),
                            dir,
                            size: 0,
                            mtime: 0,
                            attrs: 0,
                            encrypted: false,
                            parent: at,
                            children: Vec::new(),
                            entry: None,
                        });
                        tree.nodes[at].children.push(node);
                        tree.by_name.insert((at, names::fold(part)), node);
                        node
                    }
                };
            }
            let node = &mut tree.nodes[at];
            node.mtime = entry.mtime;
            node.attrs = entry.attrs;
            node.encrypted = entry.encrypted;
            node.entry = Some(position);
            if !node.dir {
                node.size = entry.size;
            }
        }
        // Folder sizes: children always come after their parents.
        for at in (1..tree.nodes.len()).rev() {
            let (size, parent) = (tree.nodes[at].size, tree.nodes[at].parent);
            tree.nodes[parent].size = tree.nodes[parent].size.saturating_add(size);
        }
        if !matches!(format, Format::Zip | Format::SevenZ) {
            tree.entries = entries;
        }
        tree
    }

    /// The entries as read, for taking items out, when reading them again would cost a pass
    /// through the whole archive; `None` when they are at hand anyway.
    pub fn listing(&self) -> Option<&[Entry]> {
        (!self.entries.is_empty()).then_some(&self.entries[..])
    }

    /// For an item of a stream, the bytes decompressed to reach the end of it, which is what
    /// taking it out costs; `None` for zip and 7z, which go straight to an item.
    pub fn depth_of(&self, node: usize) -> Option<u64> {
        let last = self.nodes[node].entry?;
        let listing = self.listing()?;
        Some(listing[..=last.min(listing.len() - 1)].iter().map(|entry| entry.size).sum())
    }

    /// The child of `node` called `name`, ignoring case.
    pub fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.by_name.get(&(node, names::fold(name))).copied()
    }

    /// How many files and folders are directly in `node`.
    pub fn counts(&self, node: usize) -> (u32, u32) {
        let folders = self.nodes[node].children.iter().filter(|&&child| self.nodes[child].dir).count() as u32;
        (self.nodes[node].children.len() as u32 - folders, folders)
    }
}

// ---------- reading trees ----------

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: String,
    len: u64,
    modified: i64,
}

impl Key {
    fn of(file: &Path) -> Result<Key, Failure> {
        let meta = fs::metadata(file).map_err(|error| Failure::Io(format!("Can't open {}: {}", file.display(), reader::io_text(&error))))?;
        Ok(Key { path: file.to_string_lossy().to_lowercase(), len: meta.len(), modified: meta.modified().map_or(0, time::from_system) })
    }

    /// A short name for cache folders that belong to this archive, or to something in it.
    fn digest(&self, inner: &[String]) -> String {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        for part in inner {
            names::fold(part).hash(&mut hasher);
        }
        format!("{:016x}", hasher.finish())
    }
}

static TREES: Mutex<Vec<(Key, Arc<Tree>)>> = Mutex::new(Vec::new());

pub fn format_of(file: &Path) -> Result<Format, Failure> {
    let name = file.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let named = Format::of(name).ok_or_else(|| Failure::Unsupported("it isn't an archive Kestrel knows".into()))?;
    Ok(reader::sniff(file, named))
}

/// The tree read for this archive as it is now, if it is in memory.
fn cached(key: &Key) -> Option<Arc<Tree>> {
    let mut trees = TREES.lock();
    let at = trees.iter().position(|(cached, _)| cached == key)?;
    let found = trees.remove(at);
    let tree = found.1.clone();
    trees.insert(0, found);
    Some(tree)
}

/// Archives being read now, by folded path, each with the lock its reader holds.
static READING: Mutex<Vec<(String, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());

/// Every archive whose listing was read, by folded path, so tests can count readings.
#[cfg(test)]
pub static TREE_READS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The archive's tree, from memory if it hasn't changed since it was read. While another caller
/// reads the same archive, this one waits for that reading instead of starting its own; either
/// stops when `job` is cancelled.
fn tree(file: &Path, job: &Job) -> Result<Arc<Tree>, Failure> {
    let key = Key::of(file)?;
    if let Some(tree) = cached(&key) {
        return Ok(tree);
    }
    let gate = {
        let mut reading = READING.lock();
        match reading.iter().find(|(path, _)| *path == key.path) {
            Some((_, gate)) => gate.clone(),
            None => {
                let gate = Arc::new(Mutex::new(()));
                reading.push((key.path.clone(), gate.clone()));
                gate
            }
        }
    };
    let read = (|| {
        let _turn = loop {
            if let Some(turn) = gate.try_lock_for(WAIT_STEP) {
                break turn;
            }
            job.check()?;
        };
        // Read meanwhile by whoever held the turn.
        if let Some(tree) = cached(&key) {
            return Ok(tree);
        }
        let format = format_of(file)?;
        let cancelled = || job.cancelled();
        #[cfg(test)]
        TREE_READS.lock().push(key.path.clone());
        let mut source = reader::open(file, format, None, extract::budget(key.len), &cancelled)?;
        let tree = Arc::new(Tree::build(source.entries()?, format));
        let mut trees = TREES.lock();
        trees.retain(|(cached, _)| cached.path != key.path);
        trees.insert(0, (key.clone(), tree.clone()));
        trees.truncate(KEEP_TREES);
        Ok(tree)
    })();
    // The gate goes once nobody else holds it; others only take it with `READING` locked.
    let mut reading = READING.lock();
    if Arc::strong_count(&gate) == 2 {
        reading.retain(|(_, other)| !Arc::ptr_eq(other, &gate));
    }
    read
}

/// The tree of an archive on disk if it is cheap to have: already read, or a zip or 7z, whose
/// listing is at its end. A compressed tar must be read through to be listed.
fn cheap_tree(file: &Path) -> Option<Arc<Tree>> {
    let key = Key::of(file).ok()?;
    if let Some(tree) = cached(&key) {
        return Some(tree);
    }
    matches!(format_of(file).ok()?, Format::Zip | Format::SevenZ).then(|| tree(file, &Job::quiet()).ok()).flatten()
}

/// How far finding an address may go to read archives.
#[derive(Clone, Copy)]
pub enum Reading<'j> {
    /// Whatever is needed, until the job is cancelled: a listing the window asked for, an item
    /// to take out.
    All(&'j Job),
    /// Only what is at hand: listings already read, archives whose list of contents is at
    /// their end (zip, 7z), and archives inside archives already copied out. For the palette,
    /// which asks again with every key typed.
    AtHand,
}

impl Reading<'_> {
    fn tree(self, file: &Path) -> Result<Arc<Tree>, Failure> {
        match self {
            Reading::All(job) => tree(file, job),
            Reading::AtHand => cheap_tree(file).ok_or_else(|| Failure::Unsupported("its list of contents hasn't been read yet".into())),
        }
    }
}

// ---------- finding an address ----------

/// Where an address leads.
pub struct Located {
    /// The archive file holding the item: the archive on disk, or a cached copy of one inside it.
    pub file: PathBuf,
    pub tree: Arc<Tree>,
    pub node: usize,
    /// The item's path inside `file`, as the archive spells its names.
    pub inner: Vec<String>,
    /// The item's address, as the archive spells its names.
    pub address: String,
    /// The archive on disk the address starts at.
    pub outer: PathBuf,
    /// Its Mark of the Web.
    pub zone: Option<Zone>,
}

impl Located {
    pub fn node(&self) -> &Node {
        &self.tree.nodes[self.node]
    }

    /// The name of the archive the item is in, for messages.
    pub fn archive_name(&self) -> String {
        self.file.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    /// The mark what comes out of it gets, and the archive to name as its source.
    pub fn mark(&self) -> Option<(Zone, &Path)> {
        self.zone.map(|zone| (zone, self.outer.as_path()))
    }
}

/// Finds an address inside an archive; `None` if it isn't in one. With `enter`, an archive
/// inside the archive that the address ends at is opened, as listing it needs.
pub fn locate(path: &str, enter: bool, reading: Reading) -> Option<Result<Located, Failure>> {
    let (archive, parts) = address::split(path)?;
    Some(locate_in(&archive, &parts, enter, reading))
}

/// [`locate`] for an item inside an archive; `None` for an archive itself, which is a file on
/// disk like any other.
pub fn locate_entry(path: &str, reading: Reading) -> Option<Result<Located, Failure>> {
    let (archive, parts) = address::split(path)?;
    (!parts.is_empty()).then(|| locate_in(&archive, &parts, false, reading))
}

fn locate_in(archive: &str, parts: &[String], enter: bool, reading: Reading) -> Result<Located, Failure> {
    let outer = PathBuf::from(archive);
    let zone = motw::zone_of(&outer);
    let mut file = outer.clone();
    let mut address = archive.to_owned();
    let mut rest = parts;
    loop {
        let tree = reading.tree(&file)?;
        let (mut node, mut inner) = (0, Vec::new());
        let mut nested = None;
        for (depth, part) in rest.iter().enumerate() {
            node = tree.child(node, part).ok_or_else(|| Failure::Io(format!("Can't find {address}\\{part}")))?;
            let found = &tree.nodes[node];
            inner.push(found.name.clone());
            address.push('\\');
            address.push_str(&found.name);
            let more = depth + 1 < rest.len();
            if !found.dir && Format::of(&found.name).is_some() && (more || enter) {
                nested = Some(depth + 1);
                break;
            }
            if !found.dir && more {
                return Err(Failure::Io(format!("{address} isn't a folder")));
            }
        }
        let Some(used) = nested else {
            return Ok(Located { file, tree, node, inner, address, outer, zone });
        };
        file = nested_copy(&file, &tree, node, &inner, zone.map(|zone| (zone, outer.as_path())), reading)?;
        rest = &rest[used..];
    }
}

// ---------- the cache ----------

fn cache() -> PathBuf {
    // Each test keeps to a scratch folder of its own, never the data folder.
    if cfg!(test) {
        return test_scratch("cache");
    }
    crate::indexer::data_dir().join("archive-cache")
}

/// Where files opened from archives are copied: a folder of the temp folder, as Explorer uses.
fn opened_root() -> PathBuf {
    if cfg!(test) {
        return test_scratch("opened");
    }
    std::env::temp_dir().join("Kestrel")
}

/// The scratch folder standing in for `what` in the test running on this thread.
pub fn test_scratch(what: &str) -> PathBuf {
    let thread = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
    std::env::temp_dir().join(format!("kestrel-archive-{what}-{}-{thread}", std::process::id()))
}

/// Copies one item out of `file` (whose tree is `tree`) into `folder` (made if needed),
/// replacing what is there, and returns its path.
#[allow(clippy::too_many_arguments)]
fn copy_out(
    file: &Path,
    tree: &Tree,
    inner: &[String],
    folder: &Path,
    password: Option<&str>,
    zone: Option<(Zone, &Path)>,
    job: &Job,
) -> Result<PathBuf, Failure> {
    let name = file.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let base = &inner[..inner.len().saturating_sub(1)];
    let items = [inner.to_vec()];
    let request = extract::Request {
        archive: file,
        format: tree.format,
        entries: tree.listing(),
        name: &name,
        password,
        items: Some(&items),
        base,
        destination: folder,
        zone,
        conflicts: Conflicts::Replace,
    };
    extract::extract(&request, job)?;
    Ok(folder.join(names::safe_name(inner.last().map_or("", String::as_str))))
}

/// The cached copy of an archive inside an archive: made if `reading` allows, until its job is
/// cancelled.
fn nested_copy(file: &Path, tree: &Tree, node: usize, inner: &[String], zone: Option<(Zone, &Path)>, reading: Reading) -> Result<PathBuf, Failure> {
    let folder = cache().join("nested").join(Key::of(file)?.digest(inner));
    let target = folder.join(names::safe_name(&tree.nodes[node].name));
    if fs::metadata(&target).is_ok_and(|meta| meta.len() == tree.nodes[node].size) {
        return Ok(target);
    }
    if tree.nodes[node].encrypted {
        return Err(Failure::Password);
    }
    match reading {
        Reading::All(job) => copy_out(file, tree, inner, &folder, None, zone, job),
        Reading::AtHand => Err(Failure::Unsupported("the archive inside it hasn't been opened yet".into())),
    }
}

/// The newest change to a folder of copies or to anything directly in it: an app that saved
/// the copy it opened counts as using it.
fn last_change(folder: &Path) -> Option<SystemTime> {
    let own = fs::metadata(folder).and_then(|meta| meta.modified()).ok();
    let inside = fs::read_dir(folder).ok()?.flatten().filter_map(|item| item.metadata().and_then(|meta| meta.modified()).ok()).max();
    own.max(inside)
}

/// Removes the folders in `root` nothing has changed for `age`, except `keep`.
fn remove_older(root: &Path, age: Duration, keep: &[&str]) {
    let now = SystemTime::now();
    let Ok(items) = fs::read_dir(root) else { return };
    for item in items.flatten() {
        if keep.iter().any(|name| item.file_name().eq_ignore_ascii_case(name)) || !item.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let old = last_change(&item.path()).is_some_and(|changed| now.duration_since(changed).unwrap_or_default() > age);
        if old {
            // A file an app still holds open stays, and goes on a later start.
            let _ = fs::remove_dir_all(item.path());
        }
    }
}

/// Removes what the cache holds from earlier days: copies made for previews, copies and drags
/// (a day), and archives inside archives (a week); and copies opened from archives in the temp
/// folder that nothing has changed for a week, or for an hour if they are of password-protected
/// items, which a run that didn't close normally left behind. Run once at start, in the
/// background.
pub fn tidy() {
    const DAY: u64 = 86_400;
    for (folder, age) in [("preview", DAY), ("out", DAY), ("nested", 7 * DAY)] {
        remove_older(&cache().join(folder), Duration::from_secs(age), &[]);
    }
    remove_older(&opened_root(), OPENED_KEPT, &[PROTECTED]);
    remove_older(&opened_root().join(PROTECTED), PROTECTED_KEPT, &[]);
}

/// Copies of password-protected items opened in this run, removed when Kestrel closes.
static PROTECTED_COPIES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Removes the copies of password-protected items opened in this run, so their decrypted
/// contents don't outlast it; one an app still holds open stays until a later start.
pub fn forget_protected_copies() {
    for folder in std::mem::take(&mut *PROTECTED_COPIES.lock()) {
        let _ = fs::remove_dir_all(folder);
    }
}

// ---------- listing ----------

/// A folder inside an archive, for the file list: its items as directory entries.
pub fn items(located: &Located) -> Vec<DirItem> {
    located
        .node()
        .children
        .iter()
        .map(|&child| {
            let node = &located.tree.nodes[child];
            let attrs = node.attrs | if node.dir { attr::DIRECTORY } else { 0 };
            DirItem { name: node.name.clone(), meta: Meta { size: node.size, alloc: 0, mtime: node.mtime, attrs, file_id: 0 }, alias: false }
        })
        .collect()
}

// ---------- taking items out ----------

/// Serialises copying out for the preview: the pane and its protocol often ask for the same item
/// at once.
static PREVIEWING: Mutex<()> = Mutex::new(());

/// The item each window's preview last asked for, folded, with a number that changes whenever
/// that item does. A copy made for the preview gives way once its window wants another item,
/// so moving through a large archive's items with the pane open doesn't queue a decompression
/// for each. Each window's own: another window's preview wanting something else is no reason.
static PREVIEW_WANTED: LazyLock<Mutex<HashMap<String, (u64, String)>>> = LazyLock::new(Mutex::default);

/// Holds the turn to read the archive at `path` as a reader would, so tests (and the
/// end-to-end suite, through `archive_dev_hold`) can stand in for another window's long
/// reading of it.
pub fn hold_reading(path: &Path) -> Result<HeldReading, Failure> {
    let key = Key::of(path)?.path;
    let gate = Arc::new(Mutex::new(()));
    READING.lock().push((key, gate.clone()));
    std::mem::forget(gate.lock());
    Ok(HeldReading(gate))
}

/// The turn [`hold_reading`] took, given back when dropped.
pub struct HeldReading(Arc<Mutex<()>>);

impl Drop for HeldReading {
    fn drop(&mut self) {
        // SAFETY: locked in `hold_reading`, whose guard was forgotten; unlocked only here, once.
        unsafe { self.0.force_unlock() };
    }
}

/// The job of a preview of `path` for the app window `window`, which stops when that window's
/// preview wants another item.
pub(super) fn preview_job(path: &str, window: &str) -> Job {
    let turn = {
        let mut wanted = PREVIEW_WANTED.lock();
        let (turn, item) = wanted.entry(window.to_owned()).or_default();
        let key = path.to_lowercase();
        if *item != key {
            *turn += 1;
            *item = key;
        }
        *turn
    };
    let window = window.to_owned();
    Job::quiet_until(move || PREVIEW_WANTED.lock().get(&window).is_none_or(|(now, _)| *now != turn))
}

/// A file inside an archive, copied out to the cache for the preview of the app window
/// `window`; `None` for a path not in an archive. The error says why it can't be previewed,
/// after "Can't preview this item."
pub fn preview_file(path: &str, window: &str) -> Option<Result<String, String>> {
    let job = preview_job(path, window);
    let located = match locate_entry(path, Reading::All(&job))? {
        Ok(located) => located,
        Err(failure) => return Some(Err(failure.problem(&display_name(path)).message)),
    };
    let node = located.node().clone();
    Some((|| {
        if node.dir {
            return Err("It's a folder.".to_owned());
        }
        if node.encrypted {
            return Err("It's protected by a password. Extract it to see it.".to_owned());
        }
        if node.size > PREVIEW_LIMIT {
            return Err(format!("At {} it's too large to preview from inside an archive. Extract it to see it.", size_text(node.size)));
        }
        if let Some(depth) = located.tree.depth_of(located.node)
            && depth > STREAM_PREVIEW_LIMIT
        {
            return Err(format!(
                "It lies {} into a compressed archive, which is read from its start to reach it. Extract it to see it.",
                size_text(depth)
            ));
        }
        let key = Key::of(&located.file).map_err(|failure| failure.problem(&located.archive_name()).message)?;
        let folder = cache().join("preview").join(key.digest(&located.inner));
        let target = folder.join(names::safe_name(&node.name));
        let _one_at_a_time = PREVIEWING.lock();
        if fs::metadata(&target).is_ok_and(|meta| meta.len() == node.size) {
            return Ok(target.to_string_lossy().into_owned());
        }
        job.check().map_err(|failure| failure.problem(&located.archive_name()).message)?;
        copy_out(&located.file, &located.tree, &located.inner, &folder, None, located.mark(), &job)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|failure| failure.problem(&located.archive_name()).message)
    })())
}

/// The last part of a path, for messages.
fn display_name(path: &str) -> String {
    path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path).to_owned()
}

/// What the preview's details say about an item in an archive, or about an archive itself,
/// which reads like a file with the counts of what it holds; `None` for anything else.
pub fn preview_details(engine: &crate::engine::Engine, path: &str, window: &str) -> Option<Result<crate::preview::Details, String>> {
    let (archive, inner) = address::split(path)?;
    if inner.is_empty() {
        let mut details = crate::preview::describe(engine, &archive).ok()?;
        // Reading an online-only archive would download it.
        if !details.online_only
            && let Some(tree) = cheap_tree(Path::new(&archive))
        {
            let (files, folders) = tree.counts(0);
            details.folder = Some(crate::preview::FolderSummary { files, folders, readable: true });
        }
        return Some(Ok(details));
    }
    let job = preview_job(path, window);
    let located = match locate_in(&archive, &inner, false, Reading::All(&job)) {
        Ok(located) => located,
        Err(failure) => return Some(Err(failure.problem(&display_name(&archive)).message)),
    };
    let node = located.node();
    let ext = if node.dir { String::new() } else { node.name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default() };
    let type_name = if node.dir {
        "File folder".to_owned()
    } else {
        crate::preview::type_name(&ext).unwrap_or_else(|| if ext.is_empty() { "File".to_owned() } else { format!("{} File", ext.to_ascii_uppercase()) })
    };
    let mut attributes = Vec::new();
    for (bit, label) in [(0x1, "Read-only"), (0x2, "Hidden"), (0x4, "System")] {
        if node.attrs & bit != 0 {
            attributes.push(label);
        }
    }
    if node.encrypted {
        attributes.push("Encrypted");
    }
    let folder = node.dir.then(|| {
        let (files, folders) = located.tree.counts(located.node);
        crate::preview::FolderSummary { files, folders, readable: true }
    });
    Some(Ok(crate::preview::Details {
        name: node.name.clone(),
        path: located.address.clone(),
        dir: node.dir,
        size: Some(node.size),
        created: 0,
        modified: time::to_unix_ms(node.mtime),
        accessed: 0,
        attributes,
        online_only: false,
        type_name,
        dimensions: None,
        folder,
    }))
}

/// Copies a file out of an archive to a folder of its own under the temp folder and opens it, as
/// Explorer does; `None` for a path not in an archive. A copy already there, unchanged, is
/// opened again rather than written over, since an app may still have it open.
pub fn open_entry(path: &str, password: Option<&str>, job: &Job) -> Option<Result<(), Failure>> {
    let copy = copy_to_open(path, password, job)?;
    Some(copy.and_then(|copy| shell_open(&copy)))
}

/// The copy of a file inside an archive that opening it opens, made if it isn't there yet.
///
/// A password-protected item is decrypted afresh each time, with the password given then: a
/// copy left from an earlier opening is never handed out again without it. Its copies go to a
/// folder of their own, and are removed when Kestrel closes ([`forget_protected_copies`]).
pub fn copy_to_open(path: &str, password: Option<&str>, job: &Job) -> Option<Result<PathBuf, Failure>> {
    let located = match locate_entry(path, Reading::All(job))? {
        Ok(located) => located,
        Err(failure) => return Some(Err(failure)),
    };
    Some((|| {
        let node = located.node();
        if node.dir {
            return Err(Failure::Io(format!("{} is a folder.", located.address)));
        }
        let protected = node.encrypted;
        if protected && password.is_none() {
            return Err(Failure::Password);
        }
        let stem = names::safe_name(Format::stem(&located.archive_name()));
        let hash = Key::of(&located.file)?.digest(&[]);
        let root = if protected { opened_root().join(PROTECTED) } else { opened_root() };
        let name = names::safe_name(&node.name);
        let mut copy = 1;
        loop {
            let folder = root.join(if copy == 1 { format!("{stem} {}", &hash[..8]) } else { format!("{stem} {} ({copy})", &hash[..8]) });
            let target = folder.join(&name);
            match fs::metadata(&target) {
                Ok(meta) if meta.len() == node.size && !protected => return Ok(target),
                // Changed since, perhaps by the app it was opened in, or protected: never
                // written over, nor opened without the password.
                Ok(_) => copy += 1,
                Err(_) => {
                    let made = copy_out(&located.file, &located.tree, &located.inner, &folder, password, located.mark(), job);
                    if protected {
                        PROTECTED_COPIES.lock().push(folder);
                    }
                    return made;
                }
            }
        }
    })())
}

fn shell_open(path: &Path) -> Result<(), Failure> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), wide.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
    };
    // ShellExecute reports success as a value greater than 32.
    if result as usize > 32 { Ok(()) } else { Err(Failure::Io(format!("Windows couldn't open {}", path.display()))) }
}

static NEXT_OUT: AtomicU32 = AtomicU32::new(1);

/// Items of one folder inside one archive, by archive file and folder: the cache folder they go
/// to, their paths inside the archive, and where the first of them was found.
type Groups = HashMap<(PathBuf, Vec<String>), (PathBuf, Vec<Vec<String>>, Located)>;

/// Real files for `paths`: items in archives are copied out to a fresh folder in the cache,
/// anything else is passed through. For the clipboard and for dragging out, which need files
/// other programs can read.
pub fn materialize(paths: &[String], password: Option<&str>, job: &Job) -> Result<Vec<String>, (Failure, String)> {
    let mut out = Vec::with_capacity(paths.len());
    // Items in one archive folder are copied out together, into one folder.
    let mut groups = Groups::new();
    let mut order = Vec::new();
    for path in paths {
        let Some(located) = locate_entry(path, Reading::All(job)) else {
            order.push(Err(path.clone()));
            continue;
        };
        let located = located.map_err(|failure| (failure, display_name(path)))?;
        let inner = located.inner.clone();
        let base = &inner[..inner.len().saturating_sub(1)];
        let key = (located.file.clone(), base.iter().map(|part| names::fold(part)).collect::<Vec<_>>());
        // Copied out under the name extraction gives it.
        let name = names::safe_name(inner.last().map_or("", String::as_str));
        let group = groups.entry(key.clone()).or_insert_with(|| {
            let folder = cache().join("out").join(format!("{}-{}", std::process::id(), NEXT_OUT.fetch_add(1, Ordering::Relaxed)));
            (folder, Vec::new(), located)
        });
        group.1.push(inner);
        order.push(Ok((key, name)));
    }
    for (folder, items, located) in groups.values() {
        let base = &located.inner[..located.inner.len().saturating_sub(1)];
        let archive = located.archive_name();
        let request = extract::Request {
            archive: &located.file,
            format: located.tree.format,
            entries: located.tree.listing(),
            name: &archive,
            password,
            items: Some(items),
            base,
            destination: folder,
            zone: located.mark(),
            conflicts: Conflicts::Replace,
        };
        let _: Extracted = extract::extract(&request, job).map_err(|failure| (failure, archive.clone()))?;
    }
    for item in order {
        match item {
            Err(path) => out.push(path),
            Ok((key, name)) => out.push(groups[&key].0.join(name).to_string_lossy().into_owned()),
        }
    }
    Ok(out)
}
