//! What the user says about each project: a status, tags, Markdown notes, whether it is pinned
//! to the top of the hub, and whether it was dismissed as "not a project".
//!
//! Projects themselves are discovered from the file index every time, so only these choices are
//! stored, keyed by the project's root folder. Paths compare without regard to case, as Windows
//! does. A root with no row simply has the defaults: active, untagged, no notes.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, Error, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "2026_10_16_projects_meta",
        sql: "CREATE TABLE project_meta (
                root TEXT PRIMARY KEY COLLATE NOCASE,
                status TEXT NOT NULL DEFAULT 'active',
                notes TEXT NOT NULL DEFAULT '',
                pinned INTEGER NOT NULL DEFAULT 0,
                dismissed INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
              );",
    },
    Migration {
        name: "2026_10_16_projects_tags",
        sql: "CREATE TABLE project_tags (
                root TEXT NOT NULL COLLATE NOCASE,
                tag TEXT NOT NULL COLLATE NOCASE,
                PRIMARY KEY (root, tag)
              );",
    },
];

/// Where a project stands, as the user sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Paused,
    Archived,
    Idea,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Paused => "paused",
            Status::Archived => "archived",
            Status::Idea => "idea",
        }
    }

    /// Unknown values read back as active rather than failing the whole list.
    fn parse(text: &str) -> Self {
        match text {
            "paused" => Status::Paused,
            "archived" => Status::Archived,
            "idea" => Status::Idea,
            _ => Status::Active,
        }
    }
}

/// The user's own data about one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProjectMeta {
    pub root: String,
    pub status: Status,
    pub tags: Vec<String>,
    pub notes: String,
    pub pinned: bool,
    pub dismissed: bool,
}

/// The longest note kept, so a runaway paste cannot bloat the database.
const MAX_NOTES: usize = 1 << 20;
/// Tags are short labels; longer ones are cut.
const MAX_TAG: usize = 40;

fn ensure_row(t: &rusqlite::Transaction<'_>, root: &str) -> rusqlite::Result<()> {
    t.execute("INSERT INTO project_meta (root) VALUES (?1) ON CONFLICT(root) DO NOTHING", params![root])?;
    Ok(())
}

fn touch(t: &rusqlite::Transaction<'_>, root: &str) -> rusqlite::Result<()> {
    t.execute("UPDATE project_meta SET updated_at = unixepoch() WHERE root = ?1", params![root])?;
    Ok(())
}

/// Every project the user has said anything about, dismissed ones included.
pub fn all(db: &Db) -> Result<Vec<ProjectMeta>> {
    db.with(|c| {
        let mut tags: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        let mut statement = c.prepare("SELECT root, tag FROM project_tags ORDER BY tag COLLATE NOCASE")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows {
            let (root, tag) = row?;
            tags.entry(root.to_lowercase()).or_default().push(tag);
        }
        let mut statement = c.prepare("SELECT root, status, notes, pinned, dismissed FROM project_meta")?;
        let rows = statement.query_map([], |row| {
            Ok(ProjectMeta {
                root: row.get(0)?,
                status: Status::parse(&row.get::<_, String>(1)?),
                tags: Vec::new(),
                notes: row.get(2)?,
                pinned: row.get(3)?,
                dismissed: row.get(4)?,
            })
        })?;
        let mut all = Vec::new();
        for row in rows {
            let mut meta = row?;
            meta.tags = tags.remove(&meta.root.to_lowercase()).unwrap_or_default();
            all.push(meta);
        }
        // Tags on a root without a meta row still belong to it.
        for (root, tags) in tags {
            all.push(ProjectMeta { root, tags, ..ProjectMeta::default() });
        }
        Ok(all)
    })
}

/// The user's data about one project, or the defaults.
pub fn get(db: &Db, root: &str) -> Result<ProjectMeta> {
    let found = db.with(|c| {
        c.query_row(
            "SELECT root, status, notes, pinned, dismissed FROM project_meta WHERE root = ?1",
            params![root],
            |row| {
                Ok(ProjectMeta {
                    root: row.get(0)?,
                    status: Status::parse(&row.get::<_, String>(1)?),
                    tags: Vec::new(),
                    notes: row.get(2)?,
                    pinned: row.get(3)?,
                    dismissed: row.get(4)?,
                })
            },
        )
        .optional()
    })?;
    let mut meta = found.unwrap_or_else(|| ProjectMeta { root: root.to_owned(), ..ProjectMeta::default() });
    meta.tags = db.with(|c| {
        let mut statement = c.prepare("SELECT tag FROM project_tags WHERE root = ?1 ORDER BY tag COLLATE NOCASE")?;
        let rows = statement.query_map(params![root], |row| row.get(0))?;
        rows.collect()
    })?;
    Ok(meta)
}

pub fn set_status(db: &Db, root: &str, status: Status) -> Result<()> {
    db.transaction(|t| {
        ensure_row(t, root)?;
        t.execute("UPDATE project_meta SET status = ?2 WHERE root = ?1", params![root, status.as_str()])?;
        touch(t, root)
    })
}

pub fn set_notes(db: &Db, root: &str, notes: &str) -> Result<()> {
    if notes.len() > MAX_NOTES {
        return Err(Error(format!("Notes are limited to {} KB", MAX_NOTES / 1024)));
    }
    db.transaction(|t| {
        ensure_row(t, root)?;
        t.execute("UPDATE project_meta SET notes = ?2 WHERE root = ?1", params![root, notes])?;
        touch(t, root)
    })
}

pub fn set_pinned(db: &Db, root: &str, pinned: bool) -> Result<()> {
    db.transaction(|t| {
        ensure_row(t, root)?;
        t.execute("UPDATE project_meta SET pinned = ?2 WHERE root = ?1", params![root, pinned])?;
        touch(t, root)
    })
}

/// Marks a folder as not a project (or a project again); discovery leaves dismissed roots out.
pub fn set_dismissed(db: &Db, root: &str, dismissed: bool) -> Result<()> {
    db.transaction(|t| {
        ensure_row(t, root)?;
        t.execute("UPDATE project_meta SET dismissed = ?2 WHERE root = ?1", params![root, dismissed])?;
        touch(t, root)
    })
}

/// Cleans up tags as typed: trimmed, inner whitespace collapsed, cut to a sensible length,
/// empty ones dropped and duplicates (ignoring case) removed, first spelling kept.
pub fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut clean = Vec::new();
    for tag in tags {
        let tag: String = tag.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TAG).collect();
        if !tag.is_empty() && seen.insert(tag.to_lowercase()) {
            clean.push(tag);
        }
    }
    clean
}

/// Replaces the project's tags. Returns them as stored.
pub fn set_tags(db: &Db, root: &str, tags: &[String]) -> Result<Vec<String>> {
    let tags = clean_tags(tags);
    db.transaction(|t| {
        ensure_row(t, root)?;
        t.execute("DELETE FROM project_tags WHERE root = ?1", params![root])?;
        for tag in &tags {
            t.execute("INSERT INTO project_tags (root, tag) VALUES (?1, ?2)", params![root, tag])?;
        }
        touch(t, root)
    })?;
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_until_something_is_said() {
        let db = Db::open_in_memory().unwrap();
        let meta = get(&db, r"C:\Code\app").unwrap();
        assert_eq!(meta.status, Status::Active);
        assert!(meta.tags.is_empty() && meta.notes.is_empty() && !meta.pinned && !meta.dismissed);
        assert!(all(&db).unwrap().is_empty());
    }

    #[test]
    fn every_field_round_trips_and_roots_ignore_case() {
        let db = Db::open_in_memory().unwrap();
        let root = r"C:\Code\App";
        set_status(&db, root, Status::Paused).unwrap();
        set_notes(&db, r"c:\code\app", "# Plan\n\n- ship it").unwrap();
        set_pinned(&db, root, true).unwrap();
        let tags = set_tags(&db, root, &["  client  work ".into(), "Rust".into(), "rust".into(), "".into()]).unwrap();
        assert_eq!(tags, ["client work", "Rust"]);

        let meta = get(&db, r"C:\CODE\APP").unwrap();
        assert_eq!(meta.status, Status::Paused);
        assert_eq!(meta.notes, "# Plan\n\n- ship it");
        assert!(meta.pinned);
        assert_eq!(meta.tags, ["client work", "Rust"]);

        set_dismissed(&db, root, true).unwrap();
        set_tags(&db, root, &["Later".into()]).unwrap();
        let listed = all(&db).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].dismissed);
        assert_eq!(listed[0].tags, ["Later"]);
        assert_eq!(listed[0].status, Status::Paused, "other fields survive later changes");
    }

    #[test]
    fn oversized_notes_are_refused() {
        let db = Db::open_in_memory().unwrap();
        assert!(set_notes(&db, "C:\\x", &"x".repeat(MAX_NOTES + 1)).is_err());
        assert_eq!(get(&db, "C:\\x").unwrap().notes, "");
    }

    #[test]
    fn statuses_serialise_in_lower_case() {
        assert_eq!(serde_json::to_string(&Status::Archived).unwrap(), "\"archived\"");
        assert_eq!(serde_json::from_str::<Status>("\"idea\"").unwrap(), Status::Idea);
        assert_eq!(Status::parse("nonsense"), Status::Active);
    }
}
