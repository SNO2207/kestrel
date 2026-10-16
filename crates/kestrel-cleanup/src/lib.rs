//! Cleanup recommendations and duplicate files, found in Kestrel's in-memory index.
//!
//! Rules look at the index (which already holds every file on the drive, with sizes on disk and
//! last write times) and at the projects the app found, and each produces recommendations: what
//! could go, why, how much space it frees, how sure the owner can be ([`Tier`]) and how it goes
//! ([`Action`]): to the Recycle Bin, or through the proper tool, shown as its exact command.
//! Nothing here deletes anything: the app removes only what its owner ticks and confirms.
//!
//! Every rule's locations come from [`Locations`], which tests point at a scratch folder laid
//! out like a real profile, so no test reads the owner's real AppData. A [`Guard`] keeps every
//! recommendation out of the system's folders, the owner's own folders, OneDrive, projects'
//! sources, Kestrel's own data and anything reached through a link.
//!
//! [`dupes`] finds exact duplicate files: size groups from the index, then a hash of each
//! file's two ends, then a full BLAKE3 hash, cached in `kestrel-db` by file id.

pub mod dupes;
mod guard;
mod hash;
pub mod inuse;
mod locations;
mod rules;
pub mod system;
mod tree;

#[cfg(test)]
mod testing;

use std::collections::HashSet;

use kestrel_index::{EntryId, Index};
use serde::Serialize;

pub use guard::Guard;
pub use locations::Locations;
pub use system::{BinContents, Tools};

/// FILETIME ticks at the Unix epoch.
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// FILETIME ticks in a day.
pub const DAY: i64 = 864_000_000_000;

/// Milliseconds since the Unix epoch for a FILETIME; 0 for none.
pub fn unix_ms(filetime: i64) -> i64 {
    if filetime <= FILETIME_UNIX_EPOCH { 0 } else { (filetime - FILETIME_UNIX_EPOCH) / 10_000 }
}

/// The current time as a FILETIME.
pub fn now() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    FILETIME_UNIX_EPOCH + (since.as_nanos() / 100) as i64
}

/// How sure the owner can be about a recommendation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Regenerable caches and temporary files.
    Safe,
    /// Things the owner may still want.
    Review,
    /// Shown only, with advice; Kestrel never acts on it.
    Info,
}

/// Which rule a recommendation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    Temp,
    Crash,
    Build,
    PackageCache,
    AppCache,
    Downloads,
    LargeOld,
    DiskImages,
    RecycleBin,
    Windows,
}

/// One thing a recommendation covers: a file or a folder with everything in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub path: String,
    pub dir: bool,
    /// Bytes on disk; for a folder, everything in it.
    pub bytes: u64,
    /// The newest change in it, in milliseconds since the Unix epoch; 0 when unknown.
    pub modified: i64,
    /// Its file id when the scan saw it (0 if unknown), so that a file put in its place since,
    /// under the same name, is never taken for it.
    #[serde(skip)]
    pub file_id: u64,
}

impl Item {
    /// Whether the index's entry for the item is still what the scan saw: the same file or
    /// folder, with nothing in it changed since (a backup tool writing a new file of the same
    /// name, a temp folder a program has started using again).
    pub fn unchanged(&self, index: &Index, entry: EntryId) -> bool {
        let same = self.file_id == 0 || index.file_id(entry) == 0 || index.file_id(entry) == self.file_id;
        same && unix_ms(tree::newest(index, entry)) <= self.modified
    }

    /// Whether what is at the item's path on disk now is still what the scan saw, by its file id
    /// and last write time, read from the item itself.
    pub fn unchanged_on_disk(&self) -> bool {
        kestrel_index::walk::info(&self.path).is_some_and(|meta| {
            let same = self.file_id == 0 || meta.file_id == 0 || meta.file_id == self.file_id;
            same && unix_ms(meta.mtime) <= self.modified
        })
    }
}

/// A program that cleans up properly, such as `cargo clean`. What it removes does not go to
/// the Recycle Bin, so it can't be undone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Tool {
    /// The program's name, found on the PATH when it runs: `cargo`.
    pub program: String,
    pub args: Vec<String>,
    /// The folder it runs in.
    pub dir: Option<String>,
    /// The command, for showing: as it runs (`pnpm store prune`), or its program and verb when
    /// the rest is the item's own path (`cargo clean`).
    pub command: String,
}

impl Tool {
    fn new(program: &str, args: &[&str], dir: Option<&str>) -> Self {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let command = std::iter::once(program.to_owned()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
        Tool { program: program.to_owned(), args, dir: dir.map(str::to_owned), command }
    }

    /// The same tool, shown as `command`.
    fn shown_as(mut self, command: &str) -> Self {
        command.clone_into(&mut self.command);
        self
    }
}

/// What carrying out a recommendation does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Sends the items the owner ticks to the Recycle Bin.
    Recycle,
    /// Runs a tool, on a click.
    Tool(Tool),
    /// Nothing Kestrel does; the advice and links say what the owner can.
    None,
}

/// A place a recommendation can open: `shell:RecycleBinFolder`, `ms-settings:storagesense` or
/// `cleanmgr` (Disk Cleanup).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    pub label: String,
    pub target: String,
}

/// Part of what an Info recommendation counts, such as "Windows Update cache".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Part {
    pub label: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Recommendation {
    /// Stable across scans for the same thing: `temp`, `build:c:\code\app`.
    pub id: String,
    pub rule: Rule,
    pub title: String,
    /// Why, in plain words.
    pub reason: String,
    pub tier: Tier,
    pub action: Action,
    pub items: Vec<Item>,
    /// Bytes on disk it would free (for a tool or an Info recommendation, at most this).
    pub bytes: u64,
    /// What the owner can do themselves, for Info recommendations and tools.
    pub advice: Option<String>,
    /// Something to know before acting, such as that items in use are skipped.
    pub note: Option<String>,
    pub parts: Vec<Part>,
    pub links: Vec<Link>,
}

impl Recommendation {
    fn new(id: impl Into<String>, rule: Rule, tier: Tier, title: impl Into<String>, reason: impl Into<String>) -> Self {
        Recommendation {
            id: id.into(),
            rule,
            title: title.into(),
            reason: reason.into(),
            tier,
            action: if tier == Tier::Info { Action::None } else { Action::Recycle },
            items: Vec::new(),
            bytes: 0,
            advice: None,
            note: None,
            parts: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Sets the items, largest first, and the total from them.
    fn with_items(mut self, mut items: Vec<Item>) -> Self {
        items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        self.bytes = items.iter().map(|item| item.bytes).sum();
        self.items = items;
        self
    }

    fn with_tool(mut self, tool: Tool) -> Self {
        self.action = Action::Tool(tool);
        self
    }

    fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn with_advice(mut self, advice: impl Into<String>) -> Self {
        self.advice = Some(advice.into());
        self
    }
}

/// A project the app found (`src-tauri/src/projects`), as the build output rule needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub root: String,
    /// The owner said it is not a project: its sources stay protected, but nothing in it is
    /// recommended.
    pub dismissed: bool,
}

/// Everything the rules look at besides the index.
#[derive(Clone, Debug)]
pub struct Context {
    pub locations: Locations,
    pub projects: Vec<Project>,
    /// Folders never recommended besides those [`Locations`] protects, such as Kestrel's own
    /// data folder when the locations are a test's.
    pub protected: Vec<String>,
    /// The time to judge ages by, as a FILETIME.
    pub now: i64,
    /// Programs running now, by executable name in lower case (`chrome.exe`).
    pub running: HashSet<String>,
    pub tools: Tools,
    pub recycle_bin: Option<BinContents>,
    /// Recommendations that would free less than this are left out (the Recycle Bin and
    /// Windows' own cleanup are always shown).
    pub min_bytes: u64,
    /// Files larger than this, untouched for a year, are offered for review (1 GB).
    pub large_file_bytes: u64,
}

impl Context {
    /// The real machine: its locations, projects, running programs, tools and Recycle Bin.
    #[cfg(windows)]
    pub fn current(app_data: &str, projects: Vec<Project>) -> Self {
        Context {
            locations: Locations::current(),
            projects,
            protected: vec![app_data.to_owned()],
            now: now(),
            running: system::running_programs(),
            tools: Tools::detect(),
            recycle_bin: system::recycle_bin(),
            min_bytes: 1 << 20,
            large_file_bytes: 1 << 30,
        }
    }

    pub fn guard(&self) -> Guard {
        let roots: Vec<String> = self.projects.iter().map(|p| p.root.clone()).collect();
        Guard::new(&self.locations, &self.protected, &roots)
    }
}

/// Entries some recommendation already covers, so later rules don't count them again.
#[derive(Default)]
pub(crate) struct Claims {
    claimed: HashSet<EntryId>,
}

impl Claims {
    pub(crate) fn claim(&mut self, entry: EntryId) {
        self.claimed.insert(entry);
    }

    /// Whether `entry` or a folder above it is claimed.
    pub(crate) fn covers(&self, index: &Index, entry: EntryId) -> bool {
        let mut cur = entry;
        while cur != kestrel_index::NO_ENTRY {
            if self.claimed.contains(&cur) {
                return true;
            }
            cur = index.parent(cur);
        }
        false
    }
}

/// Every rule, in the order a scan runs them.
pub const RULES: &[Rule] = rules::ALL;

/// A scan in progress, one rule at a time, so a caller can let go of the index between rules
/// (the app's index is kept current by a thread that needs to write it).
pub struct Scan<'a> {
    context: &'a Context,
    guard: Guard,
    claims: Claims,
    found: Vec<Recommendation>,
}

impl<'a> Scan<'a> {
    pub fn new(context: &'a Context) -> Self {
        Scan { context, guard: context.guard(), claims: Claims::default(), found: Vec::new() }
    }

    /// Runs one rule. Rules run in the order of [`RULES`]: earlier ones claim what they offer.
    pub fn run(&mut self, rule: Rule, index: &Index) {
        let found = rules::run(rule, index, self.context, &self.guard, &mut self.claims);
        self.found.extend(found);
    }

    /// The recommendations, safest first, largest first within a tier.
    pub fn finish(self) -> Vec<Recommendation> {
        let min = self.context.min_bytes.max(1);
        let mut found = self.found;
        found.retain(|rec| matches!(rec.rule, Rule::RecycleBin | Rule::Windows) || rec.bytes >= min);
        found.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.bytes.cmp(&a.bytes)).then_with(|| a.id.cmp(&b.id)));
        found
    }
}

/// Runs every rule over the index and returns the recommendations, safest first, largest first
/// within a tier.
pub fn scan(index: &Index, context: &Context) -> Vec<Recommendation> {
    let mut scan = Scan::new(context);
    for rule in RULES {
        scan.run(*rule, index);
    }
    scan.finish()
}

/// Folded form of a path for prefix comparisons: lower case, backslashes, no trailing one.
pub(crate) fn fold_path(path: &str) -> String {
    let folded = kestrel_index::fold(&path.replace('/', "\\"));
    folded.trim_end_matches('\\').to_owned()
}

/// Whether `path` is `folder` or lies beneath it; both folded.
pub(crate) fn within(path: &str, folder: &str) -> bool {
    path == folder || (path.len() > folder.len() && path.starts_with(folder) && path.as_bytes()[folder.len()] == b'\\')
}

/// Leaves out items no longer on disk. The index catches up with a deletion a moment after it,
/// so a scan straight after items went to the Recycle Bin would offer them again.
pub fn existing(recommendations: Vec<Recommendation>) -> Vec<Recommendation> {
    recommendations
        .into_iter()
        .filter_map(|mut rec| {
            let before = rec.items.len();
            rec.items.retain(|item| std::fs::symlink_metadata(&item.path).is_ok());
            match rec.items.len() {
                n if n == before => Some(rec),
                0 => None,
                _ => {
                    let items = std::mem::take(&mut rec.items);
                    Some(rec.with_items(items))
                }
            }
        })
        .collect()
}

/// Keeps only what lies beneath `root`: items elsewhere are dropped, and recommendations left
/// with none go, except those that have no items to begin with (the Recycle Bin, Windows' own
/// cleanup) when `whole_drive` says the view is of a whole drive.
pub fn within_root(recommendations: Vec<Recommendation>, root: &str, whole_drive: bool) -> Vec<Recommendation> {
    let root = fold_path(root);
    recommendations
        .into_iter()
        .filter_map(|mut rec| {
            if rec.items.is_empty() {
                return whole_drive.then_some(rec);
            }
            rec.items.retain(|item| within(&fold_path(&item.path), &root));
            if rec.items.is_empty() {
                return None;
            }
            let items = std::mem::take(&mut rec.items);
            Some(rec.with_items(items))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, items: &[&str]) -> Recommendation {
        Recommendation::new(id, Rule::Temp, Tier::Safe, "t", "r").with_items(
            items.iter().map(|path| Item { path: path.to_string(), dir: false, bytes: 10, modified: 0, file_id: 0 }).collect(),
        )
    }

    #[test]
    fn paths_compare_folded() {
        assert_eq!(fold_path(r"C:\Users\Me\"), r"c:\users\me");
        assert!(within(r"c:\users\me\a", r"c:\users\me"));
        assert!(within(r"c:\users\me", r"c:\users\me"));
        assert!(!within(r"c:\users\meg", r"c:\users\me"));
    }

    /// An item is still the scan's only while it is what the scan saw: a new file written under
    /// the same name since (a backup tool's next backup), or a temp folder a program has started
    /// using again, is not.
    #[cfg(windows)]
    #[test]
    fn items_are_acted_on_only_while_they_are_what_the_scan_saw() {
        use crate::testing::Sandbox;
        let sandbox = Sandbox::new("unchanged");
        let backup = r"Users\me\backups\full.bak";
        sandbox.write(backup, 100_000, 1);
        sandbox.write(r"Users\me\AppData\Local\Temp\setup-123\a.tmp", 20_000, 2);
        sandbox.age_tree("Users", 800);
        let mut context = sandbox.context();
        context.large_file_bytes = 64 * 1024;
        let index = sandbox.index();
        let recs = scan(&index, &context);
        let item_at = |id: &str| recs.iter().find(|r| r.id == id).unwrap().items[0].clone();
        let (old, temp) = (item_at("large-old"), item_at("temp"));
        let entry = |index: &Index, item: &Item| index.lookup(&item.path).unwrap();
        assert!(old.unchanged(&index, entry(&index, &old)) && old.unchanged_on_disk());
        assert!(temp.unchanged(&index, entry(&index, &temp)) && temp.unchanged_on_disk());

        // A new backup under the same name, and a file added to the temp folder.
        std::fs::remove_file(sandbox.path(backup)).unwrap();
        sandbox.write(backup, 100_000, 3);
        sandbox.write(r"Users\me\AppData\Local\Temp\setup-123\b.tmp", 20_000, 4);
        assert!(!old.unchanged_on_disk(), "the new backup is another file");
        assert!(!temp.unchanged_on_disk(), "the folder changed");
        let index = sandbox.index();
        assert!(!old.unchanged(&index, entry(&index, &old)));
        assert!(!temp.unchanged(&index, entry(&index, &temp)));
    }

    /// Every rule at once, over a machine whose protected folders are full of what the rules look
    /// for: crash dumps, old installers, large old files. None of it may appear, nor anything
    /// reached through a junction, nor a project's sources.
    #[cfg(windows)]
    #[test]
    fn nothing_protected_or_reached_through_a_junction_is_ever_recommended() {
        use crate::testing::{Sandbox, item_paths};
        let sandbox = Sandbox::new("exclusions");
        let protected = [
            r"Users\me\Documents",
            r"Users\me\Pictures",
            r"Users\me\Videos",
            r"Users\me\Music",
            r"Users\me\Desktop",
            r"Users\me\OneDrive",
            r"Users\me\AppData\Local\Kestrel",
            "Program Files",
            "Program Files (x86)",
            "ProgramData",
            "Windows",
        ];
        for (n, folder) in protected.iter().enumerate() {
            for name in ["crash.dmp", "setup.exe", "big.bin", r"Temp\old.tmp", r"Downloads\setup.exe"] {
                sandbox.write(&format!(r"{folder}\{name}"), 100_000, n as u8);
            }
        }
        // What the rules should find, to show they ran.
        sandbox.write(r"Users\me\Downloads\real\setup.exe", 100_000, 50);
        sandbox.write(r"Users\me\AppData\Local\Temp\real.tmp", 100_000, 51);
        // A project's sources, large and old as they are.
        sandbox.write(r"Users\me\code\app\Cargo.toml", 100_000, 52);
        sandbox.write(r"Users\me\code\app\assets\huge.bin", 100_000, 53);
        sandbox.age_tree("", 400);
        // Junctions from places the rules look in to protected folders.
        let junction = |link: &str, target: &str| {
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(sandbox.path(link))
                .arg(sandbox.path(target))
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J {link}");
        };
        junction(r"Users\me\Downloads\docs-link", r"Users\me\Documents");
        junction(r"Users\me\AppData\Local\Temp\docs-link", r"Users\me\Documents");

        let mut context = sandbox.context();
        // As when OneDrive backs up the owner's folders: Windows points them into OneDrive, and
        // the profile's own Documents and Pictures, still full, must stay protected too.
        context.locations.documents = sandbox.text(r"Users\me\OneDrive\Documents");
        context.locations.pictures = sandbox.text(r"Users\me\OneDrive\Pictures");
        context.large_file_bytes = 64 * 1024;
        context.protected = vec![sandbox.text(r"Users\me\AppData\Local\Kestrel")];
        context.projects = vec![Project { root: sandbox.text(r"Users\me\code\app"), dismissed: false }];
        let recs = scan(&sandbox.index(), &context);
        let paths = item_paths(&sandbox, &recs);
        assert!(paths.contains(&"users/me/downloads/real/setup.exe".to_owned()), "{paths:?}");
        assert!(paths.contains(&"users/me/appdata/local/temp/real.tmp".to_owned()), "{paths:?}");
        for path in &paths {
            let refused = protected.iter().any(|folder| path.starts_with(&folder.to_lowercase().replace('\\', "/")))
                || path.contains("docs-link")
                || path.contains("code/app");
            assert!(!refused, "{path} must never be recommended");
        }
        std::fs::remove_dir(sandbox.path(r"Users\me\Downloads\docs-link")).unwrap();
        std::fs::remove_dir(sandbox.path(r"Users\me\AppData\Local\Temp\docs-link")).unwrap();
    }

    #[test]
    fn items_already_gone_from_disk_are_left_out() {
        let sandbox = crate::testing::Sandbox::new("existing");
        let kept = sandbox.write("kept.tmp", 5000, 1);
        let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let gone = text(&sandbox.path("gone.tmp"));
        let recs = vec![rec("both", &[&text(&kept), &gone]), rec("none", &[&gone]), rec("bin", &[])];
        let left = existing(recs);
        assert_eq!(left.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["both", "bin"]);
        assert_eq!((left[0].items.len(), left[0].bytes), (1, 10));
    }

    #[test]
    fn a_root_keeps_only_what_lies_beneath_it() {
        let recs = vec![rec("a", &[r"C:\A\1", r"C:\B\2"]), rec("b", &[r"C:\B\3"]), rec("bin", &[])];
        let kept = within_root(recs.clone(), r"c:\a", false);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].items.len(), 1);
        assert_eq!(kept[0].bytes, 10);
        let drive = within_root(recs, r"C:\", true);
        assert_eq!(drive.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a", "b", "bin"]);
    }
}
