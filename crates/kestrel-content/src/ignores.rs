//! `.gitignore`, as git reads it: only inside a repository (a folder holding `.git`, a folder
//! or, for worktrees and submodules, a file), each file's patterns applying beneath its own
//! folder, a deeper file's patterns winning over a shallower one's, `.git\info\exclude` under
//! them all, and nothing inside an ignored folder coming back. Letters compare without case,
//! as git's do on Windows (`core.ignorecase`).
//!
//! The patterns are matched by ripgrep's `ignore` crate. Which files to read is decided here,
//! from listings and attributes, so a `.gitignore` whose contents are only in the cloud is
//! never opened (it is treated as empty).

use std::collections::HashMap;
use std::sync::Arc;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use parking_lot::Mutex;

use crate::fsinfo::{self, CLOUD_ONLY, DIRECTORY};

/// One folder's patterns.
struct Layer {
    /// The folder, with a trailing separator.
    dir: String,
    matcher: Gitignore,
}

/// The patterns that apply to the items of one folder, shallowest first, and whether the
/// folder itself is ignored.
#[derive(Clone, Default)]
pub struct Stack {
    in_repository: bool,
    layers: Vec<Arc<Layer>>,
    /// The folder is ignored, so everything in it is.
    pub ignored: bool,
}

/// What a folder holds that matters here, from its listing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Markers {
    /// It holds `.git`: it is a repository's root.
    pub git: bool,
    /// The attributes of its `.gitignore`, if it has one.
    pub gitignore: Option<u32>,
    /// The attributes of `.git\info\exclude`, if there is one (only asked for at a root).
    pub exclude: Option<u32>,
}

fn with_separator(dir: &str) -> String {
    if dir.ends_with('\\') { dir.to_owned() } else { format!("{dir}\\") }
}

impl Stack {
    /// The stack for the items of `dir`, a folder inside the one this stack is for.
    pub fn enter(&self, dir: &str, markers: Markers) -> Stack {
        let mut next = self.clone();
        if markers.git {
            // A repository inside another (a nested clone) has its own rules.
            next = Stack { in_repository: true, layers: Vec::new(), ignored: self.ignored };
        }
        if !next.in_repository {
            return next;
        }
        let mut builder = GitignoreBuilder::new(dir);
        // Only sets a flag; it returns a `Result` for reasons of its own and never fails.
        let _ = builder.case_insensitive(true);
        let mut any = false;
        let base = with_separator(dir);
        // Later files win: the repository's exclude file first, then the folder's .gitignore.
        let mut add = |path: String, attrs: Option<u32>| {
            if let Some(attrs) = attrs
                && attrs & (CLOUD_ONLY | DIRECTORY) == 0
                && builder.add(&path).is_none()
            {
                any = true;
            }
        };
        if markers.git {
            add(format!(r"{base}.git\info\exclude"), markers.exclude);
        }
        add(format!("{base}.gitignore"), markers.gitignore);
        if any && let Ok(matcher) = builder.build() {
            next.layers.push(Arc::new(Layer { dir: base, matcher }));
        }
        next
    }

    /// Whether the item at `path`, directly inside this stack's folder, is ignored.
    pub fn ignores(&self, path: &str, is_dir: bool) -> bool {
        if self.ignored {
            return true;
        }
        for layer in self.layers.iter().rev() {
            // Patterns are matched against the path below the .gitignore's folder.
            let below = path.get(..layer.dir.len()).is_some_and(|start| start.eq_ignore_ascii_case(&layer.dir));
            let relative = if below { &path[layer.dir.len()..] } else { "" };
            if relative.is_empty() {
                continue;
            }
            match layer.matcher.matched(relative.replace('\\', "/"), is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

/// Stacks for folders anywhere, found from the disk and remembered, for files that come from
/// the index rather than a walk.
#[derive(Default)]
pub struct Ignores {
    stacks: Mutex<HashMap<String, Stack>>,
}

/// Whether a path (without its trailing separator) is a drive's or a share's root: `C:`,
/// `\\server\share` or `\\server`.
fn is_root(trimmed: &str) -> bool {
    match trimmed.strip_prefix(r"\\") {
        Some(share) => share.matches('\\').count() <= 1,
        None => trimmed.len() == 2 && trimmed.ends_with(':'),
    }
}

/// The folder holding `path`, or `None` for a drive's or share's root.
pub(crate) fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('\\');
    if is_root(trimmed) {
        return None;
    }
    let cut = trimmed.rfind('\\')?;
    let parent = &trimmed[..cut];
    // A drive's root keeps its separator: `C:\`, not `C:`.
    Some(if parent.len() == 2 && parent.ends_with(':') { &trimmed[..=cut] } else { parent })
}

fn markers_of(dir: &str) -> Markers {
    let base = with_separator(dir);
    let git = fsinfo::attributes(&format!("{base}.git"));
    Markers {
        git: git.is_some(),
        gitignore: fsinfo::attributes(&format!("{base}.gitignore")).map(|info| info.attrs),
        exclude: git
            .filter(|info| info.attrs & DIRECTORY != 0)
            .and_then(|_| fsinfo::attributes(&format!(r"{base}.git\info\exclude")))
            .map(|info| info.attrs),
    }
}

impl Ignores {
    /// The stack for the items of `dir`, reading `.git` and `.gitignore` of it and of every
    /// folder above it once.
    pub fn stack(&self, dir: &str) -> Stack {
        let key = dir.trim_end_matches('\\').to_lowercase();
        if let Some(known) = self.stacks.lock().get(&key) {
            return known.clone();
        }
        let above = match parent(dir) {
            Some(up) => {
                let up_stack = self.stack(up);
                let ignored = up_stack.ignores(dir.trim_end_matches('\\'), true);
                Stack { ignored, ..up_stack }
            }
            None => Stack::default(),
        };
        let stack = above.enter(dir, markers_of(dir));
        self.stacks.lock().insert(key, stack.clone());
        stack
    }

    /// Whether the file at `path` is ignored by the `.gitignore` files of its repository.
    pub fn ignored(&self, path: &str) -> bool {
        match parent(path) {
            Some(dir) => self.stack(dir).ignores(path, false),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parents_stop_at_roots() {
        assert_eq!(parent(r"C:\a\b"), Some(r"C:\a"));
        assert_eq!(parent(r"C:\a"), Some(r"C:\"));
        assert_eq!(parent(r"C:\"), None);
        assert_eq!(parent(r"\\server\share\x"), Some(r"\\server\share"));
        assert_eq!(parent(r"\\server\share"), None);
    }

    #[test]
    fn gitignore_applies_inside_repositories_only() {
        let root = std::env::temp_dir().join(format!("kestrel-content-ignores-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git").join("info")).unwrap();
        fs::create_dir_all(repo.join("src").join("gen")).unwrap();
        fs::create_dir_all(repo.join("target")).unwrap();
        fs::write(repo.join(".gitignore"), "target/\n*.LOG\n!keep.log\n").unwrap();
        fs::write(repo.join(".git").join("info").join("exclude"), "secret.txt\n").unwrap();
        fs::write(repo.join("src").join(".gitignore"), "gen/\n").unwrap();
        // Outside the repository the same patterns mean nothing.
        fs::write(root.join(".gitignore"), "*.txt\n").unwrap();
        let path = |rel: &str| repo.join(rel).to_str().unwrap().to_owned();

        let ignores = Ignores::default();
        assert!(ignores.ignored(&path(r"target\out.txt")), "ignored folder");
        assert!(ignores.ignored(&path(r"debug.log")), "case-insensitive glob");
        assert!(!ignores.ignored(&path(r"keep.log")), "whitelisted");
        assert!(ignores.ignored(&path(r"secret.txt")), ".git/info/exclude");
        assert!(ignores.ignored(&path(r"src\gen\made.rs")), "a deeper .gitignore");
        assert!(!ignores.ignored(&path(r"src\main.rs")));
        assert!(!ignores.ignored(&path(r"notes.txt")), "the parent's .gitignore is outside the repository");
        assert!(!ignores.ignored(root.join("loose.log").to_str().unwrap()));
        fs::remove_dir_all(&root).unwrap();
    }
}
