//! Building, listing, showing and invoking a shell context menu.

use std::cell::RefCell;
use std::ffi::CString;

use serde::Serialize;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO,
    CMINVOKECOMMANDINFOEX, GCS_VERBW, IContextMenu, IContextMenu2, IContextMenu3,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_UNICODE,
};
use windows::Win32::UI::WindowsAndMessaging::{HMENU as ComMenu, SW_SHOWNORMAL};
use windows::core::{HSTRING, Interface, PCSTR, PCWSTR, PSTR};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetMenuItemCount, GetMenuItemInfoW, HMENU, MENUITEMINFOW, MFS_DEFAULT, MFS_DISABLED,
    MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, RegisterClassExW,
    SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM,
    WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_sys::w;

/// Range of command ids handed to the menu's handlers. The ids each handler takes are offsets
/// from the first, which is what `GetCommandString` and `InvokeCommand` expect back.
const FIRST_ID: u32 = 1;
const LAST_ID: u32 = 0x7FFF;
/// Submenus listed by `verbs` are followed this deep.
const MAX_DEPTH: u32 = 4;
/// The user dismissed a dialog the command showed, such as a delete confirmation.
const CANCELLED: [i32; 2] = [0x8007_04C7_u32 as i32, 0x8027_0000_u32 as i32];

/// One entry of a menu, as `verbs` prints it.
#[derive(Debug, Serialize)]
pub struct Item {
    /// Command offset, for items that run a command rather than open a submenu.
    pub id: Option<u32>,
    /// Label without its access-key marker.
    pub text: String,
    /// Language-independent name of the command, if its handler gives one.
    pub verb: Option<String>,
    pub submenu: bool,
    pub disabled: bool,
    /// The command Explorer runs on double-click, shown in bold.
    pub default: bool,
    /// The submenu's items, when its handler fills it in up front. Many (Send to, Open with)
    /// are only filled in as they open, so they are listed here as empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
}

/// A context menu that its handlers have filled in.
pub struct Menu {
    handle: HMENU,
    context: IContextMenu,
}

/// Which command to run.
pub enum Choice<'a> {
    /// An item chosen from the menu, by its offset.
    Offset(u32),
    /// A canonical verb such as `copy` or `properties`.
    Verb(&'a str),
}

/// How to run a command.
pub struct Invocation<'a> {
    pub choice: Choice<'a>,
    /// Window that owns any dialog the command shows.
    pub owner: Option<HWND>,
    /// Working directory: the folder the items are in.
    pub directory: &'a str,
    /// Where the menu was shown, so commands can place their windows near it.
    pub point: Option<(i32, i32)>,
    /// A person chose the command from the menu: pass the keyboard state on (Shift with
    /// Delete deletes for good) and let the command show its own error dialogs. Otherwise
    /// errors are only returned.
    pub interactive: bool,
}

impl Menu {
    /// Creates a menu and lets every handler add its items. `flags` are `CMF_*` values.
    pub fn build(context: IContextMenu, flags: u32) -> Result<Self, String> {
        // SAFETY: plain FFI call; the menu is destroyed when `Menu` is dropped.
        let handle = unsafe { CreatePopupMenu() };
        if handle.is_null() {
            return Err("Couldn't create a menu".into());
        }
        let menu = Self { handle, context };
        // SAFETY: the menu handle is live and owned by `menu`.
        unsafe { menu.context.QueryContextMenu(ComMenu(handle), 0, FIRST_ID, LAST_ID, flags) }
            .ok()
            .map_err(|e| format!("Windows couldn't build the menu: {}", e.message()))?;
        Ok(menu)
    }

    /// The menu's items, including those of submenus that are already filled in.
    pub fn items(&self) -> Vec<Item> {
        self.walk(self.handle, 0)
    }

    fn walk(&self, menu: HMENU, depth: u32) -> Vec<Item> {
        // SAFETY: `menu` is this menu or one of its submenus, alive while `self` is.
        let count = unsafe { GetMenuItemCount(menu) };
        let mut items = Vec::new();
        for position in 0..count.max(0) as u32 {
            let mut info = menu_item_info(MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_SUBMENU | MIIM_STRING);
            // SAFETY: `info` is initialised with its size and no string buffer, so only the
            // string's length is returned.
            if unsafe { GetMenuItemInfoW(menu, position, 1, &mut info) } == 0 || info.fType & MFT_SEPARATOR != 0 {
                continue;
            }
            let text = item_text(menu, position, info.cch);
            let submenu = !info.hSubMenu.is_null();
            let id = (!submenu && (FIRST_ID..=LAST_ID).contains(&info.wID)).then(|| info.wID - FIRST_ID);
            items.push(Item {
                id,
                verb: id.and_then(|id| self.verb(id)),
                text: clean_label(&text),
                submenu,
                disabled: info.fState & MFS_DISABLED != 0,
                default: info.fState & MFS_DEFAULT != 0,
                items: if submenu && depth < MAX_DEPTH { self.walk(info.hSubMenu, depth + 1) } else { Vec::new() },
            });
        }
        items
    }

    /// The canonical verb of the command at `offset`, if its handler has one.
    pub fn verb(&self, offset: u32) -> Option<String> {
        let mut buffer = [0u16; 256];
        // SAFETY: the buffer's length is passed in characters, as GCS_VERBW expects.
        unsafe {
            self.context.GetCommandString(
                offset as usize,
                GCS_VERBW,
                None,
                PSTR(buffer.as_mut_ptr().cast()),
                buffer.len() as u32,
            )
        }
        .ok()?;
        let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
        let verb = String::from_utf16(&buffer[..length]).ok()?;
        // Some handlers answer with an empty string, or with ANSI text despite the request
        // for UTF-16, which reads back as non-ASCII.
        (!verb.is_empty() && verb.chars().all(|c| c.is_ascii_graphic() || c == ' ')).then_some(verb)
    }

    /// Shows the menu at `at` (screen pixels) and waits for a choice. Returns the chosen
    /// command's offset, or `None` if the menu was dismissed.
    pub fn track(&self, at: (i32, i32), owner: Option<HWND>) -> Result<Option<u32>, String> {
        let window = MessageWindow::create(owner)?;
        let forwarders = Forwarders { two: self.context.cast().ok(), three: self.context.cast().ok() };
        ACTIVE.with_borrow_mut(|active| *active = Some(forwarders));
        // SAFETY: the window and menu are live. Without the foreground, the menu would not
        // close when the user clicks elsewhere; Kestrel lets the helper take it.
        let chosen = unsafe {
            SetForegroundWindow(window.0);
            TrackPopupMenuEx(self.handle, TPM_RETURNCMD | TPM_RIGHTBUTTON, at.0, at.1, window.0, std::ptr::null())
        } as u32;
        ACTIVE.with_borrow_mut(|active| *active = None);
        Ok((chosen >= FIRST_ID).then(|| chosen - FIRST_ID))
    }

    /// Runs a command of this menu.
    pub fn invoke(&self, how: &Invocation) -> Result<(), String> {
        let verb_ansi;
        let verb_wide;
        let (verb, verb_w, described) = match how.choice {
            // MAKEINTRESOURCE: a small integer in place of a string pointer means an offset.
            Choice::Offset(offset) => {
                (PCSTR(offset as usize as *const u8), PCWSTR(offset as usize as *const u16), "the command".to_owned())
            }
            Choice::Verb(name) => {
                verb_ansi = CString::new(name).map_err(|_| "A verb can't contain NUL")?;
                verb_wide = HSTRING::from(name);
                (PCSTR(verb_ansi.as_ptr().cast()), PCWSTR(verb_wide.as_ptr()), format!("\"{name}\""))
            }
        };
        let directory_wide = HSTRING::from(how.directory);
        // Handlers that read only the ANSI fields get the directory when it is plain ASCII.
        let directory_ansi = how.directory.is_ascii().then(|| CString::new(how.directory).ok()).flatten();

        let mut mask = SEE_MASK_UNICODE | SEE_MASK_NOASYNC;
        if how.point.is_some() {
            mask |= CMIC_MASK_PTINVOKE;
        }
        if how.interactive {
            // SAFETY: plain FFI calls reading this thread's keyboard state, which the menu
            // loop kept up to date.
            unsafe {
                if GetKeyState(VK_SHIFT.into()) < 0 {
                    mask |= CMIC_MASK_SHIFT_DOWN;
                }
                if GetKeyState(VK_CONTROL.into()) < 0 {
                    mask |= CMIC_MASK_CONTROL_DOWN;
                }
            }
        } else {
            mask |= SEE_MASK_FLAG_NO_UI;
        }
        let (x, y) = how.point.unwrap_or_default();
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: mask,
            hwnd: how.owner.unwrap_or_default(),
            lpVerb: verb,
            lpVerbW: verb_w,
            lpDirectory: directory_ansi.as_ref().map_or(PCSTR::null(), |text| PCSTR(text.as_ptr().cast())),
            lpDirectoryW: PCWSTR(directory_wide.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: POINT { x, y },
            ..Default::default()
        };
        // SAFETY: every string the structure points to outlives the call, and its size field
        // tells the handler it is the extended form.
        match unsafe { self.context.InvokeCommand(std::ptr::from_ref(&info).cast::<CMINVOKECOMMANDINFO>()) } {
            Ok(()) => Ok(()),
            Err(e) if CANCELLED.contains(&e.code().0) => Ok(()),
            Err(e) => Err(format!("Windows couldn't run {described}: {}", e.message())),
        }
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: the menu was created by `build` and is destroyed exactly once.
        unsafe { DestroyMenu(self.handle) };
    }
}

fn menu_item_info(mask: u32) -> MENUITEMINFOW {
    // SAFETY: MENUITEMINFOW is a plain C struct for which all zeroes is a valid value.
    let mut info: MENUITEMINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MENUITEMINFOW>() as u32;
    info.fMask = mask;
    info
}

/// The label of the item at `position`, which is `length` characters long.
fn item_text(menu: HMENU, position: u32, length: u32) -> String {
    if length == 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let mut info = menu_item_info(MIIM_STRING);
    info.dwTypeData = buffer.as_mut_ptr();
    info.cch = buffer.len() as u32;
    // SAFETY: `buffer` has room for `cch` characters including the terminator.
    if unsafe { GetMenuItemInfoW(menu, position, 1, &mut info) } == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(info.cch as usize).min(length as usize)])
}

/// Removes the access-key marker (`&Open` is shown as "Open" with O underlined) and any
/// shortcut text after a tab.
pub fn clean_label(text: &str) -> String {
    let text = text.split('\t').next().unwrap_or_default();
    let mut label = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            label.push(c);
        } else if chars.peek() == Some(&'&') {
            label.push('&');
            chars.next();
        }
    }
    label
}

/// The interfaces that draw owner-drawn items and fill in submenus (Send to, Open with) as
/// they open. Menu messages arrive at the window that owns the menu and must be passed on.
struct Forwarders {
    two: Option<IContextMenu2>,
    three: Option<IContextMenu3>,
}

thread_local! {
    /// The menu being shown on this thread, for the window procedure.
    static ACTIVE: RefCell<Option<Forwarders>> = const { RefCell::new(None) };
}

/// Passes a menu message to the menu's handlers. Returns the result to give back, or `None`
/// if no handler took it.
fn forward(message: u32, wparam: usize, lparam: isize) -> Option<isize> {
    ACTIVE.with_borrow(|active| {
        let active = active.as_ref()?;
        if let Some(three) = &active.three {
            let mut result = LRESULT(0);
            // SAFETY: plain COM call with the message's own parameters.
            let handled = unsafe { three.HandleMenuMsg2(message, WPARAM(wparam), LPARAM(lparam), Some(&mut result)) };
            if handled.is_ok() {
                return Some(match message {
                    WM_DRAWITEM | WM_MEASUREITEM => 1,
                    _ => result.0,
                });
            }
        }
        if message == WM_MENUCHAR {
            return None;
        }
        let two = active.two.as_ref()?;
        // SAFETY: plain COM call with the message's own parameters.
        unsafe { two.HandleMenuMsg(message, WPARAM(wparam), LPARAM(lparam)) }.ok()?;
        Some(if matches!(message, WM_DRAWITEM | WM_MEASUREITEM) { 1 } else { 0 })
    })
}

unsafe extern "system" fn window_procedure(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if matches!(message, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR)
        && let Some(result) = forward(message, wparam, lparam)
    {
        return result;
    }
    // SAFETY: the parameters are passed through unchanged.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// An invisible window that owns the menu while it is shown and receives its messages. It is
/// owned in turn by Kestrel's window, so the menu stays above it.
struct MessageWindow(windows_sys::Win32::Foundation::HWND);

impl MessageWindow {
    fn create(owner: Option<HWND>) -> Result<Self, String> {
        let class = w!("KestrelShellHostMenu");
        // SAFETY: the class structure and its strings are valid for the calls; registering a
        // second time fails harmlessly and the first registration is used.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut description: WNDCLASSEXW = std::mem::zeroed();
            description.cbSize = size_of::<WNDCLASSEXW>() as u32;
            description.lpfnWndProc = Some(window_procedure);
            description.hInstance = instance;
            description.lpszClassName = class;
            RegisterClassExW(&description);
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("Kestrel shell menu"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                owner.map_or(std::ptr::null_mut(), |owner| owner.0),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(format!("Couldn't create the menu's window: {}", std::io::Error::last_os_error()));
        }
        Ok(Self(hwnd))
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed exactly once.
        unsafe { DestroyWindow(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_labels() {
        assert_eq!(clean_label("&Open"), "Open");
        assert_eq!(clean_label("Save && close"), "Save & close");
        assert_eq!(clean_label("Cu&t\tCtrl+X"), "Cut");
        assert_eq!(clean_label(""), "");
    }
}
