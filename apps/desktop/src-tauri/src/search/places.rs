//! Where named places are, for phrases ("in downloads", "in kestrel") and for the Downloads
//! triage page: the user's known folders, wherever Windows keeps them, and the projects the
//! projects feature found, by name.
//!
//! Development builds let the end-to-end tests stand scratch folders in for the known folders
//! and name projects of their own (`search_dev_places`), so nothing they do touches the owner's
//! real Downloads or Documents.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use kestrel_query::natural::Places;
use parking_lot::Mutex;

use crate::projects::Projects;

/// Folders and projects the tests stand in.
#[derive(Default)]
struct Standins {
    folders: HashMap<String, String>,
    projects: Vec<(String, String)>,
}

fn standins() -> &'static Mutex<Standins> {
    static STANDINS: OnceLock<Mutex<Standins>> = OnceLock::new();
    STANDINS.get_or_init(Mutex::default)
}

/// Sets the stand-ins (development builds only): known folders by kind (`downloads`,
/// `documents`, `pictures`, `videos`, `music`, `desktop`), and projects as `(name, root)`.
pub fn stand_in(folders: HashMap<String, String>, projects: Vec<(String, String)>) {
    *standins().lock() = Standins { folders, projects };
}

/// A known folder by kind: `downloads`, `documents`, `pictures`, `videos`, `music`, `desktop`
/// or `home`, wherever Windows keeps it (OneDrive included).
pub fn known_folder(kind: &str) -> Option<String> {
    if let Some(path) = standins().lock().folders.get(kind) {
        return Some(path.clone());
    }
    let places = crate::places::places();
    if kind == "home" {
        return Some(places.home).filter(|home| !home.is_empty());
    }
    places.folders.into_iter().find(|folder| folder.kind == kind).map(|folder| folder.path)
}

/// Whether a project was named by the tests (`stand_in`).
pub fn is_stand_in_project(root: &str) -> bool {
    standins().lock().projects.iter().any(|(_, known)| known.eq_ignore_ascii_case(root))
}

/// The name a project goes by: its folder's name, in lower case.
fn project_name(root: &str) -> String {
    root.trim_end_matches('\\').rsplit('\\').next().unwrap_or(root).to_lowercase()
}

/// Projects by name, the stand-ins first. Their roots come from the projects feature, which
/// looks for them in the index the first time it is asked.
pub fn projects(found: &Projects) -> Vec<(String, String)> {
    let mut all = standins().lock().projects.iter().map(|(name, root)| (name.to_lowercase(), root.clone())).collect::<Vec<_>>();
    for (root, dismissed) in found.roots_for_cleanup() {
        if !dismissed {
            all.push((project_name(&root), root));
        }
    }
    all
}

/// The places a phrase can name. Projects are only looked up when a phrase names something
/// that is no known folder, since finding them the first time walks the index.
pub struct Resolver {
    projects: Arc<Projects>,
    found: Mutex<Option<Vec<(String, String)>>>,
}

impl Resolver {
    pub fn new(projects: Arc<Projects>) -> Self {
        Self { projects, found: Mutex::new(None) }
    }
}

impl Places for Resolver {
    fn known_folder(&self, name: &str) -> Option<String> {
        known_folder(name)
    }

    fn project(&self, name: &str) -> Option<String> {
        let mut found = self.found.lock();
        let all = found.get_or_insert_with(|| projects(&self.projects));
        all.iter().find(|(known, _)| known == name).map(|(_, root)| root.clone())
    }
}
