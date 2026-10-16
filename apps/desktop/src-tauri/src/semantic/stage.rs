//! The embedding stage: one stage of the media library's pass (`kestrel_media::pipeline`), which
//! turns photos and videos into vectors in the model's store.
//!
//! The pass decides which files to look at and when (its scopes, newest first, what the Gallery
//! shows first), pauses, waits for full-screen apps and battery saver, never opens a cloud-only
//! file, and records what each stage did by file key (volume, file id, size, time), so a renamed
//! or moved file is never read again and a changed one always is.
//!
//! This stage takes no picture from the pass: the model must see a photo as the reference
//! prepares it, Pillow's bilinear resize of the whole upright picture to its square, and the
//! pass's picture is already shrunk (a photo fitted to 256 on its longer side and then
//! stretched to 256 x 256 had its short side blurred, and its vector differed from the one
//! "Find similar" made of the same photo). So a photo is decoded here straight to the square,
//! a strip at a time (`kestrel_embed::image::decode_resized`), which is also what
//! `search.rs` does for a picture not described yet. A video is sampled here, a frame every five
//! seconds plus one where the picture cuts, since one frame cannot describe it. Decoding runs in
//! background mode; the model and the store, which searches wait on too, outside it.
//!
//! The stage is ready only while a model is loaded ([`DescribeStage::set_model`]); its version
//! comes from the model's store, so another model (or another download of different files)
//! describes everything again, and a stored file that is still current is not read twice
//! ([`kestrel_vector::Store::has_current`]). The pass's records of what the stage did can outlive
//! the vectors (a model deleted and downloaded again, a store reset after a crash, a folder that
//! left the scopes and came back): [`missing`] finds those, and the pass describes them again
//! ([`reconcile`], after each of its plans and when a model loads). The text in photos is the
//! library's own text stage's (`media_text`), which AI search weighs a description's words with
//! (`search.rs`).

use std::path::Path;
use std::sync::Weak;
use std::sync::atomic::{AtomicU32, Ordering};

use kestrel_embed::{Embedder, Rgb, video};
use kestrel_media::Kind;
use kestrel_media::pipeline::{Input, Item, Needs, Stage};
use kestrel_vector::{Key, Store};
use parking_lot::RwLock;

use super::{Loaded, Semantic};

/// The stage's name in the library's records: never changes.
pub const STAGE: &str = "embedding";
/// Bump to describe everything again with every model (a change to how pictures are prepared).
const STAGE_VERSION: u32 = 1;

/// Photos Kestrel describes: what WIC decodes on a standard Windows 11 (HEIC, WebP and AVIF
/// need their free extensions, which are installed on most machines).
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "webp", "heic", "heif", "avif", "tif", "tiff", "jxl"];
/// Videos Kestrel describes: what Media Foundation reads.
pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov", "avi", "wmv", "mkv", "webm", "3gp", "3g2", "mts", "m2ts"];

/// Attributes of a file whose contents are not on this PC: opening one downloads it.
const PLACEHOLDER: u32 = 0x40_0000 /* RECALL_ON_DATA_ACCESS */ | 0x4_0000 /* RECALL_ON_OPEN */ | 0x1000 /* OFFLINE */;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Video,
}

pub fn kind_of(ext: &str) -> Option<MediaKind> {
    let ext = ext.to_ascii_lowercase();
    if IMAGE_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Whether a file with these attributes is only a placeholder for one in the cloud.
pub fn is_placeholder(attrs: u32) -> bool {
    attrs & PLACEHOLDER != 0
}

/// Kestrel's number for the volume a path is on: its drive letter's code. Network paths have
/// none, so they are not described (file ids there are not reliable).
pub fn volume_code(path: &str) -> Option<u32> {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()).then(|| u32::from(bytes[0].to_ascii_uppercase()))
}

/// A file's attributes, size and last write time as the disk has them now, without opening it
/// (so a placeholder stays one). `None` when it is gone.
pub fn on_disk(path: &str) -> Option<(u32, u64, i64)> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA};
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: a plain C struct, for which all zeroes is a valid value.
    let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a struct of the type the call fills.
    let ok = unsafe { GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&mut data as *mut WIN32_FILE_ATTRIBUTE_DATA).cast()) };
    if ok == 0 {
        return None;
    }
    let size = u64::from(data.nFileSizeHigh) << 32 | u64::from(data.nFileSizeLow);
    let mtime = (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32 | u64::from(data.ftLastWriteTime.dwLowDateTime)) as i64;
    Some((data.dwFileAttributes, size, mtime))
}

/// The stage's version for a loaded model: from its store's tag (the model and its vision
/// tower's checksum), so the version changes exactly when the vectors would go to another
/// store. Never 0, which means "no model".
pub fn version_for(store_tag: &str) -> u32 {
    // FNV-1a: stable across runs and builds, unlike std's hasher.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in format!("{STAGE_VERSION}:{store_tag}").bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash.max(1)
}

/// The library item's key in the vector store.
fn key_of(item: &Item) -> Result<(u32, u64), String> {
    let volume = volume_code(&item.path).ok_or("Files on network drives are not described")?;
    if item.file_id == 0 {
        return Err("Its drive gives files no lasting id".into());
    }
    Ok((volume, item.file_id))
}

/// A photo as the model sees it: decoded straight to its `side` x `side` square, upright, as the
/// reference prepares it. What the stage describes, and what "Find similar" compares with, so
/// the same photo always makes the same vector.
pub fn square_of(path: &str, side: u32) -> Result<Rgb, String> {
    kestrel_media::priority::background(|| kestrel_embed::image::decode_resized(Path::new(path), side, side)).map_err(|e| e.0)
}

/// Describes a photo or samples and describes a video, into `store`.
fn describe(embedder: &dyn Embedder, side: u32, store: &RwLock<Store>, plan: &video::Plan, item: &Item) -> Result<usize, String> {
    let (volume, file_id) = key_of(item)?;
    let row = |at_ms: Option<u32>| Key { volume, file_id, size: item.size, mtime: item.mtime, at_ms };
    if store.read().has_current(volume, file_id, item.size, item.mtime) {
        return Ok(0);
    }
    // The pass checked the file before running its stages; it is opened again here, so look
    // once more that it is no placeholder (that would download it).
    let (attrs, _, _) = on_disk(&item.path).ok_or("The file went")?;
    if is_placeholder(attrs) {
        return Err(kestrel_media::pipeline::ONLINE_ONLY.into());
    }
    let rows: Vec<(Key, Vec<f32>)> = match item.kind {
        Kind::Photo => {
            let square = square_of(&item.path, side)?;
            let vectors = embedder.embed_images(&[square]).map_err(|e| e.0)?;
            vectors.into_iter().map(|vector| (row(None), vector)).collect()
        }
        Kind::Video => {
            let mut frames: Vec<(u32, Rgb)> = Vec::new();
            kestrel_media::priority::background(|| {
                video::sample(Path::new(&item.path), plan, &mut |at, frame| {
                    frames.push((at, kestrel_embed::resize::resize(&frame, side, side)));
                    true
                })
            })
            .map_err(|e| e.0)?;
            if frames.is_empty() {
                return Err("No frame could be read".into());
            }
            let mut rows = Vec::with_capacity(frames.len());
            for chunk in frames.chunks(16) {
                let pictures: Vec<Rgb> = chunk.iter().map(|(_, picture)| picture.clone()).collect();
                let vectors = embedder.embed_images(&pictures).map_err(|e| e.0)?;
                rows.extend(chunk.iter().zip(vectors).map(|((at, _), vector)| (row(Some(*at)), vector)));
            }
            rows
        }
    };
    replace(store, volume, file_id, &rows).map_err(|error| format!("The vectors couldn't be stored: {error}"))?;
    Ok(rows.len())
}

/// Replaces a file's vectors with `rows`.
fn replace(store: &RwLock<Store>, volume: u32, file_id: u64, rows: &[(Key, Vec<f32>)]) -> std::io::Result<()> {
    let mut store = store.write();
    store.tombstone_file(volume, file_id)?;
    store.append(rows)?;
    Ok(())
}

/// The stage the media library's pass runs: registered once, at start (`Semantic::new`).
pub struct DescribeStage {
    semantic: Weak<Semantic>,
    /// 0 while no model is loaded; see [`version_for`].
    version: AtomicU32,
    /// The loaded model's picture side.
    side: AtomicU32,
    plan: video::Plan,
}

impl DescribeStage {
    pub fn new(semantic: Weak<Semantic>) -> Self {
        Self { semantic, version: AtomicU32::new(0), side: AtomicU32::new(0), plan: video::Plan::default() }
    }

    /// The model the stage describes with, or `None` for none. Returns whether that changed
    /// anything, after which the pass should queue its work again (`Pipeline::requeue`).
    pub fn set_model(&self, loaded: Option<&Loaded>) -> bool {
        let (version, side) = match loaded {
            Some(loaded) => (version_for(loaded.store.read().tag()), loaded.side),
            None => (0, 0),
        };
        self.side.store(side, Ordering::SeqCst);
        self.version.swap(version, Ordering::SeqCst) != version
    }
}

impl Stage for DescribeStage {
    fn name(&self) -> &'static str {
        STAGE
    }

    fn version(&self) -> u32 {
        self.version.load(Ordering::SeqCst)
    }

    fn ready(&self) -> bool {
        self.version() != 0
    }

    /// Nothing from the pass: the stage decodes each photo to the model's square itself, as the
    /// reference does, and samples videos (see the module documentation).
    fn needs(&self) -> Needs {
        Needs { pixels: 0 }
    }

    fn run(&self, item: &Item, _: &Input) -> Result<(), String> {
        let semantic = self.semantic.upgrade().ok_or("Kestrel is closing")?;
        // The model that set this version, still loaded. Otherwise the stage stops being ready,
        // so the pass records nothing and the item waits, until the loading thread readies it
        // again for the model now in use.
        let Some(loaded) = semantic.current().filter(|loaded| version_for(loaded.store.read().tag()) == self.version()) else {
            self.version.store(0, Ordering::SeqCst);
            semantic.wake_loader();
            return Err("The model changed while the file was read".into());
        };
        let described = describe(&loaded.embedder, loaded.side, &loaded.store, &self.plan, item);
        semantic.status_changed(false);
        described.map(|_| ())
    }
}

/// The library's items that have no vector in `store`: those the stage did whose vectors went
/// (see the module documentation), and those it has not done yet, which the pass leaves alone.
pub fn missing(store: &RwLock<Store>, items: &[Item]) -> Vec<i64> {
    let store = store.read();
    items
        .iter()
        .filter(|item| volume_code(&item.path).is_none_or(|volume| !store.has_file(volume, item.file_id)))
        .map(|item| item.id)
        .collect()
}

/// Has the pass describe again the items it recorded as described that have no vector in
/// `store`: its records outlive the vectors (see the module documentation), which would
/// otherwise never be made again. Returns how many.
pub fn reconcile(store: &RwLock<Store>, pipeline: &kestrel_media::Pipeline) -> usize {
    let missing = missing(store, &pipeline.items());
    pipeline.redo(STAGE, &missing).unwrap_or(0)
}

/// Tombstones the vectors of files the library no longer holds (deleted, or gone from its
/// folders); `items` are its items, by volume code and file id. A changed file's vectors are
/// replaced when it is described again.
pub fn sweep(store: &RwLock<Store>, items: &std::collections::HashSet<(u32, u64)>) -> usize {
    let stale: Vec<u32> = {
        let store = store.read();
        (0..store.len() as u32)
            .filter(|&row| store.is_live(row) && store.key(row).is_some_and(|key| !items.contains(&key.file())))
            .collect()
    };
    if !stale.is_empty() {
        let mut store = store.write();
        for &row in &stale {
            let _ = store.tombstone(row);
        }
    }
    stale.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_embed::{DeviceInfo, Result as EmbedResult, l2_normalize};

    /// Describes a picture by its average colour.
    struct Colours;

    impl Embedder for Colours {
        fn dim(&self) -> usize {
            3
        }
        fn embed_images(&self, images: &[Rgb]) -> EmbedResult<Vec<Vec<f32>>> {
            Ok(images
                .iter()
                .map(|image| {
                    let n = image.pixels.len() as f32 / 3.0;
                    let mut v: Vec<f32> = (0..3).map(|c| image.pixels.iter().skip(c).step_by(3).map(|&b| f32::from(b)).sum::<f32>() / n + 1.0).collect();
                    l2_normalize(&mut v);
                    v
                })
                .collect())
        }
        fn embed_texts(&self, _: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
            unreachable!()
        }
        fn device(&self) -> DeviceInfo {
            DeviceInfo { gpu: false, name: "test".into(), note: None }
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-semantic-stage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(path: &Path, kind: Kind) -> Item {
        let path = path.to_str().unwrap().to_owned();
        let (_, size, mtime) = on_disk(&path).unwrap();
        Item { id: 1, volume: "c:".into(), file_id: kestrel_index::walk::file_id_of(&path).unwrap(), path, kind, size, mtime }
    }

    #[test]
    fn photos_and_videos_are_described_once_and_replaced_when_they_change() {
        let _com = kestrel_media::Apartment::enter();
        let dir = scratch("run");
        let red = dir.join("red.png");
        let clip = dir.join("clip.mp4");
        kestrel_embed::image::encode(&Rgb::filled(40, 30, [250, 10, 10]), &red, false).unwrap();
        video::write_test_video(&clip, 64, 64, 10, &[([200, 20, 20], 2000), ([20, 200, 20], 2000)]).unwrap();
        let store = RwLock::new(Store::open(&dir.join("vectors"), 3, "test").unwrap());
        let plan = video::Plan { every_ms: 1000, max_frames: 8, scene_changes: false };
        let photo = item(&red, Kind::Photo);
        assert_eq!(describe(&Colours, 8, &store, &plan, &photo), Ok(1));
        // Stored as it is now: not described again.
        assert_eq!(describe(&Colours, 8, &store, &plan, &photo), Ok(0));
        let rows = store.read().rows_of(volume_code(&photo.path).unwrap(), photo.file_id);
        let v = store.read().vector(rows[0]).unwrap();
        assert!(v[0] > v[1] && v[0] > v[2], "red, in RGB order: {v:?}");

        // A video: a frame a second, each with its moment.
        let video = item(&clip, Kind::Video);
        assert_eq!(describe(&Colours, 8, &store, &plan, &video), Ok(4));
        let store_now = store.read();
        let moments: Vec<Option<u32>> = store_now
            .rows_of(volume_code(&video.path).unwrap(), video.file_id)
            .iter()
            .map(|&row| store_now.key(row).unwrap().at_ms)
            .collect();
        assert_eq!(moments.len(), 4);
        assert!(moments.iter().all(|at| at.is_some()));
        drop(store_now);

        // Changed (another size or time): its old vector is replaced, not added to.
        std::thread::sleep(std::time::Duration::from_millis(20));
        kestrel_embed::image::encode(&Rgb::filled(50, 30, [10, 250, 10]), &red, false).unwrap();
        let changed = item(&red, Kind::Photo);
        assert_ne!((changed.size, changed.mtime), (photo.size, photo.mtime));
        assert_eq!(describe(&Colours, 8, &store, &plan, &changed), Ok(1));
        let rows = store.read().rows_of(volume_code(&photo.path).unwrap(), photo.file_id);
        assert_eq!(rows.len(), 1, "one live vector per photo");
        assert!(store.read().vector(rows[0]).unwrap()[1] > 0.9);
        // A file that is no picture can't be described.
        let broken = dir.join("broken.jpg");
        std::fs::write(&broken, b"not a picture").unwrap();
        assert!(describe(&Colours, 8, &store, &plan, &item(&broken, Kind::Photo)).is_err());

        // Files the library no longer holds lose their vectors.
        let keep = std::collections::HashSet::from([(volume_code(&photo.path).unwrap(), photo.file_id)]);
        assert_eq!(sweep(&store, &keep), 4);
        assert_eq!(store.read().live_files(), 1);
        // And the library's items without one are known.
        assert_eq!(missing(&store, &[Item { id: 7, ..changed.clone() }, Item { id: 8, ..video.clone() }]), [8]);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Keeps the pictures it is asked to describe.
    #[derive(Default)]
    struct Seen(parking_lot::Mutex<Vec<Rgb>>);

    impl Embedder for Seen {
        fn dim(&self) -> usize {
            3
        }
        fn embed_images(&self, images: &[Rgb]) -> EmbedResult<Vec<Vec<f32>>> {
            self.0.lock().extend(images.iter().cloned());
            Ok(images.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }
        fn embed_texts(&self, _: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
            unreachable!()
        }
        fn device(&self) -> DeviceInfo {
            DeviceInfo { gpu: false, name: "test".into(), note: None }
        }
    }

    /// The model sees a photo as the reference prepares it, the whole picture resized to its
    /// square, whatever else the library's pass decoded: a 4:3 photo was once given to it
    /// shrunk to 16 x 12 by the pass and stretched to 16 x 16. "Find similar" prepares the
    /// same picture of it.
    #[test]
    fn photos_reach_the_model_as_the_reference_prepares_them() {
        let _com = kestrel_media::Apartment::enter();
        let dir = scratch("reference");
        let mut picture = Rgb::filled(400, 300, [0, 0, 0]);
        for (i, value) in picture.pixels.iter_mut().enumerate() {
            *value = (i.wrapping_mul(2_654_435_761) >> 11) as u8;
        }
        let path = dir.join("photo.png");
        kestrel_embed::image::encode(&picture, &path, false).unwrap();
        let store = RwLock::new(Store::open(&dir.join("vectors"), 3, "test").unwrap());
        let seen = Seen::default();
        let plan = video::Plan::default();
        assert_eq!(describe(&seen, 16, &store, &plan, &item(&path, Kind::Photo)), Ok(1));
        let reference = kestrel_embed::resize::resize(&kestrel_embed::image::decode(&path).unwrap(), 16, 16);
        assert_eq!(seen.0.lock().as_slice(), std::slice::from_ref(&reference));
        assert_eq!(square_of(path.to_str().unwrap(), 16).unwrap(), reference);
        assert_eq!(DescribeStage::new(std::sync::Weak::new()).needs(), Needs { pixels: 0 }, "nothing from the pass");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real describing, as a stage of the pass, into a store that can be swapped.
    struct Describing {
        store: std::sync::Arc<RwLock<Store>>,
    }

    impl Stage for Describing {
        fn name(&self) -> &'static str {
            STAGE
        }
        fn version(&self) -> u32 {
            version_for("test")
        }
        fn needs(&self) -> Needs {
            Needs { pixels: 0 }
        }
        fn run(&self, item: &Item, _: &Input) -> Result<(), String> {
            describe(&Colours, 8, &self.store, &video::Plan::default(), item).map(|_| ())
        }
    }

    /// The pass's records say a photo is described; its vectors can go all the same: the model
    /// deleted and downloaded again (the same files, so the same version), the store reset after
    /// a crash, or the photo's folder out of the scopes for a while (swept) and back. They are
    /// made again, where once nothing ever made them.
    #[test]
    fn vectors_lost_while_the_records_stayed_are_made_again() {
        let _com = kestrel_media::Apartment::enter();
        let dir = scratch("lost");
        let photos = dir.join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        for (name, colour) in [("red.png", [250, 10, 10]), ("blue.png", [10, 10, 250])] {
            kestrel_embed::image::encode(&Rgb::filled(30, 20, colour), &photos.join(name), false).unwrap();
        }
        let root = photos.to_str().unwrap().to_owned();
        let candidates = || {
            let mut builder = kestrel_index::IndexBuilder::new();
            kestrel_index::walk::walk(&root, &mut builder).unwrap();
            kestrel_media::scan::scan(&builder.finish(), std::slice::from_ref(&root))
        };
        let store = std::sync::Arc::new(RwLock::new(Store::open(&dir.join("first"), 3, "test").unwrap()));
        let db = std::sync::Arc::new(kestrel_db::Db::open_in_memory().unwrap());
        let config = kestrel_media::pipeline::Config { workers: 1, conditions: Box::new(|| None), recheck: std::time::Duration::from_millis(50) };
        let pipeline = kestrel_media::Pipeline::new(db, config);
        pipeline.register(std::sync::Arc::new(Describing { store: store.clone() }));
        pipeline.start();
        let settle = || assert!(pipeline.wait_idle(std::time::Duration::from_secs(30)));
        pipeline.plan(candidates()).unwrap();
        settle();
        assert_eq!(store.read().live_files(), 2);

        // The model deleted, its folder with it, and downloaded again: an empty store, the
        // records as they were. Asking again finds nothing to do.
        *store.write() = Store::open(&dir.join("second"), 3, "test").unwrap();
        pipeline.requeue();
        settle();
        assert_eq!(store.read().live_files(), 0, "the records still say described");
        assert_eq!(reconcile(&store, &pipeline), 2);
        settle();
        assert_eq!(store.read().live_files(), 2);
        assert_eq!(reconcile(&store, &pipeline), 0, "nothing more to do");

        // A photo leaves the scopes: its vectors are swept. When it comes back, they are made
        // again.
        let without_blue: Vec<_> = candidates().into_iter().filter(|c| !c.path.ends_with("blue.png")).collect();
        pipeline.plan(without_blue).unwrap();
        let held: std::collections::HashSet<(u32, u64)> =
            pipeline.items().iter().filter_map(|item| Some((volume_code(&item.path)?, item.file_id))).collect();
        assert_eq!(sweep(&store, &held), 1);
        pipeline.plan(candidates()).unwrap();
        settle();
        assert_eq!(store.read().live_files(), 1, "back, and its record says described");
        assert_eq!(reconcile(&store, &pipeline), 1);
        settle();
        assert_eq!(store.read().live_files(), 2);
        pipeline.stop();
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_follow_the_store_and_placeholders_are_known() {
        assert_ne!(version_for("fast-abc"), version_for("fast-abd"));
        assert_eq!(version_for("fast-abc"), version_for("fast-abc"));
        assert_ne!(version_for(""), 0);
        assert!(is_placeholder(0x0040_1620), "OneDrive's online-only files");
        assert!(is_placeholder(0x1000));
        assert!(!is_placeholder(0x20));
        assert_eq!(volume_code(r"c:\x"), Some(u32::from(b'C')));
        assert_eq!(volume_code(r"\\server\share\x"), None);
        assert_eq!(kind_of("JPG"), Some(MediaKind::Image));
        assert_eq!(kind_of("mov"), Some(MediaKind::Video));
        assert_eq!(kind_of("txt"), None);
        let network = Item { id: 1, volume: "".into(), file_id: 9, path: r"\\nas\photos\a.jpg".into(), kind: Kind::Photo, size: 1, mtime: 1 };
        assert!(key_of(&network).is_err());
    }
}
