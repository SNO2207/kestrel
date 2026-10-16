//! How the window is laid out, kept between runs: one or two panes, where they split, each
//! pane's tabs, and the details view's columns (for folders and for search results). Each is
//! one named JSON document the front end writes whole whenever it changes; the database rather
//! than the web view's storage keeps it, because a test instance is stopped by killing it and
//! the web view may not have written its storage out by then.

use rusqlite::{OptionalExtension, params};

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_18_layout_state",
    sql: "CREATE TABLE layout_state (name TEXT PRIMARY KEY, value TEXT NOT NULL);",
}];

/// Longest document kept, so a runaway caller can't fill the disk.
pub const MAX_VALUE: usize = 256 << 10;

/// Every saved document, by name.
pub fn load(db: &Db) -> Result<Vec<(String, String)>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT name, value FROM layout_state ORDER BY name")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    })
}

/// One saved document.
pub fn get(db: &Db, name: &str) -> Result<Option<String>> {
    db.with(|c| c.query_row("SELECT value FROM layout_state WHERE name = ?1", params![name], |row| row.get(0)).optional())
}

/// Saves a document, replacing the one of that name; an empty value forgets it.
pub fn save(db: &Db, name: &str, value: &str) -> Result<()> {
    if value.len() > MAX_VALUE {
        return Err(crate::Error(format!("the layout {name} is too large to keep")));
    }
    db.with(|c| {
        if value.is_empty() {
            c.execute("DELETE FROM layout_state WHERE name = ?1", params![name])
        } else {
            c.execute(
                "INSERT INTO layout_state (name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                params![name, value],
            )
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_saved_replaced_and_forgotten() {
        let db = Db::open_in_memory().unwrap();
        assert!(load(&db).unwrap().is_empty());
        save(&db, "panes", r#"{"dual":true}"#).unwrap();
        save(&db, "columns:folder", "[1]").unwrap();
        save(&db, "panes", r#"{"dual":false}"#).unwrap();
        assert_eq!(get(&db, "panes").unwrap().as_deref(), Some(r#"{"dual":false}"#));
        assert_eq!(
            load(&db).unwrap(),
            [("columns:folder".to_owned(), "[1]".to_owned()), ("panes".to_owned(), r#"{"dual":false}"#.to_owned())]
        );
        save(&db, "panes", "").unwrap();
        assert_eq!(get(&db, "panes").unwrap(), None);
        assert!(save(&db, "huge", &"x".repeat(MAX_VALUE + 1)).is_err());
    }
}
