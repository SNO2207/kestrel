//! The media library (`kestrel-media`): every photo and video in the owner's media scopes, what
//! the property system says about each, which of the pipeline's stages have done their work on
//! it, and the text recognised in images.
//!
//! An item is a file known by its volume and file id, so a rename or a move keeps its row, its
//! facts and its text. Each stage's record names the size and last write time the stage worked
//! from: when the file's content changes, those no longer match and the stage runs again. Items
//! that leave the scopes are marked gone rather than dropped, so a file that comes back (from
//! the Recycle Bin, say) costs nothing; rows gone for a long while are pruned.
//!
//! Recognised text lives in an FTS5 table whose rowid is the item's id, which is what `text:`
//! searches match against.

use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "2026_10_20_media_items",
        sql: "CREATE TABLE media_items (
                id INTEGER PRIMARY KEY,
                volume TEXT NOT NULL,
                file_id INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                path TEXT NOT NULL,
                kind INTEGER NOT NULL,
                gone INTEGER,
                width INTEGER,
                height INTEGER,
                orientation INTEGER,
                taken INTEGER,
                make TEXT,
                model TEXT,
                latitude REAL,
                longitude REAL,
                duration_ms INTEGER,
                frame_rate REAL,
                codec TEXT,
                UNIQUE (volume, file_id)
              );
              CREATE INDEX media_items_taken ON media_items (taken);
              CREATE TABLE media_stages (
                item INTEGER NOT NULL REFERENCES media_items (id) ON DELETE CASCADE,
                stage TEXT NOT NULL,
                version INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                failed TEXT,
                PRIMARY KEY (item, stage)
              ) WITHOUT ROWID;",
    },
    Migration {
        name: "2026_10_20_media_text",
        sql: "CREATE VIRTUAL TABLE media_text USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');",
    },
];

/// Photo or video, as stored in `media_items.kind`.
pub const PHOTO: u8 = 1;
pub const VIDEO: u8 = 2;

/// What the Windows property system says about a photo or video. Dimensions are as the item is
/// shown, after its orientation is applied.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// EXIF orientation, 1 to 8: how the stored pixels are turned to be shown.
    pub orientation: Option<u16>,
    /// When the photo was taken or the video recorded, as a FILETIME (UTC).
    pub taken: Option<i64>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<f64>,
    pub codec: Option<String>,
}

/// One stage's record on an item: the version of the stage and the content it worked from, and
/// why it could not do its work, if it could not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageRecord {
    pub stage: String,
    pub version: u32,
    pub size: u64,
    pub mtime: i64,
    pub failed: Option<String>,
}

/// An item as the library knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub id: i64,
    pub volume: String,
    pub file_id: u64,
    pub size: u64,
    /// Last write time as a FILETIME.
    pub mtime: i64,
    pub path: String,
    pub kind: u8,
    pub gone: bool,
    pub stages: Vec<StageRecord>,
}

/// An item found in the scopes, to be added or brought up to date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub volume: String,
    pub file_id: u64,
    pub size: u64,
    pub mtime: i64,
    pub path: String,
    pub kind: u8,
}

/// What the Gallery and the details view's columns need of an item.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub id: i64,
    pub volume: String,
    pub file_id: u64,
    pub kind: u8,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub taken: Option<i64>,
    pub duration_ms: Option<u64>,
}

/// A condition on items' facts: the media clauses of a search, resolved to items.
#[derive(Clone, Debug, PartialEq)]
pub enum Where {
    /// Shown width in pixels, inclusive.
    Width(u32, u32),
    Height(u32, u32),
    /// When taken, as a half-open range of FILETIMEs.
    Taken(i64, i64),
    /// The camera's make or model contains this text, compared without case.
    Camera(String),
    /// Length in milliseconds, inclusive.
    Duration(u64, u64),
    Kind(u8),
    /// Whether a GPS position is known.
    Geo(bool),
}

fn i64_of(value: u64) -> i64 {
    value as i64
}

/// Every item the library knows, with its stages' records.
pub fn known(db: &Db) -> Result<Vec<Known>> {
    db.with(|c| {
        let mut items: Vec<Known> = c
            .prepare("SELECT id, volume, file_id, size, mtime, path, kind, gone FROM media_items ORDER BY id")?
            .query_map([], |row| {
                Ok(Known {
                    id: row.get(0)?,
                    volume: row.get(1)?,
                    file_id: row.get::<_, i64>(2)? as u64,
                    size: row.get::<_, i64>(3)? as u64,
                    mtime: row.get(4)?,
                    path: row.get(5)?,
                    kind: row.get(6)?,
                    gone: row.get::<_, Option<i64>>(7)?.is_some(),
                    stages: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut statement = c.prepare("SELECT item, stage, version, size, mtime, failed FROM media_stages ORDER BY item")?;
        let mut rows = statement.query([])?;
        // Both are in id order, so the records are dealt out in one pass.
        let mut at = 0;
        while let Some(row) = rows.next()? {
            let item: i64 = row.get(0)?;
            while at < items.len() && items[at].id < item {
                at += 1;
            }
            if at < items.len() && items[at].id == item {
                items[at].stages.push(StageRecord {
                    stage: row.get(1)?,
                    version: row.get(2)?,
                    size: row.get::<_, i64>(3)? as u64,
                    mtime: row.get(4)?,
                    failed: row.get(5)?,
                });
            }
        }
        Ok(items)
    })
}

/// Adds items, or brings the ones already known (by volume and file id) up to date with their
/// size, time, path and kind, no longer gone. Returns each one's id, in order.
pub fn add(db: &Db, found: &[Found]) -> Result<Vec<i64>> {
    db.transaction(|t| {
        let mut upsert = t.prepare_cached(
            "INSERT INTO media_items (volume, file_id, size, mtime, path, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (volume, file_id) DO UPDATE SET
               size = excluded.size, mtime = excluded.mtime, path = excluded.path, kind = excluded.kind, gone = NULL
             RETURNING id",
        )?;
        found
            .iter()
            .map(|item| {
                upsert.query_row(
                    params![item.volume, i64_of(item.file_id), i64_of(item.size), item.mtime, item.path, item.kind],
                    |row| row.get(0),
                )
            })
            .collect()
    })
}

/// Records where items are now and what their content is: `(id, size, mtime, path)`. A rename
/// or a move changes only the path.
pub fn update(db: &Db, changes: &[(i64, u64, i64, String)]) -> Result<()> {
    db.transaction(|t| {
        let mut update =
            t.prepare_cached("UPDATE media_items SET size = ?2, mtime = ?3, path = ?4, gone = NULL WHERE id = ?1")?;
        for (id, size, mtime, path) in changes {
            update.execute(params![id, i64_of(*size), mtime, path])?;
        }
        Ok(())
    })
}

/// Marks items as no longer in the scopes since `when` (seconds since the Unix epoch).
pub fn mark_gone(db: &Db, ids: &[i64], when: i64) -> Result<()> {
    db.transaction(|t| {
        let mut mark = t.prepare_cached("UPDATE media_items SET gone = ?2 WHERE id = ?1 AND gone IS NULL")?;
        for id in ids {
            mark.execute(params![id, when])?;
        }
        Ok(())
    })
}

/// Forgets the items gone since before `before` (seconds since the Unix epoch), with their
/// records and text. Returns their ids, so stages can forget what they kept of them.
pub fn prune(db: &Db, before: i64) -> Result<Vec<i64>> {
    db.transaction(|t| {
        let ids: Vec<i64> = t
            .prepare("SELECT id FROM media_items WHERE gone IS NOT NULL AND gone < ?1")?
            .query_map(params![before], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut text = t.prepare_cached("DELETE FROM media_text WHERE rowid = ?1")?;
        let mut item = t.prepare_cached("DELETE FROM media_items WHERE id = ?1")?;
        for id in &ids {
            text.execute(params![id])?;
            item.execute(params![id])?;
        }
        Ok(ids)
    })
}

/// Stores an item's facts, replacing what was known.
pub fn save_facts(db: &Db, id: i64, facts: &Facts) -> Result<()> {
    db.with(|c| {
        c.execute(
            "UPDATE media_items SET width = ?2, height = ?3, orientation = ?4, taken = ?5, make = ?6, model = ?7,
               latitude = ?8, longitude = ?9, duration_ms = ?10, frame_rate = ?11, codec = ?12
             WHERE id = ?1",
            params![
                id,
                facts.width,
                facts.height,
                facts.orientation,
                facts.taken,
                facts.make,
                facts.model,
                facts.latitude,
                facts.longitude,
                facts.duration_ms.map(i64_of),
                facts.frame_rate,
                facts.codec
            ],
        )
    })?;
    Ok(())
}

/// An item's facts, if it is known.
pub fn facts(db: &Db, id: i64) -> Result<Option<Facts>> {
    db.with(|c| {
        c.query_row(
            "SELECT width, height, orientation, taken, make, model, latitude, longitude, duration_ms, frame_rate, codec
             FROM media_items WHERE id = ?1",
            params![id],
            |row| {
                Ok(Facts {
                    width: row.get(0)?,
                    height: row.get(1)?,
                    orientation: row.get(2)?,
                    taken: row.get(3)?,
                    make: row.get(4)?,
                    model: row.get(5)?,
                    latitude: row.get(6)?,
                    longitude: row.get(7)?,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|ms| ms as u64),
                    frame_rate: row.get(9)?,
                    codec: row.get(10)?,
                })
            },
        )
        .optional()
    })
}

/// The id of the item with this volume and file id, if the library knows it.
pub fn id_of(db: &Db, volume: &str, file_id: u64) -> Result<Option<i64>> {
    db.with(|c| {
        c.query_row("SELECT id FROM media_items WHERE volume = ?1 AND file_id = ?2", params![volume, i64_of(file_id)], |row| {
            row.get(0)
        })
        .optional()
    })
}

/// Records what a stage made of an item: done, or why it could not be.
pub fn record(db: &Db, id: i64, stage: &str, version: u32, size: u64, mtime: i64, failed: Option<&str>) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT OR REPLACE INTO media_stages (item, stage, version, size, mtime, failed) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, stage, version, i64_of(size), mtime, failed],
        )
    })?;
    Ok(())
}

/// Stores the text recognised in an item, replacing any; empty text removes it.
pub fn set_text(db: &Db, id: i64, text: &str) -> Result<()> {
    db.transaction(|t| {
        t.execute("DELETE FROM media_text WHERE rowid = ?1", params![id])?;
        if !text.trim().is_empty() {
            t.execute("INSERT INTO media_text (rowid, text) VALUES (?1, ?2)", params![id, text])?;
        }
        Ok(())
    })
}

/// The text recognised in an item, if any.
pub fn text(db: &Db, id: i64) -> Result<Option<String>> {
    db.with(|c| c.query_row("SELECT text FROM media_text WHERE rowid = ?1", params![id], |row| row.get(0)).optional())
}

/// The items in the scopes whose recognised text matches an FTS5 query (see [`fts_query`]), as
/// volume and file id.
pub fn find_text(db: &Db, query: &str) -> Result<Vec<(String, u64)>> {
    db.with(|c| {
        c.prepare(
            "SELECT i.volume, i.file_id FROM media_text t JOIN media_items i ON i.id = t.rowid
             WHERE media_text MATCH ?1 AND i.gone IS NULL",
        )?
        .query_map(params![query], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
        .collect()
    })
}

/// The FTS5 query for text typed after `text:`: every word must appear, in order and side by
/// side when they were quoted together, and the last may be the start of a longer word, as
/// names match as you type. Every word is quoted, so nothing typed is read as FTS5 syntax.
pub fn fts_query(typed: &str) -> Option<String> {
    let words: Vec<&str> = typed.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let phrase = words.join(" ").replace('"', "\"\"");
    Some(format!("\"{phrase}\" *"))
}

/// The items in the scopes whose facts meet a condition, as volume and file id. Items gone from
/// the scopes are left out: the library no longer follows them, so what it knows may be old.
pub fn select(db: &Db, condition: &Where) -> Result<Vec<(String, u64)>> {
    let (sql, values): (&str, Vec<rusqlite::types::Value>) = match condition {
        Where::Width(min, max) => ("width BETWEEN ?1 AND ?2", vec![(*min).into(), (*max).into()]),
        Where::Height(min, max) => ("height BETWEEN ?1 AND ?2", vec![(*min).into(), (*max).into()]),
        Where::Taken(from, to) => ("taken >= ?1 AND taken < ?2", vec![(*from).into(), (*to).into()]),
        Where::Camera(text) => {
            let pattern = format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            ("(make LIKE ?1 ESCAPE '\\' OR model LIKE ?1 ESCAPE '\\' OR (make || ' ' || model) LIKE ?1 ESCAPE '\\')", vec![pattern.into()])
        }
        Where::Duration(min, max) => {
            ("duration_ms BETWEEN ?1 AND ?2", vec![i64_of(*min).into(), i64_of((*max).min(i64::MAX as u64)).into()])
        }
        Where::Kind(kind) => ("kind = ?1", vec![i64::from(*kind).into()]),
        Where::Geo(true) => ("latitude IS NOT NULL AND longitude IS NOT NULL", vec![]),
        Where::Geo(false) => ("(latitude IS NULL OR longitude IS NULL)", vec![]),
    };
    db.with(|c| {
        c.prepare(&format!("SELECT volume, file_id FROM media_items WHERE gone IS NULL AND {sql}"))?
            .query_map(params_from_iter(values), |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
            .collect()
    })
}

/// What the Gallery shows of every item that is not gone.
pub fn shown(db: &Db) -> Result<Vec<Shown>> {
    db.with(|c| {
        c.prepare("SELECT id, volume, file_id, kind, width, height, taken, duration_ms FROM media_items WHERE gone IS NULL")?
            .query_map([], |row| {
                Ok(Shown {
                    id: row.get(0)?,
                    volume: row.get(1)?,
                    file_id: row.get::<_, i64>(2)? as u64,
                    kind: row.get(3)?,
                    width: row.get(4)?,
                    height: row.get(5)?,
                    taken: row.get(6)?,
                    duration_ms: row.get::<_, Option<i64>>(7)?.map(|ms| ms as u64),
                })
            })?
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(file_id: u64, path: &str, kind: u8) -> Found {
        Found { volume: "c:".into(), file_id, size: 100, mtime: 5, path: path.into(), kind }
    }

    #[test]
    fn items_keep_their_ids_through_renames_and_coming_back() {
        let db = Db::open_in_memory().unwrap();
        let ids = add(&db, &[found(1, r"C:\a.jpg", PHOTO), found(u64::MAX, r"C:\b.mp4", VIDEO)]).unwrap();
        assert_eq!(ids.len(), 2);
        record(&db, ids[0], "metadata", 1, 100, 5, None).unwrap();
        record(&db, ids[0], "ocr", 2, 100, 5, Some("No text recognition")).unwrap();

        // Renamed: the same file id at another path keeps its id and its records.
        let again = add(&db, &[found(1, r"C:\renamed.jpg", PHOTO)]).unwrap();
        assert_eq!(again, [ids[0]]);
        let all = known(&db).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].path, r"C:\renamed.jpg");
        assert_eq!(all[0].stages.len(), 2);
        assert_eq!(all[0].stages[1].failed.as_deref(), Some("No text recognition"));
        assert_eq!(all[1].file_id, u64::MAX, "file ids use all 64 bits");
        assert!(all[1].stages.is_empty());

        // Gone, then back: not forgotten in between.
        mark_gone(&db, &[ids[1]], 1000).unwrap();
        assert!(known(&db).unwrap()[1].gone);
        assert_eq!(prune(&db, 1000).unwrap(), Vec::<i64>::new(), "gone at 1000 is not before 1000");
        add(&db, &[found(u64::MAX, r"C:\b.mp4", VIDEO)]).unwrap();
        assert!(!known(&db).unwrap()[1].gone);
        mark_gone(&db, &[ids[1]], 10).unwrap();
        set_text(&db, ids[1], "hello").unwrap();
        assert_eq!(prune(&db, 1000).unwrap(), [ids[1]]);
        assert_eq!(known(&db).unwrap().len(), 1);
        assert_eq!(text(&db, ids[1]).unwrap(), None, "pruning takes the text too");
    }

    #[test]
    fn facts_are_stored_and_selected() {
        let db = Db::open_in_memory().unwrap();
        let ids = add(&db, &[found(1, r"C:\a.jpg", PHOTO), found(2, r"C:\b.jpg", PHOTO), found(3, r"C:\c.mp4", VIDEO)]).unwrap();
        let photo = Facts {
            width: Some(4032),
            height: Some(3024),
            orientation: Some(1),
            taken: Some(133_000_000_000_000_000),
            make: Some("Google".into()),
            model: Some("Pixel 8".into()),
            latitude: Some(51.5),
            longitude: Some(-0.12),
            ..Facts::default()
        };
        save_facts(&db, ids[0], &photo).unwrap();
        save_facts(&db, ids[1], &Facts { width: Some(800), height: Some(600), make: Some("Canon".into()), ..Facts::default() }).unwrap();
        let clip = Facts { width: Some(1920), height: Some(1080), duration_ms: Some(95_000), frame_rate: Some(29.97), codec: Some("H.264".into()), ..Facts::default() };
        save_facts(&db, ids[2], &clip).unwrap();
        assert_eq!(facts(&db, ids[0]).unwrap(), Some(photo));
        assert_eq!(facts(&db, 999).unwrap(), None);
        assert_eq!(id_of(&db, "c:", 3).unwrap(), Some(ids[2]));

        let ids_of = |condition: Where| {
            let mut found: Vec<u64> = select(&db, &condition).unwrap().into_iter().map(|(_, id)| id).collect();
            found.sort();
            found
        };
        assert_eq!(ids_of(Where::Width(1900, u32::MAX)), [1, 3]);
        assert_eq!(ids_of(Where::Height(0, 700)), [2]);
        assert_eq!(ids_of(Where::Camera("pixel".into())), [1]);
        assert_eq!(ids_of(Where::Camera("google pixel".into())), [1]);
        assert_eq!(ids_of(Where::Camera("50%".into())), Vec::<u64>::new(), "wildcards typed are literal");
        assert_eq!(ids_of(Where::Duration(60_000, u64::MAX)), [3]);
        assert_eq!(ids_of(Where::Kind(VIDEO)), [3]);
        assert_eq!(ids_of(Where::Geo(true)), [1]);
        assert_eq!(ids_of(Where::Geo(false)), [2, 3]);
        assert_eq!(ids_of(Where::Taken(133_000_000_000_000_000, 133_000_000_000_000_001)), [1]);
        assert_eq!(shown(&db).unwrap().len(), 3);
        // Gone from the scopes: no longer found.
        mark_gone(&db, &[ids[2]], 50).unwrap();
        assert_eq!(ids_of(Where::Width(1900, u32::MAX)), [1]);
        assert_eq!(shown(&db).unwrap().len(), 2);
    }

    #[test]
    fn text_is_found_by_words_phrases_and_beginnings() {
        let db = Db::open_in_memory().unwrap();
        let ids = add(&db, &[found(1, r"C:\receipt.png", PHOTO), found(2, r"C:\note.png", PHOTO)]).unwrap();
        set_text(&db, ids[0], "COSTCO WHOLESALE\nOrder 42.17 total").unwrap();
        set_text(&db, ids[1], "Café order list").unwrap();
        let find = |typed: &str| {
            let mut found: Vec<u64> = find_text(&db, &fts_query(typed).unwrap()).unwrap().into_iter().map(|(_, id)| id).collect();
            found.sort();
            found
        };
        assert_eq!(find("costco"), [1]);
        assert_eq!(find("COST"), [1], "the last word may be a beginning");
        assert_eq!(find("order 42.17"), [1]);
        assert_eq!(find("order"), [1, 2]);
        assert_eq!(find("cafe"), [2], "accents are ignored");
        assert_eq!(find("order list"), [2]);
        assert_eq!(find("42.17 order"), Vec::<u64>::new(), "words typed together are a phrase");
        assert_eq!(find("\"unbalanced"), Vec::<u64>::new(), "quotes are taken literally");
        assert_eq!(fts_query("  "), None);
        assert_eq!(text(&db, ids[0]).unwrap().as_deref(), Some("COSTCO WHOLESALE\nOrder 42.17 total"));
        mark_gone(&db, &[ids[1]], 50).unwrap();
        assert_eq!(find("order"), [1], "gone from the scopes: not found");
        set_text(&db, ids[0], "").unwrap();
        assert_eq!(find("costco"), Vec::<u64>::new());
    }
}
