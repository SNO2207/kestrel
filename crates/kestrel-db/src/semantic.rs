//! AI search ("Describe"): which models the owner downloaded and what their self-test said.
//!
//! The vectors themselves are not here: they live in a memory-mapped store of their own per
//! model (`kestrel-vector`), which a search scans in a few milliseconds. Which files were
//! described, and which couldn't be, is the media library's to know (`media_stages`, the
//! `embedding` stage's records), and the text read from pictures is its too (`media_text`): the
//! describing is a stage of the library's pass. The tables this module once kept for both are
//! dropped (`2026_10_20_semantic_unused_dropped`).

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "2026_10_20_semantic_models",
        sql: "CREATE TABLE semantic_models (
                id TEXT PRIMARY KEY,
                installed_at INTEGER NOT NULL,
                files TEXT NOT NULL,
                selftest TEXT
              );",
    },
    Migration {
        name: "2026_10_20_semantic_skips",
        sql: "CREATE TABLE semantic_skips (
                model TEXT NOT NULL,
                volume INTEGER NOT NULL,
                file_id INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                reason TEXT NOT NULL,
                PRIMARY KEY (model, volume, file_id)
              ) WITHOUT ROWID;",
    },
    Migration {
        name: "2026_10_20_semantic_text",
        sql: "CREATE TABLE semantic_text_keys (
                id INTEGER PRIMARY KEY,
                volume INTEGER NOT NULL,
                file_id INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                UNIQUE (volume, file_id)
              );
              CREATE VIRTUAL TABLE semantic_text USING fts5(body, tokenize = 'unicode61 remove_diacritics 2');",
    },
    // The media library's pass records what was described (and what couldn't be) and holds the
    // text read from pictures, so these went unused when the describing became one of its stages.
    Migration {
        name: "2026_10_20_semantic_unused_dropped",
        sql: "DROP TABLE semantic_skips;
              DROP TABLE semantic_text;
              DROP TABLE semantic_text_keys;",
    },
];

/// A file of an installed model, as verified when it was downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// A downloaded model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub model: String,
    /// Milliseconds since the Unix epoch.
    pub installed_at: i64,
    pub files: Vec<InstalledFile>,
    /// The latest self-test's report, as JSON.
    pub selftest: Option<String>,
}

pub fn installs(db: &Db) -> Result<Vec<Install>> {
    let rows: Vec<(String, i64, String, Option<String>)> = db.with(|c| {
        let mut statement = c.prepare("SELECT id, installed_at, files, selftest FROM semantic_models ORDER BY installed_at")?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?.collect()
    })?;
    rows.into_iter()
        .map(|(model, installed_at, files, selftest)| Ok(Install { model, installed_at, files: serde_json::from_str(&files)?, selftest }))
        .collect()
}

pub fn install(db: &Db, model: &str) -> Result<Option<Install>> {
    Ok(installs(db)?.into_iter().find(|install| install.model == model))
}

/// Records that `model` is downloaded and verified, replacing any earlier record.
pub fn record_install(db: &Db, model: &str, files: &[InstalledFile], at: i64) -> Result<()> {
    let files = serde_json::to_string(files)?;
    db.with(|c| {
        c.execute(
            "INSERT INTO semantic_models (id, installed_at, files, selftest) VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(id) DO UPDATE SET installed_at = excluded.installed_at, files = excluded.files, selftest = NULL",
            params![model, at, files],
        )
    })?;
    Ok(())
}

pub fn set_selftest(db: &Db, model: &str, report: &str) -> Result<()> {
    db.with(|c| c.execute("UPDATE semantic_models SET selftest = ?2 WHERE id = ?1", params![model, report]))?;
    Ok(())
}

/// Forgets a model.
pub fn forget_install(db: &Db, model: &str) -> Result<()> {
    db.with(|c| c.execute("DELETE FROM semantic_models WHERE id = ?1", params![model]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_are_recorded_tested_and_forgotten() {
        let db = Db::open_in_memory().unwrap();
        assert!(installs(&db).unwrap().is_empty());
        let files = vec![InstalledFile { path: "onnx/vision_model_fp16.onnx".into(), size: 10, sha256: "ab".into() }];
        record_install(&db, "fast", &files, 1000).unwrap();
        set_selftest(&db, "fast", "{\"passed\":true}").unwrap();
        let fast = install(&db, "fast").unwrap().unwrap();
        assert_eq!((fast.installed_at, fast.files.clone(), fast.selftest.as_deref()), (1000, files.clone(), Some("{\"passed\":true}")));
        // Installing again starts with no self-test.
        record_install(&db, "fast", &files, 2000).unwrap();
        assert_eq!(install(&db, "fast").unwrap().unwrap().selftest, None);
        forget_install(&db, "fast").unwrap();
        assert!(install(&db, "fast").unwrap().is_none());
        assert_eq!(installs(&db).unwrap().len(), 0);
    }
}
