//! Explorer parity, second round: Folder Options' "Clear File Explorer history", for Kestrel's own
//! records only.
//!
//! Explorer's Clear empties Quick access's recent files and frequent folders. Kestrel's
//! equivalents are its own tables: the folder visits behind the frequent folders, Go to folder
//! and the palette's recency (`shortcuts::frecency`), the files opened through Kestrel that Home
//! lists as recent (`parity`), and the palette's remembered items (`palette`). Clearing empties
//! those and notes when it happened ([`CLEARED_SETTING`]), so Home leaves out the Windows Recent
//! items from before then: Windows' own Recent folder is never touched.
//!
//! The switches that hide recent files and frequent folders are view options, kept by the front
//! end; nothing here stops anything being recorded.

use serde::Serialize;

use crate::{Db, Result};

/// When the history was last cleared, in milliseconds since the Unix epoch. The feature keeps
/// no tables of its own: what it clears belongs to other features, and this is a setting.
pub const CLEARED_SETTING: &str = "parity2.history_cleared_at";

/// How many records a clearing removed, from each table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cleared {
    /// Folder visits: the frequent folders.
    pub visits: usize,
    /// Files opened through Kestrel: Home's recent files.
    pub opened: usize,
    /// Items remembered by the command palette.
    pub palette: usize,
}

/// Forgets every folder visit, opened file and palette item at once, and notes the time `now`
/// (milliseconds since the Unix epoch).
pub fn clear_history(db: &Db, now: i64) -> Result<Cleared> {
    let cleared = db.transaction(|t| {
        Ok(Cleared {
            visits: t.execute("DELETE FROM folder_visits", [])?,
            opened: t.execute("DELETE FROM opened_files", [])?,
            palette: t.execute("DELETE FROM palette_recent", [])?,
        })
    })?;
    db.set_setting(CLEARED_SETTING, &now)?;
    Ok(cleared)
}

/// When the history was last cleared (milliseconds since the Unix epoch), or 0 if never.
pub fn cleared_at(db: &Db) -> i64 {
    db.setting::<i64>(CLEARED_SETTING).ok().flatten().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{self, Use};
    use crate::parity::{opened, record_opened};
    use crate::shortcuts::frecency;

    #[test]
    fn clearing_forgets_visits_opened_files_and_palette_items_and_notes_when() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(cleared_at(&db), 0);
        frecency::record(&db, r"C:\Projects", 1_000).unwrap();
        frecency::record(&db, r"C:\Music", 1_001).unwrap();
        record_opened(&db, r"C:\Projects\notes.txt", 1_000_000).unwrap();
        let item = Use { provider: "actions".into(), id: "toggle-preview".into(), title: "Preview pane".into(), ..Use::default() };
        palette::record(&db, &item, 1_000_000).unwrap();
        // Something of another feature's, which clearing leaves alone.
        db.set_setting("show_hidden", &true).unwrap();

        let cleared = clear_history(&db, 2_000_000).unwrap();
        assert_eq!(cleared, Cleared { visits: 2, opened: 1, palette: 1 });
        assert!(frecency::ranked(&db, 2_000).unwrap().is_empty());
        assert!(opened(&db, 10).unwrap().is_empty());
        assert!(palette::recent(&db).unwrap().is_empty());
        assert_eq!(cleared_at(&db), 2_000_000);
        assert_eq!(db.setting::<bool>("show_hidden").unwrap(), Some(true));

        // Clearing an empty history is no error, and moves the time on.
        assert_eq!(clear_history(&db, 3_000_000).unwrap(), Cleared::default());
        assert_eq!(cleared_at(&db), 3_000_000);
        // New records are kept as usual afterwards.
        frecency::record(&db, r"C:\Projects", 3_100).unwrap();
        assert_eq!(frecency::ranked(&db, 3_100).unwrap().len(), 1);
    }
}
