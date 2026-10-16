//! Everything the disk usage overview says about a folder, from one walk of its subtree:
//! totals, a breakdown by type and one by age (each with size on disk, logical size and file
//! counts), the largest files and folders, and the files changed in the last week and month.
//!
//! The walk is split across threads below the first levels of the tree, each part summing
//! into its own accumulator, so a drive of millions of entries takes a fraction of a second.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use kestrel_index::{EntryId, Index};
use rayon::prelude::*;
use serde::Serialize;

use crate::category::{Category, Classifier, Place};
use crate::{DAY_MS, is_cloud_only, unix_ms};

/// Ages that files are grouped by, as the upper bound of each group in days.
pub const AGE_BOUNDS: [(Option<i64>, &str); 7] = [
    (Some(7), "Last 7 days"),
    (Some(30), "7 to 30 days"),
    (Some(182), "1 to 6 months"),
    (Some(365), "6 to 12 months"),
    (Some(730), "1 to 2 years"),
    (Some(1826), "2 to 5 years"),
    (None, "Over 5 years"),
];
/// Windows of recent changes, in days.
pub const RECENT_DAYS: [i64; 2] = [7, 30];
/// Folders listed per window of recent changes.
const RECENT_FOLDERS: usize = 12;
/// A folder one of whose subfolders holds at least this share of it only passes that
/// subfolder on, and is not listed among the largest folders itself.
const PASS_THROUGH_SHARE: f64 = 0.8;
/// Folders gathered before the walk is split among threads.
const SPLIT_AT: usize = 256;

#[derive(Clone, Debug)]
pub struct Options {
    /// Now, in milliseconds since the Unix epoch, which ages are measured from.
    pub now_ms: i64,
    /// Largest files and folders listed.
    pub top: usize,
}

/// Bytes on disk, bytes of content and a count of files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Amount {
    /// Size on disk: bytes allocated.
    pub bytes: u64,
    /// Logical size: bytes of content.
    pub logical: u64,
    pub files: u64,
}

impl Amount {
    fn add(&mut self, bytes: u64, logical: u64) {
        self.bytes += bytes;
        self.logical += logical;
        self.files += 1;
    }

    fn merge(&mut self, other: &Amount) {
        self.bytes += other.bytes;
        self.logical += other.logical;
        self.files += other.files;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// Files on this device.
    #[serde(flatten)]
    pub local: Amount,
    pub folders: u64,
    /// Files whose contents are only in the cloud: they take no space here.
    pub cloud_files: u64,
    pub cloud_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CategoryRow {
    pub category: Category,
    pub label: &'static str,
    #[serde(flatten)]
    pub amount: Amount,
    pub cloud_files: u64,
    pub cloud_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgeRow {
    pub label: &'static str,
    /// The oldest a file in this group is, in days; `None` for the oldest group.
    pub max_days: Option<i64>,
    #[serde(flatten)]
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub entry: EntryId,
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub logical: u64,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    pub mtime: i64,
    /// A file's type, or what a folder's location makes it.
    pub category: Option<Category>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderActivity {
    pub path: String,
    pub bytes: u64,
    pub files: u64,
}

/// Files changed within the last `days` days, on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Recent {
    pub days: i64,
    pub bytes: u64,
    pub files: u64,
    /// The folders holding most of them, by the bytes changed directly inside each.
    pub folders: Vec<FolderActivity>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Overview {
    pub totals: Totals,
    /// Every category, in [`Category::ALL`] order.
    pub categories: Vec<CategoryRow>,
    pub ages: Vec<AgeRow>,
    /// Ages unknown (no modification time recorded).
    pub undated: Amount,
    /// Largest by size on disk; files in the cloud are not among them.
    pub largest_files: Vec<Item>,
    /// Largest by size on disk, leaving out folders that only pass on a larger subfolder.
    pub largest_folders: Vec<Item>,
    pub recent: Vec<Recent>,
}

/// What makes one item larger than another in a top list: its size, then the lower id, so the
/// list is the same every time.
type Rank = (u64, Reverse<EntryId>);

struct Top {
    keep: usize,
    heap: BinaryHeap<Reverse<Rank>>,
}

impl Top {
    fn new(keep: usize) -> Self {
        Self { keep, heap: BinaryHeap::with_capacity(keep + 1) }
    }

    fn offer(&mut self, bytes: u64, id: EntryId) {
        if self.keep == 0 {
            return;
        }
        if self.heap.len() == self.keep && self.heap.peek().is_some_and(|Reverse(least)| *least >= (bytes, Reverse(id))) {
            return;
        }
        self.heap.push(Reverse((bytes, Reverse(id))));
        if self.heap.len() > self.keep {
            self.heap.pop();
        }
    }

    fn merge(&mut self, other: Top) {
        for Reverse((bytes, Reverse(id))) in other.heap {
            self.offer(bytes, id);
        }
    }

    /// Largest first.
    fn into_sorted(self) -> Vec<EntryId> {
        let mut ranked: Vec<Rank> = self.heap.into_iter().map(|Reverse(rank)| rank).collect();
        ranked.sort_unstable_by(|a, b| b.cmp(a));
        ranked.into_iter().map(|(_, Reverse(id))| id).collect()
    }
}

#[derive(Default)]
struct RecentAcc {
    total: Amount,
    by_folder: HashMap<EntryId, Amount>,
}

/// One part of the walk's sums.
struct Acc {
    totals: Totals,
    categories: [Amount; Category::ALL.len()],
    cloud: [(u64, u64); Category::ALL.len()],
    ages: [Amount; AGE_BOUNDS.len()],
    undated: Amount,
    files: Top,
    folders: Top,
    recent: [RecentAcc; RECENT_DAYS.len()],
}

impl Acc {
    fn new(top: usize) -> Self {
        Self {
            totals: Totals::default(),
            categories: [Amount::default(); Category::ALL.len()],
            cloud: [(0, 0); Category::ALL.len()],
            ages: [Amount::default(); AGE_BOUNDS.len()],
            undated: Amount::default(),
            files: Top::new(top),
            folders: Top::new(top),
            recent: Default::default(),
        }
    }

    fn merge(&mut self, other: Acc) {
        self.totals.local.merge(&other.totals.local);
        self.totals.folders += other.totals.folders;
        self.totals.cloud_files += other.totals.cloud_files;
        self.totals.cloud_bytes += other.totals.cloud_bytes;
        for (mine, theirs) in self.categories.iter_mut().zip(&other.categories) {
            mine.merge(theirs);
        }
        for (mine, theirs) in self.cloud.iter_mut().zip(&other.cloud) {
            mine.0 += theirs.0;
            mine.1 += theirs.1;
        }
        for (mine, theirs) in self.ages.iter_mut().zip(&other.ages) {
            mine.merge(theirs);
        }
        self.undated.merge(&other.undated);
        self.files.merge(other.files);
        self.folders.merge(other.folders);
        for (mine, theirs) in self.recent.iter_mut().zip(other.recent) {
            mine.total.merge(&theirs.total);
            for (folder, amount) in theirs.by_folder {
                mine.by_folder.entry(folder).or_default().merge(&amount);
            }
        }
    }
}

struct Walk<'a> {
    index: &'a Index,
    classifier: Classifier,
    root: EntryId,
    now_ms: i64,
}

impl Walk<'_> {
    /// Sums the files directly in `dir`, which is at `place`, and hands its folders to `more`;
    /// returns how many entries it looked at.
    fn visit(&self, dir: EntryId, place: Place, acc: &mut Acc, more: &mut Vec<(EntryId, Place)>) -> usize {
        let index = self.index;
        let mut largest_folder = 0;
        let mut looked = 0;
        for child in index.children(dir) {
            looked += 1;
            if index.is_dir(child) {
                acc.totals.folders += 1;
                largest_folder = largest_folder.max(index.alloc(child));
                more.push((child, self.classifier.child_place(index, dir, place, child)));
                continue;
            }
            self.file(child, dir, place, acc);
        }
        let size = index.alloc(dir);
        if dir != self.root && size > 0 && (largest_folder as f64) < PASS_THROUGH_SHARE * size as f64 {
            acc.folders.offer(size, dir);
        }
        looked
    }

    /// Adds the file `file`, in the folder `dir` at `place`, to the sums.
    fn file(&self, file: EntryId, dir: EntryId, place: Place, acc: &mut Acc) {
        let index = self.index;
        let (bytes, logical) = (index.alloc(file), index.size(file));
        let category = self.classifier.file(index, file, place).index();
        if is_cloud_only(index.attrs(file)) {
            acc.totals.cloud_files += 1;
            acc.totals.cloud_bytes += logical;
            acc.cloud[category].0 += 1;
            acc.cloud[category].1 += logical;
            // A partly downloaded placeholder holds what was downloaded.
            acc.totals.local.bytes += bytes;
            acc.categories[category].bytes += bytes;
            return;
        }
        acc.totals.local.add(bytes, logical);
        acc.categories[category].add(bytes, logical);
        acc.files.offer(bytes, file);
        let mtime = unix_ms(index.mtime(file));
        if mtime == 0 {
            acc.undated.add(bytes, logical);
            return;
        }
        let age_days = (self.now_ms - mtime).max(0) / DAY_MS;
        let group = AGE_BOUNDS
            .iter()
            .position(|(max, _)| max.is_none_or(|max| age_days < max))
            .unwrap_or(AGE_BOUNDS.len() - 1);
        acc.ages[group].add(bytes, logical);
        for (window, days) in acc.recent.iter_mut().zip(RECENT_DAYS) {
            if self.now_ms - mtime < days * DAY_MS {
                window.total.add(bytes, logical);
                window.by_folder.entry(dir).or_default().add(bytes, logical);
            }
        }
    }

    /// Walks everything beneath the folders in `start`.
    /// Walks on from `stack` through about `budget` entries.
    fn walk(&self, stack: &mut Vec<(EntryId, Place)>, acc: &mut Acc, budget: usize) {
        let mut more = Vec::new();
        let mut left = budget;
        while left > 0 {
            let Some((dir, place)) = stack.pop() else { break };
            left = left.saturating_sub(1);
            // Removed while the walk let go of the index (see `OverviewWalk`).
            if dir >= self.index.len() || self.index.is_deleted(dir) {
                continue;
            }
            more.clear();
            left = left.saturating_sub(self.visit(dir, place, acc, &mut more));
            stack.append(&mut more);
        }
    }
}

/// Analyses the subtree of `root`, a folder (or a single file, which is all it holds).
pub fn overview(index: &Index, root: EntryId, options: &Options) -> Overview {
    let mut walk = OverviewWalk::new(root, options);
    while !walk.step(index, usize::MAX) {}
    walk.finish(index)
}

/// Entries an [`OverviewWalk`] looks at in one step, its threads together: a few milliseconds.
pub const STEP: usize = 200_000;

/// [`overview`] in steps, so whoever holds the index can let go of it between them: the thread
/// keeping it current takes it to write every 300 ms, and while that writer waits for a long
/// reader, every other reader waits behind it. Entries keep their ids while the index changes,
/// so the walk goes on where it was; what changed meanwhile is counted as it is when reached.
pub struct OverviewWalk {
    root: EntryId,
    now_ms: i64,
    top: usize,
    started: bool,
    /// What the first levels hold.
    acc: Acc,
    /// The folders still to walk below them, shared out afresh among the parts at each step.
    pending: Vec<(EntryId, Place)>,
    /// The sums of each part, each walked on a thread of its own.
    parts: Vec<Acc>,
}

impl OverviewWalk {
    pub fn new(root: EntryId, options: &Options) -> Self {
        OverviewWalk {
            root,
            now_ms: options.now_ms,
            top: options.top,
            started: false,
            acc: Acc::new(options.top),
            pending: Vec::new(),
            parts: Vec::new(),
        }
    }

    /// Walks on through about `budget` entries, shared among threads; `true` once the whole
    /// subtree has been walked.
    pub fn step(&mut self, index: &Index, budget: usize) -> bool {
        let walk = Walk { index, classifier: Classifier::new(index), root: self.root, now_ms: self.now_ms };
        if !self.started {
            self.started = true;
            self.start(&walk);
            let parts = rayon::current_num_threads().max(1) * 4;
            self.parts = (0..parts).map(|_| Acc::new(self.top)).collect();
        }
        if self.pending.is_empty() {
            return true;
        }
        let count = self.parts.len();
        let mut stacks: Vec<Vec<(EntryId, Place)>> = (0..count).map(|_| Vec::new()).collect();
        for (at, folder) in self.pending.drain(..).enumerate() {
            stacks[at % count].push(folder);
        }
        let share = (budget / count).max(1);
        let left: Vec<Vec<(EntryId, Place)>> = stacks
            .into_par_iter()
            .zip(self.parts.par_iter_mut())
            .map(|(mut stack, acc)| {
                walk.walk(&mut stack, acc, share);
                stack
            })
            .collect();
        self.pending = left.into_iter().flatten().collect();
        self.pending.is_empty()
    }

    /// Walks the first levels, until there are enough folders to share out.
    fn start(&mut self, walk: &Walk) {
        let (index, root) = (walk.index, self.root);
        if index.is_dir(root) {
            let mut frontier = vec![(root, walk.classifier.place_of(index, root))];
            for _ in 0..4 {
                if frontier.len() >= SPLIT_AT || frontier.is_empty() {
                    break;
                }
                let mut next = Vec::new();
                for (dir, place) in frontier {
                    walk.visit(dir, place, &mut self.acc, &mut next);
                }
                frontier = next;
            }
            self.pending = frontier;
        } else if index.parent(root) != kestrel_index::NO_ENTRY {
            // A file on its own is all there is.
            let parent = index.parent(root);
            walk.file(root, parent, walk.classifier.place_of(index, parent), &mut self.acc);
        }
    }

    /// The overview, once [`OverviewWalk::step`] has said the walk is done.
    pub fn finish(self, index: &Index) -> Overview {
        self.sum().overview(index)
    }

    /// The overview's figures, once the walk is done; this needs no index, so it can be
    /// worked out with the index let go of.
    pub fn sum(mut self) -> Summed {
        for part in std::mem::take(&mut self.parts) {
            self.acc.merge(part);
        }
        sum(self.acc)
    }
}

fn item(index: &Index, classifier: &Classifier, id: EntryId) -> Item {
    let category = if index.is_dir(id) {
        classifier.place_of(index, id).category()
    } else {
        Some(classifier.file(index, id, classifier.place_of(index, index.parent(id))))
    };
    Item {
        entry: id,
        path: index.path(id),
        name: index.name(id).to_owned(),
        bytes: index.alloc(id),
        logical: index.size(id),
        mtime: unix_ms(index.mtime(id)),
        category,
    }
}

/// An overview's figures, worked out without the index: all that is left is naming the few
/// items it lists ([`Summed::overview`]), which a caller letting go of the index between steps
/// does with it held once more, briefly.
pub struct Summed {
    acc: Acc,
    files: Vec<EntryId>,
    folders: Vec<EntryId>,
    recent: Vec<RecentWindow>,
}

/// A window of recent changes as summed: its days, its total, and the folders listed.
type RecentWindow = (i64, Amount, Vec<(EntryId, Amount)>);

fn sum(mut acc: Acc) -> Summed {
    let recent = std::mem::take(&mut acc.recent)
        .into_iter()
        .zip(RECENT_DAYS)
        .map(|(window, days)| {
            let mut folders: Vec<(EntryId, Amount)> = window.by_folder.into_iter().collect();
            folders.sort_unstable_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then(a.0.cmp(&b.0)));
            folders.truncate(RECENT_FOLDERS);
            (days, window.total, folders)
        })
        .collect();
    let files = std::mem::replace(&mut acc.files, Top::new(0)).into_sorted();
    let folders = std::mem::replace(&mut acc.folders, Top::new(0)).into_sorted();
    Summed { acc, files, folders, recent }
}

impl Summed {
    /// The overview, naming its items from the index the walk read, which must be of the same
    /// generation (ids stay the same while it changes, not when it is replaced).
    pub fn overview(self, index: &Index) -> Overview {
        let (acc, classifier) = (self.acc, Classifier::new(index));
        let live = |id: &EntryId| *id < index.len();
        let categories = Category::ALL
            .iter()
            .map(|&category| CategoryRow {
                category,
                label: category.label(),
                amount: acc.categories[category.index()],
                cloud_files: acc.cloud[category.index()].0,
                cloud_bytes: acc.cloud[category.index()].1,
            })
            .collect();
        let ages = AGE_BOUNDS
            .iter()
            .zip(acc.ages)
            .map(|(&(max_days, label), amount)| AgeRow { label, max_days, amount })
            .collect();
        let recent = self
            .recent
            .into_iter()
            .map(|(days, total, folders)| Recent {
                days,
                bytes: total.bytes,
                files: total.files,
                folders: folders
                    .into_iter()
                    .filter(|(folder, _)| live(folder))
                    .map(|(folder, amount)| FolderActivity { path: index.path(folder), bytes: amount.bytes, files: amount.files })
                    .collect(),
            })
            .collect();
        Overview {
            totals: acc.totals,
            categories,
            ages,
            undated: acc.undated,
            largest_files: self.files.iter().filter(|id| live(id)).map(|&id| item(index, &classifier, id)).collect(),
            largest_folders: self.folders.iter().filter(|id| live(id)).map(|&id| item(index, &classifier, id)).collect(),
            recent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filetime;
    use crate::testing::{F, build, id};
    use kestrel_index::attr;

    /// 2026-01-01, in milliseconds since the Unix epoch.
    const NOW: i64 = 1_767_225_600_000;

    /// A FILETIME a little more than `days` days before now.
    fn ago(days: i64) -> i64 {
        filetime(NOW - days * DAY_MS - 1)
    }

    fn options() -> Options {
        Options { now_ms: NOW, top: 3 }
    }

    fn row(overview: &Overview, category: Category) -> &CategoryRow {
        &overview.categories[category.index()]
    }

    #[test]
    fn totals_types_and_ages_add_up() {
        let index = build(
            &[
                F(r"Users\me\Videos\trip.mp4", 8_000_000, ago(400), 0),
                F(r"Users\me\Videos\clip.mov", 2_000_000, ago(3), 0),
                F(r"Users\me\Music\song.mp3", 500_000, ago(40), 0),
                F(r"Users\me\Docs\letter.docx", 40_000, ago(10), 0),
                F(r"Users\me\Docs\scan.pdf", 60_000, ago(3000), 0),
                F(r"Users\me\OneDrive\film.mkv", 9_000_000, ago(1), attr::RECALL_ON_DATA_ACCESS),
                F(r"Users\me\OneDrive\offline.docx", 1_000, ago(1), attr::OFFLINE),
                F(r"Users\me\web\package.json", 1_000, ago(2), 0),
                F(r"Users\me\web\node_modules\dep\index.js", 300_000, ago(2), 0),
                F(r"Windows\explorer.exe", 4_000_000, ago(200), 0),
                F(r"Users\me\old.dat", 7_000, 0, 0),
            ],
            &[r"Users\me\Empty"],
        );
        let root = id(&index, r"C:\");
        let overview = overview(&index, root, &options());
        let totals = &overview.totals;
        assert_eq!(totals.local.bytes, index.alloc(root), "everything on disk is accounted for");
        assert_eq!(totals.local.logical + totals.cloud_bytes, index.size(root));
        assert_eq!((totals.local.files, totals.cloud_files, totals.cloud_bytes), (9, 2, 9_001_000));
        // Users, me, Videos, Music, Docs, OneDrive, web, node_modules, dep, Windows, Empty.
        assert_eq!(totals.folders, 11);

        assert_eq!(row(&overview, Category::Video).amount, Amount { bytes: 10_000_000, logical: 10_000_000, files: 2 });
        assert_eq!((row(&overview, Category::Video).cloud_files, row(&overview, Category::Video).cloud_bytes), (1, 9_000_000));
        assert_eq!(row(&overview, Category::Documents).amount.files, 2);
        assert_eq!(row(&overview, Category::Documents).cloud_bytes, 1_000);
        assert_eq!(row(&overview, Category::Audio).amount.bytes, 500_000);
        assert_eq!(row(&overview, Category::Code).amount.bytes, 1_000);
        assert_eq!(row(&overview, Category::BuildOutput).amount.bytes, 300_000);
        assert_eq!(row(&overview, Category::System).amount.bytes, 4_000_000);
        assert_eq!(row(&overview, Category::Other).amount.bytes, 7_000);
        let summed: u64 = overview.categories.iter().map(|row| row.amount.bytes).sum();
        assert_eq!(summed, totals.local.bytes);

        let ages: Vec<(&str, u64)> = overview.ages.iter().map(|row| (row.label, row.amount.bytes)).collect();
        assert_eq!(
            ages,
            [
                ("Last 7 days", 2_000_000 + 1_000 + 300_000),
                ("7 to 30 days", 40_000),
                ("1 to 6 months", 500_000),
                ("6 to 12 months", 4_000_000),
                ("1 to 2 years", 8_000_000),
                ("2 to 5 years", 0),
                ("Over 5 years", 60_000),
            ]
        );
        assert_eq!(overview.undated.bytes, 7_000);

        let largest: Vec<&str> = overview.largest_files.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(largest, ["trip.mp4", "explorer.exe", "clip.mov"], "cloud files take no space");
        assert_eq!(overview.largest_files[0].path, r"C:\Users\me\Videos\trip.mp4");
        assert_eq!(overview.largest_files[1].category, Some(Category::System));

        let week = &overview.recent[0];
        assert_eq!((week.days, week.bytes, week.files), (7, 2_301_000, 3));
        let folders: Vec<&str> = week.folders.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(folders, [r"C:\Users\me\Videos", r"C:\Users\me\web\node_modules\dep", r"C:\Users\me\web"]);
        assert_eq!(overview.recent[1].bytes, 2_341_000);
    }

    #[test]
    fn largest_folders_skip_folders_that_only_pass_one_on() {
        let index = build(
            &[
                F(r"a\b\c\big.bin", 900, 0, 0),
                F(r"a\b\small.bin", 50, 0, 0),
                F(r"a\other.bin", 10, 0, 0),
                F(r"d\x.bin", 300, 0, 0),
                F(r"d\y\z.bin", 200, 0, 0),
            ],
            &[],
        );
        let overview = overview(&index, id(&index, r"C:\"), &Options { now_ms: NOW, top: 10 });
        let folders: Vec<(&str, u64)> = overview.largest_folders.iter().map(|item| (item.path.as_str(), item.bytes)).collect();
        // `a` and `a\b` are nearly all `a\b\c`, so only `c` stands for them; `d` holds its own.
        assert_eq!(folders, [(r"C:\a\b\c", 900), (r"C:\d", 500), (r"C:\d\y", 200)]);
    }

    #[test]
    fn a_folder_or_a_file_can_be_the_root() {
        // notepad.exe has extended attributes, whose bit listings share with "recall on open".
        let ea = crate::RECALL_ON_OPEN;
        let index = build(&[F(r"Windows\notepad.exe", 100, ago(1), ea), F(r"Users\me\a.txt", 5, ago(1), 0)], &[]);
        let windows = overview(&index, id(&index, r"C:\Windows"), &options());
        assert_eq!(windows.totals.local.bytes, 100);
        assert_eq!((windows.totals.local.files, windows.totals.cloud_files), (1, 0), "on the disk, so not in the cloud");
        assert_eq!(row(&windows, Category::System).amount.files, 1, "the location carries over to a folder root");
        assert!(windows.largest_folders.is_empty(), "the root is not listed in itself");

        let file = overview(&index, id(&index, r"C:\Users\me\a.txt"), &options());
        assert_eq!((file.totals.local.bytes, file.totals.local.files, file.totals.folders), (5, 1, 0));
        assert_eq!(row(&file, Category::Documents).amount.files, 1);
        assert_eq!(file.recent[0].folders[0].path, r"C:\Users\me");
    }

    #[test]
    fn the_split_walk_finds_what_a_plain_count_does() {
        // Enough folders, levels and files that the walk is shared among threads.
        let paths: Vec<(String, u64)> = (0..2_000u64)
            .map(|n| {
                let ext = ["mp4", "rs", "zip", "bin"][n as usize % 4];
                (format!(r"top{}\mid{}\leaf{}\f{n}.{ext}", n % 7, n % 31, n % 97), (n * 7919) % 100_000 + 1)
            })
            .collect();
        let files: Vec<F> = paths.iter().map(|(path, size)| F(path, *size, ago(*size as i64 % 900), 0)).collect();
        let index = build(&files, &[]);
        let root = id(&index, r"C:\");
        let overview = overview(&index, root, &Options { now_ms: NOW, top: 25 });
        assert_eq!(overview.totals.local.bytes, index.alloc(root));
        assert_eq!(overview.totals.local.files, 2_000);
        let mut sizes: Vec<u64> = paths.iter().map(|(_, size)| *size).collect();
        sizes.sort_by(|a, b| b.cmp(a));
        let found: Vec<u64> = overview.largest_files.iter().map(|item| item.bytes).collect();
        assert_eq!(found, sizes[..25]);
        let video: u64 = paths.iter().filter(|(path, _)| path.ends_with("mp4")).map(|(_, size)| size).sum();
        assert_eq!(row(&overview, Category::Video).amount.bytes, video);
        let aged: u64 = overview.ages.iter().map(|row| row.amount.bytes).sum();
        assert_eq!(aged, overview.totals.local.bytes);
    }

    /// Walked a few folders at a time, letting go of the index between steps, the overview is
    /// the one a single walk gives; a folder removed between two steps is passed over.
    #[test]
    fn an_overview_walked_in_steps_is_the_same() {
        // Deep enough that folders are left to walk below the first levels.
        let paths: Vec<String> =
            (0..3_000u64).map(|n| format!(r"top{}\a{}\b{}\c{}\d{}\f{n}.mp4", n % 3, n % 5, n % 7, n % 11, n % 13)).collect();
        let files: Vec<F> = paths.iter().enumerate().map(|(n, path)| F(path, n as u64 + 1, ago(n as i64 % 400), 0)).collect();
        let mut index = build(&files, &[]);
        let root = id(&index, r"C:\");
        let options = Options { now_ms: NOW, top: 10 };
        let whole = overview(&index, root, &options);
        let mut walk = OverviewWalk::new(root, &options);
        let mut steps = 0;
        while !walk.step(&index, 1) {
            steps += 1;
        }
        assert!(steps >= 2, "{steps} steps");
        assert_eq!(format!("{:?}", walk.finish(&index)), format!("{whole:?}"));

        // A folder removed between steps.
        let mut walk = OverviewWalk::new(root, &options);
        assert!(!walk.step(&index, 1));
        index.remove(id(&index, r"C:\top0\a0\b0\c0\d0"));
        while !walk.step(&index, 1) {}
        let stepped = walk.finish(&index);
        assert_eq!(stepped.totals.local.bytes, index.alloc(root), "what is gone isn't counted");
    }
}
