//! The answers of the query service (`kestrel_ipc`): what other programs of this account may
//! ask the running app, read from the same index, projects and database the window uses.
//!
//! Everything here only reads. The index is read in place: no result set is stored (the
//! engine keeps few, for the window), the window's own search is never cancelled by one of
//! these (each search has a cancel flag of its own, which the server raises when it stops
//! waiting), and a folder outside the index is listed from disk without being added to it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_index::{DirItem, EntryId, Index, Meta, attr, walk};
use kestrel_ipc::protocol::{
    self as wire, DiskArgs, Entry, Failure, FailureKind, FolderListing, FolderSize, GitSummary, Hello, LimitArgs, LineMatch, ListArgs,
    PathArgs, ProjectArgs, ProjectDetails, ProjectList, ProjectSummary, ProjectsArgs, RecentFile, RecentFiles, SearchArgs,
    SearchResult, SortBy, bounds, iso_utc,
};
use kestrel_ipc::{Context, Request};
use kestrel_query::natural;
use kestrel_query::{Clause, ClauseKind, Clock, Sort, SortKey, compile, parse, search};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::engine::{Engine, normalize};
use crate::search::resolve::Resolver;
use crate::search::{Ask, Decided};
use crate::visibility::Visibility;

/// Entries walked between looks at the clock while adding a folder up.
const COUNT_STEP: usize = 200_000;
/// How long a project's details wait for git's first answer about it.
const GIT_WAIT: Duration = Duration::from_secs(3);

fn unavailable(message: impl Into<String>) -> Failure {
    Failure::new(FailureKind::Unavailable, message)
}

fn timed_out() -> Failure {
    Failure::new(FailureKind::Timeout, "Kestrel stopped before finishing: the question took too long")
}

fn internal(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureKind::Internal, error.to_string())
}

/// The index can be asked: not while it is still loading.
fn ready(engine: &Engine) -> Result<(), Failure> {
    match engine.status().state {
        "loading" => Err(unavailable("Kestrel is still loading its index; ask again in a moment")),
        _ => Ok(()),
    }
}

fn kind_of(dir: bool) -> String {
    if dir { "folder" } else { "file" }.to_owned()
}

/// An entry of the index as the protocol has it.
fn entry_of(index: &Index, id: EntryId) -> Entry {
    let attrs = index.attrs(id);
    Entry {
        path: index.path(id),
        name: index.name(id).to_owned(),
        kind: kind_of(index.is_dir(id)),
        size: index.size(id),
        size_on_disk: index.alloc(id),
        modified: iso_utc(kestrel_analyze::unix_ms(index.mtime(id))),
        hidden: attrs & attr::HIDDEN != 0,
        cloud_only: kestrel_analyze::is_cloud_only(attrs),
        matched: None,
    }
}

/// Hidden items when asked for; protected operating system files never.
fn visibility(hidden: bool) -> Visibility {
    Visibility { hidden, protected: false }
}

fn sort_of(by: Option<SortBy>, descending: Option<bool>, folders_first: bool) -> Sort {
    let by = by.unwrap_or_default();
    let key = match by {
        SortBy::Name => SortKey::Name,
        SortBy::Size => SortKey::Size,
        SortBy::Modified => SortKey::Modified,
        SortBy::Type => SortKey::Type,
        SortBy::Path => SortKey::Path,
    };
    // Sizes and dates read best largest and newest first, as the window starts them.
    let descending = descending.unwrap_or(matches!(by, SortBy::Size | SortBy::Modified));
    Sort { key, descending, folders_first }
}

/// The first `limit` of `ids` in `order`, without sorting all of them when there are many: the
/// first `limit` are picked out (by a cheap key where there is one), then sorted as the window
/// sorts.
fn first_sorted(index: &Index, ids: &mut Vec<EntryId>, order: Sort, limit: usize) {
    use std::cmp::Ordering;
    if ids.len() > limit * 4 && order.key != SortKey::Path {
        let ext = |id: EntryId| index.ext_name(index.ext(id));
        let compare = |a: &EntryId, b: &EntryId| -> Ordering {
            let by_key = match order.key {
                SortKey::Size => index.size(*a).cmp(&index.size(*b)),
                SortKey::Modified => index.mtime(*a).cmp(&index.mtime(*b)),
                SortKey::Type => ext(*a).cmp(ext(*b)),
                SortKey::Alloc => index.alloc(*a).cmp(&index.alloc(*b)),
                _ => Ordering::Equal,
            };
            let ordering = by_key.then_with(|| index.cmp_name(*a, *b));
            if order.descending { ordering.reverse() } else { ordering }
        };
        ids.select_nth_unstable_by(limit, compare);
        ids.truncate(limit);
    }
    kestrel_query::sort(index, ids, order);
    ids.truncate(limit);
}

/// What the window's search understands beyond the index, which the query service's search
/// understands too: tags, the media library's clauses, descriptions for AI search, phrases, and
/// searches inside files (`search/`). One query finds the same things wherever it is asked.
pub struct Understanding<'a> {
    pub db: &'a Db,
    /// Runs the model for descriptions (`like:`, `~`); `None` where there is none (tests).
    pub semantic: Option<&'a crate::semantic::Semantic>,
    /// Whether the media library reads a folder: `taken:` in a search limited to one it
    /// doesn't is read from the photos, as in the window.
    pub library: &'a dyn Fn(&str) -> bool,
    /// Known folders and projects, for phrases ("pdfs in downloads", "code in kestrel").
    pub places: &'a dyn natural::Places,
}

fn resolver<'a>(understanding: &Understanding<'a>, text: &str) -> Result<Resolver<'a>, Failure> {
    match understanding.semantic {
        Some(semantic) => Resolver::prepare(understanding.db, semantic, text).map_err(Failure::bad_request),
        None => Ok(Resolver::index_only(understanding.db)),
    }
}

/// The index's entries for `text` beneath `folder`, every clause resolved, with descriptions'
/// scores when the query has one. The index is read in place, with the asker's cancel flag.
fn index_ids(
    index: &Index,
    text: &str,
    folder: Option<&str>,
    shown: Visibility,
    resolver: &mut Resolver<'_>,
    context: &Context,
) -> Result<(Vec<EntryId>, Option<std::collections::HashMap<EntryId, f32>>), Failure> {
    let mut query = parse(text, Clock::local()).map_err(|error| Failure::bad_request(error.to_string()))?;
    if let Some(folder) = folder {
        query.clauses.push(Clause { negate: false, kind: ClauseKind::Within(folder.to_owned()) });
    }
    if query.clauses.is_empty() {
        return Err(Failure::bad_request("Give something to search for"));
    }
    let scores = resolver.resolve(&mut query, index).map_err(Failure::bad_request)?;
    let compiled = compile(&query, index).map_err(|error| Failure::bad_request(error.to_string()))?;
    // Its own cancel flag: the server raises it when it stops waiting.
    let mut ids = search(index, &compiled, &context.cancelled).ok_or_else(timed_out)?;
    ids.retain(|&id| shown.shows(index.attrs(id)));
    Ok((ids, scores))
}

pub fn search_index(engine: &Engine, args: &SearchArgs, context: &Context, understanding: &Understanding) -> Result<SearchResult, Failure> {
    ready(engine)?;
    let started = Instant::now();
    let limit = bounds::clamp(args.limit, bounds::SEARCH);
    let folder = args.folder.as_deref().map(normalize).filter(|folder| !folder.is_empty());
    let shown = visibility(args.hidden);
    if args.query.trim().is_empty() && folder.is_none() {
        return Err(Failure::bad_request("Give something to search for"));
    }
    // As the search box reads it: a phrase into clauses, contents and dates taken to read.
    let options = crate::search::Options::default();
    let ask = Ask { text: &args.query, scope: folder.as_deref(), folder: folder.as_deref(), visibility: shown, options: &options, library: understanding.library };
    let (understood, decided) = crate::search::decide(&ask, Clock::local(), understanding.places).map_err(Failure::bad_request)?;
    let ran_as = (understood.natural && understood.query != args.query.trim()).then(|| understood.query.clone());
    let (total, results) = match decided {
        Decided::Index { query } => {
            let mut resolver = resolver(understanding, &query)?;
            let index = engine.index().read();
            let (mut ids, scores) = index_ids(&index, &query, folder.as_deref(), shown, &mut resolver, context)?;
            let total = ids.len() as u64;
            if context.expired() {
                return Err(timed_out());
            }
            match (&scores, args.sort) {
                // A description's results come best first, as in the window, unless a sort is asked for.
                (Some(scores), None) => {
                    let score = |id: &EntryId| scores.get(id).copied().unwrap_or(f32::NEG_INFINITY);
                    ids.sort_by(|a, b| score(b).total_cmp(&score(a)).then_with(|| index.cmp_name(*a, *b)));
                    ids.truncate(limit);
                }
                _ => first_sorted(&index, &mut ids, sort_of(args.sort, args.descending, false), limit),
            }
            (total, ids.iter().map(|&id| entry_of(&index, id)).collect::<Vec<Entry>>())
        }
        Decided::Content { plan_query, patterns, taken } => {
            let choose = |plan_query: &str| -> Result<Vec<(String, DirItem)>, String> {
                let mut resolver = resolver(understanding, plan_query).map_err(|failure| failure.message)?;
                let index = engine.index().read();
                let (ids, _) = index_ids(&index, plan_query, folder.as_deref(), shown, &mut resolver, context).map_err(|failure| failure.message)?;
                Ok(ids
                    .into_iter()
                    .filter(|&id| !index.is_dir(id))
                    .map(|id| {
                        let meta = Meta { size: index.size(id), alloc: index.alloc(id), mtime: index.mtime(id), attrs: index.attrs(id), file_id: 0 };
                        (index.path(index.parent(id)), DirItem { name: index.name(id).to_owned(), meta, alias: false })
                    })
                    .collect())
            };
            let plan = crate::search::plan(engine, &ask, &plan_query, patterns, taken, choose).map_err(Failure::bad_request)?;
            let (found, status) = crate::search::run_to_end(plan, context.cancelled.clone());
            if context.expired() && !status.done {
                return Err(timed_out());
            }
            let mut entries: Vec<Entry> = found.iter().map(found_entry).collect();
            sort_entries(&mut entries, args.sort, args.descending);
            let total = entries.len() as u64;
            entries.truncate(limit);
            (total, entries)
        }
    };
    Ok(SearchResult {
        query: args.query.clone(),
        ran_as,
        folder,
        total,
        returned: results.len(),
        truncated: total > results.len() as u64,
        took_ms: started.elapsed().as_secs_f64() * 1000.0,
        results,
    })
}

/// A file a search inside files found, with its first matching line.
fn found_entry(found: &kestrel_content::Found) -> Entry {
    let meta = &found.item.meta;
    Entry {
        path: found.path(),
        name: found.item.name.clone(),
        kind: kind_of(false),
        size: meta.size,
        size_on_disk: meta.alloc,
        modified: iso_utc(kestrel_analyze::unix_ms(meta.mtime)),
        hidden: meta.attrs & attr::HIDDEN != 0,
        cloud_only: kestrel_analyze::is_cloud_only(meta.attrs),
        matched: found.hit.as_ref().map(|hit| LineMatch { line: hit.line, text: hit.text.clone(), more_lines: hit.lines.saturating_sub(1) }),
    }
}

/// Files found by reading them, sorted as asked (by name when not).
fn sort_entries(entries: &mut [Entry], by: Option<SortBy>, descending: Option<bool>) {
    let order = sort_of(by, descending, false);
    entries.sort_by(|a, b| {
        let ordering = match order.key {
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Path => a.path.to_lowercase().cmp(&b.path.to_lowercase()),
            SortKey::Type => a.name.rsplit('.').next().cmp(&b.name.rsplit('.').next()),
            _ => std::cmp::Ordering::Equal,
        }
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        if order.descending { ordering.reverse() } else { ordering }
    });
}

/// A share (`\\server\share`, `\\?\UNC\...`) or a device path, rather than a drive's folder.
pub fn is_share(path: &str) -> bool {
    path.starts_with(r"\\") || path.starts_with("//")
}

/// The live, indexed folder at `path` (not a link to one, whose contents the index never holds).
fn indexed_folder(index: &Index, path: &str) -> Option<EntryId> {
    index
        .lookup(path)
        .filter(|&id| index.is_dir(id) && !index.is_deleted(id))
        .filter(|&id| index.attrs(id) & attr::REPARSE_POINT == 0 || !walk::is_alias(path))
}

pub fn list_folder(engine: &Engine, args: &ListArgs, _context: &Context) -> Result<FolderListing, Failure> {
    ready(engine)?;
    let path = normalize(&args.path);
    if path.is_empty() {
        return Err(Failure::bad_request("Give a folder to list"));
    }
    let limit = bounds::clamp(args.limit, bounds::LIST_FOLDER);
    let order = sort_of(args.sort, args.descending, true);
    let shown = visibility(args.hidden);
    {
        let index = engine.index().read();
        if let Some(dir) = indexed_folder(&index, &path) {
            let mut ids: Vec<EntryId> = index.children(dir).filter(|&id| shown.shows(index.attrs(id))).collect();
            let total = ids.len() as u64;
            first_sorted(&index, &mut ids, order, limit);
            let items: Vec<Entry> = ids.iter().map(|&id| entry_of(&index, id)).collect();
            return Ok(FolderListing {
                path: index.path(dir),
                size: index.size(dir),
                size_on_disk: index.alloc(dir),
                indexed: true,
                total,
                returned: items.len(),
                truncated: total > items.len() as u64,
                items,
            });
        }
    }
    // Not in the index (a removable drive, a link's target): read from disk, without adding it
    // to the index. Subfolders' sizes are not known this way. Shares are not read for other
    // programs: a path a client names must never make Kestrel connect to some server (and offer
    // it this account's credentials).
    if is_share(&path) {
        return Err(Failure::not_found(format!("{path} is on a network share; other programs can ask Kestrel about this PC's own drives only")));
    }
    let listed = walk::list(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Failure::not_found(format!("{path} is not a folder")),
        std::io::ErrorKind::PermissionDenied => Failure::new(FailureKind::NotFound, crate::access::LOCKED),
        _ => Failure::not_found(format!("Can't open {path}: {error}")),
    })?;
    let mut items: Vec<_> = listed.into_iter().filter(|item| shown.shows(item.meta.attrs)).collect();
    let total = items.len() as u64;
    let dir_of = |item: &kestrel_index::DirItem| item.meta.attrs & attr::DIRECTORY != 0;
    items.sort_by(|a, b| {
        let by_key = match order.key {
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            _ => std::cmp::Ordering::Equal,
        };
        let by_key = by_key.then_with(|| kestrel_index::natural_cmp(a.name.to_lowercase().as_bytes(), b.name.to_lowercase().as_bytes()));
        let by_key = if order.descending { by_key.reverse() } else { by_key };
        dir_of(b).cmp(&dir_of(a)).then(by_key)
    });
    items.truncate(limit);
    let join = |name: &str| if path.ends_with('\\') { format!("{path}{name}") } else { format!("{path}\\{name}") };
    let entries: Vec<Entry> = items
        .iter()
        .map(|item| Entry {
            path: join(&item.name),
            name: item.name.clone(),
            kind: kind_of(dir_of(item)),
            size: if dir_of(item) { 0 } else { item.meta.size },
            size_on_disk: if dir_of(item) { 0 } else { item.meta.alloc },
            modified: iso_utc(kestrel_analyze::unix_ms(item.meta.mtime)),
            hidden: item.meta.attrs & attr::HIDDEN != 0,
            cloud_only: kestrel_analyze::is_cloud_only(item.meta.attrs),
            matched: None,
        })
        .collect();
    let size = entries.iter().map(|entry| entry.size).sum();
    let size_on_disk = entries.iter().map(|entry| entry.size_on_disk).sum();
    Ok(FolderListing { path, size, size_on_disk, indexed: false, total, returned: entries.len(), truncated: total > entries.len() as u64, items: entries })
}

/// Counts what lies beneath a folder, a step at a time, letting go of the index between steps.
struct Count {
    stack: Vec<EntryId>,
    files: u64,
    folders: u64,
    cloud: u64,
    complete: bool,
}

pub fn folder_size(engine: &Engine, args: &PathArgs, context: &Context) -> Result<FolderSize, Failure> {
    ready(engine)?;
    let path = normalize(&args.path);
    let found = {
        let index = engine.index().read();
        index.lookup(&path).filter(|&id| !index.is_deleted(id)).map(|id| (index.path(id), index.is_dir(id), index.size(id), index.alloc(id), index.attrs(id)))
    };
    let Some((shown, is_dir, size, size_on_disk, attrs)) = found else {
        return Err(Failure::not_found(format!("{path} is not in Kestrel's index (folders on shares and removable drives are not indexed)")));
    };
    if !is_dir {
        let cloud = u64::from(kestrel_analyze::is_cloud_only(attrs));
        return Ok(FolderSize { path: shown, size, size_on_disk, files: 1, folders: 0, cloud_only_files: cloud, complete: true });
    }
    let count = engine.in_steps(
        |index| Count { stack: index.lookup(&path).into_iter().collect(), files: 0, folders: 0, cloud: 0, complete: true },
        |count, index| {
            if context.expired() {
                count.complete = false;
                return true;
            }
            let mut walked = 0;
            while let Some(dir) = count.stack.pop() {
                for child in index.children(dir) {
                    walked += 1;
                    if index.is_dir(child) {
                        count.folders += 1;
                        // Links are counted, never followed: their folders are counted where they are.
                        if index.attrs(child) & attr::REPARSE_POINT == 0 {
                            count.stack.push(child);
                        }
                    } else {
                        count.files += 1;
                        count.cloud += u64::from(kestrel_analyze::is_cloud_only(index.attrs(child)));
                    }
                }
                if walked >= COUNT_STEP {
                    return false;
                }
            }
            true
        },
        |count, _| count,
    );
    Ok(FolderSize { path: shown, size, size_on_disk, files: count.files, folders: count.folders, cloud_only_files: count.cloud, complete: count.complete })
}

pub fn hello(engine: &Engine) -> Hello {
    let status = engine.status();
    Hello {
        app: "Kestrel".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: wire::PROTOCOL_VERSION,
        pid: std::process::id(),
        index: status.state.into(),
        entries: u64::from(engine.live_count()),
    }
}

// ---------- what needs the app: projects, recent files, disk usage ----------

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value.get(key).and_then(Value::as_array).map(|list| list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// A project of the hub's list (`projects::ProjectView`, as JSON) in short.
fn project_summary(view: &Value) -> ProjectSummary {
    let git = view.get("git").filter(|git| !git.is_null()).map(|git| {
        let branch = git.get("branch").cloned().unwrap_or_default();
        let counts = git.get("counts").cloned().unwrap_or_default();
        let last = git.get("last_commit").filter(|c| !c.is_null());
        GitSummary {
            branch: text(&branch, "name").or_else(|| text(&branch, "head").map(|head| format!("(detached at {head})"))),
            upstream: text(&branch, "upstream"),
            ahead: number(&branch, "ahead"),
            behind: number(&branch, "behind"),
            staged: number(&counts, "staged"),
            modified: number(&counts, "modified"),
            untracked: number(&counts, "untracked"),
            conflicted: number(&counts, "conflicted"),
            last_commit: last.and_then(|c| text(c, "subject")),
            last_commit_time: last.and_then(|c| c.get("time").and_then(Value::as_i64)).and_then(iso_utc),
            remote: text(git, "remote"),
        }
    });
    ProjectSummary {
        name: text(view, "name").unwrap_or_default(),
        root: text(view, "root").unwrap_or_default(),
        stack: strings(view, "stack"),
        status: text(view, "status").unwrap_or_else(|| "active".into()),
        tags: strings(view, "tags"),
        pinned: view.get("pinned").and_then(Value::as_bool).unwrap_or(false),
        is_repo: view.get("is_repo").and_then(Value::as_bool).unwrap_or(false),
        git,
        last_activity: view.get("last_activity").and_then(Value::as_i64).and_then(iso_utc),
        size_on_disk: number(view, "size_on_disk"),
        build_output: number(view, "reclaimable"),
    }
}

/// The hub's list of projects, as JSON, looking for them first if nothing has yet.
fn project_views(app: &AppHandle) -> Result<(Vec<Value>, bool), Failure> {
    let projects = app.state::<Arc<crate::projects::Projects>>();
    projects.ensure_found();
    let snapshot = serde_json::to_value(crate::projects::projects_list(projects, false)).map_err(internal)?;
    let indexing = snapshot.get("indexing").and_then(Value::as_bool).unwrap_or(false);
    let views = snapshot.get("projects").and_then(Value::as_array).cloned().unwrap_or_default();
    // Folders the owner said are not projects stay out.
    let views = views.into_iter().filter(|view| !view.get("dismissed").and_then(Value::as_bool).unwrap_or(false)).collect();
    Ok((views, indexing))
}

fn matches_filter(summary: &ProjectSummary, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let branch = summary.git.as_ref().and_then(|git| git.branch.clone()).unwrap_or_default();
    [summary.name.as_str(), summary.root.as_str(), summary.status.as_str(), branch.as_str()]
        .into_iter()
        .chain(summary.stack.iter().map(String::as_str))
        .chain(summary.tags.iter().map(String::as_str))
        .any(|field| field.to_lowercase().contains(&filter))
}

pub fn projects(app: &AppHandle, args: &ProjectsArgs) -> Result<ProjectList, Failure> {
    let (views, indexing) = project_views(app)?;
    let mut all: Vec<ProjectSummary> = views.iter().map(project_summary).collect();
    all.retain(|summary| matches_filter(summary, args.filter.as_deref().unwrap_or("")));
    // Pinned first, then the most recently active.
    all.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.last_activity.cmp(&a.last_activity)).then_with(|| a.name.cmp(&b.name)));
    let total = all.len();
    all.truncate(bounds::clamp(args.limit, bounds::PROJECTS));
    Ok(ProjectList { total, returned: all.len(), truncated: total > all.len(), indexing, projects: all })
}

/// The project a folder or a name means: the folder's own project, else the one so named.
fn find_project<'a>(views: &'a [Value], asked: &str) -> Result<&'a Value, Failure> {
    let path = normalize(asked).to_lowercase();
    let root_of = |view: &Value| text(view, "root").unwrap_or_default().to_lowercase();
    if let Some(view) = views.iter().find(|view| root_of(view) == path) {
        return Ok(view);
    }
    // A folder inside a project means that project.
    if let Some(view) = views.iter().filter(|view| path.starts_with(&format!("{}\\", root_of(view)))).max_by_key(|view| root_of(view).len()) {
        return Ok(view);
    }
    let named: Vec<&Value> = views.iter().filter(|view| text(view, "name").is_some_and(|name| name.eq_ignore_ascii_case(asked.trim()))).collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(Failure::not_found(format!("No project is named or found at \"{asked}\"; list_projects shows them all"))),
        several => Err(Failure::bad_request(format!(
            "Several projects are named \"{asked}\": {}. Ask by folder instead",
            several.iter().filter_map(|view| text(view, "root")).collect::<Vec<_>>().join(", ")
        ))),
    }
}

pub fn project_details(app: &AppHandle, args: &ProjectArgs, context: &Context) -> Result<ProjectDetails, Failure> {
    let (views, _) = project_views(app)?;
    let mut view = find_project(&views, &args.project)?.clone();
    let root = text(&view, "root").unwrap_or_default();
    // Git is read in the background: wait a moment for its first answer about this project.
    let started = Instant::now();
    while view.get("git_pending").and_then(Value::as_bool).unwrap_or(false) && started.elapsed() < GIT_WAIT && !context.expired() {
        std::thread::sleep(Duration::from_millis(100));
        let (views, _) = project_views(app)?;
        if let Ok(fresh) = find_project(&views, &root) {
            view = fresh.clone();
        }
    }
    let projects = app.state::<Arc<crate::projects::Projects>>();
    let detail = crate::projects::project_detail(projects, root.clone(), 0).map_err(Failure::not_found)?;
    let detail = serde_json::to_value(detail).map_err(internal)?;
    let readme = detail.get("readme").filter(|readme| !readme.is_null());
    let readme_text = readme.and_then(|readme| text(readme, "text")).map(|text| {
        if text.chars().count() > bounds::README_CHARS {
            text.chars().take(bounds::README_CHARS).chain("…".chars()).collect()
        } else {
            text
        }
    });
    let list = |key: &str| detail.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(ProjectDetails {
        summary: project_summary(&view),
        languages: list("languages").iter().map(|l| (text(l, "name").unwrap_or_default(), l.get("share").and_then(Value::as_f64).unwrap_or(0.0))).collect(),
        scripts: list("scripts")
            .iter()
            .map(|s| wire::Script { source: text(s, "source").unwrap_or_default(), name: text(s, "name").unwrap_or_default(), command: text(s, "command").unwrap_or_default() })
            .collect(),
        readme_path: readme.and_then(|readme| text(readme, "path")),
        readme: readme_text,
        notes: text(&detail, "notes").unwrap_or_default(),
        recent_files: list("recent")
            .iter()
            .take(bounds::PROJECT_RECENT)
            .map(|file| Entry {
                path: text(file, "path").unwrap_or_default(),
                name: text(file, "name").unwrap_or_default(),
                kind: "file".into(),
                size: number(file, "size"),
                size_on_disk: number(file, "size"),
                modified: file.get("mtime").and_then(Value::as_i64).and_then(iso_utc),
                hidden: false,
                cloud_only: false,
                matched: None,
            })
            .collect(),
        build_output_folders: list("reclaimable_dirs").iter().map(|d| (text(d, "path").unwrap_or_default(), number(d, "bytes"))).collect(),
    })
}

pub fn recent_files(db: &Db, args: &LimitArgs) -> RecentFiles {
    let limit = bounds::clamp(args.limit, bounds::RECENT_FILES);
    let files: Vec<RecentFile> = crate::parity::recent::recent_files(db, limit, true)
        .into_iter()
        .map(|file| RecentFile {
            opened: iso_utc(file.used).unwrap_or_default(),
            path: file.path,
            name: file.name,
            folder: file.location,
            source: file.source.into(),
            cloud_only: file.cloud,
        })
        .collect();
    RecentFiles { returned: files.len(), files }
}

pub fn disk_usage(app: &AppHandle, args: &DiskArgs) -> Result<wire::DiskUsage, Failure> {
    ready(&app.state::<Arc<Engine>>())?;
    let top = bounds::clamp(args.top, bounds::DISK_TOP);
    let disk = app.state::<Arc<crate::disk::Disk>>();
    let overview = crate::disk::disk_overview(disk, args.path.clone()).map_err(Failure::not_found)?;
    let overview = serde_json::to_value(overview).map_err(internal)?;
    let totals = overview.get("totals").cloned().unwrap_or_default();
    let amounts = |key: &str| -> Vec<wire::Amount> {
        overview
            .get(key)
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter(|row| number(row, "files") > 0 || number(row, "bytes") > 0)
                    .map(|row| wire::Amount { label: text(row, "label").unwrap_or_default(), size_on_disk: number(row, "bytes"), size: number(row, "logical"), files: number(row, "files") })
                    .collect()
            })
            .unwrap_or_default()
    };
    let items = |key: &str, folder: bool| -> Vec<Entry> {
        overview
            .get(key)
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .take(top)
                    .map(|row| Entry {
                        path: text(row, "path").unwrap_or_default(),
                        name: text(row, "name").unwrap_or_default(),
                        kind: kind_of(folder),
                        size: number(row, "logical"),
                        size_on_disk: number(row, "bytes"),
                        modified: row.get("mtime").and_then(Value::as_i64).and_then(iso_utc),
                        hidden: false,
                        cloud_only: false,
                        matched: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let drive = overview.get("volume").filter(|volume| !volume.is_null()).filter(|_| overview.get("is_drive").and_then(Value::as_bool).unwrap_or(false)).map(|volume| wire::Drive {
        root: text(volume, "root").unwrap_or_default(),
        total: number(volume, "total"),
        free: number(volume, "free"),
        file_system: text(volume, "file_system").unwrap_or_default(),
    });
    Ok(wire::DiskUsage {
        path: text(&overview, "path").unwrap_or_else(|| args.path.clone()),
        size_on_disk: number(&totals, "bytes"),
        size: number(&totals, "logical"),
        files: number(&totals, "files"),
        folders: number(&totals, "folders"),
        cloud_only_files: number(&totals, "cloud_files"),
        indexing: overview.get("indexing").and_then(Value::as_bool).unwrap_or(false),
        types: amounts("categories"),
        ages: amounts("ages"),
        largest_files: items("largest_files", false),
        largest_folders: items("largest_folders", true),
        drive,
    })
}

/// Answers one request. Results are typed by the protocol and sent as JSON.
pub fn answer(app: &AppHandle, request: Request, context: &Context) -> Result<Value, Failure> {
    let engine = app.state::<Arc<Engine>>().inner().clone();
    let db = app.state::<Arc<Db>>().inner().clone();
    let value = match request {
        Request::Hello => serde_json::to_value(hello(&engine)),
        Request::Search(args) => {
            let semantic = app.state::<Arc<crate::semantic::Semantic>>().inner().clone();
            let media = app.state::<Arc<crate::media::Media>>().inner().clone();
            let projects = app.state::<Arc<crate::projects::Projects>>().inner().clone();
            let library = |path: &str| media.covers(path);
            let places = crate::search::places::Resolver::new(projects);
            let understanding = Understanding { db: &db, semantic: Some(&semantic), library: &library, places: &places };
            serde_json::to_value(search_index(&engine, &args, context, &understanding)?)
        }
        Request::ListFolder(args) => serde_json::to_value(list_folder(&engine, &args, context)?),
        Request::FolderSize(args) => serde_json::to_value(folder_size(&engine, &args, context)?),
        Request::Projects(args) => serde_json::to_value(projects(app, &args)?),
        Request::ProjectDetails(args) => serde_json::to_value(project_details(app, &args, context)?),
        Request::RecentFiles(args) => serde_json::to_value(recent_files(&db, &args)),
        Request::DiskUsage(args) => serde_json::to_value(disk_usage(app, &args)?),
    };
    value.map_err(internal)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use kestrel_index::IndexBuilder;

    use super::*;

    fn context(seconds: u64) -> Context {
        Context {
            client_pid: None,
            client_name: "test".into(),
            deadline: Instant::now() + Duration::from_secs(seconds),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// An engine whose index holds a scratch folder: `docs` (two files), `src` (three `.rs`
    /// files), `.hidden.txt` (hidden) and `big.bin`.
    fn scratch(label: &str) -> (Engine, std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("kestrel-devtools-query-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("docs/a.md"), b"aaaa").unwrap();
        fs::write(root.join("docs/b.md"), b"bb").unwrap();
        for (name, size) in [("main.rs", 10), ("lib.rs", 30), ("util.rs", 20)] {
            fs::write(root.join("src").join(name), vec![b'x'; size]).unwrap();
        }
        fs::write(root.join(".hidden.txt"), b"h").unwrap();
        hide(&root.join(".hidden.txt"));
        fs::write(root.join("big.bin"), vec![0u8; 5000]).unwrap();
        let root_text = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&root_text, &mut builder).unwrap();
        engine.install(builder.finish());
        engine.set_status("ready", false);
        (engine, root, root_text)
    }

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    /// The media library reads nothing in these tests.
    fn nowhere(_: &str) -> bool {
        false
    }

    /// What the search understands without a model or places.
    fn plain(db: &Db) -> Understanding<'_> {
        Understanding { db, semantic: None, library: &nowhere, places: &natural::NoPlaces }
    }

    fn hide(path: &std::path::Path) {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
        let wide: Vec<u16> = path.to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated.
        assert!(unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) } != 0);
    }

    #[test]
    fn searches_are_bounded_sorted_and_scoped() {
        let database = db();
        let (engine, root, root_text) = scratch("search");
        let args = SearchArgs { query: "ext:rs".into(), folder: Some(root_text.clone()), sort: Some(SortBy::Size), limit: Some(2), ..SearchArgs::default() };
        let found = search_index(&engine, &args, &context(5), &plain(&database)).unwrap();
        assert_eq!((found.total, found.returned, found.truncated), (3, 2, true));
        // Largest first, as sizes start.
        assert_eq!(found.results.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["lib.rs", "util.rs"]);
        assert_eq!(found.results[0].kind, "file");
        assert_eq!(found.results[0].size, 30);
        assert!(found.results[0].modified.as_deref().is_some_and(|m| m.ends_with('Z')));
        let by_name = SearchArgs { sort: Some(SortBy::Name), limit: None, ..args.clone() };
        let names: Vec<String> = search_index(&engine, &by_name, &context(5), &plain(&database)).unwrap().results.into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["lib.rs", "main.rs", "util.rs"]);
        // Hidden items only when asked for.
        let hidden = SearchArgs { query: "hidden".into(), folder: Some(root_text.clone()), ..SearchArgs::default() };
        assert_eq!(search_index(&engine, &hidden, &context(5), &plain(&database)).unwrap().total, 0);
        assert_eq!(search_index(&engine, &SearchArgs { hidden: true, ..hidden }, &context(5), &plain(&database)).unwrap().total, 1);
        // A bad query is the asker's mistake.
        let bad = SearchArgs { query: "size:>>".into(), ..SearchArgs::default() };
        assert_eq!(search_index(&engine, &bad, &context(5), &plain(&database)).unwrap_err().kind, FailureKind::BadRequest);
        assert_eq!(search_index(&engine, &SearchArgs::default(), &context(5), &plain(&database)).unwrap_err().kind, FailureKind::BadRequest);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The query service's search understands what the window's does: phrases, searches inside
    /// files, the media library's clauses and tags (`Understanding`).
    #[test]
    fn searches_read_phrases_and_inside_files_as_the_window_does() {
        let database = db();
        let (engine, root, root_text) = scratch("understand");
        fs::write(root.join("docs").join("notes.md"), "first line\nthe needle is here\n").unwrap();
        let inside = |query: &str| SearchArgs { query: query.into(), folder: Some(root_text.clone()), ..SearchArgs::default() };
        // A search inside files: the files, each with its first matching line.
        let found = search_index(&engine, &inside(r#"content:needle"#), &context(5), &plain(&database)).unwrap();
        assert_eq!(found.results.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["notes.md"]);
        let matched = found.results[0].matched.clone().unwrap();
        assert_eq!((matched.line, matched.text.as_str(), matched.more_lines), (2, "the needle is here", 0));
        assert_eq!(found.ran_as, None, "typed clauses are run as they are");
        // Narrowed by the other clauses.
        assert_eq!(search_index(&engine, &inside("content:needle ext:rs"), &context(5), &plain(&database)).unwrap().total, 0);
        // A phrase is read into clauses, and says what it ran as.
        let phrase = search_index(&engine, &inside("rust files"), &context(5), &plain(&database)).unwrap();
        assert_eq!(phrase.ran_as.as_deref(), Some("ext:rs"));
        assert_eq!(phrase.total, 3);
        // The media library's clauses are answered (by its tables: nothing here is in it).
        let media = search_index(&engine, &inside("width:>100 kind:photo"), &context(5), &plain(&database)).unwrap();
        assert_eq!(media.total, 0);
        // A description needs AI search's model: without one it is the asker's mistake to fix.
        assert_eq!(search_index(&engine, &inside("like:dog"), &context(5), &plain(&database)).unwrap_err().kind, FailureKind::BadRequest);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_search_given_up_on_stops() {
        let database = db();
        let (engine, root, _) = scratch("cancel");
        let given_up = context(5);
        given_up.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        let args = SearchArgs { query: "*".into(), ..SearchArgs::default() };
        assert_eq!(search_index(&engine, &args, &given_up, &plain(&database)).unwrap_err().kind, FailureKind::Timeout);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn many_results_keep_the_first_ones_in_order() {
        let (engine, root, _) = scratch("partial");
        let index = engine.index().read();
        let mut all: Vec<EntryId> = (0..index.len()).filter(|&id| !index.is_deleted(id)).collect();
        let order = sort_of(Some(SortBy::Name), None, false);
        let mut whole = all.clone();
        kestrel_query::sort(&index, &mut whole, order);
        // Fewer wanted than a quarter of the results: picked out, then sorted.
        first_sorted(&index, &mut all, order, 2);
        assert_eq!(all, whole[..2]);
        drop(index);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folders_list_with_sizes_from_the_index_or_from_disk() {
        let (engine, root, root_text) = scratch("list");
        let listed = list_folder(&engine, &ListArgs { path: root_text.clone(), ..ListArgs::default() }, &context(5)).unwrap();
        assert!(listed.indexed);
        // Folders first, then by name; the hidden file left out.
        assert_eq!(listed.items.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["docs", "src", "big.bin"]);
        assert_eq!(listed.items[0].size, 6, "a folder's size is everything in it");
        assert_eq!(listed.size, 5067, "the hidden file counts towards the folder");
        let by_size = ListArgs { path: root_text.clone(), sort: Some(SortBy::Size), limit: Some(1), ..ListArgs::default() };
        let largest = list_folder(&engine, &by_size, &context(5)).unwrap();
        assert_eq!((largest.items[0].name.as_str(), largest.total, largest.truncated), ("src", 3, true), "folders first, the largest of them");
        // A folder the index doesn't hold is read from disk.
        let outside = Engine::default();
        outside.set_status("ready", false);
        let live = list_folder(&outside, &ListArgs { path: format!(r"{root_text}\src"), ..ListArgs::default() }, &context(5)).unwrap();
        assert!(!live.indexed);
        assert_eq!(live.items.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["lib.rs", "main.rs", "util.rs"]);
        let missing = list_folder(&outside, &ListArgs { path: format!(r"{root_text}\nowhere"), ..ListArgs::default() }, &context(5));
        assert_eq!(missing.unwrap_err().kind, FailureKind::NotFound);
        // Shares are never read for another program, so no server is ever contacted for one.
        for share in [r"\\attacker.example\share", r"\\?\UNC\attacker.example\share", "//attacker.example/share", r"\\.\pipe\x"] {
            let refused = list_folder(&outside, &ListArgs { path: share.into(), ..ListArgs::default() }, &context(5)).unwrap_err();
            assert!(refused.message.contains("network share"), "{share}: {}", refused.message);
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folder_sizes_count_what_is_beneath() {
        let (engine, root, root_text) = scratch("size");
        let size = folder_size(&engine, &PathArgs { path: root_text.clone() }, &context(5)).unwrap();
        assert_eq!((size.size, size.files, size.folders, size.complete), (5067, 7, 2, true));
        let file = folder_size(&engine, &PathArgs { path: format!(r"{root_text}\big.bin") }, &context(5)).unwrap();
        assert_eq!((file.size, file.files), (5000, 1));
        let gone = folder_size(&engine, &PathArgs { path: format!(r"{root_text}\nowhere") }, &context(5));
        assert_eq!(gone.unwrap_err().kind, FailureKind::NotFound);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_is_answered_while_the_index_loads() {
        let database = db();
        let engine = Engine::default();
        let args = SearchArgs { query: "x".into(), ..SearchArgs::default() };
        assert_eq!(search_index(&engine, &args, &context(5), &plain(&database)).unwrap_err().kind, FailureKind::Unavailable);
        assert_eq!(hello(&engine).index, "loading");
    }

    #[test]
    fn projects_are_found_by_folder_or_by_name() {
        let views: Vec<Value> = vec![
            serde_json::json!({"name": "kestrel", "root": r"C:\Code\kestrel", "stack": ["Rust"], "status": "active", "git": {"branch": {"name": "main", "head": "abc", "upstream": "origin/main", "ahead": 1, "behind": 0}, "counts": {"staged": 0, "modified": 2, "untracked": 1, "conflicted": 0}, "last_commit": {"subject": "Fix", "author": "me", "time": 1_792_063_845_000_i64}, "remote": "github.com/o/r"}, "last_activity": 1_792_063_845_000_i64, "size_on_disk": 10, "reclaimable": 4}),
            serde_json::json!({"name": "site", "root": r"C:\Code\site", "stack": ["TypeScript"], "status": "paused", "tags": ["web"]}),
            serde_json::json!({"name": "site", "root": r"D:\old\site", "stack": [], "status": "archived"}),
        ];
        assert_eq!(text(find_project(&views, r"c:\code\KESTREL\").unwrap(), "name").unwrap(), "kestrel");
        assert_eq!(text(find_project(&views, r"C:\Code\kestrel\src\main.rs").unwrap(), "name").unwrap(), "kestrel", "a folder inside");
        assert_eq!(text(find_project(&views, "Kestrel").unwrap(), "root").unwrap(), r"C:\Code\kestrel");
        assert_eq!(find_project(&views, "site").unwrap_err().kind, FailureKind::BadRequest, "two are named so");
        assert_eq!(find_project(&views, "nothing").unwrap_err().kind, FailureKind::NotFound);

        let summary = project_summary(&views[0]);
        let git = summary.git.clone().unwrap();
        assert_eq!((git.branch.as_deref(), git.ahead, git.modified, git.untracked), (Some("main"), 1, 2, 1));
        assert_eq!(git.last_commit_time.as_deref(), Some("2026-10-15T11:30:45Z"));
        assert_eq!((summary.size_on_disk, summary.build_output), (10, 4));
        assert!(matches_filter(&summary, "rust") && matches_filter(&summary, "MAIN") && !matches_filter(&summary, "web"));
        assert!(matches_filter(&project_summary(&views[1]), "web"));
        assert!(project_summary(&views[1]).git.is_none());
    }
}
