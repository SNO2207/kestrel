//! The Recycle Bin page: everything in the bin of every drive, read through the shell's
//! Recycle Bin folder with its detail columns (where each item was deleted from, when, its size
//! and type), and the page's commands.
//!
//! Every command takes the items it acts on by identity: the path of the item's own file in the
//! bin (`C:\$Recycle.Bin\<account>\$R…`), unique to it, which the listing hands out. Restoring
//! moves each item back with the shell's copy engine (`fileops`, with its conflict dialog), into
//! the folder it came from, made again if it has gone as Explorer does, and goes into the undo
//! history like any other operation. Deleting permanently asks the shell to delete those
//! items, after the page's own confirmation. Only emptying the bin acts on everything, and it
//! has no test hook: tests restore and delete only items they recycled themselves.
//!
//! The page follows the bin by asking for a stamp every couple of seconds: the modification
//! times of this account's bin folder on each drive, which change whenever an item enters or
//! leaves, and cost one file system call per drive.

use std::collections::HashMap;
use std::ptr::null_mut;

use serde::Serialize;
use tauri::{State, Window};
use windows::Win32::Foundation::{HWND, PROPERTYKEY};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FileOperation, IFileOperation, IShellItem,
    IShellItem2, SHERB_NOCONFIRMATION, SHEmptyRecycleBinW, SIGDN_FILESYSPATH,
};
use windows::core::{GUID, Interface, PCWSTR};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, GetDriveTypeW, GetFileAttributesExW, GetFileAttributesW, GetFileExInfoStandard,
    GetLogicalDrives, INVALID_FILE_ATTRIBUTES, WIN32_FILE_ATTRIBUTE_DATA,
};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

use crate::fileops::{self, Done, Op, Options, Outcome};
use crate::recycle;
use crate::undo::Journal;

/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const SHELL_DETAILS: GUID = GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac);
/// `System.Size`: for an item in the bin, the size it had (a folder's whole contents).
const SIZE: PROPERTYKEY = PROPERTYKEY { fmtid: SHELL_DETAILS, pid: 12 };
/// `System.ItemTypeText`, such as "Text Document".
const TYPE_TEXT: PROPERTYKEY = PROPERTYKEY { fmtid: SHELL_DETAILS, pid: 4 };

/// An item in the Recycle Bin, as the page lists it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BinItem {
    /// The item's own file in the bin, which identifies it to every command.
    pub id: String,
    /// The name it had.
    pub name: String,
    /// The full path it was deleted from.
    pub original: String,
    /// The folder it was deleted from.
    pub folder: String,
    /// When it was deleted, in milliseconds since the Unix epoch.
    pub deleted: i64,
    pub size: u64,
    /// The type Windows gives it, such as "Text Document" or "File folder".
    pub type_name: String,
    pub dir: bool,
    /// The extension, lower case without the dot; empty for folders.
    pub ext: String,
}

/// What a restore did.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Restored {
    /// Where the items put back now are.
    pub paths: Vec<String>,
    /// Items asked for that were no longer in the bin.
    pub missing: usize,
    /// Nothing was cancelled or skipped.
    pub completed: bool,
}

/// What a permanent deletion did.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Erased {
    pub erased: usize,
    /// Items still in the bin afterwards: cancelled, refused, or gone before.
    pub left: usize,
}

/// Runs `work` on a thread of its own with COM initialised as a single-threaded apartment, as
/// the shell's folders and its copy engine expect.
fn in_apartment<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    std::thread::Builder::new()
        .name("kestrel-recycle-bin".into())
        .spawn(move || {
            // SAFETY: COM is initialised for this thread and uninitialised after `work`, which
            // releases every interface it obtains before returning.
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(|e| e.message())?;
                let result = work();
                CoUninitialize();
                result
            }
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "Reading the Recycle Bin failed unexpectedly".to_owned())?
}

/// Copies and frees a string the shell allocated.
///
/// # Safety
/// `text` must have been allocated with `CoTaskMemAlloc` and be freed by nothing else.
unsafe fn take(text: windows::core::PWSTR) -> Option<String> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let copy = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        copy
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Whether the item's file in the bin is a folder, read from its attributes without opening it
/// (an item may be a cloud file's placeholder, which opening would download).
fn is_folder(path: &str) -> bool {
    // SAFETY: the path is NUL-terminated.
    let attributes = unsafe { GetFileAttributesW(wide(path).as_ptr()) };
    attributes != INVALID_FILE_ATTRIBUTES && attributes & FILE_ATTRIBUTE_DIRECTORY != 0
}

/// The extension of a file name, as rows carry it: lower case, without the dot, and none for
/// names that only start with one.
fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The size a bin record (`$I…`) gives its item, for when the shell gives none.
fn recorded_size(id: &str) -> Option<u64> {
    let (folder, name) = id.rsplit_once('\\')?;
    let record = format!("{folder}\\$I{}", name.strip_prefix("$R")?);
    let bytes = std::fs::read(record).ok()?;
    Some(u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?))
}

/// An entry of the bin, with the identity and details the page needs. `None` for an item with
/// no file of its own, which no command could name.
///
/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn describe(entry: &recycle::Entry) -> Option<BinItem> {
    // SAFETY: COM is initialised (guaranteed by the caller); returned strings are freed by `take`.
    unsafe {
        let id = entry.item.GetDisplayName(SIGDN_FILESYSPATH).ok().and_then(|text| take(text))?;
        let details = entry.item.cast::<IShellItem2>().ok();
        let name = fileops::file_name(&entry.original).to_owned();
        let dir = is_folder(&id);
        let size = details
            .as_ref()
            .and_then(|details| details.GetUInt64(&SIZE).ok())
            .or_else(|| recorded_size(&id))
            .unwrap_or(0);
        let type_name = details
            .as_ref()
            .and_then(|details| details.GetString(&TYPE_TEXT).ok())
            .and_then(|text| take(text))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| if dir { "File folder".into() } else { "File".into() });
        Some(BinItem {
            folder: fileops::parent(&entry.original),
            ext: if dir { String::new() } else { extension(&name) },
            name,
            original: entry.original.clone(),
            deleted: (entry.deleted - FILETIME_UNIX_EPOCH) / 10_000,
            size,
            type_name,
            dir,
            id,
        })
    }
}

/// Every item in the Recycle Bin of every drive, with the shell entry each came from.
///
/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn read() -> Result<Vec<(BinItem, recycle::Entry)>, String> {
    // SAFETY: guaranteed by the caller.
    let entries = unsafe { recycle::entries() }.map_err(|e| format!("Couldn't read the Recycle Bin: {}", e.message()))?;
    // SAFETY: as above.
    Ok(entries.into_iter().filter_map(|entry| Some((unsafe { describe(&entry) }?, entry))).collect())
}

/// Every item in the Recycle Bin, newest first.
pub fn items() -> Result<Vec<BinItem>, String> {
    in_apartment(|| {
        // SAFETY: runs in the apartment `in_apartment` set up.
        let mut items: Vec<BinItem> = unsafe { read() }?.into_iter().map(|(item, _)| item).collect();
        items.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.original.cmp(&b.original)));
        Ok(items)
    })
}

/// How paths are compared.
fn key(path: &str) -> String {
    fileops::key(path)
}

/// Puts the items with these identities back where they were deleted from. Folders that have
/// gone since are made again first, as Explorer does; an item already back, or emptied out of
/// the bin, counts as missing. Name conflicts are the shell's to ask about.
pub fn restore(ids: &[String], options: Options) -> Result<(Restored, Outcome), String> {
    let wanted: Vec<String> = ids.iter().map(|id| key(id)).collect();
    let found: Vec<(String, i64)> = in_apartment(move || {
        // SAFETY: runs in the apartment `in_apartment` set up.
        let listed = unsafe { read() }?;
        let by_id: HashMap<String, (String, i64)> =
            listed.into_iter().map(|(item, entry)| (key(&item.id), (item.original, entry.deleted))).collect();
        Ok(wanted.iter().filter_map(|id| by_id.get(id).cloned()).collect())
    })?;
    let mut missing = ids.len() - found.len();
    for (original, _) in &found {
        let folder = fileops::parent(original);
        std::fs::create_dir_all(&folder).map_err(|e| {
            format!("{} can't be put back: its folder {folder} can't be made again ({e}).", fileops::file_name(original))
        })?;
    }
    // A window of one instant: the very item listed, not another deleted from the same place.
    let ops: Vec<Op> = found
        .iter()
        .map(|(original, deleted)| Op::Restore { path: original.clone(), deleted: fileops::Window { from: *deleted, to: *deleted } })
        .collect();
    let outcome = fileops::run(ops, false, options)?;
    missing += outcome.missing.len();
    let paths = outcome
        .results()
        .filter_map(|done| match done {
            Done::Restored { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let completed = outcome.completed && missing == 0;
    Ok((Restored { paths, missing, completed }, outcome))
}

/// Deletes the items with these identities from the bin for good, with the shell's progress
/// window but without its own question: the page has asked already.
pub fn erase(ids: &[String], options: Options) -> Result<Erased, String> {
    let wanted: Vec<String> = ids.iter().map(|id| key(id)).collect();
    in_apartment(move || {
        // SAFETY: runs in the apartment `in_apartment` set up; every interface is released
        // before the closure returns.
        unsafe {
            let listed = read()?;
            let chosen: Vec<&(BinItem, recycle::Entry)> = listed.iter().filter(|(item, _)| wanted.contains(&key(&item.id))).collect();
            if !chosen.is_empty() {
                let describe = |e: windows::core::Error| e.message();
                let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(describe)?;
                let mut flags: FILEOPERATION_FLAGS = FOF_NOCONFIRMATION;
                if options.quiet {
                    flags |= FOF_SILENT | FOF_NOERRORUI;
                }
                operation.SetOperationFlags(flags).map_err(describe)?;
                if let Some(owner) = options.owner {
                    operation.SetOwnerWindow(HWND(owner as _)).map_err(describe)?;
                }
                for (_, entry) in &chosen {
                    let item: &IShellItem = &entry.item;
                    operation.DeleteItem(item, None).map_err(describe)?;
                }
                // Cancelling or a refusal shows in what is left, counted below.
                let _ = operation.PerformOperations();
            }
            let still: Vec<String> = read()?.into_iter().map(|(item, _)| key(&item.id)).collect();
            let left = chosen.iter().filter(|(item, _)| still.contains(&key(&item.id))).count();
            // The bin keeps a record of each item beside it; one whose item has gone is
            // removed, so it can't linger unseen.
            for (item, _) in &chosen {
                if !still.contains(&key(&item.id)) {
                    recycle::forget(&item.id, &item.original);
                }
            }
            Ok(Erased { erased: chosen.len() - left, left: left + (wanted.len() - chosen.len()) })
        }
    })
}

/// The string SID of the account Kestrel runs as, which names its folder in each drive's bin.
fn account_sid() -> Option<String> {
    // SAFETY: the token is closed before returning; the buffer is sized by the first call, and
    // the SID string is freed with LocalFree as documented.
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed);
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = null_mut::<u16>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return None;
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Some(sid)
    }
}

/// Changes whenever an item enters or leaves this account's Recycle Bin on any local drive:
/// each bin folder's modification time, which Windows updates as files come and go in it.
pub fn stamp() -> String {
    static SID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let Some(sid) = SID.get_or_init(account_sid) else { return String::new() };
    // SAFETY: plain FFI calls; the error mode keeps Windows from asking for a disk.
    let mask = unsafe {
        SetThreadErrorMode(SEM_FAILCRITICALERRORS, null_mut());
        GetLogicalDrives()
    };
    // Only local drives have a bin; their letters and kinds are known without asking them.
    (0..26u8)
        .filter(|bit| mask >> bit & 1 != 0)
        .map(|bit| format!("{}:\\", (b'A' + bit) as char))
        .filter(|root| {
            // SAFETY: the root is NUL-terminated.
            let kind = unsafe { GetDriveTypeW(wide(root).as_ptr()) };
            kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE
        })
        .map(|root| {
            let folder = wide(&format!("{root}$Recycle.Bin\\{sid}"));
            // SAFETY: the path is NUL-terminated and `data` is the structure the call fills.
            let modified = unsafe {
                let mut data: WIN32_FILE_ATTRIBUTE_DATA = std::mem::zeroed();
                let found = GetFileAttributesExW(folder.as_ptr(), GetFileExInfoStandard, (&mut data as *mut WIN32_FILE_ATTRIBUTE_DATA).cast());
                (found != 0).then_some(((data.ftLastWriteTime.dwHighDateTime as u64) << 32) | data.ftLastWriteTime.dwLowDateTime as u64)
            };
            format!("{root}{}", modified.map_or("-".into(), |time| time.to_string()))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Every item in the Recycle Bin of every drive.
#[tauri::command(async)]
pub fn recycle_bin_items() -> Result<Vec<BinItem>, String> {
    items()
}

/// A value that changes whenever the Recycle Bin does.
#[tauri::command(async)]
pub fn recycle_bin_stamp() -> String {
    stamp()
}

/// Restores the items with these identities, as one undoable operation.
#[tauri::command(async)]
pub fn recycle_bin_restore(window: Window, journal: State<Journal>, ids: Vec<String>) -> Result<Restored, String> {
    if ids.is_empty() {
        return Ok(Restored { completed: true, ..Restored::default() });
    }
    let (restored, outcome) = restore(&ids, crate::interactive(&window))?;
    journal.record(&outcome);
    Ok(restored)
}

/// Deletes the items with these identities from the Recycle Bin for good. The page asks first.
#[tauri::command(async)]
pub fn recycle_bin_delete(window: Window, ids: Vec<String>) -> Result<Erased, String> {
    if ids.is_empty() {
        return Ok(Erased::default());
    }
    erase(&ids, crate::interactive(&window))
}

/// Empties the Recycle Bin of every drive, with the shell's progress window. The page asks
/// first. Deliberately without any test hook.
#[tauri::command(async)]
pub fn recycle_bin_empty(window: Window) -> Result<(), String> {
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    in_apartment(move || {
        // SAFETY: plain shell call on a COM-initialised thread; the owner is the app's window.
        let emptied = unsafe { SHEmptyRecycleBinW(owner.map(|owner| HWND(owner as _)), PCWSTR::null(), SHERB_NOCONFIRMATION) };
        // An empty bin answers with an error; what counts is that nothing is left.
        // SAFETY: runs in the apartment `in_apartment` set up.
        match emptied {
            Ok(()) => Ok(()),
            Err(_) if unsafe { recycle::entries() }.is_ok_and(|left| left.is_empty()) => Ok(()),
            Err(error) => Err(format!("Couldn't empty the Recycle Bin: {}", error.message())),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{QUIET, sandbox, text};
    use crate::fileops::{Request, execute};
    use std::fs;

    #[test]
    fn extensions_are_read_as_rows_carry_them() {
        assert_eq!(extension("Report.PDF"), "pdf");
        assert_eq!(extension(".gitignore"), "");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("README"), "");
    }

    /// The items of the bin that tests running alongside can't have put there or taken out:
    /// those deleted from anywhere but the temp folder.
    fn owners_items() -> Vec<String> {
        let temp = key(&text(&std::env::temp_dir()));
        let mut ids: Vec<String> =
            items().unwrap().into_iter().filter(|item| !key(&item.original).starts_with(&temp)).map(|item| item.id).collect();
        ids.sort();
        ids
    }

    /// Sends two tiny files from a scratch folder to the real Recycle Bin, finds them in the
    /// listing by where they were deleted from, restores one and deletes the other for good,
    /// so the bin ends as it began; the owner's own items are counted before and after. The
    /// files are named `kestrel-bin-test-*`, apart from the undo tests' `kestrel-test-*`, which
    /// count what of theirs is in the bin while these run.
    #[test]
    fn lists_restores_and_erases_items_it_recycled_itself() {
        let dir = sandbox("recycle-bin-page");
        let pid = std::process::id();
        let kept = dir.join(format!("kestrel-bin-test-{pid}-kept.txt"));
        let gone = dir.join(format!("kestrel-bin-test-{pid}-gone.log"));
        fs::write(&kept, b"kept").unwrap();
        fs::write(&gone, b"gone for good").unwrap();
        let before = owners_items();
        let stamp_before = stamp();

        let recycled = execute(Request::Delete { paths: vec![text(&kept), text(&gone)], permanent: false }, QUIET).unwrap();
        assert_eq!(recycled.results().filter(|done| matches!(done, Done::Recycled { .. })).count(), 2, "{recycled:?}");
        assert!(!kept.exists() && !gone.exists());
        assert_ne!(stamp(), stamp_before, "the stamp follows items entering the bin");

        let ours = |items: Vec<BinItem>| -> Vec<BinItem> {
            items.into_iter().filter(|item| key(&item.folder) == key(&text(&dir))).collect()
        };
        let listed = ours(items().unwrap());
        assert_eq!(listed.len(), 2, "{listed:?}");
        let item = |name: &str| listed.iter().find(|item| item.name == name).cloned().unwrap();
        let (kept_item, gone_item) = (item(&format!("kestrel-bin-test-{pid}-kept.txt")), item(&format!("kestrel-bin-test-{pid}-gone.log")));
        assert_eq!(key(&kept_item.original), key(&text(&kept)));
        assert_eq!((kept_item.size, kept_item.dir, kept_item.ext.as_str()), (4, false, "txt"));
        assert_eq!((gone_item.size, gone_item.ext.as_str()), (13, "log"));
        assert!(kept_item.id.to_lowercase().contains("$recycle.bin"), "{kept_item:?}");
        assert!(!kept_item.type_name.is_empty());
        let now = (fileops::now() - FILETIME_UNIX_EPOCH) / 10_000;
        assert!((now - kept_item.deleted).abs() < 120_000, "deleted moments ago: {} vs {now}", kept_item.deleted);

        let (restored, outcome) = restore(std::slice::from_ref(&kept_item.id), QUIET).unwrap();
        assert!(restored.completed, "{restored:?}");
        assert_eq!(restored.paths.iter().map(|path| key(path)).collect::<Vec<_>>(), [key(&text(&kept))]);
        assert!(matches!(outcome.results().collect::<Vec<_>>().as_slice(), [Done::Restored { .. }]));
        assert_eq!(fs::read(&kept).unwrap(), b"kept");

        let erased = erase(std::slice::from_ref(&gone_item.id), QUIET).unwrap();
        assert_eq!(erased, Erased { erased: 1, left: 0 });
        assert!(!gone.exists(), "deleting from the bin does not put it back");
        assert!(ours(items().unwrap()).is_empty(), "nothing of ours is left in the bin");
        assert_eq!(recycle::records_mentioning(&format!("kestrel-bin-test-{pid}-")).len(), 0, "nor any record of it");

        // Asking again for what has gone does nothing.
        assert_eq!(restore(std::slice::from_ref(&kept_item.id), QUIET).unwrap().0.missing, 1);
        assert_eq!(erase(std::slice::from_ref(&gone_item.id), QUIET).unwrap(), Erased { erased: 0, left: 1 });
        assert_eq!(owners_items(), before, "the owner's own items are untouched");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder that went after its item was recycled is made again when the item comes back.
    #[test]
    fn restoring_makes_the_folder_again() {
        let dir = sandbox("recycle-bin-folder");
        let pid = std::process::id();
        let inner = dir.join("gone-folder");
        fs::create_dir_all(&inner).unwrap();
        let file = inner.join(format!("kestrel-bin-test-{pid}-orphan.txt"));
        fs::write(&file, b"orphan").unwrap();
        execute(Request::Delete { paths: vec![text(&file)], permanent: false }, QUIET).unwrap();
        fs::remove_dir(&inner).unwrap();
        let id = items().unwrap().into_iter().find(|item| key(&item.original) == key(&text(&file))).unwrap().id;
        let (restored, _) = restore(&[id], QUIET).unwrap();
        assert!(restored.completed, "{restored:?}");
        assert_eq!(fs::read(&file).unwrap(), b"orphan");
        fs::remove_dir_all(&dir).unwrap();
    }
}
