//! Archives, as Explorer handles them: "Extract all" and "Extract here", "Compress to ZIP file"
//! and "Compress to…", and browsing inside an archive as a read-only folder.
//!
//! - [`address`]: archive formats, and addresses inside archives (`C:\a.zip\folder\file`),
//!   which `list_dir`, the preview and opening a file all accept.
//! - [`reader`]: one way to read zip, 7z, tar (plain, gzip, bzip2, xz), RAR (through Windows'
//!   own tar.exe) and single compressed files.
//! - [`names`]: the paths an archive stores, made safe or refused.
//! - [`extract`]: extraction with every safety check, staged so nothing is left half-written.
//! - [`compress`]: making zip, 7z and tar.gz archives.
//! - [`browse`]: listing inside archives, and copying single items out for the preview, for
//!   opening, and for the clipboard and drags.
//! - [`motw`]: the Mark of the Web, passed on from an archive to what comes out of it.
//! - [`job`]: progress, cancelling, and failures in words.
//!
//! Every extraction and compression goes into the undo journal as the items it created, so an
//! undo sends them to the Recycle Bin, and only while they are as the job left them.

mod add;
mod address;
mod browse;
mod compress;
mod extract;
mod job;
mod motw;
mod names;
mod reader;
#[cfg(test)]
mod tests;
mod time;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::archives::Preferences;
use kestrel_index::DirItem;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State, Window};

use crate::engine::{Engine, Listing, SortSpec};
use crate::fileops::{self, Done, Outcome};
use crate::icons::IconKey;
use crate::undo::Journal;
use crate::visibility::Visibility;

pub use browse::{forget_protected_copies, preview_details, preview_file, tidy};
pub use job::Jobs;
use browse::Reading;
use job::{Failure, Job, Problem};

/// What Kestrel says when asked to change something inside an archive.
pub const READ_ONLY: &str = "Archives are read-only in Kestrel. Extract the items to change them.";

/// Whether `path` names an item inside an archive, which only exists as an address.
pub fn is_entry(path: &str) -> bool {
    address::is_entry(path)
}

/// Whether any of `paths` is inside an archive. Paths from one listing share a folder, so each
/// folder is looked at once: an item is in an archive exactly when its folder is listed from one.
fn any_entry(paths: &[String]) -> bool {
    let mut seen = HashSet::new();
    paths.iter().any(|path| {
        let parent = fileops::parent(path);
        seen.insert(parent.to_lowercase()) && address::is_location(&parent)
    })
}

/// Refuses to change items inside an archive: renaming, deleting, moving.
pub fn refuse_entries(paths: &[String]) -> Result<(), String> {
    if any_entry(paths) { Err(READ_ONLY.to_owned()) } else { Ok(()) }
}

/// Refuses to put anything into an archive, or a folder inside one.
pub fn refuse_location(folder: &str) -> Result<(), String> {
    if address::is_location(folder) { Err(READ_ONLY.to_owned()) } else { Ok(()) }
}

/// The window and the number it gave a listing, so it can cancel one it no longer wants.
pub struct Ticket<'a> {
    pub jobs: &'a Jobs,
    pub window: &'a str,
    pub id: u32,
}

/// Lists a folder inside an archive, or an archive itself, as the file list shows folders;
/// `None` for a path not in an archive. The listing names the archive, so the window knows.
/// A compressed tar is read through to be listed, which for a large one takes minutes: with a
/// `ticket`, the window can cancel it (`archive_cancel`), and the listing fails with "Cancelled."
pub fn list(engine: &Engine, path: &str, sort: SortSpec, visibility: Visibility, ticket: Option<Ticket>) -> Option<Result<Listing, String>> {
    let job = Arc::new(Job::quiet_with_id(ticket.as_ref().map_or(0, |ticket| ticket.id)));
    let _running = ticket.as_ref().map(|ticket| ticket.jobs.begin(ticket.window, job.clone()));
    let located = browse::locate(path, true, Reading::All(&job))?;
    Some(match located {
        Err(Failure::Password) => Err(format!(
            "{} is protected by a password, which its list of contents needs too. Extract it to enter the password.",
            path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path)
        )),
        Err(failure) => Err(failure.problem(path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path)).message),
        Ok(located) if !located.node().dir => Err(format!("{} isn't a folder", located.address)),
        Ok(located) => {
            let items = browse::items(&located);
            let archive = located.outer.to_string_lossy().into_owned();
            Ok(engine.show_items(located.address.clone(), items, sort, visibility, archive))
        }
    })
}

/// The items of an archive, or of a folder inside one, as directory entries, for lists other
/// than the file list (the palette's typed paths); `None` for a path not in an archive, or one
/// that can't be read without more: a password, or reading a compressed tar through, which the
/// palette, asking with every key typed, never starts (one opened in a tab is at hand).
pub fn entries(path: &str) -> Option<Vec<DirItem>> {
    let located = browse::locate(path, true, Reading::AtHand)?.ok()?;
    located.node().dir.then(|| browse::items(&located))
}

/// Whether [`icon_key`] has to ask the disk about this key: only for a path that, by its
/// spelling, could be inside an archive. The icon protocol runs on the window's own thread,
/// which must never wait for the disk; such keys are resolved on another.
pub fn icon_key_needs_disk(key: &IconKey) -> bool {
    matches!(key, IconKey::File(path) if address::may_be_entry(path))
}

/// The icon for an item inside an archive, which can't be asked of the file itself: its type's.
pub fn icon_key(key: IconKey) -> IconKey {
    match key {
        IconKey::File(path) if address::is_entry(&path) => {
            let name = fileops::file_name(&path);
            let ext = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map(|(_, ext)| ext.to_ascii_lowercase());
            IconKey::Ext(ext.unwrap_or_else(|| "_".to_owned()))
        }
        other => other,
    }
}

/// Copies items inside archives out to the cache, so the clipboard or a drag can carry them;
/// other paths pass through. Without a password, as a drag can't stop to ask for one.
pub fn materialize(paths: Vec<String>) -> Result<Vec<String>, String> {
    if !any_entry(&paths) {
        return Ok(paths);
    }
    browse::materialize(&paths, None, &Job::quiet()).map_err(|(failure, name)| failure.problem(&name).message)
}

/// Opens a file inside an archive as Explorer does, from a copy in the temp folder; `None` for
/// a path not in an archive.
pub fn open_entry(path: &str) -> Option<Result<(), String>> {
    let name = path.rsplit('\\').next().unwrap_or(path).to_owned();
    browse::open_entry(path, None, &Job::quiet()).map(|result| result.map_err(|failure| failure.problem(&name).message))
}

// ---------- adding to a zip ----------

/// The zip a drop or paste at `target` adds to, and the folder inside it: `target` is a zip
/// file, or a folder of one open in a tab. `None` for anything else.
fn zip_target(target: &str) -> Option<(PathBuf, Vec<String>)> {
    let (archive, inner) = address::split(target)?;
    let archive = PathBuf::from(archive);
    // An archive inside the zip is not the zip: nothing is added to nested archives.
    let nested = inner.iter().any(|part| address::Format::of(part).is_some());
    (add::is_zip(&archive) && !nested).then_some((archive, inner))
}

/// Whether files pasted into the folder `target` go into a zip: it is a zip open in a tab, or a
/// folder of one (see [`add_to`]).
pub fn adds_to_zip(target: &str) -> bool {
    address::is_location(target) && zip_target(target).is_some()
}

/// Adds `paths` to the zip at `target` (a zip, or a folder of one), as Explorer does with files
/// dropped on a zip or pasted into one, and records it for undo. Items inside other archives are
/// copied out first. Returns the names added, to select.
pub fn add_to(window: &Window, journal: &Journal, target: &str, paths: Vec<String>) -> Result<add::Added, String> {
    let Some((zip, inner)) = zip_target(target) else {
        return Err(READ_ONLY.to_owned());
    };
    let paths = materialize(paths)?;
    let sources: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let options = fileops::Options { owner: window.hwnd().ok().map(|hwnd| hwnd.0 as isize), quiet: false };
    let (added, outcome) = add::add_to_zip(&zip, &inner, &sources, options, &Job::quiet()).map_err(|failure| {
        let name = fileops::file_name(&zip.to_string_lossy()).to_owned();
        failure.problem(&name).message
    })?;
    let name = fileops::file_name(&added.archive).to_owned();
    journal.record_as(&outcome, format!("adding to {name}"));
    Ok(added)
}

/// Adds files dropped on a zip, or into a folder of one open in a tab, to it.
#[tauri::command(async)]
pub fn archive_add(window: Window, journal: State<Journal>, target: String, paths: Vec<String>) -> Result<add::Added, String> {
    add_to(&window, &journal, &target, paths)
}

// ---------- commands ----------

/// A job of `window`'s, whose progress goes to that window only: every window numbers its jobs
/// from 1, so another window's panel would take the progress of its own job of that number.
fn start_job(app: &AppHandle, window: &Window, id: u32) -> Arc<Job> {
    let (app, label) = (app.clone(), window.label().to_owned());
    Arc::new(Job::new(id, move |progress| {
        let _ = app.emit_to(label.as_str(), "archive-progress", progress);
    }))
}

/// The folder an item now in it is in, and its name.
fn split_path(path: &Path) -> (String, String) {
    let text = path.to_string_lossy();
    (fileops::parent(&text), fileops::file_name(&text).to_owned())
}

/// Records what a job created, as one operation the undo journal can reverse.
fn record(journal: &Journal, created: &[PathBuf], completed: bool, label: String) {
    let done = created
        .iter()
        .enumerate()
        .map(|(at, path)| (at, Done::Created { path: path.to_string_lossy().into_owned() }))
        .collect();
    journal.record_as(&Outcome { completed, done, missing: Vec::new() }, label);
}

/// A folder typed or chosen as a destination, checked.
fn destination_folder(text: &str) -> Result<PathBuf, Problem> {
    let folder = crate::engine::normalize(text);
    if folder.is_empty() || !Path::new(&folder).is_absolute() {
        return Err(Problem::other("Type the full path of a folder, such as C:\\Users\\you\\Documents."));
    }
    if address::is_location(&folder) {
        return Err(Problem::other("Kestrel can't put files into an archive. Choose a folder outside it."));
    }
    Ok(PathBuf::from(folder))
}

#[derive(Debug, Serialize)]
pub struct ExtractReport {
    /// The folder to select the new items in, and their names there.
    pub folder: String,
    pub names: Vec<String>,
    /// Where the files went, to show them.
    pub destination: String,
    pub links_skipped: usize,
    pub renamed: usize,
    /// Nothing was skipped in the conflict dialog.
    pub completed: bool,
}

/// Extracts the archive at or around `source` (an archive, or a folder inside one) into
/// `destination`, or with `items`, only those items of one folder inside an archive, keeping
/// what is beneath them. The destination is made if it doesn't exist. Progress goes out as
/// `archive-progress` events for `job`, which [`archive_cancel`] stops.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub fn archive_extract(
    app: AppHandle,
    window: Window,
    journal: State<'_, Journal>,
    jobs: State<'_, Jobs>,
    job: u32,
    source: String,
    items: Option<Vec<String>>,
    destination: String,
    password: Option<String>,
) -> Result<ExtractReport, Problem> {
    let destination = destination_folder(&destination)?;
    let not_archive = || Problem::other(format!("{source} isn't an archive Kestrel can open."));
    let (archive, inner) = address::split(&source).ok_or_else(not_archive)?;
    // Started before the archive is looked into, which may mean reading a stream through.
    let job = start_job(&app, &window, job);
    let _running = jobs.begin(window.label(), job.clone());
    let reading = Reading::All(&job);
    // The archive file to read, what of it, the archive on disk whose mark passes on, and the
    // listing already read, if any.
    let (file, format, wanted, base, outer, tree) = match items.filter(|items| !items.is_empty()) {
        // An archive on disk, whole: read as it is, without listing it first, so one whose list
        // of contents is itself encrypted can still be extracted with its password.
        None if inner.is_empty() => {
            let file = PathBuf::from(&archive);
            let format = browse::format_of(&file).map_err(|failure| failure.problem(fileops::file_name(&archive)))?;
            (file.clone(), format, None, Vec::new(), file, None)
        }
        // The archive a folder inside it belongs to, or one inside another, whole.
        None => {
            let located = browse::locate(&source, true, reading).ok_or_else(not_archive)?;
            let located = located.map_err(|failure| failure.problem(fileops::file_name(&source)))?;
            (located.file.clone(), located.tree.format, None, Vec::new(), located.outer, Some(located.tree))
        }
        Some(items) => {
            let mut found = Vec::new();
            for item in &items {
                let located = browse::locate_entry(item, reading).ok_or_else(not_archive)?;
                found.push(located.map_err(|failure| failure.problem(fileops::file_name(item)))?);
            }
            let first = found.remove(0);
            if found.iter().any(|other| other.file != first.file) {
                return Err(Problem::other("Extract items from one archive at a time."));
            }
            let base = first.inner[..first.inner.len().saturating_sub(1)].to_vec();
            let wanted: Vec<Vec<String>> = std::iter::once(first.inner.clone()).chain(found.into_iter().map(|other| other.inner)).collect();
            (first.file.clone(), first.tree.format, Some(wanted), base, first.outer, Some(first.tree))
        }
    };
    let name = file.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let zone = motw::zone_of(&outer);
    let request = extract::Request {
        archive: &file,
        format,
        entries: tree.as_ref().and_then(|tree| tree.listing()),
        name: &name,
        password: password.as_deref(),
        items: wanted.as_deref(),
        base: &base,
        destination: &destination,
        zone: zone.map(|zone| (zone, outer.as_path())),
        conflicts: extract::Conflicts::Ask(crate::interactive(&window)),
    };
    let extracted = extract::extract(&request, &job).map_err(|failure| failure.problem(&name))?;
    record(&journal, &extracted.created, extracted.completed, format!("extraction of {name}"));
    let (folder, names) = if extracted.destination_created {
        let (folder, top) = extracted.created.first().map(|path| split_path(path)).unwrap_or_default();
        (folder, vec![top])
    } else {
        let folder = destination.to_string_lossy().into_owned();
        let names = extracted
            .created
            .iter()
            .map(|path| split_path(path))
            .filter(|(parent, _)| fileops::key(parent) == fileops::key(&folder))
            .map(|(_, name)| name)
            .collect();
        (folder, names)
    };
    Ok(ExtractReport {
        folder,
        names,
        destination: destination.to_string_lossy().into_owned(),
        links_skipped: extracted.links_skipped,
        renamed: extracted.renamed,
        completed: extracted.completed,
    })
}

#[derive(Debug, Serialize)]
pub struct CompressReport {
    pub path: String,
    pub name: String,
}

/// Compresses `paths` into an archive in `folder`, named `name` or, without one, after the
/// first item, as "Compress to ZIP file" names it. A name taken meanwhile gets a number. Paths,
/// not rows: the dialog that asks for the name may stay open while the folder is listed again.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub fn archive_compress(
    app: AppHandle,
    window: Window,
    journal: State<'_, Journal>,
    jobs: State<'_, Jobs>,
    job: u32,
    paths: Vec<String>,
    folder: String,
    name: Option<String>,
    kind: compress::Kind,
    level: compress::Level,
) -> Result<CompressReport, Problem> {
    let sources = paths;
    if sources.is_empty() {
        return Err(Problem::other("Select the items to compress."));
    }
    if refuse_entries(&sources).is_err() {
        return Err(Problem::other("Items inside an archive can't be compressed as they are. Extract them first."));
    }
    let folder = destination_folder(&folder)?;
    let sources: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
    let name = match name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()) {
        Some(name) if name.to_ascii_lowercase().ends_with(kind.extension()) => name,
        Some(name) => format!("{name}{}", kind.extension()),
        None => compress::default_name(&sources[0], kind),
    };
    let job = start_job(&app, &window, job);
    let _running = jobs.begin(window.label(), job.clone());
    let request = compress::Request { sources: &sources, folder: &folder, name: &name, kind, level };
    let path = compress::compress(&request, &job).map_err(|failure| failure.problem(&name))?;
    let subject = match sources.as_slice() {
        [one] => one.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        many => format!("{} items", many.len()),
    };
    record(&journal, std::slice::from_ref(&path), true, format!("compression of {subject}"));
    let (_, name) = split_path(&path);
    Ok(CompressReport { path: path.to_string_lossy().into_owned(), name })
}

/// Asks a running job of this window's to stop: an extraction or compression removes whatever
/// it had written; a listing fails with "Cancelled.".
#[tauri::command]
pub fn archive_cancel(window: Window, jobs: State<'_, Jobs>, job: u32) -> bool {
    jobs.cancel(window.label(), job)
}

/// Opens a file inside an archive from a copy in the temp folder, with the password if needed.
/// Taking it out of a large compressed tar takes a while: progress goes out for `job`, which
/// [`archive_cancel`] stops.
#[tauri::command(async)]
pub fn archive_open(app: AppHandle, window: Window, jobs: State<'_, Jobs>, job: u32, path: String, password: Option<String>) -> Result<(), Problem> {
    let name = fileops::file_name(&path).to_owned();
    let job = start_job(&app, &window, job);
    let _running = jobs.begin(window.label(), job.clone());
    match browse::open_entry(&path, password.as_deref(), &job) {
        Some(result) => result.map_err(|failure| failure.problem(&name)),
        None => Err(Problem::other(format!("{path} isn't inside an archive."))),
    }
}

/// Copies items inside an archive to the clipboard as files: they are copied out to the cache
/// first, so that pasting anywhere, in Explorer too, copies real files.
#[tauri::command(async)]
pub fn archive_copy(app: AppHandle, window: Window, jobs: State<'_, Jobs>, job: u32, paths: Vec<String>, password: Option<String>) -> Result<usize, Problem> {
    let job = start_job(&app, &window, job);
    let _running = jobs.begin(window.label(), job.clone());
    let files = browse::materialize(&paths, password.as_deref(), &job).map_err(|(failure, name)| failure.problem(&name))?;
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    crate::clipboard::set_files(owner, &files, false).map_err(Problem::other)?;
    Ok(files.len())
}

/// Windows' folder picker, starting at `start`; `None` if cancelled.
#[tauri::command(async)]
pub fn archive_pick_folder(window: Window, start: String) -> Option<String> {
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    std::thread::spawn(move || pick_folder(owner, &start)).join().ok().flatten()
}

fn pick_folder(owner: Option<isize>, start: &str) -> Option<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
        SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    use windows::core::{HSTRING, w};
    // SAFETY: COM is initialised on this thread for the dialog and uninitialised after the last
    // interface pointer from it is dropped, at the end of the inner block; every string passed
    // outlives its call, and the returned name is freed once copied.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
        let chosen = (|| {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let options = dialog.GetOptions().ok()?;
            dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
            let _ = dialog.SetTitle(w!("Choose where to extract the files"));
            let start = Path::new(start).ancestors().find(|folder| folder.is_dir()).map(|folder| folder.to_string_lossy().into_owned());
            if let Some(start) = start
                && let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_str()), None)
            {
                let _ = dialog.SetFolder(&folder);
            }
            dialog.Show(owner.map(|hwnd| HWND(hwnd as _))).ok()?;
            let item = dialog.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let text = name.to_string().ok();
            CoTaskMemFree(Some(name.0 as *const _));
            text
        })();
        CoUninitialize();
        chosen
    }
}

/// What the archive dialogs remember.
#[tauri::command(async)]
pub fn archive_preferences(db: State<'_, Arc<Db>>) -> Preferences {
    db.archive_preferences().unwrap_or_default()
}

#[tauri::command(async)]
pub fn archive_set_preferences(db: State<'_, Arc<Db>>, preferences: Preferences) -> Result<(), String> {
    Ok(db.set_archive_preferences(&preferences)?)
}

/// The archive whose reading the end-to-end tests hold, if any (`archive_dev_hold`).
static HELD: parking_lot::Mutex<Option<browse::HeldReading>> = parking_lot::Mutex::new(None);

/// For the end-to-end tests: holds the turn to read the archive at `path` (`hold`), as another
/// window's long reading of a large compressed tar would, or gives it back, so a listing of it
/// can be seen waiting, and giving way when its tab goes elsewhere. Development builds only.
#[tauri::command(async)]
pub fn archive_dev_hold(path: String, hold: bool) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("Only development builds have this.".into());
    }
    let mut held = HELD.lock();
    *held = None;
    if hold {
        let turn = browse::hold_reading(Path::new(&path)).map_err(|failure| failure.problem(fileops::file_name(&path)).message)?;
        *held = Some(turn);
    }
    Ok(())
}

/// For the end-to-end tests, which must leave the clipboard alone: what Copy or a drag would
/// carry for these paths. Development builds only.
#[tauri::command(async)]
pub fn archive_dev_materialize(paths: Vec<String>) -> Result<Vec<String>, String> {
    if !cfg!(debug_assertions) {
        return Err("Only development builds have this.".into());
    }
    materialize(paths)
}
