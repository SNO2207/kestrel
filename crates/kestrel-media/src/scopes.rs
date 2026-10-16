//! Where the media library looks by default: Pictures, Videos, Desktop, Downloads, Documents
//! and the camera roll folders, wherever Windows keeps them. When OneDrive backs up Pictures,
//! Documents or Desktop, Windows' known folders point into OneDrive while the profile's own
//! folders of the same names often still hold files, so both are included. A folder inside
//! another scope is left out, as the outer one covers it.

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_CameraRoll, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_SavedPictures,
    FOLDERID_Screenshots, FOLDERID_SkyDriveCameraRoll, FOLDERID_SkyDrivePictures, FOLDERID_Videos, KF_FLAG_DONT_VERIFY,
    SHGetKnownFolderPath,
};
use windows::core::GUID;

fn known_folder(id: &GUID) -> Option<String> {
    // SAFETY: the returned string is copied and freed once, whether or not the call succeeds.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
        let copy = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        copy.filter(|path| !path.is_empty())
    }
}

/// Whether `inner` is `outer` or lies inside it, compared without case.
pub fn is_within(inner: &str, outer: &str) -> bool {
    let inner = inner.trim_end_matches('\\').to_lowercase();
    let outer = outer.trim_end_matches('\\').to_lowercase();
    inner == outer || inner.strip_prefix(&outer).is_some_and(|rest| rest.starts_with('\\'))
}

/// Keeps the outermost of folders that lie inside one another, in the order given, and each
/// folder once.
pub fn outermost(folders: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for folder in folders {
        let folder = folder.trim_end_matches('\\').to_owned();
        let folder = if folder.ends_with(':') { format!("{folder}\\") } else { folder };
        if kept.iter().any(|outer| is_within(&folder, outer)) {
            continue;
        }
        kept.retain(|inner| !is_within(inner, &folder));
        kept.push(folder);
    }
    kept
}

/// The default scopes on this computer, that exist.
pub fn defaults() -> Vec<String> {
    let ids = [
        FOLDERID_Pictures,
        FOLDERID_Videos,
        FOLDERID_Desktop,
        FOLDERID_Downloads,
        FOLDERID_Documents,
        FOLDERID_CameraRoll,
        FOLDERID_SavedPictures,
        FOLDERID_Screenshots,
        FOLDERID_SkyDriveCameraRoll,
        FOLDERID_SkyDrivePictures,
    ];
    let mut folders: Vec<String> = ids.iter().filter_map(known_folder).collect();
    // Where those folders were before OneDrive moved them, if anything is still there.
    if let Some(profile) = std::env::var_os("USERPROFILE").and_then(|p| p.into_string().ok()) {
        for name in ["Pictures", "Videos", "Desktop", "Downloads", "Documents"] {
            folders.push(format!("{profile}\\{name}"));
        }
    }
    folders.retain(|folder| std::path::Path::new(folder).is_dir());
    outermost(folders)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_give_way_to_the_outer_one() {
        let folders = vec![
            r"C:\Users\me\OneDrive\Pictures\Camera Roll".to_owned(),
            r"C:\Users\me\OneDrive\Pictures".to_owned(),
            r"C:\Users\me\Pictures".to_owned(),
            r"c:\users\me\pictures\".to_owned(),
            r"C:\Users\me\Pictures2".to_owned(),
            "D:".to_owned(),
            r"D:\Photos".to_owned(),
        ];
        assert_eq!(outermost(folders), [r"C:\Users\me\OneDrive\Pictures", r"C:\Users\me\Pictures", r"C:\Users\me\Pictures2", r"D:\"]);
        assert!(is_within(r"C:\a\b", r"C:\A"));
        assert!(!is_within(r"C:\ab", r"C:\a"));
    }

    #[test]
    fn defaults_are_folders_that_exist() {
        let _com = crate::Apartment::enter();
        let found = defaults();
        assert!(!found.is_empty(), "a profile has a Pictures or Documents folder");
        for folder in &found {
            assert!(std::path::Path::new(folder).is_dir(), "{folder}");
        }
    }
}
