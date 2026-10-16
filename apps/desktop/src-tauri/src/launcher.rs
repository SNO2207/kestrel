//! Kestrel in the background: the quick launcher, its system-wide shortcut, the notification
//! area icon, and the option to keep running there when the window is closed.
//!
//! - The quick launcher is a second, small window (`quick`, served from `quick.html`) with the
//!   same palette as Ctrl+K. It is made hidden at startup, so summoning it only shows it: on
//!   the monitor under the mouse, above every other window. Enter acts and hides it, Escape or
//!   clicking elsewhere hides it. What it opens it hands to the main window (`palette-run`),
//!   except files, which it opens itself.
//! - The shortcut (Ctrl+Alt+Space unless changed in the settings dialog, and stored in the
//!   database) is registered with Windows through the global shortcut plugin. Another program
//!   may already own the keys; that is reported in the dialog and is never fatal. Test
//!   instances run side by side, so `KESTREL_GLOBAL_HOTKEY=off` (which `e2e/dev.mjs` sets)
//!   leaves the shortcut unregistered; development builds have commands that summon the
//!   window and register a shortcut directly, for the end-to-end tests.
//! - The tray icon offers Show Kestrel, the quick launcher, the keep-running option, the
//!   settings and Quit. With "Keep running in the tray" on (it is off by default), closing the
//!   window hides it, so the index stays current and the shortcut keeps working.

pub mod autostart;
pub mod hotkey;
mod tray;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kestrel_db::Db;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// The app's own window.
pub const MAIN: &str = "main";
/// The quick launcher's window.
pub const QUICK: &str = "quick";
/// Set to `off` to leave the system-wide shortcut unregistered in this instance.
pub const HOTKEY_ENV: &str = "KESTREL_GLOBAL_HOTKEY";
const HOTKEY_SETTING: &str = "launcher.hotkey";
const KEEP_SETTING: &str = "launcher.keep_in_tray";
/// The quick launcher's size, in logical pixels.
const QUICK_SIZE: (f64, f64) = (720.0, 460.0);

/// Events the windows exchange.
pub mod events {
    /// To the quick window, each time it is shown: start afresh.
    pub const SHOWN: &str = "launcher-shown";
    /// To the main window: open the launcher settings dialog.
    pub const SETTINGS: &str = "launcher-settings";
}

/// The shortcut's registration, as the settings dialog shows it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HotkeyStatus {
    /// The shortcut chosen, as stored, or `None` if the user turned it off.
    pub shortcut: Option<String>,
    /// The shortcut registered with Windows now, if any.
    pub registered: Option<String>,
    /// `registered`, `failed` (see `message`), `disabled` (by `KESTREL_GLOBAL_HOTKEY=off`) or
    /// `off` (no shortcut chosen).
    pub state: &'static str,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Settings {
    pub hotkey: HotkeyStatus,
    pub default_hotkey: &'static str,
    pub keep_in_tray: bool,
    pub start_with_windows: bool,
}

struct Registration {
    /// The shortcut chosen, as stored.
    wanted: Option<String>,
    /// The shortcut registered now.
    active: Option<Shortcut>,
    /// Why the last attempt to register failed.
    error: Option<String>,
}

/// Managed by Tauri as `State<Launcher>`.
pub struct Launcher {
    db: Arc<Db>,
    disabled: bool,
    keep_in_tray: AtomicBool,
    registration: Mutex<Registration>,
    /// The quick window has had the focus since it was shown, so losing it means the user
    /// went elsewhere and the window should go too.
    quick_focused: AtomicBool,
    exiting: AtomicBool,
    tray: Mutex<Option<tray::Tray>>,
}

impl Launcher {
    pub fn new(db: Arc<Db>) -> Self {
        let disabled = std::env::var(HOTKEY_ENV).is_ok_and(|value| value.eq_ignore_ascii_case("off"));
        // Never set means the default; a stored null means the user turned the shortcut off.
        let wanted = match db.setting::<Option<String>>(HOTKEY_SETTING) {
            Ok(Some(stored)) => stored.and_then(|text| hotkey::normalize(&text)),
            _ => Some(hotkey::DEFAULT.to_owned()),
        };
        let keep = db.setting::<bool>(KEEP_SETTING).ok().flatten().unwrap_or(false);
        Self {
            db,
            disabled,
            keep_in_tray: AtomicBool::new(keep),
            registration: Mutex::new(Registration { wanted, active: None, error: None }),
            quick_focused: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            tray: Mutex::new(None),
        }
    }

    pub fn keeps_running(&self) -> bool {
        self.keep_in_tray.load(Ordering::Relaxed)
    }

    /// Registers the chosen shortcut, unless this instance runs with it turned off.
    fn register_wanted(&self, app: &AppHandle) {
        let mut registration = self.registration.lock();
        if self.disabled {
            return;
        }
        if let Some(text) = registration.wanted.clone() {
            let result = hotkey::parse(&text).and_then(|shortcut| register(app, &shortcut).map(|()| shortcut));
            match result {
                Ok(shortcut) => registration.active = Some(shortcut),
                Err(error) => {
                    eprintln!("the quick launcher's shortcut is unavailable: {error}");
                    registration.error = Some(error);
                }
            }
        }
    }

    pub fn status(&self, app: &AppHandle) -> HotkeyStatus {
        let registration = self.registration.lock();
        let registered = registration
            .active
            .filter(|shortcut| app.global_shortcut().is_registered(*shortcut))
            .map(|shortcut| hotkey::display(&shortcut));
        let (state, message) = match (&registered, &registration.error) {
            (Some(_), _) => ("registered", None),
            (None, Some(error)) => ("failed", Some(error.clone())),
            (None, None) if self.disabled => (
                "disabled",
                Some(format!("The shortcut is turned off for this instance ({HOTKEY_ENV}=off).")),
            ),
            (None, None) => ("off", None),
        };
        HotkeyStatus { shortcut: registration.wanted.clone(), registered, state, message }
    }

    /// Changes the shortcut. The new one is registered first and kept only if Windows accepts
    /// it; otherwise the old one stays registered and the status says why.
    pub fn set_hotkey(&self, app: &AppHandle, text: Option<&str>) -> Result<HotkeyStatus, String> {
        let wanted = text.map(str::trim).filter(|text| !text.is_empty()).map(hotkey::parse).transpose()?;
        {
            let mut registration = self.registration.lock();
            if !self.disabled {
                let previous = registration.active.take();
                if let Some(old) = previous {
                    let _ = app.global_shortcut().unregister(old);
                }
                if let Some(new) = wanted {
                    if let Err(error) = register(app, &new) {
                        // Keep the one that worked.
                        if let Some(old) = previous
                            && register(app, &old).is_ok()
                        {
                            registration.active = Some(old);
                        }
                        drop(registration);
                        let mut status = self.status(app);
                        status.state = "failed";
                        status.message = Some(error);
                        return Ok(status);
                    }
                    registration.active = Some(new);
                }
            }
            registration.error = None;
            registration.wanted = wanted.map(|shortcut| hotkey::display(&shortcut));
            self.db.set_setting(HOTKEY_SETTING, &registration.wanted)?;
        }
        Ok(self.status(app))
    }

    /// For the end-to-end tests: registers `text` as the active shortcut, or unregisters the
    /// active one with `None`, even in an instance with the shortcut turned off, and without
    /// storing anything.
    fn set_active_for_test(&self, app: &AppHandle, text: Option<&str>) -> Result<HotkeyStatus, String> {
        let shortcut = text.map(hotkey::parse).transpose()?;
        let mut registration = self.registration.lock();
        if let Some(old) = registration.active.take() {
            app.global_shortcut().unregister(old).map_err(|error| error.to_string())?;
        }
        registration.error = None;
        if let Some(new) = shortcut {
            match register(app, &new) {
                Ok(()) => registration.active = Some(new),
                Err(error) => registration.error = Some(error),
            }
        }
        drop(registration);
        Ok(self.status(app))
    }

    pub fn set_keep_in_tray(&self, keep: bool) -> Result<(), String> {
        self.db.set_setting(KEEP_SETTING, &keep)?;
        self.keep_in_tray.store(keep, Ordering::Relaxed);
        // A copy, so the lock is not held while the menu item waits for the main thread.
        let tray = self.tray.lock().clone();
        if let Some(tray) = tray {
            tray.show_keep(keep);
        }
        Ok(())
    }

    fn settings(&self, app: &AppHandle) -> Settings {
        Settings {
            hotkey: self.status(app),
            default_hotkey: hotkey::DEFAULT,
            keep_in_tray: self.keeps_running(),
            start_with_windows: autostart::is_enabled(&app.config().identifier),
        }
    }
}

/// Registers a shortcut with Windows, with a message people can act on when it fails.
fn register(app: &AppHandle, shortcut: &Shortcut) -> Result<(), String> {
    app.global_shortcut().register(*shortcut).map_err(|error| {
        let error = error.to_string();
        if hotkey::taken_elsewhere(&error) {
            format!("{} is in use by another app. Choose another shortcut.", hotkey::display(shortcut))
        } else {
            format!("Windows didn't accept {}: {error}", hotkey::display(shortcut))
        }
    })
}

/// The global shortcut plugin, calling [`summon`] whenever the registered shortcut is pressed.
pub fn shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .build()
}

/// Sets everything up: run from the app's `setup`, after the main window exists. Nothing here
/// may stop the app from starting: a failure is reported and the rest carries on.
pub fn setup(app: &AppHandle, db: Arc<Db>) {
    let launcher = Launcher::new(db);
    app.manage(launcher);
    let launcher = app.state::<Launcher>();
    if let Err(error) = create_quick_window(app) {
        eprintln!("the quick launcher is unavailable: {error}");
    }
    launcher.register_wanted(app);
    match tray::Tray::install(app, launcher.keeps_running()) {
        Ok(tray) => *launcher.tray.lock() = Some(tray),
        Err(error) => eprintln!("the tray icon is unavailable: {error}"),
    }
    // Started at sign-in with the tray option on: stay out of the way until asked for.
    if launcher.keeps_running()
        && std::env::args().any(|arg| arg == autostart::BACKGROUND_ARG)
        && let Some(main) = app.get_window(MAIN)
    {
        let _ = main.hide();
    }
}

fn create_quick_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(app, QUICK, WebviewUrl::App("quick.html".into()))
        .title("Kestrel quick launcher")
        .inner_size(QUICK_SIZE.0, QUICK_SIZE.1)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(true)
        .visible(false)
        .focused(false)
        .build()
}

/// Shows the quick launcher on the monitor under the mouse and gives it the keyboard.
pub fn summon(app: &AppHandle) {
    let Some(window) = app.get_webview_window(QUICK) else { return };
    place(app, &window);
    // Already in front, it gets no new focus event, and keeps hiding when it loses the focus.
    let focused = window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
    app.state::<Launcher>().quick_focused.store(focused, Ordering::Relaxed);
    let _ = window.emit_to(QUICK, events::SHOWN, ());
    let _ = window.show();
    let _ = window.set_focus();
}

/// The shortcut shows the launcher, or hides it if it is already in front.
fn toggle(app: &AppHandle) {
    let visible = app
        .get_webview_window(QUICK)
        .is_some_and(|window| window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false));
    if visible { hide_quick(app) } else { summon(app) }
}

pub fn hide_quick(app: &AppHandle) {
    app.state::<Launcher>().quick_focused.store(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(QUICK) {
        let _ = window.hide();
    }
}

/// Centred across the monitor under the mouse, a fifth of the way down.
fn place(app: &AppHandle, window: &WebviewWindow) {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|at| app.monitor_from_point(at.x, at.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let width = (QUICK_SIZE.0 * scale) as i32;
    let x = area.position.x + (area.size.width as i32 - width) / 2;
    let y = area.position.y + area.size.height as i32 / 5;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Brings forward the app window used last (`parity::appwindows`; `main` until another is
/// opened) and returns its label. Windows are looked up as `Window`s: once the preview's
/// webview is laid over one (`previewview.rs`) it holds two webviews, and Tauri no longer
/// counts it a webview window.
pub fn show_main(app: &AppHandle) -> Option<String> {
    let window = crate::parity::appwindows::target(app)?;
    let _ = window.show();
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.set_focus();
    Some(window.label().to_owned())
}

/// Window events: closing the last app window may only hide it, closing it for real ends the
/// app (the hidden quick window would otherwise keep it running), and the quick window hides
/// when it loses the focus. Of several app windows (Ctrl+N), all but the last simply close.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let app = window.app_handle();
    let Some(launcher) = app.try_state::<Launcher>() else { return };
    let label = window.label();
    let app_window = crate::parity::appwindows::is_app_window(label);
    let others = || {
        crate::parity::appwindows::app_windows(app)
            .into_iter()
            .filter(|other| other.label() != label)
            .collect::<Vec<_>>()
    };
    match (label, event) {
        (_, WindowEvent::CloseRequested { api, .. })
            if app_window
                && launcher.keeps_running()
                && !launcher.exiting.load(Ordering::Relaxed)
                && !others().iter().any(|other| other.is_visible().unwrap_or(false)) =>
        {
            api.prevent_close();
            let _ = window.hide();
        }
        (_, WindowEvent::Destroyed) if app_window => {
            if others().is_empty() && !launcher.exiting.swap(true, Ordering::Relaxed) {
                app.exit(0);
            }
        }
        (QUICK, WindowEvent::CloseRequested { api, .. }) => {
            api.prevent_close();
            hide_quick(app);
        }
        (QUICK, WindowEvent::Focused(true)) => launcher.quick_focused.store(true, Ordering::Relaxed),
        (QUICK, WindowEvent::Focused(false)) => {
            if launcher.quick_focused.swap(false, Ordering::Relaxed) {
                hide_quick(app);
            }
        }
        _ => {}
    }
}

/// Ends the app for real, whatever the tray option says.
pub fn quit(app: &AppHandle) {
    if let Some(launcher) = app.try_state::<Launcher>() {
        launcher.exiting.store(true, Ordering::Relaxed);
    }
    app.exit(0);
}

// ---------- commands ----------

#[tauri::command(async)]
pub fn launcher_settings(app: AppHandle, launcher: State<Launcher>) -> Settings {
    launcher.settings(&app)
}

/// Sets the quick launcher's shortcut, or turns it off with `null`.
#[tauri::command(async)]
pub fn set_launcher_hotkey(app: AppHandle, launcher: State<Launcher>, shortcut: Option<String>) -> Result<HotkeyStatus, String> {
    launcher.set_hotkey(&app, shortcut.as_deref())
}

#[tauri::command(async)]
pub fn set_keep_in_tray(launcher: State<Launcher>, keep: bool) -> Result<(), String> {
    launcher.set_keep_in_tray(keep)
}

/// Adds or removes the sign-in entry and returns whether it is there now.
#[tauri::command(async)]
pub fn set_start_with_windows(app: AppHandle, start: bool) -> Result<bool, String> {
    let identifier = &app.config().identifier;
    autostart::set_enabled(identifier, start)?;
    Ok(autostart::is_enabled(identifier))
}

/// Brings the app window used last forward, and returns its label, so the quick launcher can
/// hand it what was chosen.
#[tauri::command(async)]
pub fn show_main_window(app: AppHandle) -> Option<String> {
    show_main(&app)
}

#[tauri::command(async)]
pub fn hide_quick_window(app: AppHandle) {
    hide_quick(&app);
}

/// Ends Kestrel, even with "Keep running in the tray" on.
#[tauri::command(async)]
pub fn quit_app(app: AppHandle) {
    quit(&app);
}

// ---------- development hooks for the end-to-end tests ----------

fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

/// Shows the quick launcher exactly as the shortcut does, without pressing it.
#[tauri::command(async)]
pub fn launcher_dev_summon(app: AppHandle) -> Result<(), String> {
    development_only()?;
    summon(&app);
    Ok(())
}

/// Registers `shortcut` as the active shortcut, or unregisters it with `null`; see
/// [`Launcher::set_active_for_test`].
#[tauri::command(async)]
pub fn launcher_dev_hotkey(app: AppHandle, launcher: State<Launcher>, shortcut: Option<String>) -> Result<HotkeyStatus, String> {
    development_only()?;
    launcher.set_active_for_test(&app, shortcut.as_deref())
}

#[derive(Serialize)]
pub struct WindowState {
    pub main_visible: bool,
    pub quick_visible: bool,
    /// The quick launcher's outer bounds on screen: left, top, width, height, in pixels.
    pub quick_bounds: Option<[i64; 4]>,
    /// Every monitor's work area (the screen less the taskbar), as bounds.
    pub work_areas: Vec<[i64; 4]>,
    /// The tray icon exists, and whether its menu shows "Keep running in the tray" ticked.
    pub tray: bool,
    pub tray_keep_ticked: Option<bool>,
}

/// Which windows are showing, where the quick launcher is, and the tray icon's state.
#[tauri::command(async)]
pub fn launcher_dev_windows(app: AppHandle, launcher: State<Launcher>) -> Result<WindowState, String> {
    development_only()?;
    let visible = |label: &str| app.get_window(label).is_some_and(|window| window.is_visible().unwrap_or(false));
    let quick_bounds = app.get_webview_window(QUICK).and_then(|window| {
        let at = window.outer_position().ok()?;
        let size = window.outer_size().ok()?;
        Some([at.x.into(), at.y.into(), size.width.into(), size.height.into()])
    });
    let work_areas = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let area = monitor.work_area();
            [area.position.x.into(), area.position.y.into(), area.size.width.into(), area.size.height.into()]
        })
        .collect();
    let tray = launcher.tray.lock().clone();
    Ok(WindowState {
        main_visible: visible(MAIN),
        quick_visible: visible(QUICK),
        quick_bounds,
        work_areas,
        tray: tray.is_some(),
        tray_keep_ticked: tray.as_ref().and_then(tray::Tray::keep_shown),
    })
}

/// Does what clicking a tray menu item does (`tray-show`, `tray-quick`, `tray-keep` or
/// `tray-settings`), since a script cannot click in the notification area. Quit is left out.
#[tauri::command(async)]
pub fn launcher_dev_tray(app: AppHandle, item: String) -> Result<(), String> {
    development_only()?;
    if ![tray::SHOW, tray::QUICK, tray::KEEP, tray::SETTINGS].contains(&item.as_str()) {
        return Err(format!("No tray menu item {item} for tests"));
    }
    tray::activate(&app, &item);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcut_is_the_default_until_changed_and_off_once_turned_off() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        assert_eq!(Launcher::new(db.clone()).registration.lock().wanted.as_deref(), Some(hotkey::DEFAULT));
        db.set_setting(HOTKEY_SETTING, &Some("shift+control+KeyK")).unwrap();
        assert_eq!(Launcher::new(db.clone()).registration.lock().wanted.as_deref(), Some("Ctrl+Shift+K"));
        db.set_setting(HOTKEY_SETTING, &None::<String>).unwrap();
        assert_eq!(Launcher::new(db.clone()).registration.lock().wanted, None);
        assert!(!Launcher::new(db.clone()).keeps_running(), "off by default");
        db.set_setting(KEEP_SETTING, &true).unwrap();
        assert!(Launcher::new(db).keeps_running());
    }
}
