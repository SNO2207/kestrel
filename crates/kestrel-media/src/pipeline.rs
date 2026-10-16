//! The media pass: every photo and video in the scopes goes through a list of [`Stage`]s, in the
//! background, once per content.
//!
//! # How it works
//!
//! [`Pipeline::plan`] is handed what is in the scopes now ([`Candidate`]s, from the index by
//! [`crate::scan`]), whenever the index changes. Each file is an item known by its volume and
//! file id (`kestrel_db::media`), and each stage's record on an item names the size and last
//! write time it worked from and the stage's version. So:
//!
//! - a rename or a move changes only the item's path: no stage runs again;
//! - a changed file (another size or time) runs every stage again;
//! - a new version of a stage, or a new stage, runs over everything it applies to;
//! - a file that leaves the scopes is marked gone, and coming back costs nothing.
//!
//! Workers take items newest first. For each one they check the file's directory entry (never
//! opening a cloud-only file: that would download it), read its facts with the property system
//! (the built-in `metadata` stage), decode it once at the largest size any stage needs (WIC for
//! photos, a frame a tenth of the way in for videos, turned as shown), and hand the facts and
//! the picture to each stage that still has work on it. A stage that fails says why; that is
//! recorded and the stage is not tried again until the file changes.
//!
//! Workers run at background priority (`THREAD_MODE_BACKGROUND_BEGIN`, which lowers their I/O
//! and memory priority too), stop between items while paused ([`Pipeline::set_paused`]), and
//! wait while [`Config::conditions`] gives a reason (a full-screen app or game, battery saver).
//!
//! # Adding a stage
//!
//! Implement [`Stage`] and [`Pipeline::register`] it, before or after the workers start. Its
//! output belongs in tables of its own, keyed by [`Item::id`], which stays the same for a file
//! for as long as the library knows it; [`Stage::forget`] is told which ids were dropped. Per
//! thread state (an OCR engine, a model session) is the stage's own business, such as a
//! `thread_local!`; `run` is called from several worker threads at once.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_db::media::{self as store, Found, StageRecord};
use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;

use crate::decode::Bitmap;
use crate::{Facts, Kind, OnDisk};

/// The built-in stage that reads facts from the property system; every other stage gets them.
pub const METADATA: &str = "metadata";
/// Bumped when what the metadata stage reads changes, so every item is read again.
pub const METADATA_VERSION: u32 = 1;
/// How long a gone item is kept, in case it comes back, before it is forgotten.
const KEEP_GONE_SECS: i64 = 30 * 86_400;
/// How often a waiting worker asks again whether it may go on (a game may have closed).
const RECHECK: Duration = Duration::from_secs(5);

/// A file the library works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The library's id for the file: the same for as long as the library knows the file,
    /// through renames and moves. Stages key what they keep by it.
    pub id: i64,
    pub volume: String,
    pub file_id: u64,
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    /// Last write time as a FILETIME.
    pub mtime: i64,
}

/// What a stage needs decoded before it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// The picture (a photo, or a frame of a video), at least this many pixels on its longer
    /// side when the file is that large; 0 for none.
    pub pixels: u32,
}

/// What a stage is given: the item's facts, and its picture when the stage asked for one.
pub struct Input<'a> {
    pub facts: &'a Facts,
    /// Turned as shown, at the largest size any stage on this item asked for: scale it down to
    /// what the stage itself wants ([`Bitmap::fit`]).
    pub picture: Option<&'a Bitmap>,
}

/// One step of the media pass.
pub trait Stage: Send + Sync {
    /// Stored with every record, so it must never change.
    fn name(&self) -> &'static str;
    /// Bump to run the stage again over everything.
    fn version(&self) -> u32;
    /// What it works on.
    fn applies_to(&self, kind: Kind) -> bool {
        let _ = kind;
        true
    }
    fn needs(&self) -> Needs;
    /// Does the stage's work on one item. An error says, in words for the owner, why it could
    /// not; the stage is not tried on the item again until the file changes.
    fn run(&self, item: &Item, input: &Input) -> Result<(), String>;
    /// The library has forgotten these items (gone from the scopes for a month).
    fn forget(&self, ids: &[i64]) {
        let _ = ids;
    }
}

/// A photo or video in the scopes, as the index has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub volume: String,
    pub file_id: u64,
    pub size: u64,
    pub mtime: i64,
    pub attrs: u32,
    pub path: String,
    pub kind: Kind,
    /// The index entry it was found at: good only within the index generation scanned.
    pub entry: kestrel_index::EntryId,
}

pub struct Config {
    pub workers: usize,
    /// Why work should wait now, if it should: asked between items.
    pub conditions: Box<dyn Fn() -> Option<String> + Send + Sync>,
    /// How often a waiting worker asks `conditions` again.
    pub recheck: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config { workers: 2, conditions: Box::new(crate::conditions::reason_to_wait), recheck: RECHECK }
    }
}

/// Something the app may want to follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A plan found new, changed or gone items.
    Planned { added: usize, changed: usize, gone: usize },
    /// An item's stages have run.
    Processed { id: i64 },
    /// Pausing, waiting or going on.
    State,
}

/// Why an item is passed over, in words for the settings page.
pub const ONLINE_ONLY: &str = "Online only: not downloaded to this PC";
pub const NO_FILE_ID: &str = "Its drive gives files no lasting id";

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Skipped {
    pub reason: String,
    pub count: u64,
    /// One of the files, to look at.
    pub example: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct StageCount {
    pub name: String,
    pub done: u64,
    pub failed: u64,
}

/// Where the pass stands.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Progress {
    /// `idle` (all done), `running`, `paused` (by the owner) or `waiting` (on `reason`).
    pub state: &'static str,
    pub reason: Option<String>,
    /// Photos and videos in the scopes.
    pub total: u64,
    pub photos: u64,
    pub videos: u64,
    /// Every stage has done its work on them.
    pub done: u64,
    /// Waiting for a stage.
    pub queued: u64,
    /// Passed over, or a stage could not do its work: how many, by reason.
    pub skipped: Vec<Skipped>,
    /// The files being read now.
    pub working: Vec<String>,
    pub stages: Vec<StageCount>,
}

#[derive(Clone, Debug)]
struct Record {
    version: u32,
    size: u64,
    mtime: i64,
    failed: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    id: i64,
    path: String,
    kind: Kind,
    size: u64,
    mtime: i64,
    attrs: u32,
    gone: bool,
    records: HashMap<String, Record>,
}

/// A queued item: those someone is waiting for first ([`Pipeline::hurry`]), then newest first.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    urgent: bool,
    mtime: i64,
    id: i64,
}

#[derive(Default)]
struct State {
    loaded: bool,
    /// Every item, by volume and file id.
    known: HashMap<(String, u64), Entry>,
    by_id: HashMap<i64, (String, u64)>,
    queue: BinaryHeap<Queued>,
    queued: HashSet<i64>,
    /// Items in the scopes on drives without file ids: counted, never read.
    unkeyed: Vec<String>,
    paused: bool,
    waiting: Option<String>,
    working: Vec<(i64, String)>,
    stop: bool,
}

struct Shared {
    db: Arc<Db>,
    workers: usize,
    stages: RwLock<Vec<Arc<dyn Stage>>>,
    state: Mutex<State>,
    wake: Condvar,
    /// Signalled whenever a worker finishes an item, for [`Pipeline::wait_idle`].
    settled: Condvar,
    conditions: Box<dyn Fn() -> Option<String> + Send + Sync>,
    recheck: Duration,
    listener: RwLock<Option<Arc<dyn Fn(Event) + Send + Sync>>>,
}

/// The media pass. Cheap to clone; the workers run until [`Pipeline::stop`].
#[derive(Clone)]
pub struct Pipeline {
    shared: Arc<Shared>,
}

impl Entry {
    fn record_matches(&self, name: &str, version: u32) -> bool {
        self.records.get(name).is_some_and(|r| r.version == version && r.size == self.size && r.mtime == self.mtime)
    }

    fn cloud_only(&self) -> bool {
        self.attrs & crate::CLOUD_ONLY != 0
    }

    fn item(&self, key: &(String, u64)) -> Item {
        Item { id: self.id, volume: key.0.clone(), file_id: key.1, path: self.path.clone(), kind: self.kind, size: self.size, mtime: self.mtime }
    }
}

impl Shared {
    fn notify(&self, event: Event) {
        let listener = self.listener.read().clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }

    /// The stages (metadata first) with work left on an entry.
    fn pending(&self, entry: &Entry) -> Vec<(&'static str, u32)> {
        let mut pending = Vec::new();
        if !entry.record_matches(METADATA, METADATA_VERSION) {
            pending.push((METADATA, METADATA_VERSION));
        }
        for stage in self.stages.read().iter() {
            if stage.applies_to(entry.kind) && !entry.record_matches(stage.name(), stage.version()) {
                pending.push((stage.name(), stage.version()));
            }
        }
        pending
    }

    fn enqueue(&self, state: &mut State, key: &(String, u64)) {
        let Some(entry) = state.known.get(key) else { return };
        if entry.gone || entry.cloud_only() || state.queued.contains(&entry.id) || self.pending(entry).is_empty() {
            return;
        }
        state.queued.insert(entry.id);
        state.queue.push(Queued { urgent: false, mtime: entry.mtime, id: entry.id });
    }
}

fn records_of(stages: Vec<StageRecord>) -> HashMap<String, Record> {
    stages.into_iter().map(|r| (r.stage, Record { version: r.version, size: r.size, mtime: r.mtime, failed: r.failed })).collect()
}

impl Pipeline {
    /// A pipeline over the library in `db`, not yet working: [`Pipeline::start`] starts it.
    pub fn new(db: Arc<Db>, config: Config) -> Self {
        let shared = Arc::new(Shared {
            db,
            workers: config.workers.max(1),
            stages: RwLock::new(Vec::new()),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            settled: Condvar::new(),
            conditions: config.conditions,
            recheck: config.recheck,
            listener: RwLock::new(None),
        });
        Pipeline { shared }
    }

    /// Adds a stage. Items it has not done are queued at the next [`Pipeline::plan`], or at once
    /// if the library is loaded already.
    pub fn register(&self, stage: Arc<dyn Stage>) {
        self.shared.stages.write().push(stage);
        let mut state = self.shared.state.lock();
        let keys: Vec<(String, u64)> = state.known.keys().cloned().collect();
        for key in &keys {
            self.shared.enqueue(&mut state, key);
        }
        drop(state);
        self.shared.wake.notify_all();
    }

    /// The registered stages' names, metadata first.
    pub fn stage_names(&self) -> Vec<&'static str> {
        std::iter::once(METADATA).chain(self.shared.stages.read().iter().map(|stage| stage.name())).collect()
    }

    /// Follows what the pipeline does.
    pub fn set_listener(&self, listener: impl Fn(Event) + Send + Sync + 'static) {
        *self.shared.listener.write() = Some(Arc::new(listener));
    }

    /// Starts the workers.
    pub fn start(&self) {
        for n in 0..self.shared.workers {
            let shared = self.shared.clone();
            let _ = std::thread::Builder::new().name(format!("kestrel-media-{n}")).spawn(move || work(&shared));
        }
    }

    /// Stops the workers once they finish the items in hand.
    pub fn stop(&self) {
        self.shared.state.lock().stop = true;
        self.shared.wake.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.state.lock().paused = paused;
        self.shared.wake.notify_all();
        self.shared.notify(Event::State);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.state.lock().paused
    }

    fn load(&self, state: &mut State) -> Result<(), String> {
        if state.loaded {
            return Ok(());
        }
        for known in store::known(&self.shared.db)? {
            let Some(kind) = Kind::from_code(known.kind) else { continue };
            let key = (known.volume, known.file_id);
            state.by_id.insert(known.id, key.clone());
            state.known.insert(key, Entry {
                id: known.id,
                path: known.path,
                kind,
                size: known.size,
                mtime: known.mtime,
                attrs: 0,
                gone: known.gone,
                records: records_of(known.stages),
            });
        }
        state.loaded = true;
        Ok(())
    }

    /// Brings the library in line with what is in the scopes now, and queues what has work
    /// left. Returns how many items were added, changed (or moved) and gone.
    pub fn plan(&self, candidates: Vec<Candidate>) -> Result<(usize, usize, usize), String> {
        let shared = &self.shared;
        let mut state = shared.state.lock();
        self.load(&mut state)?;
        let mut seen: HashSet<(String, u64)> = HashSet::with_capacity(candidates.len());
        let mut added: Vec<Found> = Vec::new();
        let mut changed: Vec<(i64, u64, i64, String)> = Vec::new();
        let mut attrs: HashMap<(String, u64), u32> = HashMap::with_capacity(candidates.len());
        let mut unkeyed = Vec::new();
        for candidate in candidates {
            if candidate.file_id == 0 {
                unkeyed.push(candidate.path);
                continue;
            }
            let key = (candidate.volume.clone(), candidate.file_id);
            if !seen.insert(key.clone()) {
                // A second name of one file (a hard link) is the same item.
                continue;
            }
            match state.known.get_mut(&key) {
                Some(entry) => {
                    if entry.gone || entry.path != candidate.path || entry.size != candidate.size || entry.mtime != candidate.mtime {
                        changed.push((entry.id, candidate.size, candidate.mtime, candidate.path.clone()));
                        entry.gone = false;
                        entry.path = candidate.path;
                        entry.size = candidate.size;
                        entry.mtime = candidate.mtime;
                    }
                }
                None => added.push(Found {
                    volume: candidate.volume,
                    file_id: candidate.file_id,
                    size: candidate.size,
                    mtime: candidate.mtime,
                    path: candidate.path,
                    kind: candidate.kind.code(),
                }),
            }
            // The index's attributes say whether the file is online only, before anything opens it.
            attrs.insert(key, candidate.attrs);
        }
        for (key, value) in &attrs {
            if let Some(entry) = state.known.get_mut(key) {
                entry.attrs = *value;
            }
        }
        let gone: Vec<i64> = state.known.iter().filter(|(key, entry)| !entry.gone && !seen.contains(*key)).map(|(_, entry)| entry.id).collect();
        state.unkeyed = unkeyed;
        // Written outside the lock, so the workers carry on meanwhile.
        drop(state);

        let ids = store::add(&shared.db, &added)?;
        store::update(&shared.db, &changed)?;
        store::mark_gone(&shared.db, &gone, crate::now_secs())?;
        let pruned = store::prune(&shared.db, crate::now_secs() - KEEP_GONE_SECS)?;

        let mut state = shared.state.lock();
        for (found, id) in added.iter().zip(&ids) {
            let key = (found.volume.clone(), found.file_id);
            let kind = Kind::from_code(found.kind).unwrap_or(Kind::Photo);
            state.by_id.insert(*id, key.clone());
            let entry_attrs = attrs.get(&key).copied().unwrap_or(0);
            state.known.insert(key, Entry {
                id: *id,
                path: found.path.clone(),
                kind,
                size: found.size,
                mtime: found.mtime,
                attrs: entry_attrs,
                gone: false,
                records: HashMap::new(),
            });
        }
        for id in &gone {
            if let Some(key) = state.by_id.get(id).cloned()
                && let Some(entry) = state.known.get_mut(&key)
            {
                entry.gone = true;
            }
        }
        for id in &pruned {
            if let Some(key) = state.by_id.remove(id) {
                state.known.remove(&key);
            }
        }
        let keys: Vec<(String, u64)> = seen.into_iter().collect();
        for key in &keys {
            shared.enqueue(&mut state, key);
        }
        drop(state);
        if !pruned.is_empty() {
            for stage in shared.stages.read().iter() {
                stage.forget(&pruned);
            }
        }
        shared.wake.notify_all();
        let report = (added.len(), changed.len(), gone.len());
        if report != (0, 0, 0) {
            shared.notify(Event::Planned { added: report.0, changed: report.1, gone: report.2 });
        }
        Ok(report)
    }

    /// Where the pass stands: counts, skips by reason, and what is being read.
    pub fn progress(&self) -> Progress {
        let shared = &self.shared;
        let state = shared.state.lock();
        let stages = shared.stages.read();
        let mut progress = Progress::default();
        let mut skipped: HashMap<String, Skipped> = HashMap::new();
        let mut skip = |reason: &str, path: &str| {
            let entry = skipped.entry(reason.to_owned()).or_insert_with(|| Skipped { reason: reason.to_owned(), count: 0, example: path.to_owned() });
            entry.count += 1;
        };
        let mut counts: Vec<StageCount> =
            std::iter::once(METADATA).chain(stages.iter().map(|s| s.name())).map(|name| StageCount { name: name.to_owned(), ..StageCount::default() }).collect();
        for entry in state.known.values().filter(|entry| !entry.gone) {
            progress.total += 1;
            match entry.kind {
                Kind::Photo => progress.photos += 1,
                Kind::Video => progress.videos += 1,
            }
            if entry.cloud_only() {
                skip(ONLINE_ONLY, &entry.path);
                continue;
            }
            let pending = shared.pending(entry);
            for count in &mut counts {
                if let Some(record) = entry.records.get(&count.name)
                    && record.size == entry.size
                    && record.mtime == entry.mtime
                {
                    if record.failed.is_some() { count.failed += 1 } else { count.done += 1 }
                }
            }
            if !pending.is_empty() {
                progress.queued += 1;
            } else if let Some(failed) = entry.records.values().find_map(|r| r.failed.as_deref()) {
                skip(failed, &entry.path);
            } else {
                progress.done += 1;
            }
        }
        for path in &state.unkeyed {
            progress.total += 1;
            skip(NO_FILE_ID, path);
        }
        let mut skipped: Vec<Skipped> = skipped.into_values().collect();
        skipped.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.reason.cmp(&b.reason)));
        progress.skipped = skipped;
        progress.stages = counts;
        progress.working = state.working.iter().map(|(_, path)| path.clone()).collect();
        let busy = progress.queued > 0 || !state.working.is_empty();
        (progress.state, progress.reason) = if state.paused {
            ("paused", None)
        } else if let Some(reason) = state.waiting.clone().filter(|_| busy) {
            ("waiting", Some(reason))
        } else if busy {
            ("running", None)
        } else {
            ("idle", None)
        };
        progress
    }

    /// Puts these items (by volume and file id) at the head of the queue, if they have work
    /// left: those the Gallery is showing before they were read. Returns how many.
    pub fn hurry(&self, keys: &[(String, u64)]) -> usize {
        let mut state = self.shared.state.lock();
        let mut hurried = 0;
        for key in keys {
            let Some(entry) = state.known.get(key) else { continue };
            if entry.gone || entry.cloud_only() || self.shared.pending(entry).is_empty() {
                continue;
            }
            let queued = Queued { urgent: true, mtime: entry.mtime, id: entry.id };
            state.queued.insert(queued.id);
            state.queue.push(queued);
            hurried += 1;
        }
        drop(state);
        if hurried > 0 {
            self.shared.wake.notify_all();
        }
        hurried
    }

    /// Waits until nothing is queued or being worked on, for at most `timeout`. True if so.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        loop {
            if state.queue.is_empty() && state.working.is_empty() {
                return true;
            }
            if self.shared.settled.wait_until(&mut state, deadline).timed_out() {
                return state.queue.is_empty() && state.working.is_empty();
            }
        }
    }

    /// The item the library knows by this volume and file id, if it is in the scopes.
    pub fn item(&self, volume: &str, file_id: u64) -> Option<Item> {
        let state = self.shared.state.lock();
        let key = (volume.to_owned(), file_id);
        state.known.get(&key).filter(|entry| !entry.gone).map(|entry| entry.item(&key))
    }

    /// Every item in the scopes.
    pub fn items(&self) -> Vec<Item> {
        let state = self.shared.state.lock();
        state.known.iter().filter(|(_, entry)| !entry.gone).map(|(key, entry)| entry.item(key)).collect()
    }
}

/// Lowers the calling thread to background priority: CPU, I/O and memory.
pub fn background_priority() {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    // SAFETY: changes only this thread's own priority.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

/// One item's work, taken from the queue.
struct Job {
    item: Item,
    pending: Vec<(&'static str, u32)>,
}

fn work(shared: &Shared) {
    background_priority();
    let _com = crate::Apartment::enter();
    loop {
        let reason = (shared.conditions)();
        let job = {
            let mut state = shared.state.lock();
            if state.stop {
                return;
            }
            if state.waiting != reason {
                state.waiting = reason.clone();
                drop(state);
                shared.notify(Event::State);
                continue;
            }
            if state.paused || reason.is_some() {
                shared.wake.wait_for(&mut state, shared.recheck);
                continue;
            }
            let Some(Queued { id, .. }) = state.queue.pop() else {
                shared.settled.notify_all();
                shared.wake.wait_for(&mut state, shared.recheck);
                continue;
            };
            state.queued.remove(&id);
            let Some(key) = state.by_id.get(&id).cloned() else { continue };
            let Some(entry) = state.known.get(&key) else { continue };
            if entry.gone || entry.cloud_only() {
                continue;
            }
            let pending = shared.pending(entry);
            if pending.is_empty() {
                continue;
            }
            let item = entry.item(&key);
            state.working.push((id, item.path.clone()));
            Job { item, pending }
        };
        let id = job.item.id;
        let outcome = process(shared, job);
        let mut state = shared.state.lock();
        state.working.retain(|(working, _)| *working != id);
        if let Some(key) = state.by_id.get(&id).cloned() {
            apply(&mut state, &key, outcome);
            // Anything still left (a stage registered meanwhile) goes back in the queue.
            shared.enqueue(&mut state, &key);
        }
        drop(state);
        shared.settled.notify_all();
        shared.notify(Event::Processed { id });
    }
}

/// Checks the facts' dimensions against the decoded picture, which is turned as shown: a
/// property handler that reports the stored size of a turned photo or video has them the wrong
/// way round, and they are swapped. Returns whether anything changed.
pub fn agree(facts: &mut Facts, picture: &Bitmap) -> bool {
    let (Some(width), Some(height)) = (facts.width, facts.height) else { return false };
    let wide = |w: u32, h: u32| w > h;
    let differs = width != height && picture.width != picture.height && wide(width, height) != wide(picture.width, picture.height);
    if differs {
        facts.width = Some(height);
        facts.height = Some(width);
    }
    differs
}

/// What processing an item came to.
enum Outcome {
    /// The stages ran: each one's record, as stored. The file's content was `(size, mtime)`.
    Ran { size: u64, mtime: i64, records: Vec<(String, Record)> },
    /// The file turned out to be online only: its attributes now.
    CloudOnly(u32),
    /// The file is not there any more; the next plan will say where it went.
    Missing,
    /// Writing the records failed; the item is tried again at the next plan.
    Failed,
}

fn apply(state: &mut State, key: &(String, u64), outcome: Outcome) {
    let Some(entry) = state.known.get_mut(key) else { return };
    match outcome {
        Outcome::Ran { size, mtime, records } => {
            entry.size = size;
            entry.mtime = mtime;
            entry.records.extend(records);
        }
        Outcome::CloudOnly(attrs) => entry.attrs = attrs,
        Outcome::Missing | Outcome::Failed => {}
    }
}

fn process(shared: &Shared, job: Job) -> Outcome {
    let Job { mut item, pending } = job;
    // The directory entry says what the file is now, without opening it.
    let Some(disk) = OnDisk::read(&item.path) else { return Outcome::Missing };
    if disk.is_cloud_only() {
        return Outcome::CloudOnly(disk.attrs);
    }
    if disk.is_dir() {
        return Outcome::Missing;
    }
    // The index may lag behind the disk by a moment; the work is for the content there now.
    if (disk.size, disk.mtime) != (item.size, item.mtime) {
        item.size = disk.size;
        item.mtime = disk.mtime;
        if store::update(&shared.db, &[(item.id, item.size, item.mtime, item.path.clone())]).is_err() {
            return Outcome::Failed;
        }
    }
    let mut records: Vec<(String, Record)> = Vec::new();
    let mut record = |name: &str, version: u32, failed: Option<String>| -> bool {
        let stored = store::record(&shared.db, item.id, name, version, item.size, item.mtime, failed.as_deref()).is_ok();
        if stored {
            records.push((name.to_owned(), Record { version, size: item.size, mtime: item.mtime, failed }));
        }
        stored
    };

    // Facts first: the other stages are given them, and the picture is turned by them.
    let facts = if pending.iter().any(|(name, _)| *name == METADATA) {
        match crate::props::read(&item.path, item.kind) {
            Ok(facts) => {
                if store::save_facts(&shared.db, item.id, &facts).is_err() || !record(METADATA, METADATA_VERSION, None) {
                    return Outcome::Failed;
                }
                facts
            }
            Err(error) => {
                if !record(METADATA, METADATA_VERSION, Some(error)) {
                    return Outcome::Failed;
                }
                Facts::default()
            }
        }
    } else {
        store::facts(&shared.db, item.id).ok().flatten().unwrap_or_default()
    };

    let stages: Vec<Arc<dyn Stage>> = {
        let all = shared.stages.read();
        pending.iter().filter_map(|(name, version)| all.iter().find(|s| s.name() == *name && s.version() == *version).cloned()).collect()
    };
    let largest = stages.iter().map(|stage| stage.needs().pixels).max().unwrap_or(0);
    let picture: Option<Result<Bitmap, String>> = (largest > 0).then(|| match item.kind {
        Kind::Photo => crate::decode::image(&item.path, largest, facts.orientation.unwrap_or(1)),
        Kind::Video => crate::decode::video_frame(&item.path, 0.1, largest).map(|frame| frame.bitmap),
    });
    let mut facts = facts;
    if let Some(Ok(bitmap)) = &picture
        && agree(&mut facts, bitmap)
    {
        let _ = store::save_facts(&shared.db, item.id, &facts);
    }
    for stage in stages {
        let wants_picture = stage.needs().pixels > 0;
        let result = match (&picture, wants_picture) {
            (Some(Err(error)), true) => Err(error.clone()),
            (Some(Ok(bitmap)), true) => stage.run(&item, &Input { facts: &facts, picture: Some(bitmap) }),
            _ => stage.run(&item, &Input { facts: &facts, picture: None }),
        };
        if !record(stage.name(), stage.version(), result.err()) {
            return Outcome::Failed;
        }
    }
    Outcome::Ran { size: item.size, mtime: item.mtime, records }
}
