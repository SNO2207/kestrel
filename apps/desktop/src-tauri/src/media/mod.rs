//! The media library in the app (`kestrel-media` does the reading): the background pass over the
//! photos and videos in the owner's media scopes, kept current from the index, the Gallery's
//! data (`gallery.rs`), the details view's columns and the details pane's text, and the commands
//! of the library's settings page.
//!
//! A thread (`watch`) waits for the index, then a little longer so starting Kestrel is never
//! slowed by it, starts the pipeline's workers, and from then on scans the index for media under
//! the scopes whenever the index has changed (at most every `MIN_RESCAN`) or something asks
//! (`kick`: the scopes changed, a test), handing what it finds to `Pipeline::plan`. Progress
//! goes to the windows as `media-progress`, and `media-changed` says the Gallery has something
//! new to show. It runs at normal priority: it holds the index (a step at a time), the
//! database and, through the features' tidying, their stores, which every search waits on; a
//! thread in background mode holding one starves with it while the processors are busy
//! (`kestrel_media::priority`). The heavy work of the pass is the workers', in background mode.
//!
//! Scopes: the owner's own list (setting `media.scopes`), or else the defaults
//! (`kestrel_media::scopes::defaults`). `KESTREL_MEDIA_SCOPES=none` (set by `e2e/dev.mjs` for test
//! slots) leaves the defaults empty, so a test instance never reads the owner's photos.
//! Development builds can replace every scope with a scratch folder (`media_dev_sandbox`).
//!
//! The pass is the one background reader of photos and videos: AI search's embedding stage is
//! one of its stages (`semantic::stage`), so one decode feeds the facts, the thumbnail, the
//! text and the description, one pause button stops all of it, and one progress counts it.
//! Features that keep data of their own about the library's items tidy it after each plan
//! (`after_plan`).

pub mod gallery;
mod thumbstage;

use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_media::pipeline::{Candidate, Config, Event, Pipeline, Progress};
use kestrel_media::scan::Scanner;
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::engine::Engine;
pub use thumbstage::GALLERY_THUMB;

const SCOPES_SETTING: &str = "media.scopes";
const PAUSED_SETTING: &str = "media.paused";
/// How long after the index is ready the pass begins.
const START_DELAY: Duration = Duration::from_secs(20);
/// The least time between two scans the index's changes ask for. A busy disk changes the index
/// every few hundred milliseconds; the media in it changes far less often.
const MIN_RESCAN: Duration = Duration::from_secs(10);
/// How often the windows hear of progress while the pass runs, at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(400);
/// How often the Gallery is told it has something new to show, at most.
const CHANGED_EVERY: Duration = Duration::from_secs(2);
/// Entries looked at per step of a scan, between which the index is let go.
const SCAN_STEP: u32 = 1 << 18;

pub type Shared<'a> = State<'a, Arc<Media>>;

/// Run after each plan with the pipeline, whose items are then what is in the scopes.
type AfterPlan = Arc<dyn Fn(&Pipeline) + Send + Sync>;

#[derive(Default)]
struct Inner {
    app: Option<AppHandle>,
    /// The owner's own scopes; `None` means the defaults.
    custom: Option<Vec<String>>,
    defaults: Option<Vec<String>>,
    /// Development builds: scopes that stand in for all others, for the end-to-end suite.
    sandbox: Option<Vec<String>>,
    /// Plans asked for, and the last one done: a plan is due while they differ.
    kicks: u64,
    /// The watching thread ends (tests).
    stopping: bool,
    planned: u64,
    last_revision: Option<u64>,
    last_scan: Option<Instant>,
    started: bool,
    progress_due: bool,
    changed_due: bool,
    last_progress: Option<Instant>,
    last_changed: Option<Instant>,
}

pub struct Media {
    engine: Arc<Engine>,
    db: Arc<Db>,
    pipeline: Pipeline,
    inner: Mutex<Inner>,
    wake: Condvar,
    after_plan: Mutex<Vec<AfterPlan>>,
}

impl Media {
    pub fn new(engine: Arc<Engine>, db: Arc<Db>, thumbs: crate::thumbs::ThumbStore) -> Arc<Self> {
        let pipeline = Pipeline::new(db.clone(), Config::default());
        pipeline.register(Arc::new(thumbstage::ThumbnailStage::new(thumbs)));
        pipeline.register(Arc::new(kestrel_media::ocr::TextStage::new(db.clone())));
        pipeline.set_paused(db.setting::<bool>(PAUSED_SETTING).ok().flatten().unwrap_or(false));
        let custom = db.setting::<Vec<String>>(SCOPES_SETTING).ok().flatten();
        Arc::new(Media {
            engine,
            db,
            pipeline,
            inner: Mutex::new(Inner { custom, ..Inner::default() }),
            wake: Condvar::new(),
            after_plan: Mutex::new(Vec::new()),
        })
    }

    /// The pipeline, for features that add a stage of their own (`Pipeline::register`), such as
    /// the semantic search's embedding stage; see CLAUDE.md.
    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// Runs `tidy` after every plan, on the library's own thread: a feature that keeps data about
    /// the library's items drops what belongs to files no longer among them.
    pub fn after_plan(&self, tidy: impl Fn(&Pipeline) + Send + Sync + 'static) {
        self.after_plan.lock().push(Arc::new(tidy));
    }

    /// Pauses or resumes the pass, and remembers it for the next start. The one pause of every
    /// stage, AI search's included.
    pub fn set_paused(&self, paused: bool) -> Result<Progress, String> {
        self.db.set_setting(PAUSED_SETTING, &paused)?;
        self.pipeline.set_paused(paused);
        let progress = self.progress();
        self.emit("media-progress", progress.clone());
        Ok(progress)
    }

    /// Sets the scopes (folders that exist, the outermost of nested ones), or with `None` goes
    /// back to the defaults; the library follows at once.
    pub fn set_scopes(&self, scopes: Option<Vec<String>>) -> Result<(), String> {
        let scopes = scopes.map(|scopes| {
            let folders = scopes.into_iter().map(|path| crate::engine::normalize(&path)).filter(|path| std::path::Path::new(path).is_dir()).collect();
            kestrel_media::scopes::outermost(folders)
        });
        match &scopes {
            Some(scopes) => self.db.set_setting(SCOPES_SETTING, scopes)?,
            None => {
                self.db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", [SCOPES_SETTING]))?;
            }
        }
        self.inner.lock().custom = scopes;
        self.kick();
        Ok(())
    }

    /// Whether the scopes are the defaults (neither the owner's own list nor a test's sandbox).
    pub fn default_scopes(&self) -> bool {
        let inner = self.inner.lock();
        inner.custom.is_none() && inner.sandbox.is_none()
    }

    /// Starts following the index, and the pass once Kestrel has settled.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        self.inner.lock().app = Some(app);
        let listening = Arc::downgrade(self);
        self.pipeline.set_listener(move |event| {
            let Some(media) = listening.upgrade() else { return };
            let mut inner = media.inner.lock();
            inner.progress_due = true;
            if matches!(event, Event::Planned { .. } | Event::Processed { .. }) {
                inner.changed_due = true;
            }
            let urgent = matches!(event, Event::State);
            drop(inner);
            if urgent {
                media.wake.notify_all();
            }
        });
        let me = self.clone();
        let _ = std::thread::Builder::new().name("kestrel-media-watch".into()).spawn(move || me.watch());
    }

    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
        let app = self.inner.lock().app.clone();
        if let Some(app) = app {
            let _ = app.emit(event, payload);
        }
    }

    /// The scopes in force: the sandbox's, the owner's, or the defaults.
    pub fn scopes(&self) -> Vec<String> {
        let mut inner = self.inner.lock();
        if let Some(sandbox) = &inner.sandbox {
            return sandbox.clone();
        }
        if let Some(custom) = &inner.custom {
            return custom.clone();
        }
        inner.defaults.get_or_insert_with(default_scopes).clone()
    }

    /// Whether the library reads the folder at `path`: it is one of the scopes or beneath one.
    /// Searches limited to a folder it doesn't read can't ask it about `taken:`, so they read
    /// the photos instead (search/).
    pub fn covers(&self, path: &str) -> bool {
        let path = kestrel_index::fold(path.trim_end_matches('\\'));
        self.scopes().iter().any(|scope| {
            let scope = kestrel_index::fold(scope.trim_end_matches('\\'));
            path == scope || path.strip_prefix(scope.as_str()).is_some_and(|rest| rest.starts_with('\\'))
        })
    }

    fn kick(&self) {
        self.inner.lock().kicks += 1;
        self.wake.notify_all();
    }

    fn watch(self: Arc<Self>) {
        let _com = kestrel_media::Apartment::enter();
        let born = Instant::now();
        loop {
            {
                let mut inner = self.inner.lock();
                if inner.stopping {
                    return;
                }
                if inner.kicks == inner.planned {
                    self.wake.wait_for(&mut inner, Duration::from_millis(250));
                }
            }
            self.send_progress();
            if self.engine.status().state != "ready" {
                continue;
            }
            let (due, kicks) = {
                let mut inner = self.inner.lock();
                let asked = inner.kicks != inner.planned;
                if !inner.started {
                    // Starting Kestrel comes first: the pass waits unless asked for.
                    if born.elapsed() < START_DELAY && !asked && inner.sandbox.is_none() {
                        continue;
                    }
                    inner.started = true;
                    self.pipeline.start();
                }
                let revision = self.engine.revision();
                let changed = inner.last_revision != Some(revision) && inner.last_scan.is_none_or(|at| at.elapsed() >= MIN_RESCAN);
                if asked || changed {
                    inner.last_revision = Some(revision);
                }
                (asked || changed, inner.kicks)
            };
            if !due {
                continue;
            }
            let scopes = self.scopes();
            let candidates = scan(&self.engine, &scopes);
            match self.pipeline.plan(candidates) {
                Ok(_) => {
                    let tidies: Vec<AfterPlan> = self.after_plan.lock().clone();
                    for tidy in tidies {
                        tidy(&self.pipeline);
                    }
                }
                Err(error) => eprintln!("The media library couldn't be brought up to date: {error}"),
            }
            let mut inner = self.inner.lock();
            inner.planned = kicks;
            inner.last_scan = Some(Instant::now());
            inner.progress_due = true;
            drop(inner);
            self.wake.notify_all();
        }
    }

    /// Sends what is due: progress at most every `PROGRESS_EVERY`, and `media-changed` at most
    /// every `CHANGED_EVERY`.
    fn send_progress(&self) {
        let (progress, changed) = {
            let mut inner = self.inner.lock();
            let progress = inner.progress_due && inner.last_progress.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY);
            let changed = inner.changed_due && inner.last_changed.is_none_or(|at| at.elapsed() >= CHANGED_EVERY);
            if progress {
                inner.progress_due = false;
                inner.last_progress = Some(Instant::now());
            }
            if changed {
                inner.changed_due = false;
                inner.last_changed = Some(Instant::now());
            }
            (progress, changed)
        };
        if progress {
            self.emit("media-progress", self.progress());
        }
        if changed {
            self.emit("media-changed", ());
        }
    }

    pub fn progress(&self) -> Progress {
        let mut progress = self.pipeline.progress();
        if !self.inner.lock().started && progress.state == "idle" {
            progress.state = "starting";
        }
        progress
    }

    /// Waits until a plan asked for after this call has been made, then until the pass is idle
    /// or paused, for at most `timeout`.
    fn settle(&self, timeout: Duration) -> Progress {
        let deadline = Instant::now() + timeout;
        let wanted = {
            let mut inner = self.inner.lock();
            inner.kicks += 1;
            inner.kicks
        };
        self.wake.notify_all();
        {
            let mut inner = self.inner.lock();
            while inner.planned < wanted {
                if self.wake.wait_until(&mut inner, deadline).timed_out() {
                    break;
                }
            }
        }
        if !self.pipeline.is_paused() {
            self.pipeline.wait_idle(deadline.saturating_duration_since(Instant::now()));
        }
        self.progress()
    }
}

/// The default scopes, unless this instance was told to have none.
fn default_scopes() -> Vec<String> {
    if std::env::var("KESTREL_MEDIA_SCOPES").is_ok_and(|value| value.eq_ignore_ascii_case("none")) {
        return Vec::new();
    }
    let _com = kestrel_media::Apartment::enter();
    kestrel_media::scopes::defaults()
}

/// The photos and videos under `scopes`, from the index, letting go of it between steps. Kestrel's
/// own data folder is left out, whatever the scopes: its thumbnail cache is full of pictures.
pub fn scan(engine: &Engine, scopes: &[String]) -> Vec<Candidate> {
    let mut found = engine.in_steps(|index| Scanner::new(index, scopes), |scanner, index| scanner.step(index, SCAN_STEP), |scanner, _| scanner.finish());
    let own = crate::indexer::data_dir().to_string_lossy().into_owned();
    found.retain(|candidate| !kestrel_media::scopes::is_within(&candidate.path, &own));
    found
}

// ---------- commands ----------

#[derive(Clone, Serialize)]
pub struct Scope {
    path: String,
    exists: bool,
}

/// What the library's settings page shows.
#[derive(Clone, Serialize)]
pub struct Status {
    progress: Progress,
    scopes: Vec<Scope>,
    /// The owner chose the scopes; otherwise they are the defaults.
    custom: bool,
    /// Development builds: a test's scratch folder stands in for the scopes until the next start.
    sandboxed: bool,
    defaults: Vec<String>,
    /// The languages text is read in, as tags such as `en-US`.
    languages: Vec<String>,
    stages: Vec<&'static str>,
}

fn status(media: &Media) -> Status {
    let scopes = media.scopes().into_iter().map(|path| Scope { exists: std::path::Path::new(&path).is_dir(), path }).collect();
    let (custom, sandboxed) = {
        let inner = media.inner.lock();
        (inner.custom.is_some(), inner.sandbox.is_some())
    };
    let defaults = {
        let mut inner = media.inner.lock();
        inner.defaults.get_or_insert_with(default_scopes).clone()
    };
    let languages = {
        let _com = kestrel_media::Apartment::enter();
        kestrel_media::ocr::Recognizer::for_user().map(|r| r.languages().to_vec()).unwrap_or_default()
    };
    Status { progress: media.progress(), scopes, custom, sandboxed, defaults, languages, stages: media.pipeline.stage_names() }
}

#[tauri::command(async)]
pub fn media_status(media: Shared) -> Status {
    status(&media)
}

/// Pauses or resumes the pass, and remembers it for the next start.
#[tauri::command(async)]
pub fn media_set_paused(media: Shared, paused: bool) -> Result<Progress, String> {
    media.set_paused(paused)
}

/// Sets the scopes (folders that exist, the outermost of nested ones), or with `None` goes back
/// to the defaults; the library follows at once.
#[tauri::command(async)]
pub fn media_set_scopes(media: Shared, scopes: Option<Vec<String>>) -> Result<Status, String> {
    media.set_scopes(scopes)?;
    Ok(status(&media))
}

/// The library's id for a file, by its path: through the index, or failing that the file.
fn id_of(media: &Media, path: &str) -> Option<i64> {
    let volume = kestrel_media::volume_of(path)?;
    let indexed = {
        let index = media.engine.index().read();
        index.lookup(path).map(|entry| index.file_id(entry)).filter(|&id| id != 0)
    };
    let file_id = indexed.or_else(|| kestrel_index::walk::file_id_of(path))?;
    kestrel_db::media::id_of(&media.db, &volume, file_id).ok().flatten()
}

/// The text recognised in an image, for the details pane.
#[tauri::command(async)]
pub fn media_text(media: Shared, path: String) -> Result<Option<String>, String> {
    let Some(id) = id_of(&media, &path) else { return Ok(None) };
    Ok(kestrel_db::media::text(&media.db, id)?)
}

/// What the details view's media columns show of one item.
#[derive(Clone, Default, Serialize)]
pub struct ItemFacts {
    /// When it was taken, in milliseconds since the Unix epoch.
    taken: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
}

/// At most this many items' facts are given at once: the rows on screen.
const MAX_FACTS: usize = 500;

/// The facts of the photos and videos among `paths`, in order; `None` for anything else.
#[tauri::command(async)]
pub fn media_facts(media: Shared, paths: Vec<String>) -> Vec<Option<ItemFacts>> {
    paths
        .iter()
        .take(MAX_FACTS)
        .map(|path| {
            kestrel_media::kind_of_path(path)?;
            let id = id_of(&media, path)?;
            let facts = kestrel_db::media::facts(&media.db, id).ok().flatten()?;
            Some(ItemFacts {
                taken: facts.taken.map(kestrel_media::unix_ms),
                width: facts.width,
                height: facts.height,
                duration_ms: facts.duration_ms,
            })
        })
        .collect()
}

/// Most items one request may hurry: a screenful or two.
const MAX_HURRIED: usize = 400;

/// Reads these items next, if they are still waiting: the Gallery asks for those on screen.
#[tauri::command(async)]
pub fn media_hurry(media: Shared, paths: Vec<String>) -> usize {
    let keys: Vec<(String, u64)> = {
        let index = media.engine.index().read();
        paths
            .iter()
            .take(MAX_HURRIED)
            .filter_map(|path| {
                let entry = index.lookup(path)?;
                Some((kestrel_media::volume_of(path)?, index.file_id(entry)))
            })
            .collect()
    };
    media.pipeline.hurry(&keys)
}

/// The Gallery: every photo and video in the scopes, newest first (see `gallery.rs`).
#[tauri::command(async)]
pub fn gallery_open(media: Shared, filter: gallery::Filter, keep: Option<Vec<String>>) -> Result<gallery::GalleryData, String> {
    gallery::open(&media, &filter, &keep.unwrap_or_default())
}

/// Windows' folder picker, for a folder to add to the library; `None` if cancelled.
#[tauri::command(async)]
pub fn media_pick_folder(window: tauri::Window, start: String) -> Option<String> {
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    std::thread::spawn(move || pick_folder(owner, &start)).join().ok().flatten()
}

fn pick_folder(owner: Option<isize>, start: &str) -> Option<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
        SIGDN_FILESYSPATH,
    };
    use windows::core::{HSTRING, w};
    // SAFETY: COM is initialised on this thread for the dialog and uninitialised after the last
    // interface from it is dropped, at the end of the inner closure; every string passed outlives
    // its call, and the returned name is freed once copied.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
        let chosen = (|| {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let options = dialog.GetOptions().ok()?;
            dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
            let _ = dialog.SetTitle(w!("Choose a folder for the media library"));
            if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start), None) {
                let _ = dialog.SetFolder(&folder);
            }
            dialog.Show(owner.map(|hwnd| HWND(hwnd as *mut _))).ok()?;
            let item = dialog.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = name.to_string().ok();
            CoTaskMemFree(Some(name.0 as *const _));
            path
        })();
        CoUninitialize();
        chosen
    }
}

// ---------- development hooks ----------

fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

/// Development builds only: makes `folder` (or with `None`, nothing) the only scope, until the
/// next start. The pass starts at once if it had not yet.
#[tauri::command(async)]
pub fn media_dev_sandbox(media: Shared, folder: Option<String>) -> Result<(), String> {
    development_only()?;
    media.inner.lock().sandbox = folder.map(|folder| vec![crate::engine::normalize(&folder)]);
    media.kick();
    Ok(())
}

/// Development builds only: writes the end-to-end suite's photos, video and screenshot into a
/// scratch folder under the temp folder, and returns their paths.
#[tauri::command(async)]
pub fn media_dev_fixtures(folder: String) -> Result<Vec<String>, String> {
    development_only()?;
    let folder = std::path::PathBuf::from(crate::engine::normalize(&folder));
    let temp = std::env::temp_dir();
    let inside = |path: &std::path::Path, root: &std::path::Path| {
        path.to_string_lossy().to_lowercase().starts_with(&root.to_string_lossy().to_lowercase())
    };
    if !inside(&folder, &temp) || folder.components().count() <= temp.components().count() {
        return Err("Fixtures are only written to a folder inside the temp folder".into());
    }
    let _com = kestrel_media::Apartment::enter();
    gallery::fixtures(&folder)
}

/// Development builds only: asks for a plan now and waits for it, then for the pass to finish
/// (or pause), for at most `timeout_ms`; returns where it stands.
#[tauri::command(async)]
pub fn media_dev_wait(media: Shared, timeout_ms: u64) -> Result<Progress, String> {
    development_only()?;
    Ok(media.settle(Duration::from_millis(timeout_ms.min(120_000))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{SortField, SortSpec};
    use crate::icons::IconService;
    use crate::thumbs::ThumbService;
    use crate::visibility::Visibility;
    use kestrel_index::{IndexBuilder, walk};

    const BY_NAME: SortSpec = SortSpec { key: SortField::Name, desc: false };

    fn names_of(engine: &Engine, set: u32, total: u32) -> Vec<String> {
        engine.rows(set, 0, total).unwrap().into_iter().map(|row| row.name).collect()
    }

    /// The suite's fixtures, read by the whole pass, give the Gallery, the search clauses, the
    /// details view's facts and the details pane's text; the thumbnail stage fills the cache.
    #[test]
    fn the_library_reads_the_fixtures_for_the_gallery_searches_and_details() {
        let _com = kestrel_media::Apartment::enter();
        let dir = std::env::temp_dir().join(format!("kestrel-test-media-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let written = gallery::fixtures(&dir).unwrap();
        assert_eq!(written.len(), 4);
        let root = dir.to_str().unwrap().to_owned();
        let engine = Arc::new(Engine::default());
        let mut builder = IndexBuilder::new();
        walk::walk(&root, &mut builder).unwrap();
        engine.install(builder.finish());
        let db = Arc::new(Db::open_in_memory().unwrap());
        // The cache is kept outside the scope: its files are pictures too.
        let cache = std::env::temp_dir().join(format!("kestrel-test-media-app-thumbs-{}", std::process::id()));
        let thumbs = ThumbService::with(Arc::new(IconService::start()), cache.clone(), Duration::ZERO);
        let media = Media::new(engine.clone(), db.clone(), thumbs.store());
        media.inner.lock().sandbox = Some(vec![root.clone()]);
        media.pipeline.start();
        let (added, _, _) = media.pipeline.plan(scan(&engine, &media.scopes())).unwrap();
        assert_eq!(added, 4);
        assert!(media.pipeline.wait_idle(Duration::from_secs(120)));
        let progress = media.progress();
        assert_eq!((progress.total, progress.photos, progress.videos, progress.done), (4, 3, 1, 4), "{progress:?}");

        // The Gallery: newest first, with each item's shape and the video's length.
        // The page asks where items it shows are now: a path in another case is the same item.
        let beach = format!(r"{root}\trip\beach.jpg");
        let keep = [beach.to_uppercase(), format!(r"{root}\nothing.jpg")];
        let all = gallery::open(&media, &gallery::Filter::default(), &keep).unwrap();
        assert_eq!(all.total, 4);
        assert!(all.dates.windows(2).all(|pair| pair[0] >= pair[1]), "newest first");
        let names = names_of(&engine, all.set, all.total);
        let at = |name: &str| names.iter().position(|n| n == name).unwrap();
        assert!(at("receipt.png") < at("tower.jpg") && at("tower.jpg") < at("beach.jpg"), "{names:?}");
        assert!((all.ratios[at("beach.jpg")] - 1.5).abs() < 0.01);
        assert!((all.ratios[at("tower.jpg")] - 0.75).abs() < 0.01, "turned upright by its orientation");
        assert_eq!(all.videos, [(at("walk.mp4") as u32, 3)]);
        assert_eq!(all.thumb, GALLERY_THUMB);
        assert_eq!(all.kept, [at("beach.jpg") as i64, -1]);
        let photos = gallery::open(&media, &gallery::Filter { kind: Some(kestrel_media::Kind::Photo), folder: None }, &[]).unwrap();
        assert_eq!(photos.total, 3);
        let trip = format!(r"{root}\trip");
        let in_trip = gallery::open(&media, &gallery::Filter { kind: None, folder: Some(trip.clone()) }, &[]).unwrap();
        assert_eq!(names_of(&engine, in_trip.set, in_trip.total), ["tower.jpg", "beach.jpg"]);
        assert!(all.folders.iter().any(|folder| folder.path == trip && folder.count == 2));

        // The thumbnail stage filed every item's thumbnail where the Gallery asks for it.
        let store = thumbs.store();
        for row in engine.rows(all.set, 0, all.total).unwrap() {
            assert!(store.has(&row.path, GALLERY_THUMB, row.mtime), "{}", row.path);
        }

        // Searches with the media library's clauses, as the search box runs them.
        let search = |query: &str| {
            let listing = engine
                .search_resolving(query, Some(&root), BY_NAME, Visibility::ALL, |query, index| kestrel_media::query::resolve(&db, query, index))
                .unwrap();
            names_of(&engine, listing.set, listing.total)
        };
        assert_eq!(search("text:costco"), ["receipt.png"]);
        assert_eq!(search(r#"text:"order 42.17""#), ["receipt.png"]);
        assert_eq!(search("width:>1000"), ["beach.jpg", "receipt.png"]);
        assert_eq!(search("height:1200"), ["tower.jpg"]);
        assert_eq!(search("camera:pixel"), ["tower.jpg"]);
        assert_eq!(search("geo:yes"), ["beach.jpg"]);
        assert_eq!(search("taken:2024-07"), ["beach.jpg", "tower.jpg"]);
        assert_eq!(search("kind:video"), ["walk.mp4"]);
        assert_eq!(search("duration:>2s"), ["walk.mp4"]);
        assert_eq!(search("kind:photo !geo:yes"), ["receipt.png", "tower.jpg"]);

        // The details pane's text and the details view's facts, by path.
        let receipt = format!(r"{root}\screens\receipt.png");
        let id = id_of(&media, &receipt).unwrap();
        let text = kestrel_db::media::text(&db, id).unwrap().unwrap();
        assert!(text.contains("COSTCO") && text.contains("42.17"), "{text}");
        let tower = kestrel_db::media::facts(&db, id_of(&media, &format!(r"{root}\trip\tower.jpg")).unwrap()).unwrap().unwrap();
        assert_eq!((tower.width, tower.height, tower.model.as_deref()), (Some(900), Some(1200), Some("Pixel 8")));
        assert!(id_of(&media, &format!(r"{root}\nothing.jpg")).is_none());

        // Renamed: still found by its words, and nothing is read again.
        let renamed = format!(r"{root}\screens\costco.png");
        std::fs::rename(&receipt, &renamed).unwrap();
        let mut builder = IndexBuilder::new();
        walk::walk(&root, &mut builder).unwrap();
        engine.install(builder.finish());
        assert_eq!(media.pipeline.plan(scan(&engine, &media.scopes())).unwrap(), (0, 1, 0));
        assert!(media.pipeline.wait_idle(Duration::from_secs(30)));
        assert_eq!(media.progress().done, 4);
        assert_eq!(search("text:costco"), ["costco.png"]);
        media.pipeline.stop();
        drop(engine);
        std::fs::remove_dir_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// The thread that follows the index holds it (a step at a time), the database and the
    /// features' stores, which every search waits on: it plans and tidies at normal priority.
    /// In background mode, while a build kept every core busy, it once held the index's read
    /// lock for seconds, and every keystroke search with it.
    #[test]
    fn the_library_plans_and_tidies_at_normal_priority() {
        let _com = kestrel_media::Apartment::enter();
        let dir = std::env::temp_dir().join(format!("kestrel-test-media-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), kestrel_media::Bitmap::filled(16, 16, [9, 9, 9, 255]).to_png().unwrap()).unwrap();
        let root = dir.to_str().unwrap().to_owned();
        let engine = Arc::new(Engine::default());
        let mut builder = IndexBuilder::new();
        walk::walk(&root, &mut builder).unwrap();
        engine.install(builder.finish());
        engine.set_status("ready", false);
        let cache = std::env::temp_dir().join(format!("kestrel-test-media-watch-thumbs-{}", std::process::id()));
        let thumbs = ThumbService::with(Arc::new(IconService::start()), cache.clone(), Duration::ZERO);
        let media = Media::new(engine, Arc::new(Db::open_in_memory().unwrap()), thumbs.store());
        media.inner.lock().sandbox = Some(vec![root]);
        let (told, heard) = std::sync::mpsc::channel();
        media.after_plan(move |_| {
            let _ = told.send(kestrel_media::priority::in_background());
        });
        let watching = media.clone();
        let thread = std::thread::spawn(move || watching.watch());
        media.kick();
        assert_eq!(heard.recv_timeout(Duration::from_secs(30)), Ok(false), "planned and tidied at normal priority");
        media.inner.lock().stopping = true;
        media.wake.notify_all();
        thread.join().unwrap();
        media.pipeline.stop();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
