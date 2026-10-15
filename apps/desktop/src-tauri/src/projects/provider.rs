//! The command palette's projects: `@` lists every project found, and plain queries match
//! projects by name among files and folders.
//!
//! Looking for projects runs git in every repository found, which is the hub's business: the
//! palette starts it only when asked for projects by name (`@`), as opening the hub does.
//! Without the prefix it offers the projects already found, if any.

use std::sync::Arc;

use super::{ProjectView, Projects};
use crate::palette::{self, Hit, Item, Provider, Query};

pub const ID: &str = "projects";
/// Lifts projects a little above files and folders that match as well, and pinned ones more.
const BOOST: i32 = 10;
const PINNED_BOOST: i32 = 20;

pub struct ProjectsProvider {
    projects: Arc<Projects>,
}

impl ProjectsProvider {
    pub fn new(projects: Arc<Projects>) -> Self {
        Self { projects }
    }
}

impl Provider for ProjectsProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn prefix(&self) -> Option<char> {
        Some('@')
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        if query.scope == Some('@') {
            self.projects.ensure_found();
        }
        let mut views: Vec<ProjectView> = self.projects.views().into_iter().filter(|view| !view.dismissed).collect();
        // With nothing typed after `@`, the list reads like the hub's: pinned first, then the
        // most recently active.
        views.sort_by_key(|view| (!view.pinned, std::cmp::Reverse(last_touched(view))));
        palette::rank_items(query, views.iter().map(item))
    }
}

/// The latest sign of life: a file written or a commit made, in milliseconds.
fn last_touched(view: &ProjectView) -> i64 {
    let commit = view.git.as_ref().and_then(|git| git.last_commit.as_ref()).map_or(0, |commit| commit.time);
    view.last_activity.max(commit)
}

/// A project as the palette shows it. Its stack, tags, status and branch are words the query
/// may match, so `@rust` or `@paused` find projects too.
fn item(view: &ProjectView) -> Item {
    let mut keywords: Vec<&str> = view.stack.iter().map(String::as_str).collect();
    keywords.extend(view.tags.iter().map(String::as_str));
    keywords.push(view.status.as_str());
    if let Some(branch) = view.git.as_ref().and_then(|git| git.branch.name.as_deref()) {
        keywords.push(branch);
    }
    Item {
        provider: ID.into(),
        id: view.root.clone(),
        title: view.name.clone(),
        subtitle: Some(view.root.clone()),
        path: Some(view.root.clone()),
        dir: true,
        keywords: Some(keywords.join(" ")),
        boost: if view.pinned { PINNED_BOOST } else { BOOST },
        ..Item::default()
    }
}
