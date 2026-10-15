//! Saved searches: a query and where it searches, kept under a name in the sidebar.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{clean_name, next_position, not_found};
use crate::{Db, Error, Migration, Result};

pub(super) const MIGRATION: Migration = Migration {
    name: "2026_10_16_shortcuts_searches",
    sql: "
        CREATE TABLE saved_searches (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            query TEXT NOT NULL,
            scope TEXT,
            position INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
    ",
};

/// Longest query a saved search keeps; the search box never holds anything near this.
const MAX_QUERY: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: String,
    /// The folder searched beneath, or `None` to search every indexed drive.
    pub scope: Option<String>,
}

fn search_from(row: &Row<'_>) -> rusqlite::Result<SavedSearch> {
    Ok(SavedSearch { id: row.get(0)?, name: row.get(1)?, query: row.get(2)?, scope: row.get(3)? })
}

/// Every saved search, in sidebar order.
pub fn list(db: &Db) -> Result<Vec<SavedSearch>> {
    db.with(|c| {
        c.prepare("SELECT id, name, query, scope FROM saved_searches ORDER BY position, id")?
            .query_map([], search_from)?
            .collect()
    })
}

pub fn get(db: &Db, id: i64) -> Result<SavedSearch> {
    db.with(|c| {
        c.query_row("SELECT id, name, query, scope FROM saved_searches WHERE id = ?1", params![id], search_from).optional()
    })?
    .ok_or_else(|| not_found("saved search"))
}

/// Saves `query`, searching beneath `scope` or everywhere, under `name`, at the end of the list.
pub fn create(db: &Db, name: &str, query: &str, scope: Option<&str>) -> Result<SavedSearch> {
    let name = clean_name(name)?;
    let query = query.trim();
    if query.is_empty() {
        return Err(Error("There is no search to save".into()));
    }
    if query.len() > MAX_QUERY {
        return Err(Error("That search is too long to save".into()));
    }
    let scope = scope.map(str::trim).filter(|scope| !scope.is_empty());
    let id = db.transaction(|t| {
        let position = next_position(t, "saved_searches", "1")?;
        t.execute(
            "INSERT INTO saved_searches (name, query, scope, position) VALUES (?1, ?2, ?3, ?4)",
            params![name, query, scope, position],
        )?;
        Ok(t.last_insert_rowid())
    })?;
    get(db, id)
}

pub fn rename(db: &Db, id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    let rows = db.with(|c| c.execute("UPDATE saved_searches SET name = ?1 WHERE id = ?2", params![name, id]))?;
    if rows == 0 { Err(not_found("saved search")) } else { Ok(()) }
}

pub fn delete(db: &Db, id: i64) -> Result<()> {
    let rows = db.with(|c| c.execute("DELETE FROM saved_searches WHERE id = ?1", params![id]))?;
    if rows == 0 { Err(not_found("saved search")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_are_saved_renamed_and_deleted() {
        let db = Db::open_in_memory().unwrap();
        let big = create(&db, "Big videos", "  type:video size:>1gb ", None).unwrap();
        let notes = create(&db, "Notes", "ext:md", Some(r"C:\Users\me\Documents")).unwrap();
        assert_eq!(list(&db).unwrap(), [
            SavedSearch { id: big.id, name: "Big videos".into(), query: "type:video size:>1gb".into(), scope: None },
            SavedSearch {
                id: notes.id,
                name: "Notes".into(),
                query: "ext:md".into(),
                scope: Some(r"C:\Users\me\Documents".into())
            },
        ]);

        rename(&db, notes.id, "Markdown notes").unwrap();
        assert_eq!(get(&db, notes.id).unwrap().name, "Markdown notes");
        delete(&db, big.id).unwrap();
        assert_eq!(list(&db).unwrap().len(), 1);

        assert!(create(&db, "Empty", "   ", None).is_err());
        assert!(create(&db, "", "x", None).is_err());
        assert!(rename(&db, big.id, "Gone").is_err());
        assert!(delete(&db, big.id).is_err());
        assert!(get(&db, big.id).is_err());
        // A blank scope means everywhere.
        assert_eq!(create(&db, "Anywhere", "x", Some(" ")).unwrap().scope, None);
    }
}
