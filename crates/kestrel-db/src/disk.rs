//! Disk usage history: once a day, the sizes of the larger folders and of the large files,
//! which the disk usage hub compares the index with to say what grew.
//!
//! A sample is a day (counted from the Unix epoch, in UTC) and its rows. Which days are kept is
//! the analysis's decision (`kestrel_analyze::growth::days_to_keep`: daily for 90 days, then
//! weekly); this module stores, reads and deletes them.

use rusqlite::params;

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_18_disk_samples",
    sql: "CREATE TABLE disk_sample_days (
            day INTEGER PRIMARY KEY,
            taken_at INTEGER NOT NULL
          );
          CREATE TABLE disk_samples (
            day INTEGER NOT NULL REFERENCES disk_sample_days (day) ON DELETE CASCADE,
            path TEXT NOT NULL,
            bytes INTEGER NOT NULL,
            is_file INTEGER NOT NULL,
            mtime INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (day, path)
          ) WITHOUT ROWID;",
}];

/// One folder or file of a sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    /// Size on disk.
    pub bytes: u64,
    pub file: bool,
    /// A file's modification time, in milliseconds since the Unix epoch; 0 for folders.
    pub mtime: i64,
}

/// Stores the sample of `day`, taken at `taken_at` (milliseconds since the Unix epoch),
/// replacing any sample already stored for that day.
pub fn save(db: &Db, day: i64, taken_at: i64, rows: &[Row]) -> Result<()> {
    db.transaction(|t| {
        t.execute("DELETE FROM disk_sample_days WHERE day = ?1", params![day])?;
        t.execute("INSERT INTO disk_sample_days (day, taken_at) VALUES (?1, ?2)", params![day, taken_at])?;
        let mut insert = t.prepare(
            "INSERT OR REPLACE INTO disk_samples (day, path, bytes, is_file, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for row in rows {
            insert.execute(params![day, row.path, row.bytes as i64, row.file, row.mtime])?;
        }
        Ok(())
    })
}

/// The days with a sample, oldest first.
pub fn days(db: &Db) -> Result<Vec<i64>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT day FROM disk_sample_days ORDER BY day")?;
        statement.query_map([], |row| row.get(0))?.collect()
    })
}

/// The rows of the sample of `day`; none if there is no such sample.
pub fn rows(db: &Db, day: i64) -> Result<Vec<Row>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT path, bytes, is_file, mtime FROM disk_samples WHERE day = ?1")?;
        statement
            .query_map(params![day], |row| {
                Ok(Row { path: row.get(0)?, bytes: row.get::<_, i64>(1)? as u64, file: row.get(2)?, mtime: row.get(3)? })
            })?
            .collect()
    })
}

/// Deletes the samples of these days with their rows. Returns how many there were.
pub fn delete_days(db: &Db, days: &[i64]) -> Result<usize> {
    db.transaction(|t| {
        let mut deleted = 0;
        let mut delete = t.prepare("DELETE FROM disk_sample_days WHERE day = ?1")?;
        for day in days {
            deleted += delete.execute(params![day])?;
        }
        Ok(deleted)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, bytes: u64, file: bool) -> Row {
        Row { path: path.to_owned(), bytes, file, mtime: if file { 1_767_225_600_000 } else { 0 } }
    }

    #[test]
    fn samples_are_stored_replaced_and_deleted_with_their_rows() {
        let db = Db::open_in_memory().unwrap();
        assert!(days(&db).unwrap().is_empty());
        let big = 5u64 << 40;
        save(&db, 20_100, 1, &[row(r"C:\", big, false), row(r"C:\Users", 3 << 30, false)]).unwrap();
        save(&db, 20_101, 2, &[row(r"C:\", big + 1, false), row(r"C:\Users\me\film.mkv", 7 << 30, true)]).unwrap();
        assert_eq!(days(&db).unwrap(), [20_100, 20_101]);
        let mut first = rows(&db, 20_100).unwrap();
        first.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(first, [row(r"C:\", big, false), row(r"C:\Users", 3 << 30, false)]);
        assert_eq!(rows(&db, 20_101).unwrap().iter().filter(|r| r.file).count(), 1);

        // Taking a day's sample again replaces it.
        save(&db, 20_100, 3, &[row(r"C:\", 9, false)]).unwrap();
        assert_eq!(rows(&db, 20_100).unwrap(), [row(r"C:\", 9, false)]);

        assert_eq!(delete_days(&db, &[20_100, 19_000]).unwrap(), 1);
        assert_eq!(days(&db).unwrap(), [20_101]);
        assert!(rows(&db, 20_100).unwrap().is_empty());
        let left: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM disk_samples", [], |r| r.get(0))).unwrap();
        assert_eq!(left, 2, "a deleted day's rows go with it");
    }
}
