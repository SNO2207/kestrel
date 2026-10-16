//! Searches inside files, one per tab of each window, running in the background while the
//! results list shows what they found so far.
//!
//! A search starts when the tab asks with new text or options, which first stops the tab's
//! previous one (every keystroke does this), before anything else. The command answers at once
//! with what little is found by then; the thread reading files hands each match to the search's
//! list, and another sends `search-progress` to the window as soon as the first match is found
//! (or the search is done), then four times a second while it runs, and once more when it is
//! done. The window then asks again with `update`, and gets a fresh result set holding
//! everything found so far, sorted as the tab sorts, in place of the one it shows
//! (`Engine::show_found`), its selection carried across by path.
//!
//! The files a search reads, when the query's other clauses choose them, are kept for the tab
//! (`Searches::chosen`): typing changes the text to find, not the files, and choosing them
//! again on every keystroke (an index search and a row for every file, a tenth of a second
//! over a quarter of a million files) held each keystroke up.

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

use crate::engine::{Engine, Listing, SortSpec};
use crate::visibility::Visibility;

/// How often a running search tells its window how far it got.
const TICK: Duration = Duration::from_millis(250);
/// How often a new search looks for its first match (or its end), to tell the window at once.
const FIRST_LOOK: Duration = Duration::from_millis(10);

/// The files an index search chose, as `(folder, item)`; shared between the searches of one
/// tab that choose the same.
pub type Chosen = Arc<Vec<(String, DirItem)>>;

/// Where a search's files come from.
pub enum Files {
    /// Everything beneath a folder, listed from disk, narrowed by the query's other clauses.
    Folder { root: String, visibility: Visibility, filters: Filters },
    /// These files, found by an index search of the query's other clauses.
    Chosen(Chosen),
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
    /// How long each file waits before it is read: nothing, except for the end-to-end tests
    /// (`search_dev_slow`) and the unit tests, which watch results arrive.
    pub dawdle: Duration,
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
        Self::stopped_by(key, place, Arc::new(AtomicBool::new(false)))
    }

    fn stopped_by(key: String, place: String, cancel: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            key,
            cancel,
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
    /// The files each tab's last search chose, and what chose them.
    chosen: Mutex<HashMap<String, (String, Chosen)>>,
}

/// What `search-progress` carries to the window.
#[derive(Clone, Serialize)]
pub struct ProgressEvent {
    pub tab: u32,
    pub status: Status,
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
        self.chosen.lock().remove(&slot(window, tab));
    }

    /// A new search is asked for in the tab (another `key`): the one it replaces stops reading
    /// now, rather than once the new one has chosen its files and started.
    pub fn supersede(&self, window: &str, tab: u32, key: &str) {
        if let Some(running) = self.running.lock().get(&slot(window, tab))
            && running.key != key
        {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The files the tab's search reads: those its last search chose, when they were chosen the
    /// same way (`key`: the clauses that choose them, the folder, what is shown, the index's
    /// revision), or else what `choose` chooses now.
    pub fn chosen(&self, window: &str, tab: u32, key: String, choose: impl FnOnce() -> Result<Vec<(String, DirItem)>, String>) -> Result<Chosen, String> {
        let slot = slot(window, tab);
        if let Some((kept, files)) = self.chosen.lock().get(&slot)
            && *kept == key
        {
            return Ok(files.clone());
        }
        let files: Chosen = Arc::new(choose()?);
        self.chosen.lock().insert(slot, (key, files.clone()));
        Ok(files)
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

    /// Starts a search for the tab, cancelling its previous one, and returns at once: `tell`
    /// hears of its progress (the window's `search-progress`), first as soon as it finds a
    /// match or is done, then four times a second. The keystroke that started it is never held
    /// up waiting for results: a tab runs one search command at a time, so the next keystroke
    /// would wait too.
    pub fn start(&self, tell: impl Fn(ProgressEvent) + Send + 'static, window: &str, tab: u32, key: String, plan: Plan) {
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
        let _ = std::thread::Builder::new().name("kestrel-content-progress".into()).spawn(move || {
            let mut last: Option<(Counts, bool)> = None;
            let begun = Instant::now();
            loop {
                std::thread::sleep(if last.is_none() { FIRST_LOOK } else { TICK });
                if ticking.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let status = ticking.status();
                // Until the first word, nothing is said before there is something to show.
                if last.is_none() && status.counts.matched == 0 && !status.done && begun.elapsed() < TICK {
                    continue;
                }
                let now = (status.counts, status.done);
                if last != Some(now) {
                    last = Some(now);
                    tell(ProgressEvent { tab, status: status.clone() });
                }
                if status.done {
                    return;
                }
            }
        });
    }
}

/// Runs a search on this thread to the end, or until `cancel` is raised, and returns what it
/// found and how far it got: the query service's searches inside files (devtools/query.rs).
pub fn run_to_end(plan: Plan, cancel: Arc<AtomicBool>) -> (Vec<Found>, Status) {
    let running = Running::stopped_by(String::new(), plan.place.clone(), cancel);
    run(&running, plan);
    let found = std::mem::take(&mut *running.found.lock());
    (found, running.status())
}

/// Milliseconds each file waits before it is read (development builds only, `search_dev_slow`):
/// the end-to-end tests watch a search's results arrive and stop it part way.
static SLOW_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn slow_down(ms: u64) {
    SLOW_MS.store(ms, Ordering::Relaxed);
}

/// How long each file of a search planned now waits (development builds only).
pub fn slowness() -> Duration {
    let ms = SLOW_MS.load(Ordering::Relaxed);
    if cfg!(debug_assertions) { Duration::from_millis(ms) } else { Duration::ZERO }
}

fn dawdle(how_long: Duration, cancel: &AtomicBool) {
    if !how_long.is_zero() && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(how_long);
    }
}

/// Reads the files.
fn run(running: &Running, plan: Plan) {
    let Plan { search, files, gitignore, taken, dawdle: how_long, .. } = plan;
    let taken_ok = |path: &str| {
        dawdle(how_long, &running.cancel);
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
            let filter: Option<&kestrel_content::Filter<'_>> = if taken.is_empty() && how_long.is_zero() { None } else { Some(&filter) };
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
                // Tags, the media library's facts and descriptions only the index's entries can
                // answer; dates taken are read from the photos (`Plan::taken`).
                ClauseKind::Files
                | ClauseKind::Tag(_)
                | ClauseKind::Entries(_)
                | ClauseKind::Content(_)
                | ClauseKind::Media(_)
                | ClauseKind::Like(_) => Test::Always,
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

    /// A search for "needle" in these files, each waiting `dawdle` before it is read.
    fn plan_over(files: &Chosen, dawdle: Duration) -> Plan {
        let pattern = kestrel_content::Pattern { text: "needle".into(), regex: false, case: kestrel_content::Case::Insensitive, whole_word: false, negate: false };
        let search = Search::new(&[pattern], kestrel_content::Limits::default()).unwrap();
        Plan { search, files: Files::Chosen(files.clone()), gitignore: false, taken: Vec::new(), place: "test".into(), dawdle }
    }

    fn needles(label: &str) -> (std::path::PathBuf, Chosen) {
        let root = std::env::temp_dir().join(format!("kestrel-search-content-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut files = Vec::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), "hay\nneedle\n").unwrap();
            files.push((root.to_str().unwrap().to_owned(), item(name, 11, 0)));
        }
        (root, Arc::new(files))
    }

    /// Every keystroke of a search inside files starts one. The command answers at once (it once
    /// waited up to 150 ms for the first results, and the tab's next keystroke waited behind
    /// it), the window hears of the first match as soon as it is found, and the search a new
    /// keystroke replaces stops reading before anything else happens.
    #[test]
    fn a_search_inside_files_answers_at_once_and_tells_of_matches_as_they_come() {
        let (root, files) = needles("start");
        let searches = Searches::default();
        let (told, heard) = std::sync::mpsc::channel();
        let started = Instant::now();
        searches.start(move |event: ProgressEvent| drop(told.send(event.status)), "main", 1, "first".into(), plan_over(&files, Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_millis(100), "{:?}", started.elapsed());
        let first = heard.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(first.counts.matched > 0 || first.done, "the first word says something was found: {first:?}");
        let mut last = first;
        while !last.done {
            last = heard.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        assert_eq!(last.counts.matched, 3);

        // Superseded by another search: stopped at once. Asked again for the same: left alone.
        searches.start(|_: ProgressEvent| {}, "main", 1, "second".into(), plan_over(&files, Duration::from_secs(30)));
        let running = |searches: &Searches| searches.running.lock().get(&slot("main", 1)).unwrap().clone();
        searches.supersede("main", 1, "second");
        assert!(!running(&searches).cancel.load(Ordering::Relaxed));
        searches.supersede("main", 1, "third");
        assert!(running(&searches).cancel.load(Ordering::Relaxed));
        assert!(running(&searches).status().done, "a stopped search is over");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Typing in a search inside files changes the text to find, not the files: those the other
    /// clauses chose are kept while what chose them stands.
    #[test]
    fn the_files_chosen_are_kept_while_what_chose_them_stands() {
        let (root, files) = needles("chosen");
        let searches = Searches::default();
        let asked = std::cell::Cell::new(0);
        let choose = || {
            asked.set(asked.get() + 1);
            Ok(files.as_ref().clone())
        };
        let first = searches.chosen("main", 1, "ext:txt @ 7".into(), choose).unwrap();
        let again = searches.chosen("main", 1, "ext:txt @ 7".into(), choose).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(asked.get(), 1);
        searches.chosen("main", 1, "ext:txt @ 8".into(), choose).unwrap();
        searches.chosen("main", 2, "ext:txt @ 8".into(), choose).unwrap();
        assert_eq!(asked.get(), 3, "the index changed, or another tab");
        searches.cancel("main", 1);
        searches.chosen("main", 1, "ext:txt @ 8".into(), choose).unwrap();
        assert_eq!(asked.get(), 4, "a tab that left its search forgets them");
        assert!(searches.chosen("main", 3, "x".into(), || Err("no".into())).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
