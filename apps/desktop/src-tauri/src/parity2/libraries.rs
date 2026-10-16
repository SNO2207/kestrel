//! The shell's Libraries: Documents, Music, Pictures, Videos and any the owner made.
//!
//! Each library is a `.library-ms` file in the Libraries folder (`FOLDERID_Libraries`, normally
//! `%APPDATA%\Microsoft\Windows\Libraries`): XML naming the library (often an indirect string
//! such as `@shell32.dll,-34575`), its icon, whether it is pinned, and the folders it includes,
//! each a `knownfolder:{GUID}` or a path, one marked as where new items are saved. Those files are
//! read here directly ([`parse`]); nothing is written to them.
//!
//! Libraries are addresses, as in Explorer's address bar: `shell:Libraries` is the list of them
//! (a page of the front end's) and `shell:Libraries\Documents` one library, which `list_dir`
//! hands to [`list`]: the items of every folder it includes, as one listing
//! (`Engine::list_merged`). Rows carry their real paths, so every command on items works as in
//! any folder; commands given the library's address as a folder (New, Paste, drops on the open
//! folder) use its save folder ([`folder_for`]), as Explorer does.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Window};
use windows_sys::Win32::UI::Shell::{FOLDERID_Libraries, SHLoadIndirectString};

use super::{dev_only, known_folder, parse_guid, wide};
use crate::engine::{Engine, Listing, SortSpec};
use crate::visibility::Visibility;
use crate::watch::Watcher;

/// The list of libraries.
pub const ROOT: &str = "shell:Libraries";
/// Other spellings of [`ROOT`] the address bar takes, compared without case.
const ROOT_ALIASES: &[&str] = &["Libraries", "::{031E4825-7B94-4dc3-B131-E946B44C8DD5}", "shell:::{031E4825-7B94-4dc3-B131-E946B44C8DD5}"];

/// Where the end-to-end tests keep libraries of their own, instead of the owner's.
static DEV_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// One library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Library {
    /// What it is called, as Explorer shows it.
    pub name: String,
    /// Its address: `shell:Libraries\<name>`.
    pub address: String,
    /// The `.library-ms` file.
    pub file: String,
    /// The folders it includes that exist, in its order.
    pub folders: Vec<String>,
    /// Where new items go: the folder marked as the save location, else the first.
    pub save_folder: Option<String>,
    /// Shown in the navigation pane.
    pub pinned: bool,
    /// Which of Windows' own libraries it is, by its folder type, for its icon: `documents`,
    /// `music`, `pictures`, `videos`, or `library` for any other.
    pub kind: &'static str,
    /// Locations it names that are not there now: a folder removed or on a drive that is away.
    pub missing: Vec<String>,
}

/// What a `.library-ms` file says, before names and known folders are resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Description {
    pub name: Option<String>,
    pub pinned: bool,
    pub folder_type: Option<String>,
    pub locations: Vec<Location>,
}

/// One location a library includes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// `knownfolder:{GUID}`, a path, or a `file:` URL.
    pub url: String,
    pub default_save: bool,
}

// ---------- reading the files ----------

/// The text of a `.library-ms` file: UTF-8 as Windows writes them, or UTF-16 with its mark.
pub fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => String::from_utf16_lossy(&rest.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect::<Vec<_>>()),
        [0xFE, 0xFF, rest @ ..] => String::from_utf16_lossy(&rest.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect::<Vec<_>>()),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// XML text with its entities and character references replaced, and CDATA unwrapped.
fn unescape(text: &str) -> String {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix("<![CDATA[").and_then(|rest| rest.strip_suffix("]]>")) {
        return inner.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The contents of every `<tag>…</tag>` in `xml`, in order (attributes on the tag allowed).
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        // `<name>` must not match `<nameOther>`.
        if !after.starts_with(['>', ' ', '\t', '\r', '\n', '/']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            found.push("");
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        found.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    found
}

fn first(xml: &str, tag: &str) -> Option<String> {
    elements(xml, tag).first().map(|text| unescape(text)).filter(|text| !text.is_empty())
}

fn truthy(xml: &str, tag: &str) -> bool {
    first(xml, tag).is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
}

/// Reads what a library description says. Tolerant: anything missing is left out.
pub fn parse(xml: &str) -> Description {
    // The library's own name, not a location's: locations come later in the file.
    let head = xml.split("<searchConnectorDescriptionList").next().unwrap_or(xml);
    let locations = elements(xml, "searchConnectorDescription")
        .into_iter()
        .filter_map(|connector| {
            let location = elements(connector, "simpleLocation").into_iter().next().unwrap_or(connector);
            let url = first(location, "url")?;
            Some(Location { url, default_save: truthy(connector, "isDefaultSaveLocation") })
        })
        .collect();
    Description { name: first(head, "name"), pinned: truthy(head, "isLibraryPinned"), folder_type: first(head, "folderType"), locations }
}

/// An indirect string (`@shell32.dll,-34575`) as Windows resolves it; plain text as it is.
fn resolve_name(name: &str) -> Option<String> {
    if !name.starts_with('@') {
        return Some(name.to_owned());
    }
    let source = wide(name);
    let mut buffer = [0u16; 512];
    // SAFETY: `source` is NUL-terminated, and the buffer's length is passed with it.
    let hr = unsafe { SHLoadIndirectString(source.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32, std::ptr::null()) };
    (hr >= 0).then(|| String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())])).filter(|text| !text.is_empty())
}

/// The folder a location names: a known folder's path, a path, or a `file:` URL's path.
pub fn resolve_url(url: &str, known: impl Fn(&str) -> Option<String>) -> Option<String> {
    let url = url.trim();
    if let Some(id) = url.strip_prefix("knownfolder:") {
        return known(id);
    }
    if let Some(rest) = url.strip_prefix("file:///") {
        return Some(percent_decode(rest).replace('/', "\\"));
    }
    if let Some(rest) = url.strip_prefix("file://") {
        return Some(format!(r"\\{}", percent_decode(rest).replace('/', "\\")));
    }
    let path = url.replace('/', "\\");
    let absolute = path.starts_with(r"\\") || path.as_bytes().get(1) == Some(&b':');
    absolute.then(|| crate::engine::normalize(&path))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A known folder's path, from its id as a library writes it.
fn known_path(id: &str) -> Option<String> {
    known_folder(&parse_guid(id)?)
}

/// Windows' own libraries' folder types.
fn kind_of(folder_type: Option<&str>) -> &'static str {
    match folder_type.map(|kind| kind.trim().trim_matches(['{', '}']).to_ascii_lowercase()).as_deref() {
        Some("7d49d726-3c21-4f05-99aa-fdc2c9474656") => "documents",
        Some("94d6ddcc-4a68-4175-a374-bd584a510b78") => "music",
        Some("b3690e58-e961-423b-b687-386ebfd83239") => "pictures",
        Some("5fa96407-7e77-483c-ac93-691d05850de8") => "videos",
        _ => "library",
    }
}

/// Reads one `.library-ms` file, resolving known folders with `known`.
pub fn read_with(file: &Path, known: impl Fn(&str) -> Option<String>) -> Option<Library> {
    let bytes = std::fs::read(file).ok()?;
    let description = parse(&decode(&bytes));
    let stem = file.file_stem()?.to_string_lossy().into_owned();
    let name = description.name.as_deref().and_then(resolve_name).unwrap_or(stem);
    let mut folders: Vec<String> = Vec::new();
    let mut missing = Vec::new();
    let mut save_folder = None;
    for location in &description.locations {
        match resolve_url(&location.url, &known) {
            Some(folder) if Path::new(&folder).is_dir() => {
                if location.default_save && save_folder.is_none() {
                    save_folder = Some(folder.clone());
                }
                if !folders.iter().any(|known| known.eq_ignore_ascii_case(&folder)) {
                    folders.push(folder);
                }
            }
            Some(folder) => missing.push(folder),
            None => missing.push(location.url.clone()),
        }
    }
    let save_folder = save_folder.or_else(|| folders.first().cloned());
    Some(Library {
        address: address_of(&name),
        name,
        file: file.display().to_string(),
        folders,
        save_folder,
        pinned: description.pinned,
        kind: kind_of(description.folder_type.as_deref()),
        missing,
    })
}

/// Where the libraries are: Windows' Libraries folder, or the tests' own.
fn libraries_folder() -> Option<PathBuf> {
    if let Some(folder) = DEV_FOLDER.lock().clone() {
        return Some(folder);
    }
    known_folder(&FOLDERID_Libraries).map(PathBuf::from)
}

/// Every library in `folder`, Windows' four first in their usual order, then the rest by name.
pub fn read_folder(folder: &Path, known: impl Fn(&str) -> Option<String>) -> Vec<Library> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut libraries: Vec<Library> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("library-ms")))
        .filter_map(|path| read_with(&path, &known))
        .collect();
    let order = |library: &Library| ["documents", "music", "pictures", "videos"].iter().position(|kind| *kind == library.kind).unwrap_or(4);
    libraries.sort_by(|a, b| order(a).cmp(&order(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    libraries
}

/// Every library of the account.
pub fn all() -> Vec<Library> {
    libraries_folder().map(|folder| read_folder(&folder, known_path)).unwrap_or_default()
}

/// A library's address.
pub fn address_of(name: &str) -> String {
    format!(r"{ROOT}\{name}")
}

/// Whether `address` is the list of libraries, under any of its spellings.
pub fn is_root(address: &str) -> bool {
    let address = address.trim().trim_end_matches('\\');
    address.eq_ignore_ascii_case(ROOT) || ROOT_ALIASES.iter().any(|alias| alias.eq_ignore_ascii_case(address))
}

/// The library name in a library's address (`shell:Libraries\Documents` gives `Documents`),
/// judged by spelling alone.
pub fn name_in(address: &str) -> Option<&str> {
    let address = address.trim().trim_end_matches('\\');
    let (head, name) = address.split_once('\\')?;
    (is_root(head) && !name.is_empty() && !name.contains('\\')).then_some(name)
}

/// The library at `address`, if it is one.
pub fn find(address: &str) -> Option<Library> {
    let name = name_in(address)?;
    all().into_iter().find(|library| library.name.eq_ignore_ascii_case(name))
}

/// The folder to use for `path` when a command is given it as a folder to put things in: a
/// library's save folder for a library's address, the path itself otherwise.
pub fn folder_for(path: &str) -> String {
    if name_in(path).is_none() {
        return path.to_owned();
    }
    find(path).and_then(|library| library.save_folder).unwrap_or_else(|| path.to_owned())
}

/// What Properties shows for `path`: a library's own file for a library's address.
pub fn properties_target(path: &str) -> String {
    if name_in(path).is_none() {
        return path.to_owned();
    }
    find(path).map_or_else(|| path.to_owned(), |library| library.file)
}

/// Lists a library for `list_dir`: the items of all its folders as one folder, watched through
/// its save folder when `watch`. `None` for an address that isn't a library's.
pub fn list(
    app: &AppHandle,
    window: &Window,
    engine: &Engine,
    watcher: &Watcher,
    path: &str,
    sort: SortSpec,
    visibility: Visibility,
    watch: bool,
) -> Option<Result<Listing, String>> {
    let name = name_in(path)?;
    let Some(library) = find(path) else {
        return Some(Err(format!("There's no library called {name}.")));
    };
    if watch && let Some(folder) = library.save_folder.clone() {
        // A change in the save folder is a change in the library, which is what the tab shows.
        let (app, address) = (app.clone(), library.address.clone());
        watcher.watch_for(window.label(), &crate::engine::normalize(&folder), move |_| {
            let _ = app.emit("dir-changed", &address);
        });
    }
    if library.folders.is_empty() {
        return Some(Err(format!("The {} library includes no folders that are there now.", library.name)));
    }
    let (listing, skipped) = engine.list_merged(&library.address, &library.folders, sort, visibility);
    if !skipped.is_empty() {
        eprintln!("library {}: left out {}", library.name, skipped.join(", "));
    }
    Some(Ok(listing))
}

// ---------- commands ----------

/// Every library, for the Libraries page and the navigation pane.
#[tauri::command(async)]
pub fn libraries_list() -> Vec<Library> {
    all()
}

/// Development builds only: reads libraries from `folder` instead of the account's own (`None`
/// goes back to them), so the end-to-end tests use libraries they wrote themselves.
#[tauri::command(async)]
pub fn libraries_dev_folder(folder: Option<String>) -> Result<(), String> {
    dev_only()?;
    *DEV_FOLDER.lock() = folder.map(PathBuf::from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use kestrel_index::{IndexBuilder, walk};

    use crate::engine::SortField;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-test-libraries-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A library file as Windows writes one, with the given name element and locations.
    fn library_ms(name: &str, folder_type: &str, locations: &[(&str, bool)]) -> String {
        let connectors: String = locations
            .iter()
            .map(|(url, save)| {
                format!(
                    "    <searchConnectorDescription publisher=\"Microsoft\" product=\"Windows\">\r\n      <description>@shell32.dll,-34577</description>\r\n      {}<simpleLocation>\r\n        <url>{url}</url>\r\n        <serialized>MBAAAEAFCAAAAAAA</serialized>\r\n      </simpleLocation>\r\n    </searchConnectorDescription>\r\n",
                    if *save { "<isDefaultSaveLocation>true</isDefaultSaveLocation>\r\n      " } else { "" }
                )
            })
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<libraryDescription xmlns=\"http://schemas.microsoft.com/windows/2009/library\">\r\n  <name>{name}</name>\r\n  <ownerSID>S-1-5-21-1-2-3-1001</ownerSID>\r\n  <version>6</version>\r\n  <isLibraryPinned>true</isLibraryPinned>\r\n  <iconReference>imageres.dll,-1002</iconReference>\r\n  <templateInfo>\r\n    <folderType>{folder_type}</folderType>\r\n  </templateInfo>\r\n  <propertyStore>\r\n    <property name=\"HasModifiedLocations\" type=\"boolean\"><![CDATA[true]]></property>\r\n  </propertyStore>\r\n  <searchConnectorDescriptionList>\r\n{connectors}  </searchConnectorDescriptionList>\r\n</libraryDescription>"
        )
    }

    const DOCUMENTS_TYPE: &str = "{7d49d726-3c21-4f05-99aa-fdc2c9474656}";
    const DOCUMENTS_ID: &str = "{FDD39AD0-238F-46AF-ADB4-6C85480369C7}";

    #[test]
    fn a_library_description_is_read_tolerantly() {
        let xml = library_ms("Projects &amp; notes", "{00000000-0000-0000-0000-000000000000}", &[(r"C:\A", false), ("knownfolder:{X}", true)]);
        let description = parse(&xml);
        assert_eq!(description.name.as_deref(), Some("Projects & notes"));
        assert!(description.pinned);
        assert_eq!(description.locations, [Location { url: r"C:\A".into(), default_save: false }, Location { url: "knownfolder:{X}".into(), default_save: true }]);
        // A description's own <description> is never taken for the library's name.
        let unnamed = parse("<libraryDescription><searchConnectorDescriptionList><searchConnectorDescription><description>Not it</description><simpleLocation><url>D:\\</url></simpleLocation></searchConnectorDescription></searchConnectorDescriptionList></libraryDescription>");
        assert_eq!(unnamed.name, None);
        assert_eq!(unnamed.locations.len(), 1);
        assert_eq!(parse("not xml at all"), Description::default());
        assert_eq!(unescape("a &lt;b&gt; &#65;&#x42; &bogus; &"), "a <b> AB &bogus; &");
    }

    #[test]
    fn locations_resolve_to_folders() {
        let known = |id: &str| (id == DOCUMENTS_ID).then(|| r"C:\Users\me\Documents".to_owned());
        assert_eq!(resolve_url(&format!("knownfolder:{DOCUMENTS_ID}"), known).as_deref(), Some(r"C:\Users\me\Documents"));
        assert_eq!(resolve_url("knownfolder:{00000000-0000-0000-0000-000000000000}", known), None);
        assert_eq!(resolve_url(r"D:\Media\", known).as_deref(), Some(r"D:\Media"));
        assert_eq!(resolve_url("D:/Media/Film", known).as_deref(), Some(r"D:\Media\Film"));
        assert_eq!(resolve_url("file:///C:/My%20Files", known).as_deref(), Some(r"C:\My Files"));
        assert_eq!(resolve_url(r"\\nas\photos", known).as_deref(), Some(r"\\nas\photos"));
        assert_eq!(resolve_url("file://nas/photos", known).as_deref(), Some(r"\\nas\photos"));
        assert_eq!(resolve_url("relative\\path", known), None);
    }

    #[test]
    fn libraries_are_read_from_their_files() {
        let dir = sandbox("read");
        let (one, two) = (dir.join("one"), dir.join("two"));
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        let gone = dir.join("gone");
        let libs = dir.join("Libraries");
        fs::create_dir_all(&libs).unwrap();
        let text = |path: &Path| path.display().to_string();
        // Windows' Documents library names a known folder; resolving it here gives `one`.
        fs::write(libs.join("Documents.library-ms"), library_ms("@shell32.dll,-34575", DOCUMENTS_TYPE, &[(&format!("knownfolder:{DOCUMENTS_ID}"), true)])).unwrap();
        // An owner's library of two folders, the second the save folder, and one that is gone.
        fs::write(libs.join("Work.library-ms"), library_ms("Work", "", &[(&text(&one), false), (&text(&two), true), (&text(&gone), false)])).unwrap();
        // Saved as UTF-16 with its mark, which Windows also reads.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(library_ms("Wide", "", &[(&text(&one), false)]).encode_utf16().flat_map(u16::to_le_bytes)).collect();
        fs::write(libs.join("Wide.library-ms"), utf16).unwrap();
        fs::write(libs.join("desktop.ini"), "[.ShellClassInfo]").unwrap();
        fs::write(libs.join("Broken.library-ms"), [0xC3, 0x28, 0x00]).unwrap();

        let known = |id: &str| (id == DOCUMENTS_ID).then(|| text(&one));
        let libraries = read_folder(&libs, known);
        let names: Vec<&str> = libraries.iter().map(|library| library.name.as_str()).collect();
        // Windows' own first; the name of an unreadable description falls back to its file's.
        assert_eq!(names.len(), 4, "{names:?}");
        assert_eq!(libraries[0].kind, "documents");
        assert!(!libraries[0].name.starts_with('@'), "the indirect name is resolved: {}", libraries[0].name);
        assert_eq!(libraries[0].folders, [text(&one)]);
        assert_eq!(&names[1..], ["Broken", "Wide", "Work"]);
        let broken = &libraries[1];
        assert!(broken.folders.is_empty() && broken.save_folder.is_none());
        let work = libraries.iter().find(|library| library.name == "Work").unwrap();
        assert_eq!(work.folders, [text(&one), text(&two)]);
        assert_eq!(work.save_folder.as_deref(), Some(text(&two).as_str()));
        assert_eq!(work.missing, [text(&gone)]);
        assert_eq!(work.address, r"shell:Libraries\Work");
        assert!(work.pinned);
        assert_eq!(work.kind, "library");
        assert_eq!(libraries.iter().find(|library| library.name == "Wide").unwrap().folders, [text(&one)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn addresses_name_libraries() {
        assert!(is_root("shell:Libraries") && is_root("Libraries") && is_root(r"shell:libraries\") && is_root("::{031E4825-7B94-4dc3-B131-E946B44C8DD5}"));
        assert_eq!(name_in(r"shell:Libraries\Documents"), Some("Documents"));
        assert_eq!(name_in(r"Libraries\Music\"), Some("Music"));
        assert_eq!(name_in(r"shell:Libraries\Documents\Sub"), None);
        assert_eq!(name_in(r"C:\Libraries\Documents"), None);
        assert_eq!(name_in("shell:Libraries"), None);
        assert_eq!(folder_for(r"C:\Users"), r"C:\Users");
        assert_eq!(properties_target(r"C:\Users"), r"C:\Users");
    }

    /// The library's folders are listed as one, sorted together, folders first, with the same
    /// file in two included folders listed once and a folder outside the index left out.
    #[test]
    fn a_library_lists_its_folders_as_one() {
        let dir = sandbox("merged");
        let (one, two, outside) = (dir.join("one"), dir.join("two"), std::env::temp_dir().join(format!("kestrel-test-libraries-outside-{}", std::process::id())));
        let _ = fs::remove_dir_all(&outside);
        for folder in [&one, &two, &outside] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::create_dir_all(one.join("zeta folder")).unwrap();
        fs::write(one.join("b.txt"), b"bb").unwrap();
        fs::write(two.join("a.txt"), b"a").unwrap();
        fs::write(two.join("c.txt"), b"cccc").unwrap();
        fs::write(outside.join("never.txt"), b"x").unwrap();

        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&dir.display().to_string(), &mut builder).unwrap();
        engine.install(builder.finish());
        // A file arrives after the index was made: listing reconciles it, as a folder's does.
        fs::write(one.join("d.txt"), b"ddd").unwrap();

        let text = |path: &Path| path.display().to_string();
        let folders = [text(&one), text(&two), text(&one), text(&outside)];
        let sort = SortSpec { key: SortField::Name, desc: false };
        let (listing, skipped) = engine.list_merged(r"shell:Libraries\Work", &folders, sort, Visibility::ALL);
        assert_eq!(listing.path, r"shell:Libraries\Work");
        assert_eq!(skipped, [crate::engine::normalize(&text(&outside))]);
        let rows = engine.rows(listing.set, 0, 100).unwrap();
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["zeta folder", "a.txt", "b.txt", "c.txt", "d.txt"]);
        assert_eq!(rows[1].path, text(&two.join("a.txt")));
        assert_eq!(rows[4].path, text(&one.join("d.txt")));
        assert_eq!(listing.total, 5);
        assert_eq!(listing.max_size, 4);
        // Sorting by size keeps them together too.
        let by_size = SortSpec { key: SortField::Size, desc: true };
        engine.sort_set(listing.set, by_size, &Default::default()).unwrap();
        let sized: Vec<String> = engine.rows(listing.set, 0, 100).unwrap().into_iter().map(|row| row.name).collect();
        assert_eq!(sized, ["zeta folder", "c.txt", "d.txt", "b.txt", "a.txt"]);
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }
}
