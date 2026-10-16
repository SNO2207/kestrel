//! Running an extraction or a compression as a job the window can follow and cancel, and the
//! ways one can fail.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

/// Progress is reported at most this often, so a job of many small files doesn't flood the
/// window with events.
const REPORT_EVERY: Duration = Duration::from_millis(80);

/// Why a job stopped without finishing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Some of what was asked for is encrypted, and no password was given.
    Password,
    /// The password given doesn't open it.
    WrongPassword,
    /// A part of the archive Kestrel can't read, said as the rest of "Kestrel can't …".
    Unsupported(String),
    /// The archive is damaged or isn't what its name says.
    Corrupt(String),
    /// An entry, named here, would be written outside the destination.
    Unsafe(String),
    /// The destination's drive can't hold what the archive says it holds.
    Space { needed: u64, free: u64, drive: String },
    /// The archive produced more than it declared, or far more than its own size.
    Bomb(String),
    Cancelled,
    /// Writing failed: a full disk, a file in use, no permission.
    Io(String),
}

/// A failure as the window gets it: what kind, for deciding what to do (ask for a password,
/// say nothing after a cancel), and the words to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub kind: &'static str,
    pub message: String,
}

impl Problem {
    pub fn other(message: impl Into<String>) -> Self {
        Problem { kind: "other", message: message.into() }
    }
}

impl From<String> for Problem {
    fn from(message: String) -> Self {
        Problem::other(message)
    }
}

/// "1.2 GB", as the window writes sizes.
pub fn size_text(bytes: u64) -> String {
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

impl Failure {
    /// The failure in words, for an archive called `name`.
    pub fn problem(&self, name: &str) -> Problem {
        let (kind, message) = match self {
            Failure::Password => ("password", format!("{name} is protected by a password.")),
            Failure::WrongPassword => ("wrong-password", format!("That password doesn't open {name}.")),
            Failure::Unsupported(what) => ("unsupported", format!("Kestrel can't open {name}: {what}.")),
            Failure::Corrupt(detail) => {
                ("corrupt", format!("{name} is damaged or incomplete ({detail}). Nothing was extracted."))
            }
            Failure::Unsafe(entry) => (
                "unsafe",
                format!(
                    "{name} wasn't extracted: its item \u{201c}{entry}\u{201d} would be written outside the destination folder."
                ),
            ),
            Failure::Space { needed, free, drive } => (
                "space",
                format!(
                    "There isn't enough free space on {drive} for {name}: it needs {}, and {} is free.",
                    size_text(*needed),
                    size_text(*free)
                ),
            ),
            Failure::Bomb(detail) => ("bomb", format!("Extracting {name} stopped: {detail}. Nothing was extracted.")),
            Failure::Cancelled => ("cancelled", "Cancelled.".to_owned()),
            Failure::Io(detail) => ("io", detail.clone()),
        };
        Problem { kind, message }
    }
}

/// What a job reports while it runs.
#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub job: u32,
    /// `reading` (looking through the archive), `extracting`, `placing` (moving what was
    /// extracted into place, which may ask about conflicts) or `compressing`.
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
    /// The item being worked on.
    pub item: String,
}

type Report = Box<dyn Fn(Progress) + Send + Sync>;
type GivenUp = Box<dyn Fn() -> bool + Send + Sync>;

/// One running job: whether it has been asked to stop, and where its progress goes.
pub struct Job {
    id: u32,
    cancel: AtomicBool,
    /// Whether whoever wanted the job no longer does, asked as it goes (a preview of an item
    /// the window has moved on from).
    given_up: Option<GivenUp>,
    report: Option<Report>,
    last: Mutex<Option<(Instant, &'static str)>>,
}

impl Job {
    pub fn new(id: u32, report: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Job { id, cancel: AtomicBool::new(false), given_up: None, report: Some(Box::new(report)), last: Mutex::new(None) }
    }

    /// A job nobody follows, for work done on the window's behalf (a preview, a drag).
    pub fn quiet() -> Self {
        Job::quiet_with_id(0)
    }

    /// A job nobody follows that the window can still cancel by `id` (a listing).
    pub fn quiet_with_id(id: u32) -> Self {
        Job { id, cancel: AtomicBool::new(false), given_up: None, report: None, last: Mutex::new(None) }
    }

    /// A job nobody follows that stops once `given_up` says it is no longer wanted.
    pub fn quiet_until(given_up: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Job { given_up: Some(Box::new(given_up)), ..Job::quiet() }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.given_up.as_ref().is_some_and(|given_up| given_up())
    }

    /// `Err(Cancelled)` once the job has been asked to stop.
    pub fn check(&self) -> Result<(), Failure> {
        if self.cancelled() { Err(Failure::Cancelled) } else { Ok(()) }
    }

    /// Reports progress, at most every [`REPORT_EVERY`] unless the phase changes.
    pub fn tick(&self, phase: &'static str, done: u64, total: u64, item: &str) {
        let Some(report) = &self.report else { return };
        let now = Instant::now();
        {
            let mut last = self.last.lock();
            if let Some((at, previous)) = *last
                && previous == phase
                && now.duration_since(at) < REPORT_EVERY
                && done < total
            {
                return;
            }
            *last = Some((now, phase));
        }
        report(Progress { job: self.id, phase, done, total, item: item.to_owned() });
    }
}

/// The jobs running now, by the window that started each and the number it gave the job, so
/// it can cancel them. Each window numbers its own jobs, so a number alone names nothing: two
/// windows' first jobs are both 1.
#[derive(Default)]
pub struct Jobs {
    running: Mutex<HashMap<(String, u32), Arc<Job>>>,
}

impl Jobs {
    /// Registers a job of `window`'s until the returned guard is dropped.
    pub fn begin(&self, window: &str, job: Arc<Job>) -> Running<'_> {
        let key = (window.to_owned(), job.id);
        self.running.lock().insert(key.clone(), job);
        Running { jobs: self, key }
    }

    /// Asks a job of `window`'s to stop; it stops at its next check and removes what it made.
    pub fn cancel(&self, window: &str, id: u32) -> bool {
        self.running.lock().get(&(window.to_owned(), id)).map(|job| job.cancel()).is_some()
    }
}

/// Keeps a job registered while it runs.
pub struct Running<'a> {
    jobs: &'a Jobs,
    key: (String, u32),
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.jobs.running.lock().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_written_as_the_window_writes_them() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(250 << 20), "250 MB");
        assert_eq!(size_text(3 << 30), "3.0 GB");
    }

    #[test]
    fn failures_explain_themselves() {
        let unsafe_entry = Failure::Unsafe("../evil.txt".into()).problem("bad.zip");
        assert_eq!(unsafe_entry.kind, "unsafe");
        assert!(unsafe_entry.message.contains("outside the destination folder"), "{}", unsafe_entry.message);
        assert_eq!(Failure::Password.problem("a.7z").kind, "password");
        let space = Failure::Space { needed: 10 << 30, free: 1 << 30, drive: r"D:\".into() }.problem("big.zip");
        assert_eq!(space.message, r"There isn't enough free space on D:\ for big.zip: it needs 10.0 GB, and 1.0 GB is free.");
    }

    #[test]
    fn jobs_can_be_cancelled_while_they_run() {
        let jobs = Jobs::default();
        let job = Arc::new(Job::quiet());
        assert!(!jobs.cancel("main", 0));
        {
            let _running = jobs.begin("main", job.clone());
            assert!(jobs.cancel("main", 0));
            assert_eq!(job.check(), Err(Failure::Cancelled));
        }
        assert!(!jobs.cancel("main", 0), "a finished job is forgotten");
    }

    /// Each window numbers its jobs from 1: the first job of a second window must neither
    /// replace the first window's, nor be cancelled for it, nor forget it when it ends.
    #[test]
    fn windows_numbering_their_jobs_alike_keep_them_apart() {
        let jobs = Jobs::default();
        let extracting = Arc::new(Job::quiet_with_id(1));
        let compressing = Arc::new(Job::quiet_with_id(1));
        let _first = jobs.begin("main", extracting.clone());
        {
            let _second = jobs.begin("main-2", compressing.clone());
            assert!(jobs.cancel("main", 1));
            assert!(extracting.cancelled(), "the first window's job is the one cancelled");
            assert!(!compressing.cancelled(), "the second window's job runs on");
        }
        let again = Arc::new(Job::quiet_with_id(1));
        let _again = jobs.begin("main-3", again.clone());
        assert!(jobs.cancel("main", 1), "the second window's job ending forgets only its own");
    }

    #[test]
    fn a_job_stops_once_nobody_wants_it() {
        let wanted = Arc::new(AtomicBool::new(true));
        let still = wanted.clone();
        let job = Job::quiet_until(move || !still.load(Ordering::Relaxed));
        assert_eq!(job.check(), Ok(()));
        wanted.store(false, Ordering::Relaxed);
        assert_eq!(job.check(), Err(Failure::Cancelled));
    }
}
