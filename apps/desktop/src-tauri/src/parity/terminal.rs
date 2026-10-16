//! Open in Terminal: Windows Terminal in a folder, as Explorer's menu offers it, or a console
//! window where Windows Terminal is not installed. The projects feature's launcher starts it
//! (`projects::launch`), so the same development hook records the launch in the tests instead
//! of starting anything (`projects_probe`).

use std::path::Path;

use crate::projects::launch;

/// Opens Windows Terminal in `folder`.
#[tauri::command(async)]
pub fn open_in_terminal(folder: String) -> Result<(), String> {
    if !Path::new(&folder).is_dir() {
        return Err(format!("{folder} is not a folder"));
    }
    launch::start(launch::in_terminal(&folder, None))
}
