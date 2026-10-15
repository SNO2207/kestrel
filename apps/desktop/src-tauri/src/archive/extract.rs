//! Extracting an archive, or some of its items, safely.
//!
//! Nothing is written until the whole archive has been checked: every entry's path must stay
//! inside the destination ([`names::safe_path`]), and the sizes the archive declares must fit in
//! the destination drive's free space. Then everything is written into a hidden staging folder
//! beside or inside the destination, on the same drive, and only moved into place once it is
//! all there. A cancel, a damaged archive, a password that doesn't fit or a bomb removes the
//! staging folder, so no half-written item is ever left behind.
//!
//! While writing, each file may hold no more than the archive declared for it, and the whole
//! extraction no more than [`MAX_RATIO`] times the archive's size (or [`MIN_BUDGET`], if more):
//! a decompression bomb stops there. Links are never created: symbolic links are skipped, and a
//! tar hard link becomes a plain copy of the file it names when that file is in the same
//! extraction (and so inside the destination), or is skipped otherwise. Names Windows can't
//! hold are renamed; names that clash once renamed, or that differ only in case, are numbered.
//!
//! Moving into place is Explorer's: a new destination folder is simply renamed into place;
//! into an existing folder, every item that clashes with nothing is moved in, and the clashes go
//! to the shell's copy engine, which asks whether to replace, skip or keep both, as Explorer's
//! own extraction does.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, FileTimes};
use std::io::{BufWriter, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, GetDiskFreeSpaceExW, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
};

use super::address::Format;
use super::job::{Failure, Job};
use super::motw::{self, Zone};
use super::names;
use super::reader::{self, Entry, Kind};
use super::time;
use crate::fileops::{self, Done, Op, Options};

/// How much an archive may expand: just above the most DEFLATE can achieve (1032 to 1), far
/// above what real data reaches with any method.
pub const MAX_RATIO: u64 = 1100;
/// What any archive may expand to, however small, so that small archives of very repetitive
/// data still extract. Tests use a lower floor, to reach it quickly.
pub const MIN_BUDGET: u64 = if cfg!(test) { 64 << 20 } else { 1 << 30 };
/// Room left free on the drive beyond what the archive declares.
const SPACE_MARGIN: u64 = 8 << 20;
/// While writing, free space is looked at again after this much, and the extraction stops if
/// the drive is nearly full: an archive that declared nothing can't fill it.
const SPACE_CHECK_EVERY: u64 = 64 << 20;
const COPY_BUFFER: usize = 256 << 10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// `FILE_FLAG_BACKUP_SEMANTICS`, which opening a folder needs, and the one right setting its
/// date needs.
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

/// What an archive may expand to before extraction stops.
pub fn budget(archive_len: u64) -> u64 {
    archive_len.saturating_mul(MAX_RATIO).max(MIN_BUDGET)
}

/// What to do about items already in the destination.
#[derive(Clone, Copy)]
pub enum Conflicts {
    /// Ask through the shell's copy engine, as Explorer's extraction does.
    Ask(Options),
    /// Replace them. Only for Kestrel's own cache folders, which hold nothing of the user's.
    Replace,
}

pub struct Request<'a> {
    /// The archive file on disk: for one inside another, its copy in the cache.
    pub archive: &'a Path,
    pub format: Format,
    /// The archive's name, for messages.
    pub name: &'a str,
    pub password: Option<&'a str>,
    /// The entries to extract by their paths as a folder view shows them, each with everything
    /// beneath it; `None` for the whole archive.
    pub items: Option<&'a [Vec<String>]>,
    /// The folder inside the archive that `items` are in: their paths below it are kept.
    pub base: &'a [String],
    pub destination: &'a Path,
    /// The mark extracted files get, and the archive to name as their source.
    pub zone: Option<(Zone, &'a Path)>,
    pub conflicts: Conflicts,
}

#[derive(Debug, Default)]
pub struct Extracted {
    /// What the extraction made, top level only: the destination folder when it was new (or
    /// the first new folder above it), otherwise each item now in it.
    pub created: Vec<PathBuf>,
    /// The destination did not exist before.
    pub destination_created: bool,
    /// Symbolic links, and hard links to nothing extracted, left out.
    pub links_skipped: usize,
    /// Items given another name: one Windows can hold, or a number to tell them apart.
    pub renamed: usize,
    /// Nothing was skipped or cancelled when asked about conflicts.
    pub completed: bool,
}

#[derive(Clone, Debug)]
enum Action {
    Dir,
    File { size: u64 },
    /// A plain copy of the planned file at this position: a hard link, written as a file.
    Copy(usize),
    /// Replaced by a later entry of the same name.
    Superseded,
}

#[derive(Clone, Debug)]
struct Planned {
    entry: usize,
    rel: Vec<String>,
    what: Action,
    mtime: i64,
}

struct Plan {
    items: Vec<Planned>,
    /// Planned position by entry.
    by_entry: HashMap<usize, usize>,
    total: u64,
    links_skipped: usize,
    renamed: usize,
}

fn fold_parts(parts: &[String]) -> Vec<String> {
    parts.iter().map(|part| names::fold(part)).collect()
}

fn key(parts: &[String]) -> String {
    fold_parts(parts).join("\\")
}

/// Decides what to write where, refusing the whole archive if any wanted entry would leave the
/// destination or needs a password that wasn't given.
fn plan(entries: &[Entry], request: &Request) -> Result<Plan, Failure> {
    let wanted: Option<Vec<Vec<String>>> = request.items.map(|items| items.iter().map(|item| fold_parts(item)).collect());
    let selected = |shown: &[String]| {
        wanted.as_ref().is_none_or(|items| {
            let shown = fold_parts(shown);
            items.iter().any(|item| shown.len() >= item.len() && shown[..item.len()] == item[..])
        })
    };
    let base = request.base.len();
    let mut plan = Plan { items: Vec::new(), by_entry: HashMap::new(), total: 0, links_skipped: 0, renamed: 0 };
    // Planned files by folded path, and every folder planned or implied.
    let mut files: HashMap<String, usize> = HashMap::new();
    let mut folders: HashSet<String> = HashSet::new();

    for (index, entry) in entries.iter().enumerate() {
        if !selected(&names::shown_path(&entry.name)) {
            continue;
        }
        let safe = names::safe_path(&entry.name).map_err(|_| Failure::Unsafe(entry.name.clone()))?;
        if safe.len() <= base {
            continue;
        }
        let mut rel = safe[base..].to_vec();
        if entry.encrypted && request.password.is_none() && matches!(entry.kind, Kind::File) {
            return Err(Failure::Password);
        }
        let what = match &entry.kind {
            Kind::Symlink => {
                plan.links_skipped += 1;
                continue;
            }
            Kind::Other => continue,
            Kind::Dir => Action::Dir,
            Kind::File => Action::File { size: entry.size },
            Kind::HardLink(target) => {
                let target = names::safe_path(target).ok().filter(|target| target.len() > base).map(|target| key(&target[base..]));
                match target.and_then(|target| files.get(&target).copied()) {
                    Some(at) if matches!(plan.items[at].what, Action::File { .. }) => Action::Copy(at),
                    _ => {
                        plan.links_skipped += 1;
                        continue;
                    }
                }
            }
        };
        let shown: Vec<String> = names::shown_path(&entry.name).into_iter().skip(base).collect();
        // Every folder above must be free to be a folder.
        for depth in 1..rel.len() {
            let above = key(&rel[..depth]);
            if files.contains_key(&above) {
                return Err(Failure::Corrupt(format!("it holds both a file and a folder named {}", rel[..depth].join("\\"))));
            }
            folders.insert(above);
        }
        let mut here = key(&rel);
        if matches!(what, Action::Dir) {
            if files.contains_key(&here) {
                return Err(Failure::Corrupt(format!("it holds both a file and a folder named {}", rel.join("\\"))));
            }
            folders.insert(here);
        } else {
            if let Some(&earlier) = files.get(&here)
                && entries[plan.items[earlier].entry].name == entry.name
            {
                // The same name again: a later version of the file, as tar appends updates.
                if let Action::File { size } = plan.items[earlier].what {
                    plan.total = plan.total.saturating_sub(size);
                }
                plan.items[earlier].what = Action::Superseded;
                plan.by_entry.remove(&plan.items[earlier].entry);
                files.remove(&here);
            }
            let last = rel.len() - 1;
            let original = rel[last].clone();
            let mut n = 2;
            while files.contains_key(&here) || folders.contains(&here) {
                rel[last] = names::numbered(&original, n);
                here = key(&rel);
                n += 1;
            }
            files.insert(here, plan.items.len());
        }
        if rel != shown {
            plan.renamed += 1;
        }
        if let Action::File { size } = what {
            plan.total = plan.total.saturating_add(size);
        }
        if let Action::Copy(at) = what
            && let Action::File { size } = plan.items[at].what
        {
            plan.total = plan.total.saturating_add(size);
        }
        plan.by_entry.insert(index, plan.items.len());
        plan.items.push(Planned { entry: index, rel, what, mtime: entry.mtime });
    }
    Ok(plan)
}

// ---------- the drive ----------

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// The nearest folder at or above `path` that exists.
fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.is_dir())
}

/// Bytes free for this account on the drive holding `folder`, which must exist.
fn free_space(folder: &Path) -> Option<u64> {
    let mut available = 0u64;
    // SAFETY: the path is NUL-terminated and `available` outlives the call; the other two
    // outputs are optional.
    let ok = unsafe { GetDiskFreeSpaceExW(wide(folder).as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(available)
}

/// The drive or share `path` is on, as `C:\`.
fn drive_of(path: &Path) -> String {
    path.ancestors().last().map_or_else(|| path.display().to_string(), |root| root.display().to_string())
}

fn set_hidden(path: &Path, hidden: bool) {
    let path = wide(path);
    // SAFETY: the path is NUL-terminated and outlives both calls.
    unsafe {
        let attrs = GetFileAttributesW(path.as_ptr());
        if attrs != INVALID_FILE_ATTRIBUTES {
            let attrs = if hidden { attrs | FILE_ATTRIBUTE_HIDDEN } else { attrs & !FILE_ATTRIBUTE_HIDDEN };
            SetFileAttributesW(path.as_ptr(), attrs);
        }
    }
}

fn write_failure(path: &Path, error: &std::io::Error) -> Failure {
    Failure::Io(format!("Couldn't write {}: {}", path.display(), reader::io_text(error)))
}

// ---------- staging ----------

/// The hidden folder an extraction writes into, removed with everything in it unless kept.
struct Staging {
    path: PathBuf,
    keep: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

static NEXT_STAGING: AtomicU32 = AtomicU32::new(1);

/// Makes a new, empty, hidden folder in `parent` for writing an extraction of `name`.
fn stage(parent: &Path, name: &str) -> Result<Staging, Failure> {
    let stem = names::safe_name(Format::stem(name));
    loop {
        let n = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{stem} (extracting {}-{n})", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => {
                set_hidden(&path, true);
                return Ok(Staging { path, keep: false });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(write_failure(&path, &error)),
        }
    }
}

// ---------- writing ----------

struct Writer<'a> {
    request: &'a Request<'a>,
    job: &'a Job,
    staging: &'a Path,
    budget: u64,
    declared: bool,
    total: u64,
    written: u64,
    space_checked: u64,
    buffer: Vec<u8>,
}

impl Writer<'_> {
    fn path(&self, rel: &[String]) -> PathBuf {
        let mut path = self.staging.to_path_buf();
        path.extend(rel);
        path
    }

    fn folder(&self, path: &Path) -> Result<(), Failure> {
        fs::create_dir_all(path).map_err(|error| write_failure(path, &error))
    }

    fn file(&mut self, item: &Planned, size: u64, data: &mut dyn Read) -> Result<(), Failure> {
        let path = self.path(&item.rel);
        if let Some(parent) = path.parent() {
            self.folder(parent)?;
        }
        let file = File::create(&path).map_err(|error| write_failure(&path, &error))?;
        let mut out = BufWriter::with_capacity(COPY_BUFFER, file);
        let name = item.rel.last().map(String::as_str).unwrap_or_default().to_owned();
        let mut written = 0u64;
        loop {
            self.job.check()?;
            let n = data.read(&mut self.buffer).map_err(|error| reader::corrupt(&error))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            self.written += n as u64;
            if self.declared && written > size {
                return Err(Failure::Bomb("an item holds more data than the archive says".into()));
            }
            if self.written > self.budget {
                return Err(Failure::Bomb(reader::bomb_text(self.budget)));
            }
            out.write_all(&self.buffer[..n]).map_err(|error| write_failure(&path, &error))?;
            self.job.tick("extracting", self.written, self.total.max(self.written), &name);
            if self.written - self.space_checked >= SPACE_CHECK_EVERY {
                self.space_checked = self.written;
                if let Some(free) = free_space(self.staging)
                    && free < SPACE_CHECK_EVERY
                {
                    let needed = self.total.saturating_sub(self.written).max(SPACE_CHECK_EVERY);
                    return Err(Failure::Space { needed, free, drive: drive_of(self.staging) });
                }
            }
        }
        if self.declared && written < size {
            return Err(Failure::Corrupt("an item is shorter than the archive says".into()));
        }
        let file = out.into_inner().map_err(|error| write_failure(&path, error.error()))?;
        self.finish_file(file, &path, item.mtime);
        Ok(())
    }

    /// Dates a written file and gives it the archive's mark.
    fn finish_file(&self, file: File, path: &Path, mtime: i64) {
        if let Some(modified) = time::system_time(mtime) {
            let _ = file.set_times(FileTimes::new().set_modified(modified));
        }
        drop(file);
        if let Some((zone, source)) = self.request.zone {
            // A drive without alternate streams (FAT) can't hold the mark; Explorer can't
            // either.
            let _ = motw::mark(path, zone, source);
        }
    }

    fn copy(&mut self, item: &Planned, from: &Planned) -> Result<(), Failure> {
        let (source, path) = (self.path(&from.rel), self.path(&item.rel));
        if !source.is_file() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            self.folder(parent)?;
        }
        let copied = fs::copy(&source, &path).map_err(|error| write_failure(&path, &error))?;
        self.written += copied;
        if let Ok(file) = File::options().write(true).open(&path) {
            self.finish_file(file, &path, item.mtime);
        }
        Ok(())
    }
}

// ---------- moving into place ----------

/// Moves everything in `from` into `into`, folder by folder, collecting what clashes.
fn merge(from: &Path, into: &Path, staging: &Path, created: &mut Vec<PathBuf>, clashes: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), Failure> {
    let items = fs::read_dir(from).map_err(|error| write_failure(from, &error))?;
    for item in items {
        let item = item.map_err(|error| write_failure(from, &error))?;
        let (source, target) = (item.path(), into.join(item.file_name()));
        if target == staging {
            continue;
        }
        match fs::symlink_metadata(&target) {
            Err(_) => {
                fs::rename(&source, &target).map_err(|error| write_failure(&target, &error))?;
                created.push(target);
            }
            Ok(meta)
                if meta.is_dir()
                    && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                    && item.file_type().is_ok_and(|kind| kind.is_dir()) =>
            {
                merge(&source, &target, staging, created, clashes)?;
            }
            Ok(_) => clashes.push((source, into.to_path_buf())),
        }
    }
    Ok(())
}

/// Puts what was staged into the existing `destination`, asking about clashes as configured.
fn place(staged: &Path, destination: &Path, conflicts: Conflicts) -> Result<(Vec<PathBuf>, bool), Failure> {
    let mut created = Vec::new();
    let mut clashes = Vec::new();
    merge(staged, destination, staged, &mut created, &mut clashes)?;
    if clashes.is_empty() {
        return Ok((created, true));
    }
    match conflicts {
        Conflicts::Ask(options) => {
            let text = |path: &Path| path.to_string_lossy().into_owned();
            let ops = clashes
                .iter()
                .map(|(source, folder)| Op::Move { source: text(source), folder: text(folder), name: None })
                .collect();
            let outcome = fileops::run(ops, false, options).map_err(Failure::Io)?;
            for done in outcome.results() {
                if let Done::Moved { to, .. } = done {
                    created.push(PathBuf::from(to));
                }
            }
            Ok((created, outcome.completed))
        }
        Conflicts::Replace => {
            for (source, folder) in clashes {
                let target = folder.join(source.file_name().unwrap_or_default());
                let removed = if target.is_dir() { fs::remove_dir_all(&target) } else { fs::remove_file(&target) };
                removed.and_then(|()| fs::rename(&source, &target)).map_err(|error| write_failure(&target, &error))?;
                created.push(target);
            }
            Ok((created, true))
        }
    }
}

// ---------- the whole extraction ----------

/// Extracts what `request` asks for. On any failure, nothing new is left in the destination.
pub fn extract(request: &Request, job: &Job) -> Result<Extracted, Failure> {
    let archive_len = fs::metadata(request.archive).map(|meta| meta.len()).unwrap_or(0);
    let budget = budget(archive_len);
    let cancelled = || job.cancelled();
    let mut source = reader::open(request.archive, request.format, request.password, budget, &cancelled)?;
    job.tick("reading", 0, 0, request.name);
    let entries = source.entries()?;
    job.check()?;
    let plan = plan(&entries, request)?;

    let destination = request.destination;
    if destination.exists() && !destination.is_dir() {
        return Err(Failure::Io(format!("{} is a file, not a folder.", destination.display())));
    }
    let anchor = existing_ancestor(destination)
        .ok_or_else(|| Failure::Io(format!("Couldn't find the drive of {}.", destination.display())))?;
    if let Some(free) = free_space(anchor) {
        let needed = plan.total.saturating_add(SPACE_MARGIN);
        if needed > free {
            return Err(Failure::Space { needed: plan.total, free, drive: drive_of(anchor) });
        }
    }
    let fresh = anchor != destination;
    // The first folder that will be made, for the undo journal: the destination, or the
    // topmost of the folders above it that don't exist yet.
    let top_new = fresh
        .then(|| destination.ancestors().take_while(|ancestor| *ancestor != anchor).last().map(Path::to_path_buf))
        .flatten();

    let mut staging = stage(anchor, request.name)?;
    let mut writer = Writer {
        request,
        job,
        staging: &staging.path,
        budget,
        declared: source.declares_sizes(),
        total: if plan.total > 0 { plan.total } else { archive_len },
        written: 0,
        space_checked: 0,
        buffer: vec![0; COPY_BUFFER],
    };
    source.read(&|index| plan.by_entry.contains_key(&index), &mut |index, data| {
        let Some(&at) = plan.by_entry.get(&index) else { return Ok(()) };
        let item = &plan.items[at];
        match item.what {
            Action::Dir => writer.folder(&writer.path(&item.rel)),
            Action::File { size } => writer.file(item, size, data),
            Action::Copy(from) => writer.copy(item, &plan.items[from]),
            Action::Superseded => Ok(()),
        }
    })?;
    job.check()?;
    // Folders the archive lists without data, and their dates, deepest first so that making
    // what is inside doesn't change them again.
    let mut folders: Vec<&Planned> = plan.items.iter().filter(|item| matches!(item.what, Action::Dir)).collect();
    folders.sort_by_key(|item| std::cmp::Reverse(item.rel.len()));
    for item in &folders {
        let path = writer.path(&item.rel);
        writer.folder(&path)?;
        let opened = File::options().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(BACKUP_SEMANTICS).open(&path);
        if let (Some(modified), Ok(folder)) = (time::system_time(item.mtime), opened) {
            let _ = folder.set_times(FileTimes::new().set_modified(modified));
        }
    }
    drop(writer);

    job.tick("placing", plan.total, plan.total, request.name);
    let mut extracted = Extracted {
        links_skipped: plan.links_skipped,
        renamed: plan.renamed,
        destination_created: fresh,
        ..Extracted::default()
    };
    if fresh {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| write_failure(parent, &error))?;
        }
        fs::rename(&staging.path, destination).map_err(|error| write_failure(destination, &error))?;
        staging.keep = true;
        set_hidden(destination, false);
        extracted.created = top_new.into_iter().collect();
        extracted.completed = true;
    } else {
        let (created, completed) = place(&staging.path, destination, request.conflicts)?;
        extracted.created = created;
        extracted.completed = completed;
    }
    Ok(extracted)
}
