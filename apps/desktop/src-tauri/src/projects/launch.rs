//! Opening a project in other programs: VS Code, Visual Studio, Windows Terminal (plain, with
//! a script running, or with Claude Code), Explorer and the remote's web page. Everything goes
//! through `ShellExecuteW`, as a double-click would.
//!
//! A script cannot use or close the programs this starts, so development builds have a hook
//! that records each launch instead of performing it; the end-to-end tests turn it on.

use std::path::{Path, PathBuf};
use std::ptr::null;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// One program to start: what, with which arguments, in which folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Launch {
    pub file: String,
    pub params: String,
    pub dir: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Launcher {
    Vscode,
    VisualStudio,
    Terminal,
    Claude,
    Explorer,
    Web,
}

/// Quotes one argument the way `CommandLineToArgvW` (and so Windows Terminal) reads it back.
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Windows Terminal reads `;` as the start of another command; escape it inside one.
fn terminal_safe(command: &str) -> String {
    command.replace(';', "\\;")
}

/// Searches the PATH for `name` (with any of the PATHEXT extensions).
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts = ["", ".exe", ".cmd", ".bat", ".com"];
    std::env::split_paths(&path)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|candidate| candidate.is_file())
}

/// Windows Terminal, when installed.
fn terminal() -> Option<PathBuf> {
    let alias = std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join(r"Microsoft\WindowsApps\wt.exe"));
    alias.filter(|p| p.exists()).or_else(|| on_path("wt"))
}

/// VS Code's own executable (not its `code.cmd` wrapper, which would flash a console).
fn vscode() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(dir).join(r"Programs\Microsoft VS Code\Code.exe"));
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(PathBuf::from(dir).join(r"Microsoft VS Code\Code.exe"));
        }
    }
    // `...\Microsoft VS Code\bin\code.cmd` sits beside the executable's folder.
    if let Some(cmd) = on_path("code")
        && let Some(install) = cmd.parent().and_then(Path::parent)
    {
        candidates.push(install.join("Code.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Solution files in the project's root folder, by name.
pub fn solutions(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sln")) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Which launchers can work for this project on this machine.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Available {
    pub vscode: bool,
    /// The solution Visual Studio would open, if the project has one.
    pub solution: Option<String>,
    /// Windows Terminal is installed (otherwise a console window stands in).
    pub terminal: bool,
    pub claude: bool,
}

pub fn available(root: &Path) -> Available {
    Available {
        vscode: vscode().is_some(),
        solution: solutions(root).first().map(|p| p.display().to_string()),
        terminal: terminal().is_some(),
        claude: on_path("claude").is_some(),
    }
}

/// Runs `command` in a new terminal in `root`, falling back to a console window without
/// Windows Terminal. The terminal stays open afterwards so its output can be read.
pub fn in_terminal(root: &str, command: Option<&str>) -> Launch {
    match terminal() {
        Some(wt) => {
            let mut params = format!("-d {}", quote(root));
            if let Some(command) = command {
                params.push_str(&format!(" cmd /k {}", terminal_safe(command)));
            }
            Launch { file: wt.display().to_string(), params, dir: root.to_owned() }
        }
        None => Launch {
            file: "cmd.exe".to_owned(),
            params: command.map(|c| format!("/k {c}")).unwrap_or_default(),
            dir: root.to_owned(),
        },
    }
}

/// What a launcher would start for the project at `root`.
pub fn plan(launcher: Launcher, root: &str, web_url: Option<&str>) -> Result<Launch, String> {
    let dir = root.to_owned();
    Ok(match launcher {
        Launcher::Vscode => {
            let code = vscode().ok_or("VS Code isn't installed")?;
            Launch { file: code.display().to_string(), params: quote(root), dir }
        }
        Launcher::VisualStudio => {
            let solution = solutions(Path::new(root)).into_iter().next().ok_or("This project has no solution file")?;
            // Opening the solution lets Windows pick the Visual Studio version it belongs to.
            Launch { file: solution.display().to_string(), params: String::new(), dir }
        }
        Launcher::Terminal => in_terminal(root, None),
        Launcher::Claude => in_terminal(root, Some("claude")),
        Launcher::Explorer => Launch { file: "explorer.exe".to_owned(), params: quote(root), dir },
        Launcher::Web => {
            let url = web_url.ok_or("This project has no web page")?;
            if !url.starts_with("https://") {
                return Err("Only https pages are opened".into());
            }
            Launch { file: url.to_owned(), params: String::new(), dir }
        }
    })
}

/// Starts the program, or, with the development hook on, only records it.
pub fn start(launch: Launch) -> Result<(), String> {
    if cfg!(debug_assertions) {
        let mut probe = PROBE.lock();
        if probe.on {
            probe.records.push(launch);
            return Ok(());
        }
    }
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let (verb, file, params, dir) = (wide("open"), wide(&launch.file), wide(&launch.params), wide(&launch.dir));
    // SAFETY: every string is NUL-terminated and outlives the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            if launch.params.is_empty() { null() } else { params.as_ptr() },
            dir.as_ptr(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success as a value greater than 32.
    if result as usize > 32 { Ok(()) } else { Err(format!("Windows couldn't start {}", launch.file)) }
}

// ---------- development hook for the end-to-end tests ----------

struct Probe {
    on: bool,
    records: Vec<Launch>,
}

static PROBE: Mutex<Probe> = Mutex::new(Probe { on: false, records: Vec::new() });

pub fn development_only() -> Result<(), String> {
    if cfg!(debug_assertions) { Ok(()) } else { Err("Only development builds have this hook".into()) }
}

/// Switches recording on or off, forgetting what was recorded.
pub fn set_probe(on: bool) {
    let mut probe = PROBE.lock();
    probe.on = on;
    probe.records.clear();
}

/// Takes what was recorded since the last call.
pub fn take_records() -> Vec<Launch> {
    std::mem::take(&mut PROBE.lock().records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_for_windows() {
        assert_eq!(quote(r"C:\Code\app"), r"C:\Code\app");
        assert_eq!(quote(r"C:\My Code\app"), r#""C:\My Code\app""#);
        assert_eq!(quote(r"C:\My Code\"), r#""C:\My Code\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn terminal_commands_keep_semicolons_inside_the_command() {
        let launch = in_terminal(r"C:\My Code\app", Some("npm run a;b"));
        if launch.file.to_lowercase().ends_with("wt.exe") {
            assert_eq!(launch.params, r#"-d "C:\My Code\app" cmd /k npm run a\;b"#);
        } else {
            assert_eq!(launch.params, "/k npm run a;b");
        }
        assert_eq!(launch.dir, r"C:\My Code\app");
    }

    #[test]
    fn web_launches_need_an_https_page() {
        assert!(plan(Launcher::Web, r"C:\x", None).is_err());
        assert!(plan(Launcher::Web, r"C:\x", Some("file:///C:/evil.exe")).is_err());
        let launch = plan(Launcher::Web, r"C:\x", Some("https://github.com/o/r")).unwrap();
        assert_eq!(launch.file, "https://github.com/o/r");
        let explorer = plan(Launcher::Explorer, r"C:\a b", None).unwrap();
        assert_eq!((explorer.file.as_str(), explorer.params.as_str()), ("explorer.exe", r#""C:\a b""#));
    }
}
