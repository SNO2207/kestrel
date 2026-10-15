//! Commands for saved searches.

use kestrel_db::shortcuts::searches::{self, SavedSearch};

use super::Data;

#[tauri::command(async)]
pub fn searches_list(db: Data) -> Result<Vec<SavedSearch>, String> {
    Ok(searches::list(&db)?)
}

/// Saves a search: its text, and the folder it searches beneath (`None`: everywhere).
#[tauri::command(async)]
pub fn searches_save(db: Data, name: String, query: String, scope: Option<String>) -> Result<SavedSearch, String> {
    Ok(searches::create(&db, &name, &query, scope.as_deref())?)
}

#[tauri::command(async)]
pub fn searches_rename(db: Data, id: i64, name: String) -> Result<(), String> {
    Ok(searches::rename(&db, id, &name)?)
}

#[tauri::command(async)]
pub fn searches_delete(db: Data, id: i64) -> Result<(), String> {
    Ok(searches::delete(&db, id)?)
}
