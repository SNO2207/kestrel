//! The embedding stage: what turns photos and videos into vectors in the model's store, and the
//! text in photos into the text index.
//!
//! It is shaped as one stage of a media pipeline: something else decides which files to look at
//! and when (scopes, scheduling, pausing; here `driver.rs` stands in), hands it [`MediaItem`]s
//! keyed by [`FileKey`], asks [`EmbedStage::wants`] first, and gives it batches. The stage itself
//! never opens a cloud-only placeholder (it checks the file's attributes on disk right before
//! opening it, which does not download anything), records what it couldn't read so that is not
//! tried again until the file changes, and replaces a changed file's vectors.

use std::collections::HashMap;
use std::path::Path;

use kestrel_db::Db;
use kestrel_db::semantic::{self as data, FileState};
use kestrel_embed::{Embedder, Rgb, ocr::Ocr, video};
use kestrel_vector::{Key, Store};
use parking_lot::RwLock;
use rayon::prelude::*;

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

/// Which file, as it was: what the stage keys everything by. A renamed or moved file keeps its
/// key, so it is never read again; a changed one gets a new size or time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub volume: u32,
    pub file_id: u64,
    pub size: u64,
    /// Last write time, FILETIME ticks.
    pub mtime: i64,
}

impl FileKey {
    fn state(&self) -> FileState {
        FileState { volume: self.volume, file_id: self.file_id, size: self.size, mtime: self.mtime }
    }

    fn row(&self, at_ms: Option<u32>) -> Key {
        Key { volume: self.volume, file_id: self.file_id, size: self.size, mtime: self.mtime, at_ms }
    }
}

/// A photo or video handed to the stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaItem {
    pub path: String,
    pub key: FileKey,
    pub kind: MediaKind,
}

/// What became of an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stored, with this many vectors (one for a photo, one per frame for a video).
    Embedded(usize),
    /// It couldn't be read; noted, and not tried again until it changes.
    Unreadable(String),
    /// Its contents are in the cloud only; left alone.
    Placeholder,
    /// It is no longer there.
    Gone,
}

/// The model the stage feeds and the store its vectors go to.
pub struct Target<'a> {
    pub model: &'a str,
    pub embedder: &'a dyn Embedder,
    /// The side of the square the model sees: pictures are resized to it as soon as they are
    /// decoded, which is the model's own preprocessing and keeps memory small.
    pub side: u32,
    pub store: &'a RwLock<Store>,
}

pub struct EmbedStage<'a> {
    pub target: Target<'a>,
    pub db: &'a Db,
    /// Windows' text recogniser, when it has one for the owner's languages.
    pub ocr: Option<&'a Ocr>,
    pub plan: video::Plan,
    /// Low-priority threads that decode, resize and read text.
    pub pool: &'a rayon::ThreadPool,
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

/// Checks an item on disk right before it is opened: gone or a placeholder, or else the key to
/// store it under. That is the key the pipeline gave (with the file id read if it had none),
/// not what the disk says now: stored keys then always match the pipeline's, and a file changed
/// since is simply read again when the pipeline hands it over with its new key.
fn check(item: &MediaItem) -> Result<FileKey, Outcome> {
    let (attrs, _, _) = on_disk(&item.path).ok_or(Outcome::Gone)?;
    if is_placeholder(attrs) {
        return Err(Outcome::Placeholder);
    }
    let file_id = if item.key.file_id != 0 { item.key.file_id } else { kestrel_index::walk::file_id_of(&item.path).unwrap_or(0) };
    if file_id == 0 {
        return Err(Outcome::Unreadable("Windows gave the file no id".into()));
    }
    Ok(FileKey { file_id, ..item.key })
}

/// A decoded photo, made the model's size, and the text read from it.
struct Prepared {
    key: FileKey,
    picture: Rgb,
    text: Option<String>,
}

impl EmbedStage<'_> {
    /// Whether `item` needs the stage: not stored as it is now, and not a file this model
    /// couldn't read as it is now. `skips` is `kestrel_db::semantic::skips` for the model.
    pub fn wants(&self, item: &MediaItem, skips: &HashMap<(u32, u64), (u64, i64)>) -> bool {
        let key = item.key;
        if skips.get(&(key.volume, key.file_id)) == Some(&(key.size, key.mtime)) {
            return false;
        }
        !self.target.store.read().has_current(key.volume, key.file_id, key.size, key.mtime)
    }

    /// Decodes, describes and stores a batch of photos; `needs_text` says which files still
    /// want their text read (`kestrel_db::semantic::text_files`).
    pub fn run_images(&self, items: &[MediaItem], text_files: &HashMap<(u32, u64), (u64, i64)>) -> Vec<Outcome> {
        let side = self.target.side;
        let prepared: Vec<Result<Prepared, Outcome>> = self.pool.install(|| {
            items
                .par_iter()
                .map(|item| {
                    let key = check(item)?;
                    let decoded = kestrel_embed::image::decode(Path::new(&item.path)).map_err(|error| Outcome::Unreadable(error.0))?;
                    let wants_text = text_files.get(&(key.volume, key.file_id)) != Some(&(key.size, key.mtime));
                    let text = match (self.ocr, wants_text) {
                        (Some(ocr), true) => Some(ocr.read(&decoded).unwrap_or_default()),
                        _ => None,
                    };
                    Ok(Prepared { key, picture: kestrel_embed::resize::resize(&decoded, side, side), text })
                })
                .collect()
        });
        let ready: Vec<&Prepared> = prepared.iter().filter_map(|p| p.as_ref().ok()).collect();
        let pictures: Vec<Rgb> = ready.iter().map(|p| p.picture.clone()).collect();
        let vectors = if pictures.is_empty() { Ok(Vec::new()) } else { self.target.embedder.embed_images(&pictures) };
        let mut outcomes = Vec::with_capacity(items.len());
        match vectors {
            Ok(vectors) => {
                let rows: Vec<(Key, Vec<f32>)> = ready.iter().zip(vectors).map(|(p, v)| (p.key.row(None), v)).collect();
                if let Err(error) = self.store(&rows) {
                    return items.iter().map(|_| Outcome::Unreadable(format!("the store couldn't be written: {error}"))).collect();
                }
            }
            // The model failed on the batch: nothing is noted against the files, which are
            // tried again on the next pass.
            Err(error) => return items.iter().map(|_| Outcome::Unreadable(error.0.clone())).collect(),
        }
        for (item, result) in items.iter().zip(&prepared) {
            match result {
                Ok(p) => {
                    if let Some(text) = &p.text {
                        let _ = data::set_text(self.db, &p.key.state(), text);
                    }
                    outcomes.push(Outcome::Embedded(1));
                }
                Err(outcome) => {
                    if let Outcome::Unreadable(why) = outcome {
                        let _ = data::skip(self.db, self.target.model, &item.key.state(), why);
                    }
                    outcomes.push(outcome.clone());
                }
            }
        }
        outcomes
    }

    /// Samples a video's frames, describes them and stores one vector per frame with its moment.
    pub fn run_video(&self, item: &MediaItem) -> Outcome {
        let key = match check(item) {
            Ok(key) => key,
            Err(outcome) => return outcome,
        };
        let side = self.target.side;
        let mut frames: Vec<(u32, Rgb)> = Vec::new();
        let sampled = self.pool.install(|| {
            video::sample(Path::new(&item.path), &self.plan, &mut |at, frame| {
                frames.push((at, kestrel_embed::resize::resize(&frame, side, side)));
                true
            })
        });
        if let Err(error) = sampled {
            let _ = data::skip(self.db, self.target.model, &item.key.state(), &error.0);
            return Outcome::Unreadable(error.0);
        }
        if frames.is_empty() {
            let why = "no frame could be read".to_owned();
            let _ = data::skip(self.db, self.target.model, &item.key.state(), &why);
            return Outcome::Unreadable(why);
        }
        let mut rows = Vec::with_capacity(frames.len());
        for chunk in frames.chunks(16) {
            let pictures: Vec<Rgb> = chunk.iter().map(|(_, picture)| picture.clone()).collect();
            match self.target.embedder.embed_images(&pictures) {
                Ok(vectors) => rows.extend(chunk.iter().zip(vectors).map(|((at, _), v)| (key.row(Some(*at)), v))),
                Err(error) => return Outcome::Unreadable(error.0),
            }
        }
        match self.store(&rows) {
            Ok(()) => Outcome::Embedded(rows.len()),
            Err(error) => Outcome::Unreadable(format!("the store couldn't be written: {error}")),
        }
    }

    /// Replaces the vectors of the files in `rows` with these.
    fn store(&self, rows: &[(Key, Vec<f32>)]) -> std::io::Result<()> {
        let mut store = self.target.store.write();
        let mut cleared = std::collections::HashSet::new();
        for (key, _) in rows {
            if cleared.insert(key.file()) {
                store.tombstone_file(key.volume, key.file_id)?;
            }
        }
        store.append(rows)?;
        Ok(())
    }
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

    fn item(path: &Path, kind: MediaKind) -> MediaItem {
        let path = path.to_str().unwrap().to_owned();
        let (_, size, mtime) = on_disk(&path).unwrap();
        let key = FileKey { volume: volume_code(&path).unwrap(), file_id: kestrel_index::walk::file_id_of(&path).unwrap(), size, mtime };
        MediaItem { path, key, kind }
    }

    #[test]
    fn photos_and_videos_are_stored_once_and_unreadable_files_noted() {
        let dir = scratch("run");
        let red = dir.join("red.png");
        let blue = dir.join("blue.jpg");
        let broken = dir.join("broken.png");
        let clip = dir.join("clip.mp4");
        let receipt = dir.join("receipt.png");
        kestrel_embed::image::encode(&Rgb::filled(40, 30, [250, 10, 10]), &red, false).unwrap();
        kestrel_embed::image::encode(&Rgb::filled(40, 30, [10, 10, 250]), &blue, true).unwrap();
        std::fs::write(&broken, b"not a picture").unwrap();
        video::write_test_video(&clip, 64, 64, 10, &[([200, 20, 20], 2000), ([20, 200, 20], 2000)]).unwrap();
        let words = kestrel_embed::draw::words(800, 240, [255, 255, 255], [0, 0, 0], &["COSTCO", "TOTAL 42.17"], 60).unwrap();
        kestrel_embed::image::encode(&words, &receipt, false).unwrap();

        let store = RwLock::new(Store::open(&dir.join("vectors"), 3, "test").unwrap());
        let db = Db::open_in_memory().unwrap();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let ocr = Ocr::new().ok();
        let stage = EmbedStage {
            target: Target { model: "fast", embedder: &Colours, side: 8, store: &store },
            db: &db,
            ocr: ocr.as_ref(),
            plan: video::Plan { every_ms: 1000, max_frames: 8, scene_changes: false },
            pool: &pool,
        };
        let photos = [item(&red, MediaKind::Image), item(&blue, MediaKind::Image), item(&broken, MediaKind::Image), item(&receipt, MediaKind::Image)];
        let skips = data::skips(&db, "fast").unwrap();
        assert!(photos.iter().all(|p| stage.wants(p, &skips)));
        let outcomes = stage.run_images(&photos, &data::text_files(&db).unwrap());
        assert_eq!(outcomes[0], Outcome::Embedded(1));
        assert_eq!(outcomes[1], Outcome::Embedded(1));
        assert!(matches!(&outcomes[2], Outcome::Unreadable(why) if why.contains("can't read")), "{outcomes:?}");
        let video = item(&clip, MediaKind::Video);
        let frames = match stage.run_video(&video) {
            Outcome::Embedded(n) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(frames, 4, "a frame a second of a four-second video");

        // Stored: the red photo is red, and the video's frames have their moments.
        let store_now = store.read();
        let red_rows = store_now.rows_of(photos[0].key.volume, photos[0].key.file_id);
        assert_eq!(red_rows.len(), 1);
        let v = store_now.vector(red_rows[0]).unwrap();
        assert!(v[0] > v[1] && v[0] > v[2], "{v:?}");
        let moments: Vec<Option<u32>> =
            store_now.rows_of(video.key.volume, video.key.file_id).iter().map(|&row| store_now.key(row).unwrap().at_ms).collect();
        assert_eq!(moments.len(), 4);
        assert!(moments.iter().all(|at| at.is_some()));
        drop(store_now);

        // Nothing is wanted again: stored as it is, or noted as unreadable as it is.
        let skips = data::skips(&db, "fast").unwrap();
        assert!(!photos.iter().any(|p| stage.wants(p, &skips)));
        assert!(!stage.wants(&video, &skips));
        // The text in the receipt was read and can be searched.
        if ocr.is_some() {
            let hits = data::search_text(&db, &["costco".into()], 5).unwrap();
            assert_eq!(hits.len(), 1, "{hits:?}");
            assert_eq!(hits[0].file.file_id, photos[3].key.file_id);
        }

        // A changed photo is wanted again, and its old vector replaced.
        kestrel_embed::image::encode(&Rgb::filled(50, 30, [10, 250, 10]), &red, false).unwrap();
        let changed = item(&red, MediaKind::Image);
        assert!(stage.wants(&changed, &skips));
        assert_eq!(stage.run_images(std::slice::from_ref(&changed), &HashMap::new()), [Outcome::Embedded(1)]);
        let store_now = store.read();
        let rows = store_now.rows_of(changed.key.volume, changed.key.file_id);
        assert_eq!(rows.len(), 1, "one live vector per photo");
        let v = store_now.vector(rows[0]).unwrap();
        assert!(v[1] > v[0], "now green: {v:?}");
        drop(store_now);
        // The store's files are mapped until it goes.
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placeholders_and_vanished_files_are_left_alone() {
        let dir = scratch("placeholder");
        let path = dir.join("gone.png");
        let missing = MediaItem {
            path: path.to_str().unwrap().to_owned(),
            key: FileKey { volume: b'C' as u32, file_id: 9, size: 1, mtime: 1 },
            kind: MediaKind::Image,
        };
        assert_eq!(check(&missing), Err(Outcome::Gone));
        assert!(is_placeholder(0x0040_1620), "OneDrive's online-only files");
        assert!(is_placeholder(0x1000));
        assert!(!is_placeholder(0x20));
        assert_eq!(volume_code(r"c:\x"), Some(u32::from(b'C')));
        assert_eq!(volume_code(r"\\server\share\x"), None);
        assert_eq!(kind_of("JPG"), Some(MediaKind::Image));
        assert_eq!(kind_of("mov"), Some(MediaKind::Video));
        assert_eq!(kind_of("txt"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
