//! The Gallery's data: every photo and video in the scopes, newest first by the date it was
//! taken (or, failing that, last changed), as a result set of index entries the page fetches
//! rows from by range, as the file list does (`Engine::show_entries`), with what the page needs
//! of every item up front to lay out the whole grid: its date and its shape. Thumbnails come
//! through the `thumb` protocol, which the thumbnail stage has usually filled already.

use std::collections::HashMap;

use kestrel_db::media::Shown;
use kestrel_media::fixtures::{Exif, text_bitmap, write_jpeg, write_video};
use kestrel_media::{Bitmap, Kind};
use serde::{Deserialize, Serialize};

use super::Media;

/// Folders offered by the Gallery's folder filter, at most: those holding the most items.
const MAX_FOLDERS: usize = 60;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Filter {
    /// Photos or videos only.
    pub kind: Option<Kind>,
    /// Only what is inside this folder.
    pub folder: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FolderCount {
    pub path: String,
    pub count: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct GalleryData {
    /// The result set of the items, in the Gallery's order, for `rows`.
    pub set: u32,
    pub total: u32,
    /// Each item's date in milliseconds since the Unix epoch: taken, or else last changed.
    pub dates: Vec<i64>,
    /// Each item's width over its height, as shown; 0 while unknown.
    pub ratios: Vec<f32>,
    /// The positions of the videos, with their lengths in seconds (0 while unknown).
    pub videos: Vec<(u32, u32)>,
    /// The folders holding the most items (of the kind asked for), for the folder filter.
    pub folders: Vec<FolderCount>,
    pub scopes: Vec<String>,
    /// The size to ask the `thumb` protocol for: the one the thumbnail stage files.
    pub thumb: u32,
    /// Where each of the paths the page asked to keep is now, or -1: the item at the top of its
    /// view, the focused one and the selected ones, which stay so through a reload.
    pub kept: Vec<i64>,
}

struct Tile {
    entry: kestrel_index::EntryId,
    date: i64,
    ratio: f32,
    video: Option<u32>,
    path: String,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('\\').map_or(path, |(parent, _)| parent)
}

/// The Gallery's items now, and where the items at `keep` (paths) are among them.
pub fn open(media: &Media, filter: &Filter, keep: &[String]) -> Result<GalleryData, String> {
    let scopes = media.scopes();
    // The entries found must still be the index's when the set is made; a replaced index (a
    // rescan finishing) means looking again.
    for _ in 0..3 {
        let generation = media.engine.generation();
        let candidates = super::scan(&media.engine, &scopes);
        let shown: HashMap<(String, u64), Shown> =
            kestrel_db::media::shown(&media.db)?.into_iter().map(|item| ((item.volume.clone(), item.file_id), item)).collect();
        let mut tiles: Vec<Tile> = candidates
            .into_iter()
            .filter(|candidate| filter.kind.is_none_or(|kind| kind == candidate.kind))
            .map(|candidate| {
                let facts = shown.get(&(candidate.volume.clone(), candidate.file_id));
                let date = facts.and_then(|f| f.taken).unwrap_or(candidate.mtime);
                let ratio = match facts.map(|f| (f.width, f.height)) {
                    Some((Some(width), Some(height))) if height > 0 => ((width as f32 / height as f32) * 1000.0).round() / 1000.0,
                    _ => 0.0,
                };
                let video = (candidate.kind == Kind::Video).then(|| facts.and_then(|f| f.duration_ms).map_or(0, |ms| ms.saturating_add(500) / 1000) as u32);
                Tile { entry: candidate.entry, date: kestrel_media::unix_ms(date), ratio, video, path: candidate.path }
            })
            .collect();

        let mut counts: HashMap<&str, u32> = HashMap::new();
        for tile in &tiles {
            *counts.entry(parent(&tile.path)).or_default() += 1;
        }
        let mut folders: Vec<FolderCount> = counts.into_iter().map(|(path, count)| FolderCount { path: path.to_owned(), count }).collect();
        folders.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
        folders.truncate(MAX_FOLDERS);
        folders.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));

        if let Some(folder) = &filter.folder {
            tiles.retain(|tile| kestrel_media::scopes::is_within(parent(&tile.path), folder));
        }
        tiles.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.path.cmp(&b.path)));
        if media.engine.generation() != generation {
            continue;
        }
        let wanted: HashMap<String, usize> = keep.iter().enumerate().map(|(at, path)| (path.to_lowercase(), at)).collect();
        let mut kept = vec![-1i64; keep.len()];
        if !wanted.is_empty() {
            for (at, tile) in tiles.iter().enumerate() {
                if let Some(&slot) = wanted.get(&tile.path.to_lowercase()) {
                    kept[slot] = at as i64;
                }
            }
        }
        let listing = media.engine.show_entries(tiles.iter().map(|tile| tile.entry).collect());
        return Ok(GalleryData {
            kept,
            set: listing.set,
            total: listing.total,
            dates: tiles.iter().map(|tile| tile.date).collect(),
            ratios: tiles.iter().map(|tile| tile.ratio).collect(),
            videos: tiles.iter().enumerate().filter_map(|(at, tile)| tile.video.map(|secs| (at as u32, secs))).collect(),
            folders,
            scopes,
            thumb: super::GALLERY_THUMB,
        });
    }
    Err("The index was being replaced; try again in a moment".into())
}

// ---------- the end-to-end suite's files ----------

/// A picture painted pixel by pixel.
fn painted(width: u32, height: u32, paint: impl Fn(f32, f32) -> [u8; 3]) -> Bitmap {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = paint(x as f32 / width as f32, y as f32 / height as f32);
            pixels.extend_from_slice(&[b, g, r, 255]);
        }
    }
    Bitmap { width, height, pixels }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

fn set_modified(path: &std::path::Path, unix_secs: u64) -> Result<(), String> {
    let file = std::fs::File::options().write(true).open(path).map_err(|e| e.to_string())?;
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs)).map_err(|e| e.to_string())
}

/// Writes the suite's files into `folder`: two photos from a trip in July 2024 (one with a GPS
/// position, one turned upright by its orientation), a screenshot of a receipt last changed in
/// March 2025, and a three-second video last changed in December 2023. Returns their paths.
pub fn fixtures(folder: &std::path::Path) -> Result<Vec<String>, String> {
    let trip = folder.join("trip");
    let screens = folder.join("screens");
    let clips = folder.join("clips");
    for dir in [&trip, &screens, &clips] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let beach = painted(1600, 1067, |x, y| {
        if y < 0.55 {
            mix([150, 200, 240], [40, 110, 190], 1.0 - y / 0.55)
        } else if y < 0.75 {
            mix([20, 120, 140], [40, 150, 160], (y - 0.55) / 0.2 + x * 0.1)
        } else {
            mix([225, 200, 150], [200, 170, 120], (y - 0.75) / 0.25)
        }
    });
    write_jpeg(&trip.join("beach.jpg"), &beach, &Exif {
        make: Some("Kestrel".into()),
        model: Some("Test Camera 1".into()),
        taken: Some("2024:07:14 10:30:00".into()),
        orientation: Some(1),
        gps: Some((51.5007, -0.1246)),
    })?;
    // Stored on its side, 1200 by 900, and shown upright, 900 by 1200.
    let tower = painted(1200, 900, |x, y| {
        let sky = mix([250, 190, 120], [120, 80, 150], x);
        if (0.35..0.65).contains(&y) && x > 0.2 { [50, 45, 60] } else { sky }
    });
    write_jpeg(&trip.join("tower.jpg"), &tower, &Exif {
        make: Some("Google".into()),
        model: Some("Pixel 8".into()),
        taken: Some("2024:07:15 18:05:00".into()),
        orientation: Some(6),
        gps: None,
    })?;
    let receipt = text_bitmap(&["COSTCO WHOLESALE", "Order 42.17", "Thank you for shopping"], 1100, 420)?;
    let receipt_path = screens.join("receipt.png");
    std::fs::write(&receipt_path, receipt.to_png()?).map_err(|e| e.to_string())?;
    // 10 March 2025, noon UTC.
    set_modified(&receipt_path, 1_741_608_000)?;
    let video_path = clips.join("walk.mp4");
    write_video(&video_path, 640, 360, 15, 45, |n| {
        let shift = n as f32 / 45.0;
        painted(640, 360, move |x, y| mix([30, 90, 60], [200, 220, 120], ((x + shift) % 1.0 + y) / 2.0))
    })?;
    // 24 December 2023, noon UTC.
    set_modified(&video_path, 1_703_419_200)?;
    Ok([trip.join("beach.jpg"), trip.join("tower.jpg"), receipt_path, video_path].iter().map(|p| p.to_string_lossy().into_owned()).collect())
}
