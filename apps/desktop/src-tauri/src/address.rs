//! The address bar: what a typed address means, and the folders to suggest while it is typed.
//!
//! An address may be written any way Explorer accepts: with forward slashes, in quotes, with
//! environment variables (`%USERPROFILE%\Documents`, `%APPDATA%`), `~` for the user's folder,
//! a shell name (`shell:downloads`, `shell:startup`, `shell:sendto`: every known folder by its
//! canonical name, and anything else the shell's own parser knows), a long path
//! (`\\?\C:\…`, `\\?\UNC\server\share`), a share (`\\server\share`), or relative to the folder
//! the tab shows (`..`, `sub\folder`). The addresses of special locations (This PC, the
//! Recycle Bin...) are the front end's (`lib/locations.ts`) and never reach here.
//!
//! Suggestions are the folders in the folder typed so far: those whose names start with what
//! follows the last separator first, then those that merely contain it, each group in name
//! order. They come from the index, so typing never waits on the disk; folders the index does
//! not cover (shares) are listed from disk with a time limit, and `\\server\` lists the
//! server's shares.

use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use kestrel_index::{fold, natural_cmp};
use serde::Serialize;
use tauri::State;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::{
    IKnownFolderManager, KNOWNFOLDER_DEFINITION, KnownFolderManager, SHGetPathFromIDListW,
    SHParseDisplayName,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::core::{GUID, HSTRING, PWSTR};
use windows_sys::Win32::NetworkManagement::NetManagement::{MAX_PREFERRED_LENGTH, NetApiBufferFree};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, NetShareEnum, SHARE_INFO_1, STYPE_DISKTREE,
    STYPE_MASK,
};

use crate::engine::{Engine, normalize};
use crate::foldertree;
use crate::visibility::Visibility;

/// How long a question to the disk may take before the answer is given up on (a share whose
/// server is away takes half a minute to say so).
const DISK_WAIT: Duration = Duration::from_secs(3);
/// Suggestions given at most.
const MAX_SUGGESTIONS: usize = 50;

/// What a typed address leads to.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolved {
    Folder { path: String },
    /// A file: Explorer opens its folder with the file selected.
    File { path: String, folder: String, name: String },
    /// Nothing there, or no answer in time: the folder is opened anyway, so the tab says why.
    Unknown { path: String },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Suggestion {
    /// The address it completes to.
    pub path: String,
    /// What is shown: a folder's name, or the shell name.
    pub name: String,
    /// More about it: where a shell name leads.
    pub hint: Option<String>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Suggestions {
    /// The folder whose folders are suggested, as expanded.
    pub parent: Option<String>,
    pub items: Vec<Suggestion>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Runs `work` on a thread with COM initialised, as the shell's parser and the known folder
/// manager expect, and waits for it.
fn with_com<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    std::thread::spawn(move || {
        // SAFETY: COM is initialised for this thread, and uninitialised after `work`, which
        // releases everything it obtains.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
            let result = work();
            CoUninitialize();
            Some(result)
        }
    })
    .join()
    .ok()
    .flatten()
}

/// Runs a question to the disk on a thread of its own, giving up after `DISK_WAIT`.
fn with_time_limit<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (sender, answer) = mpsc::channel();
    std::thread::Builder::new()
        .name("kestrel-address".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    answer.recv_timeout(DISK_WAIT).ok()
}

/// Copies and frees a string the shell allocated; `None` for a null one.
///
/// # Safety
/// `text` must be null or allocated with `CoTaskMemAlloc` and freed by nothing else.
unsafe fn take(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        let copy = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        copy
    }
}

/// A known folder: its canonical name, which `shell:` takes, and its id.
#[derive(Clone)]
struct KnownFolder {
    name: String,
    id: GUID,
}

/// Every known folder Windows has registered, by canonical name. Read once.
fn known_folders() -> &'static [KnownFolder] {
    static FOLDERS: OnceLock<Vec<KnownFolder>> = OnceLock::new();
    FOLDERS.get_or_init(|| {
        with_com(|| {
            // SAFETY: COM is initialised by `with_com`; the id array and every string of each
            // definition are freed with CoTaskMemFree as documented.
            unsafe {
                let created: windows::core::Result<IKnownFolderManager> = CoCreateInstance(&KnownFolderManager, None, CLSCTX_ALL);
                let Ok(manager) = created else { return Vec::new() };
                let mut ids: *mut GUID = null_mut();
                let mut count = 0u32;
                if manager.GetFolderIds(&mut ids, &mut count).is_err() || ids.is_null() {
                    return Vec::new();
                }
                let mut found = Vec::new();
                for &id in std::slice::from_raw_parts(ids, count as usize) {
                    let Ok(folder) = manager.GetFolder(&id) else { continue };
                    let mut definition = KNOWNFOLDER_DEFINITION::default();
                    if folder.GetFolderDefinition(&mut definition).is_err() {
                        continue;
                    }
                    let name = take(definition.pszName);
                    for text in [
                        definition.pszDescription,
                        definition.pszRelativePath,
                        definition.pszParsingName,
                        definition.pszTooltip,
                        definition.pszLocalizedName,
                        definition.pszIcon,
                        definition.pszSecurity,
                    ] {
                        let _ = take(text);
                    }
                    if let Some(name) = name.filter(|name| !name.is_empty()) {
                        found.push(KnownFolder { name, id });
                    }
                }
                CoTaskMemFree(Some(ids as *const _));
                found.sort_by_key(|folder| folder.name.to_lowercase());
                found
            }
        })
        .unwrap_or_default()
    })
}

/// The folder a known folder is on disk, if it is one (some, such as Control Panel, are not).
fn known_folder_path(id: &GUID) -> Option<String> {
    let id = windows_sys::core::GUID { data1: id.data1, data2: id.data2, data3: id.data3, data4: id.data4 };
    let mut raw = null_mut();
    // SAFETY: `raw` receives a CoTaskMem string on success and is freed in every case.
    unsafe {
        let hr = windows_sys::Win32::UI::Shell::SHGetKnownFolderPath(&id, 0, null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(raw, len))
        });
        windows_sys::Win32::System::Com::CoTaskMemFree(raw.cast());
        path
    }
}

/// Friendlier spellings Explorer users type for folders whose canonical names are older.
const ALIASES: &[(&str, &str)] =
    &[("documents", "Personal"), ("pictures", "My Pictures"), ("music", "My Music"), ("videos", "My Video"), ("home", "Profile")];

/// The folder a shell name (what follows `shell:`) stands for.
fn shell_folder(name: &str) -> Result<String, String> {
    let wanted = ALIASES.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(name)).map_or(name, |(_, canonical)| canonical);
    if let Some(folder) = known_folders().iter().find(|folder| folder.name.eq_ignore_ascii_case(wanted)) {
        return known_folder_path(&folder.id).ok_or_else(|| format!("shell:{name} isn't a folder on disk."));
    }
    // Anything else the shell's own parser knows.
    let typed = format!("shell:{name}");
    let parsed = with_com(move || {
        // SAFETY: COM is initialised by `with_com`; the item list is freed after use.
        unsafe {
            let mut list: *mut ITEMIDLIST = null_mut();
            SHParseDisplayName(&HSTRING::from(typed.as_str()), None, &mut list, 0, None).ok()?;
            let mut buffer = [0u16; 260];
            let found = SHGetPathFromIDListW(list, &mut buffer).as_bool();
            CoTaskMemFree(Some(list as *const _));
            found.then(|| {
                let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
                String::from_utf16_lossy(&buffer[..len])
            })
        }
    })
    .flatten();
    parsed.filter(|path| !path.is_empty()).ok_or_else(|| format!("Windows doesn't know shell:{name} as a folder."))
}

/// Replaces each `%NAME%` that names an environment variable with its value.
fn expand_variables(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 && !after[..end].contains(['\\', '/']) => match lookup(&after[..end]) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Removes `.` and `..` from a path that starts with a drive or share root, and doubled
/// separators after the root.
fn tidy(path: &str) -> String {
    let (root, rest) = if let Some(share) = path.strip_prefix(r"\\") {
        // `\\server\share` is the root of a share.
        let mut parts = share.splitn(3, '\\');
        let server = parts.next().unwrap_or_default();
        match parts.next() {
            Some(share) => (format!(r"\\{server}\{share}"), parts.next().unwrap_or_default().to_owned()),
            None => return format!(r"\\{server}"),
        }
    } else if path.len() >= 2 && path.as_bytes()[1] == b':' {
        (path[..2].to_owned(), path[2..].to_owned())
    } else {
        return path.to_owned();
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let trailing = rest.ends_with('\\') && !parts.is_empty();
    let mut out = if root.starts_with(r"\\") { root } else { format!(r"{root}\") };
    if root_is_share(&out) && !parts.is_empty() {
        out.push('\\');
    }
    out.push_str(&parts.join("\\"));
    if trailing {
        out.push('\\');
    }
    out
}

fn root_is_share(text: &str) -> bool {
    text.starts_with(r"\\") && !text.ends_with('\\')
}

/// Whether the text starts with `shell:`, in any case.
fn starts_with_shell(text: &str) -> bool {
    text.get(..6).is_some_and(|start| start.eq_ignore_ascii_case("shell:"))
}

/// Whether a path names its own root: a drive (`C:`) or a share (`\\server\share`).
fn is_absolute(path: &str) -> bool {
    path.starts_with(r"\\") || (path.len() >= 2 && path.as_bytes()[0].is_ascii_alphabetic() && path.as_bytes()[1] == b':')
}

/// Turns what was typed into a full path: quotes, variables, `~`, `shell:` names, long-path
/// prefixes, forward slashes, and a path relative to `base` (the folder the tab shows).
/// Keeps a trailing separator, which says the folder's contents are wanted.
pub fn expand(text: &str, base: Option<&str>) -> Result<String, String> {
    let mut text = text.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text = text[1..text.len() - 1].trim();
    }
    let text = expand_variables(text, |name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    let mut path = text.replace('/', "\\");
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\").or_else(|| path.strip_prefix(r"\\.\UNC\")) {
        path = format!(r"\\{rest}");
    } else if let Some(rest) = path.strip_prefix(r"\\?\").or_else(|| path.strip_prefix(r"\\.\")) {
        path = rest.to_owned();
    }
    if let Some(rest) = path.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with('\\')) {
        let home = std::env::var("USERPROFILE").ok().filter(|home| !home.is_empty()).ok_or("Your user folder isn't known.")?;
        path = format!("{}{rest}", home.trim_end_matches('\\'));
    }
    if path.len() > 6 && starts_with_shell(&path) {
        let (name, rest) = path[6..].split_once('\\').map_or((&path[6..], ""), |(name, rest)| (name, rest));
        let folder = shell_folder(name.trim())?;
        path = if rest.is_empty() { folder } else { format!(r"{}\{rest}", folder.trim_end_matches('\\')) };
    }
    if !is_absolute(&path)
        && let Some(base) = base.filter(|base| is_absolute(base))
    {
        path = if let Some(rooted) = path.strip_prefix('\\') {
            // `\Windows` is on the drive the tab is on.
            format!(r"{}\{rooted}", &base[..2])
        } else {
            format!(r"{}\{path}", base.trim_end_matches('\\'))
        };
    }
    if !is_absolute(&path) {
        return Ok(path);
    }
    Ok(tidy(&path))
}

/// What is at `path`, asked of the disk with a time limit and without opening anything.
fn attributes(path: &str) -> Option<u32> {
    let target = wide(path);
    with_time_limit(move || {
        // SAFETY: the path is NUL-terminated.
        unsafe { GetFileAttributesW(target.as_ptr()) }
    })
}

/// What a typed address leads to.
pub fn resolve(text: &str, base: Option<&str>) -> Result<Resolved, String> {
    let expanded = expand(text, base)?;
    if !is_absolute(&expanded) {
        return Err(format!("Windows can't find \"{}\". Check the spelling and try again.", text.trim()));
    }
    let path = normalize(&expanded);
    Ok(match attributes(&path) {
        Some(found) if found != INVALID_FILE_ATTRIBUTES && found & FILE_ATTRIBUTE_DIRECTORY == 0 => {
            let folder = crate::fileops::parent(&path);
            let name = crate::fileops::file_name(&path).to_owned();
            Resolved::File { path, folder, name }
        }
        Some(found) if found != INVALID_FILE_ATTRIBUTES => Resolved::Folder { path },
        _ => Resolved::Unknown { path },
    })
}

/// Orders folder names against what was typed of one: names starting with it first, then
/// names containing it, each group in natural name order. Without typing, all of them.
pub fn rank(names: Vec<String>, typed: &str) -> Vec<String> {
    let typed = fold(typed);
    let mut ranked: Vec<(u8, String, String)> = names
        .into_iter()
        .filter_map(|name| {
            let folded = fold(&name);
            let group = if folded.starts_with(&typed) {
                0
            } else if folded.contains(&typed) {
                1
            } else {
                return None;
            };
            Some((group, folded, name))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| natural_cmp(a.1.as_bytes(), b.1.as_bytes())));
    ranked.into_iter().map(|(_, _, name)| name).collect()
}

/// The disk shares of a server, `\\server`, with a time limit. Hidden shares (`C$`) only when
/// hidden items are shown.
pub(crate) fn shares(server: &str, show_hidden: bool) -> Vec<String> {
    let server = server.to_owned();
    with_time_limit(move || {
        let name = wide(&server);
        let mut buffer: *mut u8 = null_mut();
        let (mut read, mut total) = (0u32, 0u32);
        // SAFETY: the buffer NetShareEnum allocates is read as the documented array and freed.
        unsafe {
            if NetShareEnum(name.as_ptr(), 1, &mut buffer, MAX_PREFERRED_LENGTH, &mut read, &mut total, null_mut()) != 0
                || buffer.is_null()
            {
                return Vec::new();
            }
            let infos = std::slice::from_raw_parts(buffer as *const SHARE_INFO_1, read as usize);
            let found = infos
                .iter()
                .filter(|info| info.shi1_type & STYPE_MASK == STYPE_DISKTREE)
                .map(|info| {
                    let text = info.shi1_netname;
                    let len = (0..).take_while(|&i| *text.add(i) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
                })
                .filter(|share| show_hidden || !share.ends_with('$'))
                .collect();
            NetApiBufferFree(buffer.cast());
            found
        }
    })
    .unwrap_or_default()
}

/// The shell names that start with, then contain, what was typed after `shell:`. Only those
/// that are folders on disk: virtual ones (Control Panel) and libraries can't be listed.
fn shell_suggestions(typed: &str) -> Vec<Suggestion> {
    let by_name: HashMap<String, &KnownFolder> = known_folders().iter().map(|folder| (folder.name.clone(), folder)).collect();
    rank(by_name.keys().cloned().collect(), typed)
        .into_iter()
        .filter_map(|name| {
            let path = by_name.get(&name).and_then(|folder| known_folder_path(&folder.id))?;
            let library = path.to_ascii_lowercase().ends_with(".library-ms");
            (!library).then(|| Suggestion { path: format!("shell:{name}"), name: format!("shell:{name}"), hint: Some(path) })
        })
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// The suggestions for what has been typed so far.
pub fn suggest(engine: &Engine, text: &str, base: Option<&str>, visibility: Visibility) -> Suggestions {
    let trimmed = text.trim_start().trim_start_matches('"');
    if starts_with_shell(trimmed) && !trimmed.contains(['\\', '/']) {
        return Suggestions { parent: None, items: shell_suggestions(&trimmed[6..]) };
    }
    let Ok(expanded) = expand(trimmed, base) else { return Suggestions::default() };
    // A server alone lists its shares.
    if let Some(server) = expanded.strip_prefix(r"\\").map(|rest| rest.trim_end_matches('\\'))
        && !server.is_empty()
        && !server.contains('\\')
    {
        if !expanded.ends_with('\\') {
            return Suggestions::default();
        }
        let parent = format!(r"\\{server}");
        let items = shares(&parent, visibility.hidden)
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|share| Suggestion { path: format!(r"{parent}\{share}"), name: share, hint: None })
            .collect();
        return Suggestions { parent: Some(parent), items };
    }
    let (parent, typed) = match expanded.rfind('\\') {
        _ if expanded.len() == 2 && expanded.ends_with(':') => (format!(r"{expanded}\"), String::new()),
        Some(at) => (expanded[..=at].to_owned(), expanded[at + 1..].to_owned()),
        None => return Suggestions::default(),
    };
    if !is_absolute(&parent) {
        return Suggestions::default();
    }
    let folder = normalize(&parent);
    let listing = foldertree::children(engine, std::slice::from_ref(&folder), visibility);
    let names: Vec<String> = listing.into_iter().flat_map(|listing| listing.folders).map(|folder| folder.name).collect();
    let items = rank(names, &typed)
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|name| Suggestion { path: format!(r"{}\{name}", folder.trim_end_matches('\\')), name, hint: None })
        .collect();
    Suggestions { parent: Some(folder), items }
}

#[tauri::command(async)]
pub fn address_resolve(text: String, base: Option<String>) -> Result<Resolved, String> {
    resolve(&text, base.as_deref())
}

#[tauri::command(async)]
pub fn address_suggest(
    engine: State<Arc<Engine>>,
    text: String,
    base: Option<String>,
    show_hidden: bool,
    show_protected: Option<bool>,
) -> Suggestions {
    let visibility = Visibility::from_request(Some(show_hidden)).with_protected(show_protected);
    suggest(&engine, &text, base.as_deref(), visibility)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hidden folders left out, as with "Show hidden items" off.
    const VISIBLE: Visibility = Visibility { hidden: false, ..Visibility::ALL };
    use kestrel_index::{IndexBuilder, walk};
    use std::fs;

    #[test]
    fn variables_are_expanded_and_unknown_ones_left_alone() {
        let lookup = |name: &str| match name.to_ascii_uppercase().as_str() {
            "USERPROFILE" => Some(r"C:\Users\me".to_owned()),
            "APPDATA" => Some(r"C:\Users\me\AppData\Roaming".to_owned()),
            _ => None,
        };
        assert_eq!(expand_variables(r"%USERPROFILE%\Documents", lookup), r"C:\Users\me\Documents");
        assert_eq!(expand_variables(r"%appdata%\Code", lookup), r"C:\Users\me\AppData\Roaming\Code");
        assert_eq!(expand_variables(r"%NOPE%\x", lookup), r"%NOPE%\x");
        assert_eq!(expand_variables("100% sure", lookup), "100% sure");
        assert_eq!(expand_variables("%%", lookup), "%%");
    }

    #[test]
    fn addresses_are_written_any_way_explorer_takes_them() {
        let home = std::env::var("USERPROFILE").unwrap();
        let appdata = std::env::var("APPDATA").unwrap();
        assert_eq!(expand("C:/Windows/System32", None).unwrap(), r"C:\Windows\System32");
        assert_eq!(expand(r#""C:\Program Files""#, None).unwrap(), r"C:\Program Files");
        assert_eq!(expand(r"\\?\C:\Windows", None).unwrap(), r"C:\Windows");
        assert_eq!(expand(r"\\?\UNC\server\share\folder", None).unwrap(), r"\\server\share\folder");
        assert_eq!(expand(r"\\server\share", None).unwrap(), r"\\server\share");
        assert_eq!(expand("%USERPROFILE%", None).unwrap().to_lowercase(), home.to_lowercase());
        assert_eq!(expand(r"%AppData%\Microsoft", None).unwrap().to_lowercase(), format!(r"{appdata}\Microsoft").to_lowercase());
        assert_eq!(expand("~", None).unwrap().to_lowercase(), home.to_lowercase());
        assert_eq!(expand(r"~/Documents", None).unwrap().to_lowercase(), format!(r"{home}\Documents").to_lowercase());
        assert_eq!(expand(r"C:\Windows\System32\..\Fonts\.\", None).unwrap(), r"C:\Windows\Fonts\");
        assert_eq!(expand("c:", None).unwrap(), r"c:\");
        assert_eq!(expand(r"..\Fonts", Some(r"C:\Windows\System32")).unwrap(), r"C:\Windows\Fonts");
        assert_eq!(expand("Temp", Some(r"C:\Windows")).unwrap(), r"C:\Windows\Temp");
        assert_eq!(expand(r"\Users", Some(r"D:\Somewhere")).unwrap(), r"D:\Users");
        assert_eq!(expand(r"..\..\..", Some(r"C:\a")).unwrap(), r"C:\", "never above the root");
    }

    #[test]
    fn shell_names_lead_to_known_folders() {
        let downloads = expand("shell:downloads", None).unwrap();
        assert!(downloads.to_lowercase().ends_with("downloads"), "{downloads}");
        let appdata = std::env::var("APPDATA").unwrap();
        assert_eq!(expand("shell:AppData", None).unwrap().to_lowercase(), appdata.to_lowercase());
        let startup = expand(r"shell:startup", None).unwrap();
        assert!(startup.to_lowercase().ends_with(r"start menu\programs\startup"), "{startup}");
        let sendto = expand("shell:sendto", None).unwrap();
        assert!(sendto.to_lowercase().ends_with("sendto"), "{sendto}");
        let recent = expand(r"shell:recent\..", None).unwrap();
        assert!(recent.to_lowercase().ends_with(r"microsoft\windows"), "{recent}");
        let desktop = expand("shell:desktop", None).unwrap();
        assert!(desktop.to_lowercase().ends_with("desktop"), "{desktop}");
        let documents = expand("shell:documents", None).unwrap();
        assert!(!documents.is_empty() && !documents.starts_with("shell:"), "{documents}");
        assert!(expand("shell:NoSuchFolderAnywhere", None).is_err());
        assert!(expand("shell:ControlPanelFolder", None).is_err(), "not a folder on disk");
    }

    #[test]
    fn completion_ranks_prefixes_before_substrings() {
        let names = ["Programs", "Program Files (x86)", "My programs", "Program Files", "Pictures", "data2", "data10"]
            .map(String::from)
            .to_vec();
        assert_eq!(rank(names.clone(), "prog"), ["Program Files", "Program Files (x86)", "Programs", "My programs"]);
        assert_eq!(rank(names.clone(), "DATA"), ["data2", "data10"], "natural order, any case");
        assert_eq!(rank(names, "").len(), 7);
    }

    #[test]
    fn typed_paths_resolve_to_folders_and_files() {
        let root = std::env::temp_dir().join(format!("kestrel-address-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Reports")).unwrap();
        fs::write(root.join("Reports/summary.txt"), b"x").unwrap();
        let base = root.to_str().unwrap();
        assert_eq!(resolve(base, None).unwrap(), Resolved::Folder { path: base.to_owned() });
        let file = format!(r"{base}\Reports\summary.txt");
        assert_eq!(
            resolve(&format!("\"{}\"", file.replace('\\', "/")), None).unwrap(),
            Resolved::File { path: file.clone(), folder: format!(r"{base}\Reports"), name: "summary.txt".into() }
        );
        assert_eq!(resolve(r"Reports\summary.txt", Some(base)).unwrap(), Resolved::File {
            path: file,
            folder: format!(r"{base}\Reports"),
            name: "summary.txt".into()
        });
        assert!(matches!(resolve(&format!(r"{base}\missing"), None).unwrap(), Resolved::Unknown { .. }));
        assert!(resolve("not a path", None).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folders_are_suggested_from_the_index() {
        let root = std::env::temp_dir().join(format!("kestrel-suggest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["Projects", "Pictures", "old projects", "notes"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("project-plan.txt"), b"a file, never suggested").unwrap();
        let path = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&path, &mut builder).unwrap();
        engine.install(builder.finish());

        let found = suggest(&engine, &format!(r"{path}\pro"), None, VISIBLE);
        assert_eq!(found.parent.as_deref(), Some(path.as_str()));
        let names: Vec<&str> = found.items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["Projects", "old projects"]);
        assert_eq!(found.items[0].path, format!(r"{path}\Projects"));
        let all = suggest(&engine, &format!("{}/", path.replace('\\', "/")), None, VISIBLE);
        assert_eq!(all.items.len(), 4);
        // Relative to the tab's folder.
        for typed in ["pic", r".\pic"] {
            let relative = suggest(&engine, typed, Some(&path), VISIBLE);
            assert_eq!(relative.items.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["Pictures"], "{typed}");
        }
        assert!(suggest(&engine, "pic", None, VISIBLE).items.is_empty(), "nothing to be relative to");
        let shells = suggest(&engine, "shell:down", None, VISIBLE);
        assert_eq!(shells.items.first().map(|item| item.name.as_str()), Some("shell:Downloads"));
        fs::remove_dir_all(&root).unwrap();
    }
}
