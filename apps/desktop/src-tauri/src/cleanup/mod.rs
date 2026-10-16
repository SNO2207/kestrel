//! Cleanup recommendations and the duplicate finder (`kestrel-cleanup`), for the disk usage
//! hub's panels.
//!
//! A scan runs every rule over the index and remembers what it recommended; every action is
//! checked against that, so the page can only ever act on what a scan offered. Nothing is
//! removed until the owner ticks items and confirms: then the items still allowed (by the same
//! guard the rules use) and not held open by a program go to the Recycle Bin through
//! `fileops`, as one operation in the undo journal. Tools (`cargo clean` and the like) run only
//! on a click, by their full path on the PATH, and their output comes back. A dry run reports
//! what would happen and changes nothing.
//!
//! Development builds have a hook that points every rule at a scratch folder laid out like a
//! machine (`Locations::under`), says which programs run and which tools are installed, and
//! records tool runs and openings instead of performing them: the end-to-end tests use it, so
//! they never look at the owner's real caches or start a real tool.

mod tool;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kestrel_cleanup::dupes::{self, Group, Progress};

use crate::semantic::similar::{self, SimilarGroup, SimilarReport};
use kestrel_cleanup::inuse::blocker;
use kestrel_cleanup::{Action, BinContents, Context, Guard, Locations, Project, Recommendation, Rule, Tier, Tools};
use kestrel_db::Db;
use kestrel_index::walk;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, Window};

use crate::engine::Engine;
use crate::fileops::{self, Done, Options, Request};
use crate::projects::Projects;
use crate::undo::Journal;

/// Items looked at inside a folder to tell whether a program holds any of it open.
const BLOCKER_LIMIT: usize = 20_000;
/// A tool still running after this long is stopped.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Cached hashes of files no search has seen for this long are forgotten.
const HASHES_KEPT_SECONDS: i64 = 180 * 86_400;

/// The test hook's stand-in for the machine. Development builds only.
#[derive(Clone, Debug, Deserialize)]
pub struct Sandbox {
    /// The scratch folder laid out like a machine.
    base: String,
    /// Programs said to be running, such as `chrome.exe`.
    #[serde(default)]
    running: Vec<String>,
    /// Tools said to be installed, such as `cargo`.
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    recycle_bin: Option<BinContents>,
    /// Files larger than this count as large (1 GB on a real machine).
    #[serde(default)]
    large_file_bytes: Option<u64>,
}

/// A tool run or an opening the test hook recorded instead of performing.
#[derive(Clone, Debug, Serialize)]
pub struct Record {
    kind: &'static str,
    program: String,
    args: Vec<String>,
    dir: Option<String>,
}

#[derive(Default)]
struct Latest {
    /// The latest scan's recommendations, by id.
    recs: HashMap<String, Recommendation>,
    /// The guard that scan ran with.
    guard: Option<Guard>,
    /// The latest duplicate search's groups.
    groups: Vec<Group>,
    /// The latest search's similar photos (AI search's vectors, `semantic::similar`).
    similar: Vec<SimilarGroup>,
}

pub struct Cleanup {
    engine: Arc<Engine>,
    db: Arc<Db>,
    projects: Arc<Projects>,
    latest: Mutex<Latest>,
    sandbox: Mutex<Option<Sandbox>>,
    records: Mutex<Vec<Record>>,
    /// Stops the duplicate search in flight.
    cancel: Mutex<Arc<AtomicBool>>,
    /// Held while a duplicate search runs; one at a time.
    searching: Mutex<()>,
}

type Shared<'a> = State<'a, Arc<Cleanup>>;

fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

fn key(path: &str) -> String {
    fileops::key(path)
}

/// "1.2 GB", as the page shows sizes.
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 { format!("{value:.0} {}", UNITS[unit]) } else { format!("{value:.1} {}", UNITS[unit]) }
}

fn items_text(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

impl Cleanup {
    pub fn new(engine: Arc<Engine>, db: Arc<Db>, projects: Arc<Projects>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            db,
            projects,
            latest: Mutex::new(Latest::default()),
            sandbox: Mutex::new(None),
            records: Mutex::new(Vec::new()),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            searching: Mutex::new(()),
        })
    }

    /// What the rules look at: this machine, or the test hook's scratch folder.
    fn context(&self) -> (Context, Option<String>) {
        let data = crate::indexer::data_dir().to_string_lossy().into_owned();
        let projects: Vec<Project> =
            self.projects.roots_for_cleanup().into_iter().map(|(root, dismissed)| Project { root, dismissed }).collect();
        let Some(sandbox) = self.sandbox.lock().clone() else { return (Context::current(&data, projects), None) };
        let base = kestrel_index::fold(&sandbox.base);
        let context = Context {
            locations: Locations::under(Path::new(&sandbox.base)),
            projects: projects.into_iter().filter(|p| kestrel_index::fold(&p.root).starts_with(&base)).collect(),
            protected: vec![data],
            now: kestrel_cleanup::now(),
            running: sandbox.running.iter().map(|name| name.to_lowercase()).collect(),
            tools: Tools::with(&sandbox.tools.iter().map(String::as_str).collect::<Vec<_>>()),
            recycle_bin: sandbox.recycle_bin,
            min_bytes: 1,
            large_file_bytes: sandbox.large_file_bytes.unwrap_or(1 << 30),
        };
        (context, Some(sandbox.base))
    }

    fn recording(&self) -> bool {
        cfg!(debug_assertions) && self.sandbox.lock().is_some()
    }

    fn record(&self, kind: &'static str, program: &str, args: &[String], dir: Option<&str>) {
        self.records.lock().push(Record { kind, program: program.to_owned(), args: args.to_vec(), dir: dir.map(str::to_owned) });
    }

    fn profile(&self) -> PathBuf {
        let sandbox = self.sandbox.lock().clone();
        let locations = match sandbox {
            Some(sandbox) => Locations::under(Path::new(&sandbox.base)),
            None => Locations::current(),
        };
        PathBuf::from(locations.profile)
    }
}

// ---------- recommendations ----------

#[derive(Serialize)]
pub struct ScanReport {
    recommendations: Vec<Recommendation>,
    /// The index is still being built, so there may be more.
    indexing: bool,
    /// Bytes the Safe and Review recommendations would free.
    total: u64,
    took_ms: u64,
}

/// Runs every rule. With `root`, only what lies beneath it is kept (and, unless it is a drive's
/// root, nothing that has no place of its own, such as the Recycle Bin).
#[tauri::command(async)]
pub fn cleanup_scan(cleanup: Shared, root: Option<String>) -> ScanReport {
    let started = Instant::now();
    let indexing = cleanup.engine.status().state != "ready";
    let (context, sandbox) = cleanup.context();
    // The index is read one rule at a time, so the thread keeping it current never waits long.
    let mut scan = kestrel_cleanup::Scan::new(&context);
    for &rule in kestrel_cleanup::RULES {
        let index = cleanup.engine.index().read();
        scan.run(rule, &index);
    }
    let mut recs = kestrel_cleanup::existing(scan.finish());
    if let Some(base) = &sandbox {
        recs = kestrel_cleanup::within_root(recs, base, true);
    }
    if let Some(root) = root.as_deref().filter(|root| !root.trim().is_empty()) {
        let whole_drive = crate::engine::normalize(root).len() <= 3;
        recs = kestrel_cleanup::within_root(recs, root, whole_drive);
    }
    let total = recs.iter().filter(|r| r.tier != Tier::Info).map(|r| r.bytes).sum();
    let mut latest = cleanup.latest.lock();
    latest.recs = recs.iter().map(|r| (r.id.clone(), r.clone())).collect();
    latest.guard = Some(context.guard());
    drop(latest);
    ScanReport { recommendations: recs, indexing, total, took_ms: started.elapsed().as_millis() as u64 }
}

/// Items the owner ticked in one recommendation.
#[derive(Deserialize)]
pub struct Chosen {
    id: String,
    paths: Vec<String>,
}

#[derive(Serialize)]
pub struct Skipped {
    path: String,
    reason: String,
}

#[derive(Serialize, Default)]
pub struct RecycleReport {
    dry_run: bool,
    /// What went (or, in a dry run, would go) to the Recycle Bin.
    recycled: Vec<String>,
    /// What the shell erased instead, after asking (an item too large for the Recycle Bin).
    erased: Vec<String>,
    skipped: Vec<Skipped>,
    bytes: u64,
    /// Nothing was cancelled or failed.
    completed: bool,
    message: String,
}

/// The shell's dialogs belong to the window.
fn interactive(window: &Window) -> Options {
    Options { owner: window.hwnd().ok().map(|hwnd| hwnd.0 as isize), quiet: false }
}

/// Checks each item (`path`, bytes) can go now and, unless `dry_run`, sends those that can to
/// the Recycle Bin as one operation in the undo journal.
fn recycle(options: Options, journal: &Journal, items: Vec<(String, u64)>, mut skipped: Vec<Skipped>, dry_run: bool) -> Result<RecycleReport, String> {
    let mut ready = Vec::new();
    for (path, bytes) in items {
        match blocker(&path, BLOCKER_LIMIT) {
            Some(why) => skipped.push(Skipped { path, reason: why.describe().into() }),
            None => ready.push((path, bytes)),
        }
    }
    let sizes: HashMap<String, u64> = ready.iter().map(|(path, bytes)| (key(path), *bytes)).collect();
    let mut report = RecycleReport { dry_run, ..RecycleReport::default() };
    if dry_run {
        report.bytes = ready.iter().map(|(_, bytes)| bytes).sum();
        report.recycled = ready.into_iter().map(|(path, _)| path).collect();
        report.completed = true;
        report.skipped = skipped;
        report.message = format!(
            "Nothing was changed. {} ({}) would go to the Recycle Bin{}.",
            items_text(report.recycled.len()),
            size_text(report.bytes),
            match report.skipped.len() {
                0 => String::new(),
                n => format!(", and {} would be skipped", items_text(n)),
            }
        );
        return Ok(report);
    }
    if !ready.is_empty() {
        let paths: Vec<String> = ready.iter().map(|(path, _)| path.clone()).collect();
        let outcome = fileops::execute(Request::Delete { paths, permanent: false }, options)?;
        journal.record(&outcome);
        report.completed = outcome.completed;
        let mut done = HashSet::new();
        for result in outcome.results() {
            match result {
                Done::Recycled { path, .. } => {
                    done.insert(key(path));
                    report.bytes += sizes.get(&key(path)).copied().unwrap_or(0);
                    report.recycled.push(path.clone());
                }
                Done::Erased { path } => {
                    done.insert(key(path));
                    report.erased.push(path.clone());
                }
                _ => {}
            }
        }
        for (path, _) in ready {
            if !done.contains(&key(&path)) {
                skipped.push(Skipped { path, reason: "it wasn't moved".into() });
            }
        }
    } else {
        report.completed = true;
    }
    report.skipped = skipped;
    report.message = if report.recycled.is_empty() {
        "Nothing was moved to the Recycle Bin.".into()
    } else {
        format!("Moved {} ({}) to the Recycle Bin.", items_text(report.recycled.len()), size_text(report.bytes))
    };
    if !report.skipped.is_empty() {
        report.message.push_str(&format!(" {} skipped.", if report.skipped.len() == 1 { "1 item was".into() } else { format!("{} items were", report.skipped.len()) }));
    }
    Ok(report)
}

/// Sends the ticked items of recommendations to the Recycle Bin, or with `dry_run` says what
/// would happen. Every item must be one the latest scan offered for the Recycle Bin, and still
/// be allowed where it is now.
#[tauri::command(async)]
pub fn cleanup_recycle(window: Window, cleanup: Shared, journal: State<Journal>, chosen: Vec<Chosen>, dry_run: bool) -> Result<RecycleReport, String> {
    let (recs, guard) = {
        let latest = cleanup.latest.lock();
        (latest.recs.clone(), latest.guard.clone().ok_or("Scan first.")?)
    };
    let mut ready = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = HashSet::new();
    let changed = |item: &kestrel_cleanup::Item| Skipped { path: item.path.clone(), reason: "it changed since the scan".into() };
    {
        let index = cleanup.engine.index().read();
        for choice in &chosen {
            let rec = recs.get(&choice.id).ok_or("That recommendation is out of date. Scan again.")?;
            if rec.tier == Tier::Info || rec.action != Action::Recycle {
                return Err(format!("\"{}\" isn't something Kestrel moves to the Recycle Bin.", rec.title));
            }
            for path in &choice.paths {
                let item = rec
                    .items
                    .iter()
                    .find(|item| key(&item.path) == key(path))
                    .ok_or_else(|| format!("{path} isn't part of \"{}\". Scan again.", rec.title))?;
                if !seen.insert(key(path)) {
                    continue;
                }
                match index.lookup(&item.path).filter(|&entry| !index.is_deleted(entry)) {
                    None => skipped.push(Skipped { path: item.path.clone(), reason: "it's no longer there".into() }),
                    // Another file under its name now, or something in it changed: the scan's
                    // reasons for offering it may no longer hold.
                    Some(entry) if !item.unchanged(&index, entry) => skipped.push(changed(item)),
                    Some(entry) => {
                        let allowed =
                            if rec.rule == Rule::Build { guard.allows_build_output(&index, entry) } else { guard.allows(&index, entry) };
                        if allowed {
                            ready.push(item.clone());
                        } else {
                            skipped.push(Skipped { path: item.path.clone(), reason: "it's somewhere Kestrel never cleans up".into() });
                        }
                    }
                }
            }
        }
    }
    // And as it is on disk this moment, read from each item itself: the index hears of a change
    // a moment after it happens.
    let mut items = Vec::new();
    for item in ready {
        if item.unchanged_on_disk() {
            items.push((item.path, item.bytes));
        } else {
            skipped.push(changed(&item));
        }
    }
    let report = recycle(interactive(&window), &journal, items, skipped, dry_run)?;
    if !dry_run {
        let gone: HashSet<String> = report.recycled.iter().chain(&report.erased).map(|path| key(path)).collect();
        let mut latest = cleanup.latest.lock();
        for rec in latest.recs.values_mut() {
            rec.items.retain(|item| !gone.contains(&key(&item.path)));
        }
    }
    Ok(report)
}

#[derive(Serialize)]
pub struct ToolReport {
    command: String,
    dir: Option<String>,
    dry_run: bool,
    /// The test hook recorded it instead of running it.
    recorded: bool,
    /// The exit code, once it ran.
    status: Option<i32>,
    timed_out: bool,
    /// What it printed, the end of it if long.
    output: String,
}

/// Runs a recommendation's tool and returns what it printed, or with `dry_run` says what would run.
#[tauri::command(async)]
pub fn cleanup_run_tool(cleanup: Shared, id: String, dry_run: bool) -> Result<ToolReport, String> {
    let rec = cleanup.latest.lock().recs.get(&id).cloned().ok_or("That recommendation is out of date. Scan again.")?;
    let Action::Tool(tool) = rec.action else { return Err(format!("\"{}\" has no tool to run.", rec.title)) };
    let mut report = ToolReport {
        command: tool.command.clone(),
        dir: tool.dir.clone(),
        dry_run,
        recorded: false,
        status: None,
        timed_out: false,
        output: String::new(),
    };
    if dry_run {
        return Ok(report);
    }
    let dir = tool.dir.clone().map(PathBuf::from).unwrap_or_else(|| cleanup.profile());
    if cleanup.recording() {
        cleanup.record("tool", &tool.program, &tool.args, Some(&dir.to_string_lossy()));
        report.recorded = true;
        report.status = Some(0);
        report.output = format!("The test hook recorded {} instead of running it.", tool.command);
    } else {
        let tools = Tools::detect();
        let program = tools.path(&tool.program).ok_or_else(|| format!("{} isn't installed.", tool.program))?;
        let ran = tool::run(program, &tool.args, &dir, TOOL_TIMEOUT)?;
        report.status = ran.status;
        report.timed_out = ran.timed_out;
        report.output = ran.output;
    }
    cleanup.latest.lock().recs.remove(&id);
    Ok(report)
}

/// Opens Storage Sense or Disk Cleanup. Neither starts elevated: Disk Cleanup asks for an
/// administrator only if the owner chooses "Clean up system files" in it.
#[tauri::command(async)]
pub fn cleanup_open(cleanup: Shared, target: String) -> Result<(), String> {
    let (file, params) = match target.as_str() {
        "ms-settings:storagesense" => (target.clone(), String::new()),
        "cleanmgr" => {
            let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
            (format!(r"{windows}\System32\cleanmgr.exe"), format!("/d {}", drive.trim_end_matches(['\\', ':'])))
        }
        _ => return Err(format!("Kestrel doesn't open {target}")),
    };
    if cleanup.recording() {
        cleanup.record("open", &file, &[params], None);
        return Ok(());
    }
    tool::open(&file, &params)
}

// ---------- duplicates ----------

#[derive(Serialize)]
pub struct DupeDefaults {
    roots: Vec<String>,
    exclude: Vec<String>,
    min_size: u64,
}

/// Where the duplicate finder looks unless told otherwise: the profile, without AppData.
#[tauri::command(async)]
pub fn dupes_defaults(cleanup: Shared) -> DupeDefaults {
    let sandbox = cleanup.sandbox.lock().clone();
    let locations = match sandbox {
        Some(sandbox) => Locations::under(Path::new(&sandbox.base)),
        None => Locations::current(),
    };
    let (roots, exclude) = locations.duplicate_roots();
    DupeDefaults { roots, exclude, min_size: 1 << 20 }
}

#[derive(Deserialize)]
pub struct DupeRequest {
    roots: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    min_size: u64,
}

/// Looks for duplicate files; `cleanup-dupes-progress` events say how far it has got. `None`
/// when it was cancelled.
#[tauri::command(async)]
pub fn dupes_scan(app: AppHandle, cleanup: Shared, request: DupeRequest) -> Result<Option<dupes::Report>, String> {
    let _searching = cleanup.searching.try_lock().ok_or("A search for duplicates is already running.")?;
    let cancel = Arc::new(AtomicBool::new(false));
    *cleanup.cancel.lock() = cancel.clone();
    let request = dupes::Request { roots: request.roots, exclude: request.exclude, min_size: request.min_size.max(1) };
    let candidates = find_candidates(&cleanup.engine, &request);
    let progress = |progress: Progress| {
        let _ = app.emit("cleanup-dupes-progress", progress);
    };
    let found = dupes::find(candidates, Some(&cleanup.db), &progress, &cancel);
    let seen_since = kestrel_cleanup::unix_ms(kestrel_cleanup::now()) / 1000 - HASHES_KEPT_SECONDS;
    let _ = cleanup.db.cleanup_forget_hashes_before(seen_since);
    match found {
        Ok(report) => {
            cleanup.latest.lock().groups = report.groups.clone();
            Ok(Some(report))
        }
        Err(dupes::Cancelled) => Ok(None),
    }
}

/// The duplicate finder's candidates, from the index, read in steps that let go of it between
/// them: a walk of a whole drive takes a quarter of a second, which the thread keeping the
/// index current, and every reader behind it, would otherwise wait out.
fn find_candidates(engine: &Engine, request: &dupes::Request) -> dupes::Candidates {
    let walk = engine.in_steps(|index| dupes::CandidateWalk::new(index, request), |walk, index| walk.step(index, dupes::STEP), |walk, _| walk);
    // Sorted with the index let go of.
    walk.finish()
}

/// Looks for similar photos (near duplicates: a burst, an edit, a resized copy) where the
/// duplicate search looked, by comparing AI search's vectors of the photos the media library has
/// described. Nothing is read from disk. Empty when no model is in use; `None` when cancelled.
/// The Duplicates tab lists them apart from exact copies and never chooses any for the owner.
#[tauri::command(async)]
pub fn dupes_similar(cleanup: Shared, semantic: crate::semantic::Shared, request: DupeRequest) -> Result<Option<SimilarReport>, String> {
    let _searching = cleanup.searching.try_lock().ok_or("A search for duplicates is already running.")?;
    let cancel = Arc::new(AtomicBool::new(false));
    *cleanup.cancel.lock() = cancel.clone();
    let within = similar::Within { roots: &request.roots, exclude: &request.exclude, min_size: request.min_size.max(1) };
    let found = similar::find(&semantic, &cleanup.engine, &within, &cancel);
    if let Some(report) = &found {
        cleanup.latest.lock().similar = report.groups.clone();
    }
    Ok(found)
}

#[tauri::command]
pub fn dupes_cancel(cleanup: Shared) {
    cleanup.cancel.lock().store(true, Ordering::Relaxed);
}

/// In one group of duplicates (or of similar photos), the copy to keep and those to remove.
#[derive(Deserialize)]
pub struct Removal {
    keep: String,
    remove: Vec<String>,
}

/// A file of a group, as recycling needs it: what it was when found, and its size on disk.
struct Copy {
    path: String,
    file_id: u64,
    size: u64,
    mtime: i64,
    /// Other names of the same file, which go with it.
    links: Vec<String>,
    bytes: u64,
}

/// The files of the latest search's group holding `keep`: exact duplicates first, then similar
/// photos.
fn group_of(latest: &Latest, keep: &str) -> Option<Vec<Copy>> {
    if let Some(group) = latest.groups.iter().find(|g| g.files.iter().any(|f| key(&f.path) == key(keep))) {
        return Some(
            group
                .files
                .iter()
                .map(|f| Copy { path: f.path.clone(), file_id: f.file_id, size: f.size, mtime: f.mtime, links: f.links.clone(), bytes: group.bytes_each })
                .collect(),
        );
    }
    let group = latest.similar.iter().find(|g| g.files.iter().any(|f| key(&f.path) == key(keep)))?;
    Some(group.files.iter().map(|f| Copy { path: f.path.clone(), file_id: f.file_id, size: f.size, mtime: f.mtime, links: Vec::new(), bytes: f.bytes }).collect())
}

/// Sends copies to the Recycle Bin, keeping one of each group, or with `dry_run` says what would
/// happen. Each copy, and the one kept, must be as the search found it. Groups of similar photos
/// are taken the same way, but only ever those the owner ticked.
#[tauri::command(async)]
pub fn dupes_recycle(window: Window, cleanup: Shared, journal: State<Journal>, removals: Vec<Removal>, dry_run: bool) -> Result<RecycleReport, String> {
    let unchanged = |file: &Copy| walk::info(&file.path).is_some_and(|meta| meta.file_id == file.file_id && meta.size == file.size && meta.mtime == file.mtime);
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for removal in &removals {
        let files = group_of(&cleanup.latest.lock(), &removal.keep).ok_or("Those duplicates are out of date. Search again.")?;
        let keep = files.iter().find(|f| key(&f.path) == key(&removal.keep)).ok_or("Search again.")?;
        let removing: Vec<&Copy> = removal
            .remove
            .iter()
            .map(|path| files.iter().find(|f| key(&f.path) == key(path)).ok_or_else(|| format!("{path} isn't a copy of {}.", removal.keep)))
            .collect::<Result<_, _>>()?;
        if removing.iter().any(|f| key(&f.path) == key(&keep.path)) {
            return Err("The copy you keep can't also be removed.".into());
        }
        if !unchanged(keep) {
            for file in removing {
                skipped.push(Skipped { path: file.path.clone(), reason: "the copy you keep changed since the search".into() });
            }
            continue;
        }
        for file in removing {
            if !unchanged(file) {
                skipped.push(Skipped { path: file.path.clone(), reason: "it changed since the search".into() });
                continue;
            }
            // Other names of the same file go with it, or its space wouldn't be freed; each only
            // while it still names that file.
            items.push((file.path.clone(), file.bytes));
            for link in &file.links {
                if walk::info(link).is_some_and(|meta| meta.file_id == file.file_id) {
                    items.push((link.clone(), 0));
                } else {
                    skipped.push(Skipped { path: link.clone(), reason: "it's no longer another name of the copy".into() });
                }
            }
        }
    }
    let report = recycle(interactive(&window), &journal, items, skipped, dry_run)?;
    if !dry_run {
        let gone: HashSet<String> = report.recycled.iter().chain(&report.erased).map(|path| key(path)).collect();
        let mut latest = cleanup.latest.lock();
        for group in &mut latest.groups {
            group.files.retain(|f| !gone.contains(&key(&f.path)));
        }
        latest.groups.retain(|g| g.files.len() >= 2);
        for group in &mut latest.similar {
            group.files.retain(|f| !gone.contains(&key(&f.path)));
        }
        latest.similar.retain(|g| g.files.len() >= 2);
    }
    Ok(report)
}

// ---------- development hook ----------

/// Development builds only: points every rule at a scratch folder laid out like a machine (or,
/// with `None`, back at this one), and records tool runs and openings instead of performing them.
#[tauri::command(async)]
pub fn cleanup_dev_sandbox(cleanup: Shared, sandbox: Option<Sandbox>) -> Result<(), String> {
    development_only()?;
    *cleanup.sandbox.lock() = sandbox;
    cleanup.records.lock().clear();
    *cleanup.latest.lock() = Latest::default();
    Ok(())
}

/// Development builds only: what was recorded since the last call.
#[tauri::command(async)]
pub fn cleanup_dev_take(cleanup: Shared) -> Result<Vec<Record>, String> {
    development_only()?;
    Ok(std::mem::take(&mut *cleanup.records.lock()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{QUIET, sandbox, text};

    /// Sends two tiny files named `kestrel-test-*` through the Recycle Bin and back, as the undo
    /// journal's own test does; a third, held open, is skipped with a note.
    #[test]
    fn recycling_skips_what_is_in_use_and_undo_brings_the_rest_back() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = sandbox("cleanup-recycle");
        let files: Vec<std::path::PathBuf> =
            ["kestrel-test-a.tmp", "kestrel-test-b.tmp", "kestrel-test-held.log"].iter().map(|n| dir.join(n)).collect();
        for file in &files {
            std::fs::write(file, b"temporary").unwrap();
        }
        let items: Vec<(String, u64)> = files.iter().map(|f| (text(f), 4096)).collect();
        let journal = Journal::default();
        // Others may read it, but not delete it.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&files[2]).unwrap();

        let plan = recycle(QUIET, &journal, items.clone(), Vec::new(), true).unwrap();
        assert_eq!((plan.recycled.len(), plan.skipped.len(), plan.bytes), (2, 1, 8192));
        assert!(plan.message.starts_with("Nothing was changed."), "{}", plan.message);
        assert!(files.iter().all(|f| f.exists()), "a dry run changes nothing");

        let done = recycle(QUIET, &journal, items, Vec::new(), false).unwrap();
        drop(held);
        assert_eq!(done.recycled.len(), 2, "{:?}", done.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>());
        assert_eq!(done.skipped[0].reason, "a program has it open");
        assert!(!files[0].exists() && !files[1].exists() && files[2].exists());
        assert_eq!(done.message, "Moved 2 items (8.0 KB) to the Recycle Bin. 1 item was skipped.");

        let undone = journal.undo(QUIET).unwrap();
        assert!(undone.message.starts_with("Undid"), "{}", undone.message);
        assert!(files.iter().all(|f| f.exists()), "both are back from the Recycle Bin");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The search for duplicates walks the whole index for its candidates, which on a real
    /// drive takes a quarter of a second: the index's writer, and every reader behind it, gets
    /// in between steps rather than after the walk.
    #[test]
    fn the_candidates_walk_lets_the_index_be_written_meanwhile() {
        let engine = crate::engine::busy::engine(2_000, 300);
        let request = dupes::Request { roots: vec![r"C:\Users\me".into()], exclude: Vec::new(), min_size: 1 };
        let (took, worst) = crate::engine::busy::writer_waits(&engine, || {
            let found = find_candidates(&engine, &request);
            assert_eq!(found.files.len(), 600_000);
        });
        assert!(took > std::time::Duration::from_millis(60), "too quick to tell: {took:?}");
        assert!(worst < took / 4, "the writer waited {worst:?} of the walk's {took:?}");
    }

    #[test]
    fn sizes_read_as_the_page_shows_them() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(5 << 30), "5.0 GB");
        assert_eq!(size_text(200 << 20), "200 MB");
        assert_eq!(items_text(1), "1 item");
        assert_eq!(items_text(3), "3 items");
    }
}
