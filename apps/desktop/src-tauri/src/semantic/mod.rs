//! AI search: photos and videos found by describing them ("a dog on a sofa", "receipt"), with
//! Google's SigLIP 2 models running on the GPU through Windows ML (`kestrel-embed`), their
//! vectors in a memory-mapped store per model (`kestrel-vector`).
//!
//! - `models.rs`: the AI search page's catalogue, downloads (only when the owner clicks, with
//!   progress, resume, cancel and SHA-256 checks), deletion and the self-test.
//! - `stage.rs`: the embedding stage (photos and video frames to vectors, photos' text to the
//!   text index), shaped to be a stage of the media pipeline.
//! - `driver.rs`: stands in for that pipeline: walks the index for photos and videos in the
//!   scopes, at low priority, pausable, never opening a cloud-only file, incremental by file key.
//! - `search.rs`: resolves `like:` clauses (and `~` queries) for the engine, as tags are: the
//!   description (or a picture, for "Find similar") against the store, with the text read from
//!   photos and their names, ranked and cut off where the owner's slider says.
//!
//! The model in use loads on a background thread when Kestrel starts (when one is installed),
//! or when a search first needs it, and stays loaded. Everything the page shows comes from
//! `semantic_state`; `semantic-state` events say when to ask again.

mod driver;
pub mod models;
mod scopes;
pub mod search;
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

use crate::engine::Engine;

/// Settings, in the database's settings table.
const ACTIVE: &str = "semantic.active";
const PAUSED: &str = "semantic.paused";
const CUTOFF: &str = "semantic.cutoff";
const SCOPES: &str = "semantic.scopes";

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

/// What the background describing is doing, for the page.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Indexing {
    /// `off` (no model), `loading`, `working`, `done`, `paused` or `failed`.
    pub state: &'static str,
    /// Photos and videos described so far with the model in use.
    pub files: usize,
    /// Photos and videos in the folders described.
    pub media: usize,
    /// Left to do in this pass, and done in it.
    pub queued: usize,
    pub done: usize,
    /// Files the model couldn't read.
    pub unreadable: u64,
    /// What it is reading now.
    pub current: Option<String>,
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
    engine: Arc<Engine>,
    db: Arc<Db>,
    app: Mutex<Option<AppHandle>>,
    runtime: Mutex<Runtime>,
    runtime_changed: Condvar,
    /// Bumped whenever the model in use, the scopes or the pause change: a pass in progress
    /// stops, and the driver looks again at once.
    epoch: AtomicU64,
    wake: (Mutex<bool>, Condvar),
    indexing: Mutex<Indexing>,
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
    pub fn new(engine: Arc<Engine>, db: Arc<Db>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            db,
            app: Mutex::new(None),
            runtime: Mutex::new(Runtime::Off),
            runtime_changed: Condvar::new(),
            epoch: AtomicU64::new(0),
            wake: (Mutex::new(false), Condvar::new()),
            indexing: Mutex::new(Indexing { state: "off", ..Indexing::default() }),
            downloads: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
            moments: Mutex::new(VecDeque::new()),
            downloads_started: AtomicUsize::new(0),
            last_status: Mutex::new(None),
        })
    }

    /// Checks the installed models are still all there, and starts describing in the
    /// background (which loads the model in use first).
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        *self.app.lock() = Some(app);
        self.check_installs();
        let me = self.clone();
        let _ = std::thread::Builder::new().name("kestrel-semantic".into()).spawn(move || driver::run(me));
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
        let previous = std::mem::replace(&mut *self.runtime.lock(), Runtime::Off);
        self.runtime_changed.notify_all();
        self.bump();
        match previous {
            Runtime::Ready(loaded) => Some(loaded),
            _ => None,
        }
    }

    /// Whether no model is loaded, loading or failed: as after a delete or a switch.
    fn runtime_is_off(&self) -> bool {
        matches!(&*self.runtime.lock(), Runtime::Off)
    }

    /// Lets go of `model` if it is loaded or loading, and waits up to `patience` until nothing
    /// holds it any more.
    fn release(&self, model: &str, patience: Duration) {
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

    pub fn paused(&self) -> bool {
        self.db.setting(PAUSED).ok().flatten().unwrap_or(false)
    }

    /// The owner's relevance slider, from 0 (fewer, only close matches) to 1 (more).
    pub fn cutoff(&self) -> f32 {
        self.db.setting::<f32>(CUTOFF).ok().flatten().unwrap_or(0.5).clamp(0.0, 1.0)
    }

    /// The folders described: the owner's choice, else their picture and video folders.
    pub fn scopes(&self) -> Vec<String> {
        match self.db.setting::<Vec<String>>(SCOPES).ok().flatten() {
            Some(chosen) => chosen,
            None => scopes::defaults(),
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Something the driver depends on changed: a pass in progress stops, and the driver looks
    /// again at once.
    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.wake_driver();
    }

    fn wake_driver(&self) {
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

    fn set_indexing(&self, change: impl FnOnce(&mut Indexing)) {
        change(&mut self.indexing.lock());
        self.status_changed(false);
    }

    /// Tells the windows the state changed: at once when `now`, else at most every 400 ms.
    fn status_changed(&self, now: bool) {
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
        let mut indexing = self.indexing.lock().clone();
        if let Some(model) = &active {
            indexing.unreadable = data::skip_count(&self.db, model).unwrap_or(0);
        }
        if let Some(loaded) = self.current() {
            indexing.files = loaded.store.read().live_files();
        }
        let chosen: Option<Vec<String>> = self.db.setting(SCOPES).ok().flatten();
        StateView {
            models,
            active,
            runtime: self.runtime_view(),
            indexing,
            paused: self.paused(),
            cutoff: self.cutoff(),
            default_scopes: chosen.is_none(),
            scopes: self.scopes(),
            local_only: models::policy() == kestrel_embed::download::Policy::LoopbackOnly,
        }
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

/// Pauses or resumes describing photos and videos in the background.
#[tauri::command(async)]
pub fn semantic_set_paused(semantic: Shared, paused: bool) -> Result<(), String> {
    semantic.db.set_setting(PAUSED, &paused)?;
    semantic.bump();
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

/// The folders to describe; `None` goes back to the picture and video folders.
#[tauri::command(async)]
pub fn semantic_set_scopes(semantic: Shared, scopes: Option<Vec<String>>) -> Result<(), String> {
    semantic.set_scopes(scopes)
}

impl Semantic {
    fn set_scopes(&self, scopes: Option<Vec<String>>) -> Result<(), String> {
        match scopes {
            Some(scopes) => {
                let mut seen = std::collections::HashSet::new();
                let scopes: Vec<String> = scopes
                    .iter()
                    .map(|s| crate::engine::normalize(s))
                    .filter(|s| !s.is_empty() && seen.insert(s.to_lowercase()))
                    .collect();
                self.db.set_setting(SCOPES, &scopes)?;
            }
            None => {
                self.db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", [SCOPES]))?;
            }
        }
        self.bump();
        self.status_changed(true);
        Ok(())
    }
}

/// Where in each video of a result set the description matched, in milliseconds, by path.
#[tauri::command(async)]
pub fn semantic_moments(semantic: Shared, set: u32) -> HashMap<String, u32> {
    semantic.moments.lock().iter().find(|(kept, _)| *kept == set).map(|(_, moments)| moments.clone()).unwrap_or_default()
}
