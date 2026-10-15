//! Commands for frequent and recent folders.

use std::path::PathBuf;
use std::sync::OnceLock;

use kestrel_db::shortcuts::frecency;
use serde::Serialize;

use super::{Data, exists, now};

/// Folders whose visits are never recorded: scratch space that comes and goes, such as the temp
/// folders where installers, builds and tests make throwaway folders, and the app's own data.
fn scratch_roots() -> &'static [String] {
    static ROOTS: OnceLock<Vec<String>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots: Vec<PathBuf> = vec![std::env::temp_dir(), crate::indexer::data_dir()];
        for (variable, below) in [("SystemRoot", "Temp"), ("SystemRoot", "SystemTemp"), ("LOCALAPPDATA", "Temp")] {
            if let Some(base) = std::env::var_os(variable) {
                roots.push(PathBuf::from(base).join(below));
            }
        }
        roots.iter().filter_map(|root| root.to_str()).map(comparable).filter(|root| !root.is_empty()).collect()
    })
}

/// A path in a form that compares equal for the same folder.
fn comparable(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Whether `path` is, or is inside, one of `roots` (given in comparable form), or in a
/// Recycle Bin.
fn is_scratch_in(path: &str, roots: &[String]) -> bool {
    let path = comparable(path);
    path.contains(r"\$recycle.bin")
        || roots.iter().any(|root| path == *root || path.strip_prefix(root.as_str()).is_some_and(|rest| rest.starts_with('\\')))
}

pub fn is_scratch(path: &str) -> bool {
    is_scratch_in(path, scratch_roots())
}

/// Records a visit to a folder, unless it is scratch space. Returns whether it was recorded.
#[tauri::command(async)]
pub fn frecency_visit(db: Data, path: String) -> Result<bool, String> {
    if path.trim().is_empty() || is_scratch(&path) {
        return Ok(false);
    }
    frecency::record(&db, &path, now())?;
    Ok(true)
}

#[derive(Serialize)]
pub struct Frequent {
    path: String,
    score: f64,
}

/// The `limit` highest-ranked folders that still exist.
#[tauri::command(async)]
pub fn frecency_top(db: Data, limit: usize) -> Result<Vec<Frequent>, String> {
    let ranked = frecency::ranked(&db, now())?;
    Ok(ranked
        .into_iter()
        .filter(|folder| !is_scratch(&folder.path) && exists(&folder.path))
        .take(limit)
        .map(|folder| Frequent { path: folder.path, score: folder.score })
        .collect())
}

/// Takes a folder off the Frequent list until it is visited again.
#[tauri::command(async)]
pub fn frecency_forget(db: Data, path: String) -> Result<bool, String> {
    Ok(frecency::forget(&db, &path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_folders_are_recognised() {
        let roots = vec![comparable(r"C:\Users\me\AppData\Local\Temp\"), comparable(r"C:\Windows\Temp")];
        assert!(is_scratch_in(r"C:\Users\me\AppData\Local\Temp", &roots));
        assert!(is_scratch_in(r"c:\users\ME\appdata\local\temp\kestrel-e2e-1\sub", &roots));
        assert!(is_scratch_in(r"C:\Windows\Temp\x", &roots));
        assert!(is_scratch_in(r"D:\$RECYCLE.BIN\S-1-5-21", &roots));
        assert!(!is_scratch_in(r"C:\Users\me\AppData\Local\Temporary Projects", &roots));
        assert!(!is_scratch_in(r"C:\Windows", &roots));
        assert!(!is_scratch_in(r"C:\Users\me\Documents", &roots));
        // The real temp folder is one of them.
        assert!(is_scratch(std::env::temp_dir().join("anything").to_str().unwrap()));
        assert!(!is_scratch(r"C:\Windows\System32"));
    }
}
