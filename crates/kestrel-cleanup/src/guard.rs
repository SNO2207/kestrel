//! What no recommendation may ever touch.
//!
//! Nothing inside the Windows folder, Program Files, ProgramData, the owner's Documents,
//! Pictures, Videos, Music or Desktop, OneDrive, Kestrel's own data folder, or a project's
//! sources (only its recognised build output, and only for the build output rule). Nothing that
//! is a link or a cloud placeholder, nor anything reached through a link (a junction, a folder
//! symbolic link, or a cloud provider's synced folder). No drive root.

use kestrel_index::{EntryId, Index, NO_ENTRY, attr};

use crate::locations::Locations;
use crate::tree::PLACEHOLDER;
use crate::{fold_path, within};

#[derive(Clone, Debug)]
pub struct Guard {
    /// Folded folders nothing is recommended inside.
    protected: Vec<String>,
    /// Folded project roots: only build output inside them, through the build rule.
    projects: Vec<String>,
}

/// Whether recommending the entry would mean deleting a link, a placeholder or what it leads to.
fn is_link_or_placeholder(attrs: u32) -> bool {
    attrs & (attr::REPARSE_POINT | PLACEHOLDER) != 0
}

impl Guard {
    /// `extra` are further protected folders (Kestrel's data folder); `projects` the roots of
    /// the projects found.
    pub fn new(locations: &Locations, extra: &[String], projects: &[String]) -> Self {
        let mut protected: Vec<String> = locations.protected().iter().chain(extra).map(|p| fold_path(p)).collect();
        protected.retain(|p| !p.is_empty());
        let projects = projects.iter().map(|p| fold_path(p)).filter(|p| !p.is_empty()).collect();
        Guard { protected, projects }
    }

    /// Whether `path` lies in a protected folder, or is one. String-only: see [`Guard::allows`]
    /// for the whole check.
    pub fn protects(&self, path: &str) -> bool {
        let folded = fold_path(path);
        // A drive root, or anything too short to be a real path, is never acted on.
        folded.len() <= 3 || self.protected.iter().any(|folder| within(&folded, folder))
    }

    /// Whether `path` is a project's root or inside one.
    pub fn in_project(&self, path: &str) -> bool {
        let folded = fold_path(path);
        self.projects.iter().any(|root| within(&folded, root))
    }

    /// Whether a rule may recommend the entry.
    pub fn allows(&self, index: &Index, entry: EntryId) -> bool {
        self.check(index, entry, false)
    }

    /// Whether the build output rule may recommend the entry, a build folder in a project.
    pub fn allows_build_output(&self, index: &Index, entry: EntryId) -> bool {
        self.check(index, entry, true)
    }

    fn check(&self, index: &Index, entry: EntryId, build_output: bool) -> bool {
        if entry >= index.len() || index.is_deleted(entry) || index.parent(entry) == NO_ENTRY {
            return false;
        }
        if is_link_or_placeholder(index.attrs(entry)) {
            return false;
        }
        // Reached through a link: the index never reads through junctions, but a synced cloud
        // folder is a reparse point it does read.
        let mut cur = index.parent(entry);
        while cur != NO_ENTRY && index.parent(cur) != NO_ENTRY {
            if index.attrs(cur) & attr::REPARSE_POINT != 0 {
                return false;
            }
            cur = index.parent(cur);
        }
        let path = index.path(entry);
        if self.protects(&path) {
            return false;
        }
        if self.in_project(&path) {
            // A project root itself is never build output.
            return build_output && !self.projects.contains(&fold_path(&path));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Entry, synthetic};

    #[test]
    fn protected_places_links_and_projects_are_refused() {
        let places = Locations::under(std::path::Path::new(r"C:\box"));
        let guard = Guard::new(&places, &[r"C:\box\Users\me\AppData\Local\Kestrel".into()], &[r"C:\box\Users\me\code\app".into()]);
        let index = synthetic(&[
            Entry::file(r"box\Users\me\Documents\a.dmp"),
            Entry::file(r"box\Users\me\Downloads\setup.exe"),
            Entry::dir(r"box\Users\me\Downloads\link").attrs(attr::REPARSE_POINT),
            Entry::file(r"box\Users\me\Downloads\link\inside.exe"),
            Entry::file(r"box\Users\me\Downloads\cloud.zip").attrs(attr::RECALL_ON_DATA_ACCESS),
            Entry::file(r"box\Users\me\code\app\main.rs"),
            Entry::dir(r"box\Users\me\code\app\target"),
            Entry::file(r"box\Users\me\AppData\Local\Kestrel\index.kidx"),
            Entry::dir(r"box\Windows"),
            Entry::dir(r"box\Program Files"),
            Entry::dir(r"box\Users\me\OneDrive"),
        ]);

        let allows = |path: &str| guard.allows(&index, index.lookup(path).unwrap());
        assert!(allows(r"C:\box\Users\me\Downloads\setup.exe"));
        for refused in [
            r"C:\box\Users\me\Documents\a.dmp",
            r"C:\box\Users\me\Documents",
            r"C:\box\Users\me\Downloads\link",
            r"C:\box\Users\me\Downloads\link\inside.exe",
            r"C:\box\Users\me\Downloads\cloud.zip",
            r"C:\box\Users\me\code\app\main.rs",
            r"C:\box\Users\me\code\app\target",
            r"C:\box\Users\me\AppData\Local\Kestrel\index.kidx",
            r"C:\box\Windows",
            r"C:\box\Program Files",
            r"C:\box\Users\me\OneDrive",
            r"C:\",
        ] {
            assert!(!allows(refused), "{refused}");
        }
        let build = |path: &str| guard.allows_build_output(&index, index.lookup(path).unwrap());
        assert!(build(r"C:\box\Users\me\code\app\target"));
        assert!(!build(r"C:\box\Users\me\code\app"), "never the project itself");
        assert!(!build(r"C:\box\Users\me\Documents\a.dmp"));
    }
}
