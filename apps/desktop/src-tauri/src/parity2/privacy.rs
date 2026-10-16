//! Folder Options' privacy section: "Clear File Explorer history", for Kestrel's own records
//! (`kestrel_db::parity2`): the frequent folders, the files opened through Kestrel and the
//! palette's remembered items. Windows' Recent items folder is never touched; Home leaves out
//! its items from before the clearing instead (`parity::recent`).

use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::parity2::{self as store, Cleared};
use tauri::{AppHandle, Emitter, State};

use crate::palette::Palette;

/// When the history was last cleared, in milliseconds since the Unix epoch (0 if never).
pub fn cleared_at(db: &Db) -> i64 {
    store::cleared_at(db)
}

/// Clears Kestrel's history and tells every window, so their lists of frequent folders, Home
/// and the palette forget it at once.
#[tauri::command(async)]
pub fn privacy_clear_history(app: AppHandle, db: State<Arc<Db>>, palette: State<Arc<Palette>>) -> Result<Cleared, String> {
    let cleared = store::clear_history(&db, crate::parity::now_ms())?;
    palette.reread_recents();
    let _ = app.emit("history-cleared", cleared);
    Ok(cleared)
}
