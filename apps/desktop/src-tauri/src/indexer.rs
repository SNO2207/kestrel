//! Keeps the engine's index complete and current for as long as the app runs.
//!
//! At startup the saved index is loaded and brought up to date from each drive's change
//! journal, which takes a fraction of a second. Only when that is impossible (no saved index,
//! or more changes than the journal holds) are the drives crawled again. After that the
//! journals are followed continuously, and the index is saved periodically and on exit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_index::live::{Outcome, Tracker};
use kestrel_index::{IndexBuilder, broker, snapshot, walk};
use tauri::{AppHandle, Emitter};

use crate::engine::Engine;
use crate::places;

/// How often the change journals are read once the index is current.
const JOURNAL_POLL: Duration = Duration::from_millis(300);
/// How often a changed index is saved, bounding the catch-up needed after a crash.
const SAVE_EVERY: Duration = Duration::from_secs(10 * 60);

/// Where the app keeps its index and other data: `%LOCALAPPDATA%\Kestrel`, unless
/// `KESTREL_DATA_DIR` names another folder, as isolated test instances do.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KESTREL_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Kestrel")
}

pub fn snapshot_path() -> PathBuf {
    data_dir().join("index.kidx")
}

pub fn save(engine: &Engine, path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = snapshot::save(&engine.index().read(), path) {
        eprintln!("could not save the index: {error}");
    }
}

/// Builds a new index of every drive and installs it: through the broker service when it is
/// installed, which reads the drives' file tables in seconds, otherwise by crawling. Either
/// way the journal positions are taken first, so changes made meanwhile are applied after.
fn rescan(engine: &Engine, roots: &[String], trackers: &[Tracker], in_background: bool) {
    match broker::scan(roots) {
        Ok(Some(index)) => return engine.install(index),
        Ok(None) => {}
        Err(error) => eprintln!("the broker could not scan, crawling instead: {error}"),
    }
    let cursors = trackers.iter().filter_map(|tracker| tracker.position().ok()).collect();
    // A background rescan leaves most cores to the user; the very first one uses them all.
    let threads = if in_background { (rayon::current_num_threads() / 4).max(2) } else { 0 };
    let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(threads).build() else { return };
    let mut builder = IndexBuilder::new();
    pool.install(|| {
        for root in roots {
            if let Err(error) = walk::walk(root, &mut builder) {
                eprintln!("could not scan {root}: {error}");
            }
        }
    });
    let mut index = pool.install(|| builder.finish());
    index.set_journal(cursors);
    engine.install(index);
}

pub fn run(app: AppHandle, engine: Arc<Engine>) {
    let announce = |state, refreshing| {
        let _ = app.emit("index-status", engine.set_status(state, refreshing));
    };
    let path = snapshot_path();
    let roots = places::fixed_drive_roots();
    // Drives without a change journal (not NTFS) can only be kept current by rescanning.
    let trackers: Vec<Tracker> = roots.iter().filter_map(|root| Tracker::open(root).ok()).collect();

    let loaded = snapshot::load(&path).map(|index| engine.install(index)).is_ok();
    announce(if loaded { "ready" } else { "scanning" }, true);
    let current = loaded
        && trackers.len() == roots.len()
        && trackers.iter().all(|tracker| matches!(tracker.catch_up(engine.index()), Ok(Outcome::Current { .. })));
    if !current {
        rescan(&engine, &roots, &trackers, loaded);
    }
    save(&engine, &path);
    announce("ready", false);

    let mut last_saved = Instant::now();
    loop {
        std::thread::sleep(JOURNAL_POLL);
        let before = engine.live_count();
        for tracker in &trackers {
            match tracker.catch_up(engine.index()) {
                Ok(Outcome::Current { changes, .. }) if changes > 0 => engine.mark_dirty(),
                Ok(Outcome::Lost(reason)) => {
                    eprintln!("rescanning: {reason}");
                    announce("ready", true);
                    rescan(&engine, &roots, &trackers, true);
                    engine.mark_dirty();
                    announce("ready", false);
                }
                Ok(_) => {}
                Err(error) => eprintln!("could not read the change journal of {}: {error}", tracker.root()),
            }
        }
        // Keeps the file count in the status bar current.
        if engine.live_count() != before {
            announce("ready", false);
        }
        if last_saved.elapsed() >= SAVE_EVERY && engine.take_dirty() {
            save(&engine, &path);
            last_saved = Instant::now();
        }
    }
}
