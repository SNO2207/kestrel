//! Similar photos: pictures that look nearly the same without being the same file (a burst, the
//! same shot again, an edit, a resized copy), found by comparing their stored vectors. The disk
//! usage hub's Duplicates tab lists them below the exact duplicates, as groups of their own that
//! are never chosen for the owner (`cleanup/`).
//!
//! Only photos already described are compared (the media library's pass describes them in the
//! background), and only the model in use's vectors: nothing is read from disk here. Two photos
//! are alike when their vectors' cosine reaches the model's `near_duplicate` cutoff; groups are
//! the photos linked that way, directly or through each other.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use kestrel_index::{EntryId, Index};
use kestrel_vector::Store;
use serde::Serialize;

use super::Semantic;
use super::stage::volume_code;
use crate::engine::{Engine, normalize};

/// Neighbours looked for per photo: more than any burst is long.
const NEIGHBOURS: usize = 32;

/// A photo in a group of similar ones.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SimilarFile {
    pub path: String,
    /// Last write time, in milliseconds since the Unix epoch.
    pub modified: i64,
    pub size: u64,
    /// Bytes on disk.
    pub bytes: u64,
    /// What the file was when it was compared, to check before acting on it.
    #[serde(skip)]
    pub file_id: u64,
    #[serde(skip)]
    pub mtime: i64,
}

/// Photos that look alike.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SimilarGroup {
    /// `similar-` and the first photo's file id, in hex.
    pub id: String,
    /// How alike the least alike pair linked in the group is (cosine).
    pub score: f32,
    pub files: Vec<SimilarFile>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SimilarReport {
    pub groups: Vec<SimilarGroup>,
    /// The model the photos were compared with; `None` when no model is in use (there is then
    /// nothing to compare).
    pub model: Option<String>,
    /// Photos described with it, among those looked at.
    pub compared: usize,
    pub threshold: f32,
}

/// Where to look, as the Duplicates tab asks.
pub struct Within<'a> {
    pub roots: &'a [String],
    pub exclude: &'a [String],
    pub min_size: u64,
}

/// A described photo under the roots: its row in the store, and the file.
struct Candidate {
    row: u32,
    file: SimilarFile,
}

fn ms_of(filetime: i64) -> i64 {
    (filetime - 116_444_736_000_000_000) / 10_000
}

/// The described photos under the roots, from the index: each file once (its first name).
fn candidates(index: &Index, rows: &HashMap<(u32, u64), u32>, within: &Within) -> Vec<Candidate> {
    let dirs = |paths: &[String]| -> Vec<EntryId> { paths.iter().filter_map(|path| index.lookup(&normalize(path))).collect() };
    let (roots, excluded) = (dirs(within.roots), dirs(within.exclude));
    let mut found = Vec::new();
    for (&(volume, file_id), &row) in rows {
        let entry = index.entries_with_file_id(file_id).into_iter().find(|&id| {
            !index.is_dir(id)
                && index.size(id) >= within.min_size
                && roots.iter().any(|&root| index.is_under(id, root))
                && !excluded.iter().any(|&dir| index.is_under(id, dir))
                && volume_code(&index.path(id)) == Some(volume)
        });
        if let Some(id) = entry {
            let mtime = index.mtime(id);
            found.push(Candidate {
                row,
                file: SimilarFile { path: index.path(id), modified: ms_of(mtime), size: index.size(id), bytes: index.alloc(id), file_id, mtime },
            });
        }
    }
    found.sort_by(|a, b| a.file.path.cmp(&b.file.path));
    found
}

/// Union-find over candidate positions.
fn root_of(parents: &mut [usize], mut at: usize) -> usize {
    while parents[at] != at {
        parents[at] = parents[parents[at]];
        at = parents[at];
    }
    at
}

/// The live rows of photos in `store` (a video's rows are its frames, each with a moment), by
/// file.
fn photo_rows(store: &Store) -> HashMap<(u32, u64), u32> {
    (0..store.len() as u32)
        .filter(|&row| store.is_live(row))
        .filter_map(|row| store.key(row).filter(|key| key.at_ms.is_none()).map(|key| (key.file(), row)))
        .collect()
}

/// The candidates that look alike, at least `threshold` (cosine), grouped, most photos first.
/// `None` when cancelled.
fn group(store: &Store, candidates: &[Candidate], threshold: f32, cancel: &AtomicBool) -> Option<Vec<SimilarGroup>> {
    let position: HashMap<u32, usize> = candidates.iter().enumerate().map(|(at, candidate)| (candidate.row, at)).collect();
    let allowed: HashSet<(u32, u64)> = candidates.iter().filter_map(|c| store.key(c.row).map(|key| key.file())).collect();
    let mut parents: Vec<usize> = (0..candidates.len()).collect();
    let mut links: HashMap<(usize, usize), f32> = HashMap::new();
    for (at, candidate) in candidates.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Some(vector) = store.vector(candidate.row) else { continue };
        let hits = store.search_where(&vector, NEIGHBOURS, threshold, |key| key.at_ms.is_none() && allowed.contains(&key.file()));
        for hit in hits {
            let Some(&other) = position.get(&hit.row) else { continue };
            if other == at {
                continue;
            }
            links.insert((at.min(other), at.max(other)), hit.score);
            let (a, b) = (root_of(&mut parents, at), root_of(&mut parents, other));
            if a != b {
                parents[a.max(b)] = a.min(b);
            }
        }
    }
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for at in 0..candidates.len() {
        let root = root_of(&mut parents, at);
        members.entry(root).or_default().push(at);
    }
    let mut groups: Vec<SimilarGroup> = members
        .into_values()
        .filter(|group| group.len() >= 2)
        .map(|group| {
            let inside: HashSet<usize> = group.iter().copied().collect();
            let score = links
                .iter()
                .filter(|((a, b), _)| inside.contains(a) && inside.contains(b))
                .map(|(_, &score)| score)
                .fold(f32::INFINITY, f32::min);
            let files: Vec<SimilarFile> = group.iter().map(|&at| candidates[at].file.clone()).collect();
            SimilarGroup { id: format!("similar-{:x}", files[0].file_id), score, files }
        })
        .collect();
    groups.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.files[0].path.cmp(&b.files[0].path)));
    Some(groups)
}

/// Groups of similar photos under the roots, most photos first; empty without a model in use.
/// `None` when cancelled.
pub fn find(semantic: &Semantic, engine: &Engine, within: &Within, cancel: &AtomicBool) -> Option<SimilarReport> {
    let Some(loaded) = semantic.current() else { return Some(SimilarReport::default()) };
    let threshold = loaded.spec.cutoffs.near_duplicate;
    let store = loaded.store.read();
    let rows = photo_rows(&store);
    let candidates = {
        let index = engine.index().read();
        candidates(&index, &rows, within)
    };
    let groups = group(&store, &candidates, threshold, cancel)?;
    Some(SimilarReport { groups, model: Some(loaded.model.clone()), compared: candidates.len(), threshold })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, walk};
    use kestrel_vector::Key;

    #[test]
    fn photos_alike_are_grouped_and_the_rest_left_alone() {
        let root = std::env::temp_dir().join(format!("kestrel-similar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        for name in ["burst 1.jpg", "burst 2.jpg", "burst 3.jpg", "beach.jpg", "small.jpg", r"elsewhere\burst 4.jpg"] {
            std::fs::write(root.join(name), vec![1u8; if name == "small.jpg" { 10 } else { 2000 }]).unwrap();
        }
        let path = |name: &str| root.join(name).to_str().unwrap().to_owned();
        let volume = volume_code(&path("beach.jpg")).unwrap();
        let id = |name: &str| walk::file_id_of(&path(name)).unwrap();
        let dir = std::env::temp_dir().join(format!("kestrel-similar-vectors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir, 3, "t").unwrap();
        let row = |name: &str, at_ms: Option<u32>| Key { volume, file_id: id(name), size: 1, mtime: 1, at_ms };
        store
            .append(&[
                (row("burst 1.jpg", None), vec![1.0, 0.0, 0.0]),
                (row("burst 2.jpg", None), vec![0.99, 0.08, 0.0]),
                // Alike to the second, a little less to the first: still the same group.
                (row("burst 3.jpg", None), vec![0.97, 0.2, 0.0]),
                (row("beach.jpg", None), vec![0.0, 0.0, 1.0]),
                (row("small.jpg", None), vec![1.0, 0.0, 0.0]),
                (row(r"elsewhere\burst 4.jpg", None), vec![1.0, 0.0, 0.0]),
            ])
            .unwrap();
        let rows: HashMap<(u32, u64), u32> = (0..store.len() as u32).map(|row| (store.key(row).unwrap().file(), row)).collect();
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let index = builder.finish();
        let within = Within { roots: &[root.to_str().unwrap().to_owned()], exclude: &[path("elsewhere")], min_size: 100 };
        let found = candidates(&index, &rows, &within);
        let names: Vec<String> = found.iter().map(|c| c.file.path.rsplit('\\').next().unwrap().to_owned()).collect();
        assert_eq!(names, ["beach.jpg", "burst 1.jpg", "burst 2.jpg", "burst 3.jpg"], "too small and left out are not looked at");
        assert!(found.iter().all(|c| c.file.size == 2000 && c.file.modified > 1_600_000_000_000));
        assert_eq!(photo_rows(&store).len(), 6);

        // At 0.95: the three bursts, linked through the second; the beach alone isn't a group.
        let groups = group(&store, &found, 0.95, &AtomicBool::new(false)).unwrap();
        assert_eq!(groups.len(), 1);
        let grouped: Vec<&str> = groups[0].files.iter().map(|f| f.path.rsplit('\\').next().unwrap()).collect();
        assert_eq!(grouped, ["burst 1.jpg", "burst 2.jpg", "burst 3.jpg"]);
        assert!(groups[0].score >= 0.95 && groups[0].score < 0.99, "{}", groups[0].score);
        assert!(groups[0].id.starts_with("similar-"));
        // Stricter: only the closest pair.
        let strict = group(&store, &found, 0.995, &AtomicBool::new(false)).unwrap();
        assert_eq!(strict.iter().map(|g| g.files.len()).collect::<Vec<_>>(), [2]);
        // Cancelled: nothing.
        assert!(group(&store, &found, 0.95, &AtomicBool::new(true)).is_none());
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn union_find_joins_through_neighbours() {
        let mut parents: Vec<usize> = (0..4).collect();
        parents[2] = 1;
        parents[1] = 0;
        assert_eq!(root_of(&mut parents, 2), 0);
        assert_eq!(root_of(&mut parents, 3), 3);
        assert_eq!(ms_of(116_444_736_000_000_000 + 10_000), 1);
    }
}
