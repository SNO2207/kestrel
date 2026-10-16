//! `kestrel-mcp`: an MCP server (stdio) that lets Claude Code, and any other MCP client, use
//! Kestrel's index: search every file on the PC, list folders with their sizes, add folders up,
//! and see the projects, the recent files and what uses the disk.
//!
//! It holds no index of its own: each tool asks the running Kestrel over its query pipe
//! (`kestrel_ipc`), which only this account can open. Every tool only reads; nothing here can
//! change a file. Add it to Claude Code with the command Kestrel's Developer tools page shows:
//!
//! ```text
//! claude mcp add --scope user kestrel -- "C:\path\to\kestrel-mcp.exe"
//! ```
//!
//! `--pipe NAME` (or `KESTREL_PIPE`) points it at another Kestrel than the owner's own, such as
//! a test instance with a data folder of its own.

use std::time::Duration;

use kestrel_ipc::protocol::{self as wire, Request};
use kestrel_ipc::{CallError, FailureKind};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

/// How long a tool waits for Kestrel: its own limit is 10 seconds a question.
const TIMEOUT: Duration = Duration::from_secs(15);

const INSTRUCTIONS: &str = "Kestrel is the file explorer running on this Windows PC. Its index holds every file and \
folder on the PC's fixed drives and is kept current within a second, so these tools answer in milliseconds without \
walking the disk. Prefer search_files over listing folders one by one to find files anywhere on the PC, folder_size \
over adding up a folder yourself, and list_projects to find code projects. Paths are Windows paths (C:\\Users\\...). \
Sizes are in bytes; a folder's size is everything in it. Dates are UTC. Every tool only reads: none changes a file. \
Files marked cloud_only are OneDrive placeholders whose contents are not on this PC: opening one downloads it.";

const SEARCH_DESCRIPTION: &str = "Search every file and folder on this PC by name, path, type, size or date, through \
Kestrel's live index (answers in milliseconds, and current within a second). The query is in Kestrel's syntax: \
clauses separated by spaces must all match, and `!` before a clause negates it.\n\
- `report`: the name contains report (case-insensitive)\n\
- `\"two words\"`: quoted text is taken literally\n\
- `src\\main`: the full path contains src\\main\n\
- `*.rs`, `img_????.jpg`: wildcards over the whole name\n\
- `ext:rs;toml`: the extension is one of these\n\
- `type:image`: one of image, video, audio, doc, archive, code, exe, folder, file\n\
- `size:>1gb`, `size:10mb..2gb`, `size:huge`: size\n\
- `dm:today`, `dm:7d`, `dm:>2026-01`, `dm:2025`: date modified\n\
- `in:C:\\Projects`: only beneath this folder (or use the folder argument)\n\
- `regex:^a.*\\d$`: a regular expression on the name\n\
- `tag:urgent`: carries this tag given in Kestrel\n\
Examples: `ext:pdf invoice dm:2025`, `*.log size:>100mb`, `type:folder node_modules in:C:\\Code`, `Cargo.toml !target`. \
Returns the total number of matches and the first results (path, kind, size in bytes, size on disk, modified in \
UTC). Hidden files are left out unless include_hidden is true.";

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SortField {
    Name,
    Size,
    Modified,
    Type,
    Path,
}

impl From<SortField> for wire::SortBy {
    fn from(field: SortField) -> Self {
        match field {
            SortField::Name => wire::SortBy::Name,
            SortField::Size => wire::SortBy::Size,
            SortField::Modified => wire::SortBy::Modified,
            SortField::Type => wire::SortBy::Type,
            SortField::Path => wire::SortBy::Path,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchFiles {
    /// The search, in Kestrel's query syntax (see the tool's description).
    query: String,
    /// Only results beneath this folder, an absolute Windows path such as C:\Users\me\Code.
    #[serde(default)]
    folder: Option<String>,
    /// How many results to return, 1 to 500. Default 50.
    #[serde(default)]
    limit: Option<u32>,
    /// Order of the results. Default name; size and modified start with the largest and newest.
    #[serde(default)]
    sort: Option<SortField>,
    /// Reverse the order.
    #[serde(default)]
    descending: Option<bool>,
    /// Include hidden files and folders. Default false.
    #[serde(default)]
    include_hidden: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListFolder {
    /// The folder, an absolute Windows path such as C:\Users\me\Downloads.
    path: String,
    /// How many items to return, 1 to 2000. Default 200.
    #[serde(default)]
    limit: Option<u32>,
    /// Order (folders always come first). Default name; size and modified start with the largest and newest.
    #[serde(default)]
    sort: Option<SortField>,
    /// Reverse the order.
    #[serde(default)]
    descending: Option<bool>,
    /// Include hidden files and folders. Default false.
    #[serde(default)]
    include_hidden: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FolderSize {
    /// The folder (or file), an absolute Windows path.
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListProjects {
    /// Only projects whose name, folder, stack (such as Rust or TypeScript), tags, status or git branch contain this.
    #[serde(default)]
    filter: Option<String>,
    /// How many projects to return, 1 to 500. Default 100.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProjectDetails {
    /// The project's folder (or any folder inside it), or its name as list_projects gives it.
    project: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecentFiles {
    /// How many files to return, 1 to 200. Default 20.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DiskUsage {
    /// A drive (C:\) or folder, an absolute Windows path.
    path: String,
    /// How many of the largest files and folders to list, 1 to 50. Default 10.
    #[serde(default)]
    top: Option<u32>,
}

fn limit(value: Option<u32>) -> Option<usize> {
    value.map(|n| n as usize)
}

/// What a client is told when Kestrel could not answer.
fn explain(error: &CallError) -> String {
    match error {
        CallError::NotRunning => "Kestrel isn't running, or its query service is turned off (Developer tools in Kestrel). \
            Start Kestrel and try again."
            .into(),
        CallError::Failed(failure) if failure.kind == FailureKind::Unavailable => failure.message.clone(),
        CallError::Failed(failure) => failure.message.clone(),
        other => other.to_string(),
    }
}

#[derive(Clone)]
struct Kestrel {
    pipe: String,
    tool_router: ToolRouter<Self>,
}

impl Kestrel {
    fn new(pipe: String) -> Self {
        Self { pipe, tool_router: Self::tool_router() }
    }

    /// Asks Kestrel, on a thread that may wait, and turns the answer into the tool's result:
    /// the answer as compact JSON, or an error the client shows.
    async fn ask(&self, request: Request) -> Result<CallToolResult, McpError> {
        let pipe = self.pipe.clone();
        let answer = tokio::task::spawn_blocking(move || kestrel_ipc::call(&pipe, &request, TIMEOUT))
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(match answer {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(explain(&error))]),
        })
    }
}

#[tool_router]
impl Kestrel {
    #[tool(
        description = SEARCH_DESCRIPTION,
        annotations(title = "Search files", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn search_files(&self, Parameters(args): Parameters<SearchFiles>) -> Result<CallToolResult, McpError> {
        self.ask(Request::Search(wire::SearchArgs {
            query: args.query,
            folder: args.folder,
            limit: limit(args.limit),
            sort: args.sort.map(Into::into),
            descending: args.descending,
            hidden: args.include_hidden.unwrap_or(false),
        }))
        .await
    }

    #[tool(
        description = "List a folder's files and subfolders with their sizes and dates. A subfolder's size is everything in it, \
            from Kestrel's index, so this is the quick way to see what is big in a folder (sort by size). Folders come first. \
            A folder outside the index (a network share, say) is read from disk, without subfolder sizes.",
        annotations(title = "List folder", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_folder(&self, Parameters(args): Parameters<ListFolder>) -> Result<CallToolResult, McpError> {
        self.ask(Request::ListFolder(wire::ListArgs {
            path: args.path,
            limit: limit(args.limit),
            sort: args.sort.map(Into::into),
            descending: args.descending,
            hidden: args.include_hidden.unwrap_or(false),
        }))
        .await
    }

    #[tool(
        description = "The total size of a folder (in bytes, and bytes on disk) and how many files and folders are beneath it, \
            from Kestrel's index, without walking the disk: even a whole drive takes well under a second.",
        annotations(title = "Folder size", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn folder_size(&self, Parameters(args): Parameters<FolderSize>) -> Result<CallToolResult, McpError> {
        self.ask(Request::FolderSize(wire::PathArgs { path: args.path })).await
    }

    #[tool(
        description = "The code projects on this PC, as Kestrel finds them in its index (folders with .git, Cargo.toml, \
            package.json, pyproject.toml, go.mod, CMakeLists.txt, a .sln or .csproj, and so on): each one's name, folder, \
            stack, git branch with commits ahead and behind and uncommitted changes, last commit, last activity, size on disk \
            and how much of it is build output, and the owner's status and tags. Pinned first, then the most recently active.",
        annotations(title = "List projects", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_projects(&self, Parameters(args): Parameters<ListProjects>) -> Result<CallToolResult, McpError> {
        self.ask(Request::Projects(wire::ProjectsArgs { limit: limit(args.limit), filter: args.filter })).await
    }

    #[tool(
        description = "One project's details: its git status, languages by share, the scripts it defines (package.json \
            scripts, Cargo's build, test and run, justfile recipes, Makefile targets) with the command each runs, the start of \
            its README, the owner's notes, the files changed most recently that git does not ignore, and its build output folders.",
        annotations(title = "Project details", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn project_details(&self, Parameters(args): Parameters<ProjectDetails>) -> Result<CallToolResult, McpError> {
        self.ask(Request::ProjectDetails(wire::ProjectArgs { project: args.project })).await
    }

    #[tool(
        description = "The files opened most recently on this PC, newest first: Windows' Recent items (files opened in any \
            program) and the files opened through Kestrel, with when each was opened (UTC).",
        annotations(title = "Recent files", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn recent_files(&self, Parameters(args): Parameters<RecentFiles>) -> Result<CallToolResult, McpError> {
        self.ask(Request::RecentFiles(wire::LimitArgs { limit: limit(args.limit) })).await
    }

    #[tool(
        description = "What uses the disk in a drive or folder, from Kestrel's index: the size on disk and the number of \
            files and folders, a breakdown by type (video, images, audio, documents, archives, code, programs, disk images, \
            developer build output, system, other) and by age, the largest files and folders, and for a drive its capacity \
            and free space.",
        annotations(title = "Disk usage", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn disk_usage(&self, Parameters(args): Parameters<DiskUsage>) -> Result<CallToolResult, McpError> {
        self.ask(Request::DiskUsage(wire::DiskArgs { path: args.path, top: limit(args.top) })).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Kestrel {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kestrel", env!("CARGO_PKG_VERSION")).with_title("Kestrel"))
            .with_instructions(INSTRUCTIONS)
    }
}

const USAGE: &str = "kestrel-mcp: an MCP server (stdio) that searches and sizes files through a running Kestrel.

Usage: kestrel-mcp [--pipe NAME]

Add it to Claude Code with:
  claude mcp add --scope user kestrel -- \"<path to kestrel-mcp.exe>\"

--pipe NAME   ask the Kestrel listening on this pipe (also KESTREL_PIPE), rather than your own";

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut pipe = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pipe" => pipe = args.next(),
            "--help" | "-h" => {
                println!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("kestrel-mcp {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("kestrel-mcp: unknown argument {other}\n\n{USAGE}");
                return std::process::ExitCode::from(2);
            }
        }
    }
    let pipe = match kestrel_ipc::name::resolve(pipe.as_deref()) {
        Ok(pipe) => pipe,
        Err(error) => {
            eprintln!("kestrel-mcp: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("kestrel-mcp: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let served = runtime.block_on(async move {
        let service = Kestrel::new(pipe).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    match served {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Standard output belongs to the protocol; complaints go to standard error.
            eprintln!("kestrel-mcp: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
