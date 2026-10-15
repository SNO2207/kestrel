//! One Kestrel per data folder.
//!
//! With "keep running in the tray" and "start with Windows", Kestrel is usually already
//! running, hidden, when someone clicks its Start menu or taskbar entry. A second process on
//! the same data folder would share `kestrel.db` and the index snapshot with the first: both
//! number undo entries from the same id and overwrite each other's, both may undo the same
//! operation, and their snapshot saves interleave. So the first process claims the folder with
//! a named mutex, and a later one asks it to show its window and exits.
//!
//! The names are derived from the data folder, so test instances, which each have their own
//! (`KESTREL_DATA_DIR`), still run side by side.

use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

/// How long a second instance waits for the first to show its window. The first answers from
/// a thread of its own once it has started, which takes a moment if it has only just been
/// launched itself.
const SHOW_WAIT: Duration = Duration::from_secs(5);
/// How long it then waits for an instance that did not answer to finish exiting (saving the
/// index on the way out takes a moment) before taking its place.
const EXIT_WAIT: Duration = Duration::from_secs(15);

/// What [`claim`] found.
pub enum Claim {
    /// No other instance has the data folder: this one runs, holding the claim until it exits.
    First(Instance),
    /// Another instance has it, and was asked to show its window (unless this one was started
    /// in the background): this one should exit at once.
    Running,
}

/// The claim on a data folder, and the event through which later instances ask for the window.
pub struct Instance {
    mutex: Handle,
    show: Handle,
    shown: Handle,
}

/// A kernel object handle, closed when dropped.
struct Handle(HANDLE);

// SAFETY: kernel object handles may be used and closed from any thread.
unsafe impl Send for Handle {}

impl Handle {
    /// Signals the event this handle is for.
    fn signal(&self) {
        // SAFETY: the handle is a valid event handle for the duration of the call.
        unsafe { SetEvent(self.0) };
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned by a Create function and is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// `Local\Kestrel-<hash of the data folder>`, plus `suffix`. A kernel object name may not
/// hold a backslash beyond the namespace, so the folder goes in as a hash.
fn object_name(data_dir: &Path, suffix: &str) -> Vec<u16> {
    // FNV-1a: stable across runs and builds, unlike the standard library's hasher.
    let hash = data_dir
        .to_string_lossy()
        .to_lowercase()
        .trim_end_matches(['\\', '/'])
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!(r"Local\Kestrel-{hash:016x}{suffix}").encode_utf16().chain([0]).collect()
}

/// An auto-reset event, created or opened by name.
fn event(name: &[u16]) -> Handle {
    // SAFETY: `name` is NUL-terminated and outlives the call.
    Handle(unsafe { CreateEventW(null(), 0, 0, name.as_ptr()) })
}

/// Waits up to `millis` (or [`INFINITE`]) for `handle`; true if it was signalled (or, for a
/// mutex, left behind by a thread that ended without releasing it).
fn wait(handle: &Handle, millis: u32) -> bool {
    // SAFETY: `handle` is a valid waitable handle for the duration of the call.
    let result = unsafe { WaitForSingleObject(handle.0, millis) };
    result == WAIT_OBJECT_0 || result == WAIT_ABANDONED
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(INFINITE - 1)
}

/// Claims `data_dir` for this process. If another instance has it, asks that one to show its
/// window when `show` is set, and returns [`Claim::Running`] once it has; if it does not answer
/// (it is exiting), waits for it to end and claims the folder after all.
pub fn claim(data_dir: &Path, show: bool) -> Claim {
    let name = object_name(data_dir, "");
    // SAFETY: `name` is NUL-terminated and outlives the call; ownership is requested so that a
    // later instance can wait for this one to end.
    let mutex = Handle(unsafe { CreateMutexW(null(), 1, name.as_ptr()) });
    // SAFETY: plain FFI call with no arguments, right after the call whose error it reads.
    let error = unsafe { GetLastError() };
    if mutex.0.is_null() {
        // Without the mutex nothing can be told; never stop the app from starting over it.
        eprintln!("could not check for another Kestrel: error {error}");
        return Claim::First(Instance { mutex, show: Handle(null_mut()), shown: Handle(null_mut()) });
    }
    let requests = event(&object_name(data_dir, "-show"));
    let answers = event(&object_name(data_dir, "-shown"));
    if error != ERROR_ALREADY_EXISTS {
        return Claim::First(Instance { mutex, show: requests, shown: answers });
    }
    if !show {
        return Claim::Running;
    }
    // SAFETY: plain FFI call. The first instance is a background process; this one was just
    // started by the user, so it may hand on its right to bring a window to the front.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    requests.signal();
    if wait(&answers, millis(SHOW_WAIT)) {
        return Claim::Running;
    }
    if wait(&mutex, millis(EXIT_WAIT)) {
        // The request this instance made is still pending, and shows its own window: fine.
        return Claim::First(Instance { mutex, show: requests, shown: answers });
    }
    eprintln!("another Kestrel is running but did not answer");
    Claim::Running
}

impl Instance {
    /// Calls `show` whenever a later instance asks for the window, from a thread of its own,
    /// for as long as the process runs. The claim lasts as long, whatever becomes of `self`.
    pub fn listen(self, show: impl Fn() + Send + 'static) {
        let Instance { mutex, show: requests, shown } = self;
        // Never closed: the mutex stays owned by the thread that created it until the process
        // ends.
        std::mem::forget(mutex);
        if requests.0.is_null() || shown.0.is_null() {
            return;
        }
        let _ = std::thread::Builder::new().name("kestrel-instance".into()).spawn(move || {
            while wait(&requests, INFINITE) {
                show();
                shown.signal();
            }
        });
    }

    /// Gives up the claim, as exiting would. Tests only: a running app keeps it until it ends.
    #[cfg(test)]
    fn release(self) {
        // SAFETY: called on the thread that created, and so owns, the mutex.
        unsafe { windows_sys::Win32::System::Threading::ReleaseMutex(self.mutex.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kestrel-instance-{name}-{}", std::process::id()))
    }

    /// The tray case: Kestrel is running hidden and its shortcut is clicked. The new process
    /// must not run beside it; the running one shows its window instead.
    #[test]
    fn a_second_instance_shows_the_first_and_exits() {
        let dir = scratch("second");
        let Claim::First(instance) = claim(&dir, true) else { panic!("nothing else has this folder") };
        let shown = Arc::new(AtomicUsize::new(0));
        let counter = shown.clone();
        // Keep the claim on this thread (a mutex is owned by a thread) and listen elsewhere.
        let Instance { mutex, show, shown: answer } = instance;
        Instance { mutex: Handle(null_mut()), show, shown: answer }.listen(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let second = std::thread::spawn({
            let dir = dir.clone();
            move || matches!(claim(&dir, true), Claim::Running)
        });
        assert!(second.join().unwrap(), "the second instance stands down");
        assert_eq!(shown.load(Ordering::SeqCst), 1, "and the first was asked to show its window");

        // Started at sign-in (in the background), it stands down without a word.
        let quiet = std::thread::spawn({
            let dir = dir.clone();
            move || matches!(claim(&dir, false), Claim::Running)
        });
        assert!(quiet.join().unwrap());
        assert_eq!(shown.load(Ordering::SeqCst), 1);

        // Another data folder (another test slot) is no concern of this one's.
        let other = std::thread::spawn(|| matches!(claim(&scratch("other"), true), Claim::First(_)));
        assert!(other.join().unwrap());
        // SAFETY: this thread created and owns the mutex.
        unsafe { windows_sys::Win32::System::Threading::ReleaseMutex(mutex.0) };
    }

    /// An instance that is exiting does not answer; the new one waits for it to end, then runs.
    #[test]
    fn an_instance_that_does_not_answer_is_waited_for() {
        let dir = scratch("exiting");
        let Claim::First(instance) = claim(&dir, true) else { panic!("nothing else has this folder") };
        let waiting = std::thread::spawn({
            let dir = dir.clone();
            move || {
                let started = std::time::Instant::now();
                let first = matches!(claim(&dir, true), Claim::First(_));
                (first, started.elapsed())
            }
        });
        // It never listens; it ends after the second has given up waiting for an answer.
        std::thread::sleep(SHOW_WAIT + Duration::from_millis(500));
        instance.release();
        let (first, waited) = waiting.join().unwrap();
        assert!(first, "the new instance runs once the old one has gone");
        assert!(waited >= SHOW_WAIT && waited < SHOW_WAIT + EXIT_WAIT, "{waited:?}");
    }

    #[test]
    fn names_follow_the_data_folder() {
        assert_eq!(object_name(Path::new(r"C:\Data\Kestrel"), ""), object_name(Path::new(r"c:\data\kestrel\"), ""));
        assert_ne!(object_name(Path::new(r"C:\Data\Kestrel"), ""), object_name(Path::new(r"C:\Data\Slot7"), ""));
        let name = String::from_utf16(&object_name(Path::new(r"C:\Data\Kestrel"), "-show")).unwrap();
        assert!(name.starts_with(r"Local\Kestrel-") && name.ends_with("-show\0"), "{name}");
        assert_eq!(name.matches('\\').count(), 1, "only the namespace's separator");
    }
}
