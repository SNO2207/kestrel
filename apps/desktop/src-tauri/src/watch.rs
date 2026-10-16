//! Watches the folder on screen and reports when its contents change, so the view refreshes
//! by itself after a download finishes or another program creates a file.

use std::mem::zeroed;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Changes arriving within this long of each other are reported once.
const SETTLE: Duration = Duration::from_millis(150);
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_ATTRIBUTES;

struct Watch {
    handle: HANDLE,
    stopped: AtomicBool,
}

// SAFETY: a directory handle may be used from any thread; all access is through Win32 calls.
unsafe impl Send for Watch {}
unsafe impl Sync for Watch {}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `watch` and is closed exactly once, here.
        unsafe { CloseHandle(self.handle) };
    }
}

/// Watches one folder at a time.
#[derive(Default)]
pub struct Watcher {
    current: Mutex<Option<(String, Arc<Watch>)>>,
}

impl Watcher {
    /// Starts watching `path`, replacing any previous watch. `changed` is called with the path
    /// after each burst of changes.
    ///
    /// The watch is armed before this returns: a change made at any point afterwards is
    /// reported. Callers therefore start watching first and read the folder second, so that
    /// nothing can change unnoticed in between.
    pub fn watch(&self, path: &str, changed: impl Fn(&str) + Send + 'static) {
        let mut current = self.current.lock();
        if current.as_ref().is_some_and(|(watched, _)| watched.eq_ignore_ascii_case(path)) {
            return;
        }
        Self::cancel(&mut current);

        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return;
        }
        let watch = Arc::new(Watch { handle, stopped: AtomicBool::new(false) });
        *current = Some((path.to_owned(), watch.clone()));

        let path = path.to_owned();
        let (armed, wait_for_armed) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("kestrel-watch".into()).spawn(move || {
            // The buffer and the OVERLAPPED block are owned by this thread and outlive every
            // read issued on them, because the loop only exits once no read is pending.
            let mut buffer = [0u64; 1024];
            // SAFETY: OVERLAPPED is plain data for which all-zero is the documented initial state.
            let mut overlapped: OVERLAPPED = unsafe { zeroed() };
            let (buffer_ptr, buffer_len) = (buffer.as_mut_ptr(), size_of_val(&buffer) as u32);
            let overlapped_ptr = &raw mut overlapped;
            let request = || {
                // SAFETY: the buffer and the OVERLAPPED block stay valid until the read
                // completes, and `watch` keeps the handle open for as long as this thread runs.
                unsafe {
                    ReadDirectoryChangesW(
                        watch.handle,
                        buffer_ptr.cast(),
                        buffer_len,
                        0,
                        FILTER,
                        null_mut(),
                        overlapped_ptr,
                        None,
                    ) != 0
                }
            };

            let mut pending = request();
            let _ = armed.send(());
            while pending {
                let mut transferred = 0u32;
                // SAFETY: waits for the read issued above on the same handle and OVERLAPPED.
                let completed = unsafe { GetOverlappedResult(watch.handle, overlapped_ptr, &mut transferred, 1) };
                if completed == 0 || watch.stopped.load(Ordering::Relaxed) {
                    break;
                }
                // One save produces several events. Let the burst finish, then swallow whatever
                // queued up meanwhile by re-arming until a read stays pending, so the burst
                // is reported once and the watch is armed again before the caller re-reads.
                std::thread::sleep(SETTLE);
                loop {
                    pending = request();
                    // SAFETY: polls, without waiting, the read that was just issued.
                    let drained = pending
                        && unsafe { GetOverlappedResult(watch.handle, overlapped_ptr, &mut transferred, 0) } != 0;
                    if !drained {
                        break;
                    }
                }
                if watch.stopped.load(Ordering::Relaxed) {
                    break;
                }
                changed(&path);
            }
        });
        if spawned.is_ok() {
            let _ = wait_for_armed.recv();
        }
    }

    /// Stops watching, as when the second pane closes (`layout.rs`).
    pub fn stop(&self) {
        Self::cancel(&mut self.current.lock());
    }

    fn cancel(current: &mut Option<(String, Arc<Watch>)>) {
        if let Some((_, old)) = current.take() {
            old.stopped.store(true, Ordering::Relaxed);
            // SAFETY: `old` keeps the handle open; this completes its thread's pending read.
            unsafe { CancelIoEx(old.handle, null_mut()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("kestrel-watch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.to_str().unwrap().to_owned();
        (dir, path)
    }

    #[test]
    fn reports_a_change_made_immediately_after_watching() {
        let (dir, path) = scratch("armed");
        let watcher = Watcher::default();
        let (sender, receiver) = mpsc::channel();
        watcher.watch(&path, move |changed| drop(sender.send(changed.to_owned())));

        // No pause here: the watch must already be armed when `watch` returns.
        fs::write(dir.join("new.txt"), b"hello").unwrap();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), path);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stops_reporting_once_replaced() {
        let (first, first_path) = scratch("first");
        let (second, second_path) = scratch("second");
        let watcher = Watcher::default();
        let (sender, receiver) = mpsc::channel();
        let report = move |changed: &str| drop(sender.send(changed.to_owned()));
        watcher.watch(&first_path, report.clone());
        watcher.watch(&second_path, report);

        fs::write(first.join("ignored.txt"), b"x").unwrap();
        fs::write(second.join("seen.txt"), b"x").unwrap();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), second_path);
        assert!(receiver.recv_timeout(Duration::from_millis(500)).is_err(), "the first folder is no longer watched");
        fs::remove_dir_all(&first).unwrap();
        fs::remove_dir_all(&second).unwrap();
    }

    #[test]
    fn stops_reporting_once_stopped() {
        let (dir, path) = scratch("stopped");
        let watcher = Watcher::default();
        let (sender, receiver) = mpsc::channel();
        watcher.watch(&path, move |changed| drop(sender.send(changed.to_owned())));
        watcher.stop();
        fs::write(dir.join("unseen.txt"), b"x").unwrap();
        assert!(receiver.recv_timeout(Duration::from_millis(500)).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
