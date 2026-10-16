//! New > every file type Windows offers, as Explorer finds them: `ShellNew` keys in the
//! registry's file classes.
//!
//! For each extension (`HKCR\.ext`), Explorer looks for `ShellNew` under the subkey named after
//! the extension's current file type (`HKCR\.docx\Word.Document.12\ShellNew`), then directly
//! under the extension (`HKCR\.txt\ShellNew`). What the key holds says how the new file is
//! made:
//!
//! - `NullFile`: empty;
//! - `Data`: these bytes (binary, or a string written as UTF-8);
//! - `FileName`: a copy of a template, an absolute path or a name in the Templates folders
//!   (the user's, everyone's, or `%SystemRoot%\ShellNew`);
//! - `Command` or `Handler`: a program or a shell extension makes it. Kestrel runs neither, so
//!   those types are left out, except shortcuts (`.lnk`), which Kestrel makes itself
//!   (`links.rs`) after asking what they point at.
//!
//! Entries marked `Config\IsOptIn` (such as libraries) only appear in particular folders and
//! are left out. The menu names a type by its `MenuText`, or the name Windows gives the file
//! type ("Microsoft Word Document"); the new file is called by its `ItemName`, or "New" and
//! the type's name, made unique as Explorer does ("New Text Document (2).txt"). If no Text
//! Document entry exists (some Windows 11 installations dropped it with the old Notepad),
//! Kestrel offers one anyway: an empty `.txt`.
//!
//! Reading every file class takes a moment, so the list is read once and kept for a while.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::State;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, KEY_READ, REG_BINARY, REG_EXPAND_SZ, REG_SZ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows_sys::Win32::UI::Shell::{
    ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYDOCNAME, AssocQueryStringW, FOLDERID_CommonTemplates, FOLDERID_Templates,
    SHLoadIndirectString,
};

use super::{known_folder, wide};
use crate::fileops::{Done, Outcome};
use crate::undo::Journal;

/// How long the list read from the registry is kept before it is read again.
const KEEP_FOR: Duration = Duration::from_secs(300);

/// How a new file of some type is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Empty,
    Data(Vec<u8>),
    Template(PathBuf),
    /// Asks for a target, then makes a shortcut to it.
    Shortcut,
}

/// One entry of the New menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NewKind {
    /// The extension with its dot, such as `.docx`, which names the entry.
    pub id: String,
    /// The extension without its dot.
    pub ext: String,
    /// What the menu calls it.
    pub label: String,
    /// The name a new file gets, before its extension and any number.
    pub base: String,
    /// `empty`, `data`, `template` or `shortcut`.
    pub kind: &'static str,
    #[serde(skip)]
    pub source: Source,
}

impl NewKind {
    fn new(ext: &str, label: String, base: String, source: Source) -> Self {
        let kind = match source {
            Source::Empty => "empty",
            Source::Data(_) => "data",
            Source::Template(_) => "template",
            Source::Shortcut => "shortcut",
        };
        let ext = ext.trim_start_matches('.').to_owned();
        Self { id: format!(".{ext}"), ext, label, base, kind, source }
    }
}

/// A registry value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    fn text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            Value::Bytes(_) => None,
        }
    }
}

/// What discovery reads: the registry's file classes, or a stand-in in tests. Keys are paths
/// below `HKEY_CLASSES_ROOT`, such as `.zip\CompressedFolder\ShellNew`.
pub trait Classes {
    /// The names of the extension keys (`.txt`, `.docx`...).
    fn extensions(&self) -> Vec<String>;
    fn exists(&self, key: &str) -> bool;
    /// A value of `key`; the empty name is the key's default value.
    fn value(&self, key: &str, name: &str) -> Option<Value>;
}

/// What discovery needs from Windows besides the registry.
pub struct Names<'a> {
    /// The name Windows gives files with an extension (`.docx`: "Microsoft Word Document").
    pub type_name: &'a dyn Fn(&str) -> Option<String>,
    /// Loads an indirect string such as `@shell32.dll,-30397`.
    pub indirect: &'a dyn Fn(&str) -> Option<String>,
    /// Where a template named without a folder may be.
    pub template_folders: Vec<PathBuf>,
}

/// Replaces `%NAME%` with the environment variable's value, as `REG_EXPAND_SZ` values and
/// shortcuts' stored paths need.
pub fn expand_variables(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// A string as the menu or a file name uses it: an indirect `@dll,-id` string loaded, and the
/// `&` that marks a menu accelerator dropped.
fn display(text: &str, names: &Names) -> Option<String> {
    let text = text.trim();
    let loaded = if text.starts_with('@') { (names.indirect)(text)? } else { text.to_owned() };
    let cleaned = loaded.replace("&&", "\u{0}").replace('&', "").replace('\u{0}', "&");
    let cleaned = cleaned.trim().to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Finds a template named by `FileName`: an absolute path, or a name in a Templates folder.
fn template(name: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    let name = expand_variables(name.trim());
    let given = Path::new(&name);
    if given.is_absolute() {
        return given.is_file().then(|| given.to_path_buf());
    }
    folders.iter().map(|folder| folder.join(given)).find(|path| path.is_file())
}

/// The New menu entry of one extension, if it has one Kestrel can make.
fn entry(classes: &dyn Classes, ext: &str, names: &Names) -> Option<NewKind> {
    let prog_id = classes.value(ext, "").and_then(|value| value.text().map(str::trim).map(str::to_owned));
    let candidates = prog_id
        .filter(|id| !id.is_empty() && !id.contains('\\'))
        .map(|id| format!(r"{ext}\{id}\ShellNew"))
        .into_iter()
        .chain([format!(r"{ext}\ShellNew")]);
    let key = candidates.into_iter().find(|key| classes.exists(key))?;
    if classes.value(&format!(r"{key}\Config"), "IsOptIn").is_some() {
        return None;
    }
    let get = |name: &str| classes.value(&key, name);
    let source = if ext.eq_ignore_ascii_case(".lnk") {
        Source::Shortcut
    } else if get("Command").is_some() || get("Handler").is_some() {
        return None;
    } else if let Some(data) = get("Data") {
        match data {
            Value::Bytes(bytes) => Source::Data(bytes),
            Value::Text(text) => Source::Data(text.into_bytes()),
        }
    } else if let Some(name) = get("FileName") {
        Source::Template(template(name.text()?, &names.template_folders)?)
    } else if get("NullFile").is_some() {
        Source::Empty
    } else {
        return None;
    };
    let type_name = || (names.type_name)(ext).unwrap_or_else(|| format!("{} File", ext.trim_start_matches('.').to_uppercase()));
    let label = get("MenuText").and_then(|text| display(text.text()?, names)).unwrap_or_else(type_name);
    let base = get("ItemName")
        .and_then(|text| display(text.text()?, names))
        .unwrap_or_else(|| format!("New {}", type_name()));
    Some(NewKind::new(ext, label, base, source))
}

/// Every New menu entry the registry offers, shortcuts first and then by name, with a Text
/// Document added when there is none.
pub fn discover(classes: &dyn Classes, names: &Names) -> Vec<NewKind> {
    let mut kinds: Vec<NewKind> = classes
        .extensions()
        .iter()
        .filter(|ext| ext.starts_with('.') && ext.len() > 1)
        .filter_map(|ext| entry(classes, ext, names))
        .collect();
    if !kinds.iter().any(|kind| kind.id.eq_ignore_ascii_case(".txt")) {
        let label = (names.type_name)(".txt").unwrap_or_else(|| "Text Document".to_owned());
        kinds.push(NewKind::new(".txt", label.clone(), format!("New {label}"), Source::Empty));
    }
    kinds.sort_by_key(|kind| (kind.source != Source::Shortcut, kind.label.to_lowercase()));
    kinds
}

// ---------- the real registry ----------

/// `HKEY_CLASSES_ROOT`, read-only.
pub struct Registry;

/// An open registry key, closed when dropped.
struct Key(HKEY);

impl Key {
    fn open(path: &str) -> Option<Key> {
        let wide = wide(path);
        let mut key: HKEY = null_mut();
        // SAFETY: `wide` is NUL-terminated and `key` receives the handle on success.
        let status = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, wide.as_ptr(), 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS).then_some(Key(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

impl Classes for Registry {
    fn extensions(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut buffer = [0u16; 256];
        for index in 0.. {
            let mut len = buffer.len() as u32;
            // SAFETY: `buffer` holds `len` characters, which bounds what the call writes; the
            // other out-pointers are optional and null.
            let status = unsafe {
                RegEnumKeyExW(HKEY_CLASSES_ROOT, index, buffer.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut())
            };
            if status != ERROR_SUCCESS {
                break;
            }
            let name = String::from_utf16_lossy(&buffer[..len as usize]);
            if name.starts_with('.') {
                names.push(name);
            }
        }
        names
    }

    fn exists(&self, key: &str) -> bool {
        Key::open(key).is_some()
    }

    fn value(&self, key: &str, name: &str) -> Option<Value> {
        let key = Key::open(key)?;
        let name = wide(name);
        let (mut kind, mut size) = (0u32, 0u32);
        // SAFETY: asks only for the value's type and size; the data pointer is null.
        let status = unsafe { RegQueryValueExW(key.0, name.as_ptr(), null(), &mut kind, null_mut(), &mut size) };
        if status != ERROR_SUCCESS {
            return None;
        }
        let mut data = vec![0u8; size as usize + 2];
        size = data.len() as u32;
        // SAFETY: `data` holds `size` bytes, which bounds what the call writes.
        let status = unsafe { RegQueryValueExW(key.0, name.as_ptr(), null(), &mut kind, data.as_mut_ptr(), &mut size) };
        if status != ERROR_SUCCESS {
            return None;
        }
        data.truncate(size as usize);
        Some(match kind {
            REG_SZ | REG_EXPAND_SZ => {
                let units: Vec<u16> = data.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
                let end = units.iter().position(|&unit| unit == 0).unwrap_or(units.len());
                let text = String::from_utf16_lossy(&units[..end]);
                Value::Text(if kind == REG_EXPAND_SZ { expand_variables(&text) } else { text })
            }
            REG_BINARY => Value::Bytes(data),
            _ => Value::Bytes(data),
        })
    }
}

/// The name Windows gives files with this extension, such as "Text Document".
fn type_name(ext: &str) -> Option<String> {
    let assoc = wide(ext);
    let mut buffer = [0u16; 256];
    let mut len = buffer.len() as u32;
    // SAFETY: `assoc` is NUL-terminated, and `buffer` holds `len` characters, which bounds
    // what the call writes.
    let result = unsafe {
        AssocQueryStringW(ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYDOCNAME, assoc.as_ptr(), null(), buffer.as_mut_ptr(), &mut len)
    };
    let end = buffer.iter().position(|&c| c == 0)?;
    (result >= 0 && end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

/// Loads an indirect string such as `@%SystemRoot%\system32\zipfldr.dll,-10194`.
fn indirect(text: &str) -> Option<String> {
    let source = wide(&expand_variables(text));
    let mut buffer = [0u16; 512];
    // SAFETY: `source` is NUL-terminated and `buffer` holds the number of characters passed.
    let result = unsafe { SHLoadIndirectString(source.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32, null()) };
    let end = buffer.iter().position(|&c| c == 0)?;
    (result >= 0 && end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

fn template_folders() -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> =
        [known_folder(&FOLDERID_Templates), known_folder(&FOLDERID_CommonTemplates)].into_iter().flatten().map(PathBuf::from).collect();
    if let Some(windows) = std::env::var_os("SystemRoot") {
        folders.push(PathBuf::from(windows).join("ShellNew"));
    }
    folders
}

/// The New menu, read from the registry at most every few minutes.
pub fn kinds() -> Vec<NewKind> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<NewKind>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let mut cached = cache.lock();
    if let Some((at, kinds)) = cached.as_ref()
        && at.elapsed() < KEEP_FOR
    {
        return kinds.clone();
    }
    let names = Names { type_name: &type_name, indirect: &indirect, template_folders: template_folders() };
    let kinds = discover(&Registry, &names);
    *cached = Some((Instant::now(), kinds.clone()));
    kinds
}

// ---------- making a file ----------

/// The first of `base.ext`, `base (2).ext`... that does not exist in `parent`.
pub fn unused_name(parent: &Path, base: &str, ext: &str) -> String {
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    (1..)
        .map(|n| if n == 1 { format!("{base}{dot}") } else { format!("{base} ({n}){dot}") })
        .find(|name| !parent.join(name).exists())
        .expect("an unused name exists")
}

/// Makes a new file of `kind` in `parent` and returns its path. A shortcut is not made here:
/// it needs a target (see `links.rs`).
pub fn create(kind: &NewKind, parent: &str) -> io::Result<PathBuf> {
    let folder = Path::new(parent);
    if !folder.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{parent} is not a folder")));
    }
    let path = folder.join(unused_name(folder, &kind.base, &kind.ext));
    // `create_new`: a file that appeared under the name meanwhile is never overwritten.
    let open = || OpenOptions::new().write(true).create_new(true).open(&path);
    match &kind.source {
        Source::Empty => drop(open()?),
        Source::Data(bytes) => open()?.write_all(bytes)?,
        // Copied byte for byte into a new file, so the copy takes none of the template's
        // attributes: templates under Program Files are often read-only.
        Source::Template(template) => {
            let mut file = open()?;
            io::copy(&mut std::fs::File::open(template)?, &mut file)?;
        }
        Source::Shortcut => return Err(io::Error::other("A shortcut needs something to point at")),
    }
    Ok(path)
}

/// What the undo history records for an item made outside the shell's copy engine.
pub fn created(path: &Path) -> Outcome {
    Outcome { completed: true, done: vec![(0, Done::Created { path: path.display().to_string() })], missing: Vec::new() }
}

/// Explains a failure to make a file the way the rest of the app words it.
pub fn explain(error: &io::Error, parent: &str) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => format!("You don't have permission to create files in {parent}."),
        _ => format!("Couldn't create the file: {error}"),
    }
}

// ---------- commands ----------

/// The New menu's file types.
#[tauri::command(async)]
pub fn new_menu() -> Vec<NewKind> {
    kinds()
}

/// Makes a new file of the type `kind` (an extension, such as `.docx`) in `parent`, records it
/// for undo, and returns the name it got, which the front end then lets the user edit.
#[tauri::command(async)]
pub fn new_file(journal: State<Journal>, parent: String, kind: String) -> Result<String, String> {
    // A library makes new things in its save folder (parity2).
    let parent = crate::parity2::libraries::folder_for(&parent);
    let kinds = kinds();
    let kind = kinds
        .iter()
        .find(|candidate| candidate.id.eq_ignore_ascii_case(&kind))
        .ok_or_else(|| format!("Windows offers no new {kind} files"))?;
    let path = create(kind, &parent).map_err(|error| explain(&error, &parent))?;
    journal.record(&created(&path));
    Ok(crate::fileops::file_name(&path.display().to_string()).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    /// A registry in memory: keys, each with its values.
    #[derive(Default)]
    struct Fake(HashMap<String, HashMap<String, Value>>);

    impl Fake {
        fn key(mut self, key: &str, values: &[(&str, Value)]) -> Self {
            let entry = self.0.entry(key.to_lowercase()).or_default();
            entry.extend(values.iter().map(|(name, value)| (name.to_lowercase(), value.clone())));
            self
        }
    }

    impl Classes for Fake {
        fn extensions(&self) -> Vec<String> {
            let mut names: Vec<String> = self.0.keys().filter(|key| !key.contains('\\')).cloned().collect();
            names.sort();
            names
        }

        fn exists(&self, key: &str) -> bool {
            self.0.contains_key(&key.to_lowercase())
        }

        fn value(&self, key: &str, name: &str) -> Option<Value> {
            self.0.get(&key.to_lowercase())?.get(&name.to_lowercase()).cloned()
        }
    }

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-test-shellnew-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovery_follows_explorers_rules() {
        let templates = scratch("templates");
        fs::write(templates.join("winword.docx"), b"PK-docx").unwrap();
        let office = scratch("office");
        let excel = office.join("excel12.xlsx");
        fs::write(&excel, b"PK-xlsx").unwrap();
        let types: HashMap<&str, &str> = [(".txt", "Text Document"), (".docx", "Microsoft Word Document"), (".zip", "Compressed (zipped) Folder")]
            .into_iter()
            .collect();
        let type_name = |ext: &str| types.get(ext).map(|name| (*name).to_owned());
        let indirect = |text: &str| match text {
            "@zipfldr.dll,-10194" => Some("New Compressed (zipped) Folder".to_owned()),
            "@shell32.dll,-30318" => Some("&Shortcut".to_owned()),
            _ => None,
        };
        let names = Names { type_name: &type_name, indirect: &indirect, template_folders: vec![templates.clone()] };
        let registry = Fake::default()
            .key(".txt", &[("", text("txtfile"))])
            .key(r".txt\ShellNew", &[("NullFile", text(""))])
            .key(".rtf", &[("", text("rtffile"))])
            .key(r".rtf\ShellNew", &[("Data", text(r"{\rtf1}"))])
            // The extension's own file type wins over its bare ShellNew key.
            .key(".zip", &[("", text("CompressedFolder"))])
            .key(r".zip\CompressedFolder\ShellNew", &[("Data", Value::Bytes(vec![0x50, 0x4b, 5, 6])), ("ItemName", text("@zipfldr.dll,-10194"))])
            .key(r".zip\ShellNew", &[("FileName", text(r"C:\elsewhere\zipnew.dat"))])
            // A template found in the Templates folders, or given with its full path.
            .key(".docx", &[("", text("Word.Document.12"))])
            .key(r".docx\Word.Document.12\ShellNew", &[("FileName", text("winword.docx"))])
            .key(".xlsx", &[("", text("Excel.Sheet.12"))])
            .key(r".xlsx\Excel.Sheet.12\ShellNew", &[("FileName", text(excel.to_str().unwrap()))])
            // A ShellNew under a file type that is not the extension's own is not offered.
            .key(".bar", &[("", text("Bar.Current"))])
            .key(r".bar\Bar.Old\ShellNew", &[("NullFile", text(""))])
            // Shortcuts are Kestrel's own; other handlers and commands are left out.
            .key(".lnk", &[("", text("lnkfile"))])
            .key(r".lnk\ShellNew", &[("Handler", text("{ceefea1b}")), ("NullFile", text("")), ("MenuText", text("@shell32.dll,-30318"))])
            .key(".mdb", &[])
            .key(r".mdb\ShellNew", &[("Command", text("msaccess.exe /NEWDB 1"))])
            .key(".library-ms", &[])
            .key(r".library-ms\ShellNew", &[("NullFile", text(""))])
            .key(r".library-ms\ShellNew\Config", &[("IsOptIn", text(""))])
            // A template that can't be found, and a key that says nothing, offer nothing.
            .key(".foo", &[])
            .key(r".foo\ShellNew", &[("FileName", text("missing.foo"))])
            .key(".empty", &[])
            .key(r".empty\ShellNew", &[])
            .key("txtfile", &[]);

        let kinds = discover(&registry, &names);
        let summary: Vec<(&str, &str, &str, &str)> =
            kinds.iter().map(|kind| (kind.id.as_str(), kind.label.as_str(), kind.base.as_str(), kind.kind)).collect();
        assert_eq!(
            summary,
            [
                (".lnk", "Shortcut", "New LNK File", "shortcut"),
                (".zip", "Compressed (zipped) Folder", "New Compressed (zipped) Folder", "data"),
                (".docx", "Microsoft Word Document", "New Microsoft Word Document", "template"),
                (".rtf", "RTF File", "New RTF File", "data"),
                (".txt", "Text Document", "New Text Document", "empty"),
                (".xlsx", "XLSX File", "New XLSX File", "template"),
            ]
        );
        let by_id = |id: &str| kinds.iter().find(|kind| kind.id == id).unwrap().source.clone();
        assert_eq!(by_id(".zip"), Source::Data(vec![0x50, 0x4b, 5, 6]));
        assert_eq!(by_id(".rtf"), Source::Data(br"{\rtf1}".to_vec()));
        assert_eq!(by_id(".docx"), Source::Template(templates.join("winword.docx")));
        assert_eq!(by_id(".xlsx"), Source::Template(excel));

        // Without a Text Document entry, Kestrel offers one of its own.
        let bare = Fake::default().key(".lnk", &[]).key(r".lnk\ShellNew", &[("NullFile", text(""))]);
        let kinds = discover(&bare, &names);
        assert_eq!(kinds.iter().map(|kind| kind.id.as_str()).collect::<Vec<_>>(), [".lnk", ".txt"]);
        assert_eq!((kinds[1].base.as_str(), &kinds[1].source), ("New Text Document", &Source::Empty));
        fs::remove_dir_all(&templates).unwrap();
        fs::remove_dir_all(&office).unwrap();
    }

    #[test]
    fn environment_variables_are_expanded() {
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand_variables(r"%SystemRoot%\ShellNew"), format!(r"{root}\ShellNew"));
        assert_eq!(expand_variables("100% sure"), "100% sure");
        assert_eq!(expand_variables("%NO_SUCH_VARIABLE_KESTREL%"), "%NO_SUCH_VARIABLE_KESTREL%");
    }

    #[test]
    fn the_real_registry_offers_shortcuts_and_text_documents() {
        let kinds = kinds();
        assert!(kinds.iter().any(|kind| kind.id == ".lnk" && kind.source == Source::Shortcut), "{kinds:?}");
        assert!(kinds.iter().any(|kind| kind.id == ".txt"), "{kinds:?}");
        for kind in &kinds {
            assert!(!kind.label.is_empty() && !kind.base.is_empty(), "{kind:?}");
            if let Source::Template(path) = &kind.source {
                assert!(path.is_file(), "{kind:?}");
            }
        }
        assert_eq!(kinds[0].source, Source::Shortcut, "shortcuts come first");
    }

    #[test]
    fn new_files_get_unused_names_and_their_contents() {
        let dir = scratch("create");
        let parent = dir.to_str().unwrap();
        let empty = NewKind::new(".txt", "Text Document".into(), "New Text Document".into(), Source::Empty);
        let first = create(&empty, parent).unwrap();
        let second = create(&empty, parent).unwrap();
        assert_eq!(first.file_name().unwrap(), "New Text Document.txt");
        assert_eq!(second.file_name().unwrap(), "New Text Document (2).txt");
        assert_eq!(fs::metadata(&first).unwrap().len(), 0);

        let data = NewKind::new(".rtf", "Rich Text".into(), "New Rich Text".into(), Source::Data(br"{\rtf1}".to_vec()));
        assert_eq!(fs::read(create(&data, parent).unwrap()).unwrap(), br"{\rtf1}");

        // A read-only template makes a writable copy.
        let template = dir.join("template.bin");
        fs::write(&template, b"template bytes").unwrap();
        let mut permissions = fs::metadata(&template).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&template, permissions).unwrap();
        let copied = NewKind::new(".bin", "Binary".into(), "New Binary".into(), Source::Template(template.clone()));
        let made = create(&copied, parent).unwrap();
        assert_eq!(fs::read(&made).unwrap(), b"template bytes");
        assert!(!fs::metadata(&made).unwrap().permissions().readonly());

        let shortcut = NewKind::new(".lnk", "Shortcut".into(), "New shortcut".into(), Source::Shortcut);
        assert!(create(&shortcut, parent).is_err());
        assert!(create(&empty, &format!(r"{parent}\missing")).is_err());
        assert_eq!(created(&first).done, vec![(0, Done::Created { path: first.display().to_string() })]);

        let mut permissions = fs::metadata(&template).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&template, permissions).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
