//! Exact duplicate files within chosen folders.
//!
//! The index already knows every file's size, so only files that share a size with another
//! are candidates, and finding them takes no I/O at all. Each candidate's folder is then listed
//! again (which opens no file) to confirm it is still what the index says and that its contents
//! are on this device: a cloud-only placeholder is never opened, since opening one downloads
//! it. Names of one file (hard links, which share a file id) count once. What is left is hashed
//! in two passes, both on background-priority threads: the first and last 64 KB of each file,
//! then, only for files that still match another, the whole file with BLAKE3. Hashes are kept
//! in `kestrel-db` by file id, size and last write time, so a rescan reads only what changed.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use kestrel_db::Db;
use kestrel_db::cleanup::CachedHash;
use kestrel_index::{EntryId, Index, NO_ENTRY, attr};
use rayon::prelude::*;
use serde::Serialize;

use crate::hash;
use crate::tree::{find_dir, is_placeholder};
use crate::{fold_path, unix_ms};

/// Where to look and what to skip.
#[derive(Clone, Debug)]
pub struct Request {
    pub roots: Vec<String>,
    /// Folders left out even inside a root, such as AppData.
    pub exclude: Vec<String>,
    /// Files smaller than this are not looked at.
    pub min_size: u64,
}

/// A file that may have a duplicate.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: String,
    /// The folded drive it is on, such as `c:`; file ids are only unique per volume.
    pub volume: String,
    pub file_id: u64,
    pub size: u64,
    /// Bytes on disk.
    pub alloc: u64,
    /// Last write time as a FILETIME.
    pub mtime: i64,
    attrs: u32,
}

/// The files to compare, found in the index.
#[derive(Clone, Debug, Default)]
pub struct Candidates {
    pub files: Vec<Candidate>,
    /// Cloud-only files left alone.
    pub placeholders: u64,
}

/// One copy in a group of duplicates.
#[derive(Clone, Debug, Serialize)]
pub struct DupeFile {
    pub path: String,
    /// Last write time, in milliseconds since the Unix epoch.
    pub modified: i64,
    /// Other names of this same file (hard links): not copies, and they go with it.
    pub links: Vec<String>,
    /// What the file was when it was hashed, to check before acting on it.
    #[serde(skip)]
    pub file_id: u64,
    #[serde(skip)]
    pub size: u64,
    #[serde(skip)]
    pub mtime: i64,
}

/// Files with exactly the same contents.
#[derive(Clone, Debug, Serialize)]
pub struct Group {
    /// The start of the contents' hash, in hex.
    pub id: String,
    pub size: u64,
    /// Bytes on disk of one copy.
    pub bytes_each: u64,
    /// What keeping only one copy would free.
    pub wasted: u64,
    pub files: Vec<DupeFile>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub groups: Vec<Group>,
    /// Files compared: big enough, and sharing a size with another.
    pub candidates: u64,
    /// Files read for a hash this time, and the bytes read.
    pub hashed_files: u64,
    pub hashed_bytes: u64,
    /// Files whose hashes came from the cache.
    pub reused: u64,
    /// Cloud-only files left alone.
    pub placeholders: u64,
    /// Files that changed or couldn't be read while being compared.
    pub skipped: u64,
    pub wasted: u64,
    pub took_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Confirming candidates against their folders.
    Checking,
    /// Hashing the ends of files.
    Partial,
    /// Hashing whole files.
    Full,
}

/// How far a search has got: files for checking and partial hashes, bytes for full hashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

/// The search was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// The drive an entry is on, folded: the name of its topmost folder up to the first separator.
fn volume_of(index: &Index, entry: EntryId) -> String {
    let mut top = entry;
    while index.parent(top) != NO_ENTRY {
        top = index.parent(top);
    }
    let name = fold_path(index.name(top));
    name.split('\\').next().unwrap_or("").to_owned()
}

/// The files under the roots that share a size with another, read from the index alone.
pub fn candidates(index: &Index, request: &Request) -> Candidates {
    let excluded: HashSet<EntryId> = request.exclude.iter().filter_map(|path| find_dir(index, path)).collect();
    let mut seen: HashSet<EntryId> = HashSet::new();
    let mut by_size: HashMap<u64, Vec<(EntryId, u32)>> = HashMap::new();
    let mut placeholders = 0;
    let mut volumes: HashMap<EntryId, String> = HashMap::new();
    for root_path in &request.roots {
        let Some(root) = find_dir(index, root_path) else { continue };
        if !seen.insert(root) {
            continue;
        }
        let volume = volume_of(index, root);
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in index.children(dir) {
                let attrs = index.attrs(entry);
                if attrs & attr::DIRECTORY != 0 {
                    // Folders under another root, or left out, are walked once or not at all.
                    if !excluded.contains(&entry) && seen.insert(entry) {
                        stack.push(entry);
                    }
                    continue;
                }
                let size = index.size(entry);
                if size == 0 || size < request.min_size || attrs & attr::SYSTEM != 0 {
                    continue;
                }
                if is_placeholder(attrs, size, index.alloc(entry)) {
                    placeholders += 1;
                    continue;
                }
                if attrs & attr::REPARSE_POINT != 0 {
                    continue;
                }
                by_size.entry(size).or_default().push((entry, attrs));
                volumes.entry(entry).or_insert_with(|| volume.clone());
            }
        }
    }
    let mut files = Vec::new();
    for (size, entries) in by_size {
        if entries.len() < 2 {
            continue;
        }
        for (entry, attrs) in entries {
            files.push(Candidate {
                path: index.path(entry),
                volume: volumes.get(&entry).cloned().unwrap_or_default(),
                file_id: index.file_id(entry),
                size,
                alloc: index.alloc(entry),
                mtime: index.mtime(entry),
                attrs,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Candidates { files, placeholders }
}

/// Calls the progress callback at most every 100 ms, and always at a phase's end.
struct Meter<'a> {
    report: &'a (dyn Fn(Progress) + Sync),
    started: Instant,
    last: AtomicU64,
}

impl Meter<'_> {
    fn tick(&self, phase: Phase, done: u64, total: u64, force: bool) {
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.last.load(Ordering::Relaxed);
        if force || (now >= last + 100 && self.last.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()) {
            (self.report)(Progress { phase, done, total });
        }
    }
}

/// A candidate as hashing sees it: one file, with every name it has.
#[derive(Clone, Debug)]
struct Unique {
    file: Candidate,
    links: Vec<String>,
    partial: Option<[u8; 32]>,
    full: Option<[u8; 32]>,
    /// The hashes were read now, not taken from the cache.
    fresh: bool,
}

/// Groups by a key, keeping groups of two or more.
fn groups_of<K: std::hash::Hash + Eq>(items: Vec<Unique>, key: impl Fn(&Unique) -> Option<K>) -> Vec<Vec<Unique>> {
    let mut map: HashMap<K, Vec<Unique>> = HashMap::new();
    for item in items {
        if let Some(k) = key(&item) {
            map.entry(k).or_default().push(item);
        }
    }
    map.into_values().filter(|group| group.len() >= 2).collect()
}

/// Lists each candidate's folder again and keeps those still as the index says (with the
/// listing's current size and time), whose contents are on this device.
fn confirm(files: Vec<Candidate>, meter: &Meter, cancel: &AtomicBool) -> Result<(Vec<Candidate>, u64, u64), Cancelled> {
    let total = files.len() as u64;
    let mut by_folder: HashMap<String, Vec<Candidate>> = HashMap::new();
    for file in files {
        let folder = file.path.rsplit_once('\\').map(|(folder, _)| folder.to_owned()).unwrap_or_default();
        by_folder.entry(folder).or_default().push(file);
    }
    let done = AtomicU64::new(0);
    let placeholders = AtomicU64::new(0);
    let gone = AtomicU64::new(0);
    let folders: Vec<(String, Vec<Candidate>)> = by_folder.into_iter().collect();
    let kept: Vec<Candidate> = folders
        .into_par_iter()
        .flat_map_iter(|(folder, files)| {
            let count = files.len() as u64;
            let listed: HashMap<String, kestrel_index::Meta> = if cancel.load(Ordering::Relaxed) {
                HashMap::new()
            } else {
                // A drive root lists as `C:\`.
                let listing = if folder.ends_with(':') { format!("{folder}\\") } else { folder.clone() };
                kestrel_index::walk::list(&listing)
                    .map(|items| items.into_iter().map(|item| (kestrel_index::fold(&item.name), item.meta)).collect())
                    .unwrap_or_default()
            };
            let mut kept = Vec::new();
            for mut file in files {
                let name = kestrel_index::fold(file.path.rsplit('\\').next().unwrap_or(""));
                let Some(meta) = listed.get(&name) else {
                    gone.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                if is_placeholder(meta.attrs, meta.size, meta.alloc) || is_placeholder(file.attrs, file.size, file.alloc) {
                    placeholders.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if meta.attrs & (attr::REPARSE_POINT | attr::DIRECTORY) != 0 || meta.size == 0 {
                    gone.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                (file.size, file.alloc, file.mtime, file.file_id, file.attrs) =
                    (meta.size, meta.alloc, meta.mtime, meta.file_id, meta.attrs);
                kept.push(file);
            }
            let done = done.fetch_add(count, Ordering::Relaxed) + count;
            meter.tick(Phase::Checking, done, total, false);
            kept
        })
        .collect();
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    meter.tick(Phase::Checking, total, total, true);
    Ok((kept, placeholders.into_inner(), gone.into_inner()))
}

/// Finds which candidates have exactly the same contents. Reads files on a few threads of
/// background priority; `progress` hears how far it has got and `cancel` stops it.
pub fn find(
    candidates: Candidates,
    db: Option<&Db>,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<Report, Cancelled> {
    let started = Instant::now();
    let meter = Meter { report: progress, started, last: AtomicU64::new(0) };
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|n| format!("kestrel-dupes-{n}"))
        .start_handler(|_| {
            #[cfg(windows)]
            crate::system::background_priority();
        })
        .build()
        .map_err(|_| Cancelled)?;
    let mut report = Report { candidates: candidates.files.len() as u64, placeholders: candidates.placeholders, ..Report::default() };
    let read = AtomicU64::new(0);
    let hashed = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);

    let (files, placeholders, gone) = pool.install(|| confirm(candidates.files, &meter, cancel))?;
    report.placeholders += placeholders;
    skipped.fetch_add(gone, Ordering::Relaxed);

    // One entry per file: hard links (one file id, several names) are one file, not copies.
    let mut by_id: HashMap<(String, u64), usize> = HashMap::new();
    let mut uniques: Vec<Unique> = Vec::new();
    for file in files {
        let key = (file.volume.clone(), file.file_id);
        if file.file_id != 0
            && let Some(&first) = by_id.get(&key)
        {
            uniques[first].links.push(file.path);
            continue;
        }
        if file.file_id != 0 {
            by_id.insert(key, uniques.len());
        }
        uniques.push(Unique { file, links: Vec::new(), partial: None, full: None, fresh: false });
    }
    let mut all: Vec<Unique> = groups_of(uniques, |u| Some(u.file.size)).into_iter().flatten().collect();

    // Hashes kept from earlier searches, for files unchanged since.
    if let Some(db) = db {
        let keys: Vec<(String, u64)> = all.iter().filter(|u| u.file.file_id != 0).map(|u| (u.file.volume.clone(), u.file.file_id)).collect();
        let cached: HashMap<(String, u64), CachedHash> =
            db.cleanup_hashes(&keys).unwrap_or_default().into_iter().map(|c| ((c.volume.clone(), c.file_id), c)).collect();
        for unique in &mut all {
            if let Some(c) = cached.get(&(unique.file.volume.clone(), unique.file.file_id))
                && c.size == unique.file.size
                && c.mtime == unique.file.mtime
            {
                unique.partial = Some(c.partial);
                unique.full = c.full.or(hash::is_whole(c.size).then_some(c.partial));
                report.reused += 1;
            }
        }
    }

    // The two ends of each file.
    let total = all.len() as u64;
    let done = AtomicU64::new(0);
    let partials: Vec<Option<[u8; 32]>> = pool.install(|| {
        all.par_iter()
            .map(|unique| {
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                meter.tick(Phase::Partial, n, total, false);
                if unique.partial.is_some() || cancel.load(Ordering::Relaxed) {
                    return unique.partial;
                }
                let file = &unique.file;
                match hash::open(&file.path, file.file_id, file.size).and_then(|mut f| hash::partial(&mut f, file.size, &read)) {
                    Ok(partial) => {
                        hashed.fetch_add(1, Ordering::Relaxed);
                        Some(partial)
                    }
                    Err(_) => {
                        skipped.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    meter.tick(Phase::Partial, total, total, true);
    for (unique, partial) in all.iter_mut().zip(partials) {
        if unique.partial.is_none() && partial.is_some() {
            unique.partial = partial;
            unique.full = partial.filter(|_| hash::is_whole(unique.file.size));
            unique.fresh = true;
        }
    }

    // Whole files, only those whose ends match another's.
    let mut by_partial: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    for (at, unique) in all.iter().enumerate() {
        if let Some(partial) = unique.partial {
            by_partial.entry((unique.file.size, partial)).or_default().push(at);
        }
    }
    let suspects: Vec<usize> = by_partial.into_values().filter(|group| group.len() >= 2).flatten().collect();
    let to_read: Vec<usize> = suspects.iter().copied().filter(|&at| all[at].full.is_none()).collect();
    let total_bytes: u64 = to_read.iter().map(|&at| all[at].file.size).sum();
    let so_far = AtomicU64::new(0);
    let fulls: Vec<(usize, Option<[u8; 32]>)> = pool.install(|| {
        to_read
            .par_iter()
            .map(|&at| {
                let file = &all[at].file;
                if cancel.load(Ordering::Relaxed) {
                    return (at, None);
                }
                let counted = AtomicU64::new(0);
                let result = hash::open(&file.path, file.file_id, file.size)
                    .and_then(|mut f| hash::full(&mut f, file.size, &counted, cancel));
                let bytes = counted.into_inner();
                read.fetch_add(bytes, Ordering::Relaxed);
                meter.tick(Phase::Full, so_far.fetch_add(bytes, Ordering::Relaxed) + bytes, total_bytes, false);
                match result {
                    Ok(full) => (at, full),
                    Err(_) => {
                        skipped.fetch_add(1, Ordering::Relaxed);
                        (at, None)
                    }
                }
            })
            .collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    meter.tick(Phase::Full, total_bytes, total_bytes, true);
    for (at, full) in fulls {
        if full.is_some() {
            all[at].full = full;
            all[at].fresh = true;
        }
    }

    // What was read now is kept for next time.
    if let Some(db) = db {
        let rows: Vec<CachedHash> = all
            .iter()
            .filter(|u| u.fresh && u.file.file_id != 0)
            .filter_map(|u| {
                Some(CachedHash {
                    volume: u.file.volume.clone(),
                    file_id: u.file.file_id,
                    size: u.file.size,
                    mtime: u.file.mtime,
                    partial: u.partial?,
                    full: u.full,
                })
            })
            .collect();
        if let Err(error) = db.cleanup_store_hashes(&rows, unix_ms(crate::now()) / 1000) {
            eprintln!("could not keep the duplicate finder's hashes: {error}");
        }
    }

    let matched: Vec<Unique> = suspects.into_iter().map(|at| all[at].clone()).collect();
    report.groups = groups_of(matched, |u| u.full.map(|f| (u.file.size, f))).into_iter().map(group).collect();
    report.groups.sort_by(|a, b| b.wasted.cmp(&a.wasted).then(b.size.cmp(&a.size)).then_with(|| a.id.cmp(&b.id)));
    report.wasted = report.groups.iter().map(|g| g.wasted).sum();
    report.hashed_files = hashed.into_inner();
    report.hashed_bytes = read.into_inner();
    report.skipped = skipped.into_inner();
    report.took_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

fn group(mut members: Vec<Unique>) -> Group {
    members.sort_by_key(|member| member.file.path.to_lowercase());
    let first = &members[0];
    let id: String = first.full.unwrap_or_default()[..8].iter().map(|b| format!("{b:02x}")).collect();
    let size = first.file.size;
    let bytes_each = if first.file.alloc > 0 { first.file.alloc } else { size };
    let files: Vec<DupeFile> = members
        .into_iter()
        .map(|u| DupeFile {
            modified: unix_ms(u.file.mtime),
            links: u.links,
            file_id: u.file.file_id,
            size: u.file.size,
            mtime: u.file.mtime,
            path: u.file.path,
        })
        .collect();
    Group { id, size, bytes_each, wasted: bytes_each * (files.len() as u64 - 1), files }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::testing::Sandbox;

    const MB: usize = 1 << 20;

    fn request(sandbox: &Sandbox, min_size: u64) -> Request {
        Request { roots: vec![sandbox.text(r"Users\me")], exclude: vec![sandbox.text(r"Users\me\AppData")], min_size }
    }

    fn search(sandbox: &Sandbox, index: &Index, db: Option<&Db>) -> Report {
        find(candidates(index, &request(sandbox, MB as u64)), db, &|_| {}, &AtomicBool::new(false)).unwrap()
    }

    /// The groups found, each as its files' names.
    fn names(report: &Report) -> Vec<Vec<String>> {
        let mut groups: Vec<Vec<String>> = report
            .groups
            .iter()
            .map(|g| g.files.iter().map(|f| f.path.rsplit('\\').next().unwrap().to_owned()).collect())
            .collect();
        groups.sort();
        groups
    }

    /// `len` bytes from `seed`, with the middle byte changed by `twist` (0 leaves it alone).
    fn data(len: usize, seed: u8, twist: u8) -> Vec<u8> {
        let mut bytes: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect();
        bytes[len / 2] ^= twist;
        bytes
    }

    fn set_attributes(path: &std::path::Path, attributes: u32) {
        use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
        let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        assert_ne!(unsafe { SetFileAttributesW(wide.as_ptr(), attributes) }, 0, "{}", path.display());
    }

    fn opened_in(sandbox: &Sandbox) -> Vec<String> {
        let base = sandbox.base.to_string_lossy().to_lowercase();
        hash::OPENED.lock().iter().filter(|p| p.to_lowercase().starts_with(&base)).cloned().collect()
    }

    #[test]
    fn exact_duplicates_are_found_and_lookalikes_are_not() {
        let sandbox = Sandbox::new("dupes");
        let photo = data(MB + MB / 2, 1, 0);
        for rel in [r"Users\me\Pictures\a.jpg", r"Users\me\Downloads\a copy.jpg", r"Users\me\Desktop\sub\a (2).jpg"] {
            sandbox.write_bytes(rel, &photo);
        }
        // The same size, different contents.
        sandbox.write_bytes(r"Users\me\Videos\x.bin", &data(MB + 600_000, 2, 0));
        sandbox.write_bytes(r"Users\me\Videos\y.bin", &data(MB + 600_000, 3, 0));
        // The same first and last 64 KB, a different middle.
        sandbox.write_bytes(r"Users\me\Music\e1.bin", &data(MB + 700_000, 4, 0));
        sandbox.write_bytes(r"Users\me\Music\e2.bin", &data(MB + 700_000, 4, 0x55));
        // Too small by default, and outside the roots.
        sandbox.write_bytes(r"Users\me\Documents\s1.txt", &data(100_000, 5, 0));
        sandbox.write_bytes(r"Users\me\Documents\s2.txt", &data(100_000, 5, 0));
        sandbox.write_bytes(r"Users\me\AppData\Local\cache\a.jpg", &photo);

        let index = sandbox.index();
        let report = search(&sandbox, &index, None);
        assert_eq!(names(&report), [vec!["a (2).jpg", "a copy.jpg", "a.jpg"]]);
        let group = &report.groups[0];
        assert_eq!(group.size, photo.len() as u64);
        assert_eq!(group.wasted, 2 * group.bytes_each);
        assert!(group.bytes_each >= photo.len() as u64);
        assert_eq!(report.candidates, 7);
        assert_eq!(report.hashed_files, 7, "every candidate's ends");
        // Whole files are read only where the ends matched: the photos and the lookalikes.
        let ends = 7 * 2 * hash::EDGE;
        let whole = 3 * photo.len() as u64 + 2 * (MB + 700_000) as u64;
        assert_eq!(report.hashed_bytes, ends + whole);

        // Smaller files are compared when asked.
        let small = find(candidates(&index, &request(&sandbox, 1)), None, &|_| {}, &AtomicBool::new(false)).unwrap();
        assert!(names(&small).contains(&vec!["s1.txt".to_owned(), "s2.txt".to_owned()]));
    }

    #[test]
    fn hard_links_are_one_file_not_duplicates() {
        let sandbox = Sandbox::new("dupes-links");
        let original = sandbox.write_bytes(r"Users\me\h1.bin", &data(MB + 1000, 6, 0));
        sandbox.dir(r"Users\me\Pictures");
        std::fs::hard_link(&original, sandbox.path(r"Users\me\Pictures\h1-link.bin")).unwrap();
        let report = search(&sandbox, &sandbox.index(), None);
        assert!(report.groups.is_empty(), "two names of one file: {:?}", names(&report));

        sandbox.write_bytes(r"Users\me\Downloads\h-copy.bin", &data(MB + 1000, 6, 0));
        let report = search(&sandbox, &sandbox.index(), None);
        assert_eq!(report.groups.len(), 1);
        let files = &report.groups[0].files;
        assert_eq!(files.len(), 2, "the copy and the file, whose other name goes with it");
        assert_eq!(files.iter().map(|f| f.links.len()).sum::<usize>(), 1);
    }

    #[test]
    fn cloud_placeholders_are_never_opened() {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE};
        let sandbox = Sandbox::new("dupes-cloud");
        for (name, seed) in [("p1", 7u8), ("p2", 8), ("p3", 9)] {
            let bytes = data(MB + 2000 + seed as usize, seed, 0);
            sandbox.write_bytes(&format!(r"Users\me\{name}.bin"), &bytes);
            sandbox.write_bytes(&format!(r"Users\me\Downloads\{name}-copy.bin"), &bytes);
        }
        // p1 is marked offline on disk, and p2 has nothing allocated, as the index knows them.
        set_attributes(&sandbox.path(r"Users\me\p1.bin"), FILE_ATTRIBUTE_OFFLINE);
        let mut index = sandbox.index();
        let p2 = index.lookup(&sandbox.text(r"Users\me\p2.bin")).unwrap();
        let meta =
            kestrel_index::Meta { alloc: 0, size: index.size(p2), mtime: index.mtime(p2), attrs: index.attrs(p2), file_id: index.file_id(p2) };
        index.update_meta(p2, meta);
        // p3 became a placeholder after the index last saw it: only its folder's listing knows.
        set_attributes(&sandbox.path(r"Users\me\p3.bin"), FILE_ATTRIBUTE_OFFLINE);

        let report = search(&sandbox, &index, None);
        assert!(report.groups.is_empty(), "{:?}", names(&report));
        assert_eq!(report.placeholders, 3);
        let opened = opened_in(&sandbox);
        assert!(opened.is_empty(), "nothing alone in its size is opened, and no placeholder ever: {opened:?}");
        for name in ["p1", "p3"] {
            set_attributes(&sandbox.path(&format!(r"Users\me\{name}.bin")), FILE_ATTRIBUTE_NORMAL);
        }
    }

    #[test]
    fn hashes_are_reused_until_a_file_changes() {
        let sandbox = Sandbox::new("dupes-cache");
        let db = Db::open_in_memory().unwrap();
        let len = MB + 3000;
        sandbox.write_bytes(r"Users\me\d1.bin", &data(len, 10, 0));
        sandbox.write_bytes(r"Users\me\Downloads\d2.bin", &data(len, 10, 0));
        sandbox.write_bytes(r"Users\me\Videos\other.bin", &data(len, 11, 0));

        let first = search(&sandbox, &sandbox.index(), Some(&db));
        assert_eq!((first.groups.len(), first.hashed_files, first.reused), (1, 3, 0));
        let second = search(&sandbox, &sandbox.index(), Some(&db));
        assert_eq!((second.groups.len(), second.hashed_files, second.reused, second.hashed_bytes), (1, 0, 3, 0));

        // d2 changes in the middle: only it is read again, and it no longer matches d1.
        std::thread::sleep(std::time::Duration::from_millis(20));
        sandbox.write_bytes(r"Users\me\Downloads\d2.bin", &data(len, 10, 0x0f));
        let third = search(&sandbox, &sandbox.index(), Some(&db));
        assert_eq!((third.groups.len(), third.hashed_files, third.reused), (0, 1, 2));
        assert_eq!(third.hashed_bytes, 2 * hash::EDGE + len as u64, "d2's ends, then all of it; d1's full hash was kept");
    }

    #[test]
    fn a_search_can_be_cancelled_and_reports_progress() {
        let sandbox = Sandbox::new("dupes-cancel");
        sandbox.write_bytes(r"Users\me\a.bin", &data(MB + 10, 12, 0));
        sandbox.write_bytes(r"Users\me\b.bin", &data(MB + 10, 12, 0));
        let index = sandbox.index();
        let cancelled = AtomicBool::new(true);
        assert_eq!(find(candidates(&index, &request(&sandbox, MB as u64)), None, &|_| {}, &cancelled).unwrap_err(), Cancelled);

        let seen = parking_lot::Mutex::new(Vec::new());
        let report =
            find(candidates(&index, &request(&sandbox, MB as u64)), None, &|p| seen.lock().push(p), &AtomicBool::new(false)).unwrap();
        assert_eq!(report.groups.len(), 1);
        let seen = seen.into_inner();
        for phase in [Phase::Checking, Phase::Partial, Phase::Full] {
            let last = seen.iter().rev().find(|p| p.phase == phase).unwrap_or_else(|| panic!("{phase:?} in {seen:?}"));
            assert_eq!(last.done, last.total, "{phase:?} ends complete");
        }
    }
}
