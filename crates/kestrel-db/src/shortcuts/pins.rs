//! Pinned places: folders and files kept in the sidebar, in user-named groups.
//!
//! Pins outside any group come first, then each group in its order. A pin remembers the file
//! id of what it points at as well as the path, so a pin whose folder was renamed or moved can
//! be found again through the index. Ctrl+1 to Ctrl+9 open pins: a pin may claim a number
//! explicitly, and the numbers no pin claims go to the remaining pins in sidebar order.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use super::{check_color, clean_name, next_position, not_found, renumber};
use crate::{Db, Error, Migration, Result};

pub(super) const MIGRATION: Migration = Migration {
    name: "2026_10_16_shortcuts_pins",
    sql: "
        CREATE TABLE pin_groups (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            color TEXT,
            collapsed INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL
        );
        CREATE TABLE pins (
            id INTEGER PRIMARY KEY,
            group_id INTEGER REFERENCES pin_groups (id) ON DELETE SET NULL,
            path TEXT NOT NULL,
            volume TEXT NOT NULL DEFAULT '',
            file_id INTEGER NOT NULL DEFAULT 0,
            dir INTEGER NOT NULL DEFAULT 1,
            alias TEXT,
            slot INTEGER UNIQUE CHECK (slot BETWEEN 1 AND 9),
            position INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE UNIQUE INDEX pins_path ON pins (path COLLATE NOCASE);
    ",
};

/// How many pins have a number key: Ctrl+1 to Ctrl+9.
pub const SLOTS: u8 = 9;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub color: Option<String>,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pin {
    pub id: i64,
    /// `None` for the pins above every group.
    pub group: Option<i64>,
    pub path: String,
    /// The volume the target was on when pinned, such as `c:`, which scopes its file id.
    pub volume: String,
    /// The target's file id, or 0 if it could not be read.
    pub file_id: u64,
    pub dir: bool,
    /// A name to show instead of the target's own.
    pub alias: Option<String>,
    /// The number key claimed explicitly, if any.
    pub slot: Option<u8>,
}

/// What a new pin points at.
#[derive(Clone, Debug)]
pub struct Target<'a> {
    pub path: &'a str,
    pub volume: &'a str,
    pub file_id: u64,
    pub dir: bool,
}

/// Every group and pin, in sidebar order.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Board {
    pub groups: Vec<Group>,
    /// Ungrouped pins first, then each group's pins, groups in order.
    pub pins: Vec<Pin>,
}

impl Board {
    /// The pin each number key opens: `keys()[0]` for Ctrl+1. Pins that claimed a number keep
    /// it; the other numbers go, in increasing order, to the unnumbered pins in sidebar order.
    pub fn keys(&self) -> [Option<i64>; SLOTS as usize] {
        let mut keys = [None; SLOTS as usize];
        for pin in &self.pins {
            if let Some(slot) = pin.slot.filter(|slot| (1..=SLOTS).contains(slot)) {
                keys[slot as usize - 1] = Some(pin.id);
            }
        }
        let mut unnumbered = self.pins.iter().filter(|pin| pin.slot.is_none());
        for key in keys.iter_mut().filter(|key| key.is_none()) {
            match unnumbered.next() {
                Some(pin) => *key = Some(pin.id),
                None => break,
            }
        }
        keys
    }

    /// The number key that opens `pin`, from 1 to 9.
    pub fn key_of(&self, pin: i64) -> Option<u8> {
        self.keys().iter().position(|key| *key == Some(pin)).map(|at| at as u8 + 1)
    }
}

/// Qualified, because the sidebar order joins the groups, which have an `id` of their own.
const PIN_COLUMNS: &str = "pins.id, pins.group_id, pins.path, pins.volume, pins.file_id, pins.dir, pins.alias, pins.slot";

fn pin_from(row: &Row<'_>) -> rusqlite::Result<Pin> {
    Ok(Pin {
        id: row.get(0)?,
        group: row.get(1)?,
        path: row.get(2)?,
        volume: row.get(3)?,
        // SQLite integers are signed; the id's bits are kept as they are.
        file_id: row.get::<_, i64>(4)? as u64,
        dir: row.get(5)?,
        alias: row.get(6)?,
        slot: row.get(7)?,
    })
}

/// Every group and pin, in sidebar order.
pub fn board(db: &Db) -> Result<Board> {
    db.with(|c| {
        let groups = c
            .prepare("SELECT id, name, color, collapsed FROM pin_groups ORDER BY position, id")?
            .query_map([], |row| {
                Ok(Group { id: row.get(0)?, name: row.get(1)?, color: row.get(2)?, collapsed: row.get(3)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let pins = c
            .prepare(&format!(
                "SELECT {PIN_COLUMNS} FROM pins LEFT JOIN pin_groups g ON g.id = pins.group_id
                 ORDER BY pins.group_id IS NOT NULL, g.position, g.id, pins.position, pins.id"
            ))?
            .query_map([], pin_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Board { groups, pins })
    })
}

/// One pin.
pub fn get(db: &Db, id: i64) -> Result<Pin> {
    db.with(|c| c.query_row(&format!("SELECT {PIN_COLUMNS} FROM pins WHERE id = ?1"), params![id], pin_from).optional())?
        .ok_or_else(|| not_found("pin"))
}

fn group_exists(c: &Connection, group: Option<i64>) -> rusqlite::Result<bool> {
    match group {
        None => Ok(true),
        Some(id) => c.query_row("SELECT EXISTS(SELECT 1 FROM pin_groups WHERE id = ?1)", params![id], |row| row.get(0)),
    }
}

fn group_filter(group: Option<i64>) -> String {
    match group {
        None => "group_id IS NULL".into(),
        Some(id) => format!("group_id = {id}"),
    }
}

/// Pins `target` at the end of `group` (`None`: above every group). A path that is already
/// pinned is not pinned twice: the existing pin is returned, with `false`.
pub fn add(db: &Db, target: &Target<'_>, group: Option<i64>) -> Result<(Pin, bool)> {
    let path = target.path.trim();
    if path.is_empty() {
        return Err(Error("Nothing to pin".into()));
    }
    let id = db.transaction(|t| {
        let existing: Option<i64> =
            t.query_row("SELECT id FROM pins WHERE path = ?1 COLLATE NOCASE", params![path], |row| row.get(0)).optional()?;
        if let Some(id) = existing {
            return Ok(Err(id));
        }
        if !group_exists(t, group)? {
            return Ok(Ok(None));
        }
        let position = next_position(t, "pins", &group_filter(group))?;
        t.execute(
            "INSERT INTO pins (group_id, path, volume, file_id, dir, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![group, path, target.volume, target.file_id as i64, target.dir, position],
        )?;
        Ok(Ok(Some(t.last_insert_rowid())))
    })?;
    match id {
        Ok(Some(id)) => Ok((get(db, id)?, true)),
        Ok(None) => Err(not_found("group")),
        Err(existing) => Ok((get(db, existing)?, false)),
    }
}

fn changed(rows: usize, what: &str) -> Result<()> {
    if rows == 0 { Err(not_found(what)) } else { Ok(()) }
}

/// Unpins. Nothing on disk is touched.
pub fn remove(db: &Db, id: i64) -> Result<()> {
    changed(db.with(|c| c.execute("DELETE FROM pins WHERE id = ?1", params![id]))?, "pin")
}

/// Shows the pin under `alias`, or under its target's own name when `alias` is `None` or blank.
pub fn set_alias(db: &Db, id: i64, alias: Option<&str>) -> Result<()> {
    let alias = match alias.map(str::trim).filter(|alias| !alias.is_empty()) {
        Some(alias) => Some(clean_name(alias)?),
        None => None,
    };
    changed(db.with(|c| c.execute("UPDATE pins SET alias = ?1 WHERE id = ?2", params![alias, id]))?, "pin")
}

/// Gives the pin number key `slot` (1 to 9), taking it from any pin that had it, or returns
/// the pin to automatic numbering with `None`.
pub fn set_slot(db: &Db, id: i64, slot: Option<u8>) -> Result<()> {
    if let Some(slot) = slot
        && !(1..=SLOTS).contains(&slot)
    {
        return Err(Error(format!("Pins have the keys Ctrl+1 to Ctrl+{SLOTS}")));
    }
    let rows = db.transaction(|t| {
        if slot.is_some() {
            t.execute("UPDATE pins SET slot = NULL WHERE slot = ?1 AND id != ?2", params![slot, id])?;
        }
        t.execute("UPDATE pins SET slot = ?1 WHERE id = ?2", params![slot, id])
    })?;
    changed(rows, "pin")
}

/// Points the pin at where its target is now, after it was found again.
pub fn retarget(db: &Db, id: i64, path: &str, file_id: u64) -> Result<()> {
    let rows = db.transaction(|t| {
        // Another pin may already point there; then this one is the duplicate and goes.
        let other: Option<i64> = t
            .query_row("SELECT id FROM pins WHERE path = ?1 COLLATE NOCASE AND id != ?2", params![path, id], |row| row.get(0))
            .optional()?;
        if other.is_some() {
            return t.execute("DELETE FROM pins WHERE id = ?1", params![id]);
        }
        t.execute("UPDATE pins SET path = ?1, file_id = ?2 WHERE id = ?3", params![path, file_id as i64, id])
    })?;
    changed(rows, "pin")
}

/// The ids of a group's pins in order, or of the ungrouped pins for `None`.
fn pin_ids(c: &Connection, group: Option<i64>) -> rusqlite::Result<Vec<i64>> {
    c.prepare(&format!("SELECT id FROM pins WHERE {} ORDER BY position, id", group_filter(group)))?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// Moves a pin to position `index` of `group` (`None`: above every group), counted among the
/// other pins there. An index past the end puts it last.
pub fn move_pin(db: &Db, id: i64, group: Option<i64>, index: usize) -> Result<()> {
    let outcome = db.transaction(|t| {
        let from: Option<Option<i64>> =
            t.query_row("SELECT group_id FROM pins WHERE id = ?1", params![id], |row| row.get(0)).optional()?;
        let Some(from) = from else { return Ok(Err("pin")) };
        if !group_exists(t, group)? {
            return Ok(Err("group"));
        }
        t.execute("UPDATE pins SET group_id = ?1 WHERE id = ?2", params![group, id])?;
        let mut order: Vec<i64> = pin_ids(t, group)?.into_iter().filter(|&other| other != id).collect();
        order.insert(index.min(order.len()), id);
        renumber(t, "pins", &order)?;
        if from != group {
            renumber(t, "pins", &pin_ids(t, from)?)?;
        }
        Ok(Ok(()))
    })?;
    outcome.map_err(not_found)
}

/// Adds a group at the end.
pub fn create_group(db: &Db, name: &str, color: Option<&str>) -> Result<Group> {
    let name = clean_name(name)?;
    let color = color.map(check_color).transpose()?;
    let id = db.transaction(|t| {
        let position = next_position(t, "pin_groups", "1")?;
        t.execute("INSERT INTO pin_groups (name, color, position) VALUES (?1, ?2, ?3)", params![name, color, position])?;
        Ok(t.last_insert_rowid())
    })?;
    Ok(Group { id, name, color, collapsed: false })
}

pub fn rename_group(db: &Db, id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    changed(db.with(|c| c.execute("UPDATE pin_groups SET name = ?1 WHERE id = ?2", params![name, id]))?, "group")
}

/// Colours a group, or takes its colour away with `None`.
pub fn set_group_color(db: &Db, id: i64, color: Option<&str>) -> Result<()> {
    let color = color.map(check_color).transpose()?;
    changed(db.with(|c| c.execute("UPDATE pin_groups SET color = ?1 WHERE id = ?2", params![color, id]))?, "group")
}

pub fn set_group_collapsed(db: &Db, id: i64, collapsed: bool) -> Result<()> {
    changed(db.with(|c| c.execute("UPDATE pin_groups SET collapsed = ?1 WHERE id = ?2", params![collapsed, id]))?, "group")
}

/// Moves a group to position `index` among the others.
pub fn move_group(db: &Db, id: i64, index: usize) -> Result<()> {
    let found = db.transaction(|t| {
        let ids: Vec<i64> =
            t.prepare("SELECT id FROM pin_groups ORDER BY position, id")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        if !ids.contains(&id) {
            return Ok(false);
        }
        let mut order: Vec<i64> = ids.into_iter().filter(|&other| other != id).collect();
        order.insert(index.min(order.len()), id);
        renumber(t, "pin_groups", &order)?;
        Ok(true)
    })?;
    if found { Ok(()) } else { Err(not_found("group")) }
}

/// Deletes a group. Its pins are kept: they move, in order, to the end of the ungrouped pins.
pub fn delete_group(db: &Db, id: i64) -> Result<()> {
    let rows = db.transaction(|t| {
        let mut order = pin_ids(t, None)?;
        order.extend(pin_ids(t, Some(id))?);
        t.execute("UPDATE pins SET group_id = NULL WHERE group_id = ?1", params![id])?;
        renumber(t, "pins", &order)?;
        t.execute("DELETE FROM pin_groups WHERE id = ?1", params![id])
    })?;
    changed(rows, "group")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str) -> Target<'_> {
        Target { path, volume: "c:", file_id: path.len() as u64, dir: true }
    }

    fn paths(db: &Db) -> Vec<String> {
        board(db).unwrap().pins.into_iter().map(|pin| pin.path).collect()
    }

    #[test]
    fn pins_keep_their_order_within_groups() {
        let db = Db::open_in_memory().unwrap();
        let work = create_group(&db, " Work ", Some("Blue")).unwrap();
        assert_eq!((work.name.as_str(), work.color.as_deref()), ("Work", Some("blue")));
        let home = create_group(&db, "Home", None).unwrap();

        let (a, new) = add(&db, &target(r"C:\a"), None).unwrap();
        assert!(new);
        let (b, _) = add(&db, &target(r"C:\b"), Some(work.id)).unwrap();
        let (c, _) = add(&db, &target(r"C:\c"), Some(home.id)).unwrap();
        let (d, _) = add(&db, &target(r"C:\d"), Some(work.id)).unwrap();
        // Ungrouped first, then the groups in the order they were made.
        assert_eq!(paths(&db), [r"C:\a", r"C:\b", r"C:\d", r"C:\c"]);

        // The same path, in any case, is not pinned twice.
        let (again, new) = add(&db, &target(r"c:\A"), Some(home.id)).unwrap();
        assert!(!new);
        assert_eq!(again.id, a.id);

        // Reorder within a group, then move across groups.
        move_pin(&db, d.id, Some(work.id), 0).unwrap();
        assert_eq!(paths(&db), [r"C:\a", r"C:\d", r"C:\b", r"C:\c"]);
        move_pin(&db, a.id, Some(home.id), 0).unwrap();
        assert_eq!(paths(&db), [r"C:\d", r"C:\b", r"C:\a", r"C:\c"]);
        move_pin(&db, c.id, None, 99).unwrap();
        assert_eq!(paths(&db), [r"C:\c", r"C:\d", r"C:\b", r"C:\a"]);

        // Groups reorder too, carrying their pins.
        move_group(&db, home.id, 0).unwrap();
        let board = board(&db).unwrap();
        assert_eq!(board.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["Home", "Work"]);
        assert_eq!(paths(&db), [r"C:\c", r"C:\a", r"C:\d", r"C:\b"]);
        assert_eq!(board.pins[1].group, Some(home.id));

        // Deleting a group keeps its pins, moved to the end of the ungrouped ones.
        delete_group(&db, work.id).unwrap();
        let board = super::board(&db).unwrap();
        assert_eq!(board.groups.len(), 1);
        assert_eq!(paths(&db), [r"C:\c", r"C:\d", r"C:\b", r"C:\a"]);
        assert!(board.pins[..3].iter().all(|pin| pin.group.is_none()));

        // Unknown groups and pins are reported, not ignored.
        assert!(add(&db, &target(r"C:\e"), Some(work.id)).is_err());
        assert!(move_pin(&db, b.id, Some(work.id), 0).is_err());
        assert!(move_group(&db, work.id, 0).is_err());
        remove(&db, b.id).unwrap();
        assert!(remove(&db, b.id).is_err());
        assert_eq!(paths(&db), [r"C:\c", r"C:\d", r"C:\a"]);
    }

    #[test]
    fn groups_are_renamed_coloured_and_collapsed() {
        let db = Db::open_in_memory().unwrap();
        let group = create_group(&db, "Clients", None).unwrap();
        rename_group(&db, group.id, "Customers").unwrap();
        set_group_color(&db, group.id, Some("green")).unwrap();
        set_group_collapsed(&db, group.id, true).unwrap();
        assert_eq!(board(&db).unwrap().groups, [Group {
            id: group.id,
            name: "Customers".into(),
            color: Some("green".into()),
            collapsed: true
        }]);
        set_group_color(&db, group.id, None).unwrap();
        assert_eq!(board(&db).unwrap().groups[0].color, None);
        assert!(rename_group(&db, group.id, "  ").is_err());
        assert!(set_group_color(&db, group.id, Some("plaid")).is_err());
        assert!(create_group(&db, "", None).is_err());
    }

    #[test]
    fn aliases_and_retargeting() {
        let db = Db::open_in_memory().unwrap();
        let (pin, _) = add(&db, &target(r"C:\Projects\Kestrel"), None).unwrap();
        assert_eq!((pin.volume.as_str(), pin.file_id, pin.alias.clone()), ("c:", 19, None));
        set_alias(&db, pin.id, Some("  Kestrel source ")).unwrap();
        assert_eq!(get(&db, pin.id).unwrap().alias.as_deref(), Some("Kestrel source"));
        set_alias(&db, pin.id, Some(" ")).unwrap();
        assert_eq!(get(&db, pin.id).unwrap().alias, None);

        retarget(&db, pin.id, r"C:\Code\Kestrel", 77).unwrap();
        let moved = get(&db, pin.id).unwrap();
        assert_eq!((moved.path.as_str(), moved.file_id), (r"C:\Code\Kestrel", 77));
        // Retargeting onto a path another pin has drops the duplicate.
        let (other, _) = add(&db, &target(r"C:\Other"), None).unwrap();
        retarget(&db, other.id, r"c:\code\kestrel", 77).unwrap();
        assert_eq!(paths(&db), [r"C:\Code\Kestrel"]);
    }

    #[test]
    fn number_keys_go_to_claimed_slots_then_in_order() {
        let db = Db::open_in_memory().unwrap();
        let ids: Vec<i64> = (0..11).map(|n| add(&db, &target(&format!(r"C:\p{n:02}")), None).unwrap().0.id).collect();
        // Without claims, the first nine pins have Ctrl+1 to Ctrl+9.
        let keys = board(&db).unwrap().keys();
        assert_eq!(keys.to_vec(), ids[..9].iter().map(|&id| Some(id)).collect::<Vec<_>>());

        // The tenth pin claims 1 and the third claims 5: the rest fill the gaps in order.
        set_slot(&db, ids[9], Some(1)).unwrap();
        set_slot(&db, ids[2], Some(5)).unwrap();
        let board = board(&db).unwrap();
        let expected = [ids[9], ids[0], ids[1], ids[3], ids[2], ids[4], ids[5], ids[6], ids[7]];
        assert_eq!(board.keys().to_vec(), expected.iter().map(|&id| Some(id)).collect::<Vec<_>>());
        assert_eq!(board.key_of(ids[9]), Some(1));
        assert_eq!(board.key_of(ids[8]), None, "pushed past the ninth key");

        // Claiming a taken number takes it over; giving it up returns to automatic numbering.
        set_slot(&db, ids[10], Some(1)).unwrap();
        assert_eq!(get(&db, ids[9]).unwrap().slot, None);
        assert_eq!(super::board(&db).unwrap().key_of(ids[10]), Some(1));
        set_slot(&db, ids[10], None).unwrap();
        assert!(set_slot(&db, ids[0], Some(10)).is_err());

        // Fewer pins than keys leave the rest empty.
        let small = Db::open_in_memory().unwrap();
        let (only, _) = add(&small, &target(r"C:\only"), None).unwrap();
        set_slot(&small, only.id, Some(3)).unwrap();
        assert_eq!(super::board(&small).unwrap().keys(), [None, None, Some(only.id), None, None, None, None, None, None]);
    }
}
