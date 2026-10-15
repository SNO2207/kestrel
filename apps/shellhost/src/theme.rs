//! Dark or light menus, matching Kestrel's window.
//!
//! Win32 popup menus are light unless the process opts in to dark mode, and the only way to
//! opt in is `SetPreferredAppMode`, which uxtheme.dll exports by ordinal without documenting
//! it (Explorer, Notepad++ and many others use it). The call is best effort: if the export is
//! missing, the menu is simply light.

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::w;

/// `uxtheme.dll` ordinal 135, `SetPreferredAppMode`, on Windows 10 1903 and later.
const SET_PREFERRED_APP_MODE: usize = 135;
/// `uxtheme.dll` ordinal 136, `FlushMenuThemes`.
const FLUSH_MENU_THEMES: usize = 136;
/// `PreferredAppMode::ForceDark`.
const FORCE_DARK: i32 = 2;

/// The colour scheme for menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Makes this process's menus dark when `theme` says so, or, without one, when apps are set
/// to dark in Windows' colour settings. Kestrel passes the scheme its window is showing, which
/// is what the menu should match.
pub fn apply(theme: Option<Theme>) {
    if theme.unwrap_or_else(system_theme) == Theme::Light {
        return;
    }
    // SAFETY: uxtheme.dll is a system library; the two exports, where present, have the
    // signatures declared here, and the library stays loaded while they are called.
    unsafe {
        let uxtheme = LoadLibraryExW(w!("uxtheme.dll"), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        if uxtheme.is_null() {
            return;
        }
        if let Some(set_mode) = GetProcAddress(uxtheme, SET_PREFERRED_APP_MODE as *const u8) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(FORCE_DARK);
        }
        if let Some(flush) = GetProcAddress(uxtheme, FLUSH_MENU_THEMES as *const u8) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
        // uxtheme stays loaded: unloading it would forget the mode just set.
    }
}

/// The "Choose your default app mode" setting.
fn system_theme() -> Theme {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated literals and `value` has room for a DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut value).cast(),
            &mut size,
        )
    };
    if status == 0 && value == 0 { Theme::Dark } else { Theme::Light }
}
