//! Kestrel's own data, as opposed to the file system's: pinned places, projects, tags,
//! history and settings, kept in one SQLite database in the app's data folder.
//!
//! Features own their tables. Each declares its schema as [`Migration`]s in its own module and
//! lists them in [`MIGRATIONS`]; a migration runs once, in name order, and is recorded by name,
//! so features developed side by side never compete for a version number. Name migrations
//! `<yyyy_mm_dd>_<feature>_<what>` and never edit one that has shipped: add another.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use rusqlite;

pub mod projects;

/// One schema change.
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, from every feature. Order here does not matter; names decide.
pub const MIGRATIONS: &[&[Migration]] = &[SETTINGS, projects::MIGRATIONS];

const SETTINGS: &[Migration] = &[Migration {
    name: "2026_10_15_core_settings",
    sql: "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
}];

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

/// Lets Tauri commands return `Result<_, String>` with `?`.
impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.0
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The database. Cheap to share behind an `Arc`; calls are serialised on one connection,
/// which is plenty for the small amounts of data the app keeps.
pub struct Db {
    connection: Mutex<Connection>,
}

impl Db {
    /// Opens (creating if needed) the database at `path` and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error(format!("creating {}: {e}", dir.display())))?;
        }
        let connection = Connection::open(path)?;
        // Write-ahead logging keeps readers and the single writer from blocking each other,
        // and NORMAL sync is durable across app crashes, which is what matters here.
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        Self::prepare(connection)
    }

    /// A private database in memory, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Self::prepare(connection)
    }

    fn prepare(mut connection: Connection) -> Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut connection, MIGRATIONS)?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// Runs `f` with the connection.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.connection.lock())?)
    }

    /// Runs `f` inside a transaction that commits if `f` succeeds and rolls back otherwise.
    pub fn transaction<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>) -> Result<T> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let value = f(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// A setting, stored as JSON.
    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| row.get(0)).optional()
        })?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, raw],
            )
        })?;
        Ok(())
    }
}

/// Applies, in name order, every migration not yet recorded, each in its own transaction.
fn migrate(connection: &mut Connection, sets: &[&[Migration]]) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL DEFAULT (unixepoch()));",
    )?;
    let mut all: Vec<&Migration> = sets.iter().flat_map(|set| set.iter()).collect();
    all.sort_by_key(|m| m.name);
    if let Some(pair) = all.windows(2).find(|pair| pair[0].name == pair[1].name) {
        return Err(Error(format!("two migrations are named {}", pair[0].name)));
    }
    for migration in all {
        let applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE name = ?1)",
            params![migration.name],
            |row| row.get(0),
        )?;
        if applied {
            continue;
        }
        let transaction = connection.transaction()?;
        transaction
            .execute_batch(migration.sql)
            .map_err(|e| Error(format!("migration {} failed: {e}", migration.name)))?;
        transaction.execute("INSERT INTO schema_migrations (name) VALUES (?1)", params![migration.name])?;
        transaction.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_as_json() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.setting::<bool>("show_hidden").unwrap(), None);
        db.set_setting("show_hidden", &true).unwrap();
        db.set_setting("columns", &vec!["name", "size"]).unwrap();
        db.set_setting("show_hidden", &false).unwrap();
        assert_eq!(db.setting::<bool>("show_hidden").unwrap(), Some(false));
        assert_eq!(db.setting::<Vec<String>>("columns").unwrap(), Some(vec!["name".into(), "size".into()]));
    }

    #[test]
    fn migrations_run_once_in_name_order_and_survive_reopening() {
        let path = std::env::temp_dir().join(format!("kestrel-db-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        const LATER: &[Migration] = &[Migration { name: "2026_10_16_b", sql: "ALTER TABLE things ADD COLUMN size INTEGER;" }];
        const EARLIER: &[Migration] = &[Migration { name: "2026_10_16_a", sql: "CREATE TABLE things (name TEXT);" }];
        {
            let mut connection = Connection::open(&path).unwrap();
            // Listed out of order on purpose: names decide.
            migrate(&mut connection, &[LATER, EARLIER]).unwrap();
            migrate(&mut connection, &[LATER, EARLIER]).unwrap();
            connection.execute("INSERT INTO things (name, size) VALUES ('a', 1)", []).unwrap();
        }
        let db = Db::open(&path).unwrap();
        let count: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))).unwrap();
        let features: usize = MIGRATIONS.iter().map(|set| set.len()).sum();
        assert_eq!(count as usize, 2 + features, "the two test migrations and every feature's");
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn duplicate_names_and_failing_migrations_are_errors() {
        const DUPLICATE: &[Migration] = &[
            Migration { name: "x", sql: "CREATE TABLE a (b);" },
            Migration { name: "x", sql: "CREATE TABLE c (d);" },
        ];
        let mut connection = Connection::open_in_memory().unwrap();
        assert!(migrate(&mut connection, &[DUPLICATE]).is_err());

        const BROKEN: &[Migration] = &[Migration { name: "broken", sql: "CREATE TABLE (" }];
        let mut connection = Connection::open_in_memory().unwrap();
        let error = migrate(&mut connection, &[BROKEN]).unwrap_err().to_string();
        assert!(error.contains("broken"), "{error}");
    }

    #[test]
    fn transactions_roll_back_on_error() {
        let db = Db::open_in_memory().unwrap();
        let failed = db.transaction(|t| {
            t.execute("INSERT INTO settings (key, value) VALUES ('k', '1')", [])?;
            t.execute("INSERT INTO nowhere VALUES (1)", [])
        });
        assert!(failed.is_err());
        assert_eq!(db.setting::<i64>("k").unwrap(), None);
    }
}
