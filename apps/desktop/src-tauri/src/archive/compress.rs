//! Making archives: zip, 7z and tar.gz.
//!
//! Folders go in with everything beneath them, empty folders included, and names are stored as
//! UTF-8 (zip sets its UTF-8 flag on every name that isn't plain ASCII, the only names whose
//! encoding is in doubt). Junctions and folder links are not followed, as the index never
//! reads through them. The archive is written under a hidden temporary name beside where it
//! goes and renamed into place only when complete, so a cancel or a failure leaves nothing.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use kestrel_index::{attr, walk};
use serde::Deserialize;
use sevenz_rust2::encoder_options::Lzma2Options;
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, NtTime};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::job::{Failure, Job};
use super::names;
use super::reader::io_text;
use super::time;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
    TarGz,
}

impl Kind {
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Zip => ".zip",
            Kind::SevenZ => ".7z",
            Kind::TarGz => ".tar.gz",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// No compression.
    Store,
    Fast,
    Normal,
    Maximum,
}

/// One thing to put in the archive.
#[derive(Debug)]
struct Item {
    path: PathBuf,
    /// Its path in the archive, with `/` between parts.
    name: String,
    dir: bool,
    size: u64,
    mtime: i64,
}

/// Lists `sources` and everything beneath them, folders before what they hold.
fn gather(sources: &[PathBuf], skip: &[&Path], job: &Job) -> Result<Vec<Item>, Failure> {
    let mut items = Vec::new();
    for source in sources {
        let meta = fs::symlink_metadata(source).map_err(|error| read_failure(source, &error))?;
        let name = source.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_owned();
        let mtime = meta.modified().map_or(0, time::from_system);
        let alias = meta.file_type().is_symlink() && meta.is_dir();
        if meta.is_dir() && !alias && !is_reparse(&meta) {
            items.push(Item { path: source.clone(), name: name.clone(), dir: true, size: 0, mtime });
            walk_folder(source, &name, skip, &mut items, job)?;
        } else if !meta.is_dir() {
            let size = fs::metadata(source).map(|meta| meta.len()).unwrap_or(0);
            items.push(Item { path: source.clone(), name, dir: false, size, mtime });
        }
    }
    Ok(items)
}

fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & attr::REPARSE_POINT != 0
}

fn walk_folder(folder: &Path, name: &str, skip: &[&Path], items: &mut Vec<Item>, job: &Job) -> Result<(), Failure> {
    job.check()?;
    let text = folder.to_string_lossy();
    let mut listed = walk::list(&text).map_err(|error| read_failure(folder, &error))?;
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in listed {
        let path = folder.join(&entry.name);
        if skip.contains(&path.as_path()) || entry.alias {
            continue;
        }
        let inner = format!("{name}/{}", entry.name);
        let dir = entry.meta.attrs & attr::DIRECTORY != 0;
        items.push(Item { path: path.clone(), name: inner.clone(), dir, size: entry.meta.size, mtime: entry.meta.mtime });
        if dir {
            walk_folder(&path, &inner, skip, items, job)?;
        }
    }
    Ok(())
}

fn read_failure(path: &Path, error: &io::Error) -> Failure {
    Failure::Io(format!("Couldn't read {}: {}", path.display(), io_text(error)))
}

fn write_failure(path: &Path, error: impl std::fmt::Display) -> Failure {
    Failure::Io(format!("Couldn't write {}: {error}", path.display()))
}

/// Reads a file into the archive, counting for progress and stopping at a cancel.
struct Progressed<'a> {
    file: File,
    job: &'a Job,
    done: &'a mut u64,
    total: u64,
    name: &'a str,
}

impl Read for Progressed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.job.cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.file.read(buf)?;
        *self.done += n as u64;
        self.job.tick("compressing", *self.done, self.total, self.name);
        Ok(n)
    }
}

/// The archive being written, removed unless finished.
struct Partial {
    path: PathBuf,
    keep: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

static NEXT_PARTIAL: AtomicU32 = AtomicU32::new(1);

/// The first of `name`, `name (2)`, `name (3)`… free in `folder`, numbered before the archive's
/// whole extension (`photos (2).tar.gz`).
pub fn unused_name(folder: &Path, name: &str, kind: Kind) -> String {
    let ext = kind.extension();
    let stem = name.strip_suffix(ext).unwrap_or(name);
    (1..)
        .map(|n| if n == 1 { format!("{stem}{ext}") } else { format!("{stem} ({n}){ext}") })
        .find(|candidate| !folder.join(candidate).exists())
        .unwrap_or_else(|| format!("{stem}{ext}"))
}

pub struct Request<'a> {
    pub sources: &'a [PathBuf],
    pub folder: &'a Path,
    /// The archive's name, extension included.
    pub name: &'a str,
    pub kind: Kind,
    pub level: Level,
}

/// Writes the archive and returns where it is; a name taken meanwhile gets a number.
pub fn compress(request: &Request, job: &Job) -> Result<PathBuf, Failure> {
    crate::fileops::validate_name(request.name).map_err(Failure::Io)?;
    let n = NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed);
    let partial_path = request.folder.join(format!(".{}.{}-{n}.partial", request.name, std::process::id()));
    let target_now = request.folder.join(request.name);
    let items = gather(request.sources, &[&partial_path, &target_now], job)?;
    let total: u64 = items.iter().filter(|item| !item.dir).map(|item| item.size).sum();

    let file = File::create_new(&partial_path).map_err(|error| write_failure(&partial_path, io_text(&error)))?;
    let mut partial = Partial { path: partial_path.clone(), keep: false };
    hide(&partial_path);
    let mut done = 0u64;
    let written = match request.kind {
        Kind::Zip => write_zip(file, &items, request.level, job, &mut done, total),
        Kind::SevenZ => write_7z(file, &items, request.level, job, &mut done, total),
        Kind::TarGz => write_tar_gz(file, &items, request.level, job, &mut done, total),
    };
    if job.cancelled() {
        return Err(Failure::Cancelled);
    }
    written?;
    job.tick("compressing", total, total, request.name);

    let name = unused_name(request.folder, request.name, request.kind);
    let target = request.folder.join(&name);
    fs::rename(&partial_path, &target).map_err(|error| write_failure(&target, io_text(&error)))?;
    partial.keep = true;
    show(&target);
    Ok(target)
}

fn hide(path: &Path) {
    set_hidden(path, true);
}

fn show(path: &Path) {
    set_hidden(path, false);
}

fn set_hidden(path: &Path, hidden: bool) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: the path is NUL-terminated and outlives both calls.
    unsafe {
        let attrs = GetFileAttributesW(wide.as_ptr());
        if attrs != INVALID_FILE_ATTRIBUTES {
            let attrs = if hidden { attrs | FILE_ATTRIBUTE_HIDDEN } else { attrs & !FILE_ATTRIBUTE_HIDDEN };
            SetFileAttributesW(wide.as_ptr(), attrs);
        }
    }
}

fn open(item: &Item) -> Result<File, Failure> {
    File::open(&item.path).map_err(|error| read_failure(&item.path, &error))
}

/// A zip's date: local time, from 1980 on.
fn zip_date(ticks: i64) -> zip::DateTime {
    time::to_local(ticks)
        .and_then(|(year, month, day, hour, minute, second)| {
            zip::DateTime::from_date_and_time(year.clamp(1980, 2107), month, day, hour, minute, second).ok()
        })
        .unwrap_or_default()
}

fn write_zip(file: File, items: &[Item], level: Level, job: &Job, done: &mut u64, total: u64) -> Result<(), Failure> {
    let (method, level) = match level {
        Level::Store => (CompressionMethod::Stored, None),
        Level::Fast => (CompressionMethod::Deflated, Some(1)),
        Level::Normal => (CompressionMethod::Deflated, Some(6)),
        Level::Maximum => (CompressionMethod::Deflated, Some(9)),
    };
    let zip_error = |error: zip::result::ZipError| Failure::Io(format!("Couldn't write the archive: {error}"));
    let mut zip = ZipWriter::new(BufWriter::new(file));
    for item in items {
        job.check()?;
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(level)
            .last_modified_time(zip_date(item.mtime))
            .large_file(item.size >= u32::MAX as u64);
        if item.dir {
            zip.add_directory(format!("{}/", item.name), options).map_err(zip_error)?;
        } else {
            zip.start_file(item.name.as_str(), options).map_err(zip_error)?;
            let mut reader = Progressed { file: open(item)?, job, done, total, name: &item.name };
            io::copy(&mut reader, &mut zip).map_err(|error| copy_failure(item, &error, job))?;
        }
    }
    let out = zip.finish().map_err(zip_error)?;
    finish_file(out)
}

/// A failed copy into an archive: the cancel that stopped it, or a read or write error.
fn copy_failure(item: &Item, error: &io::Error, job: &Job) -> Failure {
    if job.cancelled() { Failure::Cancelled } else { read_failure(&item.path, error) }
}

/// Flushes the finished archive to the disk.
fn finish_file(out: BufWriter<File>) -> Result<(), Failure> {
    let file = out
        .into_inner()
        .map_err(|error| Failure::Io(format!("Couldn't write the archive: {}", io_text(error.error()))))?;
    file.sync_all().map_err(|error| Failure::Io(format!("Couldn't write the archive: {}", io_text(&error))))
}

/// Exactly `left` bytes of a file, or an error if it turns out shorter: a tar header gives the
/// size before the data, and a file that shrank meanwhile would leave the archive unreadable.
struct Exact<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let room = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..room])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "it changed while it was being compressed"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

fn write_7z(file: File, items: &[Item], level: Level, job: &Job, done: &mut u64, total: u64) -> Result<(), Failure> {
    let seven_error = |error: sevenz_rust2::Error| {
        if job.cancelled() { Failure::Cancelled } else { Failure::Io(format!("Couldn't write the archive: {error}")) }
    };
    let mut writer = ArchiveWriter::new(BufWriter::new(file)).map_err(seven_error)?;
    let method = match level {
        Level::Store => EncoderMethod::COPY.into(),
        // 7-Zip's levels; above 7 the dictionary, and the memory compressing takes, grows past
        // what a file manager should ask for.
        Level::Fast => Lzma2Options::from_level(1).into(),
        Level::Normal => Lzma2Options::from_level(5).into(),
        Level::Maximum => Lzma2Options::from_level(7).into(),
    };
    writer.set_content_methods(vec![method]);
    for item in items {
        job.check()?;
        let mut entry = if item.dir { ArchiveEntry::new_directory(&item.name) } else { ArchiveEntry::new_file(&item.name) };
        if item.mtime > 0 {
            entry.last_modified_date = NtTime::new(item.mtime as u64);
            entry.has_last_modified_date = true;
        }
        entry.has_windows_attributes = true;
        entry.windows_attributes = if item.dir { attr::DIRECTORY } else { 0x20 };
        // sevenz-rust2 0.20 writes the anti-item flags of entries without data inverted, which
        // would make every folder an anti-item (a deletion marker 7-Zip skips). Saying "anti"
        // here comes out as "not anti", and with no entry marked the flags are left out.
        entry.is_anti_item = item.dir;
        if item.dir {
            writer.push_archive_entry::<&[u8]>(entry, None).map_err(seven_error)?;
        } else {
            let reader = Progressed { file: open(item)?, job, done, total, name: &item.name };
            writer.push_archive_entry(entry, Some(reader)).map_err(seven_error)?;
        }
    }
    let out = writer.finish().map_err(|error| seven_error(sevenz_rust2::Error::from(error)))?;
    finish_file(out)
}

fn write_tar_gz(file: File, items: &[Item], level: Level, job: &Job, done: &mut u64, total: u64) -> Result<(), Failure> {
    let level = match level {
        Level::Store => 0,
        Level::Fast => 1,
        Level::Normal => 6,
        Level::Maximum => 9,
    };
    let gzip = flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::new(level));
    let mut tar = tar::Builder::new(gzip);
    let tar_error = |error: io::Error| if job.cancelled() { Failure::Cancelled } else { Failure::Io(format!("Couldn't write the archive: {}", io_text(&error))) };
    for item in items {
        job.check()?;
        let mut header = tar::Header::new_gnu();
        header.set_mtime(time::to_unix(item.mtime).max(0) as u64);
        if item.dir {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            tar.append_data(&mut header, format!("{}/", item.name), io::empty()).map_err(tar_error)?;
        } else {
            let file = open(item)?;
            // The size as it is now, which the header gives before the data.
            let size = file.metadata().map_or(item.size, |meta| meta.len());
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(size);
            let reader = Exact { inner: Progressed { file, job, done, total, name: &item.name }, left: size };
            tar.append_data(&mut header, &item.name, reader).map_err(|error| match job.cancelled() {
                true => Failure::Cancelled,
                false => read_failure(&item.path, &error),
            })?;
        }
    }
    let gzip = tar.into_inner().map_err(tar_error)?;
    let out = gzip.finish().map_err(tar_error)?;
    finish_file(out)
}

/// The name "Compress to ZIP file" gives an archive of `first`: the item's name without its
/// extension, as Explorer names it.
pub fn default_name(first: &Path, kind: Kind) -> String {
    let name = first.file_name().and_then(|name| name.to_str()).unwrap_or("Archive");
    let stem = if first.is_dir() {
        name
    } else {
        match name.rfind('.') {
            Some(dot) if dot > 0 => &name[..dot],
            _ => name,
        }
    };
    format!("{}{}", names::safe_name(stem), kind.extension())
}
