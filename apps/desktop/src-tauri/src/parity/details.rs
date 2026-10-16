//! What the details pane (Alt+Shift+P) shows beyond what the preview's details block already
//! reads (`preview::describe`): a file's properties from the Windows property system, such as
//! a video's length or a photo's dimensions, and the totals of a selection of many items.
//!
//! Properties come from the file's property handler, which reads the start of the file. That
//! is cheap for a local file, but a cloud-only file would be downloaded, so those are skipped,
//! and a handler that takes too long is given up on.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::UI::Shell::PropertiesSystem::{
    GPS_DEFAULT, IPropertyDescription, IPropertyStore, PDFF_DEFAULT, PSFormatForDisplayAlloc, PSGetPropertyDescription,
    PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName,
};
use windows::core::{HSTRING, Interface, PWSTR};

use super::{CLOUD_ONLY, DIRECTORY, attributes};
use crate::engine::Engine;

/// The properties shown, by their canonical names, in the order shown. Each file type has some
/// of them; Windows names them in the user's language.
const PROPERTIES: &[&str] = &[
    "System.Media.Duration",
    "System.Image.Dimensions",
    "System.Video.FrameWidth",
    "System.Video.FrameHeight",
    "System.Video.FrameRate",
    "System.Audio.EncodingBitrate",
    "System.Title",
    "System.Author",
    "System.Music.Artist",
    "System.Music.AlbumTitle",
    "System.Document.PageCount",
    "System.Photo.DateTaken",
    "System.Photo.CameraModel",
];
/// How long a property handler may take before the pane goes without its answer.
const PATIENCE: Duration = Duration::from_secs(3);
/// Selections larger than this are counted, but their sizes and dates are not added up.
const MAX_SUMMED: u64 = 200_000;
/// Rows added up at a time, each time with the index held: about a quarter of a millisecond.
const CHUNK: u32 = 65_536;

/// One property, as Windows labels and formats it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Property {
    pub label: String,
    pub value: String,
}

/// A string the shell allocated, copied and freed.
///
/// # Safety
/// `text` must be null or a NUL-terminated string allocated with `CoTaskMemAlloc`.
unsafe fn take(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: as the caller promises, `text` is a valid string, freed exactly once here.
    unsafe {
        let copy = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        copy
    }
}

/// Reads the properties of a local file. COM must be initialised on the calling thread.
fn read(path: &str) -> Vec<Property> {
    // SAFETY: COM is initialised on this thread; each PROPVARIANT is cleared once formatted,
    // and every string the shell allocates is freed by `take`.
    unsafe {
        let Ok(store) = SHGetPropertyStoreFromParsingName::<_, _, IPropertyStore>(&HSTRING::from(path), None, GPS_DEFAULT) else {
            return Vec::new();
        };
        PROPERTIES
            .iter()
            .filter_map(|name| {
                let mut key = PROPERTYKEY::default();
                PSGetPropertyKeyFromName(&HSTRING::from(*name), &mut key).ok()?;
                let mut value = store.GetValue(&key).ok()?;
                let shown = PSFormatForDisplayAlloc(&key, &value, PDFF_DEFAULT).ok().and_then(|text| take(text));
                let _ = PropVariantClear(&mut value);
                let shown = shown.filter(|text| !text.trim().is_empty())?;
                let mut raw = std::ptr::null_mut();
                PSGetPropertyDescription(&key, &IPropertyDescription::IID, &mut raw).ok()?;
                let description = IPropertyDescription::from_raw(raw);
                let label = take(description.GetDisplayName().ok()?)?;
                Some(Property { label, value: shown })
            })
            .collect()
    }
}

/// The properties of the file at `path` that Windows knows, or none for a folder, a missing
/// file or one whose contents are in the cloud only.
pub fn properties(path: &str) -> Vec<Property> {
    let Some(attrs) = attributes(path) else { return Vec::new() };
    if attrs & (DIRECTORY | CLOUD_ONLY) != 0 || !Path::new(path).is_absolute() {
        return Vec::new();
    }
    let (sender, answer) = mpsc::channel();
    let path = path.to_owned();
    // A handler that hangs keeps its thread, but not the pane.
    let started = std::thread::Builder::new().name("kestrel-properties".into()).spawn(move || {
        let read = super::in_apartment(move || Ok(read(&path))).unwrap_or_default();
        let _ = sender.send(read);
    });
    if started.is_err() {
        return Vec::new();
    }
    answer.recv_timeout(PATIENCE).unwrap_or_default()
}

/// What a selection holds, added up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub items: u64,
    pub files: u64,
    pub folders: u64,
    /// Bytes in the files, and in the folders as far as the index knows them.
    pub bytes: u64,
    /// Whether the sizes and dates cover every item; very large selections are only counted.
    pub complete: bool,
    /// The oldest and newest modification time, in milliseconds since the Unix epoch.
    pub modified_from: i64,
    pub modified_to: i64,
}

/// Adds up the rows in `ranges` of a result set.
pub fn summarise(engine: &Engine, set: u32, ranges: &[(u32, u32)]) -> Result<Summary, String> {
    let mut summary = Summary { complete: true, modified_from: i64::MAX, modified_to: i64::MIN, ..Summary::default() };
    for &(start, end) in ranges {
        let mut at = start;
        while at <= end {
            if summary.items >= MAX_SUMMED {
                summary.complete = false;
                summary.items += u64::from(end - at) + 1;
                break;
            }
            let count = (end - at + 1).min(CHUNK).min(u32::try_from(MAX_SUMMED - summary.items).unwrap_or(u32::MAX));
            // Only what is added up is read: no row is made, nor any path, which for a large
            // selection, asked again at every change of it, cost a hundred times as much.
            let read = engine.row_facts(set, at, count, |dir, size, mtime| {
                summary.items += 1;
                if dir { summary.folders += 1 } else { summary.files += 1 }
                summary.bytes += size;
                if mtime > 0 {
                    summary.modified_from = summary.modified_from.min(mtime);
                    summary.modified_to = summary.modified_to.max(mtime);
                }
            })?;
            at = at.saturating_add(read);
            if read < count {
                break;
            }
        }
    }
    if summary.modified_from > summary.modified_to {
        (summary.modified_from, summary.modified_to) = (0, 0);
    }
    Ok(summary)
}

// ---------- commands ----------

/// The Windows properties of one file, for the details pane.
#[tauri::command(async)]
pub fn item_properties(path: String) -> Vec<Property> {
    properties(&path)
}

/// The totals of a selection, for the details pane and the status bar.
#[tauri::command(async)]
pub fn selection_summary(engine: State<Arc<Engine>>, set: u32, ranges: Vec<(u32, u32)>) -> Result<Summary, String> {
    summarise(&engine, set, &ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{SortField, SortSpec};
    use std::fs;

    #[test]
    fn selections_are_added_up() {
        let dir = std::env::temp_dir().join(format!("kestrel-test-summary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), b"12345").unwrap();
        fs::write(dir.join("b.txt"), b"123").unwrap();
        let engine = Engine::default();
        let listing = engine.list_dir(dir.to_str().unwrap(), SortSpec { key: SortField::Name, desc: false }).unwrap();
        // sub, a.txt, b.txt
        let all = summarise(&engine, listing.set, &[(0, 2)]).unwrap();
        assert_eq!((all.items, all.files, all.folders, all.bytes, all.complete), (3, 2, 1, 8, true));
        assert!(all.modified_from > 0 && all.modified_from <= all.modified_to);
        let two = summarise(&engine, listing.set, &[(1, 1), (2, 9)]).unwrap();
        assert_eq!((two.items, two.files, two.bytes), (2, 2, 8));
        assert_eq!(summarise(&engine, listing.set, &[]).unwrap().items, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The status bar adds a selection up again at every change of it: two hundred thousand
    /// search results take a few milliseconds, as many as the index's own figures take to read,
    /// not the hundred and fifty that making a row (with two paths) of each took.
    #[test]
    fn a_large_selection_is_added_up_without_making_rows() {
        let engine = crate::engine::busy::engine(1_000, 300);
        let listing = engine.search("file", None, SortSpec { key: SortField::Name, desc: false }).unwrap();
        assert_eq!(listing.total, 300_000);
        let started = std::time::Instant::now();
        let summary = summarise(&engine, listing.set, &[(0, 299_999)]).unwrap();
        let took = started.elapsed();
        assert_eq!((summary.items, summary.files, summary.complete), (300_000, MAX_SUMMED, false));
        let rows = engine.rows(listing.set, 0, MAX_SUMMED as u32).unwrap();
        assert_eq!(summary.bytes, rows.iter().map(|row| row.size).sum::<u64>());
        assert!(took < std::time::Duration::from_millis(40), "{took:?}");
    }

    #[test]
    fn properties_of_a_picture_are_read_and_folders_have_none() {
        // A 3 x 2 bitmap: Windows' own property handler gives its dimensions.
        let dir = std::env::temp_dir().join(format!("kestrel-test-properties-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let bmp = dir.join("tiny.bmp");
        let mut bytes = Vec::new();
        let pixels = 2 * 12; // two rows of three 24-bit pixels, each row padded to 12 bytes
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(54u32 + pixels).to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&3i32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&24u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 24]);
        bytes.extend(std::iter::repeat_n(0x80u8, pixels as usize));
        fs::write(&bmp, &bytes).unwrap();
        let found = properties(bmp.to_str().unwrap());
        assert!(
            found.iter().any(|property| property.value.contains('3') && property.value.contains('2')),
            "the dimensions are among {found:?}"
        );
        assert!(properties(dir.to_str().unwrap()).is_empty());
        assert!(properties(&format!(r"{}\missing.bmp", dir.display())).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
