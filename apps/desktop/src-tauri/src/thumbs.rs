//! Thumbnails for the icon and tile views, served to the web view by the `thumb` protocol as
//! `http://thumb.localhost/?p=<path>&s=<pixels>&m=<mtime>`.
//!
//! The shell renders them (`IShellItemImageFactory::GetImage`, thumbnails only) on a small pool
//! of single-threaded-apartment threads, which is what thumbnail handlers expect. Each one is
//! encoded as PNG and kept twice: on disk under the data folder, keyed by path, size and
//! modification time and capped at about a gigabyte (oldest out first), and in memory for the
//! ones seen most recently. The URL carries the modification time, so a changed file gets a
//! new URL and every answer can be cached by the web view for as long as it likes.
//!
//! Files without a thumbnail are answered with their icon. So are cloud-only files (OneDrive
//! placeholders whose contents are not on this computer): a thumbnail handler would read them,
//! and reading one downloads it. Such a stand-in can change without the file's modification
//! time changing (the file is downloaded, or a render that failed because the file was locked
//! would now succeed), so it is remembered only briefly and never cached by the web view.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::mem::zeroed;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use kestrel_index::fold;
use parking_lot::{Condvar, Mutex};
use tauri::UriSchemeResponder;
use tauri::http::{Request, Response, StatusCode, Uri};
use windows::Win32::Foundation::SIZE;
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY};
use windows::core::HSTRING;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
    FILE_ATTRIBUTE_RECALL_ON_OPEN, GetFileAttributesExW, GetFileExInfoStandard,
    WIN32_FILE_ATTRIBUTE_DATA,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

use crate::icons::{IconKey, IconService, encode_png};
use crate::indexer;
use crate::lru::Lru;

/// Rendering threads. Thumbnail handlers can be slow (video frames, large photos), so a few run
/// side by side, but not so many that scrolling a folder of photos saturates the machine.
const WORKERS: usize = 3;
/// Requests waiting for a worker. Scrolling through a large folder asks for far more than
/// will ever be looked at; beyond this the oldest requests, long scrolled past, are dropped.
const MAX_QUEUED: usize = 512;
const DISK_CAP: u64 = 1 << 30;
/// Eviction brings the disk cache down to this share of its cap, so it does not run for every
/// thumbnail written once the cache is full.
const DISK_TARGET_PERCENT: u64 = 90;
const MEMORY_CAP: usize = 64 << 20;
/// How long a stand-in icon answers for a file before the shell is asked again. Long enough
/// that scrolling back and forth over files without thumbnails asks the shell little, short
/// enough that a file just downloaded from the cloud soon shows its thumbnail.
const STAND_IN_LIFETIME: Duration = Duration::from_secs(15);
/// Thumbnail sizes a request may ask for, in pixels.
const MIN_SIZE: u32 = 16;
const MAX_SIZE: u32 = 1024;
/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Attributes of a file whose contents live in the cloud: opening it downloads it.
const CLOUD_ONLY: u32 =
    FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
/// Extensions whose files carry their own icon, as listed by the front end's `iconUrl`.
const OWN_ICON: [&str; 9] = ["exe", "lnk", "ico", "url", "msi", "scr", "cur", "ani", "appref-ms"];

/// Identifies one thumbnail: a file as it was at one modification time, at one size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    /// Folded, so the same file reached with different capitalisation shares a thumbnail.
    path: String,
    size: u32,
    /// Milliseconds since the Unix epoch, as the rows sent to the front end carry it.
    mtime: i64,
}

impl Key {
    pub fn new(path: &str, size: u32, mtime: i64) -> Self {
        Self { path: fold(&path.replace('/', "\\")), size, mtime }
    }

    /// Where the thumbnail is kept, relative to the cache folder: a 128-bit hash of the key,
    /// spread over 256 subfolders so that none grows huge.
    fn file_name(&self) -> PathBuf {
        let mut hash = Fnv128::default();
        hash.write(self.path.as_bytes());
        // Paths never contain NUL, so this separates the path from the numbers unambiguously.
        hash.write(&[0]);
        hash.write(&self.size.to_le_bytes());
        hash.write(&self.mtime.to_le_bytes());
        let hex = format!("{:032x}", hash.0);
        Path::new(&hex[..2]).join(format!("{hex}.png"))
    }
}

/// FNV-1a with a 128-bit state. Unlike the standard library's hasher it is the same in every
/// build, which names stored on disk need.
struct Fnv128(u128);

impl Default for Fnv128 {
    fn default() -> Self {
        Self(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d)
    }
}

impl Fnv128 {
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= byte as u128;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
}

/// The answer to a thumbnail request.
#[derive(Clone)]
enum Thumb {
    Png(Arc<Vec<u8>>),
    /// The file has no thumbnail, or must not be read for one; this icon stands in.
    Icon(IconKey),
    Missing,
    /// Dropped from the queue to make room for newer requests.
    Dropped,
}

impl Thumb {
    /// Approximate memory held, for the memory cache's budget.
    fn cost(&self) -> usize {
        const OVERHEAD: usize = 64;
        match self {
            Thumb::Png(png) => png.len() + OVERHEAD,
            _ => OVERHEAD,
        }
    }

    /// Whether the answer stays right for as long as the file keeps its modification time,
    /// which the URL names. Only a rendered thumbnail does; see the module's documentation.
    fn lasting(&self) -> bool {
        matches!(self, Thumb::Png(_))
    }
}

/// The answers given most recently, up to a total size. Remembering that a file has no
/// thumbnail matters as much as remembering one that does: it saves asking the shell again.
/// But a stand-in is remembered only for `stand_in_lifetime`.
struct MemoryCache {
    lru: Lru<Key, (Thumb, Instant)>,
    stand_in_lifetime: Duration,
}

impl MemoryCache {
    fn new(cap: usize, stand_in_lifetime: Duration) -> Self {
        Self { lru: Lru::new(cap), stand_in_lifetime }
    }

    fn get(&mut self, key: &Key) -> Option<Thumb> {
        let (thumb, found) = self.lru.get(key)?;
        if !thumb.lasting() && found.elapsed() >= self.stand_in_lifetime {
            self.lru.remove(key);
            return None;
        }
        Some(thumb)
    }

    fn put(&mut self, key: Key, thumb: Thumb) {
        let cost = thumb.cost();
        self.lru.put(key, (thumb, Instant::now()), cost);
    }
}

/// A file in the disk cache.
struct CacheFile {
    path: PathBuf,
    bytes: u64,
    written: SystemTime,
}

/// The files to delete, oldest first, to bring the cache's total down to `target` bytes, and
/// the total that remains afterwards.
fn plan_eviction(mut files: Vec<CacheFile>, target: u64) -> (Vec<PathBuf>, u64) {
    let mut total: u64 = files.iter().map(|file| file.bytes).sum();
    files.sort_by_key(|file| file.written);
    let mut doomed = Vec::new();
    for file in files {
        if total <= target {
            break;
        }
        total -= file.bytes;
        doomed.push(file.path);
    }
    (doomed, total)
}

/// Thumbnails saved under the data folder, capped in total size.
struct DiskCache {
    dir: PathBuf,
    cap: u64,
    /// Bytes in the cache, counted the first time something is written.
    used: Mutex<Option<u64>>,
}

impl DiskCache {
    fn new(dir: PathBuf, cap: u64) -> Self {
        Self { dir, cap, used: Mutex::new(None) }
    }

    fn get(&self, key: &Key) -> Option<Vec<u8>> {
        fs::read(self.dir.join(key.file_name())).ok()
    }

    fn put(&self, key: &Key, png: &[u8]) {
        static PARTS: AtomicU64 = AtomicU64::new(0);
        let path = self.dir.join(key.file_name());
        let Some(folder) = path.parent() else { return };
        let mut used = self.used.lock();
        let total = used.get_or_insert_with(|| self.files().iter().map(|file| file.bytes).sum());
        // Written under a temporary name and then renamed, so no reader sees half a file.
        let part = path.with_extension(format!("{}.part", PARTS.fetch_add(1, Ordering::Relaxed)));
        let written = fs::create_dir_all(folder).is_ok() && fs::write(&part, png).is_ok();
        if !written || fs::rename(&part, &path).is_err() {
            let _ = fs::remove_file(&part);
            return;
        }
        *total += png.len() as u64;
        if *total > self.cap {
            let (doomed, left) = plan_eviction(self.files(), self.cap / 100 * DISK_TARGET_PERCENT);
            for file in doomed {
                let _ = fs::remove_file(file);
            }
            *total = left;
        }
    }

    /// Every thumbnail in the cache.
    fn files(&self) -> Vec<CacheFile> {
        let mut files = Vec::new();
        let Ok(folders) = fs::read_dir(&self.dir) else { return files };
        for folder in folders.flatten() {
            let Ok(entries) = fs::read_dir(folder.path()) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "png") {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    let written = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    files.push(CacheFile { path, bytes: meta.len(), written });
                }
            }
        }
        files
    }
}

/// A file's attributes and modification time, read without opening it.
#[derive(Clone, Copy)]
struct FileState {
    attrs: u32,
    mtime: i64,
}

impl FileState {
    fn read(path: &str) -> Option<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        // SAFETY: WIN32_FILE_ATTRIBUTE_DATA is plain data; all-zero is a valid value.
        let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { zeroed() };
        // SAFETY: `wide` is NUL-terminated and `data` is the structure this information level
        // fills. Only the directory entry is read, which never recalls a cloud file.
        let found =
            unsafe { GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&raw mut data).cast()) } != 0;
        let written = &data.ftLastWriteTime;
        let filetime = (written.dwHighDateTime as i64) << 32 | written.dwLowDateTime as i64;
        found.then_some(Self { attrs: data.dwFileAttributes, mtime: (filetime - FILETIME_UNIX_EPOCH) / 10_000 })
    }

    fn is_dir(self) -> bool {
        self.attrs & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    fn is_cloud_only(self) -> bool {
        self.attrs & CLOUD_ONLY != 0
    }
}

/// The lower-case extension of a file name, or `""`.
fn extension(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The icon that stands in for a file's thumbnail. Only files known to be on this computer
/// may be asked for an icon of their own, since extracting one reads the file.
fn stand_in_icon(path: &str, on_disk: bool) -> IconKey {
    let ext = extension(path);
    if on_disk && OWN_ICON.contains(&ext.as_str()) {
        return IconKey::File(path.to_owned());
    }
    // The front end asks for files without an extension the same way.
    IconKey::Ext(if ext.is_empty() { "_".to_owned() } else { ext })
}

/// Converts the shell's pixels, BGRA either premultiplied by alpha or with no alpha at all, to
/// the straight RGBA that PNG stores.
fn straight_rgba(mut pixels: Vec<u8>) -> Vec<u8> {
    // Thumbnails of opaque images arrive with the alpha channel left at zero.
    let opaque = pixels.chunks_exact(4).all(|p| p[3] == 0);
    // Premultiplied colours never exceed their alpha; anything else is already straight.
    let premultiplied = !opaque && pixels.chunks_exact(4).all(|p| p[0].max(p[1]).max(p[2]) <= p[3]);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        if opaque {
            pixel[3] = 255;
        } else if premultiplied && pixel[3] > 0 && pixel[3] < 255 {
            let alpha = pixel[3] as u32;
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    pixels
}

/// Reads a bitmap's pixels as top-down BGRA.
///
/// # Safety
/// `bitmap` must be a valid bitmap handle not selected into a device context.
unsafe fn bitmap_pixels(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: the caller guarantees the handle; every buffer passed is sized for the call.
    unsafe {
        let mut info: BITMAP = zeroed();
        if GetObjectW(bitmap, size_of::<BITMAP>() as i32, (&raw mut info).cast()) == 0 {
            return None;
        }
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut header: BITMAPINFO = zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height requests top-down rows.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..zeroed()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let dc = GetDC(null_mut());
        let lines = GetDIBits(dc, bitmap, 0, height as u32, pixels.as_mut_ptr().cast(), &mut header, DIB_RGB_COLORS);
        ReleaseDC(null_mut(), dc);
        (lines != 0).then_some((width as u32, height as u32, pixels))
    }
}

/// Asks the shell for a thumbnail that fits in `size` pixels square, as PNG, or `None` if the
/// file has none.
///
/// # Safety
/// Call on a thread where COM is initialised as a single-threaded apartment.
unsafe fn render(path: &str, size: u32) -> Option<Vec<u8>> {
    let side = size as i32;
    // SAFETY: the path outlives the calls that use it; the factory is released when dropped and
    // the bitmap, which the caller of GetImage owns, is deleted below.
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let bitmap = factory.GetImage(SIZE { cx: side, cy: side }, SIIGBF_THUMBNAILONLY).ok()?;
        let pixels = bitmap_pixels(bitmap.0);
        DeleteObject(bitmap.0);
        let (width, height, bgra) = pixels?;
        encode_png(width, height, &straight_rgba(bgra))
    }
}

/// Answers one request, from the disk cache or by rendering.
///
/// # Safety
/// Call on a thread where COM is initialised as a single-threaded apartment.
unsafe fn produce(disk: &DiskCache, key: &Key, path: &str) -> Thumb {
    if let Some(png) = disk.get(key) {
        return Thumb::Png(Arc::new(png));
    }
    let Some(file) = FileState::read(path) else { return Thumb::Missing };
    if file.is_dir() {
        return Thumb::Icon(IconKey::Folder);
    }
    if file.is_cloud_only() {
        return Thumb::Icon(stand_in_icon(path, false));
    }
    // SAFETY: the caller guarantees the apartment.
    match unsafe { render(path, key.size) } {
        Some(png) => {
            // A file changed since it was listed is shown as it is now, but not filed under
            // the time it was listed with.
            if file.mtime == key.mtime {
                disk.put(key, &png);
            }
            Thumb::Png(Arc::new(png))
        }
        None => Thumb::Icon(stand_in_icon(path, true)),
    }
}

type Reply = Box<dyn FnOnce(Thumb) + Send>;

struct Job {
    path: String,
    replies: Vec<Reply>,
}

#[derive(Default)]
struct Queue {
    /// Keys waiting for a worker, oldest first. Workers take the newest, which is what is on
    /// screen now.
    waiting: VecDeque<Key>,
    /// Every key waiting or being rendered, with everyone waiting for it, so that one file
    /// asked for twice is rendered once.
    jobs: HashMap<Key, Job>,
}

struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
    memory: Mutex<MemoryCache>,
    disk: DiskCache,
}

fn work(shared: &Shared) {
    // SAFETY: initialises COM for this thread only; the thread lives for the process.
    unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED as u32) };
    loop {
        let (key, path) = {
            let mut queue = shared.queue.lock();
            loop {
                if let Some(key) = queue.waiting.pop_back() {
                    let path = queue.jobs.get(&key).map(|job| job.path.clone()).unwrap_or_default();
                    break (key, path);
                }
                shared.work.wait(&mut queue);
            }
        };
        // SAFETY: COM was initialised as a single-threaded apartment above.
        let thumb = unsafe { produce(&shared.disk, &key, &path) };
        if !matches!(thumb, Thumb::Missing) {
            shared.memory.lock().put(key.clone(), thumb.clone());
        }
        let replies = shared.queue.lock().jobs.remove(&key).map(|job| job.replies).unwrap_or_default();
        for reply in replies {
            reply(thumb.clone());
        }
    }
}

/// A thumbnail request: `/?p=<path>&s=<pixels>&m=<mtime>`.
#[derive(Debug, PartialEq, Eq)]
pub struct Wanted {
    pub path: String,
    pub size: u32,
    pub mtime: i64,
}

pub fn parse(uri: &Uri) -> Option<Wanted> {
    let (mut path, mut size, mut mtime) = (None, None, 0);
    for pair in uri.query().unwrap_or_default().split('&') {
        match pair.split_once('=') {
            Some(("p", value)) => path = Some(crate::percent_decode(value)),
            Some(("s", value)) => size = Some(value.parse().ok()?),
            Some(("m", value)) => mtime = value.parse().ok()?,
            _ => {}
        }
    }
    let size = size.filter(|size| (MIN_SIZE..=MAX_SIZE).contains(size))?;
    let path = path.filter(|path| !path.is_empty())?;
    Some(Wanted { path, size, mtime })
}

/// An answer to the web view. `lasting` says it holds for this URL for good: the URL names the
/// file's modification time, so a rendered thumbnail never goes stale and may be cached for a
/// year. A stand-in or a failure may change at any time, so the web view keeps neither.
fn response(status: StatusCode, body: Vec<u8>, lasting: bool) -> Response<Vec<u8>> {
    let caching = if status == StatusCode::OK && lasting { "max-age=31536000, immutable" } else { "no-store" };
    Response::builder()
        .status(status)
        .header("Content-Type", "image/png")
        .header("Cache-Control", caching)
        .body(body)
        .expect("static response parts are valid")
}

fn respond(responder: UriSchemeResponder, status: StatusCode, body: Vec<u8>, lasting: bool) {
    responder.respond(response(status, body, lasting));
}

pub struct ThumbService {
    shared: Arc<Shared>,
    icons: Arc<IconService>,
}

impl ThumbService {
    /// Starts the rendering threads. `icons` provides the icons that stand in for thumbnails.
    pub fn start(icons: Arc<IconService>) -> Self {
        Self::with(icons, indexer::data_dir().join("thumbs"), STAND_IN_LIFETIME)
    }

    /// Starts the rendering threads with the disk cache in `cache_dir`, remembering stand-ins
    /// for `stand_in_lifetime`.
    fn with(icons: Arc<IconService>, cache_dir: PathBuf, stand_in_lifetime: Duration) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            work: Condvar::new(),
            memory: Mutex::new(MemoryCache::new(MEMORY_CAP, stand_in_lifetime)),
            disk: DiskCache::new(cache_dir, DISK_CAP),
        });
        for n in 0..WORKERS {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(format!("kestrel-thumbs-{n}"))
                .spawn(move || work(&shared))
                .expect("spawning a thumbnail thread");
        }
        Self { shared, icons }
    }

    /// Answers a `thumb://` request.
    pub fn handle(&self, request: &Request<Vec<u8>>, responder: UriSchemeResponder) {
        let Some(wanted) = parse(request.uri()) else {
            return respond(responder, StatusCode::BAD_REQUEST, Vec::new(), false);
        };
        let size = wanted.size;
        let icons = self.icons.clone();
        self.request(wanted, move |thumb| match thumb {
            Thumb::Png(png) => respond(responder, StatusCode::OK, png.to_vec(), true),
            Thumb::Icon(key) => icons.request(key, size, move |png| match png {
                Some(png) => respond(responder, StatusCode::OK, png.to_vec(), false),
                None => respond(responder, StatusCode::NOT_FOUND, Vec::new(), false),
            }),
            Thumb::Missing => respond(responder, StatusCode::NOT_FOUND, Vec::new(), false),
            Thumb::Dropped => respond(responder, StatusCode::SERVICE_UNAVAILABLE, Vec::new(), false),
        });
    }

    fn request(&self, wanted: Wanted, reply: impl FnOnce(Thumb) + Send + 'static) {
        let key = Key::new(&wanted.path, wanted.size, wanted.mtime);
        let cached = self.shared.memory.lock().get(&key);
        if let Some(thumb) = cached {
            return reply(thumb);
        }
        let mut queue = self.shared.queue.lock();
        if let Some(job) = queue.jobs.get_mut(&key) {
            job.replies.push(Box::new(reply));
            return;
        }
        queue.jobs.insert(key.clone(), Job { path: wanted.path, replies: vec![Box::new(reply)] });
        queue.waiting.push_back(key);
        let dropped = if queue.waiting.len() > MAX_QUEUED {
            queue.waiting.pop_front().and_then(|oldest| queue.jobs.remove(&oldest))
        } else {
            None
        };
        drop(queue);
        self.shared.work.notify_one();
        for reply in dropped.into_iter().flat_map(|job| job.replies) {
            reply(Thumb::Dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn keys_name_a_file_at_one_size_and_time() {
        let key = Key::new(r"C:\Photos\Beach.PNG", 128, 1_700_000_000_000);
        let name = key.file_name();
        let text = name.to_str().unwrap();
        assert_eq!(text.len(), 2 + 1 + 32 + 4, "{text}");
        assert!(text.ends_with(".png"));
        assert_eq!(&text[..2], &text[3..5], "files are grouped by the first two digits of their hash");

        // Capitalisation and slash direction do not matter; size, time and path do.
        assert_eq!(Key::new("c:/photos/beach.png", 128, 1_700_000_000_000).file_name(), name);
        assert_ne!(Key::new(r"C:\Photos\Beach.PNG", 256, 1_700_000_000_000).file_name(), name);
        assert_ne!(Key::new(r"C:\Photos\Beach.PNG", 128, 1_700_000_000_001).file_name(), name);
        assert_ne!(Key::new(r"C:\Photos\Beach2.PNG", 128, 1_700_000_000_000).file_name(), name);
        // The size and time are not confused with the end of the path.
        assert_ne!(Key::new("a1", 2, 3).file_name(), Key::new("a", 12, 3).file_name());
    }

    #[test]
    fn requests_are_parsed_from_the_url() {
        let uri: Uri = "http://thumb.localhost/?p=C%3A%5CPhotos%5Cmy%20cat.jpg&s=256&m=1700000000123".parse().unwrap();
        let wanted = parse(&uri).unwrap();
        assert_eq!(wanted, Wanted { path: r"C:\Photos\my cat.jpg".into(), size: 256, mtime: 1_700_000_000_123 });
        for bad in ["/?p=C%3A%5Cx.png", "/?s=64", "/?p=x&s=4", "/?p=x&s=4096", "/?p=x&s=64&m=soon", "/?p=&s=64"] {
            let uri: Uri = format!("http://thumb.localhost{bad}").parse().unwrap();
            assert!(parse(&uri).is_none(), "{bad}");
        }
    }

    fn png(bytes: usize) -> Thumb {
        Thumb::Png(Arc::new(vec![0; bytes]))
    }

    #[test]
    fn the_memory_cache_keeps_the_most_recently_used() {
        let key = |n: i64| Key::new(r"C:\a.png", 64, n);
        let mut cache = MemoryCache::new(3 * png(100).cost(), Duration::from_secs(3600));
        cache.put(key(1), png(100));
        cache.put(key(2), png(100));
        cache.put(key(3), png(100));
        assert!(cache.get(&key(1)).is_some(), "using an entry makes it recent");
        cache.put(key(4), png(100));
        assert!(cache.get(&key(2)).is_none(), "the least recently used entry goes first");
        for n in [1, 3, 4] {
            assert!(cache.get(&key(n)).is_some(), "{n}");
        }
        // Replacing an entry does not count it twice, and icons are cheap to remember.
        cache.put(key(4), png(100));
        cache.put(key(5), Thumb::Icon(IconKey::Ext("txt".into())));
        assert_eq!(cache.lru.used(), 3 * png(100).cost() - png(100).cost() + Thumb::Icon(IconKey::Folder).cost());
        assert!(cache.get(&key(1)).is_none());
    }

    #[test]
    fn stand_ins_are_remembered_only_briefly() {
        let key = |n: i64| Key::new(r"C:\a.png", 64, n);
        let stand_in = || Thumb::Icon(IconKey::Ext("png".into()));
        let mut briefly = MemoryCache::new(1 << 20, Duration::ZERO);
        briefly.put(key(1), stand_in());
        briefly.put(key(2), png(100));
        assert!(briefly.get(&key(1)).is_none(), "a stand-in past its time is asked for again");
        assert!(briefly.get(&key(2)).is_some(), "a thumbnail is kept for as long as there is room");
        let mut longer = MemoryCache::new(1 << 20, Duration::from_secs(3600));
        longer.put(key(1), stand_in());
        assert!(matches!(longer.get(&key(1)), Some(Thumb::Icon(_))), "within its time, a stand-in answers");
    }

    #[test]
    fn only_rendered_thumbnails_are_cached_by_the_web_view() {
        let caching = |response: Response<Vec<u8>>| response.headers()["cache-control"].to_str().unwrap().to_owned();
        assert_eq!(caching(response(StatusCode::OK, vec![1], true)), "max-age=31536000, immutable");
        assert_eq!(caching(response(StatusCode::OK, vec![1], false)), "no-store", "a stand-in may change");
        assert_eq!(caching(response(StatusCode::NOT_FOUND, Vec::new(), true)), "no-store");
        assert!(png(1).lasting() && !Thumb::Icon(IconKey::Folder).lasting());
    }

    fn set_attributes(path: &Path, attributes: u32) {
        let wide: Vec<u16> = path.to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        assert_ne!(unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attributes) }, 0);
    }

    #[test]
    fn a_file_downloaded_from_the_cloud_gets_its_thumbnail() {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE};
        let dir = scratch("downloaded");
        let photo = dir.join("photo.png");
        write_png(&photo, 120, 80);
        // A cloud placeholder whose contents are elsewhere carries this attribute; setting it on
        // a local file changes nothing but the flag.
        set_attributes(&photo, FILE_ATTRIBUTE_OFFLINE);
        let path = photo.to_str().unwrap().to_owned();
        let mtime = FileState::read(&path).unwrap().mtime;
        // Stand-ins expire at once here, so the second request is not answered from memory.
        let service = ThumbService::with(Arc::new(IconService::start()), dir.join("cache"), Duration::ZERO);
        let ask = || {
            let (sender, answer) = std::sync::mpsc::channel();
            service.request(Wanted { path: path.clone(), size: 64, mtime }, move |thumb| {
                let _ = sender.send(thumb);
            });
            answer.recv_timeout(Duration::from_secs(60)).expect("an answer comes")
        };
        assert!(matches!(ask(), Thumb::Icon(IconKey::Ext(ext)) if ext == "png"), "an online-only file is not read");

        // The file is downloaded: its attributes change, its modification time does not, so
        // the web view asks for the same URL again.
        set_attributes(&photo, FILE_ATTRIBUTE_NORMAL);
        assert_eq!(FileState::read(&path).unwrap().mtime, mtime);
        assert!(matches!(ask(), Thumb::Png(_)), "the thumbnail replaces the stand-in");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eviction_removes_the_oldest_files_first() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let file = |name: &str, bytes, secs| CacheFile { path: PathBuf::from(name), bytes, written: at(secs) };
        let files = vec![file("new", 400, 30), file("old", 300, 10), file("mid", 200, 20)];
        let (doomed, left) = plan_eviction(files, 500);
        assert_eq!(doomed, [PathBuf::from("old"), PathBuf::from("mid")]);
        assert_eq!(left, 400);
        let (doomed, left) = plan_eviction(vec![file("a", 100, 1)], 500);
        assert!(doomed.is_empty());
        assert_eq!(left, 100);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-thumbs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_disk_cache_stays_under_its_cap() {
        let dir = scratch("disk");
        let cache = DiskCache::new(dir.clone(), 1000);
        let key = |n: i64| Key::new(r"C:\Photos\a.png", 64, n);
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        for n in 0..3 {
            cache.put(&key(n), &[n as u8; 300]);
            // Written in order, a second apart, whatever the clock's resolution.
            let file = fs::File::options().write(true).open(dir.join(key(n).file_name())).unwrap();
            file.set_modified(an_hour_ago + Duration::from_secs(n as u64)).unwrap();
        }
        assert_eq!(cache.get(&key(1)).unwrap(), [1; 300]);

        // 1200 bytes is over the cap, so the oldest goes, leaving 900 (90% of the cap).
        cache.put(&key(3), &[3; 300]);
        assert!(cache.get(&key(0)).is_none());
        for n in 1..4 {
            assert!(cache.get(&key(n)).is_some(), "{n}");
        }
        assert_eq!(*cache.used.lock(), Some(900));
        assert_eq!(cache.files().len(), 3);

        // A new cache over the same folder counts what is already there.
        let reopened = DiskCache::new(dir.clone(), 1000);
        reopened.put(&key(4), &[4; 100]);
        assert_eq!(*reopened.used.lock(), Some(1000));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shell_pixels_become_straight_rgba() {
        // No alpha channel at all: opaque.
        assert_eq!(straight_rgba(vec![10, 20, 30, 0, 1, 2, 3, 0]), [30, 20, 10, 255, 3, 2, 1, 255]);
        // Premultiplied: half-transparent colours are scaled back up.
        assert_eq!(straight_rgba(vec![100, 50, 0, 128, 0, 0, 0, 0, 9, 9, 9, 255]), [0, 100, 199, 128, 0, 0, 0, 0, 9, 9, 9, 255]);
        // Colours brighter than their alpha cannot be premultiplied, so they are left alone.
        assert_eq!(straight_rgba(vec![200, 50, 0, 128]), [0, 50, 200, 128]);
    }

    #[test]
    fn stand_in_icons_never_read_cloud_files() {
        assert!(matches!(stand_in_icon(r"C:\a\setup.EXE", true), IconKey::File(path) if path == r"C:\a\setup.EXE"));
        assert!(matches!(stand_in_icon(r"C:\a\setup.exe", false), IconKey::Ext(ext) if ext == "exe"));
        assert!(matches!(stand_in_icon(r"C:\a\notes.Txt", true), IconKey::Ext(ext) if ext == "txt"));
        assert!(matches!(stand_in_icon(r"C:\a.b\README", true), IconKey::Ext(ext) if ext == "_"));
        assert!(matches!(stand_in_icon(r"C:\a\.gitignore", true), IconKey::Ext(ext) if ext == "_"));
        let cloud = FileState { attrs: FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, mtime: 0 };
        assert!(cloud.is_cloud_only());
        assert!(!FileState { attrs: 0x20, mtime: 0 }.is_cloud_only());
    }

    /// Writes a small two-colour PNG.
    fn write_png(path: &Path, width: u32, height: u32) {
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| if i % width < width / 2 { [200, 40, 40, 255] } else { [40, 40, 200, 255] })
            .collect();
        fs::write(path, encode_png(width, height, &rgba).unwrap()).unwrap();
    }

    #[test]
    fn renders_thumbnails_and_falls_back_to_icons() {
        let dir = scratch("render");
        let cache = DiskCache::new(dir.join("cache"), 1 << 20);
        let photo = dir.join("photo.png");
        write_png(&photo, 120, 80);
        fs::write(dir.join("notes.txt"), b"no thumbnail here").unwrap();
        let path = |name: &str| dir.join(name).to_str().unwrap().to_owned();
        let mtime = FileState::read(&path("photo.png")).unwrap().mtime;

        // SAFETY: initialises COM on this test's own thread.
        unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED as u32) };
        let key = Key::new(&path("photo.png"), 64, mtime);
        // SAFETY: COM is initialised as a single-threaded apartment on this thread.
        let Thumb::Png(png) = (unsafe { produce(&cache, &key, &path("photo.png")) }) else {
            panic!("an image has a thumbnail");
        };
        let reader = png::Decoder::new(std::io::Cursor::new(png.to_vec())).read_info().unwrap();
        let (width, height) = (reader.info().width, reader.info().height);
        // The shell keeps the 3:2 shape and fits it in the size asked for.
        assert!(width <= 64 && height <= 64 && width > height, "{width}x{height}");
        assert_eq!(cache.get(&key).as_deref(), Some(png.as_slice()), "kept on disk");

        let text = Key::new(&path("notes.txt"), 64, 0);
        // SAFETY: as above.
        assert!(matches!(unsafe { produce(&cache, &text, &path("notes.txt")) }, Thumb::Icon(IconKey::Ext(ext)) if ext == "txt"));
        // SAFETY: as above.
        assert!(matches!(unsafe { produce(&cache, &text, &dir.to_string_lossy()) }, Thumb::Icon(IconKey::Folder)));
        // SAFETY: as above.
        assert!(matches!(unsafe { produce(&cache, &text, &path("gone.png")) }, Thumb::Missing));
        fs::remove_dir_all(&dir).unwrap();
    }
}
