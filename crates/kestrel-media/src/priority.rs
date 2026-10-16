//! Background mode for the media pass's heavy work, and only for that.
//!
//! `THREAD_MODE_BACKGROUND_BEGIN` lowers a thread's CPU, I/O and memory priority, so decoding
//! thousands of photos gives way to whatever the owner is doing. But a thread in background
//! mode must never hold a lock another thread waits for: while every core is busy (a build, a
//! video export) Windows lets it starve for seconds with the lock in hand, and the thread
//! waiting on that lock starves with it. parking_lot's locks are fair, so a writer queued behind
//! it holds up every new reader too: a scan holding the index's read lock once froze every
//! keystroke search for two seconds at a time this way.
//!
//! So the pass's threads run at normal priority, and the heavy work that holds no lock anyone
//! else waits for (reading a file's properties, decoding it, recognising its text, preparing a
//! picture for a model, rewriting a vector store's files) runs inside [`background`]. Anything
//! that takes a shared lock (the database, the pipeline's state, the index, a vector store, the
//! thumbnail cache) is done outside it.

use windows_sys::Win32::Foundation::{ERROR_THREAD_MODE_ALREADY_BACKGROUND, GetLastError};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
};

/// Background mode until dropped, unless the thread was in it already.
struct Background {
    entered: bool,
}

impl Background {
    fn enter() -> Self {
        // SAFETY: changes only this thread's own priority. It fails on a thread already in
        // background mode, which then stays in it when this is dropped.
        let entered = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) } != 0;
        Background { entered }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: ends the background mode this value began, on the same thread (it is
            // never sent elsewhere: `background` keeps it on its own stack).
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
        }
    }
}

/// Runs `work` in background mode, then goes back to the thread's own priority (also when it
/// panics). `work` must not take a lock another thread may be waiting for; see the module
/// documentation.
pub fn background<T>(work: impl FnOnce() -> T) -> T {
    let _mode = Background::enter();
    work()
}

/// Whether this thread is in background mode now: for tests and checks.
pub fn in_background() -> bool {
    // SAFETY: changes only this thread's own priority, and puts it back at once.
    unsafe {
        if SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) != 0 {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END);
            return false;
        }
        GetLastError() == ERROR_THREAD_MODE_ALREADY_BACKGROUND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_mode_lasts_as_long_as_the_work() {
        assert!(!in_background());
        let inside = background(|| {
            // Nested: still in it afterwards, until the outer work ends.
            let nested = background(in_background);
            (nested, in_background())
        });
        assert_eq!(inside, (true, true));
        assert!(!in_background());
        let caught = std::panic::catch_unwind(|| background(|| panic!("work failed")));
        assert!(caught.is_err());
        assert!(!in_background(), "a panic leaves background mode too");
    }
}
