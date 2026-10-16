//! Downloads triage: what arrived in the Downloads folder since the owner last looked, grouped
//! by kind (installers, archives, documents, images, videos, other), each with the actions that
//! suit it: open, extract (archives, through the archive feature's own dialog), move to a
//! suggested folder, and recycle. Moves and recycling go through `fileops` into the undo
//! journal; nothing happens without a click.
//!
//! Suggestions come from where the owner moved similar files before (`kestrel_db::triage`:
//! by extension, by the first word of the name, by kind), then the known folder for the kind
//! (Documents, Pictures, Videos, Music), then the projects the owner visits most.
//!
//! The folder is read without opening any file (its directory entries give names, sizes,
//! attributes and dates), and watched (`Watcher`, as owner "triage"): `triage-folder` says it
//! changed, for the page, and `triage-changed` carries the count of new downloads, for the
//! sidebar's badge. Marking the page seen sends only the count, so a page that marks itself seen
//! whenever it reloads never reloads itself again.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::triage::{self as store, Learned};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, Window};

use crate::fileops::{self, Done, Options, Request, Transfer};
use crate::projects::Projects;
use crate::search::places;
use crate::undo::Journal;
use crate::watch::Watcher;

const DAY_MS: i64 = 86_400_000;
/// The first time, "new" means what came in the last week.
const FIRST_LOOK: i64 = 7 * DAY_MS;
/// Downloads still being written by a browser or download manager.
const PARTIAL: &[&str] = &["crdownload", "part", "partial", "download", "opdownload", "!ut", "aria2"];
const HIDDEN: u32 = 0x2;
const SYSTEM: u32 = 0x4;
/// Suggestions offered per item, besides "Choose a folder…".
const MAX_SUGGESTIONS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Installers,
    Archives,
    Documents,
    Images,
    Videos,
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Installers => "installers",
            Kind::Archives => "archives",
            Kind::Documents => "documents",
            Kind::Images => "images",
            Kind::Videos => "videos",
            Kind::Other => "other",
        }
    }
}

/// A download's extension, lower case; empty for a folder or a name without one.
pub fn extension(name: &str, dir: bool) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !dir && !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn kind_of(name: &str, dir: bool) -> Kind {
    if dir {
        return Kind::Other;
    }
    let lower = name.to_ascii_lowercase();
    // Compressed tars name two extensions.
    if [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"].iter().any(|ext| lower.ends_with(ext)) {
        return Kind::Archives;
    }
    match extension(name, false).as_str() {
        "exe" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "iso" | "img" | "vsix" => Kind::Installers,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" => Kind::Archives,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "ods" | "odp" | "csv" | "epub" => Kind::Documents,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "avif" | "tif" | "tiff" | "svg" | "raw" | "dng" | "psd" => Kind::Images,
        "mp4" | "mkv" | "mov" | "avi" | "wmv" | "webm" | "m4v" | "mpg" | "mpeg" => Kind::Videos,
        _ => Kind::Other,
    }
}

/// The first word of a name, for telling similar downloads apart: `invoice` for
/// `Invoice-2024-03.pdf`. Words shorter than three letters say too little.
pub fn token_of(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let word: String = stem.chars().skip_while(|c| !c.is_alphabetic()).take_while(|c| c.is_alphabetic()).collect();
    if word.chars().count() >= 3 { word.to_lowercase() } else { String::new() }
}

/// One item at the top of the Downloads folder, from its directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Milliseconds since 1970.
    pub created: i64,
    pub modified: i64,
    pub attrs: u32,
}

impl Entry {
    /// When it arrived: a download is made when it starts and written until it ends, and a copy
    /// keeps the time it was last changed but is made anew.
    pub fn arrived(&self) -> i64 {
        self.created.max(self.modified)
    }

    /// Whether the page lists it: not hidden or a system file (`desktop.ini`), and not a
    /// download still in progress.
    fn listed(&self) -> bool {
        self.attrs & (HIDDEN | SYSTEM) == 0 && !PARTIAL.contains(&extension(&self.name, self.dir).as_str())
    }
}

fn unix_ms(time: std::io::Result<std::time::SystemTime>) -> i64 {
    time.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// The items at the top of `folder`. Directory entries carry everything, so nothing is opened
/// (an online-only file is not downloaded).
pub fn read(folder: &str) -> std::io::Result<Vec<Entry>> {
    use std::os::windows::fs::MetadataExt;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let Ok(entry) = entry else { continue };
        // On Windows a directory entry's metadata comes with the listing: no file is opened.
        let Ok(meta) = entry.metadata() else { continue };
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            created: unix_ms(meta.created()),
            modified: unix_ms(meta.modified()),
            attrs: meta.file_attributes(),
        });
    }
    Ok(entries)
}

/// A folder a download could go to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub path: String,
    /// The folder's name, as the button says it.
    pub label: String,
    /// `learned` (similar files went there), `kind` (the known folder for the kind) or
    /// `project` (a project visited lately).
    pub why: &'static str,
}

/// The known folders the kinds go to.
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub documents: Option<String>,
    pub pictures: Option<String>,
    pub videos: Option<String>,
    pub music: Option<String>,
}

impl Known {
    pub fn current() -> Self {
        Known {
            documents: places::known_folder("documents"),
            pictures: places::known_folder("pictures"),
            videos: places::known_folder("videos"),
            music: places::known_folder("music"),
        }
    }
}

fn folder_label(path: &str) -> String {
    let trimmed = path.trim_end_matches('\\');
    trimmed.rsplit('\\').next().filter(|name| !name.is_empty()).unwrap_or(trimmed).to_owned()
}

fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

/// Where an item could go, best first. `exists` says whether a folder is still there.
pub fn suggest(
    name: &str,
    dir: bool,
    learned: &[Learned],
    known: &Known,
    projects: &[(String, String)],
    downloads: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<Suggestion> {
    let ext = extension(name, dir);
    let kind = kind_of(name, dir);
    let token = token_of(name);
    // Similar files' destinations: the same extension counts most, then the same first word,
    // then the same kind; then how often and how lately.
    let mut scored: Vec<(f64, i64, &Learned)> = learned
        .iter()
        .filter_map(|row| {
            let mut score = 0.0;
            if !ext.is_empty() && row.ext == ext {
                score += 3.0;
            }
            if !token.is_empty() && row.token == token {
                score += 2.0;
            }
            if row.kind == kind.name() {
                score += 1.0;
            }
            (score > 0.0).then(|| (score * (1.0 + (row.count as f64).ln()), row.last_at, row))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out: Vec<Suggestion> = Vec::new();
    let push = |path: &str, why: &'static str, out: &mut Vec<Suggestion>| {
        if out.len() < MAX_SUGGESTIONS && !same(path, downloads) && !out.iter().any(|s| same(&s.path, path)) && exists(path) {
            out.push(Suggestion { path: path.to_owned(), label: folder_label(path), why });
        }
    };
    for (_, _, row) in scored {
        push(&row.destination, "learned", &mut out);
    }
    let home = match kind {
        Kind::Documents => known.documents.as_deref(),
        Kind::Images => known.pictures.as_deref(),
        Kind::Videos => known.videos.as_deref(),
        Kind::Other if matches!(ext.as_str(), "mp3" | "flac" | "wav" | "m4a" | "ogg" | "opus" | "aac") => known.music.as_deref(),
        _ => None,
    };
    if let Some(home) = home {
        push(home, "kind", &mut out);
    }
    // Pictures, videos and installers rarely belong to a project; archives, documents and the
    // rest may.
    if !matches!(kind, Kind::Images | Kind::Videos | Kind::Installers) {
        for (_, root) in projects {
            push(root, "project", &mut out);
        }
    }
    out
}

/// An item as the page shows it.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub dir: bool,
    pub kind: Kind,
    pub size: u64,
    /// Milliseconds since 1970.
    pub modified: i64,
    pub arrived: i64,
    pub attrs: u32,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Page {
    /// The Downloads folder.
    pub folder: String,
    /// What counts as new: what arrived after this, in milliseconds since 1970.
    pub since: i64,
    /// The new items, or every item when all were asked for, newest first.
    pub items: Vec<Item>,
    /// How many items are new.
    pub new: u32,
    /// How many items the folder lists in all.
    pub total: u32,
}

fn downloads() -> Result<String, String> {
    places::known_folder("downloads").ok_or_else(|| "Windows doesn't say where your Downloads folder is.".to_owned())
}

/// What counts as new: after the last look, or the first time, the last week.
fn since(db: &Db, now: i64) -> i64 {
    store::seen(db).ok().flatten().unwrap_or(now - FIRST_LOOK)
}

/// How many items arrived after `since`.
fn count_new(entries: &[Entry], since: i64) -> u32 {
    entries.iter().filter(|entry| entry.listed() && entry.arrived() > since).count() as u32
}

/// The projects visited most lately, best first, as `(name, root)`.
fn recent_projects(db: &Db, projects: &Projects, now: i64) -> Vec<(String, String)> {
    let ranked = kestrel_db::shortcuts::frecency::ranked(db, now / 1000).unwrap_or_default();
    let mut scored: Vec<(f64, String, String)> = places::projects(projects)
        .into_iter()
        .filter_map(|(name, root)| {
            let base = format!("{}\\", root.trim_end_matches('\\').to_lowercase());
            let score = ranked
                .iter()
                .filter(|visit| {
                    let path = visit.path.to_lowercase();
                    same(&path, &root) || path.starts_with(&base)
                })
                .map(|visit| visit.score)
                .fold(0.0, f64::max);
            // Projects the tests name stand in for visited ones.
            let score = if places::is_stand_in_project(&root) { score.max(1.0) } else { score };
            (score > 0.0).then_some((score, name, root))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(2).map(|(_, name, root)| (name, root)).collect()
}

/// The page: the items that arrived after `since` (after the last look when not given), or
/// with `all`, every item, newest first.
#[tauri::command(async)]
pub fn triage_list(db: State<Arc<Db>>, projects: State<Arc<Projects>>, since: Option<i64>, all: bool) -> Result<Page, String> {
    let folder = downloads()?;
    let now = crate::parity::now_ms();
    let since = since.unwrap_or_else(|| self::since(&db, now));
    let entries = read(&folder).map_err(|e| format!("Can't read {folder}: {e}"))?;
    let learned = store::learned(&db).unwrap_or_default();
    let known = Known::current();
    let recent = recent_projects(&db, &projects, now);
    let exists = |path: &str| Path::new(path).is_dir();
    let mut listed: Vec<&Entry> = entries.iter().filter(|entry| entry.listed() && (all || entry.arrived() > since)).collect();
    listed.sort_by(|a, b| b.arrived().cmp(&a.arrived()).then_with(|| a.name.cmp(&b.name)));
    let items = listed
        .into_iter()
        .map(|entry| Item {
            path: fileops::child(&folder, &entry.name),
            name: entry.name.clone(),
            ext: extension(&entry.name, entry.dir),
            dir: entry.dir,
            kind: kind_of(&entry.name, entry.dir),
            size: entry.size,
            modified: entry.modified,
            arrived: entry.arrived(),
            attrs: entry.attrs,
            suggestions: suggest(&entry.name, entry.dir, &learned, &known, &recent, &folder, &exists),
        })
        .collect();
    Ok(Page { folder, since, items, new: count_new(&entries, since), total: entries.iter().filter(|e| e.listed()).count() as u32 })
}

/// How many downloads arrived since the owner last looked, for the sidebar's badge.
#[tauri::command(async)]
pub fn triage_count(db: State<Arc<Db>>) -> u32 {
    current_count(&db)
}

fn current_count(db: &Db) -> u32 {
    let Ok(folder) = downloads() else { return 0 };
    let since = since(db, crate::parity::now_ms());
    read(&folder).map(|entries| count_new(&entries, since)).unwrap_or(0)
}

/// The owner looked at the page: what is there now is no longer new.
#[tauri::command]
pub fn triage_mark_seen(app: AppHandle, db: State<Arc<Db>>) -> Result<i64, String> {
    let now = crate::parity::now_ms();
    store::set_seen(&db, now)?;
    let _ = app.emit("triage-changed", 0u32);
    Ok(now)
}

/// Every path must be an item at the top of the Downloads folder: the page acts on nothing else.
fn check(paths: &[String]) -> Result<String, String> {
    let folder = downloads()?;
    if paths.is_empty() {
        return Err("Nothing was chosen.".into());
    }
    for path in paths {
        if !same(&fileops::parent(path), &folder) {
            return Err(format!("{path} isn't in the Downloads folder."));
        }
    }
    Ok(folder)
}

/// What a move did.
#[derive(Serialize)]
pub struct Moved {
    /// Where they went, and the names they have there.
    pub folder: String,
    pub names: Vec<String>,
    /// Nothing was skipped or cancelled in the shell's dialogs.
    pub completed: bool,
}

/// Moves downloads into `destination`, with the shell's progress and conflict dialogs, as one
/// operation Ctrl+Z undoes, and learns where such files go.
#[tauri::command(async)]
pub fn triage_move(app: AppHandle, window: Window, db: State<Arc<Db>>, journal: State<Journal>, paths: Vec<String>, destination: String) -> Result<Moved, String> {
    check(&paths)?;
    if !Path::new(&destination).is_dir() {
        return Err(format!("{destination} isn't a folder."));
    }
    let kinds: Vec<(String, String, String)> = paths
        .iter()
        .map(|path| {
            let name = fileops::file_name(path);
            let dir = Path::new(path).is_dir();
            (extension(name, dir), kind_of(name, dir).name().to_owned(), token_of(name))
        })
        .collect();
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    let request = Request::Transfer { sources: paths.clone(), destination: destination.clone(), kind: Transfer::Move };
    let outcome = fileops::execute(request, Options { owner, quiet: false })?;
    journal.record(&outcome);
    let mut names = Vec::new();
    let mut learned = HashSet::new();
    for (op, done) in &outcome.done {
        if let Done::Moved { to, .. } = done {
            names.push(fileops::file_name(to).to_owned());
            if let Some((ext, kind, token)) = kinds.get(*op)
                && learned.insert(*op)
            {
                let _ = store::record_move(&db, ext, kind, token, &destination, crate::parity::now_ms());
            }
        }
    }
    let _ = app.emit("triage-changed", current_count(&db));
    Ok(Moved { folder: destination, names, completed: outcome.completed })
}

/// Sends downloads to the Recycle Bin, as one operation Ctrl+Z undoes. `false` if cancelled.
#[tauri::command(async)]
pub fn triage_recycle(app: AppHandle, window: Window, db: State<Arc<Db>>, journal: State<Journal>, paths: Vec<String>) -> Result<bool, String> {
    check(&paths)?;
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    let outcome = fileops::execute(Request::Delete { paths, permanent: false }, Options { owner, quiet: false })?;
    journal.record(&outcome);
    let _ = app.emit("triage-changed", current_count(&db));
    Ok(outcome.completed)
}

/// Sets when the page was last looked at (development builds only), so the tests can say
/// what is new; `None` forgets it.
#[tauri::command]
pub fn triage_dev_seen(app: AppHandle, db: State<Arc<Db>>, at: Option<i64>) -> Result<(), String> {
    crate::projects::launch::development_only()?;
    match at {
        Some(at) => store::set_seen(&db, at)?,
        None => store::forget_seen(&db)?,
    }
    watch(&app);
    Ok(())
}

/// Forgets every destination learnt beneath `folder` (development builds only), so the tests
/// leave nothing of their scratch folders behind.
#[tauri::command]
pub fn triage_dev_forget(db: State<Arc<Db>>, folder: String) -> Result<(), String> {
    crate::projects::launch::development_only()?;
    let base = format!("{}\\", folder.trim_end_matches('\\').to_lowercase());
    for row in store::learned(&db)? {
        if row.destination.to_lowercase().starts_with(&base) {
            store::forget(&db, &row.destination)?;
        }
    }
    Ok(())
}

/// Watches the Downloads folder (or the folder standing in for it), telling the windows how
/// many items are new whenever it changes. Called at start and when the stand-in changes.
pub fn watch(app: &AppHandle) {
    let Ok(folder) = downloads() else { return };
    let handle = app.clone();
    let tell = move || {
        let db = handle.state::<Arc<Db>>();
        let _ = handle.emit("triage-changed", current_count(&db));
        let _ = handle.emit("triage-folder", ());
    };
    tell();
    let watcher = app.state::<Watcher>();
    watcher.watch_for("triage", &folder, move |_| tell());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learned(ext: &str, kind: &str, token: &str, destination: &str, count: i64, last_at: i64) -> Learned {
        Learned { ext: ext.into(), kind: kind.into(), token: token.into(), destination: destination.into(), count, last_at }
    }

    const DOWNLOADS: &str = r"C:\Users\me\Downloads";

    fn known() -> Known {
        Known {
            documents: Some(r"C:\Users\me\Documents".into()),
            pictures: Some(r"C:\Users\me\Pictures".into()),
            videos: Some(r"C:\Users\me\Videos".into()),
            music: Some(r"C:\Users\me\Music".into()),
        }
    }

    fn paths(suggestions: &[Suggestion]) -> Vec<(&str, &str)> {
        suggestions.iter().map(|s| (s.path.as_str(), s.why)).collect()
    }

    #[test]
    fn kinds_follow_extensions() {
        for (name, dir, kind) in [
            ("setup.exe", false, Kind::Installers),
            ("Tool.MSI", false, Kind::Installers),
            ("ubuntu.iso", false, Kind::Installers),
            ("photos.zip", false, Kind::Archives),
            ("src.tar.gz", false, Kind::Archives),
            ("report.pdf", false, Kind::Documents),
            ("data.csv", false, Kind::Documents),
            ("IMG_0001.HEIC", false, Kind::Images),
            ("clip.mp4", false, Kind::Videos),
            ("song.mp3", false, Kind::Other),
            ("unpacked", true, Kind::Other),
            ("no-extension", false, Kind::Other),
        ] {
            assert_eq!(kind_of(name, dir), kind, "{name}");
        }
    }

    #[test]
    fn first_words_tell_similar_files_apart() {
        assert_eq!(token_of("Invoice-2024-03.pdf"), "invoice");
        assert_eq!(token_of("2024 bank statement.pdf"), "bank");
        assert_eq!(token_of("IMG_1234.jpg"), "img");
        assert_eq!(token_of("a1.txt"), "");
        assert_eq!(token_of("Résumé final.docx"), "résumé");
    }

    #[test]
    fn suggestions_learn_from_earlier_moves() {
        let everything = |_: &str| true;
        let history = [
            learned("pdf", "documents", "invoice", r"D:\Finance\Invoices", 5, 100),
            learned("pdf", "documents", "manual", r"D:\Manuals", 2, 200),
            learned("jpg", "images", "img", r"D:\Phone", 9, 50),
        ];
        // An invoice goes where invoices went, then where PDFs went, then Documents.
        let invoice = suggest("Invoice-2024-04.pdf", false, &history, &known(), &[], DOWNLOADS, &everything);
        assert_eq!(paths(&invoice), [
            (r"D:\Finance\Invoices", "learned"),
            (r"D:\Manuals", "learned"),
            (r"C:\Users\me\Documents", "kind")
        ]);
        // Another PDF: the folder PDFs went to most often first.
        let other = suggest("Taxes 2025.pdf", false, &history, &known(), &[], DOWNLOADS, &everything);
        assert_eq!(other[0].path, r"D:\Finance\Invoices");
        // A picture: where pictures went, then Pictures; no projects for pictures.
        let projects = [("kestrel".to_owned(), r"C:\src\Kestrel".to_owned())];
        let photo = suggest("IMG_9999.jpg", false, &history, &known(), &projects, DOWNLOADS, &everything);
        assert_eq!(paths(&photo), [(r"D:\Phone", "learned"), (r"C:\Users\me\Pictures", "kind")]);
        // An archive: no known folder, but the project visited lately; an installer: neither.
        let archive = suggest("dataset.zip", false, &[], &known(), &projects, DOWNLOADS, &everything);
        assert_eq!(paths(&archive), [(r"C:\src\Kestrel", "project")]);
        assert!(suggest("setup.exe", false, &[], &known(), &projects, DOWNLOADS, &everything).is_empty());
        // Folders that are gone, and Downloads itself, are never offered.
        let gone = |path: &str| path != r"D:\Finance\Invoices";
        let moved_back = [learned("pdf", "documents", "", DOWNLOADS, 9, 1)];
        let invoice = suggest("Invoice-2024-05.pdf", false, &[history.to_vec(), moved_back.to_vec()].concat(), &known(), &[], DOWNLOADS, &gone);
        assert_eq!(paths(&invoice), [(r"D:\Manuals", "learned"), (r"C:\Users\me\Documents", "kind")]);
        // Music has a home too.
        assert_eq!(suggest("track.flac", false, &[], &known(), &[], DOWNLOADS, &everything)[0].path, r"C:\Users\me\Music");
    }

    /// The loop the page runs: a move is recorded as the move command records it, and the next
    /// similar download is offered the same folder first.
    #[test]
    fn a_move_recorded_in_the_database_is_suggested_next_time() {
        let db = Db::open_in_memory().unwrap();
        let everything = |_: &str| true;
        let before = suggest("Statement-2026-09.pdf", false, &store::learned(&db).unwrap(), &known(), &[], DOWNLOADS, &everything);
        assert_eq!(before[0].path, r"C:\Users\me\Documents");
        let name = "Statement-2026-08.pdf";
        store::record_move(&db, &extension(name, false), kind_of(name, false).name(), &token_of(name), r"D:\Bank", 1).unwrap();
        let after = suggest("Statement-2026-09.pdf", false, &store::learned(&db).unwrap(), &known(), &[], DOWNLOADS, &everything);
        assert_eq!(paths(&after), [(r"D:\Bank", "learned"), (r"C:\Users\me\Documents", "kind")]);
        // A picture is no statement: the PDF's folder isn't offered for it.
        let photo = suggest("IMG_1.jpg", false, &store::learned(&db).unwrap(), &known(), &[], DOWNLOADS, &everything);
        assert_eq!(paths(&photo), [(r"C:\Users\me\Pictures", "kind")]);
    }

    #[test]
    fn the_folder_is_read_without_partial_or_hidden_items() {
        let root = std::env::temp_dir().join(format!("kestrel-triage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("unpacked")).unwrap();
        for name in ["report.pdf", "setup.exe", "movie.mp4.crdownload", "big.iso.part", "desktop.ini"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let hide = |name: &str| {
            let wide: Vec<u16> = root.join(name).to_str().unwrap().encode_utf16().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), HIDDEN | SYSTEM) };
        };
        hide("desktop.ini");
        let entries = read(root.to_str().unwrap()).unwrap();
        let mut listed: Vec<&str> = entries.iter().filter(|e| e.listed()).map(|e| e.name.as_str()).collect();
        listed.sort();
        assert_eq!(listed, ["report.pdf", "setup.exe", "unpacked"]);
        let now = unix_ms(Ok(std::time::SystemTime::now()));
        assert!(entries.iter().all(|e| e.arrived() > now - 60_000 && e.arrived() <= now + 1000));
        assert_eq!(count_new(&entries, now - 60_000), 3);
        assert_eq!(count_new(&entries, now + 60_000), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
