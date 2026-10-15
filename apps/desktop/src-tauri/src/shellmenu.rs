//! The Windows context menu ("Show more options") and Open With, shown by a helper process.
//!
//! A context menu is assembled from shell extensions (archivers, version control, cloud
//! storage), and each one loads into the process that asks for the menu, where a faulty one
//! can crash it. Kestrel never loads them: `kestrel-shellhost.exe` (`apps/shellhost`), which
//! cargo builds next to Kestrel's executable, shows each menu or dialog and runs the chosen
//! command. If an extension crashes the helper, Kestrel reports it and carries on.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{State, WebviewWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

use crate::engine::Engine;

const HELPER: &str = "kestrel-shellhost.exe";
/// Windows caps a command line at 32,767 characters; longer path lists go through stdin.
const MAX_COMMAND_LINE: usize = 30_000;
/// The helper resolves every item before building the menu, which takes a moment for
/// thousands of items and far too long for a million, so larger selections are refused.
const MAX_ITEMS: u64 = 10_000;
/// Makes the helper exit as if it had crashed (see `ProbeMode::Crash`).
const TEST_CRASH: &str = "KESTREL_SHELLHOST_TEST_CRASH";

/// What the user chose from the Windows menu, as the helper reports it.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ShellChoice {
    /// Canonical verb of the chosen command, if it has one.
    pub verb: Option<String>,
    /// Whether the helper ran it. Rename is left to Kestrel, which renames in place.
    pub invoked: bool,
}

/// Where the window's client area is on screen, in physical pixels, and its DPI scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Geometry {
    fn of(window: &WebviewWindow) -> Result<Self, String> {
        let origin = window.inner_position().map_err(|e| e.to_string())?;
        let size = window.inner_size().map_err(|e| e.to_string())?;
        let scale = window.scale_factor().map_err(|e| e.to_string())?;
        Ok(Self { x: origin.x, y: origin.y, width: size.width, height: size.height, scale })
    }

    /// Converts a point in the page (CSS pixels from the top left of the window's content)
    /// to screen pixels, which is what the helper's menu is placed in.
    fn to_screen(self, x: f64, y: f64) -> (i32, i32) {
        (self.x + (x * self.scale).round() as i32, self.y + (y * self.scale).round() as i32)
    }
}

/// One run of the helper.
#[derive(Clone, Debug, PartialEq)]
enum Request {
    /// The context menu of `paths` (or, with `background`, of the one folder in `paths`),
    /// dark or light to match the window.
    Menu { paths: Vec<String>, at: (i32, i32), owner: Option<isize>, dark: bool, extended: bool, background: bool },
    OpenWith { path: String, owner: Option<isize> },
}

impl Request {
    /// The helper's arguments, and the text for its stdin when the paths go there. `listing`
    /// asks for the same items' `verbs` instead of showing anything.
    fn arguments(&self, listing: bool) -> (Vec<String>, Option<String>) {
        let mut args: Vec<String> = Vec::new();
        let owner = |args: &mut Vec<String>, owner: &Option<isize>| {
            if let Some(owner) = owner {
                args.extend(["--owner".into(), owner.to_string()]);
            }
        };
        match self {
            Request::Menu { paths, at, owner: hwnd, dark, extended, background } => {
                if listing {
                    args.push("verbs".into());
                } else {
                    args.extend(["menu".into(), "--x".into(), at.0.to_string(), "--y".into(), at.1.to_string()]);
                    owner(&mut args, hwnd);
                    args.extend(["--theme".into(), if *dark { "dark" } else { "light" }.into()]);
                }
                if *extended {
                    args.push("--extended".into());
                }
                if *background {
                    args.push("--background".into());
                }
                with_paths(args, paths)
            }
            Request::OpenWith { path, owner: hwnd } => {
                if listing {
                    args.push("verbs".into());
                } else {
                    args.push("openwith".into());
                    owner(&mut args, hwnd);
                }
                with_paths(args, std::slice::from_ref(path))
            }
        }
    }

    fn paths(&self) -> &[String] {
        match self {
            Request::Menu { paths, .. } => paths,
            Request::OpenWith { path, .. } => std::slice::from_ref(path),
        }
    }

    /// What the user sees, for messages.
    fn title(&self) -> &'static str {
        match self {
            Request::Menu { .. } => "The Windows menu",
            Request::OpenWith { .. } => "Open with",
        }
    }
}

/// Appends the paths after `--`, or `--stdin` and returns them as text when they would make
/// the command line too long.
fn with_paths(mut args: Vec<String>, paths: &[String]) -> (Vec<String>, Option<String>) {
    // Each path may be quoted and is separated by a space.
    let length: usize = paths.iter().map(|path| path.len() + 3).sum();
    if length > MAX_COMMAND_LINE {
        args.push("--stdin".into());
        return (args, Some(paths.join("\n")));
    }
    args.push("--".into());
    args.extend(paths.iter().cloned());
    (args, None)
}

/// The helper, which cargo builds into the same folder as Kestrel.
fn helper_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = exe.with_file_name(HELPER);
    if helper.is_file() {
        Ok(helper)
    } else {
        Err(format!("Kestrel's shell helper is missing: {} should be next to Kestrel.", helper.display()))
    }
}

/// Runs the helper and waits for it. Returns its output, or why it failed.
fn run_helper(helper: &Path, request: &Request, listing: bool, crash: bool) -> Result<String, String> {
    let (args, stdin) = request.arguments(listing);
    let mut command = Command::new(helper);
    command
        .args(&args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if crash {
        command.env(TEST_CRASH, "1");
    }
    let mut child = command.spawn().map_err(|e| format!("Couldn't start {}: {e}", helper.display()))?;
    // A menu that is not in the foreground stays open when the user clicks elsewhere. Kestrel
    // has the foreground, having just been clicked, and passes the right to take it on.
    // SAFETY: plain FFI call.
    unsafe { AllowSetForegroundWindow(child.id()) };
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // The helper reads all of its input before writing anything, so this cannot block.
        pipe.write_all(text.as_bytes()).map_err(|e| format!("Couldn't pass the items to the shell helper: {e}"))?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    interpret(request.title(), output.status.code(), &output.stdout, &output.stderr)
}

/// Turns the helper's exit into its output or a message for the user.
fn interpret(title: &str, code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<String, String> {
    match code {
        Some(0) => Ok(String::from_utf8_lossy(stdout).into_owned()),
        // An exception code (0x8… or 0xC…, such as 0xC0000005 for an access violation) means
        // the process crashed rather than exiting.
        Some(code) if code < 0 => Err(format!(
            "{title} closed because a shell extension crashed (error {:#010X}). Kestrel is unaffected.",
            code as u32
        )),
        code => {
            let stderr = String::from_utf8_lossy(stderr);
            match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
                Some(reason) => Err(reason.to_owned()),
                None => Err(format!("{title} failed (exit code {}).", code.unwrap_or(-1))),
            }
        }
    }
}

/// Shows the menu or dialog and waits until the user is done with it.
fn show(request: &Request, geometry: Option<Geometry>) -> Result<ShellChoice, String> {
    let helper = helper_path()?;
    let mode = PROBE.lock().mode;
    if cfg!(debug_assertions) && mode != ProbeMode::Off {
        return probe(&helper, request, geometry, mode);
    }
    let output = run_helper(&helper, request, false, false)?;
    match request {
        Request::Menu { .. } => serde_json::from_str(output.trim())
            .map_err(|e| format!("The shell helper's answer made no sense ({e}): {output}")),
        Request::OpenWith { .. } => Ok(ShellChoice::default()),
    }
}

/// Runs `show` on a thread that may wait: a menu stays open for as long as the user likes,
/// and a command such as Properties keeps the helper running until its dialog closes.
async fn show_blocking(request: Request, geometry: Option<Geometry>) -> Result<ShellChoice, String> {
    tauri::async_runtime::spawn_blocking(move || show(&request, geometry)).await.map_err(|e| e.to_string())?
}

/// The window's handle, which owns the menu and any dialog its commands show.
fn owner(window: &WebviewWindow) -> Option<isize> {
    window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
}

/// Shows the Windows context menu for the selected rows at a point in the window (CSS
/// pixels), and runs the command chosen from it. `dark` says which colour scheme the window
/// is showing, for the menu to match.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn shell_menu(
    window: WebviewWindow,
    engine: State<'_, Arc<Engine>>,
    set: u32,
    ranges: Vec<(u32, u32)>,
    x: f64,
    y: f64,
    dark: bool,
    extended: bool,
) -> Result<ShellChoice, String> {
    let count: u64 = ranges.iter().map(|&(start, end)| u64::from(end.saturating_sub(start)) + 1).sum();
    if count > MAX_ITEMS {
        return Err("The Windows menu can show at most 10,000 items at once. Select fewer.".into());
    }
    let paths = engine.paths(set, &ranges)?;
    if paths.is_empty() {
        return Ok(ShellChoice::default());
    }
    let geometry = Geometry::of(&window)?;
    let at = geometry.to_screen(x, y);
    let request = Request::Menu { paths, at, owner: owner(&window), dark, extended, background: false };
    show_blocking(request, Some(geometry)).await
}

/// Shows the Windows context menu of a folder itself (New, Paste, Open in Terminal…), as
/// for a click on empty space in it.
#[tauri::command]
pub async fn shell_folder_menu(
    window: WebviewWindow,
    path: String,
    x: f64,
    y: f64,
    dark: bool,
    extended: bool,
) -> Result<ShellChoice, String> {
    let geometry = Geometry::of(&window)?;
    let at = geometry.to_screen(x, y);
    let request = Request::Menu { paths: vec![path], at, owner: owner(&window), dark, extended, background: true };
    show_blocking(request, Some(geometry)).await
}

/// Shows Windows' Open With dialog for a file.
#[tauri::command]
pub async fn open_with(window: WebviewWindow, path: String) -> Result<(), String> {
    let geometry = Geometry::of(&window).ok();
    show_blocking(Request::OpenWith { path, owner: owner(&window) }, geometry).await.map(|_| ())
}

// ---------- development hook for the end-to-end tests ----------

/// A script cannot use or close a real menu or dialog, so in development builds the tests
/// can switch the helper to a mode where nothing is shown.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMode {
    /// Show menus and dialogs.
    #[default]
    Off,
    /// Run the helper in `verbs` mode for the same items and keep what it listed.
    Verbs,
    /// Make the helper exit as if a shell extension had crashed it.
    Crash,
}

/// What happened the last time a menu or dialog would have been shown.
#[derive(Clone, Debug, Serialize)]
pub struct ProbeRecord {
    /// `menu` or `openwith`.
    kind: &'static str,
    /// The arguments the helper would have been started with.
    args: Vec<String>,
    paths: Vec<String>,
    window: Option<Geometry>,
    /// The helper's `verbs` listing for the same items, if it succeeded.
    items: Option<Value>,
    /// The message the user was shown, if it failed.
    error: Option<String>,
}

struct Probe {
    mode: ProbeMode,
    last: Option<ProbeRecord>,
}

static PROBE: Mutex<Probe> = Mutex::new(Probe { mode: ProbeMode::Off, last: None });

fn probe(helper: &Path, request: &Request, window: Option<Geometry>, mode: ProbeMode) -> Result<ShellChoice, String> {
    let result = run_helper(helper, request, true, mode == ProbeMode::Crash);
    PROBE.lock().last = Some(ProbeRecord {
        kind: match request {
            Request::Menu { .. } => "menu",
            Request::OpenWith { .. } => "openwith",
        },
        args: request.arguments(false).0,
        paths: request.paths().to_vec(),
        window,
        items: result.as_ref().ok().and_then(|text| serde_json::from_str(text).ok()),
        error: result.as_ref().err().cloned(),
    });
    result.map(|_| ShellChoice::default())
}

fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

/// Sets how menus and dialogs are shown, and forgets the last record. Development builds only.
#[tauri::command]
pub fn shell_probe(mode: ProbeMode) -> Result<(), String> {
    development_only()?;
    let mut probe = PROBE.lock();
    probe.mode = mode;
    probe.last = None;
    Ok(())
}

/// Takes the record of the last menu or dialog. Development builds only.
#[tauri::command]
pub fn shell_probe_take() -> Result<Option<ProbeRecord>, String> {
    development_only()?;
    Ok(PROBE.lock().last.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&text| text.to_owned()).collect()
    }

    #[test]
    fn converts_page_points_to_screen_pixels() {
        let geometry = Geometry { x: -1920, y: 40, width: 1600, height: 900, scale: 1.5 };
        assert_eq!(geometry.to_screen(0.0, 0.0), (-1920, 40));
        assert_eq!(geometry.to_screen(100.0, 20.4), (-1770, 71));
        let plain = Geometry { scale: 1.0, ..geometry };
        assert_eq!(plain.to_screen(12.0, 7.0), (-1908, 47));
    }

    #[test]
    fn builds_the_helper_command_line() {
        let menu = Request::Menu {
            paths: strings(&[r"C:\a b.txt", r"C:\c.txt"]),
            at: (-5, 300),
            owner: Some(4242),
            dark: true,
            extended: true,
            background: false,
        };
        assert_eq!(
            menu.arguments(false),
            (
                strings(&[
                    "menu", "--x", "-5", "--y", "300", "--owner", "4242", "--theme", "dark", "--extended", "--",
                    r"C:\a b.txt", r"C:\c.txt",
                ]),
                None
            )
        );
        assert_eq!(menu.arguments(true).0, strings(&["verbs", "--extended", "--", r"C:\a b.txt", r"C:\c.txt"]));

        let folder = Request::Menu {
            paths: strings(&[r"C:\Users"]),
            at: (1, 2),
            owner: None,
            dark: false,
            extended: false,
            background: true,
        };
        assert_eq!(
            folder.arguments(false).0,
            strings(&["menu", "--x", "1", "--y", "2", "--theme", "light", "--background", "--", r"C:\Users"])
        );

        let open_with = Request::OpenWith { path: r"C:\a.txt".into(), owner: Some(7) };
        assert_eq!(open_with.arguments(false).0, strings(&["openwith", "--owner", "7", "--", r"C:\a.txt"]));
        assert_eq!(open_with.arguments(true).0, strings(&["verbs", "--", r"C:\a.txt"]));
    }

    #[test]
    fn long_selections_go_through_stdin() {
        let paths: Vec<String> = (0..2000).map(|i| format!(r"C:\Some folder\file number {i}.txt")).collect();
        let request = Request::Menu {
            paths: paths.clone(),
            at: (0, 0),
            owner: None,
            dark: false,
            extended: false,
            background: false,
        };
        let (args, stdin) = request.arguments(false);
        assert_eq!(args.last().map(String::as_str), Some("--stdin"));
        assert!(!args.iter().any(|arg| arg == "--"));
        assert_eq!(stdin.unwrap().lines().collect::<Vec<_>>(), paths);
    }

    #[test]
    fn reports_crashes_and_failures() {
        assert_eq!(interpret("The Windows menu", Some(0), b"{}\n", b""), Ok("{}\n".into()));
        let crash = interpret("The Windows menu", Some(0xC000_0005_u32 as i32), b"", b"").unwrap_err();
        assert!(crash.contains("crashed") && crash.contains("0xC0000005"), "{crash}");
        let refused = interpret("Open with", Some(1), b"", b"\r\nCouldn't find C:\\x.txt\r\nmore\r\n").unwrap_err();
        assert_eq!(refused, "Couldn't find C:\\x.txt");
        assert_eq!(interpret("Open with", Some(3), b"", b"").unwrap_err(), "Open with failed (exit code 3).");
    }

    /// The helper built alongside this test binary (`target/<profile>/`), if it was built:
    /// `cargo test --workspace` builds it, `cargo test -p kestrel-desktop` does not.
    fn built_helper() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let helper = exe.parent()?.parent()?.join(HELPER);
        if helper.is_file() {
            Some(helper)
        } else {
            eprintln!("skipped: {} is not built (run cargo test --workspace)", helper.display());
            None
        }
    }

    #[test]
    fn runs_the_real_helper() {
        let Some(helper) = built_helper() else { return };
        let dir = std::env::temp_dir().join(format!("kestrel-shellmenu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("other")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("other/b.txt"), "b").unwrap();
        let path = |name: &str| dir.join(name).to_str().unwrap().to_owned();
        let menu = |paths: Vec<String>| Request::Menu {
            paths,
            at: (0, 0),
            owner: None,
            dark: false,
            extended: false,
            background: false,
        };

        let listed = run_helper(&helper, &menu(vec![path("a.txt")]), true, false).unwrap();
        let items: Value = serde_json::from_str(&listed).unwrap();
        assert!(items.as_array().unwrap().iter().any(|item| item["verb"] == "properties"), "{items}");

        let refused = run_helper(&helper, &menu(vec![path("a.txt"), path(r"other\b.txt")]), true, false).unwrap_err();
        assert!(refused.contains("different folders"), "{refused}");

        let crashed = run_helper(&helper, &menu(vec![path("a.txt")]), true, true).unwrap_err();
        assert!(crashed.contains("crashed"), "{crashed}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
