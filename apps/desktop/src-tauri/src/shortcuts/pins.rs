//! Commands for pinned places and their groups.

use std::sync::Arc;

use kestrel_db::shortcuts::pins::{self, Group, Pin, Target};
use serde::Serialize;
use tauri::State;

use super::{Data, entries_by_id, exists, identity};
use crate::engine::Engine;

/// Most items one pinning gesture takes; a sidebar with more is no use to anyone.
const MAX_AT_ONCE: usize = 50;

#[derive(Serialize)]
pub struct PinView {
    #[serde(flatten)]
    pin: Pin,
    /// The number key that opens it (Ctrl+1 to Ctrl+9), whether claimed or given in order.
    key: Option<u8>,
    /// What it points at is gone from where it was.
    missing: bool,
}

#[derive(Serialize)]
pub struct BoardView {
    groups: Vec<Group>,
    pins: Vec<PinView>,
}

/// Every group and pin in sidebar order, each pin with its number key and whether its target
/// is still there.
#[tauri::command(async)]
pub fn pins_board(db: Data) -> Result<BoardView, String> {
    let board = pins::board(&db)?;
    let pins = board
        .pins
        .iter()
        .map(|pin| PinView { key: board.key_of(pin.id), missing: !exists(&pin.path), pin: pin.clone() })
        .collect();
    Ok(BoardView { groups: board.groups, pins })
}

#[derive(Serialize)]
pub struct Pinned {
    /// Newly pinned.
    added: usize,
    /// Already pinned, so left where they were.
    existing: usize,
}

/// Pins files and folders at the end of `group` (`None`: above every group).
#[tauri::command(async)]
pub fn pins_add(db: Data, paths: Vec<String>, group: Option<i64>) -> Result<Pinned, String> {
    if paths.len() > MAX_AT_ONCE {
        return Err(format!("Pin at most {MAX_AT_ONCE} items at a time."));
    }
    let mut pinned = Pinned { added: 0, existing: 0 };
    for path in &paths {
        let path = crate::engine::normalize(path);
        let metadata = std::fs::metadata(&path).map_err(|_| format!("Can't pin {path}: it isn't there."))?;
        let (volume, file_id) = identity(&path);
        let target = Target { path: &path, volume: &volume, file_id, dir: metadata.is_dir() };
        match pins::add(&db, &target, group)? {
            (_, true) => pinned.added += 1,
            (_, false) => pinned.existing += 1,
        }
    }
    Ok(pinned)
}

#[tauri::command(async)]
pub fn pins_remove(db: Data, id: i64) -> Result<(), String> {
    Ok(pins::remove(&db, id)?)
}

/// Sets what the pin is called in the sidebar; blank restores its target's own name.
#[tauri::command(async)]
pub fn pins_set_alias(db: Data, id: i64, alias: Option<String>) -> Result<(), String> {
    Ok(pins::set_alias(&db, id, alias.as_deref())?)
}

/// Gives the pin a number key of its own (1 to 9), or with `None`, the next one free.
#[tauri::command(async)]
pub fn pins_set_key(db: Data, id: i64, key: Option<u8>) -> Result<(), String> {
    Ok(pins::set_slot(&db, id, key)?)
}

/// Moves a pin to position `index` of `group`.
#[tauri::command(async)]
pub fn pins_move(db: Data, id: i64, group: Option<i64>, index: usize) -> Result<(), String> {
    Ok(pins::move_pin(&db, id, group, index)?)
}

/// Finds a missing pin's target again by its file id, through the index, and points the pin
/// there. Returns the new path.
#[tauri::command(async)]
pub fn pins_locate(db: Data, engine: State<'_, Arc<Engine>>, id: i64) -> Result<String, String> {
    let pin = pins::get(&db, id)?;
    let name = pin.alias.clone().unwrap_or_else(|| crate::fileops::file_name(&pin.path).to_owned());
    if pin.file_id == 0 {
        return Err(format!("Kestrel didn't learn {name}'s file id when it was pinned, so it can't be found."));
    }
    let found = {
        let index = engine.index().read();
        entries_by_id(&index, &pin.volume, pin.file_id).first().map(|&entry| index.path(entry))
    };
    match found.filter(|path| exists(path)) {
        Some(path) => {
            pins::retarget(&db, id, &path, pin.file_id)?;
            Ok(path)
        }
        None => Err(format!("Couldn't find {name} anywhere in the index. It may have been deleted.")),
    }
}

#[tauri::command(async)]
pub fn pins_create_group(db: Data, name: String, color: Option<String>) -> Result<Group, String> {
    Ok(pins::create_group(&db, &name, color.as_deref())?)
}

/// Changes a group: any of its name, colour (`""` takes it away) and whether it is collapsed.
#[tauri::command(async)]
pub fn pins_update_group(
    db: Data,
    id: i64,
    name: Option<String>,
    color: Option<String>,
    collapsed: Option<bool>,
) -> Result<(), String> {
    if let Some(name) = name {
        pins::rename_group(&db, id, &name)?;
    }
    if let Some(color) = color {
        pins::set_group_color(&db, id, Some(color.as_str()).filter(|color| !color.is_empty()))?;
    }
    if let Some(collapsed) = collapsed {
        pins::set_group_collapsed(&db, id, collapsed)?;
    }
    Ok(())
}

#[tauri::command(async)]
pub fn pins_move_group(db: Data, id: i64, index: usize) -> Result<(), String> {
    Ok(pins::move_group(&db, id, index)?)
}

/// Deletes a group; its pins stay, above every group.
#[tauri::command(async)]
pub fn pins_delete_group(db: Data, id: i64) -> Result<(), String> {
    Ok(pins::delete_group(&db, id)?)
}
