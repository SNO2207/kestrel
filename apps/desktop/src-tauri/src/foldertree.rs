//! The sidebar's folder tree: the folders inside a folder, and the sidebar's own settings.
//!
//! Folders come from the index when it has them, which needs no disk access at all and takes
//! microseconds even for a folder with thousands of subfolders; each says whether it has
//! folders of its own, so the tree knows where to draw a chevron. Folders the index does not
//! cover (network shares, removable drives, the far side of a junction) are listed from disk
//! on a thread of their own with a time limit, so a share that doesn't answer never holds the
//! tree up; whether their subfolders have folders is then left open until they are expanded.
//!
//! A background thread emits `folders-changed` when the index changes (it follows each drive's
//! change journal), and the tree asks again for the folders it shows.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_index::{DirItem, EntryId, Index, attr, fold, natural_cmp, walk};
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::engine::{Engine, normalize};
use crate::visibility::Visibility;

/// How long a folder outside the index may take to list.
const LIVE_WAIT: Duration = Duration::from_secs(4);
/// How long such a listing is used again: the address bar asks with every key typed.
const LIVE_KEEP: Duration = Duration::from_secs(3);
/// Entries looked at, per subfolder, to tell whether it holds a folder; beyond that it is left
/// open, and the chevron shows until the folder is expanded.
const PEEK_BUDGET: usize = 50_000;
/// Entries looked at for one listing's subfolders altogether.
const LISTING_BUDGET: usize = 2_000_000;
/// At most this many folders are expanded at once (`*`), which covers the largest drives.
const EXPAND_ALL_LIMIT: usize = 2_000_000;
/// How often the index's revision is compared with what it was.
const WATCH_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TreeFolder {
    pub name: String,
    pub hidden: bool,
    /// Whether it holds folders to show; `None` when that can't be known without reading the
    /// disk, as for a share's folders or a junction.
    pub children: Option<bool>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TreeListing {
    /// The folder, as asked for.
    pub path: String,
    pub folders: Vec<TreeFolder>,
    /// Listed from disk, not the index.
    pub live: bool,
    /// Why it couldn't be listed.
    pub error: Option<String>,
}

/// Whether `dir` holds a folder `visibility` shows, looking at no more than `budget` entries.
fn holds_folder(index: &Index, dir: EntryId, visibility: Visibility, budget: &mut usize) -> Option<bool> {
    for (looked, child) in index.children(dir).enumerate() {
        if looked >= PEEK_BUDGET || *budget == 0 {
            return None;
        }
        *budget -= 1;
        if index.is_dir(child) && visibility.shows(index.attrs(child)) {
            return Some(true);
        }
    }
    Some(false)
}

/// The folders in an indexed folder, in name order.
fn indexed(index: &Index, dir: EntryId, visibility: Visibility) -> Vec<TreeFolder> {
    let mut ids: Vec<EntryId> = index.children(dir).filter(|&id| index.is_dir(id) && visibility.shows(index.attrs(id))).collect();
    ids.sort_by(|&a, &b| index.cmp_name(a, b));
    let mut budget = LISTING_BUDGET;
    ids.into_iter()
        .map(|id| {
            let attrs = index.attrs(id);
            // A junction or folder link is indexed without what it leads to: not known.
            let empty_link = attrs & attr::REPARSE_POINT != 0 && index.children(id).next().is_none();
            let children = if empty_link { None } else { holds_folder(index, id, visibility, &mut budget) };
            TreeFolder { name: index.name(id).to_owned(), hidden: attrs & attr::HIDDEN != 0, children }
        })
        .collect()
}

/// What listing a folder from disk gave: its items, or why there are none.
type Listed = Result<Arc<Vec<DirItem>>, String>;

/// One folder's listing from disk: under way, or done and when.
struct Reading {
    started: Instant,
    done: Mutex<Option<(Instant, Listed)>>,
    ready: Condvar,
}

/// Folders outside the index (a share's) listed from disk, one listing at a time per folder:
/// whoever asks for a folder while it is being listed waits for that listing, and one listed a
/// moment ago is answered from it. The address bar asks with every key typed, mostly about the
/// same folder; listed afresh each time, a share over a slow connection was read again for each
/// character, and one whose server is away tied up a thread per key for seconds.
struct LiveListings {
    readings: Mutex<HashMap<String, Arc<Reading>>>,
    list: fn(&str) -> std::io::Result<Vec<DirItem>>,
    /// How long a listing is used again.
    keep: Duration,
    /// How long anyone waits for one.
    wait: Duration,
}

impl LiveListings {
    /// The items of the folder at `path`, read from disk at most once per `keep`.
    fn items(&self, path: &str) -> Listed {
        let key = fold(path);
        let reading = {
            let mut readings = self.readings.lock();
            // Done long ago, or abandoned, they go; a listing still under way stays shared.
            readings.retain(|_, reading| reading.done.lock().as_ref().is_none_or(|(at, _)| at.elapsed() < self.keep));
            match readings.get(&key) {
                Some(reading) => reading.clone(),
                None => {
                    let reading = Arc::new(Reading { started: Instant::now(), done: Mutex::new(None), ready: Condvar::new() });
                    self.start(path, reading.clone())?;
                    readings.insert(key, reading.clone());
                    reading
                }
            }
        };
        let deadline = reading.started + self.wait;
        let mut done = reading.done.lock();
        while done.is_none() {
            // Waited out already, by whoever asked first: nobody waits for it again.
            if reading.ready.wait_until(&mut done, deadline).timed_out() {
                break;
            }
        }
        match done.as_ref() {
            Some((_, listed)) => listed.clone(),
            None => Err(format!("{path} didn't answer.")),
        }
    }

    fn start(&self, path: &str, reading: Arc<Reading>) -> Result<(), String> {
        let (dir, list) = (path.to_owned(), self.list);
        std::thread::Builder::new()
            .name("kestrel-tree-list".into())
            .spawn(move || {
                let listed = match list(&dir) {
                    Ok(items) => Ok(Arc::new(items)),
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(crate::access::LOCKED.to_owned()),
                    Err(error) => Err(format!("Can't open {dir}: {error}")),
                };
                *reading.done.lock() = Some((Instant::now(), listed));
                reading.ready.notify_all();
            })
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

static LIVE: LazyLock<LiveListings> =
    LazyLock::new(|| LiveListings { readings: Mutex::new(HashMap::new()), list: walk::list, keep: LIVE_KEEP, wait: LIVE_WAIT });

/// The folders in a folder read from disk, with a time limit.
fn live(path: &str, visibility: Visibility) -> Result<Vec<TreeFolder>, String> {
    let items = LIVE.items(path)?;
    let mut folders: Vec<TreeFolder> = items
        .iter()
        .filter(|item| item.meta.attrs & attr::DIRECTORY != 0 && visibility.shows(item.meta.attrs))
        .map(|item| TreeFolder { hidden: item.meta.attrs & attr::HIDDEN != 0, name: item.name.clone(), children: None })
        .collect();
    folders.sort_by(|a, b| natural_cmp(fold(&a.name).as_bytes(), fold(&b.name).as_bytes()));
    Ok(folders)
}

/// The folders in each of `paths`: from the index where it has them, otherwise from disk.
pub fn children(engine: &Engine, paths: &[String], visibility: Visibility) -> Vec<TreeListing> {
    let mut listings: Vec<Option<TreeListing>> = Vec::with_capacity(paths.len());
    {
        let index = engine.index().read();
        for path in paths {
            let found = index.lookup(&normalize(path)).filter(|&dir| index.is_dir(dir));
            // A folder link is never read through by the index; it is listed from disk.
            let link = found.is_some_and(|dir| index.attrs(dir) & attr::REPARSE_POINT != 0 && index.children(dir).next().is_none());
            listings.push(found.filter(|_| !link).map(|dir| TreeListing {
                path: path.clone(),
                folders: indexed(&index, dir, visibility),
                live: false,
                error: None,
            }));
        }
    }
    // Disk listings happen without the index held.
    listings
        .into_iter()
        .zip(paths)
        .map(|(listing, path)| {
            listing.unwrap_or_else(|| match live(&normalize(path), visibility) {
                Ok(folders) => TreeListing { path: path.clone(), folders, live: true, error: None },
                Err(error) => TreeListing { path: path.clone(), folders: Vec::new(), live: true, error: Some(error) },
            })
        })
        .collect()
}

/// The folders a path is inside, from its drive's root down to its parent: what the tree must
/// expand to show it. Only paths on a lettered drive have a place in the tree.
pub fn ancestors(path: &str) -> Vec<String> {
    let path = normalize(path);
    let bytes = path.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
        return Vec::new();
    }
    let parts: Vec<&str> = path[3..].split('\\').filter(|part| !part.is_empty()).collect();
    let root = path[..3].to_owned();
    let mut found = vec![root.clone()];
    let mut current = root;
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        current = if current.ends_with('\\') { format!("{current}{part}") } else { format!("{current}\\{part}") };
        found.push(current.clone());
    }
    if parts.is_empty() {
        // A drive root sits directly under This PC.
        found.clear();
    }
    found
}

/// Every folder beneath an indexed folder, depth first in name order, as one line each:
/// depth (1 for its own folders), whether hidden (`1` or `0`), whether it holds folders
/// (`1`, `0`, or `?` when not known), then the name. Compact, because a whole drive has
/// hundreds of thousands. Folders outside the index give just their own level.
pub fn subtree(engine: &Engine, path: &str, visibility: Visibility) -> String {
    let index = engine.index().read();
    let Some(root) = index.lookup(&normalize(path)).filter(|&dir| index.is_dir(dir)) else {
        drop(index);
        let listing = children(engine, &[path.to_owned()], visibility);
        let mut out = String::new();
        for folder in listing.into_iter().flat_map(|listing| listing.folders) {
            out.push_str(&format!("1\t{}\t?\t{}\n", u8::from(folder.hidden), folder.name));
        }
        return out;
    };
    let mut out = String::new();
    let mut stack: Vec<(EntryId, usize)> = vec![(root, 0)];
    let mut emitted = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > 0 {
            let attrs = index.attrs(dir);
            let mut subfolders = index.children(dir).filter(|&id| index.is_dir(id) && visibility.shows(index.attrs(id)));
            let empty_link = attrs & attr::REPARSE_POINT != 0 && index.children(dir).next().is_none();
            let holds = if empty_link { '?' } else if subfolders.next().is_some() { '1' } else { '0' };
            out.push_str(&format!("{depth}\t{}\t{holds}\t{}\n", u8::from(attrs & attr::HIDDEN != 0), index.name(dir)));
            emitted += 1;
            if emitted >= EXPAND_ALL_LIMIT {
                break;
            }
        }
        let mut ids: Vec<EntryId> = index.children(dir).filter(|&id| index.is_dir(id) && visibility.shows(index.attrs(id))).collect();
        ids.sort_by(|&a, &b| index.cmp_name(b, a));
        stack.extend(ids.into_iter().map(|id| (id, depth + 1)));
    }
    out
}

/// "Show hidden items" and "Hide protected operating system files", as the file list has them.
fn visibility(show_hidden: bool, show_protected: Option<bool>) -> Visibility {
    Visibility::from_request(Some(show_hidden)).with_protected(show_protected)
}

#[tauri::command(async)]
pub fn tree_children(
    engine: State<Arc<Engine>>,
    paths: Vec<String>,
    show_hidden: bool,
    show_protected: Option<bool>,
) -> Vec<TreeListing> {
    children(&engine, &paths, visibility(show_hidden, show_protected))
}

/// The listings the tree needs to show `path`: those of every folder above it on its drive.
#[tauri::command(async)]
pub fn tree_reveal(
    engine: State<Arc<Engine>>,
    path: String,
    show_hidden: bool,
    show_protected: Option<bool>,
) -> Vec<TreeListing> {
    children(&engine, &ancestors(&path), visibility(show_hidden, show_protected))
}

#[tauri::command(async)]
pub fn tree_subtree(engine: State<Arc<Engine>>, path: String, show_hidden: bool, show_protected: Option<bool>) -> String {
    subtree(&engine, &path, visibility(show_hidden, show_protected))
}

/// What the sidebar remembers: collapsed sections, and "Expand to open folder".
#[derive(Serialize)]
pub struct SidebarState {
    pub collapsed: Vec<String>,
    pub expand_to_open_folder: bool,
}

#[tauri::command(async)]
pub fn sidebar_state(db: State<Arc<Db>>) -> Result<SidebarState, String> {
    Ok(SidebarState {
        collapsed: kestrel_db::places::collapsed_sections(&db)?,
        expand_to_open_folder: kestrel_db::places::expand_to_open_folder(&db)?,
    })
}

#[tauri::command(async)]
pub fn set_sidebar_section(db: State<Arc<Db>>, section: String, collapsed: bool) -> Result<(), String> {
    Ok(kestrel_db::places::set_section_collapsed(&db, &section, collapsed)?)
}

#[tauri::command(async)]
pub fn set_expand_to_open_folder(db: State<Arc<Db>>, on: bool) -> Result<(), String> {
    Ok(kestrel_db::places::set_expand_to_open_folder(&db, on)?)
}

/// Emits `folders-changed` whenever the index has changed, at most twice a second.
pub fn watch(app: AppHandle, engine: Arc<Engine>) {
    let spawned = std::thread::Builder::new().name("kestrel-tree-watch".into()).spawn(move || {
        let mut last = engine.revision();
        loop {
            std::thread::sleep(WATCH_EVERY);
            let now = engine.revision();
            if now != last {
                last = now;
                let _ = app.emit("folders-changed", now);
            }
        }
    });
    if let Err(error) = spawned {
        eprintln!("The folder tree won't follow changes: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::IndexBuilder;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};

    static SLOW_LISTINGS: AtomicUsize = AtomicUsize::new(0);
    static AWAY_LISTINGS: AtomicUsize = AtomicUsize::new(0);

    /// A share over a slow connection: listing it takes a while.
    fn slow_share(_: &str) -> std::io::Result<Vec<DirItem>> {
        SLOW_LISTINGS.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        let meta = kestrel_index::Meta { attrs: attr::DIRECTORY, ..kestrel_index::Meta::default() };
        Ok(vec![DirItem { name: "Projects".into(), meta, alias: false }])
    }

    /// A share whose server is away: listing it fails after a long while.
    fn away_share(_: &str) -> std::io::Result<Vec<DirItem>> {
        AWAY_LISTINGS.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(3));
        Err(std::io::Error::other("the network path was not found"))
    }

    fn listings(list: fn(&str) -> std::io::Result<Vec<DirItem>>) -> LiveListings {
        LiveListings { readings: Mutex::new(HashMap::new()), list, keep: Duration::from_secs(2), wait: Duration::from_millis(800) }
    }

    /// Typing a share's path asks for the same folder with every key: it is listed once, and
    /// whoever asks while that listing runs, or just after, gets it.
    #[test]
    fn a_folder_off_the_index_is_listed_once_for_everyone_asking() {
        let live = Arc::new(listings(slow_share));
        let askers: Vec<_> = (0..5)
            .map(|n| {
                let live = live.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(n * 40));
                    live.items(r"\\nas\share\").map(|items| items.len())
                })
            })
            .collect();
        for asker in askers {
            assert_eq!(asker.join().unwrap(), Ok(1));
        }
        assert_eq!(live.items(r"\\NAS\share\").map(|items| items.len()), Ok(1), "answered from the listing just made");
        assert_eq!(SLOW_LISTINGS.load(Ordering::Relaxed), 1);
    }

    /// A share whose server is away is waited for once: those asking after the wait ran out are
    /// told at once, and no new listing starts while the first is still waiting.
    #[test]
    fn a_share_that_doesnt_answer_is_waited_for_once() {
        let live = listings(away_share);
        let started = Instant::now();
        assert_eq!(live.items(r"\\away\team").unwrap_err(), r"\\away\team didn't answer.");
        assert!(started.elapsed() < Duration::from_secs(2));
        let again = Instant::now();
        assert!(live.items(r"\\away\team").is_err());
        assert!(again.elapsed() < Duration::from_millis(100), "{:?}", again.elapsed());
        assert_eq!(AWAY_LISTINGS.load(Ordering::Relaxed), 1);
    }

    fn indexed_sandbox(name: &str) -> (Engine, std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("kestrel-tree-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["b2", "b10", "Alpha/inner", "empty", ".hidden/x"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("b2/file.txt"), b"not a folder").unwrap();
        let hidden: Vec<u16> = root.join(".hidden").to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: the path is NUL-terminated.
        unsafe { SetFileAttributesW(hidden.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
        let path = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&path, &mut builder).unwrap();
        engine.install(builder.finish());
        (engine, root, path)
    }

    fn names(listing: &TreeListing) -> Vec<(&str, Option<bool>)> {
        listing.folders.iter().map(|folder| (folder.name.as_str(), folder.children)).collect()
    }

    #[test]
    fn folders_come_from_the_index_in_name_order_with_their_chevrons() {
        let (engine, root, path) = indexed_sandbox("index");
        let shown = Visibility { hidden: false, ..Visibility::ALL };
        let [listing] = children(&engine, std::slice::from_ref(&path), shown).try_into().unwrap();
        assert!(!listing.live && listing.error.is_none());
        // Natural order (b2 before b10), files left out, the hidden folder too.
        assert_eq!(names(&listing), [("Alpha", Some(true)), ("b2", Some(false)), ("b10", Some(false)), ("empty", Some(false))]);
        let [all] = children(&engine, std::slice::from_ref(&path), Visibility::ALL).try_into().unwrap();
        assert_eq!(all.folders.iter().find(|folder| folder.name == ".hidden").map(|folder| folder.hidden), Some(true));

        // Gone from disk, still in the index: the index answers, without touching the disk.
        fs::remove_dir_all(root.join("Alpha")).unwrap();
        let [again] = children(&engine, std::slice::from_ref(&path), shown).try_into().unwrap();
        assert_eq!(again.folders[0].name, "Alpha", "read from the index");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folders_outside_the_index_are_listed_from_disk() {
        let root = std::env::temp_dir().join(format!("kestrel-tree-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("one/two")).unwrap();
        fs::create_dir_all(root.join("three")).unwrap();
        let engine = Engine::default();
        let path = root.to_str().unwrap().to_owned();
        let [listing] = children(&engine, std::slice::from_ref(&path), Visibility::ALL).try_into().unwrap();
        assert!(listing.live);
        assert_eq!(names(&listing), [("one", None), ("three", None)]);
        let missing = format!(r"{path}\nope");
        let [failed] = children(&engine, &[missing], Visibility::ALL).try_into().unwrap();
        assert!(failed.error.is_some() && failed.folders.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_path_is_revealed_from_its_drive_down() {
        assert_eq!(ancestors(r"C:\Users\me\Documents"), [r"C:\", r"C:\Users", r"C:\Users\me"]);
        assert_eq!(ancestors(r"c:/Windows/"), [r"c:\"]);
        assert!(ancestors(r"C:\").is_empty(), "a drive sits under This PC itself");
        assert!(ancestors(r"\\server\share\folder").is_empty());
        assert!(ancestors("shell:RecycleBinFolder").is_empty());
    }

    #[test]
    fn a_whole_subtree_comes_in_one_compact_answer() {
        let (engine, root, path) = indexed_sandbox("subtree");
        let lines: Vec<String> = subtree(&engine, &path, Visibility { hidden: false, ..Visibility::ALL }).lines().map(str::to_owned).collect();
        assert_eq!(lines, ["1\t0\t1\tAlpha", "2\t0\t0\tinner", "1\t0\t0\tb2", "1\t0\t0\tb10", "1\t0\t0\tempty"]);
        fs::remove_dir_all(&root).unwrap();
    }
}
