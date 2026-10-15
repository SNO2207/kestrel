//! Commands for workspaces: saved sets of tabs.

use kestrel_db::shortcuts::workspaces::{self, TabState, Workspace};

use super::{Data, now};

#[tauri::command(async)]
pub fn workspaces_list(db: Data) -> Result<Vec<Workspace>, String> {
    Ok(workspaces::list(&db)?)
}

/// Saves the tabs under `name`, replacing the workspace of that name if there is one.
#[tauri::command(async)]
pub fn workspaces_save(db: Data, name: String, tabs: Vec<TabState>, active: usize) -> Result<Workspace, String> {
    Ok(workspaces::save(&db, &name, &tabs, active, now())?)
}

#[tauri::command(async)]
pub fn workspaces_rename(db: Data, id: i64, name: String) -> Result<(), String> {
    Ok(workspaces::rename(&db, id, &name)?)
}

#[tauri::command(async)]
pub fn workspaces_delete(db: Data, id: i64) -> Result<(), String> {
    Ok(workspaces::delete(&db, id)?)
}
