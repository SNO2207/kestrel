//! AI search ("Describe"): which models the owner downloaded and what their self-test said,
//! the files a model couldn't read (so they aren't tried again until they change), and the text
//! read from pictures, searchable through FTS5.
//!
//! The vectors themselves are not here: they live in a memory-mapped store of their own per
//! model (`kestrel-vector`), which a search scans in a few milliseconds. Files are known by
//! volume and file id with the size and modification time they had when they were read, so a
//! renamed or moved file is never read again and a changed one always is.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
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

/// A file as it was when it was read: which file, and its size and time then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileState {
    pub volume: u32,
    pub file_id: u64,
    pub size: u64,
    pub mtime: i64,
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

/// Forgets a model: its record and the files it couldn't read.
pub fn forget_install(db: &Db, model: &str) -> Result<()> {
    db.transaction(|t| {
        t.execute("DELETE FROM semantic_models WHERE id = ?1", params![model])?;
        t.execute("DELETE FROM semantic_skips WHERE model = ?1", params![model])?;
        Ok(())
    })
}

/// Notes that `model` couldn't read a file as it is now, and why.
pub fn skip(db: &Db, model: &str, file: &FileState, reason: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT OR REPLACE INTO semantic_skips (model, volume, file_id, size, mtime, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![model, file.volume, file.file_id as i64, file.size as i64, file.mtime, reason],
        )
    })?;
    Ok(())
}

/// Every file `model` couldn't read, with its size and time then: skipped while it stays so.
pub fn skips(db: &Db, model: &str) -> Result<HashMap<(u32, u64), (u64, i64)>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT volume, file_id, size, mtime FROM semantic_skips WHERE model = ?1")?;
        statement
            .query_map(params![model], |row| {
                Ok(((row.get::<_, u32>(0)?, row.get::<_, i64>(1)? as u64), (row.get::<_, i64>(2)? as u64, row.get(3)?)))
            })?
            .collect()
    })
}

pub fn skip_count(db: &Db, model: &str) -> Result<u64> {
    db.with(|c| c.query_row("SELECT COUNT(*) FROM semantic_skips WHERE model = ?1", params![model], |row| row.get::<_, i64>(0)))
        .map(|n| n as u64)
}

/// Stores the text read from a file, replacing what was stored for it.
pub fn set_text(db: &Db, file: &FileState, body: &str) -> Result<()> {
    db.transaction(|t| {
        let old: Option<i64> = t
            .query_row("SELECT id FROM semantic_text_keys WHERE volume = ?1 AND file_id = ?2", params![file.volume, file.file_id as i64], |row| row.get(0))
            .optional()?;
        if let Some(id) = old {
            t.execute("DELETE FROM semantic_text WHERE rowid = ?1", params![id])?;
            t.execute("DELETE FROM semantic_text_keys WHERE id = ?1", params![id])?;
        }
        t.execute(
            "INSERT INTO semantic_text_keys (volume, file_id, size, mtime) VALUES (?1, ?2, ?3, ?4)",
            params![file.volume, file.file_id as i64, file.size as i64, file.mtime],
        )?;
        let id = t.last_insert_rowid();
        t.execute("INSERT INTO semantic_text (rowid, body) VALUES (?1, ?2)", params![id, body])?;
        Ok(())
    })
}

/// The files whose text is stored, with the size and time they had when it was read.
pub fn text_files(db: &Db) -> Result<HashMap<(u32, u64), (u64, i64)>> {
    db.with(|c| {
        let mut statement = c.prepare("SELECT volume, file_id, size, mtime FROM semantic_text_keys")?;
        statement
            .query_map([], |row| Ok(((row.get::<_, u32>(0)?, row.get::<_, i64>(1)? as u64), (row.get::<_, i64>(2)? as u64, row.get(3)?))))?
            .collect()
    })
}

/// Forgets the text of a file that went.
pub fn forget_text(db: &Db, volume: u32, file_id: u64) -> Result<()> {
    db.transaction(|t| {
        let id: Option<i64> = t
            .query_row("SELECT id FROM semantic_text_keys WHERE volume = ?1 AND file_id = ?2", params![volume, file_id as i64], |row| row.get(0))
            .optional()?;
        if let Some(id) = id {
            t.execute("DELETE FROM semantic_text WHERE rowid = ?1", params![id])?;
            t.execute("DELETE FROM semantic_text_keys WHERE id = ?1", params![id])?;
        }
        Ok(())
    })
}

/// A file whose text has some of the words looked for.
#[derive(Clone, Debug, PartialEq)]
pub struct TextHit {
    pub file: FileState,
    pub body: String,
}

/// Up to `limit` files whose text has any of `words` (whole words, any case or accent).
pub fn search_text(db: &Db, words: &[String], limit: usize) -> Result<Vec<TextHit>> {
    let terms: Vec<String> = words
        .iter()
        .map(|word| word.chars().filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-').collect::<String>())
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let query = terms.join(" OR ");
    db.with(|c| {
        let mut statement = c.prepare(
            "SELECT k.volume, k.file_id, k.size, k.mtime, t.body FROM semantic_text t
             JOIN semantic_text_keys k ON k.id = t.rowid
             WHERE semantic_text MATCH ?1 ORDER BY bm25(semantic_text) LIMIT ?2",
        )?;
        statement
            .query_map(params![query, limit as i64], |row| {
                Ok(TextHit {
                    file: FileState { volume: row.get(0)?, file_id: row.get::<_, i64>(1)? as u64, size: row.get::<_, i64>(2)? as u64, mtime: row.get(3)? },
                    body: row.get(4)?,
                })
            })?
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u64) -> FileState {
        FileState { volume: b'C' as u32, file_id: id, size: 100 + id, mtime: 7 }
    }

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
        skip(&db, "fast", &file(1), "not a picture").unwrap();
        skip(&db, "best", &file(1), "not a picture").unwrap();
        assert_eq!(skip_count(&db, "fast").unwrap(), 1);
        forget_install(&db, "fast").unwrap();
        assert!(install(&db, "fast").unwrap().is_none());
        assert_eq!(skip_count(&db, "fast").unwrap(), 0, "its skips go with it");
        assert_eq!(skips(&db, "best").unwrap()[&(b'C' as u32, 1)], (101, 7));
    }

    #[test]
    fn text_read_from_pictures_is_searched_by_word() {
        let db = Db::open_in_memory().unwrap();
        set_text(&db, &file(1), "COSTCO WHOLESALE TOTAL 42.17").unwrap();
        set_text(&db, &file(2), "Café menu: crème brûlée").unwrap();
        set_text(&db, &file(3), "nothing to see").unwrap();
        let found = |words: &[&str]| -> Vec<u64> {
            search_text(&db, &words.iter().map(|w| w.to_string()).collect::<Vec<_>>(), 10).unwrap().iter().map(|hit| hit.file.file_id).collect()
        };
        assert_eq!(found(&["costco"]), [1]);
        assert_eq!(found(&["42.17"]), [1], "numbers with a point are one word");
        assert_eq!(found(&["creme"]), [2], "accents don't matter");
        let mut either = found(&["costco", "menu"]);
        either.sort();
        assert_eq!(either, [1, 2]);
        assert!(found(&["\"); DROP TABLE x; --"]).is_empty(), "quotes can't break the query");
        assert!(found(&[]).is_empty());
        // Replacing and forgetting.
        set_text(&db, &FileState { size: 999, ..file(1) }, "a new receipt").unwrap();
        assert!(found(&["costco"]).is_empty());
        assert_eq!(text_files(&db).unwrap()[&(b'C' as u32, 1)], (999, 7));
        forget_text(&db, b'C' as u32, 1).unwrap();
        assert!(found(&["receipt"]).is_empty());
        assert_eq!(text_files(&db).unwrap().len(), 2);
    }
}
