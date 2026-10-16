//! What the command palette remembers: the items people open from it, and the folders they
//! visit, so that ranking can favour what was used often and lately.
//!
//! One row per item, keyed by its provider and id. Rows are few (at most [`MAX_RECENT`]) and
//! read whole into memory by the palette, which caches them between keystrokes.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_16_palette_recent",
    sql: "CREATE TABLE palette_recent (
            key TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            item_id TEXT NOT NULL,
            title TEXT NOT NULL,
            subtitle TEXT,
            path TEXT,
            dir INTEGER NOT NULL DEFAULT 0,
            uses INTEGER NOT NULL DEFAULT 0,
            last_used INTEGER NOT NULL
          );
          CREATE INDEX palette_recent_by_time ON palette_recent (last_used);",
}];

/// Rows kept; the ones used longest ago go first.
pub const MAX_RECENT: usize = 1000;

/// One use of an item: what is recorded.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Use {
    /// The provider the item came from, such as `files` or `actions`.
    pub provider: String,
    /// The provider's id for it: a path for files and folders.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    /// The file or folder the item stands for, if any.
    pub path: Option<String>,
    #[serde(default)]
    pub dir: bool,
}

/// A remembered item and how it was used.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Recent {
    pub key: String,
    #[serde(flatten)]
    pub item: Use,
    pub uses: u32,
    /// Milliseconds since the Unix epoch.
    pub last_used: i64,
}

/// The key an item is remembered under. Ids are compared without case, as Windows compares
/// paths.
pub fn key(provider: &str, id: &str) -> String {
    format!("{provider}:{}", id.to_lowercase())
}

/// Records a use of `item` at `now` (milliseconds since the Unix epoch).
pub fn record(db: &Db, item: &Use, now: i64) -> Result<()> {
    let key = key(&item.provider, &item.id);
    db.transaction(|t| {
        t.execute(
            "INSERT INTO palette_recent (key, provider, item_id, title, subtitle, path, dir, uses, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
             ON CONFLICT(key) DO UPDATE SET
               item_id = excluded.item_id, title = excluded.title, subtitle = excluded.subtitle,
               path = excluded.path, dir = excluded.dir, uses = uses + 1,
               last_used = max(last_used, excluded.last_used)",
            params![key, item.provider, item.id, item.title, item.subtitle, item.path, item.dir, now],
        )?;
        t.execute(
            "DELETE FROM palette_recent WHERE key NOT IN
               (SELECT key FROM palette_recent ORDER BY last_used DESC LIMIT ?1)",
            params![MAX_RECENT as i64],
        )?;
        Ok(())
    })
}

/// Every remembered item, most recently used first: what was chosen from the palette, and the
/// folders visited, which `shortcuts::frecency` records once for both (see `crate::parity`).
/// A folder both chosen and visited is one item whose uses add up.
pub fn recent(db: &Db) -> Result<Vec<Recent>> {
    let mut rows = chosen(db)?;
    let visits = db.with(|c| {
        c.prepare("SELECT path, visits, last_visit FROM folder_visits")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut by_key: std::collections::HashMap<String, usize> =
        rows.iter().enumerate().map(|(at, row)| (row.key.clone(), at)).collect();
    for (path, visits, last_visit) in visits {
        let key = key("files", &path);
        let uses = u32::try_from(visits).unwrap_or(u32::MAX);
        let last_used = last_visit.saturating_mul(1000);
        match by_key.get(&key) {
            Some(&at) => {
                let row = &mut rows[at];
                row.uses = row.uses.saturating_add(uses);
                row.last_used = row.last_used.max(last_used);
            }
            None => {
                by_key.insert(key.clone(), rows.len());
                let (title, subtitle) = folder_title(&path);
                let item = Use { provider: "files".into(), id: path.clone(), title, subtitle, path: Some(path), dir: true };
                rows.push(Recent { key, item, uses, last_used });
            }
        }
    }
    rows.sort_by(|a, b| b.last_used.cmp(&a.last_used).then_with(|| a.key.cmp(&b.key)));
    Ok(rows)
}

/// A folder's name and the folder holding it, as the palette's files provider titles them; a
/// drive root is its own name.
fn folder_title(path: &str) -> (String, Option<String>) {
    let trimmed = path.trim_end_matches('\\');
    match trimmed.rfind('\\') {
        Some(at) => {
            let parent = &trimmed[..at];
            let parent = if parent.ends_with(':') { format!("{parent}\\") } else { parent.to_owned() };
            (trimmed[at + 1..].to_owned(), Some(parent))
        }
        None => (path.to_owned(), None),
    }
}

/// The items chosen from the palette, most recently used first.
fn chosen(db: &Db) -> Result<Vec<Recent>> {
    db.with(|c| {
        let mut statement = c.prepare(
            "SELECT key, provider, item_id, title, subtitle, path, dir, uses, last_used
             FROM palette_recent ORDER BY last_used DESC, key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Recent {
                key: row.get(0)?,
                item: Use {
                    provider: row.get(1)?,
                    id: row.get(2)?,
                    title: row.get(3)?,
                    subtitle: row.get(4)?,
                    path: row.get(5)?,
                    dir: row.get(6)?,
                },
                uses: row.get(7)?,
                last_used: row.get(8)?,
            })
        })?;
        rows.collect()
    })
}

/// Forgets one item, such as a file that no longer exists.
pub fn forget(db: &Db, provider: &str, id: &str) -> Result<()> {
    db.with(|c| c.execute("DELETE FROM palette_recent WHERE key = ?1", params![key(provider, id)]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> Use {
        Use {
            provider: "files".into(),
            id: path.into(),
            title: path.rsplit('\\').next().unwrap_or(path).into(),
            subtitle: None,
            path: Some(path.into()),
            dir: true,
        }
    }

    #[test]
    fn uses_are_counted_per_item_regardless_of_case() {
        let db = Db::open_in_memory().unwrap();
        record(&db, &folder(r"C:\Projects"), 1_000).unwrap();
        record(&db, &folder(r"c:\projects"), 2_000).unwrap();
        record(&db, &folder(r"C:\Music"), 1_500).unwrap();
        let rows = recent(&db).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].item.id, r"c:\projects", "the latest spelling is kept");
        assert_eq!((rows[0].uses, rows[0].last_used), (2, 2_000));
        assert_eq!((rows[1].item.title.as_str(), rows[1].uses), ("Music", 1));
        assert_eq!(rows[0].key, key("files", r"C:\PROJECTS"));

        forget(&db, "files", r"C:\MUSIC").unwrap();
        assert_eq!(recent(&db).unwrap().len(), 1);
    }

    #[test]
    fn an_older_use_never_moves_the_last_use_back() {
        let db = Db::open_in_memory().unwrap();
        record(&db, &folder(r"C:\A"), 5_000).unwrap();
        record(&db, &folder(r"C:\A"), 4_000).unwrap();
        let rows = recent(&db).unwrap();
        assert_eq!((rows[0].uses, rows[0].last_used), (2, 5_000));
    }

    #[test]
    fn only_the_most_recent_items_are_kept() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..(MAX_RECENT + 5) {
            record(&db, &folder(&format!(r"C:\F{i}")), i as i64).unwrap();
        }
        let rows = recent(&db).unwrap();
        assert_eq!(rows.len(), MAX_RECENT);
        assert_eq!(rows[0].item.id, format!(r"C:\F{}", MAX_RECENT + 4));
        assert!(rows.iter().all(|row| row.last_used >= 5), "the five oldest went first");
    }
}
