//! Workspaces: a saved set of tabs, each with its folder, view and sort, restored together.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::{clean_name, next_position, not_found};
use crate::{Db, Error, Migration, Result};

pub(super) const MIGRATION: Migration = Migration {
    name: "2026_10_16_shortcuts_workspaces",
    sql: "
        CREATE TABLE workspaces (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            tabs TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
    ",
};

/// Most tabs one workspace keeps.
pub const MAX_TABS: usize = 64;

/// How one tab looked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub path: String,
    /// The view mode, such as `details` or `large`, as the front end names it.
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub sort: Option<TabSort>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSort {
    pub key: String,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub tabs: Vec<TabState>,
    /// Which of `tabs` was active.
    pub active: usize,
    /// Unix seconds.
    pub updated_at: i64,
}

fn workspace_from(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let raw: String = row.get(2)?;
    // A row this module wrote always parses; anything else restores as no tabs, not an error
    // that would hide every other workspace.
    let tabs = serde_json::from_str(&raw).unwrap_or_default();
    Ok(Workspace { id: row.get(0)?, name: row.get(1)?, tabs, active: row.get::<_, i64>(3)? as usize, updated_at: row.get(4)? })
}

const COLUMNS: &str = "id, name, tabs, active, updated_at";

/// Every workspace, in sidebar order.
pub fn list(db: &Db) -> Result<Vec<Workspace>> {
    db.with(|c| c.prepare(&format!("SELECT {COLUMNS} FROM workspaces ORDER BY position, id"))?.query_map([], workspace_from)?.collect())
}

pub fn get(db: &Db, id: i64) -> Result<Workspace> {
    db.with(|c| c.query_row(&format!("SELECT {COLUMNS} FROM workspaces WHERE id = ?1"), params![id], workspace_from).optional())?
        .ok_or_else(|| not_found("workspace"))
}

/// Saves `tabs` under `name` at `now` (Unix seconds). Saving under a name already in use, in
/// any case, replaces that workspace's tabs and keeps its place in the list.
pub fn save(db: &Db, name: &str, tabs: &[TabState], active: usize, now: i64) -> Result<Workspace> {
    let name = clean_name(name)?;
    if tabs.is_empty() {
        return Err(Error("A workspace needs at least one tab".into()));
    }
    if tabs.len() > MAX_TABS {
        return Err(Error(format!("A workspace keeps at most {MAX_TABS} tabs")));
    }
    if tabs.iter().any(|tab| tab.path.trim().is_empty()) {
        return Err(Error("Every tab of a workspace needs a folder".into()));
    }
    let raw = serde_json::to_string(tabs)?;
    let active = active.min(tabs.len() - 1) as i64;
    let id = db.transaction(|t| {
        let existing: Option<i64> =
            t.query_row("SELECT id FROM workspaces WHERE name = ?1", params![name], |row| row.get(0)).optional()?;
        if let Some(id) = existing {
            t.execute(
                "UPDATE workspaces SET name = ?1, tabs = ?2, active = ?3, updated_at = ?4 WHERE id = ?5",
                params![name, raw, active, now, id],
            )?;
            return Ok(id);
        }
        let position = next_position(t, "workspaces", "1")?;
        t.execute(
            "INSERT INTO workspaces (name, tabs, active, position, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, raw, active, position, now],
        )?;
        Ok(t.last_insert_rowid())
    })?;
    get(db, id)
}

/// Renames a workspace. A name another workspace has is refused rather than merged.
pub fn rename(db: &Db, id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    let rows = db
        .with(|c| c.execute("UPDATE workspaces SET name = ?1 WHERE id = ?2", params![name, id]))
        .map_err(|error| match error.to_string() {
            message if message.contains("UNIQUE") => Error(format!("There is already a workspace named \"{name}\"")),
            message => Error(message),
        })?;
    if rows == 0 { Err(not_found("workspace")) } else { Ok(()) }
}

pub fn delete(db: &Db, id: i64) -> Result<()> {
    let rows = db.with(|c| c.execute("DELETE FROM workspaces WHERE id = ?1", params![id]))?;
    if rows == 0 { Err(not_found("workspace")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: &str, view: Option<&str>, sort: Option<(&str, bool)>) -> TabState {
        TabState {
            path: path.into(),
            view: view.map(Into::into),
            sort: sort.map(|(key, desc)| TabSort { key: key.into(), desc }),
        }
    }

    #[test]
    fn workspaces_save_tabs_and_replace_by_name() {
        let db = Db::open_in_memory().unwrap();
        let tabs = vec![tab(r"C:\Projects", Some("details"), Some(("size", true))), tab(r"D:\Photos", Some("large"), None)];
        let saved = save(&db, "Writing", &tabs, 1, 100).unwrap();
        assert_eq!((saved.tabs.clone(), saved.active, saved.updated_at), (tabs.clone(), 1, 100));
        let other = save(&db, "Coding", &tabs[..1], 0, 110).unwrap();

        // Saving under the same name, in any case, replaces the tabs and keeps the place.
        let replaced = save(&db, "writing", &tabs[1..], 5, 120).unwrap();
        assert_eq!(replaced.id, saved.id);
        assert_eq!((replaced.name.as_str(), replaced.tabs.len(), replaced.active), ("writing", 1, 0));
        let names: Vec<String> = list(&db).unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["writing", "Coding"]);

        assert!(rename(&db, other.id, "WRITING").is_err(), "names stay unique");
        rename(&db, other.id, "Code").unwrap();
        assert_eq!(get(&db, other.id).unwrap().name, "Code");
        delete(&db, saved.id).unwrap();
        assert_eq!(list(&db).unwrap().len(), 1);
        assert!(delete(&db, saved.id).is_err());

        assert!(save(&db, "Nothing", &[], 0, 0).is_err());
        assert!(save(&db, "Blank", &[tab(" ", None, None)], 0, 0).is_err());
        let many: Vec<TabState> = (0..=MAX_TABS).map(|n| tab(&format!(r"C:\{n}"), None, None)).collect();
        assert!(save(&db, "Too many", &many, 0, 0).is_err());
    }

    #[test]
    fn tabs_read_back_from_older_rows_with_missing_fields() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO workspaces (name, tabs, active, position, updated_at) VALUES ('Old', '[{\"path\":\"C:\\\\\"}]', 0, 0, 1)",
                [],
            )
        })
        .unwrap();
        assert_eq!(list(&db).unwrap()[0].tabs, [tab(r"C:\", None, None)]);
    }
}
