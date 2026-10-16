//! AI search: photos and videos found by describing them ("a dog on a sofa", "receipt"), with
//! Google's SigLIP 2 models running on the GPU through Windows ML (`kestrel-embed`), their
//! vectors in a memory-mapped store per model (`kestrel-vector`).
//!
//! - `models.rs`: the AI search page's catalogue, downloads (only when the owner clicks, with
//!   progress, resume, cancel and SHA-256 checks), deletion and the self-test.
//! - `stage.rs`: the embedding stage, one stage of the media library's pass (photos and video
//!   frames to vectors). The library chooses the files, schedules them, pauses (one Pause for
//!   all of it) and decodes each photo once for every stage; its folders are AI search's.
//! - `search.rs`: resolves `like:` clauses (and `~` queries) for the engine, as tags are: the
//!   description (or a picture, for "Find similar") against the store, with the text the
//!   library read from photos (`media_text`) and their names, ranked and cut off where the
//!   owner's slider says.
//!
//! - `similar.rs`: similar photos (near duplicates) by their vectors, for the disk usage hub's
//!   Duplicates tab.
//!
//! The model in use loads on a background thread when Kestrel starts (when one is installed),
//! or when a search first needs it, and stays loaded; while it is loaded the stage is ready and
//! the library describes what it holds. Everything the page shows comes from `semantic_state`;
//! `semantic-state` events say when to ask again.

pub mod models;
pub mod search;
pub mod similar;
pub mod stage;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_db::semantic as data;
use kestrel_embed::catalog::{self, ModelSpec};
use kestrel_embed::winml::Want;
use kestrel_embed::{DeviceInfo, Embedder, Siglip};
use kestrel_vector::Store;
use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::media::Media;
use stage::DescribeStage;

/// Settings, in the database's settings table.
const ACTIVE: &str = "semantic.active";
const CUTOFF: &str = "semantic.cutoff";
/// Compact the store when this share of its rows are tombstones.
const COMPACT_SHARE: f32 = 0.25;

/// How long a search waits for the model to load.
const LOAD_WAIT: Duration = Duration::from_secs(120);

/// Where downloaded models live: `<data>\models\<id>`, each file at its path in the repository.
pub fn models_dir() -> PathBuf {
    crate::indexer::data_dir().join("models")
}

/// Where each model's vectors live: `<data>\semantic\<id>`.
pub fn vectors_dir() -> PathBuf {
    crate::indexer::data_dir().join("semantic")
}

/// A model in use: its towers on their device, and its vectors.
pub struct Loaded {
    pub model: String,
    pub spec: ModelSpec,
    pub embedder: Siglip,
    /// The vision tower's picture side.
    pub side: u32,
    pub store: RwLock<Store>,
}

enum Runtime {
    Off,
    Loading(String),
    Ready(Arc<Loaded>),
    Failed(String, String),
}

/// What the background describing is doing, for the page: the media library's pass, as far as
/// its embedding stage goes.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Indexing {
    /// `off` (no model), `loading`, `working`, `waiting` (on a full-screen app or battery
    /// saver), `done`, `paused` or `failed`.
    pub state: &'static str,
    /// Photos and videos described so far with the model in use.
    pub files: usize,
    /// Photos and videos in the library's folders.
    pub media: u64,
    /// Waiting for the pass (some stage, this one included, has work left on them).
    pub queued: u64,
    /// Described with the model in use, as the pass recorded it.
    pub done: u64,
    /// Files the model couldn't read.
    pub unreadable: u64,
    /// What the pass is reading now.
    pub current: Option<String>,
    /// Why it stopped, or why it waits.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeView {
    /// `off`, `loading`, `ready` or `failed`.
    pub state: &'static str,
    pub model: Option<String>,
    pub device: Option<DeviceInfo>,
    pub error: Option<String>,
}

pub struct Semantic {
    db: Arc<Db>,
    /// The library whose pass describes: its folders, its pause and its progress are AI search's.
    media: Arc<Media>,
    /// The embedding stage, registered with the library's pass.
    stage: Arc<DescribeStage>,
    app: Mutex<Option<AppHandle>>,
    runtime: Mutex<Runtime>,
    runtime_changed: Condvar,
    /// Bumped whenever the model in use changes: the loading thread looks again at once.
    epoch: AtomicU64,
    wake: (Mutex<bool>, Condvar),
    downloads: Mutex<HashMap<String, Arc<models::Job>>>,
    /// Development builds: where a model's files come from instead, and what they are.
    overrides: Mutex<HashMap<String, models::Override>>,
    /// The moment in each video that matched, by result set and path, for the preview.
    moments: Mutex<VecDeque<(u32, HashMap<String, u32>)>>,
    /// Downloads started since Kestrel started (the end-to-end suite checks none starts by
    /// itself).
    downloads_started: AtomicUsize,
    last_status: Mutex<Option<Instant>>,
}

impl Semantic {
    /// AI search, with its embedding stage registered in the media library's pass (which only
    /// runs it while a model is loaded), and vectors of files the library let go of dropped
    /// after each of its plans.
    pub fn new(db: Arc<Db>, media: Arc<Media>) -> Arc<Self> {
        let semantic = Arc::new_cyclic(|me| Self {
            db,
            media: media.clone(),
            stage: Arc::new(DescribeStage::new(me.clone())),
            app: Mutex::new(None),
            runtime: Mutex::new(Runtime::Off),
            runtime_changed: Condvar::new(),
            epoch: AtomicU64::new(0),
            wake: (Mutex::new(false), Condvar::new()),
            downloads: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
            moments: Mutex::new(VecDeque::new()),
            downloads_started: AtomicUsize::new(0),
            last_status: Mutex::new(None),
        });
        media.pipeline().register(semantic.stage.clone());
        let tidying = Arc::downgrade(&semantic);
        media.after_plan(move |pipeline| {
            if let Some(semantic) = tidying.upgrade() {
                semantic.tidy(pipeline);
            }
        });
        semantic
    }

    /// Checks the installed models are still all there, and loads the model in use in the
    /// background, after which the library's pass describes what it holds.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        *self.app.lock() = Some(app);
        self.check_installs();
        let me = self.clone();
        let _ = std::thread::Builder::new().name("kestrel-semantic".into()).spawn(move || me.keep_ready());
    }

    /// The loading thread: keeps the model in use loaded, and the stage ready while it is.
    fn keep_ready(&self) {
        // Give the window a moment to open before the model loads.
        self.wait(Duration::from_secs(3));
        loop {
            let epoch = self.epoch();
            match self.active() {
                None => self.use_model(None),
                Some(model) => {
                    let failed = matches!(&*self.runtime.lock(), Runtime::Failed(failed, _) if *failed == model);
                    if !failed {
                        // Loading reads a gigabyte or two, at normal priority: a search may be
                        // waiting for it.
                        match self.ready() {
                            Ok(loaded) => self.use_model(Some(&loaded)),
                            Err(_) => self.use_model(None),
                        }
                    }
                }
            }
            if let Some(loaded) = self.current() {
                flush(&loaded);
            }
            if self.epoch() == epoch {
                self.wait(Duration::from_secs(30));
            }
        }
    }

    /// Readies the stage for `loaded` (or for no model), and has the library queue its work
    /// again when that changed anything.
    fn use_model(&self, loaded: Option<&Loaded>) {
        if self.stage.set_model(loaded) {
            // The model's store may lack vectors the pass recorded as made: it was deleted with
            // the model and downloaded again, or reset after a crash.
            if let Some(loaded) = loaded {
                stage::reconcile(&loaded.store, self.media.pipeline());
            }
            self.media.pipeline().requeue();
            self.status_changed(true);
        }
    }

    /// After the library's plan: vectors of files it no longer holds go (and those of files
    /// that came back are made again), the store is compacted once enough of it is tombstones,
    /// and written to disk. The store is held only for moments: searches and the describing
    /// wait on it.
    fn tidy(&self, pipeline: &kestrel_media::pipeline::Pipeline) {
        let Some(loaded) = self.current() else { return };
        let held: std::collections::HashSet<(u32, u64)> =
            pipeline.items().iter().filter_map(|item| Some((stage::volume_code(&item.path)?, item.file_id))).collect();
        stage::sweep(&loaded.store, &held);
        stage::reconcile(&loaded.store, pipeline);
        compact(&loaded.store);
        flush(&loaded);
    }

    /// Forgets installs whose files went or changed size (a full SHA-256 check happened when
    /// they were downloaded; reading gigabytes at every start would be slow).
    fn check_installs(&self) {
        for install in data::installs(&self.db).unwrap_or_default() {
            let dir = models_dir().join(&install.model);
            let intact = install
                .files
                .iter()
                .all(|file| std::fs::metadata(dir.join(&file.path)).is_ok_and(|m| m.len() == file.size));
            if !intact {
                let _ = data::forget_install(&self.db, &install.model);
            }
        }
    }

    /// The installed models, oldest install first.
    pub fn installed(&self) -> Vec<String> {
        data::installs(&self.db).unwrap_or_default().into_iter().map(|install| install.model).collect()
    }

    /// The model searches and describing use: the owner's choice, else the newest installed.
    pub fn active(&self) -> Option<String> {
        let installed = self.installed();
        let chosen: Option<String> = self.db.setting(ACTIVE).ok().flatten();
        chosen.filter(|model| installed.contains(model)).or_else(|| installed.last().cloned())
    }

    fn set_active(&self, model: &str) -> Result<(), String> {
        if !self.installed().iter().any(|m| m == model) {
            return Err(format!("The {model} model is not downloaded"));
        }
        self.db.set_setting(ACTIVE, &model)?;
        self.bump();
        Ok(())
    }

    /// A model's catalogue entry, with a development override's files when one is set.
    pub fn spec(&self, model: &str) -> Option<ModelSpec> {
        let mut spec = catalog::model(model)?;
        if let Some(over) = self.overrides.lock().get(model) {
            over.apply(&mut spec);
        }
        Some(spec)
    }

    /// The model in use, loading it if need be (on this thread) or waiting while another
    /// thread loads it.
    pub fn ready(&self) -> Result<Arc<Loaded>, String> {
        let Some(model) = self.active() else {
            return Err("Describing needs a model. Download one on the AI search page (type \"AI search\" in the address bar).".into());
        };
        let deadline = Instant::now() + LOAD_WAIT;
        let mut runtime = self.runtime.lock();
        loop {
            let loading = match &*runtime {
                Runtime::Ready(loaded) if loaded.model == model => return Ok(loaded.clone()),
                Runtime::Failed(failed, error) if *failed == model => return Err(format!("AI search couldn't start: {error}")),
                Runtime::Loading(loading) => *loading == model,
                _ => false,
            };
            if !loading {
                break;
            }
            if self.runtime_changed.wait_until(&mut runtime, deadline).timed_out() {
                return Err("AI search is still getting ready; try again in a moment.".into());
            }
        }
        *runtime = Runtime::Loading(model.clone());
        drop(runtime);
        self.status_changed(true);
        let loaded = self.load(&model);
        let mut runtime = self.runtime.lock();
        let result = match loaded {
            // Deleted, or another chosen, while it loaded: it isn't kept.
            Ok(_) if self.active().as_deref() != Some(model.as_str()) => {
                *runtime = Runtime::Off;
                Err("The model changed while it loaded; try again.".to_owned())
            }
            Ok(loaded) => {
                let loaded = Arc::new(loaded);
                *runtime = Runtime::Ready(loaded.clone());
                Ok(loaded)
            }
            Err(error) => {
                *runtime = Runtime::Failed(model, error.clone());
                Err(format!("AI search couldn't start: {error}"))
            }
        };
        drop(runtime);
        self.runtime_changed.notify_all();
        self.bump();
        result
    }

    fn load(&self, model: &str) -> Result<Loaded, String> {
        let spec = self.spec(model).ok_or_else(|| format!("No model is called {model}"))?;
        let install = data::install(&self.db, model)?.ok_or_else(|| format!("The {model} model is not downloaded"))?;
        let embedder = Siglip::open(&models_dir().join(model), &spec, Want::Gpu).map_err(|error| error.0)?;
        // Vectors are tagged with the vision tower's checksum: another download of different
        // files never mixes with them.
        let vision = install.files.iter().find(|f| f.path.contains("vision")).map_or("", |f| f.sha256.as_str());
        let tag = format!("{model}-{}", &vision[..vision.len().min(16)]);
        let store = Store::open_or_reset(&vectors_dir().join(model), embedder.dim(), &tag).map_err(|e| e.to_string())?;
        Ok(Loaded { model: model.to_owned(), spec, side: embedder.side(), embedder, store: RwLock::new(store) })
    }

    /// The model in use, if it is loaded; never waits.
    pub fn current(&self) -> Option<Arc<Loaded>> {
        match &*self.runtime.lock() {
            Runtime::Ready(loaded) => Some(loaded.clone()),
            _ => None,
        }
    }

    /// Lets go of the loaded model; it loads again when next needed. Returns it, so a caller
    /// about to delete its files can wait until nothing else holds it.
    fn unload(&self) -> Option<Arc<Loaded>> {
        // The stage stops first, so no run in progress records its failure.
        self.stage.set_model(None);
        let previous = std::mem::replace(&mut *self.runtime.lock(), Runtime::Off);
        self.runtime_changed.notify_all();
        self.bump();
        match previous {
            Runtime::Ready(loaded) => Some(loaded),
            _ => None,
        }
    }

    /// Lets go of `model` if it is loaded or loading, and waits up to `patience` until nothing
    /// holds it any more.
    fn release(&self, model: &str, patience: Duration) {
        self.stage.set_model(None);
        let deadline = Instant::now() + patience;
        let mut runtime = self.runtime.lock();
        while matches!(&*runtime, Runtime::Loading(loading) if loading == model) {
            if self.runtime_changed.wait_until(&mut runtime, deadline).timed_out() {
                return;
            }
        }
        let held = match &*runtime {
            Runtime::Ready(loaded) if loaded.model == model => Some(loaded.clone()),
            _ => None,
        };
        if held.is_some() || matches!(&*runtime, Runtime::Failed(failed, _) if failed == model) {
            *runtime = Runtime::Off;
        }
        drop(runtime);
        self.runtime_changed.notify_all();
        if let Some(loaded) = held {
            while Arc::strong_count(&loaded) > 1 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn runtime_view(&self) -> RuntimeView {
        match &*self.runtime.lock() {
            Runtime::Off => RuntimeView { state: "off", model: None, device: None, error: None },
            Runtime::Loading(model) => RuntimeView { state: "loading", model: Some(model.clone()), device: None, error: None },
            Runtime::Ready(loaded) => {
                RuntimeView { state: "ready", model: Some(loaded.model.clone()), device: Some(loaded.embedder.device()), error: None }
            }
            Runtime::Failed(model, error) => {
                RuntimeView { state: "failed", model: Some(model.clone()), device: None, error: Some(error.clone()) }
            }
        }
    }

    /// The media library's pause, which holds the describing with the rest of its pass.
    pub fn paused(&self) -> bool {
        self.media.pipeline().is_paused()
    }

    /// The owner's relevance slider, from 0 (fewer, only close matches) to 1 (more).
    pub fn cutoff(&self) -> f32 {
        self.db.setting::<f32>(CUTOFF).ok().flatten().unwrap_or(0.5).clamp(0.0, 1.0)
    }

    /// The folders described: the media library's.
    pub fn scopes(&self) -> Vec<String> {
        self.media.scopes()
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// The model in use may have changed: the loading thread looks again at once.
    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.wake_loader();
    }

    fn wake_loader(&self) {
        *self.wake.0.lock() = true;
        self.wake.1.notify_all();
    }

    /// Waits until woken or `timeout` passes.
    fn wait(&self, timeout: Duration) {
        let mut woken = self.wake.0.lock();
        if !*woken {
            self.wake.1.wait_for(&mut woken, timeout);
        }
        *woken = false;
    }

    /// Tells the windows the state changed: at once when `now`, else at most every 400 ms.
    pub(crate) fn status_changed(&self, now: bool) {
        {
            let mut last = self.last_status.lock();
            if !now && last.is_some_and(|at| at.elapsed() < Duration::from_millis(400)) {
                return;
            }
            *last = Some(Instant::now());
        }
        if let Some(app) = self.app.lock().as_ref() {
            let _ = app.emit("semantic-state", ());
        }
    }

    /// Remembers where in each video a search's hits matched, for the preview.
    pub fn remember_moments(&self, set: u32, moments: HashMap<String, u32>) {
        let mut all = self.moments.lock();
        all.retain(|(kept, _)| *kept != set);
        all.push_back((set, moments));
        while all.len() > 32 {
            all.pop_front();
        }
    }

    fn state(&self) -> StateView {
        let active = self.active();
        let installs = data::installs(&self.db).unwrap_or_default();
        let downloads = self.downloads.lock().clone();
        let models = catalog::models()
            .into_iter()
            .map(|spec| {
                let spec = self.spec(spec.id).unwrap_or(spec);
                let install = installs.iter().find(|install| install.model == spec.id);
                models::ModelView::new(&spec, install, active.as_deref() == Some(spec.id), downloads.get(spec.id).map(|job| job.progress()))
            })
            .collect();
        let indexing = self.indexing(active.is_some());
        StateView {
            models,
            active,
            runtime: self.runtime_view(),
            indexing,
            paused: self.paused(),
            cutoff: self.cutoff(),
            default_scopes: self.media.default_scopes(),
            scopes: self.scopes(),
            local_only: models::policy() == kestrel_embed::download::Policy::LoopbackOnly,
        }
    }
}

impl Semantic {
    /// Where the describing stands, from the library's progress and the stage's records.
    fn indexing(&self, has_model: bool) -> Indexing {
        if !has_model {
            return Indexing { state: "off", ..Indexing::default() };
        }
        let progress = self.media.progress();
        let mut indexing = Indexing { media: progress.total, queued: progress.queued, ..Indexing::default() };
        if let Some(counts) = progress.stages.iter().find(|counts| counts.name == stage::STAGE) {
            (indexing.done, indexing.unreadable) = (counts.done, counts.failed);
        }
        let loaded = self.current();
        if let Some(loaded) = &loaded {
            indexing.files = loaded.store.read().live_files();
        }
        indexing.state = match (&*self.runtime.lock(), progress.state) {
            (Runtime::Failed(_, error), _) => {
                indexing.error = Some(error.clone());
                "failed"
            }
            (_, "paused") => "paused",
            (Runtime::Ready(_), "running") => "working",
            (Runtime::Ready(_), "waiting") => {
                indexing.error = progress.reason.clone();
                "waiting"
            }
            (Runtime::Ready(_), _) => "done",
            _ => "loading",
        };
        if indexing.state == "working" {
            indexing.current = progress.working.first().cloned();
        }
        indexing
    }
}

/// Compacts the store once enough of it is tombstones, holding it only to begin (read) and to
/// finish (write): the kept vectors are copied to a new file with no lock held, so searches and
/// the describing go on. That copy runs at normal priority: it reads the vectors file, which
/// the describing appends to with the store held, and a read of it at background priority could
/// keep that append (and so every search) waiting on a busy disk.
fn compact(store: &RwLock<Store>) {
    let begun = {
        let store = store.read();
        if store.dead_share() > COMPACT_SHARE && store.len() > 64 { store.begin_compact() } else { Ok(None) }
    };
    if let Ok(Some(mut compaction)) = begun
        && compaction.write().is_ok()
    {
        let _ = store.write().finish_compact(compaction);
    }
}

/// Writes the store to disk without holding it.
fn flush(loaded: &Loaded) {
    let flusher = loaded.store.read().flusher();
    if let Ok(flusher) = flusher {
        let _ = flusher.run();
    }
}

/// Everything the AI search page shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    models: Vec<models::ModelView>,
    active: Option<String>,
    runtime: RuntimeView,
    indexing: Indexing,
    paused: bool,
    cutoff: f32,
    scopes: Vec<String>,
    /// The scopes are the defaults, not the owner's choice.
    default_scopes: bool,
    /// This copy downloads only from this computer (a test instance).
    local_only: bool,
}

pub type Shared<'a> = State<'a, Arc<Semantic>>;

#[tauri::command(async)]
pub fn semantic_state(semantic: Shared) -> StateView {
    semantic.state()
}

/// Uses `model` for searches and describing from now on.
#[tauri::command(async)]
pub fn semantic_set_active(semantic: Shared, model: String) -> Result<(), String> {
    semantic.set_active(&model)?;
    semantic.unload();
    semantic.status_changed(true);
    Ok(())
}

/// Pauses or resumes describing photos and videos in the background: the media library's
/// pause, which holds the rest of its pass too.
#[tauri::command(async)]
pub fn semantic_set_paused(semantic: Shared, paused: bool) -> Result<(), String> {
    semantic.media.set_paused(paused)?;
    semantic.status_changed(true);
    Ok(())
}

/// The relevance slider: 0 shows fewer results (only close matches), 1 more.
#[tauri::command(async)]
pub fn semantic_set_cutoff(semantic: Shared, value: f32) -> Result<(), String> {
    if !value.is_finite() {
        return Err("Not a number".into());
    }
    semantic.db.set_setting(CUTOFF, &value.clamp(0.0, 1.0))?;
    Ok(())
}

/// The folders to describe, which are the media library's; `None` goes back to its defaults.
#[tauri::command(async)]
pub fn semantic_set_scopes(semantic: Shared, scopes: Option<Vec<String>>) -> Result<(), String> {
    semantic.set_scopes(scopes)
}

impl Semantic {
    fn set_scopes(&self, scopes: Option<Vec<String>>) -> Result<(), String> {
        self.media.set_scopes(scopes)?;
        self.status_changed(true);
        Ok(())
    }
}

/// Where in each video of a result set the description matched, in milliseconds, by path.
#[tauri::command(async)]
pub fn semantic_moments(semantic: Shared, set: u32) -> HashMap<String, u32> {
    semantic.moments.lock().iter().find(|(kept, _)| *kept == set).map(|(_, moments)| moments.clone()).unwrap_or_default()
}
