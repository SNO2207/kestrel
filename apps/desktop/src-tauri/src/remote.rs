//! Questions to the disk that may get no answer in time.
//!
//! A path on a share whose server is away (off the VPN, a NAS asleep), or on a mapped drive to
//! one, takes about 21 seconds to say so, one path after another, and again once Windows has
//! forgotten the failure. Anything the window waits on asks such questions through [`ask_all`]:
//! each drive or share gets a thread of its own, which asks about its paths in turn; whatever
//! hasn't answered by the time limit counts as unanswered, and the caller decides what that
//! means. A drive or share whose thread has been waiting longer than a call waits is not asked
//! again until it answers, so calls made every few seconds don't pile up threads behind it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// The threads asking now: each one's number, its drive or share, and when it started.
static BUSY: Mutex<Vec<(u64, String, Instant)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The drive or share `path` is on, folded: `c:`, `\\server\share`.
pub fn volume_of(path: &str) -> String {
    let path = path.replace('/', "\\");
    let rest = path.strip_prefix(r"\\?\UNC\").map(|rest| format!(r"\\{rest}"));
    let path = rest.unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned());
    if let Some(unc) = path.strip_prefix(r"\\") {
        let mut parts = unc.split('\\').filter(|part| !part.is_empty());
        let (server, share) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        return format!(r"\\{server}\{share}").to_lowercase();
    }
    path.get(..2).unwrap_or(&path).to_lowercase()
}

/// Whether `path` is on a share, or on a drive letter mapped to one: a question about it may
/// take half a minute to fail.
pub fn is_remote(path: &str) -> bool {
    let volume = volume_of(path);
    if volume.starts_with(r"\\") {
        return true;
    }
    let bytes = volume.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    const DRIVE_REMOTE: u32 = 4;
    let root: Vec<u16> = format!(r"{volume}\").encode_utf16().chain([0]).collect();
    // SAFETY: `root` is NUL-terminated and outlives the call, which only reads it. Asking a
    // mapped drive's type needs no answer from its server.
    unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

/// Takes a thread off [`BUSY`] when it ends, however it ends.
struct Answered(u64);

impl Drop for Answered {
    fn drop(&mut self) {
        BUSY.lock().retain(|(thread, _, _)| *thread != self.0);
    }
}

/// Asks `probe` about each of `paths`, the paths of one drive or share one after another on a
/// thread of their own, and waits at most `wait` for all the answers, which come back in the
/// order of `paths`: `None` for one not answered in time, or not asked because a thread has
/// been waiting on its drive or share for longer than `wait` already.
pub fn ask_all<T: Send + 'static>(paths: &[String], probe: fn(&str) -> T, wait: Duration) -> Vec<Option<T>> {
    let mut answers: Vec<Option<T>> = paths.iter().map(|_| None).collect();
    let mut by_volume: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (at, path) in paths.iter().enumerate() {
        by_volume.entry(volume_of(path)).or_default().push((at, path.clone()));
    }
    let (sender, receiver) = mpsc::channel::<(usize, T)>();
    let mut expected = 0;
    for (volume, asked) in by_volume {
        let thread = NEXT.fetch_add(1, Ordering::Relaxed);
        {
            let mut busy = BUSY.lock();
            if busy.iter().any(|(_, waiting, since)| *waiting == volume && since.elapsed() > wait) {
                continue;
            }
            busy.push((thread, volume, Instant::now()));
        }
        let count = asked.len();
        let sender = sender.clone();
        let answered = Answered(thread);
        let spawned = std::thread::Builder::new().name("kestrel-remote".into()).spawn(move || {
            let _answered = answered;
            for (at, path) in asked {
                // The caller has stopped waiting: the rest are not asked.
                if sender.send((at, probe(&path))).is_err() {
                    break;
                }
            }
        });
        if spawned.is_ok() {
            expected += count;
        }
    }
    drop(sender);
    let deadline = Instant::now() + wait;
    for _ in 0..expected {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok((at, answer)) => answers[at] = Some(answer),
            Err(_) => break,
        }
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_grouped_by_their_drive_or_share() {
        assert_eq!(volume_of(r"C:\Users\me\a.txt"), "c:");
        assert_eq!(volume_of(r"\\NAS\Projects\x\y.docx"), r"\\nas\projects");
        assert_eq!(volume_of(r"\\?\UNC\nas\projects\x"), r"\\nas\projects");
        assert_eq!(volume_of(r"\\?\C:\x"), "c:");
        assert!(is_remote(r"\\nas\projects\x"));
        assert!(!is_remote(r"C:\Windows"));
    }

    /// A probe that, for paths on `\\away`, takes as long as a share whose server is away.
    fn slow_on_away(path: &str) -> bool {
        if path.starts_with(r"\\away") {
            std::thread::sleep(Duration::from_secs(3));
        }
        true
    }

    /// A share that doesn't answer holds nothing else up: other drives' answers come back, the
    /// caller waits only as long as it said, and the share isn't asked again while it is still
    /// waiting.
    #[test]
    fn a_share_that_doesnt_answer_holds_nothing_up() {
        let paths: Vec<String> =
            [r"\\away\share\one.txt", r"C:\Windows", r"\\away\share\two.txt", r"\\near\share\three.txt"].map(String::from).to_vec();
        let started = Instant::now();
        let answers = ask_all(&paths, slow_on_away, Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(answers, [None, Some(true), None, Some(true)]);
        let again = Instant::now();
        let answers = ask_all(&paths[..2], slow_on_away, Duration::from_millis(250));
        assert!(again.elapsed() < Duration::from_millis(200), "not asked again while still waiting");
        assert_eq!(answers, [None, Some(true)], "a drive that answers is asked as ever");
    }
}
