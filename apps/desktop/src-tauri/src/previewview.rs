//! The preview's own webview. Images, video, audio and PDF documents are shown in a second
//! webview laid over the preview pane or Quick Look, never in the app's page.
//!
//! Why: WebView2 injects Tauri's IPC bridge into every frame, and Tauri counts every custom
//! protocol host, `preview.localhost` included, as part of the app. Content served from there
//! inside the app's page (the PDF viewer's frame, above all) could therefore reach whatever the
//! page's webview may call. `preview::guard` kept the app's own commands from it, but Tauri's
//! plugin commands (window, event, app, webview) are granted per webview by capabilities and
//! never pass through the app's handler. This webview has no capability at all
//! (`capabilities/default.json` names only the app's webview, `main`), `preview::guard` refuses
//! the app's commands to any webview but `main`, and it may only ever show `preview.localhost`
//! or a blank page (here and in `navigation.rs`). Nothing shown in it can call anything.
//!
//! The front end owns the layout: it reports where the preview area is, in CSS pixels of the
//! app's page, which are the window's logical pixels, and this module places the webview there.
//! The webview is made the first time it is needed and kept: hidden, and emptied so that no
//! media keeps playing, when nothing is previewed.

use parking_lot::Mutex;
use serde::Deserialize;
use tauri::webview::NewWindowResponse;
use tauri::{LogicalPosition, LogicalSize, Manager, Runtime, Url, Webview, WebviewBuilder, WebviewUrl, Window};

/// The preview webview's label. Capabilities and `preview::guard` grant nothing to it.
pub const LABEL: &str = "preview";
/// The page the webview rests on while nothing is previewed.
const BLANK: &str = "about:blank";

/// Where the preview area is and what it shows.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Placement {
    /// A `preview.localhost` page: a file itself (PDF) or the protocol's viewer for it.
    pub url: String,
    /// The area, in CSS pixels of the app's page (the window's logical pixels).
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// False while something of the app's own, such as a menu, lies over the area: the webview
    /// is drawn above the page, so it steps aside rather than hide the menu.
    pub visible: bool,
}

/// The URL the webview was last sent to; navigating again to the same one would restart it.
static SHOWN: Mutex<Option<String>> = Mutex::new(None);
/// Placements are applied one at a time, so two can never both create the webview.
static APPLYING: Mutex<()> = Mutex::new(());

/// Whether the preview webview may be at `url`: a page of the `preview` protocol, or blank.
pub fn allowed(url: &Url) -> bool {
    url.as_str() == BLANK
        || (url.scheme() == "http" && url.host_str() == Some("preview.localhost") && url.port().is_none() && url.username().is_empty())
}

fn checked(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|_| format!("Not a preview address: {url}"))?;
    if allowed(&parsed) { Ok(parsed) } else { Err(format!("The preview can't show {url}")) }
}

/// The preview webview, made in `window` if it does not exist yet.
fn ensure<R: Runtime>(window: &Window<R>, at: &Placement) -> Result<Webview<R>, String> {
    if let Some(webview) = window.app_handle().get_webview(LABEL) {
        return Ok(webview);
    }
    let blank = Url::parse(BLANK).map_err(|e| e.to_string())?;
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(blank))
        // Clicking into a preview should not be the only way back to the list's keys; the
        // webview takes the focus only when the user clicks into it.
        .focused(false)
        .on_navigation(allowed)
        // Links in a PDF that ask for a new window, and anything else that would, get nothing.
        .on_new_window(|_, _| NewWindowResponse::Deny);
    // `add_child` waits for the window's thread, so it must not be called from it; commands
    // that reach here are `async` and run on a pool thread.
    let webview = window
        .add_child(builder, LogicalPosition::new(at.x, at.y), LogicalSize::new(at.width.max(1.0), at.height.max(1.0)))
        .map_err(|e| format!("Couldn't make the preview's view: {e}"))?;
    *SHOWN.lock() = Some(BLANK.to_owned());
    Ok(webview)
}

/// Shows `placement` in the preview webview, or with `None` empties and hides it.
pub fn apply<R: Runtime>(window: &Window<R>, placement: Option<Placement>) -> Result<(), String> {
    let _one_at_a_time = APPLYING.lock();
    let Some(at) = placement else {
        if let Some(webview) = window.app_handle().get_webview(LABEL) {
            let _ = webview.hide();
            let mut shown = SHOWN.lock();
            if shown.as_deref() != Some(BLANK) {
                // Stops a video or sound that was playing, and lets go of the file.
                webview.navigate(checked(BLANK)?).map_err(|e| e.to_string())?;
                *shown = Some(BLANK.to_owned());
            }
        }
        return Ok(());
    };
    let url = checked(&at.url)?;
    let webview = ensure(window, &at)?;
    webview
        .set_position(LogicalPosition::new(at.x, at.y))
        .and_then(|()| webview.set_size(LogicalSize::new(at.width.max(1.0), at.height.max(1.0))))
        .map_err(|e| e.to_string())?;
    {
        let mut shown = SHOWN.lock();
        if shown.as_deref() != Some(at.url.as_str()) {
            webview.navigate(url).map_err(|e| e.to_string())?;
            *shown = Some(at.url.clone());
        }
    }
    if at.visible { webview.show() } else { webview.hide() }.map_err(|e| e.to_string())
}

/// Places the preview webview over the preview area and shows `url` in it, or with `None`
/// empties and hides it. Only the app's own page may call this (`preview::guard`).
#[tauri::command(async)]
pub fn preview_surface(window: Window, placement: Option<Placement>) -> Result<(), String> {
    apply(&window, placement)
}

/// Gives the keyboard back to the app's page, which lost it to the preview webview: a click on
/// a previewed image has nothing to do there, and would otherwise leave the list's keys and
/// Quick Look's Space and Escape dead until the user clicked the page again. Nothing happens
/// if the window is no longer in front, since then the user went elsewhere.
#[tauri::command(async)]
pub fn preview_surface_return_focus(window: Window) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let ours = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    // SAFETY: plain FFI call without arguments.
    let foreground = unsafe { GetForegroundWindow() } as isize;
    if foreground != ours {
        return Ok(());
    }
    let page = window.app_handle().get_webview(crate::preview::APP_WEBVIEW).ok_or("The app's page is gone")?;
    page.set_focus().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_the_preview_protocol() {
        for good in ["about:blank", "http://preview.localhost/?p=C%3A%5Ca.pdf&v=1", "http://preview.localhost/view?kind=image&p=x"] {
            assert!(allowed(&Url::parse(good).unwrap()), "{good}");
        }
        for bad in [
            "https://preview.localhost/?p=x",
            "http://preview.localhost:8080/?p=x",
            "http://user@preview.localhost/",
            "http://localhost:1460/",
            "http://tauri.localhost/",
            "http://icon.localhost/folder",
            "file:///C:/Windows/win.ini",
            "https://example.com/",
            "data:text/html,<script>1</script>",
            "javascript:alert(1)",
        ] {
            assert!(!allowed(&Url::parse(bad).unwrap()), "{bad}");
        }
        assert!(checked("not a url").is_err());
    }
}
