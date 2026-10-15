//! Which folders this account can't open. With the broker, the index holds every folder on a
//! drive, other accounts' private ones included, so search results can sit in folders the user
//! can't list. The UI asks only about the rows on screen and remembers each answer per folder
//! (`src/lib/access.ts`), so search itself never waits on it; one answer costs one attempt to
//! open the folder, a few tens of microseconds.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// What the UI says about anything in a folder this account can't open.
pub const LOCKED: &str = "Your account can't open this folder";
/// Folders answered per request at most; the UI only asks about rows on screen.
const MAX_PER_REQUEST: usize = 4096;

/// Whether this account is refused when it asks to list `folder`. Anything else that goes
/// wrong, such as the folder no longer existing, is not a lock.
pub fn is_locked(folder: &str) -> bool {
    let wide: Vec<u16> = folder.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; the handle is closed at once.
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            // Required to open a folder; it grants nothing extra without the backup privilege,
            // which ordinary accounts don't hold.
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        CloseHandle(handle);
        false
    }
}

/// For each folder, whether this account can't open it. Folders past the first few thousand
/// are answered `false` rather than checked.
#[tauri::command(async)]
pub fn locked_folders(paths: Vec<String>) -> Vec<bool> {
    paths.iter().enumerate().map(|(at, path)| at < MAX_PER_REQUEST && is_locked(path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{sandbox, text};
    use std::fs;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    /// Replaces a scratch folder's permissions with an empty list, which refuses everyone, or
    /// puts back the ones it inherits from its parent.
    fn lock(path: &std::path::Path, locked: bool) {
        let wide: Vec<u16> = text(path).encode_utf16().chain([0]).collect();
        let mut buffer = [0u32; 16];
        let acl = buffer.as_mut_ptr().cast::<ACL>();
        // SAFETY: the buffer is large and aligned enough for an empty ACL and outlives the call;
        // the path is NUL-terminated.
        unsafe {
            assert_ne!(InitializeAcl(acl, size_of_val(&buffer) as u32, ACL_REVISION), 0);
            let how = if locked { PROTECTED_DACL_SECURITY_INFORMATION } else { UNPROTECTED_DACL_SECURITY_INFORMATION };
            let status = SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | how,
                null_mut(),
                null_mut(),
                acl,
                null(),
            );
            assert_eq!(status, ERROR_SUCCESS);
        }
    }

    #[test]
    fn spots_folders_this_account_cannot_open() {
        let dir = sandbox("access");
        let private = dir.join("private");
        fs::create_dir_all(&private).unwrap();
        fs::write(private.join("inside.txt"), b"x").unwrap();
        assert!(!is_locked(&text(&dir)));
        assert!(!is_locked(&text(&private)));
        assert!(!is_locked(&text(&dir.join("no such folder"))), "a missing folder is not a lock");

        lock(&private, true);
        let checked = locked_folders(vec![text(&dir), text(&private), text(&dir.join("missing"))]);
        let listed = fs::read_dir(&private).is_ok();
        // Restore before asserting, so a failure still leaves a folder that can be removed.
        lock(&private, false);
        assert_eq!(checked, [false, true, false], "an empty permission list refuses this account");
        assert!(!listed);
        assert!(!is_locked(&text(&private)), "and opens again once permissions are back");
        fs::remove_dir_all(&dir).unwrap();
    }
}
