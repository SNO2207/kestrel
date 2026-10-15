//! Commands a project defines for itself: `package.json` scripts, Cargo's build, test and run,
//! and the recipes of a justfile or the targets of a Makefile in the project's root.
//!
//! Only the root folder is read, and only small files: these are what the detail panel offers
//! to run, each in a new terminal in the project folder, and running one is always the user's
//! own click.

use std::fmt;
use std::path::Path;

use serde::Serialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};

/// Manifests larger than this are not read.
const MAX_MANIFEST: u64 = 1 << 20;
/// More than this many entries from one source are not shown.
const MAX_PER_SOURCE: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Script {
    /// Where it comes from: `npm`, `pnpm`, `yarn`, `bun`, `cargo`, `just` or `make`.
    pub source: &'static str,
    /// The script's or target's own name.
    pub name: String,
    /// The command line run in the project folder.
    pub command: String,
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_owned())
}

/// The `scripts` of a `package.json`, in the order the file lists them.
fn package_scripts(json: &str) -> Vec<(String, String)> {
    /// Reads a package.json, keeping only its scripts, in order. (serde_json's own map sorts
    /// its keys.)
    struct Package(Vec<(String, String)>);
    struct Scripts(Vec<(String, String)>);

    impl<'de> serde::Deserialize<'de> for Scripts {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Scripts;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a map of scripts")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scripts, A::Error> {
                    let mut scripts = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        match map.next_value::<serde_json::Value>()? {
                            serde_json::Value::String(command) => scripts.push((key, command)),
                            _ => continue,
                        }
                    }
                    Ok(Scripts(scripts))
                }
            }
            deserializer.deserialize_map(V)
        }
    }

    impl<'de> serde::Deserialize<'de> for Package {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Package;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a package.json object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Package, A::Error> {
                    let mut scripts = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if key == "scripts" {
                            scripts = map.next_value::<Option<Scripts>>()?.map(|s| s.0).unwrap_or_default();
                        } else {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                    Ok(Package(scripts))
                }
            }
            deserializer.deserialize_map(V)
        }
    }

    serde_json::from_str::<Package>(json).map(|p| p.0).unwrap_or_default()
}

/// Which package manager a Node project uses, from its lock file.
fn package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        "bun"
    } else {
        "npm"
    }
}

/// A name that is safe to put on a command line unquoted.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '/' | '@' | '+'))
}

/// The recipes of a justfile. Private recipes (a leading `_` or a `[private]` attribute) are
/// left out.
fn just_recipes(text: &str) -> Vec<String> {
    let mut recipes = Vec::new();
    let mut private = false;
    for line in text.lines() {
        if line.starts_with([' ', '\t']) || line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            private |= trimmed.contains("private");
            continue;
        }
        let was_private = std::mem::take(&mut private);
        let first = trimmed.split_whitespace().next().unwrap_or("");
        if matches!(first, "set" | "alias" | "export" | "import" | "mod") || trimmed.contains(":=") {
            continue;
        }
        let Some((head, _)) = trimmed.split_once(':') else { continue };
        let name = head.split_whitespace().next().unwrap_or("").trim_start_matches('@');
        if name.starts_with('_') || was_private || !plain_name(name) || name.contains(':') {
            continue;
        }
        if !recipes.iter().any(|r| r == name) {
            recipes.push(name.to_owned());
        }
    }
    recipes
}

/// The explicit targets of a Makefile: not special targets (`.PHONY`), pattern rules or
/// variable assignments.
fn make_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#']) {
            continue;
        }
        let Some((head, rest)) = line.split_once(':') else { continue };
        // `:=`, `::=` and `?=`-style assignments, and `name = value` lines with colons later.
        if rest.starts_with('=') || rest.starts_with(":=") || head.contains('=') || head.contains('$') {
            continue;
        }
        for name in head.split_whitespace() {
            if name.starts_with('.') || name.contains('%') || !plain_name(name) || name.contains(':') {
                continue;
            }
            if !targets.iter().any(|t| t == name) {
                targets.push(name.to_owned());
            }
        }
    }
    targets
}

/// Every script the project at `root` defines.
pub fn detect(root: &Path) -> Vec<Script> {
    let mut scripts = Vec::new();
    if let Some(json) = read_small(&root.join("package.json")) {
        let manager = package_manager(root);
        for (name, _) in package_scripts(&json).into_iter().take(MAX_PER_SOURCE) {
            if !plain_name(&name) {
                continue;
            }
            let command = match manager {
                "yarn" => format!("yarn {name}"),
                other => format!("{other} run {name}"),
            };
            scripts.push(Script { source: manager, name, command });
        }
    }
    if let Some(toml) = read_small(&root.join("Cargo.toml")) {
        let runnable = root.join("src").join("main.rs").is_file() || toml.contains("[[bin]]");
        for verb in ["build", "test"].into_iter().chain(runnable.then_some("run")) {
            scripts.push(Script { source: "cargo", name: verb.to_owned(), command: format!("cargo {verb}") });
        }
    }
    let justfile = ["justfile", "Justfile", ".justfile"].iter().map(|name| root.join(name)).find(|p| p.is_file());
    if let Some(text) = justfile.and_then(|p| read_small(&p)) {
        for name in just_recipes(&text).into_iter().take(MAX_PER_SOURCE) {
            scripts.push(Script { source: "just", command: format!("just {name}"), name });
        }
    }
    let makefile = ["GNUmakefile", "Makefile", "makefile"].iter().map(|name| root.join(name)).find(|p| p.is_file());
    if let Some(text) = makefile.and_then(|p| read_small(&p)) {
        for name in make_targets(&text).into_iter().take(MAX_PER_SOURCE) {
            scripts.push(Script { source: "make", command: format!("make {name}"), name });
        }
    }
    scripts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn package_scripts_keep_their_order_and_skip_odd_names() {
        let json = r#"{ "name": "x", "scripts": { "dev": "vite", "build": "tsc && vite build", "lint:fix": "eslint --fix",
            "evil & calc": "x", "nested": { "no": 1 } }, "dependencies": { "a": "1" } }"#;
        let names: Vec<String> = package_scripts(json).into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["dev", "build", "lint:fix", "evil & calc"]);
        assert!(!plain_name("evil & calc"));
        assert!(package_scripts("not json").is_empty());
        assert!(package_scripts(r#"{"scripts": null}"#).is_empty());
    }

    #[test]
    fn justfile_recipes() {
        let text = "set shell := [\"pwsh\", \"-c\"]\nversion := \"1\"\n\n# Build it\nbuild target='debug':\n    cargo build\n\n@test: build\n    cargo test\n_helper:\n    echo\n[private]\nhidden:\n    echo\nalias b := build\nrelease: (build \"release\")\n";
        assert_eq!(just_recipes(text), ["build", "test", "release"]);
    }

    #[test]
    fn makefile_targets() {
        let text = ".PHONY: all clean\nCC := gcc\nFLAGS = -O2 -DX=a:b\nall: app\napp: main.o util.o\n\t$(CC) -o app $^\n%.o: %.c\n\t$(CC) -c $<\nclean:\n\trm -f *.o\ninstall uninstall: app\n";
        assert_eq!(make_targets(text), ["all", "app", "clean", "install", "uninstall"]);
    }

    #[test]
    fn detects_every_source_in_a_project_folder() {
        let root = std::env::temp_dir().join(format!("kestrel-scripts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("package.json"), r#"{"scripts":{"start":"node .","test":"node --test"}}"#).unwrap();
        fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("justfile"), "fmt:\n    cargo fmt\n").unwrap();
        fs::write(root.join("Makefile"), "docs:\n\techo docs\n").unwrap();
        let commands: Vec<String> = detect(&root).into_iter().map(|s| s.command).collect();
        assert_eq!(
            commands,
            ["pnpm run start", "pnpm run test", "cargo build", "cargo test", "cargo run", "just fmt", "make docs"]
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
