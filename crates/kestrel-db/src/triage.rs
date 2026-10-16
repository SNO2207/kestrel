//! The Downloads triage page's memory: when the owner last looked at it, and where they moved
//! downloads to, by extension, kind and the first word of the name, so that the page can
//! suggest the same folder for similar files next time.

use rusqlite::params;

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_20_search_triage_moves",
    sql: "CREATE TABLE triage_moves (
              ext TEXT NOT NULL,
              kind TEXT NOT NULL,
              token TEXT NOT NULL,
              destination TEXT NOT NULL,
              count INTEGER NOT NULL DEFAULT 1,
              last_at INTEGER NOT NULL,
              PRIMARY KEY (ext, kind, token, destination)
          );",
}];

/// The setting holding when the page was last looked at, in milliseconds since 1970.
const SEEN: &str = "triage_seen";

/// Where files like one were moved before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learned {
    /// Lower-case extension, empty for folders and files without one.
    pub ext: String,
    /// `installers`, `archives`, `documents`, `images`, `videos` or `other`.
    pub kind: String,
    /// The first word of the name, lower case, or empty.
    pub token: String,
    pub destination: String,
    /// How many such files went there.
    pub count: i64,
    /// When one last did, in milliseconds since 1970.
    pub last_at: i64,
}

/// Remembers that a file of this extension, kind and first word was moved to `destination`.
pub fn record_move(db: &Db, ext: &str, kind: &str, token: &str, destination: &str, at: i64) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO triage_moves (ext, kind, token, destination, count, last_at) VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(ext, kind, token, destination) DO UPDATE SET count = count + 1, last_at = MAX(last_at, excluded.last_at)",
            params![ext, kind, token, destination, at],
        )
    })?;
    Ok(())
}

/// Everything learned, most used first.
pub fn learned(db: &Db) -> Result<Vec<Learned>> {
    db.with(|c| {
        let mut statement =
            c.prepare("SELECT ext, kind, token, destination, count, last_at FROM triage_moves ORDER BY count DESC, last_at DESC LIMIT 2000")?;
        let rows = statement.query_map([], |row| {
            Ok(Learned {
                ext: row.get(0)?,
                kind: row.get(1)?,
                token: row.get(2)?,
                destination: row.get(3)?,
                count: row.get(4)?,
                last_at: row.get(5)?,
            })
        })?;
        rows.collect()
    })
}

/// Forgets a destination, as when its folder is gone.
pub fn forget(db: &Db, destination: &str) -> Result<()> {
    db.with(|c| c.execute("DELETE FROM triage_moves WHERE destination = ?1 COLLATE NOCASE", params![destination]))?;
    Ok(())
}

/// When the page was last looked at, if ever.
pub fn seen(db: &Db) -> Result<Option<i64>> {
    db.setting(SEEN)
}

pub fn set_seen(db: &Db, at: i64) -> Result<()> {
    db.set_setting(SEEN, &at)
}

/// Forgets the last look, so that the next one starts afresh.
pub fn forget_seen(db: &Db) -> Result<()> {
    db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", params![SEEN]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_are_counted_by_kind_of_file_and_place() {
        let db = Db::open_in_memory().unwrap();
        assert!(learned(&db).unwrap().is_empty());
        record_move(&db, "pdf", "documents", "invoice", r"D:\Finance", 10).unwrap();
        record_move(&db, "pdf", "documents", "invoice", r"D:\Finance", 30).unwrap();
        record_move(&db, "pdf", "documents", "manual", r"C:\Manuals", 20).unwrap();
        let all = learned(&db).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].destination.as_str(), all[0].count, all[0].last_at), (r"D:\Finance", 2, 30));
        forget(&db, r"d:\finance").unwrap();
        assert_eq!(learned(&db).unwrap().len(), 1);
    }

    #[test]
    fn the_last_look_is_remembered() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(seen(&db).unwrap(), None);
        set_seen(&db, 1234).unwrap();
        assert_eq!(seen(&db).unwrap(), Some(1234));
        forget_seen(&db).unwrap();
        assert_eq!(seen(&db).unwrap(), None);
    }
}
