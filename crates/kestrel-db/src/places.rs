//! The sidebar's own state: which of its sections are collapsed, and whether the folder tree
//! expands to show the folder a tab opens ("Expand to open folder", on by default as in
//! Explorer).
//!
//! Sections are known by the name the sidebar gives them (`Pinned`, `Places`, `Frequent`,
//! `This PC`...), so a section added later needs no change here.

use rusqlite::params;

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_18_places_sidebar",
    sql: "CREATE TABLE places_sidebar (
            section TEXT PRIMARY KEY,
            collapsed INTEGER NOT NULL DEFAULT 0
          );",
}];

/// The settings key of "Expand to open folder".
const EXPAND_TO_OPEN: &str = "places.expand_to_open_folder";

/// The sections the user has collapsed, in no particular order.
pub fn collapsed_sections(db: &Db) -> Result<Vec<String>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT section FROM places_sidebar WHERE collapsed = 1 ORDER BY section")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    })
}

/// Collapses a section, or expands it again.
pub fn set_section_collapsed(db: &Db, section: &str, collapsed: bool) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO places_sidebar (section, collapsed) VALUES (?1, ?2)
             ON CONFLICT(section) DO UPDATE SET collapsed = excluded.collapsed",
            params![section, collapsed],
        )
    })?;
    Ok(())
}

/// Whether the folder tree reveals the folder a tab opens: on unless turned off.
pub fn expand_to_open_folder(db: &Db) -> Result<bool> {
    Ok(db.setting::<bool>(EXPAND_TO_OPEN)?.unwrap_or(true))
}

pub fn set_expand_to_open_folder(db: &Db, on: bool) -> Result<()> {
    db.set_setting(EXPAND_TO_OPEN, &on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_remember_being_collapsed() {
        let db = Db::open_in_memory().unwrap();
        assert!(collapsed_sections(&db).unwrap().is_empty());
        set_section_collapsed(&db, "Frequent", true).unwrap();
        set_section_collapsed(&db, "Saved searches", true).unwrap();
        set_section_collapsed(&db, "Frequent", false).unwrap();
        assert_eq!(collapsed_sections(&db).unwrap(), ["Saved searches"]);
    }

    #[test]
    fn expand_to_open_folder_is_on_until_turned_off() {
        let db = Db::open_in_memory().unwrap();
        assert!(expand_to_open_folder(&db).unwrap());
        set_expand_to_open_folder(&db, false).unwrap();
        assert!(!expand_to_open_folder(&db).unwrap());
        set_expand_to_open_folder(&db, true).unwrap();
        assert!(expand_to_open_folder(&db).unwrap());
    }
}
