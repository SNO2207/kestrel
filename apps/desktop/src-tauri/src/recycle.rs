//! Finding items in the Recycle Bin again, through the shell's Recycle Bin folder, so an undo
//! can put back exactly the items Kestrel recycled: each is matched by the path it was deleted
//! from and the time it was deleted, then moved back by the shell's copy engine.

use std::collections::HashSet;
use std::path::Path;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_RecycleBinFolder, IEnumShellItems, IShellItem, IShellItem2,
    KF_FLAG_DEFAULT, SHGetKnownFolderItem,
};
use windows::core::{GUID, Interface, PWSTR};

use crate::fileops::{Window, key};

const DISPLACED: GUID = GUID::from_u128(0x9b174b33_40ff_11d2_a27e_00c04fc30871);
/// `System.Recycle.DeletedFrom`: the folder an item was deleted from.
const DELETED_FROM: PROPERTYKEY = PROPERTYKEY { fmtid: DISPLACED, pid: 2 };
/// `System.Recycle.DateDeleted`.
const DATE_DELETED: PROPERTYKEY = PROPERTYKEY { fmtid: DISPLACED, pid: 3 };
/// `System.ItemNameDisplay`: for an item in the bin, the name it had before it was deleted.
const NAME: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac), pid: 10 };
/// How far a recorded deletion time may stray from the operation's own clock readings, which
/// are taken just before and after the shell does its work.
const SLACK: i64 = 20_000_000;

/// An item in the Recycle Bin.
pub struct Entry {
    pub item: IShellItem,
    /// The full path it was deleted from.
    pub original: String,
    /// When it was deleted, in FILETIME ticks.
    pub deleted: i64,
}

/// Copies and frees a string the shell allocated.
///
/// # Safety
/// `text` must be a string allocated with `CoTaskMemAlloc` that nothing else frees.
unsafe fn take(text: PWSTR) -> Option<String> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let copy = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        copy
    }
}

/// Every item in the Recycle Bin of every drive, with where it came from and when.
///
/// # Safety
/// COM must be initialised on the calling thread.
pub unsafe fn entries() -> windows::core::Result<Vec<Entry>> {
    // SAFETY: COM is initialised (guaranteed by the caller); every out-pointer is valid.
    unsafe {
        let bin: IShellItem = SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems)?;
        let mut entries = Vec::new();
        loop {
            let mut batch = [None];
            let mut fetched = 0;
            items.Next(&mut batch, Some(&mut fetched))?;
            let Some(item) = batch[0].take().filter(|_| fetched == 1) else { break };
            let Ok(details) = item.cast::<IShellItem2>() else { continue };
            let folder = details.GetString(&DELETED_FROM).ok().and_then(|text| take(text));
            let name = details.GetString(&NAME).ok().and_then(|text| take(text));
            let deleted = details.GetFileTime(&DATE_DELETED).ok();
            let (Some(folder), Some(name), Some(deleted)) = (folder, name, deleted) else { continue };
            let deleted = ((deleted.dwHighDateTime as i64) << 32) | deleted.dwLowDateTime as i64;
            let original = format!("{}\\{name}", folder.trim_end_matches('\\'));
            entries.push(Entry { item, original, deleted });
        }
        Ok(entries)
    }
}

/// For each item that was deleted from a path within a window of time, the Recycle Bin item
/// it became, or `None` if it is no longer there. When several match, the latest deletion wins.
///
/// # Safety
/// COM must be initialised on the calling thread.
pub unsafe fn find(wanted: &[(String, Window)]) -> windows::core::Result<Vec<Option<IShellItem>>> {
    // SAFETY: guaranteed by the caller.
    let entries = unsafe { entries()? };
    let mut used = HashSet::new();
    Ok(wanted
        .iter()
        .map(|(path, window)| {
            let wanted = key(path);
            let widened = Window { from: window.from - SLACK, to: window.to + SLACK };
            let best = entries
                .iter()
                .enumerate()
                .filter(|(at, entry)| !used.contains(at) && key(&entry.original) == wanted && widened.contains(entry.deleted))
                .max_by_key(|(_, entry)| entry.deleted)
                .map(|(at, _)| at)?;
            used.insert(best);
            Some(entries[best].item.clone())
        })
        .collect())
}

/// The path a Recycle Bin record (an `$I…` file) says its item was deleted from. Version 1
/// records (Windows Vista to 8) hold a fixed 260-character path; version 2 a counted one.
fn recorded_original(bytes: &[u8]) -> Option<String> {
    let version = i64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    let text = match version {
        1 => bytes.get(24..24 + 520)?,
        2 => {
            let units = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            bytes.get(28..28 + units * 2)?
        }
        _ => return None,
    };
    let units: Vec<u16> = text.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    let text = String::from_utf16(&units).ok()?;
    Some(text.trim_end_matches('\0').to_owned())
}

/// Finishes putting back an item that was moved out of the Recycle Bin from `bin_item` (its
/// `$R…` file): the bin keeps a separate record of each item (the matching `$I…` file), which
/// moving the item leaves behind, invisible but taking up a slot. Explorer's own Restore
/// removes it, and so does this, but only a record that names `original` and whose item is
/// gone. Returns whether a record was removed.
pub fn forget(bin_item: &str, original: &str) -> bool {
    let item = Path::new(bin_item);
    let in_bin = key(bin_item).contains("\\$recycle.bin\\");
    let (Some(folder), Some(name)) = (item.parent(), item.file_name().and_then(|name| name.to_str())) else { return false };
    let Some(id) = name.strip_prefix("$R").filter(|_| in_bin) else { return false };
    let record = folder.join(format!("$I{id}"));
    if item.exists() {
        return false;
    }
    match std::fs::read(&record) {
        Ok(bytes) if recorded_original(&bytes).is_some_and(|recorded| key(&recorded) == key(original)) => {
            std::fs::remove_file(&record).is_ok()
        }
        _ => false,
    }
}

/// Recycle Bin records on drive C: whose original path contains `text`, for tests to check
/// that nothing of theirs is left behind.
#[cfg(test)]
pub fn records_mentioning(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    // Only this account's own folder is readable; the others are skipped.
    for folder in std::fs::read_dir(r"C:\$Recycle.Bin").into_iter().flatten().flatten() {
        for record in std::fs::read_dir(folder.path()).into_iter().flatten().flatten() {
            let is_record = record.file_name().to_string_lossy().starts_with("$I");
            let original = is_record.then(|| std::fs::read(record.path()).ok()).flatten().and_then(|bytes| recorded_original(&bytes));
            if let Some(original) = original.filter(|original| original.contains(text)) {
                found.push(original);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{sandbox, text};

    fn record(version: i64, original: &str) -> Vec<u8> {
        let units: Vec<u16> = original.encode_utf16().chain([0]).collect();
        let mut bytes = Vec::new();
        bytes.extend(version.to_le_bytes());
        bytes.extend(42u64.to_le_bytes());
        bytes.extend(134_000_000_000_000_000i64.to_le_bytes());
        if version == 2 {
            bytes.extend((units.len() as u32).to_le_bytes());
            bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        } else {
            let mut fixed = units.clone();
            fixed.resize(260, 0);
            bytes.extend(fixed.iter().flat_map(|unit| unit.to_le_bytes()));
        }
        bytes
    }

    #[test]
    fn reads_both_kinds_of_record() {
        assert_eq!(recorded_original(&record(2, r"C:\a\b.txt")).as_deref(), Some(r"C:\a\b.txt"));
        assert_eq!(recorded_original(&record(1, r"D:\old.doc")).as_deref(), Some(r"D:\old.doc"));
        assert_eq!(recorded_original(&record(3, r"C:\x")), None);
        assert_eq!(recorded_original(&[1, 2, 3]), None);
    }

    /// Exercised on look-alike files in a scratch folder: only a record outside any real
    /// Recycle Bin folder would be refused, so the folder is named like one.
    #[test]
    fn forgets_only_the_record_of_an_item_that_left() {
        let dir = sandbox("forget").join("$Recycle.Bin").join("S-1-5-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("$IABC123.txt"), record(2, r"C:\work\a.txt")).unwrap();
        std::fs::write(dir.join("$RABC123.txt"), b"still here").unwrap();
        let item = text(&dir.join("$RABC123.txt"));
        assert!(!forget(&item, r"C:\work\a.txt"), "the item is still in the bin");
        std::fs::remove_file(dir.join("$RABC123.txt")).unwrap();
        assert!(!forget(&item, r"C:\work\other.txt"), "the record names something else");
        assert!(forget(&item, r"C:\WORK\A.txt"));
        assert!(!dir.join("$IABC123.txt").exists());
        assert!(!forget(r"C:\work\$Rnot-in-a-bin.txt", r"C:\work\a.txt"));
        std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap()).unwrap();
    }
}
