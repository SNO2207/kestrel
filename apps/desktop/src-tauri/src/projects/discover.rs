//! Finding projects in the file index, which already holds every file on the drive.
//!
//! A folder is a candidate when it holds a marker: `.git` (a folder, or the file a submodule
//! or worktree has), a build manifest such as `Cargo.toml` or `package.json`, or a solution or
//! project file such as `*.sln`. A project's root is the outermost folder with `.git` above a
//! marker, so a submodule or a nested clone counts as part of the repository around it;
//! without one, it is the topmost marker folder, so a workspace's members belong to the
//! workspace. Dependency, cache and system folders (`node_modules`, `AppData`, `Program
//! Files`…) are never looked in, nor are drive roots and profile folders, where a stray
//! `package.json` would otherwise turn a whole home folder into one project.

use std::collections::{HashMap, HashSet};

use kestrel_index::{EntryId, Index, NO_ENTRY, attr};
use rayon::prelude::*;

/// Something that marks a folder as (part of) a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Marker {
    Git,
    Cargo,
    Node,
    Python,
    Go,
    CMake,
    DotNet,
    Unreal,
    Gradle,
    Maven,
    Composer,
    Ruby,
}

impl Marker {
    const ALL: [Marker; 12] = [
        Marker::Git,
        Marker::Cargo,
        Marker::Node,
        Marker::Python,
        Marker::Go,
        Marker::CMake,
        Marker::DotNet,
        Marker::Unreal,
        Marker::Gradle,
        Marker::Maven,
        Marker::Composer,
        Marker::Ruby,
    ];

    fn bit(self) -> u32 {
        1 << self as u32
    }

    /// The technology the marker stands for, as the hub's stack shows it. Git is not a stack.
    pub fn label(self) -> Option<&'static str> {
        Some(match self {
            Marker::Git => return None,
            Marker::Cargo => "Rust",
            Marker::Node => "Node.js",
            Marker::Python => "Python",
            Marker::Go => "Go",
            Marker::CMake => "CMake",
            Marker::DotNet => ".NET",
            Marker::Unreal => "Unreal Engine",
            Marker::Gradle => "Gradle",
            Marker::Maven => "Maven",
            Marker::Composer => "PHP",
            Marker::Ruby => "Ruby",
        })
    }
}

/// A set of markers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Markers(u32);

impl Markers {
    pub fn has(self, marker: Marker) -> bool {
        self.0 & marker.bit() != 0
    }

    fn add(&mut self, marker: Marker) {
        self.0 |= marker.bit();
    }

    fn union(self, other: Markers) -> Markers {
        Markers(self.0 | other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Marker> {
        Marker::ALL.into_iter().filter(move |marker| self.has(*marker))
    }
}

/// Marker file names, folded as the index stores them. `.git` may be a folder or a file; the
/// others must be files.
const NAMED: &[(&str, Marker)] = &[
    (".git", Marker::Git),
    ("cargo.toml", Marker::Cargo),
    ("package.json", Marker::Node),
    ("pyproject.toml", Marker::Python),
    ("go.mod", Marker::Go),
    ("cmakelists.txt", Marker::CMake),
    ("build.gradle", Marker::Gradle),
    ("build.gradle.kts", Marker::Gradle),
    ("pom.xml", Marker::Maven),
    ("composer.json", Marker::Composer),
    ("gemfile", Marker::Ruby),
];

/// Marker file extensions.
const BY_EXTENSION: &[(&str, Marker)] = &[("sln", Marker::DotNet), ("csproj", Marker::DotNet), ("uproject", Marker::Unreal)];

/// Folders whose contents are dependencies, caches, tools or the system, never the user's own
/// projects. Compared with folded names.
const EXCLUDED: &[&str] = &[
    // Dependencies and build output.
    "node_modules",
    "bower_components",
    "target",
    "vendor",
    ".venv",
    "site-packages",
    "__pycache__",
    ".tox",
    // Toolchains, package stores and caches.
    ".cargo",
    ".rustup",
    ".npm",
    "npm-cache",
    ".pnpm-store",
    ".pnpm",
    ".yarn",
    ".nuget",
    ".gradle",
    ".m2",
    ".cache",
    ".dotnet",
    ".vscode",
    ".vscode-insiders",
    ".cursor",
    // Claude Code's plugin caches and marketplaces, and its worktrees (which belong to the
    // repository they are in anyway).
    ".claude",
    "scoop",
    ".git",
    // System locations.
    "appdata",
    "program files",
    "program files (x86)",
    "programdata",
    "windows",
    "$recycle.bin",
    "system volume information",
    "$windows.~bt",
    "$windows.~ws",
    "$winreagent",
    "windows.old",
];

/// A project found in the index.
#[derive(Clone, Debug)]
pub struct Found {
    pub root: EntryId,
    pub path: String,
    /// Markers anywhere in the project, nested members included.
    pub markers: Markers,
}

fn name_is(index: &Index, id: EntryId, name: &str) -> bool {
    index.lower_bytes(id) == name.as_bytes()
}

/// Whether `id` is a folder no project should be looked for in: a drive root, the `Users`
/// folder or a profile folder directly inside it.
fn is_too_broad(index: &Index, id: EntryId) -> bool {
    let parent = index.parent(id);
    if parent == NO_ENTRY {
        return true;
    }
    let is_users = |id: EntryId| name_is(index, id, "users") && index.parent(id) != NO_ENTRY && index.parent(index.parent(id)) == NO_ENTRY;
    is_users(id) || is_users(parent)
}

/// Every entry that is a project marker, with its marker.
fn marker_hits(index: &Index) -> Vec<(EntryId, Marker)> {
    let offsets = index.name_offsets();
    let arena = index.lower_arena();
    let by_extension: Vec<(u16, Marker)> =
        BY_EXTENSION.iter().filter_map(|&(ext, marker)| index.ext_id(ext).map(|id| (id, marker))).collect();
    // Names are only compared when their length matches one of the markers'.
    let lengths: u64 = NAMED.iter().fold(0, |mask, (name, _)| mask | 1 << name.len());
    (0..index.len())
        .into_par_iter()
        .with_min_len(1 << 14)
        .filter_map(|id| {
            let attrs = index.attrs(id);
            if attrs & attr::DELETED != 0 {
                return None;
            }
            let dir = attrs & attr::DIRECTORY != 0;
            if !dir {
                let ext = index.ext(id);
                if ext != 0
                    && let Some(&(_, marker)) = by_extension.iter().find(|(id, _)| *id == ext)
                {
                    return Some((id, marker));
                }
            }
            let start = offsets[id as usize] as usize;
            let len = offsets[id as usize + 1] as usize - 1 - start;
            if len >= 64 || lengths & (1 << len) == 0 {
                return None;
            }
            let name = &arena[start..start + len];
            NAMED
                .iter()
                .find(|(marker_name, _)| marker_name.as_bytes() == name)
                .filter(|(_, marker)| *marker == Marker::Git || !dir)
                .map(|&(_, marker)| (id, marker))
        })
        .collect()
}

/// Answers "is this folder in an excluded location?", remembering every folder it walks past.
struct Exclusions<'a> {
    index: &'a Index,
    allowed: &'a HashSet<EntryId>,
    known: HashMap<EntryId, bool>,
}

impl Exclusions<'_> {
    /// A folder is excluded when it, or a folder above it, has an excluded name, unless an
    /// allowed folder comes first on the way up. Allowed folders only exist for tests, whose
    /// fixtures live in the temp folder under `AppData`.
    fn excluded(&mut self, id: EntryId) -> bool {
        let mut chain = Vec::new();
        let mut cur = id;
        let answer = loop {
            if let Some(&known) = self.known.get(&cur) {
                break known;
            }
            if self.allowed.contains(&cur) {
                break false;
            }
            let name = self.index.lower_bytes(cur);
            if EXCLUDED.iter().any(|excluded| excluded.as_bytes() == name) {
                break true;
            }
            chain.push(cur);
            cur = self.index.parent(cur);
            if cur == NO_ENTRY {
                break false;
            }
        };
        for id in chain {
            self.known.insert(id, answer);
        }
        answer
    }
}

/// Finds every project in the index. `allowed` folders lift the location rules for anything
/// beneath them (see [`Exclusions::excluded`]); pass none outside tests.
pub fn discover(index: &Index, allowed: &[EntryId]) -> Vec<Found> {
    let allowed: HashSet<EntryId> = allowed.iter().copied().collect();
    let mut exclusions = Exclusions { index, allowed: &allowed, known: HashMap::new() };

    // Marker folders and what each holds.
    let mut folders: HashMap<EntryId, Markers> = HashMap::new();
    for (entry, marker) in marker_hits(index) {
        let folder = index.parent(entry);
        if folder == NO_ENTRY {
            continue;
        }
        folders.entry(folder).or_default().add(marker);
    }
    folders.retain(|&folder, _| !is_too_broad(index, folder) && !exclusions.excluded(folder));

    // Each marker folder belongs to the outermost repository above it, or failing that to the
    // topmost marker folder above it.
    let mut roots: HashMap<EntryId, Markers> = HashMap::new();
    for (&folder, &markers) in &folders {
        let mut outermost_git = None;
        let mut topmost_marker = folder;
        let mut cur = folder;
        while cur != NO_ENTRY {
            if let Some(found) = folders.get(&cur) {
                topmost_marker = cur;
                if found.has(Marker::Git) {
                    outermost_git = Some(cur);
                }
            }
            cur = index.parent(cur);
        }
        let root = outermost_git.unwrap_or(topmost_marker);
        let entry = roots.entry(root).or_default();
        *entry = entry.union(markers);
    }

    let mut found: Vec<Found> =
        roots.into_iter().map(|(root, markers)| Found { root, path: index.path(root), markers }).collect();
    found.sort_by_key(|found| found.path.to_lowercase());
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta};

    fn dir() -> Meta {
        Meta { attrs: attr::DIRECTORY, ..Meta::default() }
    }

    fn file() -> Meta {
        Meta { size: 10, alloc: 10, ..Meta::default() }
    }

    /// Builds an index from paths below `C:`; a trailing `\` makes a folder.
    fn index(paths: &[&str]) -> Index {
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let mut ids: HashMap<String, u32> = HashMap::new();
        for path in paths {
            let is_dir = path.ends_with('\\');
            let parts: Vec<&str> = path.trim_end_matches('\\').split('\\').collect();
            let mut parent = root;
            let mut sofar = String::new();
            for (i, part) in parts.iter().enumerate() {
                sofar.push('\\');
                sofar.push_str(part);
                let last = i == parts.len() - 1;
                parent = *ids
                    .entry(sofar.to_lowercase())
                    .or_insert_with(|| builder.push(parent, part, if last && !is_dir { file() } else { dir() }));
            }
        }
        builder.finish()
    }

    fn roots(index: &Index, allowed: &[EntryId]) -> Vec<String> {
        discover(index, allowed).into_iter().map(|found| found.path).collect()
    }

    #[test]
    fn roots_are_repositories_or_topmost_marker_folders() {
        let idx = index(&[
            r"Code\app\.git\",
            r"Code\app\Cargo.toml",
            r"Code\app\crates\core\Cargo.toml",
            r"Code\app\web\package.json",
            // A submodule (with its `.git` file) and a nested clone belong to the repository.
            r"Code\app\libs\sub\.git",
            r"Code\app\libs\sub\CMakeLists.txt",
            r"Code\app\third_party\clone\.git\",
            // A workspace without git: members belong to the topmost marker folder.
            r"Code\mono\package.json",
            r"Code\mono\packages\a\package.json",
            r"Code\mono\packages\b\pyproject.toml",
            // A solution and an Unreal project by extension.
            r"Code\game\Game.uproject",
            r"Code\tool\Tool.sln",
            r"Code\tool\Tool\Tool.csproj",
            // A folder named like a marker is not one.
            r"Code\notes\package.json\",
        ]);
        assert_eq!(roots(&idx, &[]), [r"C:\Code\app", r"C:\Code\game", r"C:\Code\mono", r"C:\Code\tool"]);

        let found = discover(&idx, &[]);
        let app = found.iter().find(|f| f.path.ends_with("app")).unwrap();
        let labels: Vec<_> = app.markers.iter().collect();
        assert_eq!(labels, [Marker::Git, Marker::Cargo, Marker::Node, Marker::CMake]);
        let tool = found.iter().find(|f| f.path.ends_with("tool")).unwrap();
        assert!(tool.markers.has(Marker::DotNet) && !tool.markers.has(Marker::Git));
    }

    #[test]
    fn dependency_cache_and_system_locations_are_ignored() {
        let idx = index(&[
            r"Code\web\package.json",
            r"Code\web\node_modules\left-pad\package.json",
            r"Code\web\node_modules\left-pad\.git\",
            r"Users\me\.cargo\registry\src\serde\Cargo.toml",
            r"Users\me\AppData\Local\Temp\unpacked\package.json",
            r"Program Files\Thing\package.json",
            r"Windows\System32\stuff\Cargo.toml",
            r"Code\py\.venv\Lib\site-packages\pkg\pyproject.toml",
            r"Code\rs\target\debug\build\x\Cargo.toml",
            // Stray markers in a drive root or a profile folder do not swallow everything.
            r"package.json",
            r"Users\me\package.json",
            r"Users\me\projects\real\go.mod",
        ]);
        assert_eq!(roots(&idx, &[]), [r"C:\Code\web", r"C:\Users\me\projects\real"]);
    }

    #[test]
    fn allowed_folders_lift_the_location_rules_above_them_only() {
        let idx = index(&[
            r"Users\me\AppData\Local\Temp\sandbox\proj\package.json",
            r"Users\me\AppData\Local\Temp\sandbox\proj\node_modules\dep\package.json",
            r"Users\me\AppData\Local\Temp\other\package.json",
        ]);
        let sandbox = idx.lookup(r"C:\Users\me\AppData\Local\Temp\sandbox").unwrap();
        assert_eq!(roots(&idx, &[sandbox]), [r"C:\Users\me\AppData\Local\Temp\sandbox\proj"]);
        assert!(roots(&idx, &[]).is_empty());
    }

    #[test]
    fn deleted_markers_no_longer_count() {
        let mut idx = index(&[r"Code\gone\Cargo.toml", r"Code\kept\Cargo.toml"]);
        let gone = idx.lookup(r"C:\Code\gone\Cargo.toml").unwrap();
        idx.remove(gone);
        assert_eq!(roots(&idx, &[]), [r"C:\Code\kept"]);
    }
}
