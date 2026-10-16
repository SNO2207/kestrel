//! The developer tools' data: the terminal pane's and the query service's settings, and a short
//! log of what other programs asked the query service (which the settings page shows, so the
//! owner can see what, say, Claude Code looked up).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, Migration, Result};

/// Every migration of the developer tools, listed once in [`crate::MIGRATIONS`].
pub const MIGRATIONS: &[Migration] = &[SETTINGS, QUERIES];

/// The settings, one JSON document, so a new setting needs no migration.
pub const SETTINGS: Migration = Migration {
    name: "2026_10_20_devtools_settings",
    sql: "CREATE TABLE devtools_settings (id INTEGER PRIMARY KEY CHECK (id = 1), document TEXT NOT NULL);",
};

/// What other programs asked, newest kept.
pub const QUERIES: Migration = Migration {
    name: "2026_10_20_devtools_queries",
    sql: "
        CREATE TABLE devtools_queries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at INTEGER NOT NULL,
            client TEXT NOT NULL,
            op TEXT NOT NULL,
            summary TEXT NOT NULL,
            ok INTEGER NOT NULL,
            took_ms REAL NOT NULL
        );
        CREATE INDEX devtools_queries_by_time ON devtools_queries (at);
    ",
};

/// Queries kept in the log; older ones go.
pub const MAX_QUERIES: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The shell new terminals start (`pwsh`, `powershell`, `cmd`, `wsl:<name>`); the default
    /// shell (PowerShell 7 if installed, else Windows PowerShell) when unset.
    pub default_shell: Option<String>,
    /// The projects hub's Run buttons run scripts in the terminal pane rather than in a new
    /// Windows Terminal.
    pub scripts_in_terminal: bool,
    /// Other programs of this account may ask Kestrel questions (search, folder sizes...).
    pub query_service: bool,
    /// Their questions are kept in the log.
    pub log_queries: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { default_shell: None, scripts_in_terminal: false, query_service: true, log_queries: true }
    }
}

pub fn settings(db: &Db) -> Result<Settings> {
    let document: Option<String> =
        db.with(|c| c.query_row("SELECT document FROM devtools_settings WHERE id = 1", [], |row| row.get(0)).optional())?;
    // A document that no longer reads (an older or damaged one) gives the defaults.
    Ok(document.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
}

pub fn set_settings(db: &Db, settings: &Settings) -> Result<()> {
    let document = serde_json::to_string(settings)?;
    db.with(|c| {
        c.execute(
            "INSERT INTO devtools_settings (id, document) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET document = excluded.document",
            params![document],
        )
    })?;
    Ok(())
}

/// One question another program asked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    /// The asking program, such as `kestrel-mcp.exe`.
    pub client: String,
    /// Which question: `search`, `list_folder`...
    pub op: String,
    /// What it was about: the search, the folder.
    pub summary: String,
    pub ok: bool,
    pub took_ms: f64,
}

/// Adds a question to the log, letting the oldest go past [`MAX_QUERIES`].
pub fn record_query(db: &Db, query: &Query) -> Result<()> {
    db.transaction(|t| {
        t.execute(
            "INSERT INTO devtools_queries (at, client, op, summary, ok, took_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![query.at, query.client, query.op, query.summary, query.ok, query.took_ms],
        )?;
        t.execute(
            "DELETE FROM devtools_queries WHERE id NOT IN (SELECT id FROM devtools_queries ORDER BY id DESC LIMIT ?1)",
            params![MAX_QUERIES as i64],
        )?;
        Ok(())
    })
}

/// The latest questions, newest first.
pub fn recent_queries(db: &Db, limit: usize) -> Result<Vec<Query>> {
    db.with(|c| {
        c.prepare("SELECT at, client, op, summary, ok, took_ms FROM devtools_queries ORDER BY id DESC LIMIT ?1")?
            .query_map(params![limit as i64], |row| {
                Ok(Query { at: row.get(0)?, client: row.get(1)?, op: row.get(2)?, summary: row.get(3)?, ok: row.get(4)?, took_ms: row.get(5)? })
            })?
            .collect()
    })
}

/// Empties the log. Returns how many questions it held.
pub fn clear_queries(db: &Db) -> Result<usize> {
    db.with(|c| c.execute("DELETE FROM devtools_queries", []))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_start_with_the_service_on_and_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let defaults = settings(&db).unwrap();
        assert!(defaults.query_service && defaults.log_queries && !defaults.scripts_in_terminal);
        assert_eq!(defaults.default_shell, None);
        let changed = Settings { default_shell: Some("cmd".into()), scripts_in_terminal: true, query_service: false, log_queries: false };
        set_settings(&db, &changed).unwrap();
        assert_eq!(settings(&db).unwrap(), changed);
        // Settings a newer version added, or left out, take their defaults.
        db.with(|c| c.execute("UPDATE devtools_settings SET document = '{\"scripts_in_terminal\":true,\"later\":1}'", [])).unwrap();
        assert_eq!(settings(&db).unwrap(), Settings { scripts_in_terminal: true, ..Settings::default() });
        db.with(|c| c.execute("UPDATE devtools_settings SET document = 'not json'", [])).unwrap();
        assert_eq!(settings(&db).unwrap(), Settings::default());
    }

    #[test]
    fn the_query_log_keeps_the_newest() {
        let db = Db::open_in_memory().unwrap();
        for n in 0..(MAX_QUERIES + 25) {
            let query = Query { at: n as i64, client: "kestrel-mcp.exe".into(), op: "search".into(), summary: format!("q{n}"), ok: n % 2 == 0, took_ms: 1.5 };
            record_query(&db, &query).unwrap();
        }
        let kept = recent_queries(&db, 1000).unwrap();
        assert_eq!(kept.len(), MAX_QUERIES);
        assert_eq!(kept[0].summary, format!("q{}", MAX_QUERIES + 24));
        assert_eq!(kept.last().unwrap().summary, "q25");
        assert_eq!(recent_queries(&db, 3).unwrap().len(), 3);
        assert_eq!(clear_queries(&db).unwrap(), MAX_QUERIES);
        assert!(recent_queries(&db, 10).unwrap().is_empty());
    }
}
