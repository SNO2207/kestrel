//! The AI search page's models: what each is, downloading one when the owner clicks (and only
//! then), deleting one, and the self-test that runs after a download and whenever asked.
//!
//! A download fetches the model's files one after another into `<data>\models\<id>`, each to a
//! `.part` file first, resuming after an interruption and checked against its published SHA-256
//! (`kestrel_embed::download`). Only when every file is there and verified is the model recorded
//! as installed; then it is loaded and self-tested, and the report is kept with the install.
//!
//! Development builds can point a model at other files (`semantic_dev_setup`): a web server on
//! this computer serving small files, which is how the end-to-end suite drives the Download
//! button without contacting Hugging Face. A test instance (one with its own data folder,
//! `KESTREL_DATA_DIR`) downloads only from this computer whatever it is told.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use kestrel_db::semantic::{self as data, Install, InstalledFile};
use kestrel_embed::catalog::{HUGGING_FACE, ModelSpec};
use kestrel_embed::download::{self, Cancel, Failure, Policy, Target};
use kestrel_embed::selftest::{self, Report};
use kestrel_embed::winml::Want;
use kestrel_embed::{Embedder, Siglip};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{Semantic, Shared, models_dir, vectors_dir};

/// Photo-sized pictures the self-test embeds to measure speed.
const THROUGHPUT_IMAGES: usize = 64;

/// Which hosts downloads may use: only this computer in a test instance.
pub fn policy() -> Policy {
    if std::env::var_os("KESTREL_DATA_DIR").is_some() { Policy::LoopbackOnly } else { Policy::AnyHost }
}

/// A download in progress, or how the last one ended.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `downloading`, `testing`, `done`, `failed` or `cancelled`.
    pub state: &'static str,
    /// Bytes there of all the model's files, and their total.
    pub done: u64,
    pub total: u64,
    /// The file being fetched.
    pub file: String,
    pub bytes_per_second: f64,
    pub message: Option<String>,
}

pub struct Job {
    cancel: Cancel,
    progress: Mutex<Progress>,
}

impl Job {
    pub fn progress(&self) -> Progress {
        self.progress.lock().clone()
    }

    fn running(&self) -> bool {
        matches!(self.progress.lock().state, "downloading" | "testing")
    }
}

/// Development builds: a model's files from elsewhere.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Override {
    /// Where the files are, instead of Hugging Face; must be this computer.
    pub base_url: String,
    /// The files there, as the catalogue lists them: same paths, their own sizes and hashes.
    pub files: Vec<OverrideFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

impl Override {
    pub fn apply(&self, spec: &mut ModelSpec) {
        for file in &mut spec.files {
            if let Some(other) = self.files.iter().find(|o| o.path == file.path) {
                file.size = other.size;
                file.sha256 = std::borrow::Cow::Owned(other.sha256.clone());
            }
        }
    }
}

/// A file as the page lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    path: String,
    size: u64,
    sha256: String,
    url: String,
}

/// A model as the page shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    id: String,
    title: String,
    summary: String,
    repo: String,
    revision: String,
    page: String,
    licence: String,
    author: String,
    converted_by: String,
    files: Vec<FileView>,
    total_size: u64,
    image_size: u32,
    dim: usize,
    installed: bool,
    installed_at: Option<i64>,
    active: bool,
    selftest: Option<serde_json::Value>,
    download: Option<Progress>,
}

impl ModelView {
    pub fn new(spec: &ModelSpec, install: Option<&Install>, active: bool, download: Option<Progress>) -> ModelView {
        ModelView {
            id: spec.id.into(),
            title: spec.title.into(),
            summary: spec.summary.into(),
            repo: spec.repo.into(),
            revision: spec.revision.into(),
            page: spec.page(),
            licence: spec.licence.into(),
            author: spec.author.into(),
            converted_by: spec.converted_by.into(),
            files: spec
                .files
                .iter()
                .map(|file| FileView { path: file.path.into(), size: file.size, sha256: file.sha256.to_string(), url: spec.url(HUGGING_FACE, file) })
                .collect(),
            total_size: spec.total_size(),
            image_size: spec.image.size,
            dim: spec.dim,
            installed: install.is_some(),
            installed_at: install.map(|install| install.installed_at),
            active,
            selftest: install.and_then(|install| install.selftest.as_deref()).and_then(|json| serde_json::from_str(json).ok()),
            download,
        }
    }
}

fn now_ms() -> i64 {
    crate::disk::now_ms()
}

impl Semantic {
    fn base_url(&self, model: &str) -> String {
        self.overrides.lock().get(model).map_or_else(|| HUGGING_FACE.to_owned(), |over| over.base_url.clone())
    }

    fn set_progress(&self, job: &Job, change: impl FnOnce(&mut Progress)) {
        change(&mut job.progress.lock());
        self.status_changed(false);
    }

    /// Downloads every file of `model`, then records it, loads it and tests it.
    fn download(self: &Arc<Self>, model: &str, job: &Job) {
        let Some(spec) = self.spec(model) else { return };
        let base = self.base_url(model);
        let dir = models_dir().join(model);
        let total = spec.total_size();
        let started = Instant::now();
        let mut finished = 0u64;
        let mut first_byte: Option<(Instant, u64)> = None;
        for file in &spec.files {
            let target = Target { url: spec.url(&base, file), dest: dir.join(file.path), size: file.size, sha256: file.sha256.to_string() };
            self.set_progress(job, |p| p.file = file.path.to_owned());
            let result = download::fetch(&target, &job.cancel, policy(), &mut |done, _| {
                let there = finished + done;
                let (since, from) = *first_byte.get_or_insert((Instant::now(), there));
                let seconds = since.elapsed().as_secs_f64();
                self.set_progress(job, |p| {
                    p.done = there;
                    p.bytes_per_second = if seconds > 0.5 { (there - from) as f64 / seconds } else { 0.0 };
                });
            });
            match result {
                Ok(()) => finished += file.size,
                Err(Failure::Cancelled) => {
                    self.set_progress(job, |p| {
                        p.state = "cancelled";
                        p.message = Some("Cancelled. What arrived is kept, and the download resumes from there.".into());
                    });
                    self.status_changed(true);
                    return;
                }
                Err(failure) => {
                    let message = format!("{}: {failure}", file.path);
                    self.set_progress(job, |p| {
                        p.state = "failed";
                        p.message = Some(message);
                    });
                    self.status_changed(true);
                    return;
                }
            }
        }
        let files: Vec<InstalledFile> =
            spec.files.iter().map(|f| InstalledFile { path: f.path.into(), size: f.size, sha256: f.sha256.to_string() }).collect();
        if let Err(error) = data::record_install(&self.db, model, &files, now_ms()) {
            self.set_progress(job, |p| {
                p.state = "failed";
                p.message = Some(error.to_string());
            });
            return;
        }
        // The new download replaces whatever of this model was loaded.
        if self.current().is_some_and(|loaded| loaded.model == model) {
            self.unload();
        }
        self.set_progress(job, |p| {
            p.state = "testing";
            p.done = total;
            p.message = Some(format!("Downloaded in {} s; testing it.", started.elapsed().as_secs()));
        });
        self.status_changed(true);
        let tested = self.selftest(model);
        self.set_progress(job, |p| {
            p.state = "done";
            p.message = match &tested {
                Ok(report) if report.passed => None,
                Ok(_) => Some("The self-test failed: see its results below.".into()),
                Err(error) => Some(error.clone()),
            };
        });
        self.status_changed(true);
    }

    /// Runs the self-test on `model` and keeps its report with the install.
    pub fn selftest(&self, model: &str) -> Result<Report, String> {
        let report = if self.active().as_deref() == Some(model) {
            let loaded = self.ready()?;
            selftest::run(&loaded.embedder, THROUGHPUT_IMAGES)
        } else {
            let spec = self.spec(model).ok_or("No such model")?;
            let embedder = Siglip::open(&models_dir().join(model), &spec, Want::Gpu).map_err(|e| e.0)?;
            selftest::run(&embedder as &dyn Embedder, THROUGHPUT_IMAGES)
        };
        let json = serde_json::to_string(&report).map_err(|e| e.to_string())?;
        data::set_selftest(&self.db, model, &json)?;
        self.status_changed(true);
        Ok(report)
    }

    /// Deletes a model's files and its vectors, after the page's confirmation.
    fn delete(&self, model: &str) -> Result<(), String> {
        if self.downloads.lock().get(model).is_some_and(|job| job.running()) {
            return Err("Cancel the download first".into());
        }
        // Forgotten first, so nothing chooses to load it again; then let go of it, waiting out a
        // load in progress and the describing thread, which lets go between batches (the
        // vectors can't be deleted while their file is mapped).
        data::forget_install(&self.db, model)?;
        self.downloads.lock().remove(model);
        // A development build's stand-in files went with it.
        self.overrides.lock().remove(model);
        self.bump();
        self.release(model, Duration::from_secs(20));
        for dir in [models_dir().join(model), vectors_dir().join(model)] {
            remove_own_folder(&dir)?;
        }
        self.bump();
        self.status_changed(true);
        Ok(())
    }
}

/// Removes a folder of the app's own data (a model or its vectors), permanently: the page asks
/// first and says so. Anything outside the data folder is refused.
fn remove_own_folder(dir: &Path) -> Result<(), String> {
    let data = crate::indexer::data_dir();
    if !dir.starts_with(&data) || dir == data {
        return Err(format!("{} is not Kestrel's own data", dir.display()));
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Couldn't delete {}: {error}", dir.display())),
    }
}

/// Downloads `model`: the AI search page's Download button, and nothing else, calls this.
#[tauri::command(async)]
pub fn semantic_download(semantic: Shared, model: String) -> Result<(), String> {
    let spec = semantic.spec(&model).ok_or_else(|| format!("No model is called {model}"))?;
    let job = {
        let mut downloads = semantic.downloads.lock();
        if downloads.get(&model).is_some_and(|job| job.running()) {
            return Err("It is already downloading".into());
        }
        let job = Arc::new(Job {
            cancel: Cancel::default(),
            progress: Mutex::new(Progress {
                state: "downloading",
                done: 0,
                total: spec.total_size(),
                file: String::new(),
                bytes_per_second: 0.0,
                message: None,
            }),
        });
        downloads.insert(model.clone(), job.clone());
        job
    };
    semantic.downloads_started.fetch_add(1, Ordering::SeqCst);
    semantic.status_changed(true);
    let me = semantic.inner().clone();
    std::thread::Builder::new()
        .name("kestrel-semantic-download".into())
        .spawn(move || me.download(&model, &job))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Stops a download; what arrived is kept for the next attempt.
#[tauri::command(async)]
pub fn semantic_cancel_download(semantic: Shared, model: String) {
    if let Some(job) = semantic.downloads.lock().get(&model) {
        job.cancel.cancel();
    }
}

/// Deletes a model's files and the vectors made with it.
#[tauri::command(async)]
pub fn semantic_delete(semantic: Shared, model: String) -> Result<(), String> {
    semantic.delete(&model)
}

/// Runs the self-test of an installed model again.
#[tauri::command(async)]
pub fn semantic_selftest(semantic: Shared, model: String) -> Result<Report, String> {
    if !semantic.installed().contains(&model) {
        return Err(format!("The {model} model is not downloaded"));
    }
    semantic.selftest(&model)
}

// ---------- development hooks ----------

fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

/// A folder under the temp folder, where the hooks may write.
fn in_temp(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let temp = std::env::temp_dir();
    let inside = path.canonicalize().ok().or_else(|| path.parent()?.canonicalize().ok()).is_some_and(|real| {
        temp.canonicalize().is_ok_and(|temp| real.starts_with(temp))
    });
    if inside { Ok(path) } else { Err(format!("{} is not under the temp folder", path.display())) }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevSetup {
    /// The folders to describe.
    scopes: Option<Vec<String>>,
    /// A model to fetch from this computer instead, or `None` to clear it.
    model: Option<String>,
    #[serde(rename = "override")]
    over: Option<Override>,
}

/// Development builds: points a model at files served by this computer, and the describing at
/// a scratch folder.
#[tauri::command(async)]
pub fn semantic_dev_setup(semantic: Shared, setup: DevSetup) -> Result<(), String> {
    development_only()?;
    if let Some(scopes) = setup.scopes {
        semantic.set_scopes(Some(scopes))?;
    }
    if let Some(model) = setup.model {
        match setup.over {
            Some(over) => {
                if !download::is_loopback_url(&over.base_url) {
                    return Err("Development downloads come only from this computer".into());
                }
                semantic.overrides.lock().insert(model, over);
            }
            None => {
                semantic.overrides.lock().remove(&model);
            }
        }
    }
    semantic.bump();
    Ok(())
}

/// Development builds: writes the tiny test model's files into `dir` (under the temp folder),
/// for a web server there; returns each file's path, size and SHA-256.
#[tauri::command(async)]
pub fn semantic_dev_tiny_files(dir: String) -> Result<Vec<(String, u64, String)>, String> {
    development_only()?;
    let dir = in_temp(&dir)?;
    kestrel_embed::onnx::write_tiny_model(&dir, true).map_err(|e| e.to_string())
}

/// Development builds: installs the tiny test model as `model`, as if it had been downloaded
/// and verified, then loads and self-tests it.
#[tauri::command(async)]
pub fn semantic_dev_install_tiny(semantic: Shared, model: String) -> Result<Report, String> {
    development_only()?;
    let dir = models_dir().join(&model);
    remove_own_folder(&dir)?;
    let written = kestrel_embed::onnx::write_tiny_model(&dir, true).map_err(|e| e.to_string())?;
    let files: Vec<OverrideFile> = written.iter().map(|(path, size, sha256)| OverrideFile { path: path.clone(), size: *size, sha256: sha256.clone() }).collect();
    let installed: Vec<InstalledFile> =
        written.into_iter().map(|(path, size, sha256)| InstalledFile { path, size, sha256 }).collect();
    // The catalogue's sizes and hashes are the real model's; these files have their own.
    semantic.overrides.lock().insert(model.clone(), Override { base_url: "http://127.0.0.1".into(), files });
    data::record_install(&semantic.db, &model, &installed, now_ms())?;
    semantic.db.set_setting(super::ACTIVE, &model)?;
    semantic.unload();
    semantic.selftest(&model)
}

/// Development builds: writes a video of coloured segments (an H.264 MP4) under the temp folder.
#[tauri::command(async)]
pub fn semantic_dev_write_video(path: String, size: (u32, u32), fps: u32, segments: Vec<([u8; 3], u32)>) -> Result<(), String> {
    development_only()?;
    let path = in_temp(&path)?;
    kestrel_embed::video::write_test_video(&path, size.0, size.1, fps, &segments).map_err(|e| e.0)
}

/// Development builds: how many downloads started since Kestrel started.
#[tauri::command(async)]
pub fn semantic_dev_downloads(semantic: Shared) -> Result<usize, String> {
    development_only()?;
    Ok(semantic.downloads_started.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_swaps_sizes_and_hashes_but_keeps_the_paths() {
        let mut spec = kestrel_embed::catalog::fast();
        let over = Override {
            base_url: "http://127.0.0.1:9".into(),
            files: vec![OverrideFile { path: "tokenizer.json".into(), size: 12, sha256: "ab".repeat(32) }],
        };
        over.apply(&mut spec);
        let tokenizer = spec.file("tokenizer").unwrap();
        assert_eq!(tokenizer.size, 12);
        assert_eq!(tokenizer.sha256, "ab".repeat(32));
        assert_eq!(spec.file("vision").unwrap().size, 186_131_676, "files it doesn't name stay as published");
        let view = ModelView::new(&spec, None, false, None);
        assert!(!view.installed);
        assert_eq!(view.files[0].url, format!("{HUGGING_FACE}/{}/resolve/{}/onnx/vision_model_fp16.onnx", spec.repo, spec.revision));
    }

    #[test]
    fn only_the_apps_own_data_is_ever_removed() {
        assert!(remove_own_folder(Path::new(r"C:\Windows")).is_err());
        assert!(remove_own_folder(&crate::indexer::data_dir()).is_err());
        assert!(remove_own_folder(&crate::indexer::data_dir().join("models").join("kestrel-test-missing")).is_ok());
        assert!(in_temp(r"C:\Windows\x.mp4").is_err());
        assert!(in_temp(std::env::temp_dir().join("kestrel-test.mp4").to_str().unwrap()).is_ok());
    }
}
