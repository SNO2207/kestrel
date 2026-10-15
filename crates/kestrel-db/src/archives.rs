//! What the archive dialogs remember between runs: whether "Extract all" shows the extracted
//! files when it is done, and the format and level "Compress to…" was last used with. Kept in
//! the settings table, so the feature has no table of its own and no migration. Passwords are
//! never kept, here or anywhere.

use serde::{Deserialize, Serialize};

use crate::{Db, Result};

const KEY: &str = "archives.preferences";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// "Show extracted files when complete", on by default as in Explorer.
    pub show_extracted: bool,
    /// `zip`, `7z` or `tar-gz`.
    pub format: String,
    /// `store`, `fast`, `normal` or `maximum`.
    pub level: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences { show_extracted: true, format: "zip".into(), level: "normal".into() }
    }
}

impl Db {
    pub fn archive_preferences(&self) -> Result<Preferences> {
        Ok(self.setting(KEY)?.unwrap_or_default())
    }

    pub fn set_archive_preferences(&self, preferences: &Preferences) -> Result<()> {
        self.set_setting(KEY, preferences)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_start_as_explorer_has_them_and_are_remembered() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.archive_preferences().unwrap(), Preferences::default());
        let chosen = Preferences { show_extracted: false, format: "7z".into(), level: "maximum".into() };
        db.set_archive_preferences(&chosen).unwrap();
        assert_eq!(db.archive_preferences().unwrap(), chosen);
        // A setting written by an older version, missing fields, still reads.
        db.set_setting(KEY, &serde_json::json!({ "format": "tar-gz" })).unwrap();
        assert_eq!(db.archive_preferences().unwrap().format, "tar-gz");
        assert!(db.archive_preferences().unwrap().show_extracted);
    }
}
