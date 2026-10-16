//! Explorer parity's data: the files opened through Kestrel, which Home lists among the recent
//! files, and the migration that made folder visits one record shared by the frequent folders
//! (`shortcuts::frecency`) and the command palette's recency (`palette`).
//!
//! # One record of folder visits
//!
//! Two trackers used to count visits: the shortcuts feature recorded each folder a tab
//! arrived in (`folder_visits`), and the palette recorded each folder a tab stayed in for a
//! moment as a use of a `files` item (`palette_recent`). A visit now goes into `folder_visits`
//! only, and the palette reads those rows as uses of the folders ([`crate::palette::recent`]).
//! Folders chosen from the palette are still remembered there as choices.
//!
//! The migration [`VISITS`] moves the folder rows the palette had into `folder_visits`. Where
//! both had the folder, both counted the same visits, so the larger count and the later time
//! are kept rather than their sum. Keys are folded by `kestrel_fold`, the same folding
//! `shortcuts::frecency` uses, registered on every connection by [`register_functions`]:
//! SQLite's own `lower()` folds ASCII only, and would key `Ü` apart from `ü`.

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Db, Migration, Result};

/// Every migration of the parity feature, listed once in [`crate::MIGRATIONS`].
pub const MIGRATIONS: &[Migration] = &[VISITS, OPENED];

/// Carries the palette's remembered folder visits over into `folder_visits`.
pub const VISITS: Migration = Migration {
    name: "2026_10_18_parity_visits",
    sql: "
        INSERT INTO folder_visits (key, path, rank, visits, last_visit)
        SELECT kestrel_fold(item_id), kestrel_tidy(item_id), uses, uses, last_used / 1000
        FROM palette_recent
        WHERE provider = 'files' AND dir = 1 AND trim(item_id) <> ''
        ON CONFLICT(key) DO UPDATE SET
            rank = max(rank, excluded.rank),
            visits = max(visits, excluded.visits),
            last_visit = max(last_visit, excluded.last_visit);
        DELETE FROM palette_recent WHERE provider = 'files' AND dir = 1;
    ",
};

/// Files opened through Kestrel, for Home's recent files.
pub const OPENED: Migration = Migration {
    name: "2026_10_18_parity_opened",
    sql: "
        CREATE TABLE opened_files (
            key TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            opens INTEGER NOT NULL,
            last_opened INTEGER NOT NULL
        );
        CREATE INDEX opened_files_by_time ON opened_files (last_opened);
    ",
};

/// Files remembered as opened; the ones opened longest ago go first.
pub const MAX_OPENED: usize = 500;

/// A path with backslashes and no trailing separator, except on a drive root: the spelling
/// `shortcuts::frecency` keeps.
pub fn tidy(path: &str) -> String {
    let mut path = path.trim().replace('/', "\\");
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    path
}

/// How a path is keyed: tidy, and folded as Windows compares names.
pub fn fold(path: &str) -> String {
    tidy(path).to_lowercase()
}

/// Registers the SQL functions the migrations use. Called on every connection before they run.
pub fn register_functions(connection: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    connection.create_scalar_function("kestrel_fold", 1, flags, |context| Ok(fold(&context.get::<String>(0)?)))?;
    connection.create_scalar_function("kestrel_tidy", 1, flags, |context| Ok(tidy(&context.get::<String>(0)?)))
}

/// A file opened through Kestrel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub path: String,
    pub opens: i64,
    /// Milliseconds since the Unix epoch.
    pub last_opened: i64,
}

/// Records that `path` was opened at `now` (milliseconds since the Unix epoch).
pub fn record_opened(db: &Db, path: &str, now: i64) -> Result<()> {
    let path = tidy(path);
    if path.is_empty() {
        return Ok(());
    }
    db.transaction(|t| {
        t.execute(
            "INSERT INTO opened_files (key, path, opens, last_opened) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(key) DO UPDATE SET path = excluded.path, opens = opens + 1,
               last_opened = max(last_opened, excluded.last_opened)",
            params![fold(&path), path, now],
        )?;
        t.execute(
            "DELETE FROM opened_files WHERE key NOT IN
               (SELECT key FROM opened_files ORDER BY last_opened DESC LIMIT ?1)",
            params![MAX_OPENED as i64],
        )?;
        Ok(())
    })
}

/// The files opened most recently, newest first, at most `limit`.
pub fn opened(db: &Db, limit: usize) -> Result<Vec<Opened>> {
    db.with(|c| {
        c.prepare("SELECT path, opens, last_opened FROM opened_files ORDER BY last_opened DESC, key LIMIT ?1")?
            .query_map(params![limit as i64], |row| Ok(Opened { path: row.get(0)?, opens: row.get(1)?, last_opened: row.get(2)? }))?
            .collect()
    })
}

/// Forgets a file, as when it is taken off Home's recent files.
pub fn forget_opened(db: &Db, path: &str) -> Result<bool> {
    Ok(db.with(|c| c.execute("DELETE FROM opened_files WHERE key = ?1", params![fold(path)]))? > 0)
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;
    use crate::palette::{self, Use};
    use crate::shortcuts::frecency;
    use crate::{MIGRATIONS as ALL, migrate};

    const NOW_MS: i64 = 1_800_000_000_000;

    fn folder_use(path: &str) -> Use {
        Use {
            provider: "files".into(),
            id: path.into(),
            title: path.rsplit('\\').next().unwrap_or(path).into(),
            subtitle: None,
            path: Some(path.into()),
            dir: true,
        }
    }

    /// A database as it was before this feature: every other feature's migrations applied.
    fn before_parity() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        register_functions(&connection).unwrap();
        let others: Vec<&[Migration]> =
            ALL.iter().copied().filter(|set| !set.iter().any(|migration| migration.name.contains("_parity_"))).collect();
        migrate(&mut connection, &others).unwrap();
        connection
    }

    #[test]
    fn the_palettes_folder_visits_move_into_the_shared_record() {
        let connection = before_parity();
        let db = Db { connection: Mutex::new(connection) };
        // Both trackers saw Music; the palette alone saw Über (spelled with a trailing
        // separator) and the temp folder; the shortcuts alone saw Projects.
        frecency::record(&db, r"C:\Users\me\Music", 1_700_000_000).unwrap();
        for _ in 0..3 {
            palette::record(&db, &folder_use(r"C:\Users\me\Music"), NOW_MS).unwrap();
        }
        palette::record(&db, &folder_use(r"C:\Daten\Über\"), NOW_MS - 5_000).unwrap();
        frecency::record(&db, r"C:\Projects", 1_750_000_000).unwrap();
        // A file and an action the palette remembers stay where they are.
        let file = Use { dir: false, ..folder_use(r"C:\Notes\todo.txt") };
        palette::record(&db, &file, NOW_MS).unwrap();
        let action = Use { provider: "actions".into(), id: "new-tab".into(), title: "New tab".into(), ..Use::default() };
        palette::record(&db, &action, NOW_MS).unwrap();

        let mut connection = db.connection.lock();
        migrate(&mut connection, ALL).unwrap();
        drop(connection);

        let ranked = frecency::ranked(&db, NOW_MS / 1000).unwrap();
        let find = |path: &str| ranked.iter().find(|row| row.path == path).cloned();
        let music = find(r"C:\Users\me\Music").expect("Music is kept");
        assert_eq!((music.visits, music.rank, music.last_visit), (3, 3.0, NOW_MS / 1000), "the larger count, the later time");
        let uber = find(r"C:\Daten\Über").expect("a folder only the palette knew is carried over");
        assert_eq!((uber.visits, uber.last_visit), (1, (NOW_MS - 5_000) / 1000));
        assert!(find(r"C:\Projects").is_some(), "visits only the shortcuts knew are untouched");
        assert_eq!(ranked.len(), 3);
        // Keyed as frecency keys them, so a later visit lands on the same row.
        frecency::record(&db, r"c:\daten\über", NOW_MS / 1000).unwrap();
        assert_eq!(frecency::ranked(&db, NOW_MS / 1000).unwrap().len(), 3);

        // The palette keeps its file and action, and reads the folders from the shared record.
        let recent = palette::recent(&db).unwrap();
        let keys: Vec<&str> = recent.iter().map(|row| row.key.as_str()).collect();
        assert!(keys.contains(&"files:c:\\notes\\todo.txt") && keys.contains(&"actions:new-tab"), "{keys:?}");
        let music = recent.iter().find(|row| row.key == "files:c:\\users\\me\\music").expect("visits are uses");
        assert_eq!((music.uses, music.last_used, music.item.dir), (3, NOW_MS, true));
        assert_eq!((music.item.title.as_str(), music.item.subtitle.as_deref()), ("Music", Some(r"C:\Users\me")));
        let rows_in_palette: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM palette_recent WHERE dir = 1", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(rows_in_palette, 0, "no folder visit is kept twice");
    }

    #[test]
    fn a_visit_is_recorded_once_and_both_read_it() {
        let db = Db::open_in_memory().unwrap();
        frecency::record(&db, r"D:\Photos", NOW_MS / 1000).unwrap();
        let recent = palette::recent(&db).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!((recent[0].key.as_str(), recent[0].uses), ("files:d:\\photos", 1));
        // Choosing the folder in the palette adds a use to the same item rather than another row.
        palette::record(&db, &folder_use(r"D:\Photos"), NOW_MS + 1).unwrap();
        let recent = palette::recent(&db).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!((recent[0].uses, recent[0].last_used), (2, NOW_MS + 1));
        // A drive root keeps its separator and is named after the drive.
        frecency::record(&db, r"E:\", NOW_MS / 1000 - 10).unwrap();
        let root = palette::recent(&db).unwrap().into_iter().find(|row| row.key == "files:e:\\").unwrap();
        assert_eq!((root.item.title.as_str(), root.item.subtitle), (r"E:\", None));
    }

    #[test]
    fn opened_files_are_kept_newest_first_and_forgotten_on_request() {
        let db = Db::open_in_memory().unwrap();
        record_opened(&db, r"C:\Notes\a.txt", 1_000).unwrap();
        record_opened(&db, r"C:\Notes\b.txt", 2_000).unwrap();
        record_opened(&db, r"c:\notes\A.TXT", 3_000).unwrap();
        let list = opened(&db, 10).unwrap();
        assert_eq!(list.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(), [r"c:\notes\A.TXT", r"C:\Notes\b.txt"]);
        assert_eq!(list[0].opens, 2);
        assert_eq!(opened(&db, 1).unwrap().len(), 1);
        assert!(forget_opened(&db, r"C:\NOTES\a.txt").unwrap());
        assert!(!forget_opened(&db, r"C:\NOTES\a.txt").unwrap());
        record_opened(&db, "  ", 4_000).unwrap();
        assert_eq!(opened(&db, 10).unwrap().len(), 1);
        for i in 0..(MAX_OPENED + 3) {
            record_opened(&db, &format!(r"C:\F\{i}.txt"), 10_000 + i as i64).unwrap();
        }
        assert_eq!(opened(&db, MAX_OPENED * 2).unwrap().len(), MAX_OPENED);
    }

    #[test]
    fn paths_fold_as_frecency_folds_them() {
        assert_eq!(tidy(r"C:/Users/me/ "), r"C:\Users\me");
        assert_eq!(tidy(r"D:\"), r"D:\");
        assert_eq!(fold(r"C:\ÜBER\"), r"c:\über");
    }
}
