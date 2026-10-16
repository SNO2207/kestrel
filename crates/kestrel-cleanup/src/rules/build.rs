//! Build output in stale projects: projects nobody has changed a file in for 90 days, whose
//! `node_modules`, `target`, `bin` and `obj`, virtual environments, `__pycache__`, `dist`,
//! `build`, `.next`, `.nuxt`, `.gradle`, Unreal's `Intermediate`, `DerivedDataCache` and
//! `Saved`, and Unity's `Library` and `Temp` can all be made again from the sources.
//!
//! A folder only counts as build output beside the file that makes it so (`target` beside
//! `Cargo.toml`, `bin` beside a `.csproj`…), and `dist` or `build` holding build files of its
//! own is someone's sources, not output. Cargo's output goes with `cargo clean` when Cargo is
//! installed; everything else to the Recycle Bin.

use std::collections::HashSet;

use kestrel_index::{EntryId, Index, NO_ENTRY, attr};
use rayon::prelude::*;

use super::{days, file_name};
use crate::locations::join;
use crate::tree::{child, find_dir, item_of, name};
use crate::{Claims, Context, DAY, Guard, Recommendation, Rule, Tier, Tool, fold_path, within};

/// Days without a change to a project's own files before its build output is offered.
pub const STALE_DAYS: i64 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    /// Cargo's `target`, which `cargo clean` removes.
    Cargo,
    Plain,
}

/// Folders kept with the sources that aren't the project's own work: not looked in.
const FOREIGN: &[&str] = &["vendor", "bower_components", ".vs", ".idea", ".tox", ".mypy_cache", ".pytest_cache", ".vscode"];

/// Files whose presence makes a folder someone's sources rather than build output.
const SOURCE_MARKERS: &[&str] =
    &["cmakelists.txt", "package.json", "build.gradle", "build.gradle.kts", "cargo.toml", "pyproject.toml", "setup.py"];

/// What a folder called `name` (folded) is, beside `siblings` (folded names).
fn classify(index: &Index, entry: EntryId, name: &str, siblings: &[&str]) -> Option<Output> {
    let has = |sibling: &str| siblings.contains(&sibling);
    let has_ext = |ext: &str| siblings.iter().any(|s| s.ends_with(ext));
    let dotnet = has_ext(".csproj") || has_ext(".fsproj") || has_ext(".vbproj") || has_ext(".vcxproj");
    let node = has("package.json");
    let python = has("pyproject.toml") || has("setup.py") || has("setup.cfg");
    let gradle = ["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "gradlew"].iter().any(|n| has(n));
    let unreal = has_ext(".uproject");
    let unity = has("assets") && has("projectsettings");
    let holds_sources = || index.children(entry).any(|c| SOURCE_MARKERS.contains(&self::name(index, c)) || self::name(index, c).ends_with(".csproj"));
    Some(match name {
        "node_modules" | "__pycache__" => Output::Plain,
        "target" if has("cargo.toml") => Output::Cargo,
        "target" if has("pom.xml") => Output::Plain,
        "bin" | "obj" if dotnet => Output::Plain,
        ".venv" | "venv" if child(index, entry, "pyvenv.cfg").is_some() => Output::Plain,
        "dist" if (node || python) && !holds_sources() => Output::Plain,
        "build" if (node || python || gradle || has("cmakelists.txt")) && !holds_sources() => Output::Plain,
        ".next" | ".nuxt" if node => Output::Plain,
        ".gradle" if gradle => Output::Plain,
        "intermediate" | "deriveddatacache" | "saved" if unreal => Output::Plain,
        "library" | "temp" if unity => Output::Plain,
        _ => return None,
    })
}

/// One walk of a project: the newest change to its own files, and its build output folders.
fn walk(index: &Index, root: EntryId) -> (i64, Vec<(EntryId, Output)>) {
    let mut newest = 0;
    let mut outputs = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let children: Vec<EntryId> = index.children(dir).collect();
        let siblings: Vec<&str> = children.iter().map(|&c| name(index, c)).collect();
        for (&entry, &entry_name) in children.iter().zip(&siblings) {
            if !index.is_dir(entry) {
                newest = newest.max(index.mtime(entry));
                continue;
            }
            if entry_name == ".git" {
                // A commit, a checkout or staging changes git's own top-level files.
                for inside in index.children(entry) {
                    if !index.is_dir(inside) {
                        newest = newest.max(index.mtime(inside));
                    }
                }
                continue;
            }
            if let Some(output) = classify(index, entry, entry_name, &siblings) {
                outputs.push((entry, output));
                continue;
            }
            if index.attrs(entry) & attr::REPARSE_POINT != 0 || FOREIGN.contains(&entry_name) {
                continue;
            }
            stack.push(entry);
        }
    }
    (newest, outputs)
}

/// Unity projects (`ProjectSettings\ProjectVersion.txt` beside `Assets`), which have no file the
/// projects list knows them by unless they are in git.
fn unity_projects(index: &Index, context: &Context) -> Vec<String> {
    let found: Vec<EntryId> = (0..index.len())
        .into_par_iter()
        .with_min_len(1 << 14)
        .filter(|&id| index.lower_bytes(id) == b"projectversion.txt" && !index.is_deleted(id))
        .collect();
    let app_data = fold_path(&join(&context.locations.profile, "AppData"));
    found
        .into_iter()
        .filter_map(|id| {
            let settings = index.parent(id);
            if settings == NO_ENTRY || name(index, settings) != "projectsettings" {
                return None;
            }
            let root = index.parent(settings);
            (root != NO_ENTRY && child(index, root, "assets").is_some()).then(|| index.path(root))
        })
        .filter(|root| !within(&fold_path(root), &app_data))
        .collect()
}

/// Whether a project lies in a hidden folder such as `~\.codex\plugins\cache`, where a tool put
/// it rather than its owner: there, its virtual environment may be what makes the tool run.
fn tool_managed(root: &str) -> bool {
    root.split('\\').skip(1).any(|part| part.starts_with('.'))
}

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let listed: Vec<String> =
        context.projects.iter().filter(|p| !p.dismissed && !tool_managed(&p.root)).map(|p| p.root.clone()).collect();
    let dismissed: Vec<String> = context.projects.iter().filter(|p| p.dismissed).map(|p| fold_path(&p.root)).collect();
    let mut roots = listed.clone();
    let known: Vec<String> = context.projects.iter().map(|p| fold_path(&p.root)).collect();
    for unity in unity_projects(index, context) {
        if !known.iter().any(|root| within(&fold_path(&unity), root)) && !tool_managed(&unity) {
            roots.push(unity);
        }
    }
    let unity_guard;
    let guard = if roots.len() > listed.len() {
        // Unity projects found here are projects too: their sources are protected like any other.
        let mut all: Vec<String> = context.projects.iter().map(|p| p.root.clone()).collect();
        all.extend(roots[listed.len()..].iter().cloned());
        unity_guard = Guard::new(&context.locations, &context.protected, &all);
        &unity_guard
    } else {
        guard
    };

    let cutoff = context.now - STALE_DAYS * DAY;
    let mut seen = HashSet::new();
    let mut recs = Vec::new();
    for root_path in roots {
        let folded = fold_path(&root_path);
        if !seen.insert(folded.clone()) || dismissed.iter().any(|d| within(&folded, d)) {
            continue;
        }
        let Some(root) = find_dir(index, &root_path) else { continue };
        let (newest, outputs) = walk(index, root);
        if newest <= 0 || newest > cutoff {
            continue;
        }
        let idle = (context.now - newest) / DAY;
        let project = file_name(&root_path).to_owned();
        let reason = format!(
            "Nobody has changed a file in {project} for {}. Its build output is made again from its sources the next time \
             it is built.",
            days(idle)
        );
        let outputs: Vec<(EntryId, Output)> = outputs
            .into_iter()
            .filter(|&(entry, _)| index.alloc(entry) > 0 && !claims.covers(index, entry) && guard.allows_build_output(index, entry))
            .collect();
        let mut plain = Vec::new();
        for (entry, output) in outputs {
            claims.claim(entry);
            if output == Output::Cargo && context.tools.has("cargo") {
                let folder = index.path(index.parent(entry));
                // Told which folder to clean: otherwise Cargo cleans the one its environment or
                // configuration names (CARGO_TARGET_DIR, `build.target-dir`), which may be a
                // build folder shared by every project, and leaves the one shown.
                let target = index.path(entry);
                let tool = Tool::new("cargo", &["clean", "--target-dir", &target], Some(&folder)).shown_as("cargo clean");
                recs.push(
                    Recommendation::new(
                        format!("cargo:{}", fold_path(&folder)),
                        Rule::Build,
                        Tier::Safe,
                        format!("Rust build output in {project}"),
                        reason.clone(),
                    )
                    .with_items(vec![item_of(index, entry)])
                    .with_tool(tool)
                    .with_note("cargo clean deletes the target folder outright: it doesn't go to the Recycle Bin."),
                );
            } else {
                plain.push(item_of(index, entry));
            }
        }
        if !plain.is_empty() {
            recs.push(
                Recommendation::new(format!("build:{folded}"), Rule::Build, Tier::Safe, format!("Build output in {project}"), reason)
                    .with_items(plain),
            );
        }
    }
    recs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, item_paths};
    use crate::{Action, Project, Tools, scan};

    const CODE: &str = r"Users\me\code";

    fn project(sandbox: &Sandbox, name: &str) -> Project {
        Project { root: sandbox.text(&format!(r"{CODE}\{name}")), dismissed: false }
    }

    /// Writes a file of the project, sized to take clusters of its own.
    fn put(sandbox: &Sandbox, rel: &str) {
        sandbox.write(&format!(r"{CODE}\{rel}"), 9000, rel.len() as u8);
    }

    #[test]
    fn build_output_of_stale_projects_only_is_offered() {
        let sandbox = Sandbox::new("build");
        // A stale Rust and web project, a stale .NET, Python and Unreal one, and an active one.
        for file in [r"rusty\Cargo.toml", r"rusty\src\main.rs", r"rusty\target\debug\rusty.exe", r"rusty\web\package.json"] {
            put(&sandbox, file);
        }
        put(&sandbox, r"rusty\web\node_modules\left-pad\index.js");
        put(&sandbox, r"rusty\web\dist\app.js");
        put(&sandbox, r"rusty\.git\HEAD");
        for file in [r"net\App.csproj", r"net\Program.cs", r"net\bin\Debug\App.dll", r"net\obj\project.assets.json"] {
            put(&sandbox, file);
        }
        for file in [r"py\pyproject.toml", r"py\pkg\mod.py", r"py\pkg\__pycache__\mod.cpython-312.pyc", r"py\.venv\pyvenv.cfg"] {
            put(&sandbox, file);
        }
        put(&sandbox, r"py\.venv\Lib\site.py");
        // `build` holding its own build file is sources, and a `venv` without pyvenv.cfg is not one.
        put(&sandbox, r"py\build\CMakeLists.txt");
        put(&sandbox, r"py\venv\notes.txt");
        for file in [r"game\Game.uproject", r"game\Source\Game.cpp", r"game\Intermediate\Build\x.obj", r"game\Saved\Logs\Game.log"] {
            put(&sandbox, file);
        }
        for file in [r"active\Cargo.toml", r"active\src\lib.rs", r"active\target\debug\lib.rlib"] {
            put(&sandbox, file);
        }
        sandbox.age_tree(CODE, 120);
        sandbox.age(&format!(r"{CODE}\active\src\lib.rs"), 3);

        let mut context = sandbox.context();
        context.projects = ["rusty", "net", "py", "game", "active"].iter().map(|name| project(&sandbox, name)).collect();
        context.tools = Tools::with(&["cargo"]);
        let recs = scan(&sandbox.index(), &context);
        let build: Vec<_> = recs.iter().filter(|r| r.rule == Rule::Build).cloned().collect();
        assert_eq!(
            item_paths(&sandbox, &build),
            [
                "users/me/code/game/intermediate",
                "users/me/code/game/saved",
                "users/me/code/net/bin",
                "users/me/code/net/obj",
                "users/me/code/py/.venv",
                "users/me/code/py/pkg/__pycache__",
                "users/me/code/rusty/target",
                "users/me/code/rusty/web/dist",
                "users/me/code/rusty/web/node_modules",
            ]
        );
        // Cargo's own tool for `target`, run in the project and told which folder to clean.
        let cargo = build.iter().find(|r| r.id.starts_with("cargo:")).unwrap();
        let Action::Tool(tool) = &cargo.action else { panic!("{cargo:?}") };
        assert_eq!(tool.command, "cargo clean");
        assert_eq!(tool.args, ["clean", "--target-dir", sandbox.text(r"Users\me\code\rusty\target").as_str()]);
        assert_eq!(tool.dir.as_deref(), Some(sandbox.text(r"Users\me\code\rusty").as_str()));
        assert!(cargo.reason.contains("120 days"), "{}", cargo.reason);
        let web = build.iter().find(|r| r.title == "Build output in rusty").unwrap();
        assert_eq!(web.action, Action::Recycle);
        assert!(build.iter().all(|r| r.tier == Tier::Safe));

        // Without Cargo installed, `target` goes to the Recycle Bin with the rest.
        context.tools = Tools::default();
        let recs = scan(&sandbox.index(), &context);
        let rusty = recs.iter().find(|r| r.title == "Build output in rusty").unwrap();
        assert_eq!(rusty.items.len(), 3);
        assert!(!recs.iter().any(|r| matches!(r.action, Action::Tool(_))));
        // Sources never appear, whatever rule.
        let all = item_paths(&sandbox, &recs);
        assert!(!all.iter().any(|p| p.ends_with(".rs") || p.ends_with(".cs") || p.ends_with("/build")), "{all:?}");
    }

    /// The project's own configuration (or the environment, or the owner's) can point Cargo at
    /// another build folder, one shared by every project: run as offered, `cargo clean` still
    /// removes the `target` folder shown, and nothing else.
    #[test]
    fn cargo_clean_removes_the_target_folder_shown_whatever_cargo_is_configured_with() {
        let sandbox = Sandbox::new("build-cargo-clean");
        let project = r"Users\me\code\stale";
        sandbox.write_bytes(&format!(r"{project}\Cargo.toml"), b"[package]\nname = \"stale\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        sandbox.write_bytes(&format!(r"{project}\src\main.rs"), b"fn main() {}\n");
        sandbox.write(&format!(r"{project}\target\debug\stale.exe"), 9000, 1);
        let shared = sandbox.write(r"shared-target\debug\other-project.exe", 9000, 2);
        let config = format!("[build]\ntarget-dir = '{}'\n", sandbox.text("shared-target"));
        sandbox.write_bytes(&format!(r"{project}\.cargo\config.toml"), config.as_bytes());
        sandbox.age_tree("Users", 120);
        let mut context = sandbox.context();
        context.projects = vec![Project { root: sandbox.text(project), dismissed: false }];
        context.tools = Tools::with(&["cargo"]);
        let recs = scan(&sandbox.index(), &context);
        let cargo = recs.iter().find(|r| r.id.starts_with("cargo:")).unwrap();
        let Action::Tool(tool) = &cargo.action else { panic!("{cargo:?}") };

        let program = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ran = std::process::Command::new(program)
            .args(&tool.args)
            .current_dir(tool.dir.as_deref().unwrap())
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
        assert!(!sandbox.path(&format!(r"{project}\target")).exists(), "the folder shown is removed");
        assert!(shared.exists(), "the build folder Cargo is configured with is left alone");
    }

    #[test]
    fn projects_a_tool_keeps_in_a_hidden_folder_are_left_alone() {
        let sandbox = Sandbox::new("build-tool");
        let root = r"Users\me\.codex\plugins\cache\vendor\plugin\0.1.0";
        for file in ["pyproject.toml", r"src\main.py", r".venv\pyvenv.cfg", r".venv\Lib\site.py", r"src\__pycache__\main.pyc"] {
            sandbox.write(&format!(r"{root}\{file}"), 9000, 1);
        }
        sandbox.age_tree("Users", 200);
        let mut context = sandbox.context();
        context.projects = vec![Project { root: sandbox.text(root), dismissed: false }];
        let recs = scan(&sandbox.index(), &context);
        assert!(recs.iter().all(|r| r.rule != Rule::Build), "{:?}", item_paths(&sandbox, &recs));
    }

    #[test]
    fn unity_projects_are_found_without_git_and_dismissed_projects_are_left_alone() {
        let sandbox = Sandbox::new("build-unity");
        for file in [r"tower\Assets\Main.cs", r"tower\ProjectSettings\ProjectVersion.txt", r"tower\Library\cache.bin", r"tower\Temp\x.tmp"] {
            put(&sandbox, file);
        }
        for file in [r"old\package.json", r"old\index.js", r"old\node_modules\dep\index.js"] {
            put(&sandbox, file);
        }
        sandbox.age_tree(CODE, 200);
        let mut context = sandbox.context();
        context.projects = vec![Project { dismissed: true, ..project(&sandbox, "old") }];
        let recs = scan(&sandbox.index(), &context);
        let paths = item_paths(&sandbox, &recs);
        assert_eq!(paths, ["users/me/code/tower/library", "users/me/code/tower/temp"]);
    }
}
