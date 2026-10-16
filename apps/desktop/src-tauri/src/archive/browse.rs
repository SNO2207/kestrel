//! Browsing inside archives: listing a folder in one, read-only, and getting single items out of
//! one for the preview, for opening, and for copying or dragging elsewhere.
//!
//! An archive's entries are read once into a [`Tree`] of folders, kept for the few archives
//! looked at last and read again when the archive changes. An archive inside an archive is
//! copied out to the cache folder (under the data folder) and browsed from there. Every item
//! taken out goes through the same extraction as "Extract all", with all its checks, and gets
//! the outermost archive's Mark of the Web.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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
}

/// An archive's entries as folders and files. Folders the archive only implies, by naming items
/// inside them, are made up; node 0 is the archive itself.
pub struct Tree {
    pub nodes: Vec<Node>,
    pub format: Format,
    by_name: HashMap<(usize, String), usize>,
}

impl Tree {
    pub fn build(entries: &[Entry], format: Format) -> Tree {
        let root = Node { name: String::new(), dir: true, size: 0, mtime: 0, attrs: 0, encrypted: false, parent: 0, children: Vec::new() };
        let mut tree = Tree { nodes: vec![root], format, by_name: HashMap::new() };
        for entry in entries {
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
            if !node.dir {
                node.size = entry.size;
            }
        }
        // Folder sizes: children always come after their parents.
        for at in (1..tree.nodes.len()).rev() {
            let (size, parent) = (tree.nodes[at].size, tree.nodes[at].parent);
            tree.nodes[parent].size = tree.nodes[parent].size.saturating_add(size);
        }
        tree
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

/// The archive's tree, from memory if it hasn't changed since it was read.
fn tree(file: &Path) -> Result<Arc<Tree>, Failure> {
    let key = Key::of(file)?;
    {
        let mut trees = TREES.lock();
        if let Some(at) = trees.iter().position(|(cached, _)| *cached == key) {
            let found = trees.remove(at);
            let tree = found.1.clone();
            trees.insert(0, found);
            return Ok(tree);
        }
    }
    let format = format_of(file)?;
    let never = || false;
    let mut source = reader::open(file, format, None, extract::budget(key.len), &never)?;
    let tree = Arc::new(Tree::build(&source.entries()?, format));
    let mut trees = TREES.lock();
    trees.retain(|(cached, _)| cached.path != key.path);
    trees.insert(0, (key, tree.clone()));
    trees.truncate(KEEP_TREES);
    Ok(tree)
}

/// The tree of an archive on disk if it is cheap to have: already read, or a zip or 7z, whose
/// listing is at its end. A compressed tar must be read through to be listed.
fn cheap_tree(file: &Path) -> Option<Arc<Tree>> {
    let key = Key::of(file).ok()?;
    if let Some((_, tree)) = TREES.lock().iter().find(|(cached, _)| *cached == key) {
        return Some(tree.clone());
    }
    matches!(format_of(file).ok()?, Format::Zip | Format::SevenZ).then(|| tree(file).ok()).flatten()
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
pub fn locate(path: &str, enter: bool) -> Option<Result<Located, Failure>> {
    let (archive, parts) = address::split(path)?;
    Some(locate_in(&archive, &parts, enter))
}

/// [`locate`] for an item inside an archive; `None` for an archive itself, which is a file on
/// disk like any other.
pub fn locate_entry(path: &str) -> Option<Result<Located, Failure>> {
    let (archive, parts) = address::split(path)?;
    (!parts.is_empty()).then(|| locate_in(&archive, &parts, false))
}

fn locate_in(archive: &str, parts: &[String], enter: bool) -> Result<Located, Failure> {
    let outer = PathBuf::from(archive);
    let zone = motw::zone_of(&outer);
    let mut file = outer.clone();
    let mut address = archive.to_owned();
    let mut rest = parts;
    loop {
        let tree = tree(&file)?;
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
        file = nested_copy(&file, &tree, node, &inner, zone.map(|zone| (zone, outer.as_path())))?;
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

/// Copies one item out of `file` into `folder` (made if needed), replacing what is there,
/// and returns its path.
fn copy_out(file: &Path, format: Format, inner: &[String], folder: &Path, password: Option<&str>, zone: Option<(Zone, &Path)>, job: &Job) -> Result<PathBuf, Failure> {
    let name = file.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let base = &inner[..inner.len().saturating_sub(1)];
    let items = [inner.to_vec()];
    let request = extract::Request {
        archive: file,
        format,
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

/// The cached copy of an archive inside an archive.
fn nested_copy(file: &Path, tree: &Tree, node: usize, inner: &[String], zone: Option<(Zone, &Path)>) -> Result<PathBuf, Failure> {
    let folder = cache().join("nested").join(Key::of(file)?.digest(inner));
    let target = folder.join(names::safe_name(&tree.nodes[node].name));
    if fs::metadata(&target).is_ok_and(|meta| meta.len() == tree.nodes[node].size) {
        return Ok(target);
    }
    if tree.nodes[node].encrypted {
        return Err(Failure::Password);
    }
    copy_out(file, tree.format, inner, &folder, None, zone, &Job::quiet())
}

/// Removes what the cache holds from earlier days: copies made for previews, copies and drags
/// (a day), and archives inside archives (a week). Run once at start, in the background.
pub fn tidy() {
    let now = SystemTime::now();
    for (folder, age) in [("preview", 1), ("out", 1), ("nested", 7)] {
        let Ok(items) = fs::read_dir(cache().join(folder)) else { continue };
        for item in items.flatten() {
            let old = item
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() > Duration::from_secs(age * 86_400));
            if old {
                let _ = fs::remove_dir_all(item.path());
            }
        }
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

/// A file inside an archive, copied out to the cache for the preview; `None` for a path not in
/// an archive. The error says why it can't be previewed, after "Can't preview this item."
pub fn preview_file(path: &str) -> Option<Result<String, String>> {
    let located = match locate_entry(path)? {
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
        let key = Key::of(&located.file).map_err(|failure| failure.problem(&located.archive_name()).message)?;
        let folder = cache().join("preview").join(key.digest(&located.inner));
        let target = folder.join(names::safe_name(&node.name));
        let _one_at_a_time = PREVIEWING.lock();
        if fs::metadata(&target).is_ok_and(|meta| meta.len() == node.size) {
            return Ok(target.to_string_lossy().into_owned());
        }
        copy_out(&located.file, located.tree.format, &located.inner, &folder, None, located.mark(), &Job::quiet())
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
pub fn preview_details(engine: &crate::engine::Engine, path: &str) -> Option<Result<crate::preview::Details, String>> {
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
    let located = match locate_in(&archive, &inner, false) {
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
pub fn open_entry(path: &str, password: Option<&str>) -> Option<Result<(), Failure>> {
    let copy = copy_to_open(path, password)?;
    Some(copy.and_then(|copy| shell_open(&copy)))
}

/// The copy of a file inside an archive that opening it opens, made if it isn't there yet.
pub fn copy_to_open(path: &str, password: Option<&str>) -> Option<Result<PathBuf, Failure>> {
    let located = match locate_entry(path)? {
        Ok(located) => located,
        Err(failure) => return Some(Err(failure)),
    };
    Some((|| {
        let node = located.node();
        if node.dir {
            return Err(Failure::Io(format!("{} is a folder.", located.address)));
        }
        let stem = names::safe_name(Format::stem(&located.archive_name()));
        let hash = Key::of(&located.file)?.digest(&[]);
        let root = opened_root();
        let name = names::safe_name(&node.name);
        let mut copy = 1;
        loop {
            let folder = root.join(if copy == 1 { format!("{stem} {}", &hash[..8]) } else { format!("{stem} {} ({copy})", &hash[..8]) });
            let target = folder.join(&name);
            match fs::metadata(&target) {
                Ok(meta) if meta.len() == node.size => return Ok(target),
                // Changed since, perhaps by the app it was opened in: never written over.
                Ok(_) => copy += 1,
                Err(_) => {
                    return copy_out(&located.file, located.tree.format, &located.inner, &folder, password, located.mark(), &Job::quiet());
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
        let Some(located) = locate_entry(path) else {
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
