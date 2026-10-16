//! Developer tools: the terminal pane, and Kestrel's index for other programs.
//!
//! - [`terminal`]: shells in pseudoconsoles for the terminal pane (`kestrel_term`), owned by
//!   the window that opened them and ended with it.
//! - [`query`]: the answers of the local query service (`kestrel_ipc`): search, folder
//!   listings with sizes, folder sizes, projects, recent files and disk usage, read-only, for
//!   other programs of this account over a named pipe only this account can open.
//!   `kestrel-cli --live` and `kestrel-mcp` (the MCP server, `apps/mcp`) ask through it.
//! - [`QueryService`]: starts and stops the pipe server as the settings say, and keeps a log
//!   of what was asked (`kestrel_db::devtools`).
//! - The commands of the Developer tools page: settings, where `kestrel-mcp.exe` is and the
//!   `claude mcp add` command for it, and the log.

pub mod clipboard;
pub mod query;
pub mod terminal;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kestrel_db::Db;
use kestrel_db::devtools::{self as store, Settings};
use kestrel_ipc::server::{Handler, Observer};
use kestrel_ipc::{Context, Failure, Limits, Request, Server};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Manager, State, Window, WindowEvent};

pub use terminal::Terminals;

/// The pipe server, while the settings let it run.
pub struct QueryService {
    name: Option<String>,
    server: Mutex<Option<Server>>,
    /// Why it isn't running, when it should be.
    problem: Mutex<Option<String>>,
}

impl QueryService {
    /// For the Kestrel whose data folder is `data_dir`.
    pub fn new(data_dir: &Path) -> Self {
        let name = kestrel_ipc::name::pipe_name(data_dir);
        let problem = name.as_ref().err().map(|error| format!("Couldn't work out the pipe's name: {error}"));
        Self { name: name.ok(), server: Mutex::new(None), problem: Mutex::new(problem) }
    }

    pub fn pipe_name(&self) -> Option<String> {
        self.name.clone()
    }

    pub fn running(&self) -> bool {
        self.server.lock().is_some()
    }

    /// Starts or stops the server to match `on`.
    pub fn apply(&self, app: &AppHandle, on: bool) {
        let mut server = self.server.lock();
        if !on {
            // Dropping the server stops it.
            server.take();
            return;
        }
        let Some(name) = &self.name else { return };
        if server.is_some() {
            return;
        }
        let answering = app.clone();
        let handler: Arc<Handler> = Arc::new(move |request: Request, context: &Context| query::answer(&answering, request, context));
        let logging = app.clone();
        let observer: Arc<Observer> = Arc::new(move |request: &Request, context: &Context, outcome: Result<(), &Failure>, took: Duration| {
            log_query(&logging, request, context, outcome, took);
        });
        match Server::start(name, Limits::default(), handler, Some(observer)) {
            Ok(started) => {
                *server = Some(started);
                *self.problem.lock() = None;
            }
            Err(error) => {
                let message = format!("The query service couldn't start: {error}");
                eprintln!("{message}");
                *self.problem.lock() = Some(message);
            }
        }
    }
}

fn log_query(app: &AppHandle, request: &Request, context: &Context, outcome: Result<(), &Failure>, took: Duration) {
    let db = app.state::<Arc<Db>>();
    if !store::settings(&db).map(|settings| settings.log_queries).unwrap_or(true) {
        return;
    }
    let client = if context.client_name.is_empty() { "another program".to_owned() } else { context.client_name.clone() };
    let query = store::Query {
        at: crate::parity::now_ms(),
        client,
        op: request.op().to_owned(),
        summary: request.summary(),
        ok: outcome.is_ok(),
        took_ms: took.as_secs_f64() * 1000.0,
    };
    let _ = store::record_query(&db, &query);
}

/// Starts what the developer tools run in the background: the query service, if the settings
/// let it.
pub fn setup(app: &AppHandle) {
    let on = store::settings(&app.state::<Arc<Db>>()).map(|settings| settings.query_service).unwrap_or(true);
    app.state::<Arc<QueryService>>().apply(app, on);
}

/// A window closing (or hiding in the tray) ends its terminals.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if matches!(event, WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed)
        && let Some(terminals) = window.app_handle().try_state::<Arc<Terminals>>()
    {
        terminals.close_window(window.label());
    }
}

/// Kestrel is exiting: every terminal ends, and the pipe goes.
pub fn on_exit(app: &AppHandle) {
    if let Some(terminals) = app.try_state::<Arc<Terminals>>() {
        terminals.close_all();
    }
    if let Some(service) = app.try_state::<Arc<QueryService>>() {
        service.apply(app, false);
    }
}

// ---------- the Developer tools page ----------

/// A program of Kestrel's that lives beside `kestrel.exe`, if it is there.
fn beside_app(name: &str) -> Option<PathBuf> {
    let found = std::env::current_exe().ok()?.parent()?.join(name);
    found.is_file().then_some(found)
}

/// Whether Kestrel uses the default data folder, which clients find by themselves.
fn default_data_folder() -> bool {
    std::env::var_os("KESTREL_DATA_DIR").is_none()
}

/// The command that adds the MCP server to Claude Code, for every project of this account.
/// The pipe is named only when this Kestrel has a data folder of its own (a test instance),
/// which the server could not otherwise find.
pub fn claude_command(mcp: &Path, pipe: Option<&str>) -> String {
    let mut command = format!("claude mcp add --scope user kestrel -- \"{}\"", mcp.display());
    if let Some(pipe) = pipe {
        command.push_str(&format!(" --pipe \"{pipe}\""));
    }
    command
}

#[derive(Serialize)]
pub struct DevtoolsState {
    settings: Settings,
    shells: terminal::ShellList,
    /// The query service answers.
    service_running: bool,
    /// Why it doesn't, when it should.
    service_problem: Option<String>,
    pipe: Option<String>,
    /// `kestrel-mcp.exe` beside `kestrel.exe`, when it was built.
    mcp_path: Option<String>,
    /// Where it would be.
    mcp_expected: String,
    claude_command: String,
    cli_path: Option<String>,
}

fn state_of(app: &AppHandle, db: &Db) -> DevtoolsState {
    let service = app.state::<Arc<QueryService>>();
    let pipe = service.pipe_name();
    let expected = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("kestrel-mcp.exe"))).unwrap_or_else(|| PathBuf::from("kestrel-mcp.exe"));
    let mcp = beside_app("kestrel-mcp.exe");
    let named = if default_data_folder() { None } else { pipe.as_deref() };
    DevtoolsState {
        settings: store::settings(db).unwrap_or_default(),
        shells: terminal::shell_list(db),
        service_running: service.running(),
        service_problem: service.problem.lock().clone(),
        claude_command: claude_command(mcp.as_deref().unwrap_or(&expected), named),
        mcp_path: mcp.map(|path| path.display().to_string()),
        mcp_expected: expected.display().to_string(),
        cli_path: beside_app("kestrel-cli.exe").map(|path| path.display().to_string()),
        pipe,
    }
}

#[tauri::command(async)]
pub fn devtools_state(app: AppHandle, db: State<Arc<Db>>) -> DevtoolsState {
    state_of(&app, &db)
}

/// Saves the settings and starts or stops the query service to match.
#[tauri::command(async)]
pub fn devtools_set_settings(app: AppHandle, db: State<Arc<Db>>, settings: Settings) -> Result<DevtoolsState, String> {
    store::set_settings(&db, &settings)?;
    app.state::<Arc<QueryService>>().apply(&app, settings.query_service);
    Ok(state_of(&app, &db))
}

/// The latest questions other programs asked, newest first.
#[tauri::command(async)]
pub fn devtools_queries(db: State<Arc<Db>>, limit: usize) -> Result<Vec<store::Query>, String> {
    Ok(store::recent_queries(&db, limit.clamp(1, store::MAX_QUERIES))?)
}

#[tauri::command(async)]
pub fn devtools_clear_queries(db: State<Arc<Db>>) -> Result<usize, String> {
    Ok(store::clear_queries(&db)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claude_command_names_the_server_and_only_a_test_instance_names_its_pipe() {
        let exe = Path::new(r"C:\Program Files\Kestrel\kestrel-mcp.exe");
        assert_eq!(claude_command(exe, None), r#"claude mcp add --scope user kestrel -- "C:\Program Files\Kestrel\kestrel-mcp.exe""#);
        assert_eq!(
            claude_command(exe, Some(r"\\.\pipe\kestrel-query-x")),
            r#"claude mcp add --scope user kestrel -- "C:\Program Files\Kestrel\kestrel-mcp.exe" --pipe "\\.\pipe\kestrel-query-x""#
        );
    }
}
