//! More than one window, as Explorer's Ctrl+N opens: each is another webview window in the same
//! process, sharing the index, the database, the undo history and the quick launcher.
//!
//! The first window is `main`; the others are `main-2`, `main-3`... and their pages carry their
//! label in the address (`index.html?window=main-2`), which the end-to-end harness reads to
//! tell them apart. Every rule that names `main` holds for them too:
//!
//! - capabilities name the webviews `main` and `main-*`, and `preview::guard` admits exactly
//!   those (and the quick launcher);
//! - each window has its own preview webview, `preview` for `main` and `preview-2` for
//!   `main-2`, held to the preview protocol by `navigation.rs` like the first;
//! - each window's folder watch is its own (`watch.rs`), and so is its drag hook
//!   (`dragdrop::install`, run on the window's thread when it is made);
//! - the quick launcher, the tray's Show Kestrel and a second launch of Kestrel bring forward
//!   the window used last, and closing a window ends Kestrel only when it was the last one
//!   (`launcher.rs`).

use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;
use tauri::{AppHandle, LogicalSize, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent};

/// The first window's label.
pub const MAIN: &str = crate::launcher::MAIN;
const PREFIX: &str = "main-";
const PREVIEW: &str = crate::previewview::LABEL;
/// Where a new window opens relative to the one it was opened from, in logical pixels.
const CASCADE: f64 = 32.0;

/// The number the next window gets.
static NEXT: AtomicU32 = AtomicU32::new(2);
/// The app window that had the focus last.
static LAST_FOCUSED: Mutex<Option<String>> = Mutex::new(None);
/// Folders new windows start in, by label, until their page asks.
static STARTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn numbered(label: &str, prefix: &str) -> bool {
    label.strip_prefix(prefix).is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `label` names one of the app's own windows (and its page's webview).
pub fn is_app_window(label: &str) -> bool {
    label == MAIN || numbered(label, PREFIX)
}

/// The label of the preview webview inside the app window `window`.
pub fn preview_label(window: &str) -> String {
    match window.strip_prefix(PREFIX) {
        Some(number) => format!("{PREVIEW}-{number}"),
        None => PREVIEW.to_owned(),
    }
}

/// Whether `label` names a preview webview, which may only show the preview protocol.
pub fn is_preview_label(label: &str) -> bool {
    label == PREVIEW || numbered(label, &format!("{PREVIEW}-"))
}

/// The app's windows, the first one first.
pub fn app_windows<R: Runtime>(app: &AppHandle<R>) -> Vec<Window<R>> {
    let mut windows: Vec<Window<R>> = app.windows().into_values().filter(|window| is_app_window(window.label())).collect();
    let order = |label: &str| label.strip_prefix(PREFIX).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
    windows.sort_by_key(|window| order(window.label()));
    windows
}

/// The window to bring forward or hand things to: the one used last, else the first.
pub fn target<R: Runtime>(app: &AppHandle<R>) -> Option<Window<R>> {
    let last = LAST_FOCUSED.lock().clone();
    last.and_then(|label| app.get_window(&label)).or_else(|| app_windows(app).into_iter().next())
}

/// Keeps track of the window used last, and forgets a window's state once it is gone.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let label = window.label();
    if !is_app_window(label) {
        return;
    }
    match event {
        WindowEvent::Focused(true) => *LAST_FOCUSED.lock() = Some(label.to_owned()),
        WindowEvent::Destroyed => {
            let mut last = LAST_FOCUSED.lock();
            if last.as_deref() == Some(label) {
                *last = None;
            }
            drop(last);
            STARTS.lock().retain(|(waiting, _)| waiting != label);
            window.app_handle().state::<crate::watch::Watcher>().forget(label);
            crate::previewview::forget(label);
        }
        _ => {}
    }
}

// ---------- commands ----------

/// Opens another window, in `path` if given, the size of `window` and a little below and to the
/// right of it, as Explorer's Ctrl+N does. Returns its label.
#[tauri::command(async)]
pub fn new_window(app: AppHandle, window: Window, path: Option<String>) -> Result<String, String> {
    let label = format!("{PREFIX}{}", NEXT.fetch_add(1, Ordering::Relaxed));
    if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
        STARTS.lock().push((label.clone(), path));
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = window.inner_size().map_or(LogicalSize::new(1280.0, 800.0), |size| size.to_logical::<f64>(scale));
    let url = WebviewUrl::App(format!("index.html?window={label}").into());
    let mut builder = WebviewWindowBuilder::new(&app, &label, url)
        .title("Kestrel")
        .inner_size(size.width, size.height)
        .min_inner_size(720.0, 420.0)
        .decorations(false)
        .shadow(true);
    if let Ok(at) = window.outer_position() {
        let at = at.to_logical::<f64>(scale);
        builder = builder.position(at.x + CASCADE, at.y + CASCADE);
    }
    builder.build().map_err(|error| format!("Couldn't open another window: {error}"))?;
    // Dragging files out needs a hook on the window, set on the thread that made it.
    let handle = app.clone();
    let made = label.clone();
    app.run_on_main_thread(move || {
        if let Some(window) = handle.get_window(&made)
            && let Err(error) = crate::dragdrop::install(&window)
        {
            eprintln!("Dragging files out of {made} is unavailable: {error}");
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(label)
}

/// The folder this window was opened to show, asked once by its page as it starts.
#[tauri::command]
pub fn window_start(window: Window) -> Option<String> {
    let mut starts = STARTS.lock();
    let at = starts.iter().position(|(label, _)| label == window.label())?;
    Some(starts.remove(at).1)
}

/// Development builds only: the labels of the app's windows, for the end-to-end tests.
#[tauri::command(async)]
pub fn app_windows_dev(app: AppHandle) -> Result<Vec<String>, String> {
    if !cfg!(debug_assertions) {
        return Err("Only development builds have this hook".into());
    }
    Ok(app_windows(&app).iter().map(|window| window.label().to_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_app_windows_and_their_previews() {
        for label in ["main", "main-2", "main-17"] {
            assert!(is_app_window(label), "{label}");
        }
        for label in ["quick", "preview", "main-", "main-x", "mainly", "preview-2", "main-2-preview"] {
            assert!(!is_app_window(label), "{label}");
        }
        assert_eq!(preview_label("main"), "preview");
        assert_eq!(preview_label("main-3"), "preview-3");
        assert!(is_preview_label("preview") && is_preview_label("preview-3"));
        assert!(!is_preview_label("main") && !is_preview_label("preview-") && !is_preview_label("previews"));
    }
}
