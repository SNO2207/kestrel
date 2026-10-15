//! Which entries folder listings and searches include: the "Show hidden items" view option.
//!
//! Explorer leaves hidden entries out unless the user asks for them, and with them the
//! protected operating system files, which are hidden and system both. An entry that is only
//! marked system, such as `C:\Windows\Fonts`, is shown like any other. The filter is applied
//! by the engine, before sorting, so a hidden file never occupies a row position and counts,
//! selection ranges and row fetching all agree with what is on screen.

use kestrel_index::attr;

/// Attribute bits that keep an entry out of view unless hidden items are shown.
const CONCEALING: u32 = attr::HIDDEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    /// Hidden entries are included.
    pub hidden: bool,
}

impl Visibility {
    /// Everything, as the engine listed before the option existed.
    pub const ALL: Self = Self { hidden: true };

    /// The visibility a command asked for. A caller that does not say gets everything, so
    /// older callers keep their behaviour.
    pub fn from_request(show_hidden: Option<bool>) -> Self {
        Self { hidden: show_hidden.unwrap_or(true) }
    }

    /// Whether an entry with these attributes belongs in the view.
    pub fn shows(self, attrs: u32) -> bool {
        self.hidden || attrs & CONCEALING == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, SortField, SortSpec};
    use kestrel_index::{IndexBuilder, walk};
    use std::fs;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, SetFileAttributesW};

    const BY_NAME: SortSpec = SortSpec { key: SortField::Name, desc: false };
    const VISIBLE_ONLY: Visibility = Visibility { hidden: false };

    #[test]
    fn hidden_entries_are_concealed_unless_asked_for() {
        let plain = attr::READONLY;
        assert!(VISIBLE_ONLY.shows(0));
        assert!(VISIBLE_ONLY.shows(plain | attr::DIRECTORY));
        assert!(!VISIBLE_ONLY.shows(attr::HIDDEN));
        // Explorer shows system items that are not hidden, such as C:\Windows\Fonts.
        assert!(VISIBLE_ONLY.shows(attr::READONLY | attr::SYSTEM | attr::DIRECTORY));
        assert!(!VISIBLE_ONLY.shows(attr::HIDDEN | attr::SYSTEM));
        for attrs in [0, attr::HIDDEN, attr::SYSTEM, attr::HIDDEN | attr::SYSTEM] {
            assert!(Visibility::ALL.shows(attrs));
        }
        assert_eq!(Visibility::from_request(None), Visibility::ALL);
        assert_eq!(Visibility::from_request(Some(false)), VISIBLE_ONLY);
    }

    fn set_attributes(path: &Path, attributes: u32) {
        let wide: Vec<u16> = path.to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        assert_ne!(unsafe { SetFileAttributesW(wide.as_ptr(), attributes) }, 0);
    }

    fn hide(path: &Path) {
        set_attributes(path, FILE_ATTRIBUTE_HIDDEN);
    }

    fn names(engine: &Engine, set: u32) -> Vec<String> {
        engine.rows(set, 0, 100).unwrap().into_iter().map(|row| row.name).collect()
    }

    #[test]
    fn listings_and_searches_leave_hidden_files_out() {
        let root = std::env::temp_dir().join(format!("kestrel-hidden-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("secret-dir")).unwrap();
        fs::write(root.join("shown.txt"), b"1").unwrap();
        fs::write(root.join("secret.txt"), b"22").unwrap();
        fs::create_dir_all(root.join("system-dir")).unwrap();
        hide(&root.join("secret.txt"));
        hide(&root.join("secret-dir"));
        // System but not hidden, as C:\Windows\Fonts is: Explorer shows it, and so does Kestrel.
        set_attributes(&root.join("system-dir"), FILE_ATTRIBUTE_SYSTEM);
        let root_str = root.to_str().unwrap().to_owned();

        // A folder outside the index is listed straight from disk.
        let engine = Engine::default();
        let live = engine.list_dir_with(&root_str, BY_NAME, VISIBLE_ONLY).unwrap();
        assert_eq!(names(&engine, live.set), ["system-dir", "shown.txt"]);
        assert_eq!(live.max_size, 1, "hidden files do not count towards the size scale");
        let all = engine.list_dir_with(&root_str, BY_NAME, Visibility::ALL).unwrap();
        assert_eq!(names(&engine, all.set), ["secret-dir", "system-dir", "secret.txt", "shown.txt"]);
        // The original entry point still lists everything.
        assert_eq!(engine.list_dir(&root_str, BY_NAME).unwrap().total, 4);

        // The same folder, indexed.
        let mut builder = IndexBuilder::new();
        walk::walk(&root_str, &mut builder).unwrap();
        engine.install(builder.finish());
        let indexed = engine.list_dir_with(&root_str, BY_NAME, VISIBLE_ONLY).unwrap();
        assert_eq!(names(&engine, indexed.set), ["system-dir", "shown.txt"]);
        // Search results are sorted by name alone, folders among files.
        let found = engine.search_with("s", Some(&root_str), BY_NAME, VISIBLE_ONLY).unwrap();
        assert_eq!(names(&engine, found.set), ["shown.txt", "system-dir"]);
        let found = engine.search_with("s", Some(&root_str), BY_NAME, Visibility::ALL).unwrap();
        assert_eq!(names(&engine, found.set), ["secret-dir", "secret.txt", "shown.txt", "system-dir"]);
        fs::remove_dir_all(&root).unwrap();
    }
}
