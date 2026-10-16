//! Searches inside files, one per tab of each window, running in the background while the
//! results list shows what they found so far.
//!
//! A search starts when the tab asks with new text or options, which first cancels the tab's
//! previous one (every keystroke does this). The thread reading files hands each match to the
//! search's list; another sends `search-progress` to the window four times a second while it
//! runs, and once more when it is done. The window then asks again with `update`, and gets a
//! fresh result set holding everything found so far, sorted as the tab sorts, in place of the
//! one it shows (`Engine::show_found`), its selection carried across by path.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kestrel_content::{Counts, Found, Progress, Run, Search, Source};
use kestrel_index::{DirItem, fold};
use kestrel_query::{ClauseKind, NameFilter, Query};
use parking_lot::Mutex;
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::engine::{Engine, Listing, SortSpec};
use crate::visibility::Visibility;

/// How often a running search tells its window how far it got.
const TICK: Duration = Duration::from_millis(250);
/// How long a new search may run before its first results are shown.
const FIRST_LOOK: Duration = Duration::from_millis(150);

/// Where a search's files come from.
pub enum Files {
    /// Everything beneath a folder, listed from disk, narrowed by the query's other clauses.
    Folder { root: String, visibility: Visibility, filters: Filters },
    /// These files, found by an index search of the query's other clauses.
    Chosen(Vec<(String, DirItem)>),
}

/// Everything a search needs, decided by the command before it starts.
pub struct Plan {
    pub search: Search,
    pub files: Files,
    pub gitignore: bool,
    /// The span photos must have been taken in (`taken:`), as FILETIME ticks, and whether it
    /// is negated.
    pub taken: Vec<(i64, i64, bool)>,
    /// Where it looks, in words, for the status line: "in Downloads".
    pub place: String,
}

/// A search's state, as the window shows it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    #[serde(flatten)]
    pub counts: Counts,
    pub done: bool,
    pub place: String,
    /// Milliseconds since it started.
    pub elapsed_ms: u64,
}

struct Running {
    /// What was asked, so an update of the same search can be told from a new one.
    key: String,
    cancel: Arc<AtomicBool>,
    progress: Progress,
    found: Mutex<Vec<Found>>,
    done: AtomicBool,
    place: String,
    started: Instant,
}

impl Running {
    fn new(key: String, place: String) -> Arc<Self> {
        Arc::new(Self {
            key,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Progress::default(),
            found: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
            place,
            started: Instant::now(),
        })
    }

    fn status(&self) -> Status {
        Status {
            counts: self.progress.counts(),
            // A stopped search is over, whatever its threads are still finishing.
            done: self.done.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed),
            place: self.place.clone(),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        }
    }
}

/// The searches inside files of every tab of every window, by window label and tab.
#[derive(Default)]
pub struct Searches {
    running: Mutex<HashMap<String, Arc<Running>>>,
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    tab: u32,
    status: Status,
}

fn slot(window: &str, tab: u32) -> String {
    format!("{window}:{tab}")
}

impl Searches {
    /// Cancels the tab's search, if it has one, and forgets it: the tab left its search.
    pub fn cancel(&self, window: &str, tab: u32) {
        if let Some(old) = self.running.lock().remove(&slot(window, tab)) {
            old.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Stops the tab's search where it got to, keeping what it found: updates of it then show
    /// that rather than starting it again.
    pub fn stop(&self, window: &str, tab: u32) {
        if let Some(running) = self.running.lock().get(&slot(window, tab)) {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the tab's search is the one asked for with `key`.
    pub fn is_current(&self, window: &str, tab: u32, key: &str) -> bool {
        self.running.lock().get(&slot(window, tab)).is_some_and(|running| running.key == key)
    }

    /// What the tab's search found so far, as a result set sorted by `spec`, and how far it got.
    pub fn snapshot(&self, engine: &Engine, window: &str, tab: u32, spec: SortSpec) -> Option<(Listing, Status)> {
        let running = self.running.lock().get(&slot(window, tab)).cloned()?;
        let status = running.status();
        let found = running.found.lock().clone();
        Some((engine.show_found(found, spec), status))
    }

    /// Runs a search for the tab on this thread, to the end, without telling any window.
    #[cfg(test)]
    pub fn run_here(&self, window: &str, tab: u32, plan: Plan) {
        let running = Running::new(String::new(), plan.place.clone());
        self.running.lock().insert(slot(window, tab), running.clone());
        run(&running, plan);
    }

    /// Starts a search for the tab, cancelling its previous one, and waits a moment for the
    /// first results.
    pub fn start(&self, app: AppHandle, window: &str, tab: u32, key: String, plan: Plan) {
        let running = Running::new(key, plan.place.clone());
        if let Some(old) = self.running.lock().insert(slot(window, tab), running.clone()) {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let reader = running.clone();
        let started = std::thread::Builder::new().name("kestrel-content-search".into()).spawn(move || run(&reader, plan));
        if started.is_err() {
            running.done.store(true, Ordering::Relaxed);
            return;
        }
        let ticking = running.clone();
        let label = window.to_owned();
        let _ = std::thread::Builder::new().name("kestrel-content-progress".into()).spawn(move || {
            let mut last: Option<(Counts, bool)> = None;
            loop {
                std::thread::sleep(TICK);
                if ticking.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let status = ticking.status();
                let now = (status.counts, status.done);
                if last != Some(now) {
                    last = Some(now);
                    let _ = app.emit_to(label.as_str(), "search-progress", ProgressEvent { tab, status: status.clone() });
                }
                if status.done {
                    return;
                }
            }
        });
        // The first results, or the whole answer for a small folder, come with the reply.
        let until = Instant::now() + FIRST_LOOK;
        while Instant::now() < until && !running.done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Milliseconds each file waits before it is read (development builds only, `search_dev_slow`):
/// the end-to-end tests watch a search's results arrive and stop it part way.
static SLOW_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn slow_down(ms: u64) {
    SLOW_MS.store(ms, Ordering::Relaxed);
}

fn dawdle(cancel: &AtomicBool) {
    let ms = SLOW_MS.load(Ordering::Relaxed);
    if cfg!(debug_assertions) && ms > 0 && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Reads the files.
fn run(running: &Running, plan: Plan) {
    let Plan { search, files, gitignore, taken, .. } = plan;
    let taken_ok = |path: &str| {
        dawdle(&running.cancel);
        if taken.is_empty() {
            return true;
        }
        let when = super::taken::date_taken(path);
        taken.iter().all(|&(from, to, negate)| when.is_some_and(|when| (from..to).contains(&when)) != negate)
    };
    let found = |hit: Found| running.found.lock().push(hit);
    let cancel = running.cancel.clone();
    match files {
        Files::Folder { root, visibility, filters } => {
            let shows = move |attrs: u32| visibility.shows(attrs);
            let filter = |path: &str, item: &DirItem| filters.matches(path, item) && taken_ok(path);
            let run = Run { search: &search, gitignore, filter: Some(&filter), cancel: &cancel, progress: &running.progress };
            run.run(Source::Folder { root, shows: &shows }, &found);
        }
        Files::Chosen(chosen) => {
            let filter = |path: &str, _: &DirItem| taken_ok(path);
            let slow = cfg!(debug_assertions) && SLOW_MS.load(Ordering::Relaxed) > 0;
            let filter: Option<&kestrel_content::Filter<'_>> = if taken.is_empty() && !slow { None } else { Some(&filter) };
            let run = Run { search: &search, gitignore, filter, cancel: &cancel, progress: &running.progress };
            run.run(Source::Files(chosen), &found);
        }
    }
    running.done.store(true, Ordering::Relaxed);
}

// ---------- the query's other clauses, for files listed from disk ----------

enum Test {
    Name(String),
    Path(String),
    Glob(NameFilter),
    Regex(Regex),
    Ext(Vec<String>),
    Size(u64, u64),
    Modified(i64, i64),
    Within(String),
    Never,
    Always,
}

/// The query's clauses other than contents and dates taken, tested on files listed from disk
/// rather than found in the index: name, path, wildcards, regular expressions, extensions,
/// sizes, dates and folders. (Tags, which only the index can answer, let everything through.)
#[derive(Default)]
pub struct Filters {
    tests: Vec<(bool, Test)>,
}

impl Filters {
    pub fn new(query: &Query) -> Result<Self, String> {
        let mut tests = Vec::new();
        for clause in &query.clauses {
            let test = match &clause.kind {
                ClauseKind::Name(text) => Test::Name(text.clone()),
                ClauseKind::Path(text) => Test::Path(text.clone()),
                ClauseKind::Glob(pattern) => Test::Glob(NameFilter::new(pattern)),
                ClauseKind::Regex(pattern) => {
                    Test::Regex(RegexBuilder::new(pattern).case_insensitive(true).build().map_err(|e| format!("Invalid regex: {e}"))?)
                }
                ClauseKind::Ext(exts) => Test::Ext(exts.clone()),
                &ClauseKind::Size(min, max) => Test::Size(min, max),
                &ClauseKind::Modified(from, to) => Test::Modified(from, to),
                ClauseKind::Folders => Test::Never,
                ClauseKind::Within(path) => Test::Within(format!("{}\\", fold(path.trim_end_matches('\\')))),
                ClauseKind::Files | ClauseKind::Tag(_) | ClauseKind::Entries(_) | ClauseKind::Content(_) | ClauseKind::Taken(..) => Test::Always,
            };
            tests.push((clause.negate, test));
        }
        Ok(Self { tests })
    }

    /// Whether the file at `path` passes every clause.
    pub fn matches(&self, path: &str, item: &DirItem) -> bool {
        if self.tests.is_empty() {
            return true;
        }
        let name = fold(&item.name);
        let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        self.tests.iter().all(|(negate, test)| {
            let passes = match test {
                Test::Name(text) => name.contains(text.as_str()),
                Test::Path(text) => fold(path).contains(text.as_str()),
                Test::Glob(filter) => filter.matches(name.as_bytes()),
                Test::Regex(regex) => regex.is_match(&item.name),
                Test::Ext(exts) => exts.iter().any(|wanted| wanted == ext),
                Test::Size(min, max) => (*min..=*max).contains(&item.meta.size),
                Test::Modified(from, to) => (*from..*to).contains(&item.meta.mtime),
                Test::Within(folder) => fold(path).starts_with(folder.as_str()),
                Test::Never => false,
                Test::Always => true,
            };
            passes != *negate
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::Meta;
    use kestrel_query::{Clock, parse};

    const CLOCK: Clock = Clock { now_unix: 20_739 * 86_400 + 12 * 3600, utc_offset_secs: 0 };

    fn item(name: &str, size: u64, mtime: i64) -> DirItem {
        DirItem { name: name.into(), meta: Meta { size, mtime, attrs: 0x20, ..Meta::default() }, alias: false }
    }

    #[test]
    fn other_clauses_are_tested_on_files_from_disk() {
        let filters = |text: &str| Filters::new(&parse(text, CLOCK).unwrap()).unwrap();
        let today = (20_739 * 86_400 + 11_644_473_600) * 10_000_000 + 1;
        let notes = item("Notes.TXT", 2048, today);
        let path = r"C:\work\Notes.TXT";
        for (query, expected) in [
            ("", true),
            ("note", true),
            ("ext:txt;md", true),
            ("ext:rs", false),
            ("!ext:rs", true),
            ("size:>1kb", true),
            ("size:>1mb", false),
            ("dm:today", true),
            ("dm:yesterday", false),
            ("*.txt", true),
            ("n?tes.*", true),
            (r"regex:^notes\.", true),
            (r"in:C:\work", true),
            (r"in:C:\wor", false),
            (r"work\notes", true),
            ("type:folder", false),
            ("content:anything tag:x", true),
        ] {
            assert_eq!(filters(query).matches(path, &notes), expected, "{query}");
        }
        assert!(Filters::new(&parse("regex:(", CLOCK).unwrap()).is_err());
    }
}
