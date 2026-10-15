//! Frequent and recent folders, ranked by "frecency" as zoxide ranks directories.
//!
//! Each visit adds one to a folder's rank. A folder's score is its rank weighted by how long
//! ago it was last visited: four times within the hour, twice within the day, half within the
//! week, a quarter after that. Ranks age so that the list keeps up with changing habits: once
//! they add up to more than [`MAX_AGE`], every rank is scaled down to 90 % of that total and
//! folders left below a rank of one are forgotten.

use rusqlite::params;
use serde::Serialize;

use crate::{Db, Migration, Result};

pub(super) const MIGRATION: Migration = Migration {
    name: "2026_10_16_shortcuts_frecency",
    sql: "
        CREATE TABLE folder_visits (
            key TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            rank REAL NOT NULL,
            visits INTEGER NOT NULL,
            last_visit INTEGER NOT NULL
        );
    ",
};

/// The total rank at which every rank is aged (zoxide's default `_ZO_MAXAGE`).
pub const MAX_AGE: f64 = 10_000.0;

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ranked {
    pub path: String,
    pub score: f64,
    pub rank: f64,
    pub visits: i64,
    /// Unix seconds.
    pub last_visit: i64,
}

/// A folder's score `now`, from its rank and when it was last visited.
pub fn score(rank: f64, last_visit: i64, now: i64) -> f64 {
    let age = now.saturating_sub(last_visit);
    let weight = if age < HOUR {
        4.0
    } else if age < DAY {
        2.0
    } else if age < WEEK {
        0.5
    } else {
        0.25
    };
    rank * weight
}

/// A path with backslashes and no trailing separator, except on a drive root.
fn tidy(path: &str) -> String {
    let mut path = path.trim().replace('/', "\\");
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    path
}

/// How a folder is keyed: its tidy path, case-insensitively.
fn key(path: &str) -> String {
    tidy(path).to_lowercase()
}

/// Records a visit to `path` at `now` (Unix seconds).
pub fn record(db: &Db, path: &str, now: i64) -> Result<()> {
    record_aging_at(db, path, now, MAX_AGE)
}

/// [`record`], aging every rank once they add up to more than `max_age`.
pub fn record_aging_at(db: &Db, path: &str, now: i64, max_age: f64) -> Result<()> {
    let path = tidy(path);
    if path.is_empty() {
        return Ok(());
    }
    db.transaction(|t| {
        t.execute(
            "INSERT INTO folder_visits (key, path, rank, visits, last_visit) VALUES (?1, ?2, 1, 1, ?3)
             ON CONFLICT(key) DO UPDATE SET path = excluded.path, rank = rank + 1, visits = visits + 1, last_visit = excluded.last_visit",
            params![key(&path), path, now],
        )?;
        let total: f64 = t.query_row("SELECT COALESCE(SUM(rank), 0) FROM folder_visits", [], |row| row.get(0))?;
        if total > max_age {
            let factor = 0.9 * max_age / total;
            t.execute("UPDATE folder_visits SET rank = rank * ?1", params![factor])?;
            t.execute("DELETE FROM folder_visits WHERE rank < 1", [])?;
        }
        Ok(())
    })
}

/// Every remembered folder, highest score first.
pub fn ranked(db: &Db, now: i64) -> Result<Vec<Ranked>> {
    let mut all = db.with(|c| {
        c.prepare("SELECT path, rank, visits, last_visit FROM folder_visits")?
            .query_map([], |row| {
                let (rank, last_visit) = (row.get(1)?, row.get(3)?);
                Ok(Ranked { path: row.get(0)?, score: score(rank, last_visit, now), rank, visits: row.get(2)?, last_visit })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    all.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.last_visit.cmp(&a.last_visit)).then_with(|| a.path.cmp(&b.path)));
    Ok(all)
}

/// Forgets a folder, as when the user takes it off the Frequent list.
pub fn forget(db: &Db, path: &str) -> Result<bool> {
    Ok(db.with(|c| c.execute("DELETE FROM folder_visits WHERE key = ?1", params![key(path)]))? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn order(db: &Db, now: i64) -> Vec<String> {
        ranked(db, now).unwrap().into_iter().map(|r| r.path).collect()
    }

    #[test]
    fn recent_visits_weigh_more_than_old_ones() {
        assert_eq!(score(10.0, NOW, NOW), 40.0);
        assert_eq!(score(10.0, NOW - HOUR, NOW), 20.0);
        assert_eq!(score(10.0, NOW - DAY, NOW), 5.0);
        assert_eq!(score(10.0, NOW - WEEK + 1, NOW), 5.0);
        assert_eq!(score(10.0, NOW - WEEK, NOW), 2.5);
        assert_eq!(score(10.0, NOW - 365 * DAY, NOW), 2.5);
    }

    #[test]
    fn frequency_and_recency_both_count() {
        let db = Db::open_in_memory().unwrap();
        // Projects: visited five times, two days ago. Downloads: once, just now.
        for _ in 0..5 {
            record(&db, r"C:\Projects", NOW - 2 * DAY).unwrap();
        }
        record(&db, r"C:\Downloads\", NOW).unwrap();
        // 5 x 0.5 = 2.5 against 1 x 4.
        assert_eq!(order(&db, NOW), [r"C:\Downloads", r"C:\Projects"]);
        // Two hours later the recent visit has faded: 5 x 0.5 against 1 x 2.
        assert_eq!(order(&db, NOW + 2 * HOUR), [r"C:\Projects", r"C:\Downloads"]);

        // Visits are keyed without regard to case or a trailing separator, and keep the
        // spelling of the latest one.
        record(&db, r"c:\projects\", NOW).unwrap();
        let top = &ranked(&db, NOW).unwrap()[0];
        assert_eq!((top.path.as_str(), top.rank, top.visits, top.last_visit), (r"c:\projects", 6.0, 6, NOW));
        // A drive root keeps its separator.
        record(&db, r"D:\", NOW - 30 * DAY).unwrap();
        assert_eq!(order(&db, NOW).last().map(String::as_str), Some(r"D:\"));
        assert!(forget(&db, r"d:\").unwrap());

        assert!(forget(&db, r"C:\PROJECTS").unwrap());
        assert!(!forget(&db, r"C:\Projects").unwrap());
        assert_eq!(order(&db, NOW), [r"C:\Downloads"]);
        // Blank paths are ignored rather than recorded.
        record(&db, "  ", NOW).unwrap();
        assert_eq!(ranked(&db, NOW).unwrap().len(), 1);
    }

    #[test]
    fn ranks_age_and_rarely_visited_folders_drop_out() {
        let db = Db::open_in_memory().unwrap();
        for _ in 0..60 {
            record_aging_at(&db, r"C:\Daily", NOW, 100.0).unwrap();
        }
        record_aging_at(&db, r"C:\Once", NOW, 100.0).unwrap();
        for _ in 0..39 {
            record_aging_at(&db, r"C:\Weekly", NOW, 100.0).unwrap();
        }
        // The total reached 100 but did not pass it, so nothing has aged yet.
        let ranks: Vec<f64> = ranked(&db, NOW).unwrap().iter().map(|r| r.rank).collect();
        assert_eq!(ranks, [60.0, 39.0, 1.0]);

        // One more visit takes the total to 101: everything is scaled to 90 % of 100, and the
        // folder visited once falls below a rank of one and is forgotten.
        record_aging_at(&db, r"C:\Weekly", NOW, 100.0).unwrap();
        let after = ranked(&db, NOW).unwrap();
        assert_eq!(after.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), [r"C:\Daily", r"C:\Weekly"]);
        let factor = 90.0 / 101.0;
        assert!((after[0].rank - 60.0 * factor).abs() < 1e-9);
        assert!((after[1].rank - 40.0 * factor).abs() < 1e-9);
        assert!(after.iter().map(|r| r.rank).sum::<f64>() < 100.0);
        // Visit counts are kept as they were; only ranks age.
        assert_eq!(after[0].visits, 60);
    }
}
