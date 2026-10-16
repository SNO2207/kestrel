//! The embedding stage: one stage of the media library's pass (`kestrel_media::pipeline`), which
//! turns photos and videos into vectors in the model's store.
//!
//! The pass decides which files to look at and when (its scopes, newest first, what the Gallery
//! shows first), pauses, waits for full-screen apps and battery saver, never opens a cloud-only
//! file, and records what each stage did by file key (volume, file id, size, time), so a renamed
//! or moved file is never read again and a changed one always is. It decodes each photo once, at
//! the largest size any stage asks for, turned upright; this stage asks for its model's side
//! and resizes that picture to the model's square with the model's own filter. A video is
//! sampled here, a frame every five seconds plus one where the picture cuts, since one frame
//! cannot describe it.
//!
//! The stage is ready only while a model is loaded ([`DescribeStage::set_model`]); its version
//! comes from the model's store, so another model (or another download of different files)
//! describes everything again, and a stored file that is still current is not read twice
//! ([`kestrel_vector::Store::has_current`]). The text in photos is the library's own text stage's
//! (`media_text`), which AI search weighs a description's words with (`search.rs`).

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

/// A picture of the pass's (BGRA, upright) as the model's RGB.
fn rgb_of(picture: &kestrel_media::Bitmap) -> Rgb {
    let pixels = picture.pixels.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
    Rgb { width: picture.width, height: picture.height, pixels }
}

/// Describes a photo already decoded, or samples and describes a video, into `store`.
fn describe(embedder: &dyn Embedder, side: u32, store: &RwLock<Store>, plan: &video::Plan, item: &Item, input: &Input) -> Result<usize, String> {
    let (volume, file_id) = key_of(item)?;
    let row = |at_ms: Option<u32>| Key { volume, file_id, size: item.size, mtime: item.mtime, at_ms };
    if store.read().has_current(volume, file_id, item.size, item.mtime) {
        return Ok(0);
    }
    let rows: Vec<(Key, Vec<f32>)> = match item.kind {
        Kind::Photo => {
            let picture = input.picture.ok_or("The picture couldn't be decoded")?;
            let square = kestrel_embed::resize::resize(&rgb_of(picture), side, side);
            let vectors = embedder.embed_images(&[square]).map_err(|e| e.0)?;
            vectors.into_iter().map(|vector| (row(None), vector)).collect()
        }
        Kind::Video => {
            // The pass checked the file before running its stages; the sampler opens it again,
            // so look once more that it is no placeholder (that would download it).
            let (attrs, _, _) = on_disk(&item.path).ok_or("The file went")?;
            if is_placeholder(attrs) {
                return Err(kestrel_media::pipeline::ONLINE_ONLY.into());
            }
            let mut frames: Vec<(u32, Rgb)> = Vec::new();
            video::sample(Path::new(&item.path), plan, &mut |at, frame| {
                frames.push((at, kestrel_embed::resize::resize(&frame, side, side)));
                true
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

    fn needs(&self) -> Needs {
        Needs { pixels: self.side.load(Ordering::SeqCst) }
    }

    fn run(&self, item: &Item, input: &Input) -> Result<(), String> {
        let semantic = self.semantic.upgrade().ok_or("Kestrel is closing")?;
        // The model that set this version, still loaded. Otherwise the stage stops being ready,
        // so the pass records nothing and the item waits, until the loading thread readies it
        // again for the model now in use.
        let Some(loaded) = semantic.current().filter(|loaded| version_for(loaded.store.read().tag()) == self.version()) else {
            self.version.store(0, Ordering::SeqCst);
            semantic.wake_loader();
            return Err("The model changed while the file was read".into());
        };
        let described = describe(&loaded.embedder, loaded.side, &loaded.store, &self.plan, item, input);
        semantic.status_changed(false);
        described.map(|_| ())
    }
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

    /// The pass's picture: BGRA, as WIC gives it.
    fn bgra(width: u32, height: u32, rgb: [u8; 3]) -> kestrel_media::Bitmap {
        kestrel_media::Bitmap::filled(width, height, [rgb[2], rgb[1], rgb[0], 255])
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
        let facts = kestrel_media::Facts::default();
        let picture = bgra(40, 30, [250, 10, 10]);
        let photo = item(&red, Kind::Photo);
        let with_picture = Input { facts: &facts, picture: Some(&picture) };
        assert_eq!(describe(&Colours, 8, &store, &plan, &photo, &with_picture), Ok(1));
        // Stored as it is now: not described again.
        assert_eq!(describe(&Colours, 8, &store, &plan, &photo, &with_picture), Ok(0));
        let rows = store.read().rows_of(volume_code(&photo.path).unwrap(), photo.file_id);
        let v = store.read().vector(rows[0]).unwrap();
        assert!(v[0] > v[1] && v[0] > v[2], "red, in RGB order: {v:?}");
        // Without its picture (it couldn't be decoded), a photo can't be described.
        assert!(describe(&Colours, 8, &store, &plan, &Item { mtime: 5, ..photo.clone() }, &Input { facts: &facts, picture: None }).is_err());

        // A video: a frame a second, each with its moment.
        let video = item(&clip, Kind::Video);
        assert_eq!(describe(&Colours, 8, &store, &plan, &video, &Input { facts: &facts, picture: None }), Ok(4));
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
        let green = bgra(50, 30, [10, 250, 10]);
        let changed = Item { size: photo.size + 1, ..photo.clone() };
        assert_eq!(describe(&Colours, 8, &store, &plan, &changed, &Input { facts: &facts, picture: Some(&green) }), Ok(1));
        let rows = store.read().rows_of(volume_code(&photo.path).unwrap(), photo.file_id);
        assert_eq!(rows.len(), 1, "one live vector per photo");
        assert!(store.read().vector(rows[0]).unwrap()[1] > 0.9);

        // Files the library no longer holds lose their vectors.
        let keep = std::collections::HashSet::from([(volume_code(&photo.path).unwrap(), photo.file_id)]);
        assert_eq!(sweep(&store, &keep), 4);
        assert_eq!(store.read().live_files(), 1);
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
