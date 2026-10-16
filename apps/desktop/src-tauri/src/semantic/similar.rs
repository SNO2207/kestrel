//! Similar photos: pictures that look nearly the same without being the same file (a burst, the
//! same shot again, an edit, a resized copy), found by comparing their stored vectors. The disk
//! usage hub's Duplicates tab lists them below the exact duplicates, as groups of their own that
//! are never chosen for the owner (`cleanup/`).
//!
//! Only photos already described are compared (the media library's pass describes them in the
//! background), and only the model in use's vectors: nothing is read from disk here. Two photos
//! are alike when their vectors' cosine reaches the model's `near_duplicate` cutoff; groups are
//! the photos linked that way, directly or through each other.
//!
//! Every photo is compared with every other, which for a large library takes a while, so the
//! shared state is held only in steps: the index a few thousand photos at a time
//! (`Engine::in_steps`), and the vector store for one pass over its rows that compares a batch
//! of photos at once (`Store::search_many`). The store's lock is fair, so a describing worker
//! waiting to write would otherwise hold up every search behind a scan of minutes. Photos are
//! known by their file, not their row, as a compaction between steps renumbers the rows.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use kestrel_index::{EntryId, Index};
use kestrel_vector::Store;
use parking_lot::RwLock;
use serde::Serialize;

use super::Semantic;
use super::stage::volume_code;
use crate::engine::{Engine, normalize};

/// Neighbours looked for per photo: more than any burst is long.
const NEIGHBOURS: usize = 32;
/// Photos compared per hold of the store: one pass over its rows for all of them, a few tens
/// of milliseconds over 200,000 vectors.
const BATCH: usize = 8;
/// Described photos looked up in the index per hold of it.
const LOOKUP_STEP: usize = 4096;

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

/// A described photo under the roots: its file in the store, and the file.
struct Candidate {
    key: (u32, u64),
    file: SimilarFile,
}

fn ms_of(filetime: i64) -> i64 {
    (filetime - 116_444_736_000_000_000) / 10_000
}

/// The roots and the folders left out, as entries of this index.
struct Places {
    roots: Vec<EntryId>,
    excluded: Vec<EntryId>,
}

impl Places {
    fn of(index: &Index, within: &Within) -> Places {
        let dirs = |paths: &[String]| -> Vec<EntryId> { paths.iter().filter_map(|path| index.lookup(&normalize(path))).collect() };
        Places { roots: dirs(within.roots), excluded: dirs(within.exclude) }
    }

    /// The described photo `key` under the roots, by its first name there.
    fn look_up(&self, index: &Index, key: (u32, u64), min_size: u64) -> Option<Candidate> {
        let (volume, file_id) = key;
        let id = index.entries_with_file_id(file_id).into_iter().find(|&id| {
            !index.is_dir(id)
                && index.size(id) >= min_size
                // As the duplicate finder walks: a folder left out is skipped only beneath a root,
                // so a root inside one (a folder under AppData chosen on purpose) is still looked in.
                && self.roots.iter().any(|&root| {
                    index.is_under(id, root) && !self.excluded.iter().any(|&dir| dir != root && index.is_under(dir, root) && index.is_under(id, dir))
                })
                && volume_code(&index.path(id)) == Some(volume)
        })?;
        let mtime = index.mtime(id);
        Some(Candidate {
            key,
            file: SimilarFile { path: index.path(id), modified: ms_of(mtime), size: index.size(id), bytes: index.alloc(id), file_id, mtime },
        })
    }
}

/// The described photos under the roots, from the index, a step at a time: each file once (its
/// first name), by path.
fn candidates(engine: &Engine, files: &[(u32, u64)], within: &Within) -> Vec<Candidate> {
    let mut found = engine.in_steps(
        |index| (Places::of(index, within), 0usize, Vec::new()),
        |(places, at, found), index| {
            let end = (*at + LOOKUP_STEP).min(files.len());
            found.extend(files[*at..end].iter().filter_map(|&key| places.look_up(index, key, within.min_size)));
            *at = end;
            end == files.len()
        },
        |(_, _, found), _| found,
    );
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

/// The files of the live photo rows in `store` (a video's rows are its frames, each with a
/// moment).
fn photo_files(store: &Store) -> Vec<(u32, u64)> {
    let files: HashSet<(u32, u64)> = (0..store.len() as u32)
        .filter(|&row| store.is_live(row))
        .filter_map(|row| store.key(row).filter(|key| key.at_ms.is_none()).map(|key| key.file()))
        .collect();
    files.into_iter().collect()
}

/// A photo's vector: its live row's.
fn vector_of(store: &Store, (volume, file_id): (u32, u64)) -> Option<Vec<f32>> {
    let row = store.rows_of(volume, file_id).into_iter().find(|&row| store.key(row).is_some_and(|key| key.at_ms.is_none()))?;
    store.vector(row)
}

/// The candidates that look alike, at least `threshold` (cosine), grouped, most photos first.
/// The store is held for one batch at a time; `go_on` is asked between batches, with nothing
/// held, and `None` comes back once it says no (cancelled).
fn group(store: &RwLock<Store>, candidates: &[Candidate], threshold: f32, mut go_on: impl FnMut() -> bool) -> Option<Vec<SimilarGroup>> {
    let position: HashMap<(u32, u64), usize> = candidates.iter().enumerate().map(|(at, candidate)| (candidate.key, at)).collect();
    let mut parents: Vec<usize> = (0..candidates.len()).collect();
    let mut links: HashMap<(usize, usize), f32> = HashMap::new();
    let all: Vec<usize> = (0..candidates.len()).collect();
    for batch in all.chunks(BATCH) {
        if !go_on() {
            return None;
        }
        let found: Vec<(usize, usize, f32)> = {
            let store = store.read();
            let (asked, vectors): (Vec<usize>, Vec<Vec<f32>>) =
                batch.iter().filter_map(|&at| vector_of(&store, candidates[at].key).map(|vector| (at, vector))).unzip();
            let hits = store.search_many(&vectors, NEIGHBOURS, threshold, |key| key.at_ms.is_none() && position.contains_key(&key.file()));
            asked
                .iter()
                .zip(hits)
                .flat_map(|(&at, hits)| {
                    let store = &store;
                    let position = &position;
                    hits.into_iter().filter_map(move |hit| Some((at, *position.get(&store.key(hit.row)?.file())?, hit.score)))
                })
                .collect()
        };
        for (at, other, score) in found {
            if other == at {
                continue;
            }
            links.insert((at.min(other), at.max(other)), score);
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
    let files = photo_files(&loaded.store.read());
    let candidates = candidates(engine, &files, within);
    let groups = group(&loaded.store, &candidates, threshold, || !cancel.load(Ordering::Relaxed))?;
    Some(SimilarReport { groups, model: Some(loaded.model.clone()), compared: candidates.len(), threshold })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, walk};
    use kestrel_vector::Key;

    struct Fixture {
        root: std::path::PathBuf,
        dir: std::path::PathBuf,
        engine: Engine,
        store: RwLock<Store>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Fixture {
        fn path(&self, name: &str) -> String {
            self.root.join(name).to_str().unwrap().to_owned()
        }

        fn key(&self, name: &str, at_ms: Option<u32>) -> Key {
            let path = self.path(name);
            Key { volume: volume_code(&path).unwrap(), file_id: walk::file_id_of(&path).unwrap(), size: 1, mtime: 1, at_ms }
        }

        fn names(found: &[Candidate]) -> Vec<String> {
            found.iter().map(|c| c.file.path.rsplit('\\').next().unwrap().to_owned()).collect()
        }
    }

    /// Photos in a scratch folder, indexed, each with a vector.
    fn fixture(name: &str, photos: &[(&str, u64, [f32; 3])]) -> Fixture {
        let root = std::env::temp_dir().join(format!("kestrel-similar-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        for (photo, size, _) in photos {
            std::fs::write(root.join(photo), vec![1u8; *size as usize]).unwrap();
        }
        let dir = std::env::temp_dir().join(format!("kestrel-similar-vectors-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let engine = Engine::default();
        engine.install(builder.finish());
        let fixture = Fixture { root, dir: dir.clone(), engine, store: RwLock::new(Store::open(&dir, 3, "t").unwrap()) };
        let rows: Vec<(Key, Vec<f32>)> = photos.iter().map(|(photo, _, vector)| (fixture.key(photo, None), vector.to_vec())).collect();
        fixture.store.write().append(&rows).unwrap();
        fixture
    }

    #[test]
    fn photos_alike_are_grouped_and_the_rest_left_alone() {
        let f = fixture("groups", &[
            ("burst 1.jpg", 2000, [1.0, 0.0, 0.0]),
            ("burst 2.jpg", 2000, [0.99, 0.08, 0.0]),
            // Alike to the second, a little less to the first: still the same group.
            ("burst 3.jpg", 2000, [0.97, 0.2, 0.0]),
            ("beach.jpg", 2000, [0.0, 0.0, 1.0]),
            ("small.jpg", 10, [1.0, 0.0, 0.0]),
            (r"elsewhere\burst 4.jpg", 2000, [1.0, 0.0, 0.0]),
        ]);
        // A video's frames are no photos.
        f.store.write().append(&[(f.key("beach.jpg", Some(500)), vec![1.0, 0.0, 0.0])]).unwrap();
        let files = photo_files(&f.store.read());
        assert_eq!(files.len(), 6);
        let roots = [f.root.to_str().unwrap().to_owned()];
        let within = Within { roots: &roots, exclude: &[f.path("elsewhere")], min_size: 100 };
        let found = candidates(&f.engine, &files, &within);
        assert_eq!(Fixture::names(&found), ["beach.jpg", "burst 1.jpg", "burst 2.jpg", "burst 3.jpg"], "too small and left out are not looked at");
        assert!(found.iter().all(|c| c.file.size == 2000 && c.file.modified > 1_600_000_000_000));
        // A folder chosen inside one left out (a scratch folder under AppData) is looked in.
        let inside_roots = [f.path("elsewhere")];
        let inside = Within { roots: &inside_roots, exclude: &[roots[0].clone(), f.path("elsewhere")], min_size: 100 };
        assert_eq!(Fixture::names(&candidates(&f.engine, &files, &inside)), ["burst 4.jpg"]);

        // At 0.95: the three bursts, linked through the second; the beach alone isn't a group.
        let groups = group(&f.store, &found, 0.95, || true).unwrap();
        assert_eq!(groups.len(), 1);
        let grouped: Vec<&str> = groups[0].files.iter().map(|f| f.path.rsplit('\\').next().unwrap()).collect();
        assert_eq!(grouped, ["burst 1.jpg", "burst 2.jpg", "burst 3.jpg"]);
        assert!(groups[0].score >= 0.95 && groups[0].score < 0.99, "{}", groups[0].score);
        assert!(groups[0].id.starts_with("similar-"));
        // Stricter: only the closest pair.
        let strict = group(&f.store, &found, 0.995, || true).unwrap();
        assert_eq!(strict.iter().map(|g| g.files.len()).collect::<Vec<_>>(), [2]);
        // Cancelled: nothing.
        assert!(group(&f.store, &found, 0.95, || false).is_none());
    }

    /// Comparing every photo with every other takes minutes in a large library: the store is
    /// held for one batch at a time, so the describing (which writes to it) and the searches
    /// queued behind it get in between; and a compaction between batches, which renumbers the
    /// rows, changes nothing.
    #[test]
    fn the_store_is_let_go_between_batches() {
        // Pairs around a circle, a third of a radian apart; the two of a pair barely differ.
        let photos: Vec<(String, u64, [f32; 3])> = (0..40)
            .map(|n| {
                let angle = (n / 2) as f32 * 0.3;
                (format!("p{n:02}.jpg"), 500, [angle.cos(), angle.sin(), (n % 2) as f32 * 0.01])
            })
            .collect();
        let named: Vec<(&str, u64, [f32; 3])> = photos.iter().map(|(name, size, v)| (name.as_str(), *size, *v)).collect();
        let f = fixture("steps", &named);
        // A tombstoned row first, so a compaction renumbers every other.
        let gone = f.key("p00.jpg", None);
        f.store.write().append(&[(Key { mtime: 0, ..gone }, vec![0.0, 0.0, 1.0])]).unwrap();
        f.store.write().tombstone(40).unwrap();
        f.store.write().tombstone(0).unwrap();
        f.store.write().append(&[(gone, vec![1.0, 0.0, 0.0])]).unwrap();
        let roots = [f.root.to_str().unwrap().to_owned()];
        let within = Within { roots: &roots, exclude: &[], min_size: 1 };
        let found = candidates(&f.engine, &photo_files(&f.store.read()), &within);
        assert_eq!(found.len(), 40);
        let mut steps = 0;
        let groups = group(&f.store, &found, 0.99, || {
            // Nothing is held here: a writer gets in at once.
            let mut store = f.store.try_write().expect("the store is free between batches");
            if steps == 1 {
                assert!(store.compact().unwrap() > 0);
            }
            steps += 1;
            true
        })
        .unwrap();
        assert_eq!(steps, 40usize.div_ceil(BATCH));
        // Pairs: p00 and p01 alike, p02 and p03, ...
        assert_eq!(groups.len(), 20, "{groups:?}");
        assert!(groups.iter().all(|group| group.files.len() == 2));
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
