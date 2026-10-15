//! The undo history, kept between runs. The app's `undo.rs` decides what an operation is and
//! how it is reversed; here each one is a row with its label, its results as JSON and what
//! the items it left looked like (also JSON), on the undo or the redo stack, in the order of
//! its id.

use rusqlite::{OptionalExtension, params};

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "2026_10_16_undo_history",
        sql: "CREATE TABLE undo_entries (
                  id INTEGER PRIMARY KEY,
                  stack TEXT NOT NULL CHECK (stack IN ('undo', 'redo')),
                  label TEXT NOT NULL,
                  steps TEXT NOT NULL
              );
              CREATE TABLE undo_erased (
                  only INTEGER PRIMARY KEY CHECK (only = 1),
                  subjects TEXT NOT NULL
              );",
    },
    // Rows written before it have none: their items can only be matched by path.
    Migration { name: "2026_10_17_undo_marks", sql: "ALTER TABLE undo_entries ADD COLUMN marks TEXT;" },
];

/// Which stack an operation is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stack {
    Undo,
    Redo,
}

impl Stack {
    fn name(self) -> &'static str {
        match self {
            Stack::Undo => "undo",
            Stack::Redo => "redo",
        }
    }
}

/// One operation as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEntry {
    /// Unique and increasing: on each stack, the latest operation has the largest id.
    pub id: u64,
    pub stack: Stack,
    /// What the operation was, such as "rename of report.txt".
    pub label: String,
    /// What it did, as JSON.
    pub steps: String,
    /// What the items it left looked like just after, as JSON; `None` for operations stored
    /// before this was kept.
    pub marks: Option<String>,
}

/// A change to the stored history.
#[derive(Default)]
pub struct UndoChange<'a> {
    /// Operations that are gone.
    pub removed: &'a [u64],
    /// Operations that are new or changed.
    pub written: &'a [StoredEntry],
    /// What the latest operation erased for good, if it did; `None` clears it.
    pub erased: Option<&'a str>,
}

impl Db {
    /// Every stored operation, oldest first.
    pub fn undo_entries(&self) -> Result<Vec<StoredEntry>> {
        self.with(|c| {
            let mut query = c.prepare("SELECT id, stack, label, steps, marks FROM undo_entries ORDER BY id")?;
            let rows = query.query_map([], |row| {
                let stack: String = row.get(1)?;
                Ok(StoredEntry {
                    id: row.get::<_, i64>(0)? as u64,
                    stack: if stack == "redo" { Stack::Redo } else { Stack::Undo },
                    label: row.get(2)?,
                    steps: row.get(3)?,
                    marks: row.get(4)?,
                })
            })?;
            rows.collect()
        })
    }

    /// What the latest stored operation erased for good, if it did.
    pub fn undo_erased(&self) -> Result<Option<String>> {
        self.with(|c| c.query_row("SELECT subjects FROM undo_erased WHERE only = 1", [], |row| row.get(0)).optional())
    }

    /// The largest id ever stored, so new operations can be numbered after it.
    pub fn undo_last_id(&self) -> Result<u64> {
        self.with(|c| c.query_row("SELECT COALESCE(MAX(id), 0) FROM undo_entries", [], |row| row.get::<_, i64>(0)))
            .map(|id| id as u64)
    }

    /// Applies `change` in one transaction, then keeps only the newest `keep` operations on
    /// each stack, so the stored history stays bounded whatever the caller does.
    pub fn update_undo(&self, change: &UndoChange<'_>, keep: usize) -> Result<()> {
        self.transaction(|t| {
            for &id in change.removed {
                t.execute("DELETE FROM undo_entries WHERE id = ?1", params![id as i64])?;
            }
            for entry in change.written {
                t.execute(
                    "INSERT OR REPLACE INTO undo_entries (id, stack, label, steps, marks) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![entry.id as i64, entry.stack.name(), entry.label, entry.steps, entry.marks],
                )?;
            }
            match change.erased {
                Some(subjects) => t.execute("INSERT OR REPLACE INTO undo_erased (only, subjects) VALUES (1, ?1)", params![subjects])?,
                None => t.execute("DELETE FROM undo_erased", [])?,
            };
            for stack in [Stack::Undo, Stack::Redo] {
                t.execute(
                    "DELETE FROM undo_entries WHERE stack = ?1 AND id NOT IN
                         (SELECT id FROM undo_entries WHERE stack = ?1 ORDER BY id DESC LIMIT ?2)",
                    params![stack.name(), keep as i64],
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, stack: Stack, label: &str) -> StoredEntry {
        StoredEntry { id, stack, label: label.into(), steps: format!("[{id}]"), marks: Some(format!("[{{\"id\":{id}}}]")) }
    }

    #[test]
    fn rows_from_before_marks_were_kept_read_back_without_them() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| c.execute("INSERT INTO undo_entries (id, stack, label, steps) VALUES (1, 'undo', 'rename of a.txt', '[]')", []))
            .unwrap();
        let stored = db.undo_entries().unwrap();
        assert_eq!(stored[0].marks, None);
        assert_eq!(stored[0].steps, "[]");
    }

    #[test]
    fn operations_are_stored_changed_and_removed() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.undo_entries().unwrap().is_empty());
        assert_eq!(db.undo_last_id().unwrap(), 0);

        let written = [entry(1, Stack::Undo, "rename of a.txt"), entry(2, Stack::Undo, "move of 2 items")];
        db.update_undo(&UndoChange { written: &written, ..UndoChange::default() }, 100).unwrap();
        assert_eq!(db.undo_entries().unwrap(), written);

        // The latest is undone: it leaves the undo stack and comes back on the redo stack.
        let moved = [entry(3, Stack::Redo, "move of 2 items")];
        db.update_undo(&UndoChange { removed: &[2], written: &moved, erased: None }, 100).unwrap();
        assert_eq!(db.undo_entries().unwrap(), [written[0].clone(), moved[0].clone()]);
        assert_eq!(db.undo_last_id().unwrap(), 3);

        db.update_undo(&UndoChange { erased: Some("3 items"), ..UndoChange::default() }, 100).unwrap();
        assert_eq!(db.undo_erased().unwrap().as_deref(), Some("3 items"));
        db.update_undo(&UndoChange::default(), 100).unwrap();
        assert_eq!(db.undo_erased().unwrap(), None);
    }

    #[test]
    fn only_the_newest_operations_are_kept() {
        let db = Db::open_in_memory().unwrap();
        let many: Vec<StoredEntry> = (1..=150).map(|id| entry(id, Stack::Undo, &format!("op {id}"))).collect();
        let redo = [entry(151, Stack::Redo, "undone")];
        db.update_undo(&UndoChange { written: &many, ..UndoChange::default() }, 100).unwrap();
        db.update_undo(&UndoChange { written: &redo, ..UndoChange::default() }, 100).unwrap();
        let kept = db.undo_entries().unwrap();
        assert_eq!(kept.len(), 101);
        assert_eq!(kept.first().unwrap().id, 51, "the oldest fifty went");
        assert_eq!(kept.last().unwrap(), &redo[0]);
    }
}
