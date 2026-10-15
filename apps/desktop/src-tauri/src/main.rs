// Keeps a console window from opening alongside the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod engine;
mod fileops;
mod icons;
mod indexer;
mod places;
mod preview;
mod watch;

use std::collections::HashSet;
use std::ptr::null;
use std::sync::Arc;

use kestrel_index::walk;
use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewWindow};
use windows_sys::Win32::UI::Shell::{SHObjectProperties, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use engine::{Engine, IndexStatus, Listing, Marks, Row, SortSpec};
use fileops::{Options, Request, Transfer};
use icons::{IconKey, IconService};
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
/// no change can fall between the two.
#[tauri::command(async)]
fn list_dir(
    app: AppHandle,
    engine: Shared,
    watcher: State<Watcher>,
    path: String,
    sort: SortSpec,
    watch: bool,
) -> Result<Listing, String> {
    if watch {
        let path = engine::normalize(&path);
        watcher.watch(&path, move |changed| {
            let _ = app.emit("dir-changed", changed);
        });
    }
    engine.list_dir(&path, sort)
}

#[tauri::command(async)]
fn search(engine: Shared, query: String, scope: Option<String>, sort: SortSpec) -> Result<Listing, String> {
    engine.search(&query, scope.as_deref(), sort)
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
    set: u32,
    ranges: Vec<(u32, u32)>,
    permanent: bool,
) -> Result<bool, String> {
    let paths = engine.paths(set, &ranges)?;
    if paths.is_empty() {
        return Ok(false);
    }
    fileops::perform(Request::Delete { paths, permanent }, interactive(&window))
}

#[tauri::command(async)]
fn rename_item(window: WebviewWindow, path: String, new_name: String) -> Result<bool, String> {
    fileops::perform(Request::Rename { path, new_name }, interactive(&window))
}

/// Creates a folder with the first unused "New folder" name and returns that name.
#[tauri::command(async)]
fn new_folder(window: WebviewWindow, parent: String) -> Result<String, String> {
    let name = fileops::unused_name(&parent, "New folder");
    fileops::perform(Request::NewFolder { parent, name: name.clone() }, interactive(&window))?;
    Ok(name)
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

/// Copies or moves the clipboard's files into `destination`. Returns the names of the items
/// pasted, or an empty list if there was nothing to paste or the user cancelled.
#[tauri::command(async)]
fn paste_items(window: WebviewWindow, destination: String) -> Result<Vec<String>, String> {
    let Some(clip) = clipboard::files(owner(&window))? else { return Ok(Vec::new()) };
    let source_names: Vec<String> = clip
        .paths
        .iter()
        .filter_map(|path| std::path::Path::new(path).file_name()?.to_str().map(str::to_owned))
        .collect();
    let names_in = |dir: &str| -> HashSet<String> {
        walk::list(dir).map(|items| items.into_iter().map(|item| item.name).collect()).unwrap_or_default()
    };
    let before = names_in(&destination);

    let kind = if clip.cut { Transfer::Move } else { Transfer::Copy };
    let request = Request::Transfer { sources: clip.paths, destination: destination.clone(), kind };
    if !fileops::perform(request, interactive(&window))? {
        return Ok(Vec::new());
    }
    // Cut files can only be pasted once.
    if clip.cut {
        let _ = clipboard::clear(owner(&window));
    }

    // Duplicating in place gives the copies new names ("name - Copy"), which only a
    // before-and-after comparison reveals. When nothing new appeared, existing items were
    // replaced, and those carry the source names.
    let created: Vec<String> = names_in(&destination).into_iter().filter(|name| !before.contains(name)).collect();
    Ok(if created.is_empty() { source_names } else { created })
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
    if result as usize > 32 { Ok(()) } else { Err(format!("Windows couldn't open {path}")) }
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
    let icons = IconService::start();

    tauri::Builder::default()
        .manage(engine.clone())
        .manage(Watcher::default())
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
        // File bytes for the preview pane; see `preview.rs` for why it is safe to expose.
        .register_asynchronous_uri_scheme_protocol("preview", preview::protocol)
        // Commands are refused to frames other than the app's own, such as previewed content.
        .invoke_handler(preview::guard(tauri::generate_handler![
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
            preview::preview_text,
            preview::preview_details,
            preview::open_link,
        ]))
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
