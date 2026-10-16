//! Files and folders from the index: the palette's main source.
//!
//! Every name in the index is a candidate. The scan runs on all cores: a byte check on each
//! folded name throws out those that cannot match (most of them), nucleo scores the rest, and
//! each thread keeps only its best few hundred. That shortlist is then ranked more finely with
//! what a name score cannot see: the whole path (deep and system folders count for less), and
//! whether the whole name was typed. Remembered folders and files are matched too, so one used
//! every day is found even when thousands of names match the query better.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use kestrel_index::{EntryId, Index, NO_ENTRY, fold};
use rayon::prelude::*;

use super::{Hit, Item, Provider, Query, Scorer};
use crate::engine::Engine;
use crate::visibility::Visibility;

/// The provider's id, and the front end handler its items go to.
pub const ID: &str = "files";
/// Candidates kept from the scan for the finer ranking.
const SHORTLIST: usize = 400;
/// Entries each parallel task scans: large enough that tasks are few, since each one sets up
/// a matcher of its own.
const TASK_ENTRIES: usize = 1 << 16;
/// Parts of a path, folded, that mark places people rarely mean: system stores, package
/// caches, version control internals.
const NOISY: [&str; 11] = [
    r"\windows\winsxs\",
    r"\windows\servicing\",
    r"\windows\installer\",
    r"\$recycle.bin\",
    r"\system volume information\",
    r"\node_modules\",
    r"\.git\",
    r"\.cargo\registry\",
    r"\.rustup\toolchains\",
    r"\.nuget\packages\",
    r"\site-packages\",
];

pub struct FilesProvider {
    engine: Arc<Engine>,
}

impl FilesProvider {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

impl Provider for FilesProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let mut scorer = Scorer::new(query.text);
        // An empty palette lists remembered items, which the palette itself adds.
        if scorer.is_empty() {
            return Vec::new();
        }
        let visibility = Visibility { hidden: query.show_hidden, ..Visibility::ALL };
        let mut hits = {
            let index = self.engine.index().read();
            let found = scan(&index, &scorer, visibility, SHORTLIST, &|| query.cancelled());
            found.into_iter().map(|(score, id)| hit_for(&index, id, score, query.text)).collect::<Vec<_>>()
        };
        hits.extend(remembered(query, &mut scorer));
        query.keep_best(&mut hits);
        for hit in &mut hits {
            hit.highlights = scorer.highlights(&hit.item.title);
        }
        hits
    }
}

/// Scores every visible entry whose name matches and returns the best `keep`, best first.
pub fn scan(
    index: &Index,
    scorer: &Scorer,
    visibility: Visibility,
    keep: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<(u32, EntryId)> {
    let best = (0..index.len())
        .into_par_iter()
        .with_min_len(TASK_ENTRIES)
        .fold(
            || (scorer.fork(), Best::new(keep)),
            |(mut scorer, mut best), id| {
                if id % 4096 == 0 && cancelled() {
                    best.stopped = true;
                }
                if best.stopped || index.is_deleted(id) || !visibility.shows(index.attrs(id)) {
                    return (scorer, best);
                }
                if scorer.may_match(index.lower_bytes(id))
                    && let Some(score) = scorer.score(index.name(id))
                {
                    best.offer(score, id);
                }
                (scorer, best)
            },
        )
        .map(|(_, best)| best)
        .reduce(|| Best::new(keep), Best::merge);
    best.into_sorted()
}

/// The best few candidates seen, kept in a min-heap so the weakest is the one dropped.
struct Best {
    keep: usize,
    heap: BinaryHeap<Reverse<(u32, Reverse<EntryId>)>>,
    stopped: bool,
}

impl Best {
    fn new(keep: usize) -> Self {
        Self { keep, heap: BinaryHeap::with_capacity(keep + 1), stopped: false }
    }

    fn offer(&mut self, score: u32, id: EntryId) {
        if self.heap.len() < self.keep {
            self.heap.push(Reverse((score, Reverse(id))));
        } else if self.heap.peek().is_some_and(|Reverse((weakest, _))| score > *weakest) {
            self.heap.pop();
            self.heap.push(Reverse((score, Reverse(id))));
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.stopped |= other.stopped;
        for Reverse((score, Reverse(id))) in other.heap {
            self.offer(score, id);
        }
        self
    }

    fn into_sorted(self) -> Vec<(u32, EntryId)> {
        let mut found: Vec<(u32, EntryId)> = self.heap.into_iter().map(|Reverse((score, Reverse(id)))| (score, id)).collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        found
    }
}

fn hit_for(index: &Index, id: EntryId, score: u32, text: &str) -> Hit {
    let path = index.path(id);
    let dir = index.is_dir(id);
    let parent = match index.parent(id) {
        NO_ENTRY => None,
        parent => Some(index.path(parent)),
    };
    let name = index.name(id).to_owned();
    let matched = refine(score, &name, text, &path, dir);
    Hit::new(item(name, path, parent, dir), matched, Vec::new())
}

fn item(title: String, path: String, parent: Option<String>, dir: bool) -> Item {
    Item { provider: ID.into(), id: path.clone(), title, subtitle: parent, path: Some(path), dir, ..Item::default() }
}

/// The parent folder of a path, as Explorer would show it: `C:\` for a drive's top level.
pub fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('\\');
    let (parent, _) = trimmed.rsplit_once('\\')?;
    Some(if parent.ends_with(':') { format!("{parent}\\") } else { parent.to_owned() })
}

/// Adjusts a name's match score for what the name alone does not show.
pub fn refine(score: u32, name: &str, text: &str, path: &str, dir: bool) -> u32 {
    let name = fold(name);
    let typed = fold(text.trim());
    let stem = if dir { name.as_str() } else { name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem) };
    let mut factor = 1.0;
    // The whole name typed, with or without its extension.
    if name == typed || stem == typed {
        factor *= 1.5;
    }
    // Folders are what people jump to most.
    if dir {
        factor *= 1.08;
    }
    // Each level below the second costs a little, up to a quarter.
    let depth = path.trim_end_matches('\\').matches('\\').count();
    factor *= 1.0 - 0.025 * depth.saturating_sub(2).min(10) as f64;
    let folded_path = fold(path);
    if NOISY.iter().any(|noisy| folded_path.contains(noisy)) {
        factor *= 0.6;
    }
    (f64::from(score) * factor).round() as u32
}

/// Remembered files and folders that match: found even when the scan's shortlist is full of
/// better-matching names, and even outside the index.
fn remembered(query: &Query, scorer: &mut Scorer) -> Vec<Hit> {
    query
        .recents
        .rows()
        .iter()
        .filter(|row| row.item.provider == ID)
        .filter_map(|row| {
            let path = row.item.path.as_deref()?;
            let score = scorer.score(&row.item.title)?;
            std::path::Path::new(path).exists().then(|| {
                let matched = refine(score, &row.item.title, query.text, path, row.item.dir);
                Hit::new(item(row.item.title.clone(), path.to_owned(), parent_of(path), row.item.dir), matched, Vec::new())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, walk};
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};

    const VISIBLE: Visibility = Visibility { hidden: false, ..Visibility::ALL };

    /// A scratch folder with a few named files and folders, indexed.
    fn indexed(name: &str, files: &[&str]) -> (std::path::PathBuf, Index) {
        let root = std::env::temp_dir().join(format!("kestrel-palette-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if file.ends_with('\\') { fs::create_dir_all(&path).unwrap() } else { fs::write(&path, b"x").unwrap() }
        }
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        (root, builder.finish())
    }

    fn names(index: &Index, query: &str, keep: usize) -> Vec<String> {
        let scorer = Scorer::new(query);
        scan(index, &scorer, VISIBLE, keep, &|| false).into_iter().map(|(_, id)| index.name(id).to_owned()).collect()
    }

    #[test]
    fn the_scan_finds_fuzzy_matches_best_first_and_keeps_only_the_best() {
        let (root, index) = indexed(
            "scan",
            &[r"Quarterly report.xlsx", r"notes\quarter-plan.md", r"misc\q-u-a-r-t.txt", r"misc\unrelated.txt", r"Quartz\"],
        );
        let found = names(&index, "quart", 10);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(!found.contains(&"unrelated.txt".to_owned()));
        assert_eq!(found.last().unwrap(), "q-u-a-r-t.txt", "scattered letters rank last: {found:?}");
        assert_eq!(names(&index, "quart", 2).len(), 2);
        assert!(names(&index, "zzz", 10).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hidden_entries_are_found_only_when_shown() {
        let (root, mut index) = indexed("hidden", &["visible-zebra.txt", "secret-zebra.txt"]);
        let secret = index.lookup(root.join("secret-zebra.txt").to_str().unwrap()).unwrap();
        index.update_meta(secret, kestrel_index::Meta { attrs: kestrel_index::attr::HIDDEN, size: 1, ..Default::default() });
        let scorer = Scorer::new("zebra");
        assert_eq!(scan(&index, &scorer, VISIBLE, 10, &|| false).len(), 1);
        assert_eq!(scan(&index, &scorer, Visibility::ALL, 10, &|| false).len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cancelled_scan_stops_early() {
        let (root, index) = indexed("cancel", &["alpha.txt", "alpine.txt"]);
        let cancel = AtomicBool::new(true);
        let scorer = Scorer::new("alp");
        assert!(scan(&index, &scorer, VISIBLE, 10, &|| cancel.load(Ordering::Relaxed)).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn whole_names_folders_and_shallow_paths_rank_higher() {
        let base = 100;
        let plain = refine(base, "budget.xlsx", "budget", r"C:\Users\me\budget-old.xlsx", false);
        assert!(refine(base, "budget.xlsx", "budget", r"C:\Users\me\budget.xlsx", false) > base, "stem typed in full");
        assert_eq!(refine(base, "Budget", "BUDGET", r"C:\Budget", true), 162, "exact name, folder, shallow");
        assert!(refine(base, "x", "x", r"C:\a\b\c\d\e\f\x", false) < refine(base, "x", "x", r"C:\a\x", false));
        assert!(refine(base, "x.txt", "x", r"C:\Windows\WinSxS\amd64\x.txt", false) < plain);
        assert!(refine(base, "x.txt", "x", r"C:\code\node_modules\x.txt", false) < refine(base, "x.txt", "x", r"C:\code\src\x.txt", false));
        assert_eq!(parent_of(r"C:\Users\me"), Some(r"C:\Users".into()));
        assert_eq!(parent_of(r"C:\Users"), Some(r"C:\".into()));
        assert_eq!(parent_of(r"C:\"), None);
    }
}
