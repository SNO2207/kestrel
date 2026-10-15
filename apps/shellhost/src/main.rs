//! `kestrel-shellhost`: shows the Windows context menu and Open With dialog for Kestrel.
//!
//! A context menu is assembled from shell extensions (archivers, version control, cloud
//! storage, antivirus), and each one loads into the process that asks for the menu. A faulty
//! extension can crash that process. Kestrel therefore never loads them itself: it starts
//! this helper for each menu, so a crash costs only the helper.
//!
//! Modes (see `args.rs`):
//! - `menu` shows the menu at a point on screen and runs the chosen command;
//! - `verbs` prints the menu's items as JSON without showing anything;
//! - `invoke` runs one command by its canonical verb without showing anything;
//! - `openwith` shows the Open With dialog for a file.
//!
//! Exit codes: 0 when done (including a menu dismissed without a choice), 1 when Windows
//! refused (the reason is on stderr), 2 for a bad command line. Anything else is a crash.

// No console window: commands the menu runs (Open in Terminal, a console program) then get
// consoles of their own instead of sharing a hidden one.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod args;
mod items;
mod menu;
mod process;
mod theme;

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_ITEMMENU, CMF_NORMAL, IContextMenu, OAIF_ALLOW_REGISTRATION,
    OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SHOpenWithDialog,
};
use windows::core::{HSTRING, PCWSTR};
use windows_sys::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, IsWindow};

use args::{Command, Target};
use menu::{Choice, Invocation, Menu};
use theme::Theme;

/// Set by Kestrel's development builds to check how it reports a crashed helper: the helper
/// exits at once with an access violation's exit code, without faulting for real (which
/// would bring up Windows Error Reporting).
const TEST_CRASH: &str = "KESTREL_SHELLHOST_TEST_CRASH";
const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;

/// What happened to a menu shown with `menu`, printed as JSON for Kestrel.
#[derive(Serialize)]
struct Outcome {
    /// Canonical verb of the chosen command, if it has one.
    verb: Option<String>,
    /// Whether the helper ran it. Rename is left to Kestrel, which renames in place.
    invoked: bool,
}

fn main() {
    if std::env::var_os(TEST_CRASH).is_some() {
        std::process::exit(STATUS_ACCESS_VIOLATION as i32);
    }
    let read_stdin = || {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map(|_| text)
    };
    let command = match args::parse(std::env::args().skip(1), read_stdin) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("kestrel-shellhost: {message}\n{}", args::USAGE);
            std::process::exit(2);
        }
    };
    if let Err(message) = run(command) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

/// Balances `OleInitialize`. OLE rather than plain COM, because Copy and Cut put an OLE data
/// object on the clipboard.
struct Ole;

impl Ole {
    fn initialize() -> Result<Self, String> {
        // SAFETY: plain FFI call; balanced by `Drop`.
        let result = unsafe { OleInitialize(std::ptr::null()) };
        if result < 0 {
            let error = windows::core::Error::from_hresult(windows::core::HRESULT(result));
            return Err(format!("Couldn't start OLE: {}", error.message()));
        }
        Ok(Self)
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        // SAFETY: only constructed after OleInitialize succeeded on this thread.
        unsafe { OleUninitialize() };
    }
}

fn run(command: Command) -> Result<(), String> {
    let _ole = Ole::initialize()?;
    let reference = process::Reference::install();
    let result = match command {
        Command::Menu { at, target, owner, theme } => show_menu(at, &target, window(owner)?, theme),
        Command::Verbs { target } => list_verbs(&target).map(|()| Ran::Nothing),
        Command::Invoke { verb, target, owner } => invoke_verb(&verb, &target, window(owner)?),
        Command::OpenWith { path, owner } => open_with(&path, window(owner)?),
    };
    // What a command put on the clipboard stays there, and whatever it left running (a
    // Properties dialog, a zip being made) finishes, before the process ends.
    process::flush_clipboard();
    let settle = if matches!(result, Ok(Ran::Command)) { process::SETTLE } else { Duration::ZERO };
    reference.wait(settle);
    process::flush_clipboard();
    result.map(|_| ())
}

/// Whether a command was run, which may have left a dialog or background work behind.
enum Ran {
    Command,
    Nothing,
}

/// Checks a window handle given on the command line.
fn window(handle: Option<isize>) -> Result<Option<HWND>, String> {
    let Some(handle) = handle else { return Ok(None) };
    // SAFETY: IsWindow accepts any value.
    if unsafe { IsWindow(handle as _) } == 0 {
        return Err(format!("{handle:#x} is not a window"));
    }
    Ok(Some(HWND(handle as _)))
}

/// The menu for the target and the folder its commands run in.
fn context_menu(target: &Target, owner: Option<HWND>) -> Result<(IContextMenu, String), String> {
    let owner = owner.unwrap_or_default();
    if target.background {
        let folder = &target.paths[0];
        return Ok((items::background_menu(folder, owner)?, folder.clone()));
    }
    let items = items::Items::parse(&target.paths)?;
    Ok((items.context_menu(owner)?, items.directory.clone()))
}

/// `CMF_*` flags for the target's menu. `can_rename` adds Rename, which only Kestrel can
/// carry out, since it renames in place.
fn flags(target: &Target, can_rename: bool) -> u32 {
    let mut flags = CMF_NORMAL;
    if !target.background {
        flags |= CMF_ITEMMENU;
        if can_rename {
            flags |= CMF_CANRENAME;
        }
    }
    if target.extended {
        flags |= CMF_EXTENDEDVERBS;
    }
    flags
}

fn show_menu(at: (i32, i32), target: &Target, owner: Option<HWND>, theme: Option<Theme>) -> Result<Ran, String> {
    let (context, directory) = context_menu(target, owner)?;
    let menu = Menu::build(context, flags(target, true))?;
    theme::apply(theme);
    let outcome = match menu.track(at, owner)? {
        None => Outcome { verb: None, invoked: false },
        Some(offset) => {
            let verb = menu.verb(offset);
            if verb.as_deref() == Some("rename") {
                Outcome { verb, invoked: false }
            } else {
                // The command may start a program that should come to the front.
                // SAFETY: plain FFI call.
                unsafe { AllowSetForegroundWindow(ASFW_ANY) };
                let how = Invocation {
                    choice: Choice::Offset(offset),
                    owner,
                    directory: &directory,
                    point: Some(at),
                    interactive: true,
                };
                menu.invoke(&how)?;
                Outcome { verb, invoked: true }
            }
        }
    };
    print_json(&outcome)?;
    Ok(if outcome.invoked { Ran::Command } else { Ran::Nothing })
}

fn list_verbs(target: &Target) -> Result<(), String> {
    let (context, _) = context_menu(target, None)?;
    let menu = Menu::build(context, flags(target, true))?;
    print_json(&menu.items())
}

fn invoke_verb(verb: &str, target: &Target, owner: Option<HWND>) -> Result<Ran, String> {
    let (context, directory) = context_menu(target, owner)?;
    let menu = Menu::build(context, flags(target, false))?;
    let how = Invocation { choice: Choice::Verb(verb), owner, directory: &directory, point: None, interactive: false };
    menu.invoke(&how)?;
    Ok(Ran::Command)
}

fn open_with(path: &str, owner: Option<HWND>) -> Result<Ran, String> {
    let file = Path::new(path);
    if !file.exists() {
        return Err(format!("Couldn't find {path}"));
    }
    if file.is_dir() {
        return Err(format!("{path} is a folder; Open with is for files"));
    }
    let path = HSTRING::from(path);
    let info = OPENASINFO {
        pcszFile: PCWSTR(path.as_ptr()),
        pcszClass: PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
    };
    // SAFETY: plain FFI call; the program chosen should come to the front.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    // SAFETY: `info` and the path it points to outlive the call, which returns when the
    // dialog closes.
    match unsafe { SHOpenWithDialog(owner, &info) } {
        Ok(()) => Ok(Ran::Command),
        // Closing the dialog without choosing is not a failure.
        Err(e) if e.code().0 == 0x8007_04C7_u32 as i32 => Ok(Ran::Nothing),
        Err(e) => Err(format!("Windows couldn't show Open with: {}", e.message())),
    }
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}
