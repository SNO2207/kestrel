//! Projects end to end against real repositories, made in a scratch folder under %TEMP% with
//! the git command line: a repository with a bare remote it is ahead of and behind, staged,
//! modified, untracked and ignored files, a nested `node_modules` and an embedded repository
//! (as a submodule is), and a project without git beside it.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use kestrel_index::{IndexBuilder, walk};

use super::*;
use crate::projects::git::Change;

/// Runs git for the fixture, isolated from the machine's own git configuration (signing,
/// hooks, templates) by the empty file `config`, and fails the test if it fails.
fn git(config: &Path, dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(["-c", "protocol.file.allow=always", "-c", "advice.addEmbeddedRepo=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Kestrel Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .env("GIT_COMMITTER_NAME", "Kestrel Tester")
        .env("GIT_COMMITTER_EMAIL", "tester@example.com")
        .creation_flags(0x0800_0000)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Fixture {
    base: PathBuf,
    /// The folder projects may be discovered in; see `Inner::allowed`.
    work: PathBuf,
    app: PathBuf,
    plain: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Builds the scratch repositories. `name` keeps parallel tests apart.
fn fixture(name: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("kestrel-projects-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let work = base.join("work");
    let app = work.join("app");
    let plain = work.join("plain");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir_all(&plain).unwrap();
    let config = base.join("gitconfig");
    fs::write(&config, "").unwrap();
    let git = |dir: &Path, args: &[&str]| git(&config, dir, args);

    // A bare remote and a second clone that pushes a commit the app has not seen. Both sit
    // outside `work`, the folder the tests index.
    git(&base, &["init", "--bare", "-q", "remote.git"]);
    let remote = base.join("remote.git");
    let remote_text = remote.to_str().unwrap();

    write(&app.join("Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
    write(&app.join("src").join("main.rs"), "fn main() {\n    println!(\"hello\");\n}\n");
    write(&app.join("README.md"), "# App\n\nA fixture.\n");
    write(&app.join("web").join("package.json"), r#"{"scripts":{"dev":"vite"}}"#);
    write(&app.join(".gitignore"), "node_modules/\n*.log\ntarget/\n");
    // An embedded repository, as a submodule is: its own .git, recorded as a gitlink.
    let inner = app.join("libs").join("inner");
    write(&inner.join("Cargo.toml"), "[package]\nname = \"inner\"\n");
    git(&inner, &["init", "-q"]);
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-q", "-m", "Inner commit"]);
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "Initial commit"]);
    git(&app, &["remote", "add", "origin", remote_text]);
    git(&app, &["push", "-q", "-u", "origin", "main"]);

    git(&base, &["clone", "-q", remote_text, "other"]);
    let other = base.join("other");
    write(&other.join("CHANGELOG.md"), "Changes\n");
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "Commit from elsewhere"]);
    git(&other, &["push", "-q"]);

    // The app commits on its own and then fetches: one ahead, one behind.
    write(&app.join("src").join("lib.rs"), "pub fn lib() {}\n");
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "Add the library"]);
    git(&app, &["fetch", "-q"]);
    // Point origin at a hosted address only now, after fetching: the link test needs one.
    git(&app, &["remote", "set-url", "origin", "git@github.com:kestrel-test/app.git"]);

    // Working tree changes: one modified, one staged, one untracked, two ignored.
    write(&app.join("src").join("main.rs"), "fn main() {\n    println!(\"changed\");\n}\n");
    write(&app.join("src").join("staged.rs"), "pub fn staged() {}\n");
    git(&app, &["add", "src/staged.rs"]);
    write(&app.join("notes.txt"), "untracked\n");
    write(&app.join("debug.log"), "ignored\n");
    write(&app.join("node_modules").join("dep").join("package.json"), r#"{"name":"dep"}"#);
    write(&app.join("node_modules").join("dep").join("index.js"), "module.exports = 1;\n");
    write(&app.join("target").join("debug").join("app.exe"), &"x".repeat(5000));

    // A project without git, beside it.
    write(&plain.join("pyproject.toml"), "[project]\nname = \"plain\"\n");
    write(&plain.join("main.py"), "print('hi')\n");

    Fixture { base, work, app, plain }
}

/// An engine whose index holds the fixture, and the projects service over it.
fn service(fixture: &Fixture) -> Arc<Projects> {
    let engine = Arc::new(Engine::default());
    // Only `work` is indexed: the remote and the other clone are not projects to find.
    let mut builder = IndexBuilder::new();
    walk::walk(fixture.work.to_str().unwrap(), &mut builder).unwrap();
    engine.install(builder.finish());
    let db = Arc::new(Db::open_in_memory().unwrap());
    let projects = Projects::new(engine, db);
    {
        let mut inner = projects.inner.lock();
        inner.active = true;
        // The fixture lives in the temp folder, under AppData, which the rules exclude.
        inner.allowed = vec![fixture.work.to_str().unwrap().to_owned()];
    }
    projects
}

/// The bytes and modification time of the repository's index file, which `git status` would
/// rewrite if it were allowed to take its optional lock.
fn index_file(repo: &Path) -> (Vec<u8>, std::time::SystemTime) {
    let path = repo.join(".git").join("index");
    (fs::read(&path).unwrap(), fs::metadata(&path).unwrap().modified().unwrap())
}

#[test]
fn discovers_the_repository_and_the_plain_project_but_nothing_nested() {
    let fixture = fixture("discover");
    let projects = service(&fixture);
    projects.look();
    let views = projects.views();
    let roots: Vec<&str> = views.iter().map(|v| v.root.as_str()).collect();
    assert_eq!(roots, [fixture.app.to_str().unwrap(), fixture.plain.to_str().unwrap()]);

    let app = &views[0];
    assert_eq!(app.name, "app");
    assert!(app.is_repo);
    assert_eq!(app.markers, ["git", "Cargo.toml", "package.json"]);
    assert_eq!(app.stack, ["Rust", "Node.js"]);
    // node_modules and target are build output, totalled but not walked.
    let reclaimable: Vec<&str> = app.reclaimable_dirs.iter().map(|r| r.path.rsplit('\\').next().unwrap()).collect();
    assert_eq!(reclaimable.len(), 2);
    assert!(reclaimable.contains(&"node_modules") && reclaimable.contains(&"target"));
    assert!(app.reclaimable >= 5000 && app.reclaimable < app.size_on_disk);
    assert!(app.last_activity > 0);

    let plain = &views[1];
    assert!(!plain.is_repo && !plain.git_pending);
    assert_eq!(plain.stack, ["Python"]);

    // Every repository that is a project was queued for git; the plain folder was not.
    let queued: Vec<String> = projects.queue.lock().iter().cloned().collect();
    assert_eq!(queued, [fixture.app.to_str().unwrap()]);
}

#[test]
fn git_facts_come_from_the_command_line_without_writing() {
    let fixture = fixture("facts");
    let before = index_file(&fixture.app);
    let (facts, _view) = read_repo(&fixture.app).unwrap();
    assert_eq!(facts.branch.name.as_deref(), Some("main"));
    assert_eq!(facts.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!((facts.branch.ahead, facts.branch.behind), (1, 1));
    assert_eq!(facts.counts, Counts { staged: 1, modified: 1, untracked: 1, conflicted: 0 });
    let commit = facts.last_commit.as_ref().unwrap();
    assert_eq!(commit.subject, "Add the library");
    assert_eq!(commit.author, "Kestrel Tester");
    assert!(commit.time > 1_700_000_000_000);
    assert_eq!(facts.remote.as_deref(), Some("git@github.com:kestrel-test/app.git"));
    assert_eq!(facts.web_url.as_deref(), Some("https://github.com/kestrel-test/app"));
    assert_eq!(index_file(&fixture.app), before, "git status must not touch the index file");

    // A year of commits for the heatmap: the two on this branch.
    let year_ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 365 * 86_400;
    assert_eq!(git::commit_times(&fixture.app, year_ago, 100).unwrap().len(), 2);
}

#[test]
fn badges_mark_changes_and_ignored_items_in_folder_views() {
    let fixture = fixture("badges");
    let projects = service(&fixture);
    let before = index_file(&fixture.app);
    let items = |names: &[(&str, bool)]| -> Vec<Item> {
        names.iter().map(|&(name, dir)| Item { name: name.to_owned(), dir }).collect()
    };
    let root = fixture.app.to_str().unwrap();
    let (repo, badges) = projects.badges(
        root,
        &items(&[
            ("src", true),
            ("libs", true),
            ("node_modules", true),
            ("target", true),
            ("notes.txt", false),
            ("debug.log", false),
            ("README.md", false),
            ("Cargo.toml", false),
        ]),
    );
    assert_eq!(repo.as_deref(), Some(root));
    let shown: Vec<(Option<Change>, bool)> = badges.iter().map(|b| (b.change, b.ignored)).collect();
    assert_eq!(
        shown,
        [
            (Some(Change::Modified), false),
            (None, false),
            (None, true),
            (None, true),
            (Some(Change::Untracked), false),
            (None, true),
            (None, false),
            (None, false),
        ]
    );

    let src = fixture.app.join("src");
    let (_, badges) =
        projects.badges(src.to_str().unwrap(), &items(&[("main.rs", false), ("staged.rs", false), ("lib.rs", false)]));
    let changes: Vec<Option<Change>> = badges.iter().map(|b| b.change).collect();
    assert_eq!(changes, [Some(Change::Modified), Some(Change::Added), None]);

    // A folder holding the embedded repository: git refuses to answer about paths inside a
    // submodule, which must not cost the other items their answers.
    let libs = fixture.app.join("libs");
    write(&libs.join("build.log"), "ignored\n");
    let (_, badges) = projects.badges(libs.to_str().unwrap(), &items(&[("inner", true), ("build.log", false)]));
    let shown: Vec<(Option<Change>, bool)> = badges.iter().map(|b| (b.change, b.ignored)).collect();
    assert_eq!(shown, [(None, false), (None, true)]);

    // Inside an ignored folder, everything is ignored.
    let dep = fixture.app.join("node_modules").join("dep");
    let (_, badges) = projects.badges(dep.to_str().unwrap(), &items(&[("index.js", false), ("package.json", false)]));
    assert!(badges.iter().all(|b| b.ignored && b.change.is_none()));

    // Outside any repository there are no badges (unless the temp folder itself is in one).
    if badges::find_repo(&std::env::temp_dir()).is_none() {
        let (repo, badges) = projects.badges(fixture.plain.to_str().unwrap(), &items(&[("main.py", false)]));
        assert_eq!(repo, None);
        assert_eq!(badges, [Badge::default()]);
    }
    assert_eq!(index_file(&fixture.app), before, "badges must not touch the index file");
}

#[test]
fn a_changed_fingerprint_queues_a_refresh_and_an_unchanged_one_does_not() {
    let fixture = fixture("fingerprint");
    let projects = service(&fixture);
    let root = fixture.app.to_str().unwrap();
    let store = |fingerprint| {
        let result = read_repo(&fixture.app);
        projects.store(root, fingerprint, result);
    };
    store(Some(7));
    projects.consider(root, Some(7), false);
    assert!(projects.queue.lock().is_empty(), "nothing changed");
    projects.consider(root, Some(8), false);
    assert_eq!(projects.queue.lock().len(), 1, "the fingerprint moved");
    projects.consider(root, Some(9), false);
    assert_eq!(projects.queue.lock().len(), 1, "already queued once");
    projects.queue.lock().clear();
    store(Some(9));
    projects.consider(root, Some(9), true);
    assert_eq!(projects.queue.lock().len(), 1, "the hub was opened");
}

#[test]
fn detail_scripts_readme_and_dismissal() {
    let fixture = fixture("detail");
    let projects = service(&fixture);
    projects.look();
    let scripts: Vec<String> = scripts::detect(&fixture.app).into_iter().map(|s| s.command).collect();
    assert_eq!(scripts, ["cargo build", "cargo test", "cargo run"]);
    let readme = find_readme(&fixture.app).unwrap();
    assert!(readme.text.starts_with("# App") && !readme.truncated);

    // Recently changed files leave out what git ignores, such as the log, and count the rest.
    let ignored = git::ignored_paths(&fixture.app).unwrap();
    assert!(ignored.contains("debug.log") && ignored.contains("node_modules/") && ignored.contains("target/"), "{ignored:?}");
    let own = {
        let index = projects.engine.index().read();
        let entry = index.lookup(fixture.app.to_str().unwrap()).unwrap();
        facts::own_activity(&index, entry, &ignored, 0, 0)
    };
    let names: Vec<String> = own.recent.iter().map(|f| f.name.clone()).collect();
    assert!(!names.contains(&"debug.log".to_owned()), "{names:?}");
    assert!(names.contains(&"notes.txt".to_owned()) && names.contains(&"main.rs".to_owned()), "{names:?}");
    // Every file the fixture wrote today, except the ignored log, counts towards today.
    assert_eq!(own.days.values().sum::<u32>() as usize, names.len());

    // Dismissed projects stay listed, marked, and their repositories are no longer refreshed.
    let root = fixture.app.to_str().unwrap();
    meta::set_dismissed(&projects.db, root, true).unwrap();
    projects.queue.lock().clear();
    projects.repos.lock().clear();
    projects.inner.lock().refresh_git = true;
    projects.look();
    assert!(projects.views().iter().any(|v| v.root == root && v.dismissed));
    assert!(projects.queue.lock().is_empty());

    assert!(projects.project(r"C:\nowhere").is_err(), "only known projects can be launched");
}

#[test]
fn the_palette_offers_projects_by_name_stack_tag_and_status() {
    let fixture = fixture("palette");
    let projects = service(&fixture);
    // As at startup: nothing has asked for projects yet.
    projects.inner.lock().active = false;
    projects.engine.set_status("ready", false);
    let palette = crate::palette::Palette::new(Arc::new(Db::open_in_memory().unwrap()));
    palette.register(Arc::new(ProjectsProvider::new(projects.clone())));
    let ask = |text: &str| -> Vec<String> {
        let request = crate::palette::Request { text: text.into(), session: "t".into(), items: Vec::new(), show_hidden: false, limit: None };
        palette.query(request).unwrap().hits.into_iter().map(|hit| hit.item.title).collect()
    };
    let app = fixture.app.to_str().unwrap();
    let plain = fixture.plain.to_str().unwrap();

    assert!(ask("app").is_empty(), "a plain query never starts looking for projects");
    meta::set_pinned(&projects.db, plain, true).unwrap();
    assert_eq!(ask("@"), ["plain", "app"], "`@` looks for them, and lists the pinned first");
    assert!(projects.inner.lock().active, "and keeps them current from then on");
    assert_eq!(ask("app"), ["app"], "once found, plain queries find them too");
    assert_eq!(ask("@rust"), ["app"], "by stack");
    meta::set_tags(&projects.db, app, &["client".to_owned()]).unwrap();
    assert_eq!(ask("@client"), ["app"], "by tag");
    meta::set_status(&projects.db, plain, meta::Status::Paused).unwrap();
    assert_eq!(ask("@paused"), ["plain"], "by status");
    meta::set_dismissed(&projects.db, plain, true).unwrap();
    assert_eq!(ask("@"), ["app"], "dismissed folders are not offered");
}

#[test]
fn slow_git_commands_are_abandoned() {
    let error = git::run(&std::env::temp_dir(), &["--version"], None, std::time::Duration::ZERO).unwrap_err();
    assert!(error.contains("took longer"), "{error}");
    let output = git::run(&std::env::temp_dir(), &["--version"], None, git::TIMEOUT).unwrap();
    assert!(output.ok() && output.text().starts_with("git version"));
}

/// A repository git can't read (here a worktree whose main repository is gone; equally git
/// missing from the PATH, a refused owner or a timeout) has an error and no status. Each badge
/// request used to read it again and announce it, and the announcement made the list ask
/// again, forever. A failed read is trusted for as long as a successful one would be.
#[test]
fn a_repository_git_cannot_read_is_not_read_again_for_every_badge_request() {
    let fixture = fixture("unreadable");
    let projects = service(&fixture);
    let broken = fixture.work.join("broken");
    write(&broken.join(".git"), "gitdir: C:/kestrel-nowhere/missing.git\n");
    write(&broken.join("a.txt"), "a\n");
    let items = [Item { name: "a.txt".to_owned(), dir: false }];
    let folder = broken.to_str().unwrap();

    let (repo, badges) = projects.badges(folder, &items);
    assert_eq!(repo.as_deref(), Some(folder));
    assert_eq!(badges, [Badge::default()]);
    let read_at = |projects: &Projects| {
        let repos = projects.repos.lock();
        let repo = repos.get(&key(folder)).expect("the repository is known");
        assert!(repo.error.is_some() && repo.view.is_none(), "git can't read it");
        repo.computed
    };
    let first = read_at(&projects);
    for _ in 0..3 {
        projects.badges(folder, &items);
    }
    assert_eq!(read_at(&projects), first, "git was not run again");
}

/// Sets a file's last write time back, as extracting an archive leaves it: git then hashes the
/// file to tell whether it changed, through whatever filter its attributes name.
fn backdate(path: &Path) {
    let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    fs::File::options().write(true).open(path).unwrap().set_modified(then).unwrap();
}

/// A repository extracted from an archive brings its own `.git/config`, which can name filter
/// programs that `git status` runs on every file whose timestamps changed. Opening the folder
/// in Kestrel must run none of them: not the repository's, and not an embedded repository's.
#[test]
fn reading_a_repository_never_runs_the_filter_programs_it_names() {
    let base = std::env::temp_dir().join(format!("kestrel-projects-filters-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let inner = repo.join("inner");
    fs::create_dir_all(&inner).unwrap();
    let config = base.join("gitconfig");
    fs::write(&config, "").unwrap();
    let git = |dir: &Path, args: &[&str]| git(&config, dir, args);
    let marker = |name: &str| base.join(name);
    // The filter writes a marker, then passes the contents through unchanged.
    let filter = |name: &str| format!("echo ran >> '{}'; cat", marker(name).to_str().unwrap().replace('\\', "/"));

    write(&inner.join("c.txt"), "c\n");
    git(&inner, &["init", "-q"]);
    git(&inner, &["config", "filter.inner-evil.clean", &filter("inner.txt")]);
    write(&inner.join(".git").join("info").join("attributes"), "* filter=inner-evil\n");
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-q", "-m", "Inner"]);

    write(&repo.join("a.txt"), "a\n");
    write(&repo.join("b.md"), "b\n");
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "Initial"]);
    // Two drivers, one with dots in its name, applied by the repository's own attributes.
    git(&repo, &["config", "filter.evil.clean", &filter("repo.txt")]);
    git(&repo, &["config", "filter.odd.name.clean", &filter("repo.txt")]);
    git(&repo, &["config", "filter.odd.name.required", "true"]);
    write(&repo.join(".git").join("info").join("attributes"), "*.txt filter=evil\n*.md filter=odd.name\n");
    for file in [repo.join("a.txt"), repo.join("b.md"), inner.join("c.txt")] {
        backdate(&file);
    }

    // The fixture is armed: a plain `git status` runs both repositories' filters (without
    // writing the index, which would record the new timestamps and disarm it).
    git(&repo, &["--no-optional-locks", "status", "--porcelain"]);
    assert!(marker("repo.txt").exists() && marker("inner.txt").exists(), "a plain status runs the filters");
    fs::remove_file(marker("repo.txt")).unwrap();
    fs::remove_file(marker("inner.txt")).unwrap();

    let status = git::status(&repo).unwrap();
    assert!(status.entries.is_empty(), "nothing changed: {:?}", status.entries);
    git::ignored_paths(&repo).unwrap();
    read_repo(&repo).unwrap();
    assert!(!marker("repo.txt").exists(), "the repository's filters never ran");
    assert!(!marker("inner.txt").exists(), "the embedded repository's filter never ran");
    fs::remove_dir_all(&base).unwrap();
}

/// A project in OneDrive with its files online only: its README and manifests are never opened
/// (that would download them), whoever asks for its details; the query service lets other
/// programs ask.
#[test]
fn online_only_readmes_and_manifests_are_never_opened() {
    let dir = std::env::temp_dir().join(format!("kestrel-projects-cloud-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("README.md"), "# In the cloud\n").unwrap();
    fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();
    fs::write(dir.join("Cargo.toml"), "[package]\nname = \"cloud\"\n").unwrap();
    fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
    let set = |name: &str, attributes: u32| {
        let wide: Vec<u16> = dir.join(name).to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        assert_ne!(unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attributes) }, 0);
    };
    // Offline: the contents are elsewhere, as OneDrive's online-only files say.
    for name in ["README.md", "package.json", "Cargo.toml"] {
        set(name, 0x1000);
    }
    assert!(find_readme(&dir).is_none());
    assert!(scripts::detect(&dir).is_empty());
    for name in ["README.md", "package.json", "Cargo.toml"] {
        set(name, 0x80);
    }
    assert!(find_readme(&dir).unwrap().text.starts_with("# In the cloud"));
    let commands: Vec<String> = scripts::detect(&dir).into_iter().map(|s| s.command).collect();
    assert_eq!(commands, ["npm run dev", "cargo build", "cargo test", "cargo run"]);
    fs::remove_dir_all(&dir).unwrap();
}
