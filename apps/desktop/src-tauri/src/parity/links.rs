//! Shortcuts (`.lnk` files), made and read through the shell's `IShellLink`, as Explorer does.
//!
//! Kestrel makes them for New > Shortcut (after asking what the shortcut points at) and for
//! files dropped with Alt held, or with Ctrl and Shift together. It reads them for Home's
//! recent files: Windows keeps a shortcut to each recent item in its Recent folder. Reading a
//! shortcut only reads the `.lnk` file: the target is never resolved, searched for or opened,
//! so a cloud-only target is not downloaded.

use std::path::Path;

use tauri::State;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{HSTRING, Interface};

use super::shellnew::{created, unused_name};
use super::{attributes, in_apartment};
use crate::fileops::{self, Done, Outcome};
use crate::undo::Journal;

/// The name of a shortcut to `target` made by dropping it with Alt, as Explorer names it:
/// "report.docx - Shortcut".
pub fn dropped_name(target: &str) -> String {
    format!("{} - Shortcut", fileops::file_name(target))
}

/// The name New > Shortcut suggests for a shortcut to `target`: the target's name, without
/// its extension for a file.
pub fn suggested_name(target: &str) -> String {
    let name = fileops::file_name(target.trim_end_matches('\\'));
    let name = if name.is_empty() || name.ends_with(':') { target.trim_end_matches('\\') } else { name };
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && !Path::new(target).is_dir() => stem.to_owned(),
        _ => name.trim_end_matches(':').to_owned(),
    }
}

/// Writes a shortcut at `link` to `target`. COM must be initialised on the calling thread.
fn write_link(target: &str, link: &Path) -> Result<(), String> {
    let describe = |e: windows::core::Error| e.message();
    // SAFETY: COM is initialised on this thread (see `in_apartment`), and every string passed
    // outlives the call it is passed to.
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(describe)?;
        shell_link.SetPath(&HSTRING::from(target)).map_err(describe)?;
        // A shortcut to a file starts its program in the file's folder, as Explorer's do.
        if !Path::new(target).is_dir() {
            let folder = fileops::parent(target);
            shell_link.SetWorkingDirectory(&HSTRING::from(folder.as_str())).map_err(describe)?;
        }
        let file: IPersistFile = shell_link.cast().map_err(describe)?;
        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(describe)
    }
}

/// The path a shortcut points at, as it is stored in it. COM must be initialised on the
/// calling thread. Nothing at the target is touched.
pub fn read_target(link: &Path) -> Option<String> {
    // SAFETY: COM is initialised on this thread, and the buffer outlives the call that fills
    // it; the find data is optional and not asked for.
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = shell_link.cast().ok()?;
        file.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;
        let mut buffer = [0u16; 2048];
        shell_link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).ok()?;
        let end = buffer.iter().position(|&c| c == 0)?;
        let path = String::from_utf16_lossy(&buffer[..end]);
        let path = super::shellnew::expand_variables(&path);
        (!path.is_empty()).then_some(path)
    }
}

/// Makes a shortcut in `folder` to each of `targets`, named as Explorer names dropped ones,
/// and reports what was made the way the shell's copy engine does, for the undo history.
pub fn make_shortcuts(targets: &[String], folder: &str) -> Result<Outcome, String> {
    if !Path::new(folder).is_dir() {
        return Err(format!("{folder} is not a folder"));
    }
    let (targets, folder) = (targets.to_vec(), folder.to_owned());
    in_apartment(move || {
        let mut outcome = Outcome { completed: true, ..Outcome::default() };
        for (index, target) in targets.iter().enumerate() {
            if attributes(target).is_none() {
                outcome.completed = false;
                continue;
            }
            let link = Path::new(&folder).join(unused_name(Path::new(&folder), &dropped_name(target), "lnk"));
            write_link(target, &link)?;
            outcome.done.push((index, Done::Created { path: link.display().to_string() }));
        }
        Ok(outcome)
    })
}

/// Makes one shortcut called `name` (".lnk" is added) in `folder` to `target`.
pub fn make_shortcut(target: &str, folder: &str, name: &str) -> Result<std::path::PathBuf, String> {
    let target = target.trim().trim_matches('"').to_owned();
    if target.is_empty() {
        return Err("Type the location of the item.".into());
    }
    // Only the attributes are read: a shortcut to a cloud-only file downloads nothing.
    if attributes(&target).is_none() {
        return Err(format!("Windows can't find {target}. Check the location and try again."));
    }
    let name = name.trim().trim_end_matches(".lnk").trim();
    let name = if name.is_empty() { suggested_name(&target) } else { name.to_owned() };
    fileops::validate_name(&name)?;
    let folder_path = Path::new(folder);
    if !folder_path.is_dir() {
        return Err(format!("{folder} is not a folder"));
    }
    let link = folder_path.join(unused_name(folder_path, &name, "lnk"));
    let made = link.clone();
    in_apartment(move || write_link(&target, &made))?;
    Ok(link)
}

// ---------- commands ----------

/// New > Shortcut: makes a shortcut in `folder` to `target`, called `name` (or after the
/// target), records it for undo, and returns the name of the `.lnk` file.
#[tauri::command(async)]
pub fn new_shortcut(journal: State<Journal>, folder: String, target: String, name: Option<String>) -> Result<String, String> {
    let link = make_shortcut(&target, &folder, name.as_deref().unwrap_or(""))?;
    journal.record(&created(&link));
    Ok(fileops::file_name(&link.display().to_string()).to_owned())
}

/// The name New > Shortcut suggests for a target, and whether the target exists.
#[tauri::command(async)]
pub fn shortcut_suggestion(target: String) -> (String, bool) {
    let target = target.trim().trim_matches('"');
    (suggested_name(target), attributes(target).is_some())
}

/// Development builds only: the target a shortcut file points at, for the end-to-end tests.
#[tauri::command(async)]
pub fn shortcut_target_dev(link: String) -> Result<Option<String>, String> {
    if !cfg!(debug_assertions) {
        return Err("Only development builds have this hook".into());
    }
    in_apartment(move || Ok(read_target(Path::new(&link))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn shortcuts_are_named_as_explorer_names_them() {
        assert_eq!(dropped_name(r"C:\Docs\report.docx"), "report.docx - Shortcut");
        assert_eq!(suggested_name(r"C:\Docs\report.docx"), "report");
        assert_eq!(suggested_name(r"C:\Windows"), "Windows");
        assert_eq!(suggested_name(r"D:\"), "D");
        assert_eq!(suggested_name(r"C:\Docs\.gitignore"), ".gitignore");
    }

    #[test]
    fn shortcuts_are_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("kestrel-test-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("inside")).unwrap();
        let target = dir.join("notes.txt");
        fs::write(&target, b"notes").unwrap();
        let target_text = target.display().to_string();
        let folder = dir.join("inside").display().to_string();

        let outcome = make_shortcuts(&[target_text.clone(), format!(r"{}\missing.txt", dir.display())], &folder).unwrap();
        assert!(!outcome.completed, "a missing target is skipped");
        let made: Vec<String> = outcome
            .results()
            .map(|done| match done {
                Done::Created { path } => path.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(made, [format!(r"{folder}\notes.txt - Shortcut.lnk")]);
        let again = make_shortcuts(std::slice::from_ref(&target_text), &folder).unwrap();
        assert!(matches!(again.results().next(), Some(Done::Created { path }) if path.ends_with("notes.txt - Shortcut (2).lnk")));

        let read = in_apartment({
            let link = Path::new(&made[0]).to_path_buf();
            move || Ok(read_target(&link))
        })
        .unwrap();
        assert_eq!(read.as_deref().map(str::to_lowercase), Some(target_text.to_lowercase()));

        let named = make_shortcut(&target_text, &folder, "My notes").unwrap();
        assert_eq!(named.file_name().unwrap(), "My notes.lnk");
        let suggested = make_shortcut(&format!("\"{target_text}\""), &folder, "").unwrap();
        assert_eq!(suggested.file_name().unwrap(), "notes.lnk");
        assert!(make_shortcut(r"C:\no\such\thing.txt", &folder, "x").is_err());
        assert!(make_shortcut(&target_text, &folder, "bad|name").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
