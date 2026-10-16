//! Home's recent files: Windows' Recent items (the shortcuts in the Recent folder, which
//! Windows adds as files are opened in any program) and the files opened through Kestrel
//! (`kestrel_db::parity`), newest first.
//!
//! Each Recent item is a shortcut; only the shortcut is read, never its target, so a target
//! that lives in the cloud is not downloaded. Targets that are gone, and folders (which Home
//! shows as frequent folders instead), are left out. "Remove from Recent" hides an item in
//! Kestrel (until it is opened again) rather than deleting anything of Windows'.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use kestrel_db::Db;
use kestrel_db::parity::{self as store, fold};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::State;
use windows_sys::Win32::UI::Shell::FOLDERID_Recent;

use super::{CLOUD_ONLY, DIRECTORY, attributes, in_apartment, known_folder, now_ms};

/// Windows' Recent items read at most, newest first.
const MAX_SHORTCUTS: usize = 300;
/// How long the Recent folder's answer is reused.
const KEEP_FOR: Duration = Duration::from_secs(5);
/// Where the items hidden with "Remove from Recent" are kept, with when they were hidden.
const HIDDEN_SETTING: &str = "parity.recent_hidden";
const MAX_HIDDEN: usize = 500;

/// One recent file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    /// The folder it is in.
    pub location: String,
    /// When it was last opened, in milliseconds since the Unix epoch.
    pub used: i64,
    /// `windows` (the Recent folder) or `kestrel` (opened through Kestrel).
    pub source: &'static str,
    /// Its contents are in the cloud only.
    pub cloud: bool,
}

/// A recent item before it is checked: a path and when it was used.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub path: String,
    pub used: i64,
    pub source: &'static str,
}

/// Merges candidates from both sources: one per file (by path, ignoring case), with its latest
/// use, newest first, without hidden ones (hidden since that use) and those `keep` refuses.
pub fn merge(
    candidates: impl IntoIterator<Item = Candidate>,
    hidden: &HashMap<String, i64>,
    keep: impl Fn(&str) -> Option<bool>,
    limit: usize,
) -> Vec<RecentFile> {
    let mut best: HashMap<String, Candidate> = HashMap::new();
    for candidate in candidates {
        let key = fold(&candidate.path);
        if hidden.get(&key).is_some_and(|&at| at >= candidate.used) {
            continue;
        }
        match best.get(&key) {
            Some(existing) if existing.used >= candidate.used => {}
            _ => {
                best.insert(key, candidate);
            }
        }
    }
    let mut list: Vec<Candidate> = best.into_values().collect();
    list.sort_by(|a, b| b.used.cmp(&a.used).then_with(|| a.path.cmp(&b.path)));
    list.into_iter()
        .filter_map(|candidate| {
            let cloud = keep(&candidate.path)?;
            let path = Path::new(&candidate.path);
            Some(RecentFile {
                name: path.file_name()?.to_string_lossy().into_owned(),
                location: crate::fileops::parent(&candidate.path),
                path: candidate.path,
                used: candidate.used,
                source: candidate.source,
                cloud,
            })
        })
        .take(limit)
        .collect()
}

/// Whether a path is worth listing: an existing file, not a folder. Returns whether its
/// contents are in the cloud only, or `None` to leave it out. Only attributes are read.
fn file_state(path: &str) -> Option<bool> {
    let attrs = attributes(path)?;
    (attrs & DIRECTORY == 0).then_some(attrs & CLOUD_ONLY != 0)
}

/// The shortcuts in Windows' Recent folder, newest first, with their targets.
fn windows_recent() -> Vec<Candidate> {
    static CACHE: Mutex<Option<(Instant, Vec<Candidate>)>> = Mutex::new(None);
    if let Some((at, list)) = CACHE.lock().as_ref()
        && at.elapsed() < KEEP_FOR
    {
        return list.clone();
    }
    let Some(folder) = known_folder(&FOLDERID_Recent) else { return Vec::new() };
    let mut links: Vec<(PathBuf, i64)> = std::fs::read_dir(&folder)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")))
                .filter_map(|entry| {
                    let modified = entry.metadata().ok()?.modified().ok()?;
                    let ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
                    Some((entry.path(), ms))
                })
                .collect()
        })
        .unwrap_or_default();
    links.sort_by(|a, b| b.1.cmp(&a.1));
    links.truncate(MAX_SHORTCUTS);
    let list = in_apartment(move || {
        Ok(links
            .into_iter()
            .filter_map(|(link, used)| Some(Candidate { path: super::links::read_target(&link)?, used, source: "windows" }))
            .collect::<Vec<_>>())
    })
    .unwrap_or_default();
    *CACHE.lock() = Some((Instant::now(), list.clone()));
    list
}

fn hidden(db: &Db) -> HashMap<String, i64> {
    db.setting::<HashMap<String, i64>>(HIDDEN_SETTING).ok().flatten().unwrap_or_default()
}

/// The recent files, newest first.
pub fn recent_files(db: &Db, limit: usize, with_windows: bool) -> Vec<RecentFile> {
    let opened = store::opened(db, store::MAX_OPENED).unwrap_or_default();
    let kestrel = opened.into_iter().map(|item| Candidate { path: item.path, used: item.last_opened, source: "kestrel" });
    let windows = if with_windows { windows_recent() } else { Vec::new() };
    merge(kestrel.chain(windows), &hidden(db), file_state, limit)
}

// ---------- commands ----------

/// Home's recent files. `windows: false` leaves Windows' Recent items out (for the tests,
/// which must not depend on what the owner opened lately).
#[tauri::command(async)]
pub fn home_recent(db: State<Arc<Db>>, limit: usize, windows: Option<bool>) -> Vec<RecentFile> {
    recent_files(&db, limit.clamp(1, 200), windows.unwrap_or(true))
}

/// Remembers that a file was opened through Kestrel.
#[tauri::command(async)]
pub fn record_opened(db: State<Arc<Db>>, path: String) -> Result<(), String> {
    store::record_opened(&db, &path, now_ms())?;
    Ok(())
}

/// Takes a file off Home's recent files until it is opened again.
#[tauri::command(async)]
pub fn forget_recent(db: State<Arc<Db>>, path: String) -> Result<(), String> {
    store::forget_opened(&db, &path)?;
    let mut hidden = hidden(&db);
    hidden.insert(fold(&path), now_ms());
    if hidden.len() > MAX_HIDDEN {
        let mut by_age: Vec<(String, i64)> = hidden.into_iter().collect();
        by_age.sort_by(|a, b| b.1.cmp(&a.1));
        by_age.truncate(MAX_HIDDEN);
        hidden = by_age.into_iter().collect();
    }
    db.set_setting(HIDDEN_SETTING, &hidden)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, used: i64, source: &'static str) -> Candidate {
        Candidate { path: path.into(), used, source }
    }

    #[test]
    fn recent_files_are_merged_newest_first_once_each() {
        let hidden: HashMap<String, i64> = [(fold(r"C:\Hidden.txt"), 500), (fold(r"C:\Again.txt"), 100)].into_iter().collect();
        let keep = |path: &str| match path.to_lowercase().as_str() {
            r"c:\gone.txt" => None,
            r"c:\cloud.docx" => Some(true),
            _ => Some(false),
        };
        let list = merge(
            [
                candidate(r"C:\Notes.txt", 100, "windows"),
                candidate(r"c:\notes.TXT", 300, "kestrel"),
                candidate(r"C:\Cloud.docx", 200, "windows"),
                candidate(r"C:\Gone.txt", 900, "windows"),
                candidate(r"C:\Hidden.txt", 400, "windows"),
                // Opened again after it was hidden: it comes back.
                candidate(r"C:\Again.txt", 150, "kestrel"),
            ],
            &hidden,
            keep,
            10,
        );
        let summary: Vec<(&str, i64, &str, bool)> = list.iter().map(|f| (f.path.as_str(), f.used, f.source, f.cloud)).collect();
        assert_eq!(
            summary,
            [(r"c:\notes.TXT", 300, "kestrel", false), (r"C:\Cloud.docx", 200, "windows", true), (r"C:\Again.txt", 150, "kestrel", false)]
        );
        assert_eq!((list[0].name.as_str(), list[0].location.as_str()), ("notes.TXT", r"c:\"));
        assert_eq!(merge([candidate(r"C:\a.txt", 1, "kestrel"), candidate(r"C:\b.txt", 2, "kestrel")], &hidden, |_| Some(false), 1).len(), 1);
    }

    #[test]
    fn opened_files_and_removal_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("kestrel-test-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("opened.txt");
        std::fs::write(&file, b"x").unwrap();
        let path = file.display().to_string();
        store::record_opened(&db, &path, now_ms()).unwrap();
        store::record_opened(&db, &dir.display().to_string(), now_ms()).unwrap();
        let list = recent_files(&db, 10, false);
        assert_eq!(list.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), [path.as_str()], "folders are left out");
        // Hiding works on the same database as the command uses.
        store::forget_opened(&db, &path).unwrap();
        assert!(recent_files(&db, 10, false).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn windows_recent_items_are_read_without_failing() {
        // Whatever the owner opened lately: only that reading works and finds files.
        for file in recent_files(&Db::open_in_memory().unwrap(), 20, true) {
            assert!(!file.path.is_empty() && !file.name.is_empty());
        }
    }
}
