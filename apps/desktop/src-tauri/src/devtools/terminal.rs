//! The terminal pane's backend: shells in pseudoconsoles (`kestrel_term`), each owned by the
//! window that opened it, their output streamed to that window's page through a channel.
//!
//! A session ends when its tab closes (`terminal_close`), when its window closes or is hidden
//! to the tray (`on_window_event`), when its page loads again (`terminal_reset`: the page that
//! drew it is gone) and when Kestrel exits (`close_all`). Ending one ends the console programs
//! it started; see `kestrel_term::pty`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use kestrel_db::Db;
use kestrel_term::{Event, Session, Shell, Size, shells};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{State, Window};

/// One open terminal.
struct Term {
    window: String,
    session: Session,
    shell: Shell,
}

#[derive(Default)]
pub struct Terminals {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Term>>,
}

impl Terminals {
    fn take(&self, keep: impl Fn(u32, &Term) -> bool) -> Vec<Term> {
        let mut open = self.open.lock();
        let ids: Vec<u32> = open.iter().filter(|(id, term)| !keep(**id, term)).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| open.remove(&id)).collect()
    }

    /// Ends every terminal of `window`.
    pub fn close_window(&self, window: &str) {
        for term in self.take(|_, term| term.window != window) {
            term.session.close();
        }
    }

    /// Ends every terminal, as Kestrel exits.
    pub fn close_all(&self) {
        for term in self.take(|_, _| false) {
            term.session.close();
        }
    }
}

pub type Shared<'a> = State<'a, Arc<Terminals>>;

/// What a terminal sends its page.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TermEvent {
    Output { data: String },
    /// The shell exited (or was ended); nothing follows.
    Exit { code: Option<u32> },
}

/// One of a project's scripts, run by the projects hub's Run.
#[derive(Deserialize)]
pub struct ScriptRef {
    pub root: String,
    pub source: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct Opened {
    pub id: u32,
    pub pid: u32,
    /// The shell's id and name.
    pub shell: String,
    pub shell_name: String,
    pub cwd: String,
    /// What the tab is called: the shell, or the script it runs.
    pub title: String,
}

/// Turns bytes into text as they arrive, holding back a character cut in two between reads.
#[derive(Default)]
pub struct Utf8 {
    carry: Vec<u8>,
}

impl Utf8 {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        match std::str::from_utf8(&self.carry) {
            Ok(text) => {
                let text = text.to_owned();
                self.carry.clear();
                text
            }
            // Only the end is incomplete: keep it for the next read.
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                let text = String::from_utf8_lossy(&self.carry[..valid]).into_owned();
                self.carry.drain(..valid);
                text
            }
            Err(_) => String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned(),
        }
    }
}

/// The folder a terminal opens in: the one asked for if it is a folder, else the user folder.
fn start_folder(asked: Option<&str>) -> PathBuf {
    asked
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

/// Variables the shell should not inherit from Kestrel, and those it gets: a test instance's
/// debugging port must not reach programs started in its terminal, and `kestrel-cli --live`
/// run in the terminal asks this Kestrel.
fn environment(pipe: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut env = vec![("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".to_owned(), None)];
    if let Some(pipe) = pipe {
        env.push(("KESTREL_PIPE".to_owned(), Some(pipe.to_owned())));
    }
    env
}

/// The shells installed, the default (as the settings choose it, else PowerShell 7 if
/// installed, else Windows PowerShell) first among equals: `default` names it.
#[derive(Serialize)]
pub struct ShellList {
    pub shells: Vec<Shell>,
    pub default: Option<String>,
}

pub fn shell_list(db: &Db) -> ShellList {
    let shells = shells::detect();
    let chosen = kestrel_db::devtools::settings(db).ok().and_then(|settings| settings.default_shell);
    let default = shells::pick(&shells, chosen.as_deref()).map(|shell| shell.id.clone());
    ShellList { shells, default }
}

#[tauri::command(async)]
pub fn terminal_shells(db: State<Arc<Db>>) -> ShellList {
    shell_list(&db)
}

/// Opens a terminal in `cwd` (the user folder if that is no folder) with `shell` (the default
/// if unset), or, given `script`, runs one of a project's own scripts in Command Prompt there. A
/// library's address opens in its save folder, and a folder inside a WSL distribution in that
/// distribution's own shell unless another was asked for.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub fn terminal_open(
    window: Window,
    terminals: Shared,
    db: State<Arc<Db>>,
    service: State<Arc<super::QueryService>>,
    cwd: Option<String>,
    shell: Option<String>,
    script: Option<ScriptRef>,
    cols: u16,
    rows: u16,
    on_event: Channel<TermEvent>,
) -> Result<Opened, String> {
    let installed = shells::detect();
    let chosen = shell.or_else(|| kestrel_db::devtools::settings(&db).ok().and_then(|settings| settings.default_shell));
    let mut picked = shells::pick(&installed, chosen.as_deref()).cloned().ok_or("No shell is installed")?;
    let (mut command, title) = match &script {
        Some(script) => {
            // Only a script the project's own files define, as the hub's Run in Windows Terminal.
            let root = PathBuf::from(&script.root);
            let found = crate::projects::scripts::detect(&root)
                .into_iter()
                .find(|s| s.source == script.source && s.name == script.name)
                .ok_or_else(|| format!("{} is not one of this project's scripts", script.name))?;
            if let Some(cmd) = installed.iter().find(|s| s.id == "cmd") {
                picked = cmd.clone();
            }
            (shells::run_in_cmd(&found.command, &root), found.command)
        }
        None => {
            let asked = cwd.as_deref().map(crate::parity2::libraries::folder_for);
            if shell.is_none()
                && let Some(own) = asked.as_deref().and_then(|folder| shells::for_wsl_folder(&installed, folder))
            {
                picked = own.clone();
            }
            let folder = start_folder(asked.as_deref());
            (shells::launch(&picked, &folder), picked.name.clone())
        }
    };
    if !command.cwd.is_dir() {
        return Err(format!("{} is not a folder", command.cwd.display()));
    }
    command.env = environment(service.pipe_name().as_deref());

    let mut text = Utf8::default();
    let channel = on_event.clone();
    let session = Session::spawn(&command, Size { cols, rows }, move |event| match event {
        Event::Output(bytes) => {
            let data = text.push(bytes);
            if !data.is_empty() {
                let _ = channel.send(TermEvent::Output { data });
            }
        }
        Event::Exit(code) => {
            let _ = channel.send(TermEvent::Exit { code });
        }
    })
    .map_err(|error| error.to_string())?;
    let id = terminals.next.fetch_add(1, Ordering::Relaxed) + 1;
    let opened = Opened {
        id,
        pid: session.pid(),
        shell: picked.id.clone(),
        shell_name: picked.name.clone(),
        cwd: command.cwd.display().to_string(),
        title,
    };
    terminals.open.lock().insert(id, Term { window: window.label().to_owned(), session, shell: picked });
    Ok(opened)
}

/// Runs `f` with the terminal `id`, if it belongs to `window`.
fn with<T>(terminals: &Terminals, window: &Window, id: u32, f: impl FnOnce(&Term) -> T) -> Result<T, String> {
    let open = terminals.open.lock();
    match open.get(&id) {
        Some(term) if term.window == window.label() => Ok(f(term)),
        _ => Err("That terminal has closed".into()),
    }
}

/// Keys typed, or text pasted, into a terminal.
#[tauri::command(async)]
pub fn terminal_write(window: Window, terminals: Shared, id: u32, data: String) -> Result<(), String> {
    // The write may wait for the shell to read; the list isn't held meanwhile.
    let session = {
        let open = terminals.open.lock();
        match open.get(&id) {
            Some(term) if term.window == window.label() => term.session.writer(),
            _ => return Err("That terminal has closed".into()),
        }
    };
    session.write(data.as_bytes()).map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn terminal_resize(window: Window, terminals: Shared, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    with(&terminals, &window, id, |term| term.session.resize(Size { cols, rows }))?.map_err(|error| error.to_string())
}

/// Ends a terminal and the console programs it started.
#[tauri::command(async)]
pub fn terminal_close(window: Window, terminals: Shared, id: u32) {
    let label = window.label().to_owned();
    for term in terminals.take(|other, term| other != id || term.window != label) {
        term.session.close();
    }
}

/// Ends every terminal of this window: its page has just loaded, so any it had belonged to the
/// page before (a reload).
#[tauri::command(async)]
pub fn terminal_reset(window: Window, terminals: Shared) {
    terminals.close_window(window.label());
}

/// Types the command that takes the terminal's shell to `folder`.
#[tauri::command(async)]
pub fn terminal_cd(window: Window, terminals: Shared, id: u32, folder: String) -> Result<(), String> {
    let folder = crate::parity2::libraries::folder_for(&folder);
    if !Path::new(&folder).is_dir() {
        return Err(format!("{folder} is not a folder"));
    }
    let (kind, distribution, session) = with(&terminals, &window, id, |term| {
        (term.shell.kind, term.shell.id.strip_prefix("wsl:").map(str::to_owned), term.session.writer())
    })?;
    let line = shells::cd_command(kind, distribution.as_deref(), &folder).ok_or_else(|| match kind {
        kestrel_term::Kind::Cmd => "Command Prompt can't be taken to a folder whose name has a % in it".to_owned(),
        _ => format!("This shell can't be taken to {folder}"),
    })?;
    session.write(line.as_bytes()).map_err(|error| error.to_string())
}

/// What a path printed in a terminal is: `folder`, `file`, or nothing (`null`) when there is
/// nothing there or the disk did not say in time. Only attributes are read, so nothing is
/// downloaded; shares are never asked about, so text a program prints can't make Kestrel
/// contact a server.
#[tauri::command(async)]
pub fn terminal_paths(paths: Vec<String>) -> Vec<Option<&'static str>> {
    let paths: Vec<String> = paths.into_iter().take(200).collect();
    let local: Vec<String> = paths.iter().filter(|path| !super::query::is_share(path)).cloned().collect();
    let mut answers = crate::remote::ask_all(&local, crate::parity::attributes, Duration::from_millis(400)).into_iter();
    paths
        .iter()
        .map(|path| {
            if super::query::is_share(path) {
                return None;
            }
            answers.next().flatten().flatten().map(|attrs| if attrs & 0x10 != 0 { "folder" } else { "file" })
        })
        .collect()
}

/// Text on the clipboard, for pasting into a terminal from its menu (Ctrl+V pastes through the
/// web view itself). `None` when it holds no text.
#[tauri::command(async)]
pub fn terminal_clipboard_text() -> Option<String> {
    super::clipboard::text()
}

/// A terminal as the development hook reports it.
#[derive(Serialize)]
pub struct Running {
    id: u32,
    window: String,
    shell: String,
    pid: u32,
    /// Every process the session's job holds: the shell and what it started.
    processes: Vec<u32>,
}

/// Development builds only: the open terminals and their processes, for the end-to-end tests.
#[tauri::command(async)]
pub fn terminal_dev_sessions(terminals: Shared) -> Result<Vec<Running>, String> {
    crate::projects::launch::development_only()?;
    let open = terminals.open.lock();
    let mut running: Vec<Running> = open
        .iter()
        .map(|(id, term)| Running { id: *id, window: term.window.clone(), shell: term.shell.id.clone(), pid: term.session.pid(), processes: term.session.processes() })
        .collect();
    running.sort_by_key(|term| term.id);
    Ok(running)
}

/// Development builds only: whether each process is still running.
#[tauri::command(async)]
pub fn terminal_dev_alive(pids: Vec<u32>) -> Result<Vec<bool>, String> {
    crate::projects::launch::development_only()?;
    Ok(pids.into_iter().map(kestrel_term::pty::is_alive).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_cut_between_reads_are_kept_whole() {
        let mut text = Utf8::default();
        let word = "héllo ✓".as_bytes();
        // "é" is two bytes and "✓" three: cut inside each.
        assert_eq!(text.push(&word[..2]), "h");
        assert_eq!(text.push(&word[2..8]), "éllo ");
        assert_eq!(text.push(&word[8..9]), "");
        assert_eq!(text.push(&word[9..]), "✓");
        // Bytes that are no character at all are replaced rather than held forever.
        assert_eq!(text.push(&[b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn terminals_open_in_a_folder_or_the_user_folder() {
        let temp = std::env::temp_dir();
        assert_eq!(start_folder(temp.to_str()), temp);
        let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
        assert_eq!(start_folder(Some("Home")), home, "a special location is no folder");
        assert_eq!(start_folder(Some(r"C:\no\such\folder")), home);
        assert_eq!(start_folder(None), home);
    }

    #[test]
    fn the_shell_gets_the_pipe_and_not_the_debugging_port() {
        let env = environment(Some(r"\\.\pipe\kestrel-query-x"));
        assert!(env.contains(&("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".to_owned(), None)));
        assert!(env.contains(&("KESTREL_PIPE".to_owned(), Some(r"\\.\pipe\kestrel-query-x".to_owned()))));
        assert_eq!(environment(None).len(), 1);
    }
}
