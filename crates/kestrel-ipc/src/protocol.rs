//! What travels over the pipe: one JSON request per line from the client, one JSON response
//! per line from the app. Every request only reads: nothing here can change a file.
//!
//! ```text
//! > {"op":"search","query":"ext:rs main","limit":5}
//! < {"ok":true,"result":{"query":"ext:rs main","total":12,"returned":5,"truncated":true,...}}
//! > {"op":"folder_size","path":"C:\\nowhere"}
//! < {"ok":false,"error":{"kind":"not_found","message":"C:\\nowhere is not in the index"}}
//! ```
//!
//! Each request carries its own limit on how many results come back, clamped to the ranges in
//! [`bounds`]; the server adds limits of its own on sizes and time (`server::Limits`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol's version, reported by [`Request::Hello`]. Raised when a change would break
/// an older client.
pub const PROTOCOL_VERSION: u32 = 1;

/// How many results each request returns: (by default, at most).
pub mod bounds {
    pub const SEARCH: (usize, usize) = (50, 500);
    pub const LIST_FOLDER: (usize, usize) = (200, 2000);
    pub const PROJECTS: (usize, usize) = (100, 500);
    pub const RECENT_FILES: (usize, usize) = (20, 200);
    /// Largest files and folders, and folders that changed, in a disk usage summary.
    pub const DISK_TOP: (usize, usize) = (10, 50);
    /// Characters of a project's README sent with its details.
    pub const README_CHARS: usize = 4000;
    /// Recently changed files sent with a project's details.
    pub const PROJECT_RECENT: usize = 15;

    /// `asked`, or the default, never above the maximum (nor below one).
    pub fn clamp(asked: Option<usize>, (default, max): (usize, usize)) -> usize {
        asked.unwrap_or(default).clamp(1, max)
    }
}

/// How results are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    #[default]
    Name,
    Size,
    Modified,
    /// By extension, then name.
    Type,
    /// By the folder an item is in, then name.
    Path,
}

/// One request. The `op` field says which.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The app's name, version and index state; a cheap way to see that it is there.
    Hello,
    Search(SearchArgs),
    ListFolder(ListArgs),
    FolderSize(PathArgs),
    Projects(ProjectsArgs),
    ProjectDetails(ProjectArgs),
    RecentFiles(LimitArgs),
    DiskUsage(DiskArgs),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchArgs {
    /// In Kestrel's query syntax (see the README's "Search syntax").
    pub query: String,
    /// Only beneath this folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descending: Option<bool>,
    /// Include hidden items (protected operating system files stay out regardless).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListArgs {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descending: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathArgs {
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectsArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Only projects whose name, folder, stack, tags, status or branch contain this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectArgs {
    /// The project's folder, or its name.
    pub project: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskArgs {
    pub path: String,
    /// How many of the largest files and folders to list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<usize>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Request {
    /// The request's name, as it travels.
    pub fn op(&self) -> &'static str {
        match self {
            Request::Hello => "hello",
            Request::Search(_) => "search",
            Request::ListFolder(_) => "list_folder",
            Request::FolderSize(_) => "folder_size",
            Request::Projects(_) => "projects",
            Request::ProjectDetails(_) => "project_details",
            Request::RecentFiles(_) => "recent_files",
            Request::DiskUsage(_) => "disk_usage",
        }
    }

    /// A short description for the app's log of queries: what was asked about.
    pub fn summary(&self) -> String {
        let text = match self {
            Request::Hello => String::new(),
            Request::Search(args) => match &args.folder {
                Some(folder) => format!("{} in {folder}", args.query),
                None => args.query.clone(),
            },
            Request::ListFolder(args) => args.path.clone(),
            Request::FolderSize(args) => args.path.clone(),
            Request::Projects(args) => args.filter.clone().unwrap_or_default(),
            Request::ProjectDetails(args) => args.project.clone(),
            Request::RecentFiles(_) => String::new(),
            Request::DiskUsage(args) => args.path.clone(),
        };
        // Kept short: it is a reminder, not a record.
        if text.chars().count() > 200 { text.chars().take(199).chain(['…']).collect() } else { text }
    }
}

// ---------- responses ----------

/// Why a request was not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The request was malformed or asked for something impossible (a bad query, say).
    BadRequest,
    /// The folder, file or project asked about is not there.
    NotFound,
    /// The request, or its answer, was larger than allowed.
    TooLarge,
    /// The answer took longer than allowed.
    Timeout,
    /// Too many requests at once; try again shortly.
    Busy,
    /// The app can't answer yet (its index is still loading).
    Unavailable,
    /// The owner turned the service off in Kestrel's settings.
    Off,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(FailureKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(FailureKind::NotFound, message)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// One response line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
}

impl Response {
    pub fn success(result: Value) -> Self {
        Self { ok: true, result: Some(result), error: None }
    }

    pub fn failure(failure: Failure) -> Self {
        Self { ok: false, result: None, error: Some(failure) }
    }

    pub fn into_result(self) -> Result<Value, Failure> {
        match (self.ok, self.result, self.error) {
            (true, Some(result), _) => Ok(result),
            (true, None, _) => Ok(Value::Null),
            (false, _, Some(error)) => Err(error),
            (false, _, None) => Err(Failure::new(FailureKind::Internal, "The app answered with an error but no message")),
        }
    }
}

// ---------- results ----------

/// What [`Request::Hello`] answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub app: String,
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    /// `loading`, `scanning` (the first index is being built) or `ready`.
    pub index: String,
    /// Files and folders in the index.
    pub entries: u64,
}

/// A file or folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub name: String,
    /// `file` or `folder`.
    pub kind: String,
    /// Bytes; for a folder, everything in it.
    pub size: u64,
    /// Bytes allocated on disk.
    pub size_on_disk: u64,
    /// Last modified, in UTC (ISO 8601), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Its contents are in the cloud only (OneDrive): opening it would download it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cloud_only: bool,
    /// For a search inside files (`content:`): the first line that matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<LineMatch>,
}

/// The line of a file a search inside it matched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineMatch {
    /// One-based.
    pub line: u64,
    /// The line, trimmed, and cut around the match when long.
    pub text: String,
    /// How many other lines match too.
    pub more_lines: u32,
}

impl Entry {
    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: String,
    /// What a phrase was read as ("pdfs from last week" runs as `ext:pdf dm:lastweek`), when
    /// that differs from the query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_as: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Everything that matched.
    pub total: u64,
    pub returned: usize,
    /// More matched than were returned.
    pub truncated: bool,
    pub took_ms: f64,
    pub results: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderListing {
    pub path: String,
    /// Bytes in the folder, everything in it included.
    pub size: u64,
    pub size_on_disk: u64,
    /// Read from the index (current within a second); otherwise read from the disk now, with
    /// subfolders' sizes unknown (0).
    pub indexed: bool,
    pub total: u64,
    pub returned: usize,
    pub truncated: bool,
    pub items: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderSize {
    pub path: String,
    pub size: u64,
    pub size_on_disk: u64,
    /// Files and folders beneath it.
    pub files: u64,
    pub folders: u64,
    /// Files whose contents are only in the cloud, counted in `files` but taking no space.
    pub cloud_only_files: u64,
    /// The counts are complete (they stop if counting takes too long).
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    pub folder: String,
    /// When it was last opened, in UTC (ISO 8601).
    pub opened: String,
    /// `windows` (Windows' Recent items) or `kestrel` (opened through Kestrel).
    pub source: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cloud_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentFiles {
    pub returned: usize,
    pub files: Vec<RecentFile>,
}

/// What git says about a project, in short.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GitSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub staged: u64,
    pub modified: u64,
    pub untracked: u64,
    pub conflicted: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
    /// When the last commit was made, in UTC (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_commit_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub root: String,
    /// Languages and build systems, such as `Rust`, `TypeScript`, `pnpm`.
    pub stack: Vec<String>,
    /// The owner's own status: active, paused, archived or idea.
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    pub is_repo: bool,
    /// Present once git has been read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSummary>,
    /// Last change to any of its files, in UTC (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    pub size_on_disk: u64,
    /// Of which build output (`node_modules`, `target`...).
    pub build_output: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectList {
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
    /// The index is still being built, so the list may be incomplete.
    pub indexing: bool,
    pub projects: Vec<ProjectSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Script {
    pub source: String,
    pub name: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectDetails {
    #[serde(flatten)]
    pub summary: ProjectSummary,
    /// Languages by share of source bytes, from 0 to 1.
    pub languages: Vec<(String, f64)>,
    pub scripts: Vec<Script>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme_path: Option<String>,
    /// The start of the README.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The files changed most recently that git does not ignore.
    pub recent_files: Vec<Entry>,
    /// Build output folders and their size on disk.
    pub build_output_folders: Vec<(String, u64)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    pub label: String,
    pub size_on_disk: u64,
    pub size: u64,
    pub files: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drive {
    pub root: String,
    pub total: u64,
    pub free: u64,
    pub file_system: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiskUsage {
    pub path: String,
    pub size_on_disk: u64,
    pub size: u64,
    pub files: u64,
    pub folders: u64,
    pub cloud_only_files: u64,
    /// The index is still being built, so these numbers will grow.
    pub indexing: bool,
    /// By type: video, images, documents, code, build output...
    pub types: Vec<Amount>,
    /// By when files last changed.
    pub ages: Vec<Amount>,
    pub largest_files: Vec<Entry>,
    pub largest_folders: Vec<Entry>,
    /// The drive's own figures, when the path is a drive's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<Drive>,
}

// ---------- dates ----------

/// Milliseconds since the Unix epoch as `2026-10-15T09:30:00Z`; `None` for 0 or less, which
/// the index uses for "unknown".
pub fn iso_utc(unix_ms: i64) -> Option<String> {
    if unix_ms <= 0 {
        return None;
    }
    let seconds = unix_ms.div_euclid(1000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    Some(format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60))
}

/// The proleptic Gregorian date of a day counted from 1970-01-01 (Howard Hinnant's method).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_travel_as_flat_json_with_their_op() {
        let request = Request::Search(SearchArgs { query: "ext:rs".into(), limit: Some(5), ..SearchArgs::default() });
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(text, r#"{"op":"search","query":"ext:rs","limit":5}"#);
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        assert_eq!(serde_json::from_str::<Request>(r#"{"op":"hello"}"#).unwrap(), Request::Hello);
        let listing: Request = serde_json::from_value(json!({"op": "list_folder", "path": "C:\\x", "sort": "size", "descending": true})).unwrap();
        assert_eq!(
            listing,
            Request::ListFolder(ListArgs { path: r"C:\x".into(), sort: Some(SortBy::Size), descending: Some(true), ..ListArgs::default() })
        );
        // Unknown fields are ignored, so a newer client can add some.
        assert!(serde_json::from_str::<Request>(r#"{"op":"recent_files","limit":3,"colour":"blue"}"#).is_ok());
        // Unknown operations are not: in particular, nothing that would change a file exists.
        assert!(serde_json::from_str::<Request>(r#"{"op":"delete","path":"C:\\x"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"folder_size"}"#).is_err(), "a path is required");
    }

    #[test]
    fn limits_have_defaults_and_ceilings() {
        assert_eq!(bounds::clamp(None, bounds::SEARCH), 50);
        assert_eq!(bounds::clamp(Some(10), bounds::SEARCH), 10);
        assert_eq!(bounds::clamp(Some(1_000_000), bounds::SEARCH), 500);
        assert_eq!(bounds::clamp(Some(0), bounds::SEARCH), 1);
        assert_eq!(bounds::clamp(Some(5000), bounds::LIST_FOLDER), 2000);
    }

    #[test]
    fn responses_carry_a_result_or_a_failure() {
        let ok = Response::success(json!({"a": 1}));
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"ok":true,"result":{"a":1}}"#);
        assert_eq!(ok.into_result().unwrap(), json!({"a": 1}));
        let failed = Response::failure(Failure::not_found("gone"));
        assert_eq!(serde_json::to_string(&failed).unwrap(), r#"{"ok":false,"error":{"kind":"not_found","message":"gone"}}"#);
        assert_eq!(failed.into_result().unwrap_err().kind, FailureKind::NotFound);
    }

    #[test]
    fn summaries_are_short() {
        let long = Request::Search(SearchArgs { query: "x".repeat(500), ..SearchArgs::default() });
        assert_eq!(long.summary().chars().count(), 200);
        let scoped = Request::Search(SearchArgs { query: "a".into(), folder: Some(r"C:\b".into()), ..SearchArgs::default() });
        assert_eq!(scoped.summary(), r"a in C:\b");
    }

    #[test]
    fn dates_are_iso_utc() {
        assert_eq!(iso_utc(0), None);
        assert_eq!(iso_utc(1).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(iso_utc(951_782_400_000).as_deref(), Some("2000-02-29T00:00:00Z"));
        assert_eq!(iso_utc(1_792_063_845_000).as_deref(), Some("2026-10-15T11:30:45Z"));
    }
}
