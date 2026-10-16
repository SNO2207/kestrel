// Keeps a console window from opening alongside the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod access;
mod address;
mod batchrename;
mod clipboard;
mod dragdrop;
mod engine;
mod fileops;
mod foldertree;
mod icons;
mod indexer;
mod instance;
mod launcher;
mod layout;
mod lru;
mod navigation;
mod palette;
mod parity;
mod places;
mod preview;
mod previewview;
mod projects;
mod recycle;
mod recyclebin;
mod shellmenu;
mod shortcuts;
mod thispc;
mod thumbs;
mod undo;
mod visibility;
mod watch;

use std::ptr::null;
use std::sync::Arc;

use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, Window};
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
/// `show_hidden` is false, and protected operating system files (hidden and system both) when
/// `show_protected` is false too. Each window watches its own folder.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn list_dir(
    app: AppHandle,
    window: Window,
    engine: Shared,
    watcher: State<Watcher>,
    path: String,
    sort: SortSpec,
    watch: bool,
    show_hidden: Option<bool>,
    show_protected: Option<bool>,
) -> Result<Listing, String> {
    if watch {
        let path = engine::normalize(&path);
        // Each window watches the folder its tab on screen shows (`parity::appwindows`).
        watcher.watch_for(window.label(), &path, move |changed| {
            let _ = app.emit("dir-changed", changed);
        });
    }
    engine.list_dir_with(&path, sort, Visibility::from_request(show_hidden).with_protected(show_protected))
}

/// Runs a search. `tag:` clauses are resolved to the items carrying the tag.
#[tauri::command(async)]
fn search(
    engine: Shared,
    db: shortcuts::Data,
    query: String,
    scope: Option<String>,
    sort: SortSpec,
    show_hidden: Option<bool>,
    show_protected: Option<bool>,
) -> Result<Listing, String> {
    let visibility = Visibility::from_request(show_hidden).with_protected(show_protected);
    engine.search_resolving(&query, scope.as_deref(), sort, visibility, |query, index| {
        shortcuts::tags::resolve(&db, query, index)
    })
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
///
/// Commands take the `Window`, never a `WebviewWindow`: the main window also holds the
/// preview's own webview (`previewview.rs`), and Tauri refuses to hand a command a
/// `WebviewWindow` for a window with more than one webview.
fn owner(window: &Window) -> Option<isize> {
    window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
}

fn interactive(window: &Window) -> Options {
    Options { owner: owner(window), quiet: false }
}

/// Sends the selected rows to the Recycle Bin, or erases them when `permanent`. Returns
/// `false` if the user cancelled.
#[tauri::command(async)]
fn delete_items(
    window: Window,
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
fn rename_item(window: Window, journal: State<Journal>, path: String, new_name: String) -> Result<Option<String>, String> {
    let outcome = fileops::execute(Request::Rename { path, new_name }, interactive(&window))?;
    journal.record(&outcome);
    Ok(outcome.results().find_map(|done| match done {
        Done::Renamed { to, .. } => Some(fileops::file_name(to).to_owned()),
        _ => None,
    }))
}

/// Creates a folder with the first unused "New folder" name and returns the name it got.
#[tauri::command(async)]
fn new_folder(window: Window, journal: State<Journal>, parent: String) -> Result<String, String> {
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
    window: Window,
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
fn paste_items(window: Window, journal: State<Journal>, destination: String) -> Result<Vec<String>, String> {
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
fn show_properties(window: Window, path: String) -> Result<(), String> {
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

/// Opens a file with its default application, as a double-click in Explorer would, and
/// remembers it among Home's recent files (`parity::recent`).
#[tauri::command(async)]
fn open_path(db: State<Arc<kestrel_db::Db>>, path: String) -> Result<(), String> {
    open_with_shell(&path)?;
    if !std::path::Path::new(&path).is_dir() {
        let _ = kestrel_db::parity::record_opened(&db, &path, parity::now_ms());
    }
    Ok(())
}

fn open_with_shell(path: &str) -> Result<(), String> {
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

/// Kestrel's own data (pins, projects, settings). Commands take it as `State<Arc<Db>>`. If the
/// file cannot be opened the app still runs, on a temporary in-memory database, and says so on
/// stderr: browsing must never depend on it.
fn open_database() -> Arc<kestrel_db::Db> {
    let path = indexer::data_dir().join("kestrel.db");
    let db = kestrel_db::Db::open(&path).unwrap_or_else(|error| {
        eprintln!("could not open {}: {error}; using a temporary database", path.display());
        kestrel_db::Db::open_in_memory().expect("an in-memory database always opens")
    });
    Arc::new(db)
}

fn main() {
    // Before anything touches the data folder: one Kestrel per folder (see instance.rs).
    let background = std::env::args().any(|arg| arg == launcher::autostart::BACKGROUND_ARG);
    let instance = match instance::claim(&indexer::data_dir(), !background) {
        instance::Claim::First(instance) => instance,
        instance::Claim::Running => return,
    };
    let engine = Arc::new(Engine::default());
    let icons = Arc::new(IconService::start());
    let thumbs = ThumbService::start(icons.clone());
    let database = open_database();
    let projects = projects::Projects::new(engine.clone(), database.clone());
    // The command palette's providers; other features register theirs in `setup` (palette.rs).
    let palette = Arc::new(palette::Palette::new(database.clone()));
    palette.register(Arc::new(palette::files::FilesProvider::new(engine.clone())));
    palette.register(Arc::new(palette::paths::PathsProvider::default()));
    // `@` projects, and the shortcuts (pins, frequent folders, saved searches, workspaces, `#` tags).
    palette.register(Arc::new(projects::ProjectsProvider::new(projects.clone())));
    shortcuts::provider::register(&palette, &database);

    tauri::Builder::default()
        // The window never leaves the app's own page; see navigation.rs.
        .plugin(navigation::guard())
        // The quick launcher's system-wide shortcut; see launcher.rs.
        .plugin(launcher::shortcut_plugin())
        .manage(engine.clone())
        .manage(Watcher::default())
        // The second pane's folder is watched too (layout.rs).
        .manage(layout::OtherPaneWatcher::default())
        // The undo history is kept in the database, and brought back in `setup`.
        .manage(Journal::with_store(database.clone()))
        .manage(database.clone())
        .manage(projects.clone())
        .manage(palette)
        // This PC's drives, each asked about on a thread of its own (thispc.rs).
        .manage(Arc::new(thispc::Drives::default()))
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
            undo::undo,
            undo::redo,
            undo::undo_state,
            batchrename::preview_renames,
            batchrename::batch_rename,
            access::locked_folders,
            preview::preview_text,
            preview::preview_details,
            preview::open_link,
            previewview::preview_surface,
            previewview::preview_surface_return_focus,
            dragdrop::start_drag,
            dragdrop::drop_items,
            dragdrop::drop_effect,
            shellmenu::shell_menu,
            shellmenu::shell_folder_menu,
            shellmenu::open_with,
            shellmenu::shell_probe,
            shellmenu::shell_probe_take,
            shortcuts::pins::pins_board,
            shortcuts::pins::pins_add,
            shortcuts::pins::pins_remove,
            shortcuts::pins::pins_set_alias,
            shortcuts::pins::pins_set_key,
            shortcuts::pins::pins_move,
            shortcuts::pins::pins_locate,
            shortcuts::pins::pins_create_group,
            shortcuts::pins::pins_update_group,
            shortcuts::pins::pins_move_group,
            shortcuts::pins::pins_delete_group,
            shortcuts::frecency::frecency_visit,
            shortcuts::frecency::frecency_top,
            shortcuts::frecency::frecency_forget,
            shortcuts::goto::goto_folders,
            shortcuts::searches::searches_list,
            shortcuts::searches::searches_save,
            shortcuts::searches::searches_rename,
            shortcuts::searches::searches_delete,
            shortcuts::tags::tags_state,
            shortcuts::tags::tags_create,
            shortcuts::tags::tags_update,
            shortcuts::tags::tags_delete,
            shortcuts::tags::tags_assign,
            shortcuts::workspaces::workspaces_list,
            shortcuts::workspaces::workspaces_save,
            shortcuts::workspaces::workspaces_rename,
            shortcuts::workspaces::workspaces_delete,
            projects::projects_list,
            projects::project_detail,
            projects::project_set_status,
            projects::project_set_tags,
            projects::project_set_notes,
            projects::project_set_pinned,
            projects::project_set_dismissed,
            projects::project_launch,
            projects::project_run_script,
            projects::git_badges,
            projects::projects_probe,
            projects::projects_probe_take,
            palette::palette_query,
            palette::palette_record,
            palette::palette_forget,
            launcher::launcher_settings,
            launcher::set_launcher_hotkey,
            launcher::set_keep_in_tray,
            launcher::set_start_with_windows,
            launcher::show_main_window,
            launcher::hide_quick_window,
            launcher::quit_app,
            launcher::launcher_dev_summon,
            launcher::launcher_dev_hotkey,
            launcher::launcher_dev_windows,
            launcher::launcher_dev_tray,
            thispc::this_pc,
            thispc::eject_drive,
            shellmenu::shell_paths_menu,
            recyclebin::recycle_bin_items,
            recyclebin::recycle_bin_stamp,
            recyclebin::recycle_bin_restore,
            recyclebin::recycle_bin_delete,
            recyclebin::recycle_bin_empty,
            foldertree::tree_children,
            foldertree::tree_reveal,
            foldertree::tree_subtree,
            foldertree::sidebar_state,
            foldertree::set_sidebar_section,
            foldertree::set_expand_to_open_folder,
            address::address_resolve,
            address::address_suggest,
            layout::group_set,
            layout::group_headers,
            layout::filter_set,
            layout::transfer_items,
            layout::image_sizes,
            layout::layout_load,
            layout::layout_save,
            layout::watch_other_pane,
            layout::watch_active_folder,
            layout::folders_exist,
            parity::shellnew::new_menu,
            parity::shellnew::new_file,
            parity::links::new_shortcut,
            parity::links::shortcut_suggestion,
            parity::links::shortcut_target_dev,
            parity::recent::home_recent,
            parity::recent::record_opened,
            parity::recent::forget_recent,
            parity::terminal::open_in_terminal,
            parity::details::item_properties,
            parity::details::selection_summary,
            parity::syncroots::sync_roots_list,
            parity::appwindows::new_window,
            parity::appwindows::window_start,
            parity::appwindows::app_windows_dev,
        ]))
        .on_window_event(|window, event| {
            dragdrop::on_window_event(event);
            parity::appwindows::on_window_event(window, event);
            launcher::on_window_event(window, event);
        })
        .setup(move |app| {
            if let Some(window) = app.get_window("main")
                && let Err(error) = dragdrop::install(&window)
            {
                eprintln!("Dragging files out is unavailable: {error}");
            }
            let restoring = app.handle().clone();
            std::thread::Builder::new()
                .name("kestrel-undo-restore".into())
                .spawn(move || restoring.state::<Journal>().restore())?;
            projects.start(app.handle().clone());
            // The quick launcher, its shortcut and the tray icon; never fatal.
            launcher::setup(app.handle(), database.clone());
            // Launching Kestrel again shows this one, wherever it is hiding.
            let showing = app.handle().clone();
            instance.listen(move || {
                let _ = launcher::show_main(&showing);
            });
            // Places: drives coming and going, late drive details, and index changes the
            // sidebar's folder tree follows.
            thispc::watch(app.handle().clone());
            let late = app.handle().clone();
            app.state::<Arc<thispc::Drives>>().on_late(move |drive| {
                let _ = late.emit("drive-details", drive);
            });
            foldertree::watch(app.handle().clone(), app.state::<Arc<Engine>>().inner().clone());
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
