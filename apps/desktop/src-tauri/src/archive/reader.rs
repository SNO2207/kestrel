//! Reading every supported format through one interface, [`Source`]: list the entries, then
//! read the wanted ones in the archive's own order.
//!
//! - zip, through the `zip` crate, which checks each file's CRC and decrypts ZipCrypto and AES;
//! - 7z, through `sevenz-rust2`, which checks CRCs and decrypts AES;
//! - tar, plain or compressed with gzip, bzip2 or xz, through `tar` with `flate2`, `bzip2` and
//!   `lzma-rust2`, read as a stream (each listing or extraction reads it from the start);
//! - RAR, which no Rust crate reads well: Windows' own `tar.exe` (libarchive) turns it into a
//!   tar stream on its standard output, read as above, so every safety rule applies to it the
//!   same way. Kestrel never runs any other program for this;
//! - a single compressed file (`.gz`, `.bz2`, `.xz`), as an archive of one file.
//!
//! Every source counts what it decompresses, so a stream that expands without end is stopped
//! by the extraction's budget even while entries are only being skipped.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use sevenz_rust2::{ArchiveEntry, ArchiveReader, EncoderMethod, Password};
use zip::ZipArchive;
use zip::result::ZipError;

use super::address::{Codec, Format};
use super::job::{Failure, size_text};
use super::time;

/// How often a reader that is only skipping data looks for a cancel.
const CHECK_EVERY: u64 = 1 << 20;
/// `CREATE_NO_WINDOW`: Windows' tar.exe runs without a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// 7-Zip keeps a Unix mode in the high 16 bits of the attributes when this bit is set.
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;
/// Attributes of an archived item worth keeping: read-only, hidden, system.
const KEPT_ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link, which is never extracted as one.
    Symlink,
    /// A hard link to the earlier entry named here.
    HardLink(String),
    /// A device, a pipe, an anti-item: nothing to write.
    Other,
}

/// One item in an archive, as stored.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The path as stored, with its own separators.
    pub name: String,
    pub kind: Kind,
    /// The size the archive gives it, uncompressed.
    pub size: u64,
    /// Last write time in FILETIME ticks; 0 when not known.
    pub mtime: i64,
    /// Read-only, hidden and system, where the archive keeps Windows attributes.
    pub attrs: u32,
    /// Its data is encrypted.
    pub encrypted: bool,
}

/// Passes the data of one wanted entry, by its position in [`Source::entries`].
pub type Visit<'v> = dyn FnMut(usize, &mut dyn Read) -> Result<(), Failure> + 'v;

pub trait Source {
    /// Every entry, in the archive's order.
    fn entries(&mut self) -> Result<Vec<Entry>, Failure>;
    /// Reads the entries `wanted` picks out, handing each one's data to `visit`. Entries come
    /// in the order the archive stores their data, which is not always the listing's order.
    /// `last` is the position of the last wanted entry: a stream, read from its start, stops
    /// there rather than going on to its end.
    fn read(&mut self, wanted: &dyn Fn(usize) -> bool, last: usize, visit: &mut Visit<'_>) -> Result<(), Failure>;
    /// Whether each entry's size is known before its data is read, so producing more than that
    /// means the archive lies.
    fn declares_sizes(&self) -> bool {
        true
    }
}

/// An `io::Error` in words, without Rust's "(os error N)".
pub fn io_text(error: &io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if !text.starts_with("CRC") => first.to_lowercase().chain(chars).collect(),
        _ => text.to_owned(),
    }
}

/// A read error from inside an archive: the archive is damaged.
pub fn corrupt(error: &io::Error) -> Failure {
    Failure::Corrupt(lower_first(io_text(error).trim_end_matches('.')))
}

fn open_file(path: &Path) -> Result<File, Failure> {
    File::open(path).map_err(|error| Failure::Io(format!("Couldn't open {}: {}", path.display(), io_text(&error))))
}

/// The format of the file at `path`: what its name says, unless its first bytes say it is
/// another kind of archive (a `.zip` that is really a 7z, say), as archivers decide.
pub fn sniff(path: &Path, named: Format) -> Format {
    let mut head = [0u8; 8];
    let read = File::open(path).and_then(|mut file| file.read(&mut head)).unwrap_or(0);
    let head = &head[..read];
    let by_bytes = if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Format::Zip)
    } else if head.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        Some(Format::SevenZ)
    } else if head.starts_with(b"Rar!\x1A\x07") {
        Some(Format::Rar)
    } else {
        None
    };
    match (named, by_bytes) {
        (Format::Zip | Format::SevenZ | Format::Rar, Some(actual)) => actual,
        _ => named,
    }
}

/// Opens the archive at `path`. `budget` bounds how many bytes stream formats may decompress.
pub fn open<'c>(path: &Path, format: Format, password: Option<&str>, budget: u64, cancelled: &'c dyn Fn() -> bool) -> Result<Box<dyn Source + 'c>, Failure> {
    let path = path.to_path_buf();
    Ok(match format {
        Format::Zip => Box::new(ZipSource::open(&path, password)?),
        Format::SevenZ => Box::new(SevenZ::open(&path, password, cancelled)?),
        Format::Tar(codec) => Box::new(Stream::new(Opener::Tar(path, codec), budget, cancelled)),
        Format::Rar => {
            if rar_encrypted(&path).unwrap_or(false) {
                return Err(Failure::Unsupported(RAR_ENCRYPTED.into()));
            }
            Box::new(Stream::new(Opener::Rar(path), budget, cancelled))
        }
        Format::Single(codec) => Box::new(Single::open(path, codec, budget, cancelled)?),
    })
}

// ---------- counting what is decompressed ----------

/// Why a counted reader stopped.
type Stop = Rc<RefCell<Option<Failure>>>;

/// Counts what passes through, stopping at the budget or when the job is cancelled.
struct Counted<'c, R> {
    inner: R,
    read: u64,
    checked: u64,
    budget: u64,
    cancelled: &'c dyn Fn() -> bool,
    stop: Stop,
}

impl<'c, R: Read> Counted<'c, R> {
    fn new(inner: R, budget: u64, cancelled: &'c dyn Fn() -> bool, stop: Stop) -> Self {
        Counted { inner, read: 0, checked: 0, budget, cancelled, stop }
    }

    fn halt(&self, failure: Failure) -> io::Error {
        *self.stop.borrow_mut() = Some(failure);
        io::Error::other("stopped")
    }
}

#[cfg(test)]
thread_local! {
    /// Bytes decompressed by counted readers on this thread, so tests can see what was read.
    pub(super) static DECOMPRESSED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        #[cfg(test)]
        DECOMPRESSED.with(|counted| counted.set(counted.get() + n as u64));
        self.read += n as u64;
        if self.read > self.budget {
            return Err(self.halt(Failure::Bomb(bomb_text(self.budget))));
        }
        if self.read - self.checked >= CHECK_EVERY {
            self.checked = self.read;
            if (self.cancelled)() {
                return Err(self.halt(Failure::Cancelled));
            }
        }
        Ok(n)
    }
}

/// What the budget's limit means, in words.
pub fn bomb_text(budget: u64) -> String {
    format!("it expands to more than {}, far more than an archive of its size should", size_text(budget))
}

/// The failure behind an error from a counted stream: why it was stopped, if it was.
fn failure_of(stop: &Stop, error: &io::Error) -> Failure {
    stop.borrow_mut().take().unwrap_or_else(|| corrupt(error))
}

/// Replaces a damage report with the counted stream's own reason for stopping, when it has one:
/// a stopped stream shows up downstream as a read error.
fn explain(stop: &Stop, failure: Failure) -> Failure {
    match (failure, stop.borrow_mut().take()) {
        (Failure::Corrupt(_), Some(reason)) => reason,
        (failure, _) => failure,
    }
}

/// Reads and drops an entry's data, looking for a cancel as it goes.
fn drain(data: &mut dyn Read, cancelled: &dyn Fn() -> bool) -> Result<(), Failure> {
    let mut buffer = vec![0u8; 64 << 10];
    let mut since = 0u64;
    loop {
        let n = data.read(&mut buffer).map_err(|error| corrupt(&error))?;
        if n == 0 {
            return Ok(());
        }
        since += n as u64;
        if since >= CHECK_EVERY {
            since = 0;
            if cancelled() {
                return Err(Failure::Cancelled);
            }
        }
    }
}

// ---------- zip ----------

struct ZipSource {
    zip: ZipArchive<BufReader<File>>,
    password: Option<String>,
}

fn zip_failure(error: ZipError) -> Failure {
    match error {
        ZipError::InvalidPassword => Failure::WrongPassword,
        ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => Failure::Password,
        ZipError::UnsupportedArchive(what) => Failure::Unsupported(lower_first(what)),
        ZipError::CompressionMethodNotSupported(method) => {
            Failure::Unsupported(format!("it uses a compression method Kestrel doesn't read (method {method})"))
        }
        ZipError::InvalidArchive(what) => Failure::Corrupt(lower_first(&what)),
        ZipError::Io(error) => corrupt(&error),
        other => Failure::Corrupt(lower_first(&other.to_string())),
    }
}

impl ZipSource {
    fn open(path: &Path, password: Option<&str>) -> Result<Self, Failure> {
        let zip = ZipArchive::new(BufReader::new(open_file(path)?)).map_err(zip_failure)?;
        Ok(ZipSource { zip, password: password.map(str::to_owned) })
    }
}

/// `CP_OEMCP`: the console's code page, which zip names without the UTF-8 flag are in.
const OEM_CODE_PAGE: u32 = 1;

/// A zip entry's name as Explorer reads it. Names that are UTF-8 are taken as such: those with
/// the UTF-8 flag, and those with an Info-ZIP Unicode path, which the zip crate puts in place of
/// the stored name. Others were written in the OEM code page of the machine that made them, and
/// Explorer reads them in this machine's: Shift-JIS on a Japanese system, CP866 on a Russian
/// one, CP437 on an American one. Decoding them as CP437 everywhere, as the zip crate does,
/// turns them into other characters, and Shift-JIS's second bytes 0x5C into backslashes, which
/// would split one name into folders.
fn zip_name(raw: &[u8], fallback: impl FnOnce() -> Option<String>) -> String {
    #[cfg(test)]
    let code_page = TEST_CODE_PAGE.with(|page| page.get()).unwrap_or(OEM_CODE_PAGE);
    #[cfg(not(test))]
    let code_page = OEM_CODE_PAGE;
    decode_name(raw, code_page).or_else(fallback).unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned())
}

#[cfg(test)]
thread_local! {
    /// The code page a test stands in for the OEM one, to read archives made on other systems.
    pub(super) static TEST_CODE_PAGE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// `raw` as UTF-8 if it is, or else in `code_page`; `None` if the code page can't read it.
pub(super) fn decode_name(raw: &[u8], code_page: u32) -> Option<String> {
    use windows_sys::Win32::Globalization::{MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    if let Ok(text) = std::str::from_utf8(raw) {
        return Some(text.to_owned());
    }
    let len = i32::try_from(raw.len()).ok()?;
    // SAFETY: the first call only measures (no output buffer); the second writes at most
    // `needed` UTF-16 units into a buffer of that length. `raw` outlives both calls.
    unsafe {
        let needed = MultiByteToWideChar(code_page, MB_ERR_INVALID_CHARS, raw.as_ptr(), len, std::ptr::null_mut(), 0);
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(code_page, MB_ERR_INVALID_CHARS, raw.as_ptr(), len, wide.as_mut_ptr(), needed);
        (written > 0).then(|| String::from_utf16_lossy(&wide[..written as usize]))
    }
}

/// A zip's DOS date and time, which is local time, in FILETIME ticks.
fn zip_time(time: Option<zip::DateTime>) -> i64 {
    time.map_or(0, |t| time::from_local(t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second()))
}

impl Source for ZipSource {
    fn entries(&mut self) -> Result<Vec<Entry>, Failure> {
        (0..self.zip.len())
            .map(|index| {
                let file = self.zip.by_index_data(index).map_err(zip_failure)?;
                let name = zip_name(file.name_raw(), || file.name().ok().map(|name| name.into_owned()));
                let unix = file.unix_mode();
                let dos = if unix.is_none() { file.external_attributes() & 0xFFFF } else { 0 };
                let link = unix.is_some_and(|mode| mode & 0o170000 == 0o120000) || dos & FILE_ATTRIBUTE_REPARSE_POINT != 0;
                let kind = if file.is_dir() {
                    Kind::Dir
                } else if link {
                    Kind::Symlink
                } else {
                    Kind::File
                };
                Ok(Entry {
                    name,
                    kind,
                    size: file.size(),
                    mtime: zip_time(file.last_modified()),
                    attrs: dos & KEPT_ATTRIBUTES,
                    encrypted: file.encrypted(),
                })
            })
            .collect()
    }

    fn read(&mut self, wanted: &dyn Fn(usize) -> bool, _last: usize, visit: &mut Visit<'_>) -> Result<(), Failure> {
        for index in 0..self.zip.len() {
            if !wanted(index) {
                continue;
            }
            let encrypted = self.zip.by_index_data(index).map_err(zip_failure)?.encrypted();
            let mut file = if encrypted {
                let password = self.password.as_deref().ok_or(Failure::Password)?;
                self.zip.by_index_decrypt(index, password.as_bytes())
            } else {
                self.zip.by_index(index)
            }
            .map_err(zip_failure)?;
            match visit(index, &mut file) {
                // ZipCrypto's password check passes one wrong password in 256; the CRC at the
                // end then fails.
                Err(Failure::Corrupt(_)) if encrypted => return Err(Failure::WrongPassword),
                other => other?,
            }
        }
        Ok(())
    }
}

// ---------- 7z ----------

struct SevenZ<'c> {
    reader: ArchiveReader<BufReader<File>>,
    /// Whether each block is encrypted.
    encrypted: Vec<bool>,
    has_password: bool,
    cancelled: &'c dyn Fn() -> bool,
}

fn seven_failure(error: sevenz_rust2::Error) -> Failure {
    use sevenz_rust2::Error;
    match error {
        Error::PasswordRequired => Failure::Password,
        Error::MaybeBadPassword(_) => Failure::WrongPassword,
        Error::BadSignature(_) => Failure::Corrupt("it isn't a 7z archive".into()),
        Error::UnsupportedCompressionMethod(method) => {
            Failure::Unsupported(format!("it uses a compression method Kestrel doesn't read ({method})"))
        }
        Error::Unsupported(what) => Failure::Unsupported(lower_first(&what)),
        Error::MaxMemLimited { .. } => Failure::Unsupported("it needs more memory to decompress than Kestrel allows".into()),
        Error::ChecksumVerificationFailed | Error::NextHeaderCrcMismatch => Failure::Corrupt("a checksum doesn't match".into()),
        Error::Io(error, _) | Error::FileOpen(error, _) => corrupt(&error),
        other => Failure::Corrupt(lower_first(&other.to_string())),
    }
}

impl<'c> SevenZ<'c> {
    fn open(path: &Path, password: Option<&str>, cancelled: &'c dyn Fn() -> bool) -> Result<Self, Failure> {
        let secret = password.map_or_else(Password::empty, Password::from);
        let mut reader = ArchiveReader::new(BufReader::new(open_file(path)?), secret).map_err(seven_failure)?;
        reader.set_thread_count(1);
        let encrypted = reader
            .archive()
            .blocks
            .iter()
            .map(|block| block.coders.iter().any(|coder| coder.encoder_method_id() == EncoderMethod::ID_AES256_SHA256))
            .collect();
        Ok(SevenZ { reader, encrypted, has_password: password.is_some(), cancelled })
    }
}

/// Whether a 7z entry is a link: a reparse point, or a Unix symbolic link.
fn seven_link(entry: &ArchiveEntry) -> bool {
    let attrs = entry.windows_attributes;
    entry.has_windows_attributes
        && (attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (attrs & FILE_ATTRIBUTE_UNIX_EXTENSION != 0 && (attrs >> 16) & 0o170000 == 0o120000))
}

impl Source for SevenZ<'_> {
    fn entries(&mut self) -> Result<Vec<Entry>, Failure> {
        let archive = self.reader.archive();
        Ok(archive
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let kind = if file.is_anti_item {
                    Kind::Other
                } else if seven_link(file) {
                    Kind::Symlink
                } else if file.is_directory {
                    Kind::Dir
                } else {
                    Kind::File
                };
                let block = archive.stream_map.file_block_index.get(index).copied().flatten();
                Entry {
                    name: file.name.clone(),
                    kind,
                    size: file.size,
                    mtime: if file.has_last_modified_date { u64::from(file.last_modified_date) as i64 } else { 0 },
                    attrs: if file.has_windows_attributes { file.windows_attributes & KEPT_ATTRIBUTES } else { 0 },
                    encrypted: block.is_some_and(|block| self.encrypted.get(block).copied().unwrap_or(false)),
                }
            })
            .collect())
    }

    fn read(&mut self, wanted: &dyn Fn(usize) -> bool, _last: usize, visit: &mut Visit<'_>) -> Result<(), Failure> {
        let encrypted: Vec<bool> = self.entries()?.iter().map(|entry| entry.encrypted).collect();
        // The decoder hands each entry over by reference into the archive's list of files; its
        // position there is its index.
        let files = &self.reader.archive().files;
        let (base, count) = (files.as_ptr() as usize, files.len());
        let stride = size_of::<ArchiveEntry>();
        let cancelled = self.cancelled;
        let mut failure: Option<Failure> = None;
        let mut seen = vec![false; count];
        let result = self.reader.for_each_entries(|entry, data| {
            let offset = (entry as *const ArchiveEntry as usize).wrapping_sub(base);
            let index = offset / stride;
            let outcome = if offset % stride != 0 || index >= count {
                Err(Failure::Corrupt("its list of files doesn't match its contents".into()))
            } else if seen[index] {
                Ok(())
            } else {
                seen[index] = true;
                if wanted(index) { visit(index, data) } else { drain(data, cancelled) }
            };
            outcome.map(|()| true).map_err(|stop| {
                failure = Some(stop);
                sevenz_rust2::Error::Other("stopped".into())
            })
        });
        match failure {
            // A wrong password decodes to garbage, which fails a check further on.
            Some(Failure::Corrupt(_)) if self.has_password && encrypted.iter().any(|&e| e) => Err(Failure::WrongPassword),
            Some(failure) => Err(failure),
            None => result.map_err(seven_failure),
        }
    }
}

// ---------- tar streams, and RAR through Windows' tar.exe ----------

enum Opener {
    Tar(PathBuf, Codec),
    Rar(PathBuf),
}

/// Windows' own tar.exe (bsdtar, on libarchive), which reads RAR.
fn system_tar() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join("tar.exe")
}

/// tar.exe converting a RAR archive into a tar stream.
struct Converter {
    child: Option<Child>,
    errors: Option<std::thread::JoinHandle<String>>,
}

impl Converter {
    /// Starts tar.exe with the archive on its standard input, so no path, however spelled, is
    /// ever parsed by it, and `-P` so names reach Kestrel exactly as stored, to be judged here.
    fn start(path: &Path) -> Result<(Box<dyn Read>, Converter), Failure> {
        let archive = open_file(path)?;
        let mut child = Command::new(system_tar())
            .args(["-P", "--format", "pax", "-cf", "-", "@-"])
            .stdin(Stdio::from(archive))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| {
                Failure::Unsupported(format!("Windows' tar.exe, which reads RAR archives, didn't start ({})", io_text(&error)))
            })?;
        let stdout = child.stdout.take().ok_or_else(|| Failure::Io("tar.exe gave no output".into()))?;
        let mut stderr = child.stderr.take();
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(stderr) = stderr.as_mut() {
                let _ = stderr.read_to_string(&mut text);
            }
            text
        });
        Ok((Box::new(BufReader::new(stdout)), Converter { child: Some(child), errors: Some(errors) }))
    }

    /// Waits for tar.exe, which reports a damaged or encrypted archive only by failing.
    fn finish(mut self) -> Result<(), Failure> {
        let status = self.child.take().map(|mut child| child.wait());
        let errors = self.errors.take().and_then(|errors| errors.join().ok()).unwrap_or_default();
        match status {
            Some(Ok(status)) if status.success() => Ok(()),
            _ => Err(rar_failure(&errors)),
        }
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What a password-protected RAR archive is, for the rest of "Kestrel can't open …".
const RAR_ENCRYPTED: &str = "it's a password-protected RAR archive, which Kestrel can't decrypt";

/// What tar.exe's complaint about a RAR archive means. libarchive often gives no words at all
/// ("(null)"), which is why encryption is looked for beforehand ([`rar_encrypted`]).
fn rar_failure(errors: &str) -> Failure {
    let lower = errors.to_lowercase();
    if lower.contains("encrypt") || lower.contains("password") {
        return Failure::Unsupported(RAR_ENCRYPTED.into());
    }
    let line = errors
        .lines()
        .map(|line| line.trim().trim_start_matches("tar.exe:").trim())
        .find(|line| !line.is_empty() && *line != "(null)")
        .unwrap_or("Windows' tar.exe couldn't read it");
    Failure::Corrupt(lower_first(line.trim_end_matches('.')))
}

/// Reads a RAR variable-length integer: seven bits a byte, low bits first.
fn rar_vint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Whether a RAR archive is password-protected: its headers (RAR 5's archive encryption
/// header, RAR 4's encrypted-headers flag) or any file in it (RAR 5's encryption record, RAR
/// 4's password flag). Only headers are read; data is skipped over.
fn rar_encrypted(path: &Path) -> io::Result<bool> {
    const BLOCKS: usize = 1 << 20;
    let mut file = BufReader::new(File::open(path)?);
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature[..7])?;
    if signature[..7] == *b"Rar!\x1a\x07\x00" {
        // RAR 4: blocks of CRC (2), type (1), flags (2), size (2), and for files a packed size.
        for _ in 0..BLOCKS {
            let mut head = [0u8; 7];
            if file.read_exact(&mut head).is_err() {
                return Ok(false);
            }
            let (kind, flags, size) = (head[2], u16::from_le_bytes([head[3], head[4]]), u16::from_le_bytes([head[5], head[6]]) as u64);
            let mut rest = vec![0u8; (size as usize).saturating_sub(7)];
            file.read_exact(&mut rest)?;
            match kind {
                0x73 if flags & 0x0080 != 0 => return Ok(true),
                0x74 if flags & 0x0004 != 0 => return Ok(true),
                0x7b => return Ok(false),
                _ => {}
            }
            let long = flags & 0x8000 != 0 || kind == 0x74;
            if long && rest.len() >= 4 {
                let mut skip = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as u64;
                if kind == 0x74 && flags & 0x0100 != 0 && rest.len() >= 29 {
                    skip |= (u32::from_le_bytes([rest[25], rest[26], rest[27], rest[28]]) as u64) << 32;
                }
                file.seek_relative(skip as i64)?;
            }
        }
        return Ok(false);
    }
    file.read_exact(&mut signature[7..])?;
    if signature != *b"Rar!\x1a\x07\x01\x00" {
        return Ok(false);
    }
    // RAR 5: blocks of CRC (4), header size, then type, flags, extra size, data size.
    for _ in 0..BLOCKS {
        let mut crc = [0u8; 4];
        if file.read_exact(&mut crc).is_err() {
            return Ok(false);
        }
        let mut size_bytes = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            file.read_exact(&mut byte)?;
            size_bytes.push(byte[0]);
            if byte[0] & 0x80 == 0 || size_bytes.len() >= 3 {
                break;
            }
        }
        let size = rar_vint(&size_bytes, &mut 0).unwrap_or(0) as usize;
        let mut body = vec![0u8; size.min(2 << 20)];
        file.read_exact(&mut body)?;
        let mut at = 0;
        let (Some(kind), Some(flags)) = (rar_vint(&body, &mut at), rar_vint(&body, &mut at)) else { return Ok(false) };
        let extra = if flags & 0x1 != 0 { rar_vint(&body, &mut at).unwrap_or(0) as usize } else { 0 };
        let data = if flags & 0x2 != 0 { rar_vint(&body, &mut at).unwrap_or(0) } else { 0 };
        match kind {
            4 => return Ok(true),
            5 => return Ok(false),
            2 | 3 if extra > 0 && extra <= body.len() => {
                let records = &body[body.len() - extra..];
                let mut at = 0;
                while at < records.len() {
                    let Some(length) = rar_vint(records, &mut at) else { break };
                    let start = at;
                    if rar_vint(records, &mut at) == Some(0x01) {
                        return Ok(true);
                    }
                    at = start + length as usize;
                }
            }
            _ => {}
        }
        file.seek_relative(data as i64)?;
    }
    Ok(false)
}

struct Opened {
    reader: Box<dyn Read>,
    converter: Option<Converter>,
}

impl Opener {
    fn open(&self) -> Result<Opened, Failure> {
        match self {
            Opener::Tar(path, codec) => {
                let file = BufReader::new(open_file(path)?);
                Ok(Opened { reader: decoder(file, *codec), converter: None })
            }
            Opener::Rar(path) => {
                let (reader, converter) = Converter::start(path)?;
                Ok(Opened { reader, converter: Some(converter) })
            }
        }
    }
}

/// A stream decompressed by `codec`.
fn decoder(file: BufReader<File>, codec: Codec) -> Box<dyn Read> {
    match codec {
        Codec::Plain => Box::new(file),
        Codec::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        Codec::Xz => Box::new(lzma_rust2::XzReader::new(file, true)),
    }
}

/// Handles one entry of a tar stream, by its position.
type EachEntry<'e> = dyn FnMut(usize, &mut tar::Entry<'_, Counted<'_, Box<dyn Read>>>) -> Result<(), Failure> + 'e;

struct Stream<'c> {
    opener: Opener,
    budget: u64,
    cancelled: &'c dyn Fn() -> bool,
}

impl<'c> Stream<'c> {
    fn new(opener: Opener, budget: u64, cancelled: &'c dyn Fn() -> bool) -> Self {
        Stream { opener, budget, cancelled }
    }

    /// Goes through the stream from the start, handing each entry to `each` with its position,
    /// up to the entry at `until` if given, or to the end.
    fn walk(&self, until: Option<usize>, each: &mut EachEntry<'_>) -> Result<(), Failure> {
        let opened = self.opener.open()?;
        let stop: Stop = Rc::default();
        let mut archive = tar::Archive::new(Counted::new(opened.reader, self.budget, self.cancelled, stop.clone()));
        let entries = archive.entries().map_err(|error| failure_of(&stop, &error))?;
        for (index, entry) in entries.enumerate() {
            let mut entry = entry.map_err(|error| failure_of(&stop, &error))?;
            each(index, &mut entry).map_err(|failure| explain(&stop, failure))?;
            if until == Some(index) {
                // What was wanted has been read; the rest is never decompressed (tar.exe, for
                // a RAR, is stopped when the converter is dropped).
                return Ok(());
            }
        }
        drop(archive);
        match opened.converter {
            Some(converter) => converter.finish(),
            None => Ok(()),
        }
    }
}

fn tar_entry<R: Read>(entry: &tar::Entry<'_, R>) -> Entry {
    use tar::EntryType;
    let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
    let header = entry.header();
    let kind = match header.entry_type() {
        EntryType::Directory => Kind::Dir,
        // Old archives mark folders only by a trailing slash.
        EntryType::Regular | EntryType::Continuous if name.ends_with('/') => Kind::Dir,
        EntryType::Regular | EntryType::Continuous => Kind::File,
        EntryType::Symlink => Kind::Symlink,
        EntryType::Link => Kind::HardLink(entry.link_name_bytes().map(|link| String::from_utf8_lossy(&link).into_owned()).unwrap_or_default()),
        _ => Kind::Other,
    };
    Entry {
        name,
        kind,
        size: entry.size(),
        mtime: header.mtime().map_or(0, |seconds| time::from_unix(seconds as i64)),
        attrs: 0,
        encrypted: false,
    }
}

impl Source for Stream<'_> {
    fn entries(&mut self) -> Result<Vec<Entry>, Failure> {
        let mut list = Vec::new();
        let cancelled = self.cancelled;
        self.walk(None, &mut |_, entry| {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            list.push(tar_entry(entry));
            Ok(())
        })?;
        Ok(list)
    }

    fn read(&mut self, wanted: &dyn Fn(usize) -> bool, last: usize, visit: &mut Visit<'_>) -> Result<(), Failure> {
        self.walk(Some(last), &mut |index, entry| if wanted(index) { visit(index, entry) } else { Ok(()) })
    }
}

// ---------- one compressed file ----------

struct Single<'c> {
    path: PathBuf,
    codec: Codec,
    name: String,
    size: u64,
    mtime: i64,
    budget: u64,
    cancelled: &'c dyn Fn() -> bool,
}

impl<'c> Single<'c> {
    fn open(path: PathBuf, codec: Codec, budget: u64, cancelled: &'c dyn Fn() -> bool) -> Result<Self, Failure> {
        let mut file = open_file(&path)?;
        let meta = file.metadata().map_err(|error| corrupt(&error))?;
        // A gzip stream ends with its size modulo 4 GiB: right for anything smaller.
        let mut size = 0;
        if codec == Codec::Gzip && meta.len() >= 18 && file.seek(SeekFrom::End(-4)).is_ok() {
            let mut tail = [0u8; 4];
            if file.read_exact(&mut tail).is_ok() {
                size = u32::from_le_bytes(tail) as u64;
            }
        }
        let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file").to_owned();
        let name = super::address::Format::stem(&file_name).to_owned();
        let mtime = meta.modified().map_or(0, time::from_system);
        Ok(Single { path, codec, name, size, mtime, budget, cancelled })
    }
}

impl Source for Single<'_> {
    fn entries(&mut self) -> Result<Vec<Entry>, Failure> {
        Ok(vec![Entry { name: self.name.clone(), kind: Kind::File, size: self.size, mtime: self.mtime, attrs: 0, encrypted: false }])
    }

    fn read(&mut self, wanted: &dyn Fn(usize) -> bool, _last: usize, visit: &mut Visit<'_>) -> Result<(), Failure> {
        if !wanted(0) {
            return Ok(());
        }
        let stop: Stop = Rc::default();
        let file = BufReader::new(open_file(&self.path)?);
        let mut data = Counted::new(decoder(file, self.codec), self.budget, self.cancelled, stop.clone());
        visit(0, &mut data).map_err(|failure| explain(&stop, failure))
    }

    /// Only gzip says how large its file is, and only modulo 4 GiB.
    fn declares_sizes(&self) -> bool {
        false
    }
}
