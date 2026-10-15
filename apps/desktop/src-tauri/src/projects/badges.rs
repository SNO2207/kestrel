//! Git badges for the items of a folder view: whether each item is modified, added, untracked
//! or conflicted, and whether git ignores it.
//!
//! Statuses come from the repository's cached `git status`; a folder shows the most pressing
//! status of anything inside it. Ignored items are found with `git check-ignore`, asked only
//! about the items on screen and remembered until the status next changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::git::{Change, Status};

/// One item of a folder view, as the list sends it.
#[derive(Clone, Debug, Deserialize)]
pub struct Item {
    pub name: String,
    pub dir: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Badge {
    pub change: Option<Change>,
    pub ignored: bool,
}

/// The repository a folder is in: the nearest folder above it, or itself, with a `.git` folder
/// or file. Nothing inside a `.git` folder belongs to the working tree.
pub fn find_repo(folder: &Path) -> Option<PathBuf> {
    let mut current = Some(folder);
    while let Some(dir) = current {
        if dir.file_name().is_some_and(|name| name.eq_ignore_ascii_case(".git")) {
            return None;
        }
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// `folder` relative to `repo`, lower-cased, with `\` separators and no leading or trailing
/// separator; empty for the repository root itself.
pub fn relative(repo: &Path, folder: &Path) -> Option<String> {
    let rel = folder.strip_prefix(repo).ok()?;
    Some(rel.to_string_lossy().to_lowercase().trim_matches('\\').to_owned())
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_owned() } else { format!("{dir}\\{name}") }
}

/// A repository's status arranged for answering "what about this path?".
#[derive(Debug, Default)]
pub struct RepoView {
    /// Changed files (and untracked folders), by lower-cased relative path.
    paths: HashMap<String, Change>,
    /// Every folder above a change, with the most pressing change beneath it.
    folders: HashMap<String, Change>,
    /// Untracked folders: everything inside them is untracked too.
    untracked: Vec<String>,
}

impl RepoView {
    pub fn new(status: &Status) -> Self {
        let mut view = RepoView::default();
        for entry in &status.entries {
            let path = entry.path.to_lowercase();
            let path = match path.strip_suffix('\\') {
                Some(folder) => {
                    view.untracked.push(folder.to_owned());
                    folder.to_owned()
                }
                None => path,
            };
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('\\') {
                let slot = view.folders.entry(parent.to_owned()).or_insert(entry.change);
                *slot = (*slot).max(entry.change);
                ancestor = parent;
            }
            let slot = view.paths.entry(path).or_insert(entry.change);
            *slot = (*slot).max(entry.change);
        }
        view
    }

    /// The change to show for the item at `rel` (lower-cased, relative to the repository).
    pub fn change(&self, rel: &str) -> Option<Change> {
        let own = self.paths.get(rel).copied();
        let inside = self.folders.get(rel).copied();
        let best = own.max(inside);
        if best.is_some() {
            return best;
        }
        self.untracked
            .iter()
            .any(|folder| rel.len() > folder.len() && rel.starts_with(folder.as_str()) && rel.as_bytes()[folder.len()] == b'\\')
            .then_some(Change::Untracked)
    }
}

/// The paths `git check-ignore` should be asked about for a folder `rel` and some of its
/// items: the folder and each folder above it (if one is ignored, so is everything in it), then
/// the items. Paths use `/`, and folders end in `/` so that folder-only patterns apply.
pub fn ignore_queries(rel: &str, items: &[Item]) -> (Vec<String>, Vec<String>) {
    let mut ancestors = Vec::new();
    let mut path = rel;
    while !path.is_empty() {
        ancestors.push(format!("{}/", path.replace('\\', "/")));
        path = path.rsplit_once('\\').map_or("", |(parent, _)| parent);
    }
    let items = items
        .iter()
        .map(|item| {
            let path = join(rel, &item.name.to_lowercase()).replace('\\', "/");
            if item.dir { format!("{path}/") } else { path }
        })
        .collect();
    (ancestors, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::git::parse_status;

    #[test]
    fn folders_show_the_loudest_change_beneath_them() {
        let status = parse_status(
            b"1 .M N... 1 1 1 a b src/core/lib.rs\0? src/core/new.rs\0u UU N... 1 1 1 1 a b c docs/clash.md\0\
1 A. N... 0 1 1 0 c docs/added.md\0? notes/\0",
        );
        let view = RepoView::new(&status);
        assert_eq!(view.change(r"src\core\lib.rs"), Some(Change::Modified));
        assert_eq!(view.change(r"src\core\new.rs"), Some(Change::Untracked));
        assert_eq!(view.change(r"src\core"), Some(Change::Modified));
        assert_eq!(view.change("src"), Some(Change::Modified));
        assert_eq!(view.change("docs"), Some(Change::Conflicted));
        assert_eq!(view.change(r"docs\added.md"), Some(Change::Added));
        assert_eq!(view.change("notes"), Some(Change::Untracked));
        assert_eq!(view.change(r"notes\deep\idea.txt"), Some(Change::Untracked));
        assert_eq!(view.change("notesmore"), None);
        assert_eq!(view.change("readme.md"), None);
    }

    #[test]
    fn ignore_queries_cover_the_folder_and_its_ancestors() {
        let items = [Item { name: "Pkg".into(), dir: true }, Item { name: "index.JS".into(), dir: false }];
        let (ancestors, paths) = ignore_queries(r"web\node_modules", &items);
        assert_eq!(ancestors, ["web/node_modules/", "web/"]);
        assert_eq!(paths, ["web/node_modules/pkg/", "web/node_modules/index.js"]);
        let (ancestors, paths) = ignore_queries("", &items[1..]);
        assert!(ancestors.is_empty());
        assert_eq!(paths, ["index.js"]);
    }

    #[test]
    fn repositories_are_found_from_any_folder_inside() {
        let root = std::env::temp_dir().join(format!("kestrel-badges-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo").join(".git").join("refs")).unwrap();
        std::fs::create_dir_all(root.join("repo").join("src").join("deep")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let repo = root.join("repo");
        assert_eq!(find_repo(&repo.join("src").join("deep")), Some(repo.clone()));
        assert_eq!(find_repo(&repo), Some(repo.clone()));
        assert_eq!(find_repo(&repo.join(".git").join("refs")), None);
        assert_eq!(relative(&repo, &repo.join("Src").join("Deep")).as_deref(), Some(r"src\deep"));
        assert_eq!(relative(&repo, &repo).as_deref(), Some(""));
        // The temp folder itself is not in a repository (unless the machine is very unusual).
        if find_repo(&std::env::temp_dir()).is_none() {
            assert_eq!(find_repo(&root.join("outside")), None);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
