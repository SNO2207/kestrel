//! A squarified treemap of a folder's subtree (Bruls, Huizing and van Wijk, "Squarified
//! Treemaps", 2000): each item gets a rectangle whose area is proportional to its size, laid
//! out in rows that keep the rectangles as close to square as the algorithm can.
//!
//! Only what can be seen is laid out. A folder's contents are placed inside it (below a strip
//! for its name when it is big enough) only while the space inside is at least the smallest
//! area worth drawing, and items smaller than that are drawn together as one "smaller items"
//! rectangle per folder. The work therefore depends on the size of the picture rather than of
//! the drive: a few thousand folders' children are looked at, whatever lies below them.

use std::collections::VecDeque;

use kestrel_index::{EntryId, Index, NO_ENTRY};
use serde::Serialize;

use crate::Measure;
use crate::category::{Category, Classifier, Place};

/// Marks a tile with no parent tile: it lies directly in the frame.
pub const NO_TILE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn area(&self) -> f64 {
        self.w * self.h
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub width: f64,
    pub height: f64,
    /// The smallest rectangle worth drawing, in square pixels.
    pub min_area: f64,
    /// Folder levels laid out below the root, at most.
    pub max_depth: u32,
    /// Space between a folder's edge and its contents.
    pub padding: f64,
    /// Height of the strip that carries an open folder's name, when the folder is tall and
    /// wide enough to spare it.
    pub header: f64,
    pub measure: Measure,
    /// Tiles at most; folders beyond this stay closed.
    pub max_tiles: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1200.0,
            height: 800.0,
            min_area: 24.0,
            max_depth: 32,
            padding: 2.0,
            header: 16.0,
            measure: Measure::Allocated,
            max_tiles: 40_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TileKind {
    File,
    Folder,
    /// The items of a folder too small to draw one by one.
    Rest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// The entry drawn; `NO_ENTRY` for a `Rest` tile.
    pub entry: EntryId,
    /// The folder tile this lies in, or [`NO_TILE`].
    pub parent: u32,
    pub rect: Rect,
    /// 0 for the root's own children.
    pub depth: u32,
    pub kind: TileKind,
    /// A file's type; for a folder, what its location makes everything in it (`None` for most).
    pub category: Option<Category>,
    /// The item's size by the layout's measure.
    pub bytes: u64,
    /// Items in a `Rest` tile.
    pub count: u32,
    /// A folder whose contents are laid out inside it.
    pub open: bool,
    /// An open folder with a strip for its name along its top edge.
    pub header: bool,
}

#[derive(Debug)]
pub struct Treemap {
    pub tiles: Vec<Tile>,
    /// The root's size by the layout's measure.
    pub total: u64,
    /// Some folders big enough to open were left closed because of `max_tiles`.
    pub truncated: bool,
}

/// Lays out the subtree of `root` in a frame of `options.width` by `options.height`.
pub fn layout(index: &Index, root: EntryId, options: &Options) -> Treemap {
    let frame = Rect { x: 0.0, y: 0.0, w: options.width.max(0.0), h: options.height.max(0.0) };
    let classifier = Classifier::new(index);
    let mut tiles: Vec<Tile> = Vec::new();
    let mut truncated = false;
    if !index.is_dir(root) || frame.area() < options.min_area {
        return Treemap { tiles, total: options.measure.of(index, root), truncated };
    }
    let mut queue: VecDeque<(EntryId, Rect, u32, u32, Place)> = VecDeque::new();
    queue.push_back((root, frame, 0, NO_TILE, classifier.place_of(index, root)));
    let mut items: Vec<(u64, EntryId)> = Vec::new();
    let mut areas: Vec<f64> = Vec::new();
    let mut rects: Vec<Rect> = Vec::new();

    // Breadth first, so that if the tile budget runs out it is the deepest levels that go.
    while let Some((dir, rect, depth, parent_tile, place)) = queue.pop_front() {
        if tiles.len() >= options.max_tiles {
            truncated = true;
            break;
        }
        items.clear();
        let mut total: u64 = 0;
        for child in index.children(dir) {
            let value = options.measure.of(index, child);
            if value > 0 {
                items.push((value, child));
                total += value;
            }
        }
        if total == 0 {
            continue;
        }
        let scale = rect.area() / total as f64;
        // Items worth a rectangle of their own go first, largest first (ties by id, so the
        // picture is the same every time); the rest are summed into one rectangle at the end.
        let mut big = 0;
        for i in 0..items.len() {
            if items[i].0 as f64 * scale >= options.min_area {
                items.swap(i, big);
                big += 1;
            }
        }
        items[..big].sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let rest: u64 = items[big..].iter().map(|item| item.0).sum();
        areas.clear();
        areas.extend(items[..big].iter().map(|&(value, _)| value as f64 * scale));
        if rest > 0 {
            areas.push(rest as f64 * scale);
        }
        squarify(&areas, rect, &mut rects);
        if parent_tile != NO_TILE {
            tiles[parent_tile as usize].open = true;
        }

        for (k, &r) in rects.iter().enumerate() {
            if k >= big {
                tiles.push(Tile {
                    entry: NO_ENTRY,
                    parent: parent_tile,
                    rect: r,
                    depth,
                    kind: TileKind::Rest,
                    category: None,
                    bytes: rest,
                    count: (items.len() - big) as u32,
                    open: false,
                    header: false,
                });
                continue;
            }
            let (value, id) = items[k];
            let tile_index = tiles.len() as u32;
            if index.is_dir(id) {
                let child_place = classifier.child_place(index, dir, place, id);
                let inner = inset(r, options);
                tiles.push(Tile {
                    entry: id,
                    parent: parent_tile,
                    rect: r,
                    depth,
                    kind: TileKind::Folder,
                    category: child_place.category(),
                    bytes: value,
                    count: 0,
                    open: false,
                    header: inner.is_some_and(|(_, header)| header),
                });
                if let Some((inner, _)) = inner
                    && depth + 1 < options.max_depth
                    && inner.area() >= options.min_area
                {
                    queue.push_back((id, inner, depth + 1, tile_index, child_place));
                }
            } else {
                tiles.push(Tile {
                    entry: id,
                    parent: parent_tile,
                    rect: r,
                    depth,
                    kind: TileKind::File,
                    category: Some(classifier.file(index, id, place)),
                    bytes: value,
                    count: 0,
                    open: false,
                    header: false,
                });
            }
        }
    }
    // A folder queued but never reached keeps no strip for a name it doesn't need.
    for tile in tiles.iter_mut().filter(|tile| tile.kind == TileKind::Folder && !tile.open) {
        tile.header = false;
    }
    Treemap { tiles, total: options.measure.of(index, root), truncated }
}

/// The space inside a folder's rectangle for its contents, and whether a strip for its name is
/// kept along the top. `None` if nothing fits.
fn inset(r: Rect, options: &Options) -> Option<(Rect, bool)> {
    let header = r.h >= options.header * 3.0 && r.w >= options.header * 4.0;
    let top = options.padding + if header { options.header } else { 0.0 };
    let inner = Rect {
        x: r.x + options.padding,
        y: r.y + top,
        w: r.w - 2.0 * options.padding,
        h: r.h - top - options.padding,
    };
    (inner.w >= 1.0 && inner.h >= 1.0).then_some((inner, header))
}

/// The worst aspect ratio of a row of rectangles with these largest and smallest areas and
/// this total, laid along a side of length `side`.
fn worst(max: f64, min: f64, sum: f64, side: f64) -> f64 {
    let (side2, sum2) = (side * side, sum * sum);
    (side2 * max / sum2).max(sum2 / (side2 * min))
}

/// Divides `frame` into rectangles with the given areas, in order, which should add up to the
/// frame's area and are best given largest first. Rows go along the shorter side; a row takes
/// items for as long as that makes its worst aspect ratio no worse.
pub fn squarify(areas: &[f64], frame: Rect, out: &mut Vec<Rect>) {
    out.clear();
    let mut free = frame;
    let mut start = 0;
    while start < areas.len() {
        let short = free.w.min(free.h);
        if short <= 0.0 {
            out.extend((start..areas.len()).map(|_| Rect { x: free.x, y: free.y, w: 0.0, h: 0.0 }));
            return;
        }
        let (mut max, mut min, mut sum) = (areas[start], areas[start], areas[start]);
        let mut ratio = worst(max, min, sum, short);
        let mut end = start + 1;
        while end < areas.len() {
            let area = areas[end];
            let next = worst(max.max(area), min.min(area), sum + area, short);
            if next > ratio {
                break;
            }
            (max, min, sum, ratio) = (max.max(area), min.min(area), sum + area, next);
            end += 1;
        }
        // The last row takes whatever is left, so rounding never leaves a sliver uncovered.
        let last_row = end == areas.len();
        let row = &areas[start..end];
        // The last item of the row likewise takes the rest of its length.
        let last = row.len() - 1;
        if free.w >= free.h {
            // A column along the left edge.
            let width = if last_row { free.w } else { (sum / free.h).min(free.w) };
            let mut y = free.y;
            for (k, &area) in row.iter().enumerate() {
                let h = if k == last { free.bottom() - y } else { area / width };
                out.push(Rect { x: free.x, y, w: width, h });
                y += h;
            }
            free.x += width;
            free.w -= width;
        } else {
            // A row along the top edge.
            let height = if last_row { free.h } else { (sum / free.w).min(free.h) };
            let mut x = free.x;
            for (k, &area) in row.iter().enumerate() {
                let w = if k == last { free.right() - x } else { area / height };
                out.push(Rect { x, y: free.y, w, h: height });
                x += w;
            }
            free.y += height;
            free.h -= height;
        }
        start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{F, build, id};
    use kestrel_index::attr;

    const EPSILON: f64 = 1e-6;

    fn overlap(a: &Rect, b: &Rect) -> f64 {
        let w = a.right().min(b.right()) - a.x.max(b.x);
        let h = a.bottom().min(b.bottom()) - a.y.max(b.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    fn inside(inner: &Rect, outer: &Rect) -> bool {
        inner.x >= outer.x - EPSILON
            && inner.y >= outer.y - EPSILON
            && inner.right() <= outer.right() + EPSILON
            && inner.bottom() <= outer.bottom() + EPSILON
    }

    fn assert_partition(rects: &[Rect], areas: &[f64], frame: Rect) {
        assert_eq!(rects.len(), areas.len());
        for (r, &area) in rects.iter().zip(areas) {
            assert!((r.area() - area).abs() < 1e-6 * frame.area().max(1.0), "{r:?} should have area {area}");
            assert!(inside(r, &frame), "{r:?} lies outside {frame:?}");
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(overlap(a, b) < EPSILON, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn squarify_partitions_the_frame_in_proportion() {
        // The example of the paper: a 6 by 4 rectangle and areas 6, 6, 4, 3, 2, 2, 1.
        let frame = Rect { x: 0.0, y: 0.0, w: 6.0, h: 4.0 };
        let areas = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let mut rects = Vec::new();
        squarify(&areas, frame, &mut rects);
        assert_partition(&rects, &areas, frame);
        // The first row is the paper's: the two sixes stacked in a column 3 wide.
        assert_eq!((rects[0].w, rects[0].h, rects[1].y), (3.0, 2.0, 2.0));

        // Equal items in a square come out square.
        let frame = Rect { x: 10.0, y: 20.0, w: 400.0, h: 400.0 };
        let areas = vec![10_000.0; 16];
        squarify(&areas, frame, &mut rects);
        assert_partition(&rects, &areas, frame);
        for r in &rects {
            assert!((r.w / r.h - 1.0).abs() < 1e-9, "{r:?} is not square");
        }

        // A long, uneven list in a wide frame.
        let frame = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let raw: Vec<f64> = (1..=300).map(|n| 1.0 / n as f64).collect();
        let total: f64 = raw.iter().sum();
        let areas: Vec<f64> = raw.iter().map(|a| a / total * frame.area()).collect();
        squarify(&areas, frame, &mut rects);
        assert_partition(&rects, &areas, frame);
    }

    /// Every tile lies inside its parent's contents (or the frame), siblings never overlap, and
    /// each open folder's contents divide its inner space in proportion to their sizes.
    fn assert_well_formed(map: &Treemap, options: &Options) {
        let frame = Rect { x: 0.0, y: 0.0, w: options.width, h: options.height };
        let mut by_parent: std::collections::HashMap<u32, Vec<&Tile>> = Default::default();
        for tile in &map.tiles {
            by_parent.entry(tile.parent).or_default().push(tile);
        }
        for (parent, children) in &by_parent {
            let (space, total) = if *parent == NO_TILE {
                (frame, map.total)
            } else {
                let folder = &map.tiles[*parent as usize];
                assert!(folder.open && folder.kind == TileKind::Folder);
                (inset(folder.rect, options).unwrap().0, folder.bytes)
            };
            let summed: u64 = children.iter().map(|tile| tile.bytes).sum();
            assert_eq!(summed, total, "a folder's tiles add up to its size");
            for (i, a) in children.iter().enumerate() {
                assert!(inside(&a.rect, &space), "{:?} lies outside {space:?}", a.rect);
                let share = a.bytes as f64 / total as f64;
                assert!((a.rect.area() / space.area() - share).abs() < 1e-6, "area in proportion to size");
                for b in &children[i + 1..] {
                    assert!(overlap(&a.rect, &b.rect) < EPSILON, "{:?} overlaps {:?}", a.rect, b.rect);
                }
            }
        }
    }

    fn sample() -> kestrel_index::Index {
        let mut files = vec![
            F(r"big\movie.mkv", 40_000_000, 0, 0),
            F(r"big\song.mp3", 6_000_000, 0, 0),
            F(r"docs\a.pdf", 3_000_000, 0, 0),
            F(r"docs\deeper\b.docx", 2_000_000, 0, 0),
            F(r"docs\deeper\deepest\c.txt", 1_000_000, 0, 0),
            F("loose.bin", 9_000_000, 0, 0),
            F(r"cloud\online.mp4", 50_000_000, 0, attr::RECALL_ON_DATA_ACCESS),
        ];
        // Many crumbs, each far too small to draw on its own.
        let crumbs: Vec<String> = (0..500).map(|n| format!(r"crumbs\c{n}.txt")).collect();
        files.extend(crumbs.iter().map(|path| F(path, 1_000, 0, 0)));
        build(&files, &[r"empty"])
    }

    #[test]
    fn layout_is_proportional_nested_and_deterministic() {
        let index = sample();
        let root = id(&index, r"C:\");
        let options = Options { width: 800.0, height: 600.0, ..Options::default() };
        let map = layout(&index, root, &options);
        assert_well_formed(&map, &options);
        assert_eq!(map.total, index.alloc(root));
        assert!(!map.truncated);

        let tile = |name: &str| map.tiles.iter().find(|t| t.entry != NO_ENTRY && index.name(t.entry) == name).unwrap();
        assert_eq!(tile("movie.mkv").category, Some(Category::Video));
        assert_eq!(tile("big").kind, TileKind::Folder);
        assert!(tile("big").open && tile("big").header);
        assert_eq!(tile("deepest").depth, 2);
        // The crumbs are one "smaller items" tile in their folder.
        let crumbs = tile("crumbs");
        let rest: Vec<&Tile> = map.tiles.iter().filter(|t| t.kind == TileKind::Rest).collect();
        assert_eq!(rest.len(), 1);
        assert_eq!((rest[0].count, rest[0].bytes), (500, 500_000));
        assert_eq!(&map.tiles[rest[0].parent as usize], crumbs);
        // Online-only files take no space on disk, so they get no area; the empty folder neither.
        assert!(map.tiles.iter().all(|t| t.entry == NO_ENTRY || !matches!(index.name(t.entry), "online.mp4" | "cloud" | "empty")));

        let again = layout(&index, root, &options);
        assert_eq!(again.tiles, map.tiles, "the same index lays out the same every time");
    }

    #[test]
    fn logical_sizes_count_cloud_files_and_depth_is_limited() {
        let index = sample();
        let root = id(&index, r"C:\");
        let options = Options { width: 640.0, height: 480.0, measure: Measure::Logical, max_depth: 2, ..Options::default() };
        let map = layout(&index, root, &options);
        assert_well_formed(&map, &options);
        assert_eq!(map.total, index.size(root));
        assert!(map.tiles.iter().any(|t| t.entry != NO_ENTRY && index.name(t.entry) == "online.mp4"));
        assert!(map.tiles.iter().all(|t| t.depth < 2), "two levels at most");
        let deeper = map.tiles.iter().find(|t| t.entry != NO_ENTRY && index.name(t.entry) == "deeper").unwrap();
        assert!(!deeper.open && !deeper.header, "the last level's folders stay closed");
    }

    #[test]
    fn a_small_frame_or_tile_budget_keeps_folders_closed() {
        let index = sample();
        let root = id(&index, r"C:\");
        let options = Options { width: 800.0, height: 600.0, max_tiles: 4, ..Options::default() };
        let map = layout(&index, root, &options);
        assert!(map.truncated);
        assert!(map.tiles.iter().all(|t| t.parent == NO_TILE && !t.open));
        assert_well_formed(&map, &options);

        // Below the smallest area nothing is drawn; a file is no folder to look into.
        assert!(layout(&index, root, &Options { width: 4.0, height: 4.0, ..Options::default() }).tiles.is_empty());
        assert!(layout(&index, id(&index, r"C:\loose.bin"), &options).tiles.is_empty());
    }

    #[test]
    fn a_folder_root_takes_its_place_from_its_location() {
        let index = build(&[F(r"Windows\System32\drivers\x.dat", 4096, 0, 0), F(r"Windows\System32\y.mp4", 8192, 0, 0)], &[]);
        let map = layout(&index, id(&index, r"C:\Windows\System32"), &Options::default());
        assert!(map.tiles.iter().all(|t| t.category == Some(Category::System)));
    }
}
