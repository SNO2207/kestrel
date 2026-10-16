//! The commands behind the window's layout: grouping a listing (Explorer's "Group by"), the
//! filter box, copying and moving to the other pane, the Content view's image sizes, and the
//! layout kept between runs (panes, tabs and columns, in `kestrel_db::layout`).
//!
//! Result sets stay in the engine: grouping rearranges a set and keeps where each group starts
//! (`Engine::group_set`), the list asks for the names and counts of only the groups it shows
//! (`group_headers`), and the filter makes a narrowed copy of the listing it was given
//! (`Engine::filter_set`), which it can narrow again as the text changes.

use std::fs::File;
use std::io::Read;
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_query::{GroupBy, Grouping, NameFilter};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, Window};
use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};

use crate::engine::{Engine, GroupHeader, Listing, Marks, SortSpec};
use crate::fileops::{self, Done, Options, Request, Transfer};
use crate::undo::Journal;
use crate::watch::Watcher;

type Shared<'a> = State<'a, Arc<Engine>>;

/// Reading these files' data fetches them from the cloud first, so their headers are never read.
const RECALL: u32 = 0x1000 | 0x4_0000 | 0x40_0000;
const DIRECTORY: u32 = 0x10;
/// How much of an image is read for its size; JPEG files can keep it behind their metadata.
const HEADER_BYTES: u64 = 256 << 10;
/// Image sizes asked for at once: the Content view asks for the rows on screen.
const MAX_IMAGE_SIZES: usize = 256;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff", "psd"];

/// "Group by" as the front end sends it.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct GroupSpec {
    pub by: GroupField,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupField {
    Name,
    Modified,
    Type,
    Size,
}

impl GroupSpec {
    fn grouping(self) -> Grouping {
        let by = match self.by {
            GroupField::Name => GroupBy::Name,
            GroupField::Modified => GroupBy::Modified,
            GroupField::Type => GroupBy::Type,
            GroupField::Size => GroupBy::Size,
        };
        Grouping { by, descending: self.desc }
    }
}

#[derive(Serialize)]
pub struct Grouped {
    /// Where the marked rows are now.
    pub marks: Marks,
    /// The first row of each group, in order; empty when the set is not grouped.
    pub starts: Vec<u32>,
}

/// Sorts a set and groups it, or ungroups it when `group` is null. Its rows keep their places
/// within each group as the sort put them.
#[tauri::command(async)]
pub fn group_set(engine: Shared, set: u32, sort: SortSpec, group: Option<GroupSpec>, marks: Marks) -> Result<Grouped, String> {
    let (marks, starts) = engine.group_set(set, sort, group.map(GroupSpec::grouping), &marks)?;
    Ok(Grouped { marks, starts })
}

/// The names and sizes of `count` groups of a set from the group `from`, for the headers in view.
#[tauri::command(async)]
pub fn group_headers(engine: Shared, set: u32, from: u32, count: u32) -> Result<Vec<GroupHeader>, String> {
    engine.group_headers(set, from, count.min(4096))
}

#[derive(Serialize)]
pub struct Filtered {
    pub listing: Listing,
    pub starts: Vec<u32>,
}

/// The rows of the set `base` whose names contain `text`, or match it when it holds `*` or `?`,
/// as a new set sorted and grouped as asked. `base` stays as it was.
#[tauri::command(async)]
pub fn filter_set(
    engine: Shared,
    base: u32,
    text: String,
    sort: SortSpec,
    group: Option<GroupSpec>,
) -> Result<Filtered, String> {
    let (listing, starts) = engine.filter_set(base, &NameFilter::new(&text), sort, group.map(GroupSpec::grouping))?;
    Ok(Filtered { listing, starts })
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransferMode {
    Copy,
    Move,
}

/// Copies or moves the rows of a set into `destination`, the folder of the other pane, with the
/// shell's progress and conflict windows. The operation goes into the undo journal, as a paste
/// does. Returns the names the items got there.
#[tauri::command(async)]
pub fn transfer_items(
    window: Window,
    engine: Shared,
    journal: State<'_, Journal>,
    set: u32,
    ranges: Vec<(u32, u32)>,
    destination: String,
    mode: TransferMode,
) -> Result<Vec<String>, String> {
    let sources = engine.paths(set, &ranges)?;
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let kind = if mode == TransferMode::Copy { Transfer::Copy } else { Transfer::Move };
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    let outcome = fileops::execute(Request::Transfer { sources, destination, kind }, Options { owner, quiet: false })?;
    journal.record(&outcome);
    Ok(transferred_names(outcome.results()))
}

/// The names copied or moved items ended up with, "name - Copy" and conflict renames included.
fn transferred_names<'a>(done: impl Iterator<Item = &'a Done>) -> Vec<String> {
    done.filter_map(|done| match done {
        Done::Copied { to, .. } | Done::Moved { to, .. } => Some(fileops::file_name(to).to_owned()),
        _ => None,
    })
    .collect()
}

/// The pixel size of each image, read from its header, or null for anything else: folders,
/// other files, and files whose contents are only in the cloud, which are never opened.
#[tauri::command(async)]
pub fn image_sizes(paths: Vec<String>) -> Vec<Option<(u32, u32)>> {
    paths.iter().take(MAX_IMAGE_SIZES).map(|path| image_size(path)).collect()
}

fn image_size(path: &str) -> Option<(u32, u32)> {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase())?;
    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call, which only reads the attributes.
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES || attrs & (RECALL | DIRECTORY) != 0 {
        return None;
    }
    let mut header = Vec::new();
    File::open(path).ok()?.take(HEADER_BYTES).read_to_end(&mut header).ok()?;
    crate::preview::image_dimensions(&header)
}

/// For each path, whether it is a folder that exists, read without opening it: a tab kept from
/// the last run on a folder deleted since opens elsewhere rather than on an error.
#[tauri::command(async)]
pub fn folders_exist(paths: Vec<String>) -> Vec<bool> {
    paths.iter().map(|path| is_folder(path)).collect()
}

fn is_folder(path: &str) -> bool {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call, which only reads the attributes.
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & DIRECTORY != 0
}

/// Watches the folder the other pane shows, as `list_dir` watches the active tab's, so both
/// panes refresh themselves when their folders change.
#[derive(Default)]
pub struct OtherPaneWatcher(Watcher);

/// Watches `path` for the other pane from now on, or nothing when it is null. The front end
/// calls this before listing that pane's folder, so no change falls between the two.
#[tauri::command(async)]
pub fn watch_other_pane(app: AppHandle, watcher: State<'_, OtherPaneWatcher>, path: Option<String>) {
    match path {
        Some(path) => watcher.0.watch(&crate::engine::normalize(&path), move |changed| {
            let _ = app.emit("dir-changed", changed);
        }),
        None => watcher.0.stop(),
    }
}

/// Watches `path` as the active tab's folder, as `list_dir` would, without listing it again:
/// for a pane that takes the keyboard while already showing a watched folder.
#[tauri::command(async)]
pub fn watch_active_folder(app: AppHandle, watcher: State<'_, Watcher>, path: String) {
    watcher.watch(&crate::engine::normalize(&path), move |changed| {
        let _ = app.emit("dir-changed", changed);
    });
}

/// Every saved part of the layout, by name, as the JSON the front end saved.
#[tauri::command(async)]
pub fn layout_load(db: State<'_, Arc<Db>>) -> Result<Vec<(String, String)>, String> {
    Ok(kestrel_db::layout::load(&db)?)
}

/// Saves one part of the layout; an empty value forgets it.
#[tauri::command(async)]
pub fn layout_save(db: State<'_, Arc<Db>>, name: String, value: String) -> Result<(), String> {
    Ok(kestrel_db::layout::save(&db, &name, &value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SortField;
    use kestrel_index::{IndexBuilder, walk};
    use std::fs;

    const BY_NAME: SortSpec = SortSpec { key: SortField::Name, desc: false };

    fn names(engine: &Engine, set: u32) -> Vec<String> {
        engine.rows(set, 0, 100).unwrap().into_iter().map(|row| row.name).collect()
    }

    /// A scratch folder with files of known sizes, indexed, and the engine holding it.
    fn folder(name: &str, files: &[(&str, usize)]) -> (Engine, std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("kestrel-layout-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Sub")).unwrap();
        for (file, size) in files {
            fs::write(root.join(file), vec![b'x'; *size]).unwrap();
        }
        let path = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        walk::walk(&path, &mut builder).unwrap();
        engine.install(builder.finish());
        (engine, root, path)
    }

    #[test]
    fn grouping_a_listing_keeps_the_sort_inside_each_group_and_follows_a_resort() {
        let (engine, root, path) = folder("group", &[("b.txt", 20_000), ("a.txt", 10), ("c.rs", 0), ("d.rs", 5)]);
        let listing = engine.list_dir(&path, BY_NAME).unwrap();
        let by_type = Some(Grouping { by: GroupBy::Type, descending: false });
        let marks = Marks { ranges: vec![(1, 1)], focus: Some(1) };
        // Sub, a.txt, b.txt, c.rs, d.rs; a.txt is selected.
        let (marks, starts) = engine.group_set(listing.set, BY_NAME, by_type, &marks).unwrap();
        assert_eq!(names(&engine, listing.set), ["Sub", "c.rs", "d.rs", "a.txt", "b.txt"]);
        assert_eq!(starts, [0, 1, 3]);
        assert_eq!((marks.ranges, marks.focus), (vec![(3, 3)], Some(3)), "the selection follows its file");
        let headers = engine.group_headers(listing.set, 0, 10).unwrap();
        let summary: Vec<(&str, u32, u32)> = headers.iter().map(|h| (h.label.as_str(), h.start, h.count)).collect();
        assert_eq!(summary, [("Folder", 0, 1), ("RS file", 1, 2), ("TXT file", 3, 2)]);
        assert_eq!(engine.group_headers(listing.set, 2, 5).unwrap().len(), 1);

        // Sorting by size, largest first, sorts within the groups.
        let by_size = SortSpec { key: SortField::Size, desc: true };
        engine.sort_set(listing.set, by_size, &Marks::default()).unwrap();
        assert_eq!(names(&engine, listing.set), ["Sub", "d.rs", "c.rs", "b.txt", "a.txt"]);

        // By size, descending groups: the largest group first, folders last.
        let by_size_groups = Some(Grouping { by: GroupBy::Size, descending: true });
        let (_, starts) = engine.group_set(listing.set, BY_NAME, by_size_groups, &Marks::default()).unwrap();
        assert_eq!(names(&engine, listing.set), ["b.txt", "a.txt", "d.rs", "c.rs", "Sub"]);
        assert_eq!(starts, [0, 1, 3, 4]);

        // Ungrouping restores the plain sort, folders first.
        let (_, starts) = engine.group_set(listing.set, BY_NAME, None, &Marks::default()).unwrap();
        assert!(starts.is_empty());
        assert_eq!(names(&engine, listing.set), ["Sub", "a.txt", "b.txt", "c.rs", "d.rs"]);
        assert!(engine.group_headers(listing.set, 0, 10).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_filter_narrows_a_copy_of_the_listing() {
        let (engine, root, path) = folder("filter", &[("report-2026.pdf", 1), ("Report.txt", 2), ("notes.txt", 3)]);
        let listing = engine.list_dir(&path, BY_NAME).unwrap();
        let (narrowed, _) = engine.filter_set(listing.set, &NameFilter::new("REPORT"), BY_NAME, None).unwrap();
        assert_eq!(names(&engine, narrowed.set), ["report-2026.pdf", "Report.txt"]);
        assert_eq!(narrowed.total, 2);
        let (wild, _) = engine.filter_set(listing.set, &NameFilter::new("*.txt"), BY_NAME, None).unwrap();
        assert_eq!(names(&engine, wild.set), ["notes.txt", "Report.txt"]);
        let grouped = Some(Grouping { by: GroupBy::Type, descending: true });
        let (by_type, starts) = engine.filter_set(listing.set, &NameFilter::new("r"), BY_NAME, grouped).unwrap();
        assert_eq!(names(&engine, by_type.set), ["Report.txt", "report-2026.pdf"]);
        assert_eq!(starts, [0, 1]);
        assert_eq!(engine.rows(listing.set, 0, 10).unwrap().len(), 4, "the listing itself is untouched");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folders_outside_the_index_group_and_filter_too() {
        let root = std::env::temp_dir().join(format!("kestrel-layout-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Zeta")).unwrap();
        fs::write(root.join("alpha.TXT"), b"a").unwrap();
        fs::write(root.join("beta.md"), b"").unwrap();
        let engine = Engine::default();
        let listing = engine.list_dir(root.to_str().unwrap(), BY_NAME).unwrap();
        let by_name = Some(Grouping { by: GroupBy::Name, descending: false });
        let (_, starts) = engine.group_set(listing.set, BY_NAME, by_name, &Marks::default()).unwrap();
        assert_eq!(names(&engine, listing.set), ["alpha.TXT", "beta.md", "Zeta"]);
        assert_eq!(starts, [0, 2]);
        let labels: Vec<String> = engine.group_headers(listing.set, 0, 9).unwrap().into_iter().map(|h| h.label).collect();
        assert_eq!(labels, ["A - H", "Q - Z"]);
        let (narrowed, _) = engine.filter_set(listing.set, &NameFilter::new("TXT"), BY_NAME, None).unwrap();
        assert_eq!(names(&engine, narrowed.set), ["alpha.TXT"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rows_carry_the_extra_columns_and_sort_by_them() {
        let (engine, root, path) = folder("columns", &[("big.bin", 9000), ("small.bin", 1)]);
        let listing = engine.list_dir(&path, SortSpec { key: SortField::Alloc, desc: true }).unwrap();
        let rows = engine.rows(listing.set, 0, 10).unwrap();
        assert_eq!(rows[0].name, "Sub", "folders first");
        assert_eq!(rows[1].name, "big.bin");
        assert!(rows[1].alloc >= 9000, "allocated size {}", rows[1].alloc);
        assert!(rows[0].attrs & 0x10 != 0, "a folder's attributes say so");
        let search = engine.search("bin", Some(&path), SortSpec { key: SortField::Path, desc: false }).unwrap();
        assert_eq!(names(&engine, search.set), ["big.bin", "small.bin"]);
        engine.sort_set(search.set, SortSpec { key: SortField::Extension, desc: false }, &Marks::default()).unwrap();
        engine.sort_set(search.set, SortSpec { key: SortField::Attributes, desc: false }, &Marks::default()).unwrap();
        assert_eq!(engine.rows(search.set, 0, 10).unwrap().len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn image_sizes_read_headers_of_images_only() {
        let root = std::env::temp_dir().join(format!("kestrel-layout-images-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // A GIF's header holds its size at bytes 6 to 9.
        fs::write(root.join("dot.gif"), [b"GIF89a".as_slice(), &[3, 0, 2, 0], &[0; 16]].concat()).unwrap();
        fs::write(root.join("dot.txt"), b"GIF89a\x03\x00\x02\x00").unwrap();
        let path = |name: &str| root.join(name).to_str().unwrap().to_owned();
        let sizes = image_sizes(vec![path("dot.gif"), path("dot.txt"), path("missing.png"), root.to_str().unwrap().to_owned()]);
        assert_eq!(sizes, [Some((3, 2)), None, None, None]);
        let found = folders_exist(vec![root.to_str().unwrap().to_owned(), path("dot.gif"), path("gone"), r"C:\".to_owned()]);
        assert_eq!(found, [true, false, false, true]);
        fs::remove_dir_all(&root).unwrap();
    }
}
