// Keeps a console window from opening alongside the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod access;
mod batchrename;
mod clipboard;
mod engine;
mod fileops;
mod icons;
mod indexer;
mod places;
mod recycle;
mod thumbs;
mod undo;
mod visibility;
mod watch;

use std::ptr::null;
use std::sync::Arc;

use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewWindow};
use windows_sys::Win32::UI::Shell::{SHObjectProperties, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use engine::{Engine, IndexStatus, Listing, Marks, Row, SortSpec};
use fileops::{Done, Options, Request, Transfer};
use icons::{IconKey, IconService};
use thumbs::ThumbService;
use undo::Journal;
use visibility::Visibility;
use watch::Watcher;

/// `SHOP_FILEPATH`: the object name passed to `SHObjectProperties` is a file system path.
const SHOP_FILEPATH: u32 = 2;

type Shared<'a> = State<'a, Arc<Engine>>;

#[tauri::command]
fn index_status(engine: Shared) -> IndexStatus {
    engine.status()
}

/// Lists a folder. With `watch`, the folder is also watched from now on and `dir-changed`
/// is emitted whenever its contents change; the watch is armed before the folder is read, so
/// no change can fall between the two. Hidden and system items are left out when
/// `show_hidden` is false.
#[tauri::command(async)]
fn list_dir(
    app: AppHandle,
    engine: Shared,
    watcher: State<Watcher>,
    path: String,
    sort: SortSpec,
    watch: bool,
    show_hidden: Option<bool>,
) -> Result<Listing, String> {
    if watch {
        let path = engine::normalize(&path);
        watcher.watch(&path, move |changed| {
            let _ = app.emit("dir-changed", changed);
        });
    }
    engine.list_dir_with(&path, sort, Visibility::from_request(show_hidden))
}

#[tauri::command(async)]
fn search(
    engine: Shared,
    query: String,
    scope: Option<String>,
    sort: SortSpec,
    show_hidden: Option<bool>,
) -> Result<Listing, String> {
    engine.search_with(&query, scope.as_deref(), sort, Visibility::from_request(show_hidden))
}

/// Maps a selection from one result set to another showing the same files.
#[tauri::command(async)]
fn carry_marks(engine: Shared, from: u32, to: u32, marks: Marks) -> Marks {
    engine.carry(from, to, &marks)
}

#[tauri::command]
fn release_set(engine: Shared, set: u32) {
    engine.release(set);
}

#[tauri::command(async)]
fn rows(engine: Shared, set: u32, start: u32, count: u32) -> Result<Vec<Row>, String> {
    engine.rows(set, start, count)
}

#[tauri::command(async)]
fn sort_set(engine: Shared, set: u32, sort: SortSpec, marks: Marks) -> Result<Marks, String> {
    engine.sort_set(set, sort, &marks)
}

#[tauri::command(async)]
fn locate_names(engine: Shared, set: u32, names: Vec<String>) -> Vec<(u32, u32)> {
    engine.locate_names(set, &names)
}

/// The window's handle, for parenting shell dialogs and owning the clipboard.
fn owner(window: &WebviewWindow) -> Option<isize> {
    window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
}

fn interactive(window: &WebviewWindow) -> Options {
    Options { owner: owner(window), quiet: false }
}

/// Sends the selected rows to the Recycle Bin, or erases them when `permanent`. Returns
/// `false` if the user cancelled.
#[tauri::command(async)]
fn delete_items(
    window: WebviewWindow,
    engine: Shared,
    journal: State<Journal>,
    set: u32,
    ranges: Vec<(u32, u32)>,
    permanent: bool,
) -> Result<bool, String> {
    let paths = engine.paths(set, &ranges)?;
    if paths.is_empty() {
        return Ok(false);
    }
    let outcome = fileops::execute(Request::Delete { paths, permanent }, interactive(&window))?;
    journal.record(&outcome);
    Ok(outcome.completed)
}

/// Renames an item and returns the name it ended up with, which differs from the one asked
/// for when the user resolves a conflict by keeping both. `None` if nothing was renamed.
#[tauri::command(async)]
fn rename_item(window: WebviewWindow, journal: State<Journal>, path: String, new_name: String) -> Result<Option<String>, String> {
    let outcome = fileops::execute(Request::Rename { path, new_name }, interactive(&window))?;
    journal.record(&outcome);
    Ok(outcome.results().find_map(|done| match done {
        Done::Renamed { to, .. } => Some(fileops::file_name(to).to_owned()),
        _ => None,
    }))
}

/// Creates a folder with the first unused "New folder" name and returns the name it got.
#[tauri::command(async)]
fn new_folder(window: WebviewWindow, journal: State<Journal>, parent: String) -> Result<String, String> {
    let name = fileops::unused_name(&parent, "New folder");
    let outcome = fileops::execute(Request::NewFolder { parent, name: name.clone() }, interactive(&window))?;
    journal.record(&outcome);
    Ok(outcome
        .results()
        .find_map(|done| match done {
            Done::Created { path } => Some(fileops::file_name(path).to_owned()),
            _ => None,
        })
        .unwrap_or(name))
}

/// Puts the selected rows on the clipboard as files. Returns how many.
#[tauri::command(async)]
fn copy_items(
    window: WebviewWindow,
    engine: Shared,
    set: u32,
    ranges: Vec<(u32, u32)>,
    cut: bool,
) -> Result<usize, String> {
    let paths = engine.paths(set, &ranges)?;
    if !paths.is_empty() {
        clipboard::set_files(owner(&window), &paths, cut)?;
    }
    Ok(paths.len())
}

#[tauri::command(async)]
fn can_paste() -> bool {
    clipboard::has_files()
}

/// Copies or moves the clipboard's files into `destination`. Returns the names the pasted
/// items ended up with there ("name - Copy" and conflict renames included), or an empty list
/// if there was nothing to paste or the user cancelled.
#[tauri::command(async)]
fn paste_items(window: WebviewWindow, journal: State<Journal>, destination: String) -> Result<Vec<String>, String> {
    let Some(clip) = clipboard::files(owner(&window))? else { return Ok(Vec::new()) };
    let kind = if clip.cut { Transfer::Move } else { Transfer::Copy };
    let request = Request::Transfer { sources: clip.paths, destination, kind };
    let outcome = fileops::execute(request, interactive(&window))?;
    journal.record(&outcome);
    // Cut files can only be pasted once.
    if clip.cut && outcome.completed {
        let _ = clipboard::clear(owner(&window));
    }
    Ok(outcome
        .results()
        .filter_map(|done| match done {
            Done::Copied { to, .. } | Done::Moved { to, .. } => Some(fileops::file_name(to).to_owned()),
            _ => None,
        })
        .collect())
}

/// Opens the standard Windows Properties dialog for a file or folder.
#[tauri::command(async)]
fn show_properties(window: WebviewWindow, path: String) -> Result<(), String> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let owner = owner(&window).map_or(std::ptr::null_mut(), |hwnd| hwnd as _);
    // SAFETY: `wide` is NUL-terminated and outlives the call; the shell shows the dialog on a
    // thread of its own.
    let shown = unsafe { SHObjectProperties(owner, SHOP_FILEPATH, wide.as_ptr(), null()) };
    if shown != 0 { Ok(()) } else { Err(format!("Couldn't show properties for {path}")) }
}


#[tauri::command(async)]
fn find_prefix(engine: Shared, set: u32, prefix: String, from: u32) -> Option<u32> {
    engine.find_prefix(set, &prefix, from)
}

#[tauri::command(async)]
fn places() -> places::Places {
    places::places()
}

/// Opens a file with its default application, as a double-click in Explorer would.
#[tauri::command(async)]
fn open_path(path: String) -> Result<(), String> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), wide.as_ptr(), null(), null(), SW_SHOWNORMAL)
    };
    // ShellExecute reports success as a value greater than 32.
    if result as usize > 32 {
        Ok(())
    } else if access::is_locked(&fileops::parent(&path)) {
        Err(access::LOCKED.to_owned())
    } else {
        Err(format!("Windows couldn't open {path}"))
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
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

/// Maps an `icon://` request to an icon: `/folder`, `/ext/<extension>` or `/file?p=<path>`,
/// each with an optional `s=<pixels>`.
fn parse_icon_request(uri: &tauri::http::Uri) -> Option<(IconKey, u32)> {
    let mut size = 32;
    let mut file = None;
    for pair in uri.query().unwrap_or_default().split('&') {
        match pair.split_once('=') {
            Some(("s", value)) => size = value.parse().ok()?,
            Some(("p", value)) => file = Some(percent_decode(value)),
            _ => {}
        }
    }
    let path = uri.path().trim_start_matches('/');
    let key = match path.split_once('/') {
        Some(("ext", ext)) if !ext.is_empty() => IconKey::Ext(percent_decode(ext)),
        None if path == "folder" => IconKey::Folder,
        None if path == "file" => IconKey::File(file?),
        _ => return None,
    };
    Some((key, size))
}

fn main() {
    let engine = Arc::new(Engine::default());
    let icons = Arc::new(IconService::start());
    let thumbs = ThumbService::start(icons.clone());

    tauri::Builder::default()
        .manage(engine.clone())
        .manage(Watcher::default())
        .manage(Journal::default())
        // `http://thumb.localhost/?p=<path>&s=<pixels>&m=<mtime>`: see thumbs.rs.
        .register_asynchronous_uri_scheme_protocol("thumb", move |_context, request, responder| {
            thumbs.handle(&request, responder)
        })
        .register_asynchronous_uri_scheme_protocol("icon", move |_context, request, responder| {
            let respond = move |status: StatusCode, body: Vec<u8>| {
                let response = Response::builder()
                    .status(status)
                    .header("Content-Type", "image/png")
                    .header("Cache-Control", "max-age=604800, immutable")
                    .body(body)
                    .expect("static response parts are valid");
                responder.respond(response);
            };
            match parse_icon_request(request.uri()) {
                Some((key, size)) => icons.request(key, size, move |png| match png {
                    Some(png) => respond(StatusCode::OK, png.to_vec()),
                    None => respond(StatusCode::NOT_FOUND, Vec::new()),
                }),
                None => respond(StatusCode::BAD_REQUEST, Vec::new()),
            }
        })
        .invoke_handler(tauri::generate_handler![
            index_status,
            list_dir,
            search,
            carry_marks,
            release_set,
            rows,
            sort_set,
            find_prefix,
            locate_names,
            places,
            open_path,
            delete_items,
            rename_item,
            new_folder,
            copy_items,
            can_paste,
            paste_items,
            show_properties,
            undo::undo,
            undo::redo,
            undo::undo_state,
            batchrename::preview_renames,
            batchrename::batch_rename,
            access::locked_folders,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let engine = app.state::<Arc<Engine>>().inner().clone();
            std::thread::Builder::new()
                .name("kestrel-indexer".into())
                .spawn(move || indexer::run(handle, engine))?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while starting Kestrel")
        .run(move |_app, event| {
            // Saving on the way out means the next start only replays what changed since.
            if let RunEvent::Exit = event
                && engine.take_dirty()
            {
                indexer::save(&engine, &indexer::snapshot_path());
            }
        });
}
