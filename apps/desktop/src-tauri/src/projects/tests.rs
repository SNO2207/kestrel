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
fn slow_git_commands_are_abandoned() {
    let error = git::run(&std::env::temp_dir(), &["--version"], None, std::time::Duration::ZERO).unwrap_err();
    assert!(error.contains("took longer"), "{error}");
    let output = git::run(&std::env::temp_dir(), &["--version"], None, git::TIMEOUT).unwrap();
    assert!(output.ok() && output.text().starts_with("git version"));
}
