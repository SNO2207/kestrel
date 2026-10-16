//! Whether an item can be deleted now, asked before anything is sent to the Recycle Bin, so an
//! item a program still has open is skipped with a note rather than stopping the whole
//! operation on the shell's "file in use" prompt.

use serde::Serialize;

/// Why an item can't be deleted now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocker {
    /// A program has it (or something inside it) open.
    InUse,
    /// This account may not delete it.
    Denied,
    /// It is no longer there.
    Gone,
}

impl Blocker {
    pub fn describe(self) -> &'static str {
        match self {
            Blocker::InUse => "a program has it open",
            Blocker::Denied => "your account can't delete it",
            Blocker::Gone => "it's no longer there",
        }
    }
}

#[cfg(windows)]
mod real {
    use std::ptr::{null, null_mut};

    use kestrel_index::walk;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION, ERROR_PATH_NOT_FOUND,
        ERROR_SHARING_VIOLATION, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    use super::Blocker;

    /// The extended-length form of a path, which lifts the 260-character limit.
    fn extended(path: &str) -> Vec<u16> {
        let path = path.replace('/', "\\");
        let text = if path.starts_with(r"\\?\") {
            path
        } else if let Some(share) = path.strip_prefix(r"\\") {
            format!(r"\\?\UNC\{share}")
        } else {
            format!(r"\\?\{path}")
        };
        text.encode_utf16().chain([0]).collect()
    }

    /// Opens the item itself (never what a link leads to) for deletion, as deleting it would,
    /// and closes it again at once. A program holding it open without letting others delete it
    /// makes this fail with a sharing violation.
    fn try_item(path: &str) -> Option<Blocker> {
        let wide = extended(path);
        // SAFETY: `wide` is NUL-terminated and outlives the call; the handle is closed at once.
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            );
            if handle != INVALID_HANDLE_VALUE {
                CloseHandle(handle);
                return None;
            }
            match GetLastError() {
                ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION => Some(Blocker::InUse),
                ERROR_ACCESS_DENIED => Some(Blocker::Denied),
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Some(Blocker::Gone),
                _ => None,
            }
        }
    }

    /// What stops the item at `path` from being deleted now, if anything: for a folder, the
    /// first thing inside it that would, looking at no more than `limit` items (beyond that the
    /// shell's own prompt is left to say).
    pub fn blocker(path: &str, limit: usize) -> Option<Blocker> {
        let path = path.trim_end_matches('\\');
        let blocked = try_item(path);
        if blocked.is_some() || !std::path::Path::new(path).is_dir() || walk::is_alias(path) {
            return blocked;
        }
        let mut seen = 0;
        let mut folders = vec![path.to_owned()];
        while let Some(folder) = folders.pop() {
            let Ok(items) = walk::list(&folder) else { continue };
            for item in items {
                seen += 1;
                if seen > limit {
                    return None;
                }
                let child = format!(r"{folder}\{}", item.name);
                // Something inside that went meanwhile doesn't stop the rest.
                if let Some(blocked) = try_item(&child).filter(|b| *b != Blocker::Gone) {
                    return Some(blocked);
                }
                if item.meta.attrs & kestrel_index::attr::DIRECTORY != 0 && !item.alias {
                    folders.push(child);
                }
            }
        }
        None
    }
}

#[cfg(windows)]
pub use real::blocker;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::testing::Sandbox;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn items_held_open_are_in_use() {
        let sandbox = Sandbox::new("inuse");
        let file = sandbox.write(r"folder\deep\held.log", 100, 1);
        sandbox.write(r"folder\free.txt", 100, 2);
        let free = sandbox.write("free.tmp", 100, 3);
        let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
        assert_eq!(blocker(&text(&free), 100), None);
        {
            // Opened as editors and loggers do: others may read, but not delete.
            const FILE_SHARE_READ: u32 = 1;
            let _held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&file).unwrap();
            assert_eq!(blocker(&text(&file), 100), Some(Blocker::InUse));
            assert_eq!(blocker(&sandbox.text("folder"), 100), Some(Blocker::InUse));
            // Looking no further than the limit, the folder isn't known to be in use.
            assert_eq!(blocker(&sandbox.text("folder"), 1), None);
        }
        assert_eq!(blocker(&sandbox.text("folder"), 100), None);
        assert_eq!(blocker(&sandbox.text("missing.tmp"), 100), Some(Blocker::Gone));
    }
}
