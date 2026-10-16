//! The folders AI search describes unless the owner chooses others: where Windows keeps their
//! pictures, videos and desktop, and the profile's own Pictures and Videos when OneDrive moved
//! the known folders elsewhere (the old folders often still hold photos). Files there that are
//! only in the cloud are skipped, never downloaded (`stage.rs`).

use crate::places;

/// The default folders, each once.
pub fn defaults() -> Vec<String> {
    let found = places::places();
    let mut out: Vec<String> =
        found.folders.iter().filter(|folder| matches!(folder.kind, "pictures" | "videos" | "desktop")).map(|folder| folder.path.clone()).collect();
    if !found.home.is_empty() {
        for name in ["Pictures", "Videos"] {
            let own = format!(r"{}\{name}", found.home);
            if std::path::Path::new(&own).is_dir() {
                out.push(own);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|path| seen.insert(path.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_defaults_are_folders_of_this_profile() {
        let defaults = super::defaults();
        assert!(!defaults.is_empty());
        let mut lower: Vec<String> = defaults.iter().map(|p| p.to_lowercase()).collect();
        lower.dedup();
        assert_eq!(lower.len(), defaults.len(), "each once: {defaults:?}");
        assert!(defaults.iter().any(|p| p.to_lowercase().ends_with("pictures")), "{defaults:?}");
    }
}
