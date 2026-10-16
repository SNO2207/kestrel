//! A whole search: the files of a folder, walked from disk, or files chosen by an index search,
//! read on a pool of threads of its own, with progress counted and results handed over as they
//! are found.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use kestrel_index::DirItem;
use kestrel_index::walk;
use rayon::prelude::*;
use serde::Serialize;

use crate::fsinfo::{CLOUD_ONLY, DIRECTORY};
use crate::ignores::{Ignores, Markers, Stack};
use crate::search::{Hit, Outcome, Search, Skip};

/// A file that matched.
#[derive(Clone, Debug)]
pub struct Found {
    /// The folder it is in.
    pub dir: String,
    pub item: DirItem,
    /// Its first matching line, when the search looked for text.
    pub hit: Option<Hit>,
}

impl Found {
    pub fn path(&self) -> String {
        join(&self.dir, &self.item.name)
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

/// What a search has done so far.
#[derive(Default)]
pub struct Progress {
    pub searched: AtomicU64,
    pub matched: AtomicU64,
    pub binary: AtomicU64,
    pub too_large: AtomicU64,
    pub cloud: AtomicU64,
    pub unreadable: AtomicU64,
    pub ignored: AtomicU64,
    /// It stopped at the limit of files found.
    pub full: AtomicBool,
}

/// [`Progress`] as numbers, to report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub searched: u64,
    pub matched: u64,
    pub binary: u64,
    pub too_large: u64,
    pub cloud: u64,
    pub unreadable: u64,
    pub ignored: u64,
    pub full: bool,
}

impl Progress {
    pub fn counts(&self) -> Counts {
        let read = |n: &AtomicU64| n.load(Ordering::Relaxed);
        Counts {
            searched: read(&self.searched),
            matched: read(&self.matched),
            binary: read(&self.binary),
            too_large: read(&self.too_large),
            cloud: read(&self.cloud),
            unreadable: read(&self.unreadable),
            ignored: read(&self.ignored),
            full: self.full.load(Ordering::Relaxed),
        }
    }

    fn skipped(&self, skip: Skip) {
        let counter = match skip {
            Skip::Binary => &self.binary,
            Skip::TooLarge => &self.too_large,
            Skip::CloudOnly => &self.cloud,
            Skip::Unreadable => &self.unreadable,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Where the files come from.
pub enum Source<'a> {
    /// Every file beneath a folder, listed from disk now. `shows` says whether an item with
    /// these attributes is shown (hidden and system items, as the window's options say).
    Folder { root: String, shows: &'a (dyn Fn(u32) -> bool + Sync) },
    /// These files, as an index search found them: `(folder, item)`.
    Files(Vec<(String, DirItem)>),
}

/// A test a file must pass, given its path and its directory entry.
pub type Filter<'f> = dyn Fn(&str, &DirItem) -> bool + Sync + 'f;

/// How to run a search.
pub struct Run<'a> {
    pub search: &'a Search,
    /// `.gitignore` files are honoured inside repositories.
    pub gitignore: bool,
    /// Another test a file must pass before its contents are read, such as the date a photo
    /// was taken. It is never asked about a file whose contents are only in the cloud.
    pub filter: Option<&'a Filter<'a>>,
    pub cancel: &'a AtomicBool,
    pub progress: &'a Progress,
}

/// Threads reading files: searches wait on the disk, and keeping them off the shared pool
/// leaves that free for the index's own searches. `None` if no thread could be started, when
/// the shared pool stands in.
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 12);
        rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|n| format!("kestrel-content-{n}")).build().ok()
    })
    .as_ref()
}

impl Run<'_> {
    fn halted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.progress.full.load(Ordering::Relaxed)
    }

    /// Searches one file and hands it over if it matched.
    fn file(&self, dir: &str, item: DirItem, found: &(dyn Fn(Found) + Sync)) {
        if self.halted() {
            return;
        }
        let path = join(dir, &item.name);
        if item.meta.attrs & CLOUD_ONLY != 0 {
            self.progress.skipped(Skip::CloudOnly);
            return;
        }
        if let Some(filter) = self.filter
            && !filter(&path, &item)
        {
            self.progress.searched.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let outcome = self.search.search_file(&path, &item.name, item.meta.size, item.meta.attrs, self.cancel);
        match outcome {
            Outcome::Cancelled => {}
            Outcome::Skipped(skip) => self.progress.skipped(skip),
            Outcome::NoMatch => {
                self.progress.searched.fetch_add(1, Ordering::Relaxed);
            }
            Outcome::Match(hit) => {
                self.progress.searched.fetch_add(1, Ordering::Relaxed);
                let matched = self.progress.matched.fetch_add(1, Ordering::Relaxed) + 1;
                if matched as usize > self.search.limits.max_found {
                    // Found by a thread that started before the limit was reached.
                    self.progress.matched.fetch_sub(1, Ordering::Relaxed);
                    self.progress.full.store(true, Ordering::Relaxed);
                    return;
                }
                found(Found { dir: dir.to_owned(), item, hit });
                if matched as usize == self.search.limits.max_found {
                    self.progress.full.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// Lists a folder and searches what is in it, its subfolders in parallel. Junctions and
    /// folder links are never followed (`DirItem::alias`), nor `.git` folders entered.
    fn folder<'s>(&'s self, scope: &rayon::Scope<'s>, dir: Arc<str>, above: Stack, shows: &'s (dyn Fn(u32) -> bool + Sync), found: &'s (dyn Fn(Found) + Sync)) {
        if self.halted() {
            return;
        }
        let Ok(items) = walk::list(&dir) else {
            self.progress.unreadable.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let stack = if self.gitignore {
            let find = |name: &str| items.iter().find(|item| item.name.eq_ignore_ascii_case(name));
            let git = find(".git");
            let exclude = git
                .filter(|git| git.meta.attrs & DIRECTORY != 0)
                .and_then(|_| crate::fsinfo::attributes(&format!(r"{}\.git\info\exclude", dir.trim_end_matches('\\'))))
                .map(|info| info.attrs);
            above.enter(&dir, Markers { git: git.is_some(), gitignore: find(".gitignore").map(|item| item.meta.attrs), exclude })
        } else {
            above
        };
        for item in items {
            if !shows(item.meta.attrs) {
                continue;
            }
            let is_dir = item.meta.attrs & DIRECTORY != 0;
            if is_dir && (item.alias || item.name.eq_ignore_ascii_case(".git")) {
                continue;
            }
            let path = join(&dir, &item.name);
            if self.gitignore && stack.ignores(&path, is_dir) {
                self.progress.ignored.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if is_dir {
                let stack = stack.clone();
                scope.spawn(move |scope| self.folder(scope, Arc::from(path), stack, shows, found));
            } else {
                let dir = dir.clone();
                scope.spawn(move |_| self.file(&dir, item, found));
            }
        }
    }

    /// Runs the search over `source`, handing each matching file to `found` as it is found.
    /// Returns once every file has been searched, the search was cancelled, or enough matched.
    pub fn run(&self, source: Source<'_>, found: &(dyn Fn(Found) + Sync)) {
        let work = || match source {
            Source::Folder { root, shows } => {
                // The folder's own repository may begin above it.
                let above = match crate::ignores::parent(&root) {
                    Some(up) if self.gitignore => Ignores::default().stack(up),
                    _ => Stack::default(),
                };
                // A folder searched on purpose is searched even if its repository ignores it.
                let mut above = above;
                above.ignored = false;
                rayon::scope(|scope| self.folder(scope, Arc::from(root.as_str()), above, shows, found));
            }
            Source::Files(files) => {
                let ignores = Ignores::default();
                files.into_par_iter().for_each(|(dir, item)| {
                    if self.halted() {
                        return;
                    }
                    if self.gitignore && ignores.ignored(&join(&dir, &item.name)) {
                        self.progress.ignored.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    self.file(&dir, item, found);
                });
            }
        };
        match pool() {
            Some(pool) => pool.install(work),
            None => work(),
        }
    }
}
