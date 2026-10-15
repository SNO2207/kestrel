//! The notification area icon and its menu.

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use super::{Launcher, MAIN, events};

pub const SHOW: &str = "tray-show";
pub const QUICK: &str = "tray-quick";
pub const KEEP: &str = "tray-keep";
pub const SETTINGS: &str = "tray-settings";
pub const QUIT: &str = "tray-quit";

/// The icon, kept alive for as long as the app runs, and the menu item that mirrors the
/// keep-running option. Both are handles, cheap to copy.
#[derive(Clone)]
pub struct Tray {
    _icon: TrayIcon,
    keep: CheckMenuItem<tauri::Wry>,
}

impl Tray {
    pub fn install(app: &AppHandle, keep_running: bool) -> tauri::Result<Self> {
        let keep = CheckMenuItem::with_id(app, KEEP, "Keep running in the tray", true, keep_running, None::<&str>)?;
        let menu = Menu::with_items(
            app,
            &[
                &MenuItem::with_id(app, SHOW, "Show Kestrel", true, None::<&str>)?,
                &MenuItem::with_id(app, QUICK, "Quick launcher", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &keep,
                &MenuItem::with_id(app, SETTINGS, "Launcher settings…", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, QUIT, "Quit Kestrel", true, None::<&str>)?,
            ],
        )?;
        let mut builder = TrayIconBuilder::with_id("kestrel")
            .tooltip("Kestrel")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(on_menu)
            .on_tray_icon_event(|tray, event| {
                // A left click brings the window back, as the menu's first item does.
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    super::show_main(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        Ok(Self { _icon: builder.build(app)?, keep })
    }

    /// Shows the keep-running option's new state, after a change made in the settings dialog.
    pub fn show_keep(&self, keep: bool) {
        let _ = self.keep.set_checked(keep);
    }

    /// Whether the menu shows the keep-running option as on.
    pub fn keep_shown(&self) -> Option<bool> {
        self.keep.is_checked().ok()
    }
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    activate(app, event.id().as_ref());
}

/// Carries out a menu item, by id: what a click on it does.
pub fn activate(app: &AppHandle, id: &str) {
    match id {
        SHOW => super::show_main(app),
        QUICK => super::summon(app),
        KEEP => {
            let launcher = app.state::<Launcher>();
            // The option flips, and the item's tick is set to match (a click has already
            // flipped it, so that only confirms it).
            let keep = !launcher.keeps_running();
            if let Err(error) = launcher.set_keep_in_tray(keep) {
                eprintln!("could not save the tray option: {error}");
            }
        }
        SETTINGS => {
            super::show_main(app);
            let _ = app.emit_to(MAIN, events::SETTINGS, ());
        }
        QUIT => super::quit(app),
        _ => {}
    }
}
