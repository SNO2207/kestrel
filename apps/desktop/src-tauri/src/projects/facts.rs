//! What the index alone says about a project: its size on disk, how much of that is build
//! output that could be reclaimed, which languages its source is written in, when anything in
//! it last changed, and a fingerprint that changes whenever any file that git would see does.
//!
//! One walk of the project's subtree answers everything. It does not descend into build output
//! or dependency folders, whose totals the index already keeps, so a project with a huge
//! `node_modules` or `target` costs no more to look at than its own sources.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use kestrel_index::{EntryId, Index};
use serde::Serialize;

use super::discover::Markers;

/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Recently changed files shown in the detail panel.
pub const RECENT_FILES: usize = 12;
/// A language needs at least this share of the source bytes to count as part of the stack.
const STACK_SHARE: f64 = 0.05;
const MAX_STACK_LANGUAGES: usize = 4;

pub fn unix_ms(filetime: i64) -> i64 {
    if filetime <= FILETIME_UNIX_EPOCH { 0 } else { (filetime - FILETIME_UNIX_EPOCH) / 10_000 }
}

/// A build output folder inside a project, which could be deleted and rebuilt.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Reclaimable {
    pub path: String,
    /// Bytes on disk.
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LanguageShare {
    pub name: &'static str,
    pub bytes: u64,
    /// Share of all source bytes, from 0 to 1.
    pub share: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub mtime: i64,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TreeFacts {
    /// Bytes on disk, everything included.
    pub size_on_disk: u64,
    pub reclaimable: Vec<Reclaimable>,
    pub languages: Vec<LanguageShare>,
    /// Newest modification of the project's own files (not build output, dependencies or
    /// git's internals), in milliseconds since the Unix epoch; 0 if there are none.
    pub last_activity: i64,
    /// Changes whenever a file git would notice is added, removed, renamed or written,
    /// including git's own refs and index.
    pub fingerprint: u64,
}

impl TreeFacts {
    pub fn reclaimable_bytes(&self) -> u64 {
        self.reclaimable.iter().map(|r| r.bytes).sum()
    }
}

/// The language of a source file, by its extension.
pub fn language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyi" | "pyx" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "ipp" => "C++",
        "cs" => "C#",
        "fs" | "fsx" => "F#",
        "vb" => "Visual Basic",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "groovy" => "Groovy",
        "php" => "PHP",
        "rb" => "Ruby",
        "swift" => "Swift",
        "m" | "mm" => "Objective-C",
        "dart" => "Dart",
        "lua" => "Lua",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "zig" => "Zig",
        "hs" => "Haskell",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "clj" | "cljs" => "Clojure",
        "r" => "R",
        "jl" => "Julia",
        "sql" => "SQL",
        "hlsl" | "glsl" | "shader" | "usf" | "ush" | "wgsl" => "Shaders",
        "gd" => "GDScript",
        "nim" => "Nim",
        "ml" | "mli" => "OCaml",
        "pl" | "pm" => "Perl",
        "asm" => "Assembly",
        "cu" => "CUDA",
        _ => return None,
    })
}

/// The stack shown for a project: its main languages by share, then what its markers say
/// that the languages do not.
pub fn stack(languages: &[LanguageShare], markers: Markers) -> Vec<String> {
    let mut stack: Vec<String> = languages
        .iter()
        .filter(|l| l.share >= STACK_SHARE)
        .take(MAX_STACK_LANGUAGES)
        .map(|l| l.name.to_owned())
        .collect();
    for label in markers.iter().filter_map(|marker| marker.label()) {
        if !stack.iter().any(|s| s == label) {
            stack.push(label.to_owned());
        }
    }
    stack
}

/// How a child folder of a project is treated by the walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// The project's own files: walked and counted.
    Own,
    /// Build output: its size is reclaimable and it is not walked.
    Reclaimable,
    /// Dependencies kept with the sources, or tool state: walked for the fingerprint, since git
    /// may track them, but not counted as the project's activity or languages.
    Foreign,
    /// Git's own folder: only the parts that change when refs or the staging area do.
    Git,
    /// Never walked: git's object store, and nested repositories' internals.
    Skip,
}

/// Decides what a folder named `name` (folded) is, given its siblings' folded names.
fn classify(name: &str, siblings: &[&str], in_git: bool) -> Kind {
    if in_git {
        return match name {
            "objects" | "lfs" | "modules" | "worktrees" => Kind::Skip,
            _ => Kind::Git,
        };
    }
    let has = |sibling: &str| siblings.contains(&sibling);
    let has_ext = |ext: &str| siblings.iter().any(|s| s.ends_with(ext));
    match name {
        ".git" => Kind::Git,
        "node_modules" | ".venv" | "__pycache__" | "dist" | "build" | ".next" | "obj" => Kind::Reclaimable,
        "bin" if has("obj") || has_ext(".csproj") || has_ext(".fsproj") || has_ext(".vbproj") => Kind::Reclaimable,
        "target" if has("cargo.toml") || has("pom.xml") => Kind::Reclaimable,
        "intermediate" | "deriveddatacache" | "saved" if has_ext(".uproject") => Kind::Reclaimable,
        "library" if has("assets") && has("projectsettings") => Kind::Reclaimable,
        "vendor" | "bower_components" | ".vs" | ".idea" | ".gradle" | "venv" | ".tox" | ".mypy_cache" | ".pytest_cache" => {
            Kind::Foreign
        }
        _ => Kind::Own,
    }
}

/// Mixes one entry into the fingerprint. The sum of mixed values does not depend on the order
/// entries are visited in, and an entry's id changes when it is renamed.
fn mix(id: EntryId, mtime: i64, size: u64) -> u64 {
    let mut z = (id as u64) ^ (mtime as u64).rotate_left(21) ^ size.rotate_left(42);
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Walks the project rooted at `root` once and gathers everything above.
pub fn scan(index: &Index, root: EntryId) -> TreeFacts {
    let mut facts = TreeFacts { size_on_disk: index.alloc(root), ..TreeFacts::default() };
    let mut by_language: HashMap<&'static str, u64> = HashMap::new();
    let mut fingerprint: u64 = 0;
    let mut count: u64 = 0;
    // Folders to walk, with their kind.
    let mut stack: Vec<(EntryId, Kind)> = vec![(root, Kind::Own)];
    let mut names: Vec<(EntryId, Vec<u8>, bool)> = Vec::new();

    while let Some((dir, kind)) = stack.pop() {
        names.clear();
        names.extend(index.children(dir).map(|child| (child, index.lower_bytes(child).to_vec(), index.is_dir(child))));
        let siblings: Vec<&str> = names.iter().filter_map(|(_, name, _)| std::str::from_utf8(name).ok()).collect();
        for (child, name, is_dir) in &names {
            let (child, is_dir) = (*child, *is_dir);
            if is_dir {
                let name = std::str::from_utf8(name).unwrap_or("");
                let child_kind = match kind {
                    Kind::Own => classify(name, &siblings, false),
                    Kind::Git => classify(name, &siblings, true),
                    // A nested repository's internals are its own business.
                    Kind::Foreign if name == ".git" => Kind::Skip,
                    other => other,
                };
                match child_kind {
                    Kind::Reclaimable => {
                        facts.reclaimable.push(Reclaimable { path: index.path(child), bytes: index.alloc(child) })
                    }
                    Kind::Skip => {}
                    // Sub-repositories and submodules keep their internals in their own `.git`.
                    Kind::Git if kind == Kind::Own && dir != root => {}
                    _ => stack.push((child, child_kind)),
                }
                continue;
            }
            let mtime = index.mtime(child);
            let size = index.size(child);
            fingerprint = fingerprint.wrapping_add(mix(child, mtime, size));
            count += 1;
            if kind != Kind::Own {
                continue;
            }
            if let Some(language) = language(index.ext_name(index.ext(child))) {
                *by_language.entry(language).or_default() += size;
            }
            facts.last_activity = facts.last_activity.max(unix_ms(mtime));
        }
    }

    let total: u64 = by_language.values().sum();
    let mut languages: Vec<LanguageShare> = by_language
        .into_iter()
        .map(|(name, bytes)| LanguageShare { name, bytes, share: if total > 0 { bytes as f64 / total as f64 } else { 0.0 } })
        .collect();
    languages.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(b.name)));
    facts.languages = languages;
    // An empty build folder (never built, or cleaned) reclaims nothing.
    facts.reclaimable.retain(|r| r.bytes > 0);
    facts.reclaimable.sort_by_key(|r| Reverse(r.bytes));
    facts.fingerprint = fingerprint ^ count.rotate_left(32);
    facts
}

/// What the detail panel shows of the project's own files.
#[derive(Debug, Default)]
pub struct OwnActivity {
    /// The newest files, newest first.
    pub recent: Vec<RecentFile>,
    /// Files last written on each day since the given time, by day (counted from the Unix epoch
    /// in the viewer's time zone).
    pub days: HashMap<i64, u32>,
}

/// Walks the project's own files: not build output, dependencies or git's internals, and not
/// what git ignores. `ignored` holds lower-cased paths relative to the root with `/`
/// separators, folders ending in `/`, as `git::ignored_paths` gives them. Files written since
/// `since_ms` are counted per day in the time zone `offset_minutes` east of UTC.
pub fn own_activity(index: &Index, root: EntryId, ignored: &HashSet<String>, since_ms: i64, offset_minutes: i64) -> OwnActivity {
    let mut activity = OwnActivity::default();
    let mut recent: BinaryHeap<Reverse<(i64, EntryId)>> = BinaryHeap::new();
    let mut stack: Vec<(EntryId, String)> = vec![(root, String::new())];
    let mut names: Vec<(EntryId, Vec<u8>, bool)> = Vec::new();
    while let Some((dir, prefix)) = stack.pop() {
        names.clear();
        names.extend(index.children(dir).map(|child| (child, index.lower_bytes(child).to_vec(), index.is_dir(child))));
        let siblings: Vec<&str> = names.iter().filter_map(|(_, name, _)| std::str::from_utf8(name).ok()).collect();
        for (child, name, is_dir) in &names {
            let name = std::str::from_utf8(name).unwrap_or("");
            let rel = format!("{prefix}{name}");
            if *is_dir {
                let folder = format!("{rel}/");
                if classify(name, &siblings, false) == Kind::Own && !ignored.contains(&folder) {
                    stack.push((*child, folder));
                }
                continue;
            }
            if ignored.contains(&rel) {
                continue;
            }
            let mtime = index.mtime(*child);
            let ms = unix_ms(mtime);
            if ms >= since_ms {
                *activity.days.entry((ms + offset_minutes * 60_000).div_euclid(86_400_000)).or_default() += 1;
            }
            recent.push(Reverse((mtime, *child)));
            if recent.len() > RECENT_FILES {
                recent.pop();
            }
        }
    }
    let mut newest: Vec<(i64, EntryId)> = recent.into_iter().map(|Reverse(item)| item).collect();
    newest.sort_by(|a, b| b.cmp(a));
    activity.recent = newest
        .into_iter()
        .map(|(mtime, id)| RecentFile { path: index.path(id), name: index.name(id).to_owned(), mtime: unix_ms(mtime), size: index.size(id) })
        .collect();
    activity
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta, attr};

    /// 2026-01-01 as a FILETIME, plus `days`.
    fn day(days: i64) -> i64 {
        FILETIME_UNIX_EPOCH + (1_767_225_600_000 + days * 86_400_000) * 10_000
    }

    fn build(entries: &[(&str, u64, i64)]) -> (Index, EntryId) {
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let mut ids: HashMap<String, u32> = HashMap::new();
        let proj = builder.push(root, "proj", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        ids.insert(String::new(), proj);
        for &(path, size, mtime) in entries {
            let is_dir = path.ends_with('\\');
            let parts: Vec<&str> = path.trim_end_matches('\\').split('\\').collect();
            let mut parent = proj;
            let mut sofar = String::new();
            for (i, part) in parts.iter().enumerate() {
                sofar.push('\\');
                sofar.push_str(part);
                let leaf = i == parts.len() - 1 && !is_dir;
                let meta = if leaf {
                    Meta { size, alloc: size.div_ceil(4096) * 4096, mtime, ..Meta::default() }
                } else {
                    Meta { attrs: attr::DIRECTORY, ..Meta::default() }
                };
                parent = *ids.entry(sofar.to_lowercase()).or_insert_with(|| builder.push(parent, part, meta));
            }
        }
        let index = builder.finish();
        let proj = index.lookup(r"C:\proj").unwrap();
        (index, proj)
    }

    #[test]
    fn walks_sources_and_totals_build_output() {
        let (index, proj) = build(&[
            ("Cargo.toml", 300, day(0)),
            (r"src\main.rs", 6000, day(5)),
            (r"web\app.ts", 3000, day(9)),
            (r"web\style.css", 1000, day(2)),
            (r"web\node_modules\dep\index.js", 50_000, day(30)),
            (r"target\debug\app.exe", 900_000, day(31)),
            (r".git\HEAD", 20, day(40)),
            (r".git\objects\ab\cdef", 5000, day(41)),
            (r"vendor\lib\lib.rs", 70_000, day(42)),
            // Not a Cargo or Maven folder, so `target` here is the project's own.
            (r"docs\target\notes.md", 10, day(3)),
        ]);
        let facts = scan(&index, proj);
        let reclaimable: Vec<(&str, u64)> =
            facts.reclaimable.iter().map(|r| (r.path.rsplit('\\').next().unwrap(), r.bytes)).collect();
        assert_eq!(reclaimable, [("target", 901_120), ("node_modules", 53_248)]);
        assert_eq!(facts.reclaimable_bytes(), 901_120 + 53_248);
        let names: Vec<_> = facts.languages.iter().map(|l| (l.name, l.bytes)).collect();
        assert_eq!(names, [("Rust", 6000), ("TypeScript", 3000), ("CSS", 1000)]);
        assert!((facts.languages[0].share - 0.6).abs() < 1e-9);
        // Neither build output, git's files nor vendored code count as the project's activity.
        assert_eq!(facts.last_activity, unix_ms(day(9)));
        assert_eq!(facts.size_on_disk, index.alloc(proj));

        let activity = own_activity(&index, proj, &HashSet::new(), 0, 0);
        let recent: Vec<String> = activity.recent.into_iter().map(|f| f.name).collect();
        assert_eq!(recent, ["app.ts", "main.rs", "notes.md", "style.css", "Cargo.toml"]);
        // What git ignores is left out too: a whole folder, or one file.
        let ignored: HashSet<String> = ["docs/target/".to_owned(), "web/style.css".to_owned()].into();
        let recent: Vec<String> = own_activity(&index, proj, &ignored, 0, 0).recent.into_iter().map(|f| f.name).collect();
        assert_eq!(recent, ["app.ts", "main.rs", "Cargo.toml"]);
    }

    #[test]
    fn empty_build_folders_reclaim_nothing() {
        let (index, proj) = build(&[("package.json", 10, day(0)), (r"dist\", 0, 0)]);
        assert!(scan(&index, proj).reclaimable.is_empty());
    }

    #[test]
    fn the_fingerprint_follows_what_git_would_see() {
        let (mut index, proj) = build(&[
            (r"src\lib.rs", 100, day(1)),
            (r".git\HEAD", 20, day(1)),
            (r".git\refs\heads\main", 41, day(1)),
            (r"node_modules\x.js", 10, day(1)),
        ]);
        let first = scan(&index, proj).fingerprint;
        assert_eq!(scan(&index, proj).fingerprint, first, "stable while nothing changes");

        // Build output changing does not matter.
        let dep = index.lookup(r"C:\proj\node_modules\x.js").unwrap();
        index.update_meta(dep, Meta { size: 99, alloc: 4096, mtime: day(3), ..Meta::default() });
        assert_eq!(scan(&index, proj).fingerprint, first);

        // A commit moves a ref.
        let head = index.lookup(r"C:\proj\.git\refs\heads\main").unwrap();
        index.update_meta(head, Meta { size: 41, alloc: 4096, mtime: day(2), ..Meta::default() });
        let after_commit = scan(&index, proj).fingerprint;
        assert_ne!(after_commit, first);

        // A rename changes nothing but the name, and still counts.
        let src = index.lookup(r"C:\proj\src").unwrap();
        let lib = index.lookup(r"C:\proj\src\lib.rs").unwrap();
        index.rename(lib, src, "main.rs");
        assert_ne!(scan(&index, proj).fingerprint, after_commit);
    }

    #[test]
    fn stacks_put_main_languages_first_then_markers() {
        let languages = vec![
            LanguageShare { name: "Rust", bytes: 70, share: 0.7 },
            LanguageShare { name: "TypeScript", bytes: 27, share: 0.27 },
            LanguageShare { name: "Shell", bytes: 3, share: 0.03 },
        ];
        let found = super::super::discover::discover(&{
            let mut b = IndexBuilder::new();
            let root = b.add_root("C:");
            let p = b.push(root, "p", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
            b.push(p, "Cargo.toml", Meta::default());
            b.push(p, "package.json", Meta::default());
            b.finish()
        }, &[]);
        assert_eq!(stack(&languages, found[0].markers), ["Rust", "TypeScript", "Node.js"]);
    }

    #[test]
    fn file_activity_is_counted_per_local_day() {
        let (index, proj) = build(&[
            (r"a.txt", 1, day(10)),
            (r"b.txt", 1, day(10)),
            (r"c.txt", 1, day(12)),
            (r"old.txt", 1, day(-400)),
            (r"node_modules\x.js", 1, day(10)),
        ]);
        let since = unix_ms(day(0));
        let days = own_activity(&index, proj, &HashSet::new(), since, 0).days;
        let base = unix_ms(day(0)) / 86_400_000;
        assert_eq!(days.get(&(base + 10)), Some(&2));
        assert_eq!(days.get(&(base + 12)), Some(&1));
        assert_eq!(days.len(), 2);
        // Twelve hours east of UTC, a file written at noon UTC falls on the next day.
        let noon = (unix_ms(day(12)) + 12 * 3_600_000) * 10_000 + FILETIME_UNIX_EPOCH;
        let (index, proj) = build(&[(r"late.txt", 1, noon)]);
        let days = own_activity(&index, proj, &HashSet::new(), since, 12 * 60).days;
        assert_eq!(days.keys().copied().collect::<Vec<_>>(), [base + 13]);
    }
}
