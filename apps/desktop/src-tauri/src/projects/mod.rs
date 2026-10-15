//! Projects: the folders on the drive that hold code, found in the index, with what git and
//! the index say about each, and the user's own status, tags and notes for them.
//!
//! A background thread rediscovers projects whenever the index changes (debounced) once the
//! hub has been opened, and walks each project's part of the index to keep its facts current.
//! The walk yields a fingerprint of what git would see; when it changes, the project's git
//! facts are refreshed by a small pool of threads running the git command line, and the result
//! is sent to the window as a `project-git` event. The same cache answers the git badges of
//! folder views. Nothing here ever writes to a repository.

mod badges;
mod discover;
mod facts;
mod git;
mod launch;
mod provider;
mod scripts;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_db::projects::{self as meta, ProjectMeta};
use kestrel_index::EntryId;
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::engine::Engine;
use badges::{Badge, Item, RepoView};
use discover::{Marker, Markers};
use facts::{LanguageShare, Reclaimable, RecentFile, TreeFacts};
use git::{Branch, Commit, Counts};
use launch::{Available, Launch, Launcher};
use scripts::Script;

pub use provider::ProjectsProvider;

/// How long the index must stay quiet before projects are looked for again.
const QUIET: Duration = Duration::from_secs(1);
/// While it keeps changing (it rarely stops on a busy machine), how often they are looked for
/// anyway. A look over 3.6 million entries and a few dozen projects takes about 15 ms.
const MAX_WAIT: Duration = Duration::from_secs(3);
/// Threads running git at once.
const GIT_THREADS: usize = 3;
/// A repository's git facts are refreshed at least this often while something asks for them,
/// even if its fingerprint says nothing changed (it cannot see a commit made in a linked
/// worktree, whose git files live in the main repository). Opening the hub refreshes them all.
const GIT_MAX_AGE: Duration = Duration::from_secs(300);
/// Repositories outside the index (network shares, say) have no fingerprint, so their badges
/// trust a status for this long.
const UNINDEXED_MAX_AGE: Duration = Duration::from_secs(5);
/// Repositories shown in folder views are kept current for this long after the last look.
const BADGE_INTEREST: Duration = Duration::from_secs(600);
/// README files larger than this are shown cut short.
const MAX_README: u64 = 512 * 1024;
/// Languages sent with each project in the list.
const LIST_LANGUAGES: usize = 6;
/// Commits counted for the activity heatmap; a year of a very busy repository is plenty.
const MAX_HEATMAP_COMMITS: u32 = 20_000;
const LIST_RECLAIMABLE: usize = 8;

// ---------- git facts, cached per repository ----------

/// What git says about a repository, as the hub shows it.
#[derive(Clone, Debug, Serialize)]
pub struct GitFacts {
    pub branch: Branch,
    pub counts: Counts,
    pub last_commit: Option<Commit>,
    /// The `origin` remote, with any credentials removed.
    pub remote: Option<String>,
    /// The repository's page on GitHub, GitLab or Bitbucket.
    pub web_url: Option<String>,
}

#[derive(Default)]
struct RepoState {
    facts: Option<Arc<GitFacts>>,
    view: Option<Arc<RepoView>>,
    error: Option<String>,
    /// The index fingerprint the facts were computed for.
    fingerprint: Option<u64>,
    /// The fingerprint most recently seen; differs from `fingerprint` while a refresh is due.
    seen: Option<u64>,
    computed: Option<Instant>,
    queued: bool,
    /// `git check-ignore` answers by `/`-separated relative path, until the status changes.
    ignored: HashMap<String, bool>,
    /// Last time a folder view asked about this repository.
    badge_interest: Option<Instant>,
}

/// The payload of the `project-git` event.
#[derive(Clone, Serialize)]
struct GitEvent {
    root: String,
    git: Option<Arc<GitFacts>>,
    error: Option<String>,
}

// ---------- discovered projects ----------

#[derive(Clone)]
struct Project {
    root: String,
    entry: EntryId,
    markers: Markers,
    tree: TreeFacts,
}

#[derive(Default)]
struct Inner {
    /// The hub has been opened: keep projects current from now on.
    active: bool,
    /// Look again as soon as possible.
    kick: bool,
    /// Refresh every project's git facts on the next look (the hub was just opened).
    refresh_git: bool,
    /// The index revision last looked at, and when the index was last seen to change.
    looked_at: Option<u64>,
    observed: u64,
    changed_at: Option<Instant>,
    last_look: Option<Instant>,
    projects: Vec<Project>,
    /// Folders where test fixtures may be discovered despite the location rules; see
    /// `discover::Exclusions`. Development builds only.
    allowed: Vec<String>,
}

pub struct Projects {
    engine: Arc<Engine>,
    db: Arc<Db>,
    app: Mutex<Option<AppHandle>>,
    inner: Mutex<Inner>,
    wake: Condvar,
    /// Held for the whole of a look, so the worker and a command never look at once.
    looking: Mutex<()>,
    repos: Mutex<HashMap<String, RepoState>>,
    queue: Mutex<VecDeque<String>>,
    queued: Condvar,
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

fn name_of(root: &str) -> String {
    root.trim_end_matches('\\').rsplit('\\').next().unwrap_or(root).to_owned()
}

impl Projects {
    pub fn new(engine: Arc<Engine>, db: Arc<Db>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            db,
            app: Mutex::new(None),
            inner: Mutex::new(Inner::default()),
            wake: Condvar::new(),
            looking: Mutex::new(()),
            repos: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            queued: Condvar::new(),
        })
    }

    /// Starts the background work. Called once the app exists, to send it events.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        *self.app.lock() = Some(app);
        let me = self.clone();
        let _ = std::thread::Builder::new().name("kestrel-projects".into()).spawn(move || me.watch());
        for n in 0..GIT_THREADS {
            let me = self.clone();
            let _ = std::thread::Builder::new().name(format!("kestrel-git-{n}")).spawn(move || me.git_worker());
        }
    }

    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
        if let Some(app) = self.app.lock().as_ref() {
            let _ = app.emit(event, payload);
        }
    }

    // ---------- looking for projects ----------

    /// The background loop: looks again once the index has been quiet for a moment after a
    /// change, or at least every `MAX_WAIT` while it keeps changing.
    fn watch(self: Arc<Self>) {
        loop {
            {
                let mut inner = self.inner.lock();
                if !inner.kick {
                    self.wake.wait_for(&mut inner, Duration::from_millis(500));
                }
            }
            // Until the index is ready there is nothing to look in.
            if self.engine.status().state != "ready" {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            let revision = self.engine.revision();
            let badges_shown = self.has_badge_interest();
            let due = {
                let mut inner = self.inner.lock();
                if revision != inner.observed {
                    inner.observed = revision;
                    inner.changed_at = Some(Instant::now());
                }
                let changed = inner.looked_at != Some(revision);
                let quiet = inner.changed_at.is_none_or(|at| at.elapsed() >= QUIET);
                let overdue = inner.last_look.is_none_or(|at| at.elapsed() >= MAX_WAIT);
                let interested = inner.active || badges_shown;
                // A kick with nothing to look for is spent, or this loop would never wait.
                if !interested {
                    inner.kick = false;
                }
                interested && (inner.kick || (changed && (quiet || overdue)))
            };
            if due {
                self.look();
            }
        }
    }

    /// Keeps projects current from now on, as opening the hub does, and looks for them now if
    /// nothing has yet, so that whoever asks (the palette's `@`) gets them at once.
    pub fn ensure_found(&self) {
        let first = {
            let mut inner = self.inner.lock();
            !std::mem::replace(&mut inner.active, true)
        };
        if !first {
            return;
        }
        if self.engine.status().state == "ready" {
            self.look();
        } else {
            self.wake.notify_one();
        }
    }

    fn has_badge_interest(&self) -> bool {
        self.repos.lock().values().any(|repo| repo.badge_interest.is_some_and(|at| at.elapsed() < BADGE_INTEREST))
    }

    /// Rediscovers projects (when the hub has been opened), walks each one, and queues a git
    /// refresh for every repository whose fingerprint changed.
    fn look(&self) {
        let _looking = self.looking.lock();
        let revision = self.engine.revision();
        let (active, refresh_git, allowed) = {
            let mut inner = self.inner.lock();
            inner.kick = false;
            (inner.active, std::mem::take(&mut inner.refresh_git), inner.allowed.clone())
        };

        let mut changed_list = false;
        if active {
            let found = {
                let index = self.engine.index().read();
                let allowed: Vec<EntryId> = allowed.iter().filter_map(|path| index.lookup(path)).collect();
                discover::discover(&index, &allowed)
            };
            let mut projects = Vec::with_capacity(found.len());
            for found in found {
                // Each walk takes the lock afresh, so the journal is never held up for long.
                let index = self.engine.index().read();
                if found.root >= index.len() || index.is_deleted(found.root) {
                    continue;
                }
                let tree = facts::scan(&index, found.root);
                projects.push(Project { root: found.path, entry: found.root, markers: found.markers, tree });
            }
            let mut inner = self.inner.lock();
            changed_list = !same_projects(&inner.projects, &projects);
            inner.projects = projects;
        }

        // Repositories to keep current: the projects that are repositories (and not dismissed),
        // and those folder views are showing.
        let dismissed: HashSet<String> = meta::all(&self.db)
            .map(|all| all.into_iter().filter(|m| m.dismissed).map(|m| key(&m.root)).collect())
            .unwrap_or_default();
        let mut watched: Vec<(String, Option<u64>)> = {
            let inner = self.inner.lock();
            inner
                .projects
                .iter()
                .filter(|p| p.markers.has(Marker::Git) && !dismissed.contains(&key(&p.root)))
                .map(|p| (p.root.clone(), Some(p.tree.fingerprint)))
                .collect()
        };
        let known: HashSet<String> = watched.iter().map(|(root, _)| key(root)).collect();
        let badge_repos: Vec<String> = self
            .repos
            .lock()
            .iter()
            .filter(|(k, repo)| !known.contains(*k) && repo.badge_interest.is_some_and(|at| at.elapsed() < BADGE_INTEREST))
            .map(|(k, _)| k.clone())
            .collect();
        for root in badge_repos {
            let fingerprint = {
                let index = self.engine.index().read();
                index.lookup(&root).map(|entry| facts::scan(&index, entry).fingerprint)
            };
            watched.push((root, fingerprint));
        }
        for (root, fingerprint) in watched {
            self.consider(&root, fingerprint, refresh_git);
        }

        let mut inner = self.inner.lock();
        inner.looked_at = Some(revision);
        inner.last_look = Some(Instant::now());
        drop(inner);
        if changed_list {
            self.emit("projects-changed", ());
        }
    }

    /// Queues a git refresh of `root` if its fingerprint moved, it is too old, or `force`.
    fn consider(&self, root: &str, fingerprint: Option<u64>, force: bool) {
        let mut repos = self.repos.lock();
        let repo = repos.entry(key(root)).or_default();
        repo.seen = fingerprint;
        let stale = repo.computed.is_none_or(|at| at.elapsed() >= GIT_MAX_AGE);
        let moved = match fingerprint {
            Some(_) => repo.fingerprint != fingerprint,
            // Outside the index nothing says when it changed, so only age counts.
            None => repo.computed.is_none_or(|at| at.elapsed() >= UNINDEXED_MAX_AGE),
        };
        if (force || stale || moved) && !repo.queued {
            repo.queued = true;
            drop(repos);
            self.queue.lock().push_back(root.to_owned());
            self.queued.notify_one();
        }
    }

    // ---------- git ----------

    fn git_worker(self: Arc<Self>) {
        loop {
            let root = {
                let mut queue = self.queue.lock();
                loop {
                    if let Some(root) = queue.pop_front() {
                        break root;
                    }
                    self.queued.wait(&mut queue);
                }
            };
            let fingerprint = self.repos.lock().get(&key(&root)).and_then(|repo| repo.seen);
            let result = read_repo(Path::new(&root));
            let event = self.store(&root, fingerprint, result);
            self.emit("project-git", event);
        }
    }

    /// Keeps the outcome of reading a repository and returns the event announcing it.
    fn store(&self, root: &str, fingerprint: Option<u64>, result: Result<(GitFacts, RepoView), String>) -> GitEvent {
        let mut repos = self.repos.lock();
        let repo = repos.entry(key(root)).or_default();
        repo.queued = false;
        repo.computed = Some(Instant::now());
        repo.fingerprint = fingerprint;
        repo.ignored.clear();
        match result {
            Ok((facts, view)) => {
                repo.facts = Some(Arc::new(facts));
                repo.view = Some(Arc::new(view));
                repo.error = None;
            }
            Err(error) => {
                repo.error = Some(error);
            }
        }
        GitEvent { root: root.to_owned(), git: repo.facts.clone(), error: repo.error.clone() }
    }

    /// The repository's status for badges, read now if it has not been read yet or the last
    /// read is too old to trust.
    ///
    /// A read that failed (git missing, a refused owner, a broken worktree, a timeout) counts
    /// as recent too, and answers with no status. Reading again at once would announce the
    /// repository again, which makes the lists ask for its badges again, round and round.
    fn repo_view(&self, root: &Path) -> Option<Arc<RepoView>> {
        let root_text = root.to_string_lossy().into_owned();
        let k = key(&root_text);
        let indexed = self.engine.index().read().lookup(&root_text).is_some();
        {
            let mut repos = self.repos.lock();
            let repo = repos.entry(k.clone()).or_default();
            repo.badge_interest = Some(Instant::now());
            let max_age = if indexed { GIT_MAX_AGE } else { UNINDEXED_MAX_AGE };
            if repo.computed.is_some_and(|at| at.elapsed() < max_age) {
                return repo.view.clone();
            }
        }
        // Wake the watcher, which keeps this repository current from now on.
        self.wake.notify_one();
        let fingerprint = {
            let index = self.engine.index().read();
            index.lookup(&root_text).map(|entry| facts::scan(&index, entry).fingerprint)
        };
        let event = self.store(&root_text, fingerprint, read_repo(root));
        self.emit("project-git", event);
        self.repos.lock().get(&k).and_then(|repo| repo.view.clone())
    }

    /// Badges for some items of `folder`.
    fn badges(&self, folder: &str, items: &[Item]) -> (Option<String>, Vec<Badge>) {
        let none = || vec![Badge::default(); items.len()];
        let folder_path = PathBuf::from(crate::engine::normalize(folder));
        let Some(repo) = badges::find_repo(&folder_path) else { return (None, none()) };
        let Some(rel) = badges::relative(&repo, &folder_path) else { return (None, none()) };
        let repo_text = repo.to_string_lossy().into_owned();
        let Some(view) = self.repo_view(&repo) else { return (Some(repo_text), none()) };

        let mut out: Vec<Badge> = items
            .iter()
            .map(|item| {
                let item_rel = if rel.is_empty() { item.name.to_lowercase() } else { format!("{rel}\\{}", item.name.to_lowercase()) };
                Badge { change: view.change(&item_rel), ignored: false }
            })
            .collect();

        // Ignored items: ask git about the folder's ancestors and the unchanged items it has
        // not been asked about since the status last changed.
        let (ancestors, paths) = badges::ignore_queries(&rel, items);
        let mut ask: Vec<String> = Vec::new();
        {
            let repos = self.repos.lock();
            let known = repos.get(&key(&repo_text)).map(|r| &r.ignored);
            let unknown = |path: &String| known.is_none_or(|k| !k.contains_key(path));
            ask.extend(ancestors.iter().filter(|p| unknown(p)).cloned());
            for (badge, path) in out.iter().zip(&paths) {
                if badge.change.is_none() && unknown(path) {
                    ask.push(path.clone());
                }
            }
        }
        if !ask.is_empty()
            && let Ok(ignored) = git::ignored(&repo, &ask)
        {
            let ignored: HashSet<String> = ignored.into_iter().map(|p| p.to_lowercase()).collect();
            let mut repos = self.repos.lock();
            let repo_state = repos.entry(key(&repo_text)).or_default();
            for path in ask {
                let is_ignored = ignored.contains(&path) || ignored.contains(path.trim_end_matches('/'));
                repo_state.ignored.insert(path, is_ignored);
            }
        }
        let repos = self.repos.lock();
        if let Some(known) = repos.get(&key(&repo_text)).map(|r| &r.ignored) {
            let folder_ignored = ancestors.iter().any(|p| known.get(p).copied().unwrap_or(false));
            for (badge, path) in out.iter_mut().zip(&paths) {
                badge.ignored = badge.change.is_none() && (folder_ignored || known.get(path).copied().unwrap_or(false));
            }
        }
        (Some(repo_text), out)
    }

    // ---------- what the commands return ----------

    fn project(&self, root: &str) -> Result<Project, String> {
        let inner = self.inner.lock();
        inner.projects.iter().find(|p| p.root.eq_ignore_ascii_case(root)).cloned().ok_or_else(|| format!("{root} is not a known project"))
    }

    fn views(&self) -> Vec<ProjectView> {
        let metas: HashMap<String, ProjectMeta> =
            meta::all(&self.db).unwrap_or_default().into_iter().map(|m| (key(&m.root), m)).collect();
        let repos = self.repos.lock();
        let inner = self.inner.lock();
        inner
            .projects
            .iter()
            .map(|p| {
                let m = metas.get(&key(&p.root)).cloned().unwrap_or_default();
                let repo = repos.get(&key(&p.root));
                let is_repo = p.markers.has(Marker::Git);
                ProjectView {
                    name: name_of(&p.root),
                    root: p.root.clone(),
                    markers: p.markers.iter().map(marker_name).collect(),
                    stack: facts::stack(&p.tree.languages, p.markers),
                    languages: p.tree.languages.iter().take(LIST_LANGUAGES).cloned().collect(),
                    is_repo,
                    git: repo.and_then(|r| r.facts.clone()),
                    git_error: repo.and_then(|r| r.error.clone()),
                    git_pending: is_repo && repo.is_none_or(|r| r.facts.is_none() && r.error.is_none()),
                    last_activity: p.tree.last_activity,
                    size_on_disk: p.tree.size_on_disk,
                    reclaimable: p.tree.reclaimable_bytes(),
                    reclaimable_dirs: p.tree.reclaimable.iter().take(LIST_RECLAIMABLE).cloned().collect(),
                    status: m.status,
                    tags: m.tags,
                    pinned: m.pinned,
                    dismissed: m.dismissed,
                    has_notes: !m.notes.trim().is_empty(),
                }
            })
            .collect()
    }
}

fn marker_name(marker: Marker) -> &'static str {
    match marker {
        Marker::Git => "git",
        Marker::Cargo => "Cargo.toml",
        Marker::Node => "package.json",
        Marker::Python => "pyproject.toml",
        Marker::Go => "go.mod",
        Marker::CMake => "CMakeLists.txt",
        Marker::DotNet => ".sln / .csproj",
        Marker::Unreal => ".uproject",
        Marker::Gradle => "build.gradle",
        Marker::Maven => "pom.xml",
        Marker::Composer => "composer.json",
        Marker::Ruby => "Gemfile",
    }
}

/// Whether two lists show the same projects with the same facts (fingerprints aside, which
/// only matter to git).
fn same_projects(a: &[Project], b: &[Project]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.root == b.root
                && a.markers == b.markers
                && a.tree.size_on_disk == b.tree.size_on_disk
                && a.tree.last_activity == b.tree.last_activity
                && a.tree.reclaimable == b.tree.reclaimable
                && a.tree.languages == b.tree.languages
        })
}

/// Reads everything the hub and the badges need from one repository.
fn read_repo(root: &Path) -> Result<(GitFacts, RepoView), String> {
    let status = git::status(root)?;
    let last_commit = if status.branch.head.is_some() { git::last_commit(root)? } else { None };
    let origin = git::origin(root)?;
    let view = RepoView::new(&status);
    let facts = GitFacts {
        counts: status.counts(),
        branch: status.branch,
        last_commit,
        web_url: origin.as_deref().and_then(git::web_url),
        remote: origin.as_deref().map(git::display_remote),
    };
    Ok((facts, view))
}

// ---------- commands ----------

#[derive(Serialize)]
pub struct ProjectView {
    root: String,
    name: String,
    markers: Vec<&'static str>,
    stack: Vec<String>,
    languages: Vec<LanguageShare>,
    is_repo: bool,
    git: Option<Arc<GitFacts>>,
    git_error: Option<String>,
    /// A repository whose git facts have not arrived yet.
    git_pending: bool,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    last_activity: i64,
    size_on_disk: u64,
    reclaimable: u64,
    reclaimable_dirs: Vec<Reclaimable>,
    status: meta::Status,
    tags: Vec<String>,
    pinned: bool,
    dismissed: bool,
    has_notes: bool,
}

#[derive(Serialize)]
pub struct Snapshot {
    projects: Vec<ProjectView>,
    /// The index is still being built, so the list may be incomplete.
    indexing: bool,
}

type Shared<'a> = State<'a, Arc<Projects>>;

/// The projects found so far. With `refresh` (the hub was just opened) projects are looked for
/// again and every repository's git facts are refreshed; the first time, the look happens
/// before this returns.
#[tauri::command(async)]
pub fn projects_list(projects: Shared, refresh: bool) -> Snapshot {
    let first = {
        let mut inner = projects.inner.lock();
        let first = !inner.active;
        if refresh {
            inner.active = true;
            inner.kick = true;
            inner.refresh_git = true;
        }
        first && refresh
    };
    let ready = projects.engine.status().state == "ready";
    if first && ready {
        projects.look();
    } else if refresh {
        projects.wake.notify_one();
    }
    Snapshot { projects: projects.views(), indexing: !ready }
}

#[derive(Serialize)]
pub struct Readme {
    path: String,
    text: String,
    truncated: bool,
}

#[derive(Serialize)]
pub struct Activity {
    /// The first day shown, in days since the Unix epoch in the viewer's time zone.
    start: i64,
    /// Days with any activity: day, commits, files changed.
    days: Vec<(i64, u32, u32)>,
}

#[derive(Serialize)]
pub struct Detail {
    root: String,
    notes: String,
    readme: Option<Readme>,
    scripts: Vec<Script>,
    launchers: Available,
    activity: Activity,
    recent: Vec<RecentFile>,
    languages: Vec<LanguageShare>,
    reclaimable_dirs: Vec<Reclaimable>,
}

fn find_readme(root: &Path) -> Option<Readme> {
    let mut candidates: Vec<(u8, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let rank = match name.as_str() {
                "readme.md" => 0,
                "readme.markdown" => 1,
                "readme" | "readme.txt" => 2,
                "readme.rst" => 3,
                _ => return None,
            };
            entry.path().is_file().then(|| (rank, entry.path()))
        })
        .collect();
    candidates.sort();
    let (_, path) = candidates.into_iter().next()?;
    let length = std::fs::metadata(&path).ok()?.len();
    let mut bytes = Vec::new();
    use std::io::Read;
    std::fs::File::open(&path).ok()?.take(MAX_README).read_to_end(&mut bytes).ok()?;
    Some(Readme {
        path: path.display().to_string(),
        text: String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_owned(),
        truncated: length > MAX_README,
    })
}

/// Everything the detail panel shows about one project. `offset_minutes` is the viewer's time
/// zone, east of UTC, for the activity heatmap's days.
#[tauri::command(async)]
pub fn project_detail(projects: Shared, root: String, offset_minutes: i64) -> Result<Detail, String> {
    let project = projects.project(&root)?;
    let root = project.root.clone();
    let path = Path::new(&root);
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let today = (now_ms + offset_minutes * 60_000).div_euclid(86_400_000);
    // A year of whole weeks, as the heatmap draws them.
    let start = today - 52 * 7 - (today + 4).rem_euclid(7);
    let since_ms = start * 86_400_000 - offset_minutes * 60_000;

    let is_repo = project.markers.has(Marker::Git);
    // The project's own files are those git does not ignore (build logs, test output and the
    // like are not anyone's work), as well as those the folder rules already leave out.
    let ignored = if is_repo { git::ignored_paths(path).unwrap_or_default() } else { HashSet::new() };
    let (tree, own) = {
        let index = projects.engine.index().read();
        let entry = index.lookup(&root).filter(|&e| index.is_dir(e)).unwrap_or(project.entry);
        if entry >= index.len() || index.is_deleted(entry) {
            return Err(format!("{root} is no longer in the index"));
        }
        (facts::scan(&index, entry), facts::own_activity(&index, entry, &ignored, since_ms, offset_minutes))
    };
    let commits = if is_repo { git::commit_times(path, since_ms / 1000, MAX_HEATMAP_COMMITS).unwrap_or_default() } else { Vec::new() };
    let mut days: HashMap<i64, (u32, u32)> = HashMap::new();
    for time in commits {
        days.entry((time * 1000 + offset_minutes * 60_000).div_euclid(86_400_000)).or_default().0 += 1;
    }
    for (day, files) in own.days {
        days.entry(day).or_default().1 += files;
    }
    let mut days: Vec<(i64, u32, u32)> =
        days.into_iter().filter(|(day, _)| *day >= start && *day <= today).map(|(day, (c, f))| (day, c, f)).collect();
    days.sort();
    let recent = own.recent;

    Ok(Detail {
        notes: meta::get(&projects.db, &root).map(|m| m.notes).unwrap_or_default(),
        readme: find_readme(path),
        scripts: scripts::detect(path),
        launchers: launch::available(path),
        activity: Activity { start, days },
        recent,
        languages: tree.languages.clone(),
        reclaimable_dirs: tree.reclaimable.clone(),
        root,
    })
}

#[tauri::command(async)]
pub fn project_set_status(projects: Shared, root: String, status: meta::Status) -> Result<(), String> {
    Ok(meta::set_status(&projects.db, &root, status)?)
}

#[tauri::command(async)]
pub fn project_set_tags(projects: Shared, root: String, tags: Vec<String>) -> Result<Vec<String>, String> {
    Ok(meta::set_tags(&projects.db, &root, &tags)?)
}

#[tauri::command(async)]
pub fn project_set_notes(projects: Shared, root: String, notes: String) -> Result<(), String> {
    Ok(meta::set_notes(&projects.db, &root, &notes)?)
}

#[tauri::command(async)]
pub fn project_set_pinned(projects: Shared, root: String, pinned: bool) -> Result<(), String> {
    Ok(meta::set_pinned(&projects.db, &root, pinned)?)
}

/// "Not a project", or undoing it.
#[tauri::command(async)]
pub fn project_set_dismissed(projects: Shared, root: String, dismissed: bool) -> Result<(), String> {
    meta::set_dismissed(&projects.db, &root, dismissed)?;
    projects.emit("projects-changed", ());
    Ok(())
}

/// Opens the project in another program.
#[tauri::command(async)]
pub fn project_launch(projects: Shared, root: String, launcher: Launcher) -> Result<(), String> {
    let project = projects.project(&root)?;
    let web = projects.repos.lock().get(&key(&project.root)).and_then(|r| r.facts.as_ref()).and_then(|f| f.web_url.clone());
    launch::start(launch::plan(launcher, &project.root, web.as_deref())?)
}

/// Runs one of the project's own scripts in a new terminal in its folder. The script is named
/// rather than sent, and must be one the project defines.
#[tauri::command(async)]
pub fn project_run_script(projects: Shared, root: String, source: String, name: String) -> Result<(), String> {
    let project = projects.project(&root)?;
    let script = scripts::detect(Path::new(&project.root))
        .into_iter()
        .find(|s| s.source == source && s.name == name)
        .ok_or_else(|| format!("{name} is not one of this project's scripts"))?;
    launch::start(launch::in_terminal(&project.root, Some(&script.command)))
}

#[derive(Serialize)]
pub struct Badges {
    /// The repository the folder is in, if any.
    repo: Option<String>,
    /// One per item asked about, in order.
    badges: Vec<Badge>,
}

/// Git badges for some items of a folder: those on screen.
#[tauri::command(async)]
pub fn git_badges(projects: Shared, folder: String, items: Vec<Item>) -> Badges {
    let (repo, badges) = projects.badges(&folder, &items);
    Badges { repo, badges }
}

/// Development builds only: records launches instead of performing them while `on`, and
/// lets projects be discovered under `allow` (test fixtures live in the temp folder, which the
/// location rules exclude).
#[tauri::command(async)]
pub fn projects_probe(projects: Shared, on: bool, allow: Vec<String>) -> Result<(), String> {
    launch::development_only()?;
    launch::set_probe(on);
    let mut inner = projects.inner.lock();
    inner.allowed = allow;
    inner.kick = true;
    projects.wake.notify_one();
    Ok(())
}

/// Development builds only: the launches recorded since the last call.
#[tauri::command(async)]
pub fn projects_probe_take() -> Result<Vec<Launch>, String> {
    launch::development_only()?;
    Ok(launch::take_records())
}

#[cfg(test)]
mod tests;
