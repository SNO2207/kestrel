//! The disk usage hub's backend: what uses the disk, worked out from the index
//! (`kestrel-analyze`), set beside what the drive says about itself (`volume.rs`), with a daily
//! sample of folder sizes to say what grew (`history.rs`).
//!
//! Everything comes from the in-memory index: no folder is listed and no file is opened, so a
//! whole drive is analysed in about a tenth of a second and cloud-only files are never fetched.
//! While the hub is open, a `disk-changed` event (debounced on `Engine::revision`) tells it the
//! index has moved on, and it asks again.

mod history;
mod volume;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kestrel_analyze::category::SYSTEM_ROOT_FILES;
use kestrel_analyze::overview::{self, Overview};
use kestrel_analyze::treemap::{self, NO_TILE, TileKind};
use kestrel_analyze::Measure;
use kestrel_db::Db;
use kestrel_index::{EntryId, Index, NO_ENTRY};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, Window};

use crate::engine::{Engine, normalize};
use crate::fileops::{self, Options, Request};
use crate::undo::Journal;
pub use history::GrowthView;
use volume::Volume;

/// Largest files and folders listed.
const TOP: usize = 50;
/// How long the index must stay quiet before the hub is told it changed...
const QUIET: Duration = Duration::from_millis(800);
/// ...or, while it keeps changing, how often at most.
const MAX_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(400);
/// The smallest rectangle the treemap draws, in square CSS pixels.
const MIN_AREA: f64 = 30.0;
/// Tiles sent at most; a 4K screen's worth.
const MAX_TILES: usize = 30_000;

pub struct Disk {
    engine: Arc<Engine>,
    db: Arc<Db>,
    app: Mutex<Option<AppHandle>>,
    /// The windows whose hub is open, which changes to the index are announced to.
    watching: Watchers,
}

/// The windows whose disk usage hub is open. Each window opens and closes its own, so one
/// closing its hub must leave another's live.
#[derive(Default)]
struct Watchers(Mutex<HashSet<String>>);

impl Watchers {
    fn set(&self, window: &str, open: bool) {
        let mut watching = self.0.lock();
        if open {
            watching.insert(window.to_owned());
        } else {
            watching.remove(window);
        }
    }

    /// The windows to tell, forgetting those `exists` says have closed (with their hub open).
    fn to_tell(&self, exists: impl Fn(&str) -> bool) -> Vec<String> {
        let mut watching = self.0.lock();
        watching.retain(|window| exists(window));
        let mut windows: Vec<String> = watching.iter().cloned().collect();
        windows.sort();
        windows
    }

    fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }
}

impl Disk {
    pub fn new(engine: Arc<Engine>, db: Arc<Db>) -> Arc<Self> {
        Arc::new(Self { engine, db, app: Mutex::new(None), watching: Watchers::default() })
    }

    /// Starts the background work: announcing index changes to an open hub, and the daily
    /// sample of folder sizes.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        *self.app.lock() = Some(app);
        let me = self.clone();
        let _ = std::thread::Builder::new().name("kestrel-disk".into()).spawn(move || me.watch());
        let (engine, db) = (self.engine.clone(), self.db.clone());
        let _ = std::thread::Builder::new().name("kestrel-disk-history".into()).spawn(move || history::run(&engine, &db));
    }

    /// Tells an open hub when the index has changed: once it has been quiet for a moment, or
    /// every few seconds while it keeps changing.
    fn watch(self: Arc<Self>) {
        let mut seen = self.engine.revision();
        let mut first_change: Option<Instant> = None;
        let mut last_change = Instant::now();
        loop {
            std::thread::sleep(POLL);
            let revision = self.engine.revision();
            if self.watching.is_empty() {
                (seen, first_change) = (revision, None);
                continue;
            }
            if revision != seen {
                seen = revision;
                last_change = Instant::now();
                first_change.get_or_insert(last_change);
            }
            let due = first_change.is_some_and(|first| last_change.elapsed() >= QUIET || first.elapsed() >= MAX_WAIT);
            if due {
                first_change = None;
                if let Some(app) = self.app.lock().as_ref() {
                    for window in self.watching.to_tell(|window| app.get_window(window).is_some()) {
                        let _ = app.emit_to(window.as_str(), "disk-changed", revision);
                    }
                }
            }
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The live entry at `path`.
fn locate(index: &Index, path: &str) -> Result<EntryId, String> {
    let path = normalize(path);
    index
        .lookup(&path)
        .filter(|&id| !index.is_deleted(id))
        .ok_or_else(|| format!("{path} is not in the index"))
}

/// The root of the drive (or indexed folder) that `id` is on.
fn top_of(index: &Index, id: EntryId) -> EntryId {
    let mut top = id;
    while index.parent(top) != NO_ENTRY {
        top = index.parent(top);
    }
    top
}

type Shared<'a> = State<'a, Arc<Disk>>;

/// One of Windows' own files in a drive's root.
#[derive(Serialize)]
pub struct SystemFile {
    name: &'static str,
    bytes: u64,
    /// The index holds it, so it is part of what the index covers.
    counted: bool,
}

#[derive(Serialize)]
pub struct VolumeView {
    #[serde(flatten)]
    volume: Volume,
    used: u64,
    /// What the index holds for the whole drive, on disk.
    indexed: u64,
    /// Files the index holds for the whole drive.
    indexed_files: u64,
    system_files: Vec<SystemFile>,
}

#[derive(Serialize)]
pub struct OverviewView {
    path: String,
    name: String,
    /// The root of a drive, whose totals can be set beside the drive's own.
    is_drive: bool,
    is_dir: bool,
    /// The index revision this was worked out from.
    revision: u64,
    took_ms: f64,
    /// The index is still being built, so these numbers will grow.
    indexing: bool,
    #[serde(flatten)]
    analysis: Overview,
    volume: Option<VolumeView>,
}

/// Everything the overview shows about the folder (or drive) at `path`.
#[tauri::command(async)]
pub fn disk_overview(disk: Shared, path: String) -> Result<OverviewView, String> {
    overview_of(&disk.engine, &path)
}

fn overview_of(engine: &Engine, path: &str) -> Result<OverviewView, String> {
    let started = Instant::now();
    let revision = engine.revision();
    let indexing = engine.status().state != "ready";
    let options = overview::Options { now_ms: now_ms(), top: TOP };
    // The walk lets go of the index between steps, and the figures are added up without it:
    // the hub asks again every few seconds while the index changes, and nothing else that reads
    // the index may wait for it each time.
    let (analysis, facts) = loop {
        let (walked, generation) = engine.in_steps(
            |index| locate(index, path).map(|root| (root, overview::OverviewWalk::new(root, &options))),
            |walk, index| walk.as_mut().map_or(true, |(_, walk)| walk.step(index, overview::STEP)),
            |walk, _| (walk, engine.generation()),
        );
        let (root, walk) = walked?;
        let summed = walk.sum();
        let index = engine.index().read();
        // Replaced meanwhile: the walk's entries are not this index's.
        if engine.generation() == generation {
            break (summed.overview(&index), facts_of(&index, root));
        }
    };
    let indexed_files = if facts.top == facts.root { analysis.totals.local.files + analysis.totals.cloud_files } else { 0 };
    let volume = volume::read(&facts.drive_path).map(|volume| VolumeView {
        used: volume.total.saturating_sub(volume.free),
        volume,
        indexed: facts.indexed,
        indexed_files,
        system_files: facts.system_files,
    });
    Ok(OverviewView {
        path: facts.view_path,
        name: facts.name,
        is_drive: facts.is_drive,
        is_dir: facts.is_dir,
        revision,
        took_ms: started.elapsed().as_secs_f64() * 1000.0,
        indexing,
        analysis,
        volume,
    })
}

/// What the overview says of the item at `root` beyond its analysis: where it is, and its drive.
struct Facts {
    root: EntryId,
    top: EntryId,
    drive_path: String,
    indexed: u64,
    system_files: Vec<SystemFile>,
    view_path: String,
    name: String,
    is_dir: bool,
    is_drive: bool,
}

fn facts_of(index: &Index, root: EntryId) -> Facts {
    let top = top_of(index, root);
    let drive_path = index.path(top);
    // The page file and its kind, in the index or (held open by Windows, so never re-listed)
    // only on the drive.
    let system_files: Vec<SystemFile> = SYSTEM_ROOT_FILES[..3]
        .iter()
        .filter_map(|&name| match index.children(top).find(|&child| index.lower_bytes(child) == name.as_bytes()) {
            Some(entry) => Some(SystemFile { name, bytes: index.alloc(entry), counted: true }),
            None => volume::listed_size(&format!("{}{name}", drive_path)).map(|bytes| SystemFile { name, bytes, counted: false }),
        })
        .collect();
    Facts {
        root,
        top,
        indexed: index.alloc(top),
        system_files,
        view_path: index.path(root),
        name: index.name(root).to_owned(),
        is_dir: index.is_dir(root),
        is_drive: root == top && drive_path.ends_with(":\\"),
        drive_path,
    }
}

/// The tiles of a treemap, a column per field, which keeps tens of thousands of them small.
#[derive(Serialize, Default)]
pub struct TreemapView {
    path: String,
    total: u64,
    truncated: bool,
    revision: u64,
    took_ms: f64,
    /// Index entry of each tile; `u32::MAX` for a tile of smaller items.
    entry: Vec<u32>,
    /// The tile each lies in, or -1.
    parent: Vec<i64>,
    x: Vec<f32>,
    y: Vec<f32>,
    w: Vec<f32>,
    h: Vec<f32>,
    depth: Vec<u32>,
    /// 0 a file, 1 a folder, 2 smaller items.
    kind: Vec<u8>,
    /// Position in the list of categories, or -1 for none.
    category: Vec<i8>,
    /// 1: an open folder; 2: with a strip for its name; 4: a file whose contents are in the cloud.
    flags: Vec<u8>,
    bytes: Vec<u64>,
    /// Logical size, for the tooltip.
    logical: Vec<u64>,
    /// Items in a tile of smaller items.
    count: Vec<u32>,
    name: Vec<String>,
}

/// Rounds a coordinate to a hundredth of a pixel, which keeps the message short.
fn px(value: f64) -> f32 {
    ((value * 100.0).round() / 100.0) as f32
}

/// A treemap of the folder at `path`, `width` by `height` CSS pixels, by size on disk unless
/// `measure` says otherwise.
#[tauri::command(async)]
pub fn disk_treemap(disk: Shared, path: String, width: f64, height: f64, measure: Option<Measure>) -> Result<TreemapView, String> {
    treemap_of(&disk.engine, &path, width, height, measure.unwrap_or_default())
}

fn treemap_of(engine: &Engine, path: &str, width: f64, height: f64, measure: Measure) -> Result<TreemapView, String> {
    let started = Instant::now();
    let revision = engine.revision();
    let index = engine.index().read();
    let root = locate(&index, path)?;
    let options = treemap::Options {
        width: width.clamp(0.0, 16_000.0),
        height: height.clamp(0.0, 16_000.0),
        min_area: MIN_AREA,
        padding: 2.0,
        header: 18.0,
        measure,
        max_tiles: MAX_TILES,
        ..treemap::Options::default()
    };
    let map = treemap::layout(&index, root, &options);
    let mut view = TreemapView {
        path: index.path(root),
        total: map.total,
        truncated: map.truncated,
        revision,
        ..TreemapView::default()
    };
    for tile in &map.tiles {
        let entry = tile.entry;
        view.entry.push(entry);
        view.parent.push(if tile.parent == NO_TILE { -1 } else { tile.parent as i64 });
        view.x.push(px(tile.rect.x));
        view.y.push(px(tile.rect.y));
        view.w.push(px(tile.rect.w));
        view.h.push(px(tile.rect.h));
        view.depth.push(tile.depth);
        view.kind.push(match tile.kind {
            TileKind::File => 0,
            TileKind::Folder => 1,
            TileKind::Rest => 2,
        });
        view.category.push(tile.category.map_or(-1, |category| category.index() as i8));
        let cloud = entry != NO_ENTRY && tile.kind == TileKind::File && kestrel_analyze::is_cloud_only(index.attrs(entry));
        view.flags.push(u8::from(tile.open) | u8::from(tile.header) << 1 | u8::from(cloud) << 2);
        view.bytes.push(tile.bytes);
        view.logical.push(if entry == NO_ENTRY { tile.bytes } else { index.size(entry) });
        view.count.push(tile.count);
        view.name.push(if entry == NO_ENTRY { String::new() } else { index.name(entry).to_owned() });
    }
    drop(index);
    view.took_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(view)
}

/// What grew within the folder at `path` over the last day, week and month, from the daily
/// samples; with none old enough yet, the hub shows recently changed files instead.
#[tauri::command(async)]
pub fn disk_growth(disk: Shared, path: String) -> Result<GrowthView, String> {
    history::growth(&disk.engine, &disk.db, &path)
}

/// Development builds only: takes a sample of the index now, or keeps or deletes the sample of
/// `days_ago` days back, so the end-to-end suite can give "What grew" a history.
#[tauri::command(async)]
pub fn disk_history_probe(
    disk: Shared,
    action: history::Probe,
    days_ago: i64,
    rows: Option<Vec<kestrel_analyze::growth::SampleRow>>,
) -> Result<Vec<kestrel_analyze::growth::SampleRow>, String> {
    if !cfg!(debug_assertions) {
        return Err("Only development builds have this hook".into());
    }
    history::probe(&disk.engine, &disk.db, action, days_ago, rows.unwrap_or_default())
}

/// The window's hub opened or closed: while it is open, index changes are announced to the
/// window as `disk-changed`.
#[tauri::command(async)]
pub fn disk_watch(window: Window, disk: Shared, open: bool) {
    disk.watching.set(window.label(), open);
}

/// Sends items to the Recycle Bin as one undoable operation. The hub has asked first. Returns
/// `false` if nothing was recycled (cancelled, or the items are gone).
#[tauri::command(async)]
pub fn disk_recycle(window: Window, journal: State<Journal>, paths: Vec<String>) -> Result<bool, String> {
    let paths: Vec<String> = paths.into_iter().map(|path| normalize(&path)).filter(|path| std::path::Path::new(path).exists()).collect();
    if paths.is_empty() {
        return Ok(false);
    }
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    let outcome = fileops::execute(Request::Delete { paths, permanent: false }, Options { owner, quiet: false })?;
    journal.record(&outcome);
    Ok(outcome.completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta, attr};

    /// `C:\` with a film, an online-only video and the page file.
    fn engine() -> Engine {
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let users = builder.push(root, "Users", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        builder.push(users, "film.mkv", Meta { size: 4 << 20, alloc: 4 << 20, mtime: 1, ..Meta::default() });
        let online = Meta { size: 9 << 20, alloc: 0, attrs: attr::RECALL_ON_DATA_ACCESS, ..Meta::default() };
        builder.push(users, "online.mp4", online);
        builder.push(root, "pagefile.sys", Meta { size: 2 << 20, alloc: 2 << 20, ..Meta::default() });
        engine.install(builder.finish());
        engine
    }

    /// Two windows with the hub open: one closing its hub leaves the other's told of changes,
    /// and a window that closed with its hub open is forgotten.
    #[test]
    fn each_window_watches_for_its_own_hub() {
        let watchers = Watchers::default();
        watchers.set("main", true);
        watchers.set("main-2", true);
        watchers.set("main", false);
        assert_eq!(watchers.to_tell(|_| true), ["main-2"]);
        assert!(!watchers.is_empty());
        assert_eq!(watchers.to_tell(|window| window != "main-2"), Vec::<String>::new());
        assert!(watchers.is_empty(), "a closed window is forgotten");
    }

    #[test]
    fn paths_are_found_however_they_are_written() {
        let engine = engine();
        let index = engine.index().read();
        assert_eq!(locate(&index, "c:/users/").map(|id| index.path(id)), Ok(r"C:\Users".to_owned()));
        assert!(locate(&index, r"C:\nowhere").unwrap_err().contains("not in the index"));
        assert_eq!(index.path(top_of(&index, locate(&index, r"C:\Users\film.mkv").unwrap())), r"C:\");
        assert_eq!(px(1.23456), 1.23);
    }

    #[test]
    fn the_overview_sets_the_index_beside_the_drive() {
        let view = overview_of(&engine(), "C:").unwrap();
        assert_eq!((view.path.as_str(), view.is_drive, view.is_dir), (r"C:\", true, true));
        assert_eq!(view.analysis.totals.local.bytes, 6 << 20);
        assert_eq!((view.analysis.totals.cloud_files, view.analysis.totals.cloud_bytes), (1, 9 << 20));
        let volume = view.volume.expect("the real C: drive describes itself");
        assert_eq!((volume.indexed, volume.indexed_files), (6 << 20, 3));
        assert!(volume.used > 0 && volume.used < volume.volume.total);
        // The test index holds a page file, so it is counted; the others are looked for on the
        // real drive, where this machine may or may not have them.
        let page = volume.system_files.iter().find(|file| file.name == "pagefile.sys").unwrap();
        assert!(page.counted && page.bytes == 2 << 20);
        assert!(volume.system_files.iter().filter(|file| file.name != "pagefile.sys").all(|file| !file.counted));

        let folder = overview_of(&engine(), r"C:\Users").unwrap();
        assert!(!folder.is_drive);
        assert_eq!(folder.volume.unwrap().indexed_files, 0, "only a drive's own overview counts its files");
    }

    /// The hub works its overview out again every few seconds while the index changes: the
    /// index's writer, and every reader behind it, gets in between steps of the walk.
    #[test]
    fn the_overview_lets_the_index_be_written_meanwhile() {
        let engine = crate::engine::busy::engine(400_000, 4);
        // What one walk under one lock takes, which is how long the writer waited before.
        let whole = {
            let index = engine.index().read();
            let root = locate(&index, r"C:\Users\me").unwrap();
            let started = Instant::now();
            overview::overview(&index, root, &overview::Options { now_ms: now_ms(), top: TOP });
            started.elapsed()
        };
        let (_, worst) = crate::engine::busy::writer_waits(&engine, || {
            let view = overview_of(&engine, r"C:\Users\me").unwrap();
            assert_eq!(view.analysis.totals.local.files, 1_600_000);
        });
        assert!(whole > std::time::Duration::from_millis(10), "too quick to tell: {whole:?}");
        assert!(worst < whole / 2, "the writer waited {worst:?}; the whole walk takes {whole:?}");
    }

    #[test]
    fn treemaps_are_sent_a_column_per_field() {
        let view = treemap_of(&engine(), r"C:\", 400.0, 300.0, Measure::Allocated).unwrap();
        assert_eq!(view.total, 6 << 20);
        let names: Vec<&str> = view.name.iter().map(String::as_str).collect();
        // The online-only video takes no space on disk.
        assert_eq!(names, ["Users", "pagefile.sys", "film.mkv"]);
        assert_eq!(view.kind, [1, 0, 0]);
        assert_eq!(view.parent, [-1, -1, 0]);
        assert_eq!(view.flags[0] & 1, 1, "Users is open");
        let area = |i: usize| view.w[i] as f64 * view.h[i] as f64;
        assert!((area(0) + area(1) - 400.0 * 300.0).abs() < 1.0);

        let logical = treemap_of(&engine(), r"C:\", 400.0, 300.0, Measure::Logical).unwrap();
        let online = logical.name.iter().position(|name| name == "online.mp4").unwrap();
        assert_eq!(logical.flags[online] & 4, 4, "marked as in the cloud");
    }
}
