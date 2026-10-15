//! Coloured tags on files and folders.
//!
//! A tagged item is known by its file id on its volume, so its tags follow it through renames
//! and moves on that volume; its last known path is kept beside the id as a fallback, for
//! items whose id could not be read and for finding the item again when the id changes (an
//! editor that saves by replacing the file gives it a new one). Callers resolve where an item
//! is now and report it back with [`relocate`].

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;

use super::{check_color, clean_name, next_position, not_found};
use crate::{Db, Error, Migration, Result};

pub(super) const MIGRATION: Migration = Migration {
    name: "2026_10_16_shortcuts_tags",
    sql: "
        CREATE TABLE tags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            color TEXT NOT NULL,
            position INTEGER NOT NULL
        );
        CREATE TABLE tagged_items (
            id INTEGER PRIMARY KEY,
            volume TEXT NOT NULL,
            file_id INTEGER NOT NULL DEFAULT 0,
            path TEXT NOT NULL
        );
        CREATE INDEX tagged_items_file ON tagged_items (volume, file_id) WHERE file_id != 0;
        CREATE INDEX tagged_items_path ON tagged_items (path COLLATE NOCASE);
        CREATE TABLE item_tags (
            item_id INTEGER NOT NULL REFERENCES tagged_items (id) ON DELETE CASCADE,
            tag_id INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
            PRIMARY KEY (item_id, tag_id)
        );
    ",
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub color: String,
}

/// A file or folder as the caller sees it now.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    pub path: &'a str,
    /// Such as `c:`; file ids are only unique within a volume.
    pub volume: &'a str,
    /// 0 when it could not be read.
    pub file_id: u64,
}

/// A tagged item: where it was last seen, and its tags.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    pub id: i64,
    pub volume: String,
    pub file_id: u64,
    pub path: String,
    pub tags: Vec<i64>,
}

fn tag_from(row: &Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
}

/// Every tag, in sidebar order.
pub fn list(db: &Db) -> Result<Vec<Tag>> {
    db.with(|c| c.prepare("SELECT id, name, color FROM tags ORDER BY position, id")?.query_map([], tag_from)?.collect())
}

/// Every tag, in sidebar order, with how many items carry it.
pub fn with_counts(db: &Db) -> Result<Vec<(Tag, usize)>> {
    db.with(|c| {
        c.prepare(
            "SELECT tags.id, tags.name, tags.color, COUNT(item_tags.item_id) FROM tags
             LEFT JOIN item_tags ON item_tags.tag_id = tags.id
             GROUP BY tags.id ORDER BY tags.position, tags.id",
        )?
        .query_map([], |row| Ok((tag_from(row)?, row.get::<_, i64>(3)? as usize)))?
        .collect()
    })
}

/// The tag with this name, in any case.
pub fn by_name(db: &Db, name: &str) -> Result<Option<Tag>> {
    db.with(|c| c.query_row("SELECT id, name, color FROM tags WHERE name = ?1", params![name.trim()], tag_from).optional())
}

fn duplicate_name(error: Error, name: &str) -> Error {
    if error.0.contains("UNIQUE") { Error(format!("There is already a tag named \"{name}\"")) } else { error }
}

/// Characters a tag name can't contain, since `tag:<name>` must survive the search box.
fn check_tag_name(name: &str) -> Result<String> {
    let name = clean_name(name)?;
    if name.contains('"') {
        return Err(Error("A tag name can't contain quotation marks".into()));
    }
    Ok(name)
}

pub fn create(db: &Db, name: &str, color: &str) -> Result<Tag> {
    let name = check_tag_name(name)?;
    let color = check_color(color)?;
    let id = db
        .transaction(|t| {
            let position = next_position(t, "tags", "1")?;
            t.execute("INSERT INTO tags (name, color, position) VALUES (?1, ?2, ?3)", params![name, color, position])?;
            Ok(t.last_insert_rowid())
        })
        .map_err(|error| duplicate_name(error, &name))?;
    Ok(Tag { id, name, color })
}

pub fn rename(db: &Db, id: i64, name: &str) -> Result<()> {
    let name = check_tag_name(name)?;
    let rows = db
        .with(|c| c.execute("UPDATE tags SET name = ?1 WHERE id = ?2", params![name, id]))
        .map_err(|error| duplicate_name(error, &name))?;
    if rows == 0 { Err(not_found("tag")) } else { Ok(()) }
}

pub fn set_color(db: &Db, id: i64, color: &str) -> Result<()> {
    let color = check_color(color)?;
    let rows = db.with(|c| c.execute("UPDATE tags SET color = ?1 WHERE id = ?2", params![color, id]))?;
    if rows == 0 { Err(not_found("tag")) } else { Ok(()) }
}

/// Deletes a tag, taking it off every item. The items themselves are untouched.
pub fn delete(db: &Db, id: i64) -> Result<()> {
    let rows = db.transaction(|t| {
        let rows = t.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
        forget_untagged(t)?;
        Ok(rows)
    })?;
    if rows == 0 { Err(not_found("tag")) } else { Ok(()) }
}

/// Items with no tags left are not kept.
fn forget_untagged(c: &Connection) -> rusqlite::Result<usize> {
    c.execute("DELETE FROM tagged_items WHERE id NOT IN (SELECT item_id FROM item_tags)", [])
}

/// The item `target` is: the one with its file id on its volume, or failing that, the one last
/// seen at its path.
fn find(c: &Connection, target: &Target<'_>) -> rusqlite::Result<Option<i64>> {
    if target.file_id != 0 {
        let by_id = c
            .query_row(
                "SELECT id FROM tagged_items WHERE volume = ?1 AND file_id = ?2",
                params![target.volume, target.file_id as i64],
                |row| row.get(0),
            )
            .optional()?;
        if by_id.is_some() {
            return Ok(by_id);
        }
    }
    c.query_row("SELECT id FROM tagged_items WHERE path = ?1 COLLATE NOCASE", params![target.path], |row| row.get(0))
        .optional()
}

/// Puts tag `tag` on `target`. The item's recorded path and id are brought up to date.
pub fn assign(db: &Db, tag: i64, target: &Target<'_>) -> Result<()> {
    let outcome = db.transaction(|t| {
        let exists: bool = t.query_row("SELECT EXISTS(SELECT 1 FROM tags WHERE id = ?1)", params![tag], |row| row.get(0))?;
        if !exists {
            return Ok(false);
        }
        let item = match find(t, target)? {
            Some(item) => {
                t.execute(
                    "UPDATE tagged_items SET volume = ?1, file_id = ?2, path = ?3 WHERE id = ?4",
                    params![target.volume, target.file_id as i64, target.path, item],
                )?;
                item
            }
            None => {
                t.execute(
                    "INSERT INTO tagged_items (volume, file_id, path) VALUES (?1, ?2, ?3)",
                    params![target.volume, target.file_id as i64, target.path],
                )?;
                t.last_insert_rowid()
            }
        };
        t.execute("INSERT OR IGNORE INTO item_tags (item_id, tag_id) VALUES (?1, ?2)", params![item, tag])?;
        Ok(true)
    })?;
    if outcome { Ok(()) } else { Err(not_found("tag")) }
}

/// Takes tag `tag` off `target`, if it has it.
pub fn unassign(db: &Db, tag: i64, target: &Target<'_>) -> Result<()> {
    db.transaction(|t| {
        if let Some(item) = find(t, target)? {
            t.execute("DELETE FROM item_tags WHERE item_id = ?1 AND tag_id = ?2", params![item, tag])?;
            forget_untagged(t)?;
        }
        Ok(())
    })
}

/// The tags `target` has.
pub fn tags_of(db: &Db, target: &Target<'_>) -> Result<Vec<i64>> {
    db.with(|c| {
        let Some(item) = find(c, target)? else { return Ok(Vec::new()) };
        c.prepare("SELECT tag_id FROM item_tags JOIN tags ON tags.id = tag_id WHERE item_id = ?1 ORDER BY tags.position, tags.id")?
            .query_map(params![item], |row| row.get(0))?
            .collect()
    })
}

/// The items matching `filter`, an SQL condition that takes `tag` as its parameter, if given.
fn items_where(db: &Db, filter: &str, tag: Option<i64>) -> Result<Vec<Item>> {
    db.with(|c| {
        let mut items: Vec<Item> = c
            .prepare(&format!("SELECT id, volume, file_id, path FROM tagged_items {filter} ORDER BY id"))?
            .query_map(params_from_iter(tag), |row| {
                Ok(Item {
                    id: row.get(0)?,
                    volume: row.get(1)?,
                    file_id: row.get::<_, i64>(2)? as u64,
                    path: row.get(3)?,
                    tags: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut tags_of = c.prepare(
            "SELECT tag_id FROM item_tags JOIN tags ON tags.id = tag_id WHERE item_id = ?1 ORDER BY tags.position, tags.id",
        )?;
        for item in &mut items {
            item.tags = tags_of.query_map(params![item.id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        }
        Ok(items)
    })
}

/// Every tagged item.
pub fn items(db: &Db) -> Result<Vec<Item>> {
    items_where(db, "", None)
}

/// The items that have tag `tag`.
pub fn items_with(db: &Db, tag: i64) -> Result<Vec<Item>> {
    items_where(db, "WHERE id IN (SELECT item_id FROM item_tags WHERE tag_id = ?1)", Some(tag))
}

/// Records where an item is now, once the caller has found it after a rename or move.
pub fn relocate(db: &Db, item: i64, path: &str, file_id: u64) -> Result<()> {
    db.with(|c| c.execute("UPDATE tagged_items SET path = ?1, file_id = ?2 WHERE id = ?3", params![path, file_id as i64, item]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, file_id: u64) -> Target<'_> {
        Target { path, volume: "c:", file_id }
    }

    #[test]
    fn tags_are_created_renamed_recoloured_and_deleted() {
        let db = Db::open_in_memory().unwrap();
        let urgent = create(&db, " Urgent ", "Red").unwrap();
        let later = create(&db, "Later", "blue").unwrap();
        assert_eq!(list(&db).unwrap(), [urgent.clone(), later.clone()]);
        assert_eq!((urgent.name.as_str(), urgent.color.as_str()), ("Urgent", "red"));
        assert!(create(&db, "URGENT", "green").unwrap_err().to_string().contains("already"));
        assert!(create(&db, "Odd \"name\"", "green").is_err());
        assert!(create(&db, "Plaid", "plaid").is_err());

        rename(&db, later.id, "Someday").unwrap();
        set_color(&db, later.id, "purple").unwrap();
        assert_eq!(by_name(&db, "someday").unwrap(), Some(Tag { id: later.id, name: "Someday".into(), color: "purple".into() }));
        assert!(rename(&db, later.id, "urgent").is_err());

        assign(&db, urgent.id, &at(r"C:\a.txt", 1)).unwrap();
        assign(&db, later.id, &at(r"C:\a.txt", 1)).unwrap();
        delete(&db, urgent.id).unwrap();
        assert_eq!(tags_of(&db, &at(r"C:\a.txt", 1)).unwrap(), [later.id]);
        delete(&db, later.id).unwrap();
        // With its last tag gone the item is forgotten too.
        assert!(items(&db).unwrap().is_empty());
        assert!(delete(&db, later.id).is_err());
        assert!(assign(&db, later.id, &at(r"C:\a.txt", 1)).is_err());
    }

    #[test]
    fn tags_are_counted_in_sidebar_order() {
        let db = Db::open_in_memory().unwrap();
        let red = create(&db, "Red", "red").unwrap();
        let blue = create(&db, "Blue", "blue").unwrap();
        assign(&db, blue.id, &at(r"C:\a.txt", 1)).unwrap();
        assign(&db, blue.id, &at(r"C:\b.txt", 2)).unwrap();
        assign(&db, red.id, &at(r"C:\b.txt", 2)).unwrap();
        let counted: Vec<(String, usize)> = with_counts(&db).unwrap().into_iter().map(|(tag, n)| (tag.name, n)).collect();
        assert_eq!(counted, [("Red".to_owned(), 1), ("Blue".to_owned(), 2)]);
        create(&db, "Unused", "gray").unwrap();
        assert_eq!(with_counts(&db).unwrap()[2].1, 0, "a tag on nothing counts none");
    }

    #[test]
    fn tags_follow_their_item_through_a_rename_by_file_id() {
        let db = Db::open_in_memory().unwrap();
        let red = create(&db, "Red", "red").unwrap();
        let green = create(&db, "Green", "green").unwrap();
        assign(&db, red.id, &at(r"C:\work\draft.txt", 42)).unwrap();

        // Renamed and moved: same file id, new path. It is the same item.
        let renamed = at(r"C:\archive\final.txt", 42);
        assert_eq!(tags_of(&db, &renamed).unwrap(), [red.id]);
        assign(&db, green.id, &renamed).unwrap();
        let all = items(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].path.as_str(), all[0].tags.clone()), (r"C:\archive\final.txt", vec![red.id, green.id]));

        // The same file id on another volume is another file.
        assert!(tags_of(&db, &Target { path: r"D:\x", volume: "d:", file_id: 42 }).unwrap().is_empty());

        // A new file at the old path is not the tagged one; the new path wins over it.
        assert!(tags_of(&db, &at(r"C:\work\draft.txt", 99)).unwrap().is_empty());

        // An item whose id changed (saved by replacing) is found by its path.
        assert_eq!(tags_of(&db, &at(r"c:\ARCHIVE\final.txt", 77)).unwrap(), [red.id, green.id]);
        unassign(&db, green.id, &at(r"C:\archive\final.txt", 77)).unwrap();
        assert_eq!(items(&db).unwrap()[0].tags, [red.id]);

        // Callers report where an item turned up.
        let item = items(&db).unwrap()[0].id;
        relocate(&db, item, r"C:\elsewhere\final.txt", 42).unwrap();
        assert_eq!(items_with(&db, red.id).unwrap()[0].path, r"C:\elsewhere\final.txt");
        assert!(items_with(&db, green.id).unwrap().is_empty());

        // Items without a readable file id are known by path alone.
        assign(&db, green.id, &at(r"\\server\share\doc.txt", 0)).unwrap();
        assert_eq!(tags_of(&db, &at(r"\\SERVER\share\doc.txt", 0)).unwrap(), [green.id]);
        assert_eq!(items_with(&db, green.id).unwrap().len(), 1);
        unassign(&db, green.id, &at(r"\\server\share\doc.txt", 0)).unwrap();
        assert_eq!(items(&db).unwrap().len(), 1);
    }
}
