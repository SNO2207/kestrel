use super::*;

/// A folder of its own under the temp folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kestrel-vector-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// xorshift64*: repeatable vectors without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (bits >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    fn vector(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| self.next()).collect()
    }
}

fn key(file_id: u64) -> Key {
    Key { volume: b'C' as u32, file_id, size: 1000 + file_id, mtime: 5_000 + file_id as i64, at_ms: None }
}

/// The plain computation a search must agree with: every live row's stored (f16) vector,
/// widened, dotted with the normalised query, sorted.
fn naive(store: &Store, query: &[f32], limit: usize, min_score: f32) -> Vec<Hit> {
    let mut unit = query.to_vec();
    normalize(&mut unit);
    let mut all: Vec<Hit> = (0..store.len() as u32)
        .filter(|&row| store.is_live(row))
        .map(|row| {
            let v = store.vector(row).unwrap();
            Hit { row, score: v.iter().zip(&unit).map(|(a, b)| a * b).sum() }
        })
        .filter(|hit| hit.score >= min_score)
        .collect();
    all.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.row.cmp(&b.row)));
    all.truncate(limit);
    all
}

fn assert_same(found: &[Hit], expected: &[Hit]) {
    assert_eq!(found.len(), expected.len(), "{found:?}\nvs\n{expected:?}");
    for (a, b) in found.iter().zip(expected) {
        assert!((a.score - b.score).abs() < 1e-4, "{a:?} vs {b:?}");
        // Rows may only swap where two scores are equal to within rounding.
        if a.row != b.row {
            assert!((a.score - b.score).abs() < 1e-5, "{a:?} vs {b:?}");
        }
    }
}

#[test]
fn search_agrees_with_a_naive_scan() {
    let scratch = Scratch::new("naive");
    let dim = 72;
    let mut store = Store::open(&scratch.0, dim, "test").unwrap();
    let mut rng = Rng(42);
    // More rows than one chunk, so several threads take part and their tops are merged.
    let rows: Vec<(Key, Vec<f32>)> = (0..5000).map(|i| (key(i), rng.vector(dim))).collect();
    store.append(&rows[..3000]).unwrap();
    store.append(&rows[3000..]).unwrap();
    assert_eq!((store.len(), store.live()), (5000, 5000));
    for _ in 0..5 {
        let query = rng.vector(dim);
        for (limit, min) in [(1, -1.0), (10, -1.0), (500, -1.0), (10_000, 0.2)] {
            assert_same(&store.search(&query, limit, min), &naive(&store, &query, limit, min));
        }
    }
    // A stored row is its own best match, at a cosine of 1.
    let probe = rows[1234].1.clone();
    let best = store.search(&probe, 1, -1.0)[0];
    assert_eq!(best.row, 1234);
    assert!((best.score - 1.0).abs() < 2e-3, "{best:?}");
    // Rows are stored normalised.
    let norm: f32 = store.vector(7).unwrap().iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-2, "{norm}");
    // A filter narrows what is searched.
    let even = store.search_where(&probe, 50, -1.0, |key| key.file_id % 2 == 0);
    assert!(even.iter().all(|hit| store.key(hit.row).unwrap().file_id.is_multiple_of(2)));
    assert!(!even.iter().any(|hit| hit.row == 1234 + 1));
    // Wrong sizes are refused rather than misread.
    assert!(store.search(&[1.0; 3], 5, -1.0).is_empty());
    assert!(store.append(&[(key(1), vec![0.5; 3])]).is_err());
}

#[test]
fn a_store_persists_and_refuses_another_models_vectors() {
    let scratch = Scratch::new("persist");
    let mut rng = Rng(7);
    let rows: Vec<(Key, Vec<f32>)> = (0..300)
        .map(|i| (Key { at_ms: (i % 3 == 0).then_some(i as u32 * 1000), ..key(i / 3) }, rng.vector(16)))
        .collect();
    let query = rng.vector(16);
    let before = {
        let mut store = Store::open(&scratch.0, 16, "model-a").unwrap();
        store.append(&rows).unwrap();
        store.tombstone(5).unwrap();
        store.flush().unwrap();
        store.search(&query, 20, -1.0)
    };
    let store = Store::open(&scratch.0, 16, "model-a").unwrap();
    assert_eq!((store.len(), store.live()), (300, 299));
    assert!(!store.is_live(5));
    assert_eq!(store.key(9).unwrap(), rows[9].0, "keys come back, a frame's moment included");
    assert_eq!(store.key(10).unwrap().at_ms, None);
    assert_same(&store.search(&query, 20, -1.0), &before);
    drop(store);

    assert!(matches!(Store::open(&scratch.0, 16, "model-b"), Err(OpenError::Mismatch(_))));
    assert!(matches!(Store::open(&scratch.0, 32, "model-a"), Err(OpenError::Mismatch(_))));
    let fresh = Store::open_or_reset(&scratch.0, 32, "model-b").unwrap();
    assert_eq!(fresh.len(), 0, "another model's store starts again empty");
}

#[test]
fn a_row_half_written_by_a_crash_is_cut_off() {
    let scratch = Scratch::new("torn");
    let mut rng = Rng(9);
    {
        let mut store = Store::open(&scratch.0, 8, "t").unwrap();
        store.append(&(0..10).map(|i| (key(i), rng.vector(8))).collect::<Vec<_>>()).unwrap();
    }
    // A vector written without its key, and half a key.
    let mut vectors = OpenOptions::new().append(true).open(vectors_path(&scratch.0)).unwrap();
    vectors.write_all(&[0x3c; 16]).unwrap();
    let mut keys = OpenOptions::new().append(true).open(keys_path(&scratch.0)).unwrap();
    keys.write_all(&[1; 13]).unwrap();
    drop((vectors, keys));
    let mut store = Store::open(&scratch.0, 8, "t").unwrap();
    assert_eq!(store.len(), 10);
    let row = store.append(&[(key(99), rng.vector(8))]).unwrap();
    assert_eq!(row, 10);
    drop(store);
    assert_eq!(Store::open(&scratch.0, 8, "t").unwrap().key(10).unwrap().file_id, 99);
}

#[test]
fn tombstones_hide_rows_and_compaction_removes_them() {
    let scratch = Scratch::new("compact");
    let mut rng = Rng(3);
    let dim = 24;
    let mut store = Store::open(&scratch.0, dim, "t").unwrap();
    // File 0 to 99, a version each, then a second version of files 0 to 9 (edited since).
    let mut rows: Vec<(Key, Vec<f32>)> = (0..100).map(|i| (key(i), rng.vector(dim))).collect();
    store.append(&rows).unwrap();
    for i in 0..10 {
        assert_eq!(store.tombstone_file(b'C' as u32, i).unwrap(), 1);
        let edited = Key { mtime: 9_999, ..key(i) };
        rows.push((edited, rng.vector(dim)));
        store.append(&rows[rows.len() - 1..]).unwrap();
    }
    assert!(!store.has_current(b'C' as u32, 3, key(3).size, key(3).mtime), "the old version is gone");
    assert!(store.has_current(b'C' as u32, 3, key(3).size, 9_999));
    assert_eq!((store.len(), store.live()), (110, 100));
    assert!(!store.tombstone(0).unwrap(), "a row dies once");
    let query = rows[2].1.clone();
    let hits = store.search(&query, 200, -1.0);
    assert!(hits.iter().all(|hit| store.is_live(hit.row)));
    assert!(!hits.iter().any(|hit| hit.row == 2), "a tombstoned row is never found");
    assert_same(&hits, &naive(&store, &query, 200, -1.0));
    let before: Vec<(Key, u32)> = hits.iter().map(|hit| (store.key(hit.row).unwrap(), (hit.score * 1e4) as u32)).collect();

    assert!((store.dead_share() - 10.0 / 110.0).abs() < 1e-6);
    assert_eq!(store.compact().unwrap(), 10);
    assert_eq!((store.len(), store.live(), store.dead_share()), (100, 100, 0.0));
    let after: Vec<(Key, u32)> =
        store.search(&query, 200, -1.0).iter().map(|hit| (store.key(hit.row).unwrap(), (hit.score * 1e4) as u32)).collect();
    assert_eq!(after, before, "the same files at the same scores, under new row numbers");
    assert_eq!(store.compact().unwrap(), 0);
    drop(store);
    let store = Store::open(&scratch.0, dim, "t").unwrap();
    assert_eq!((store.len(), store.live()), (100, 100));
    assert!(!scratch.0.join("vectors.kvec.new").exists());
}

#[test]
fn a_video_is_one_result_at_its_best_moment() {
    let scratch = Scratch::new("frames");
    let mut store = Store::open(&scratch.0, 3, "t").unwrap();
    let frame = |id: u64, at: u32, v: [f32; 3]| (Key { at_ms: Some(at), ..key(id) }, v.to_vec());
    store
        .append(&[
            frame(1, 0, [1.0, 0.0, 0.0]),
            frame(1, 2000, [0.0, 1.0, 0.0]),
            frame(1, 4000, [0.6, 0.8, 0.0]),
            (key(2), vec![0.0, 0.9, 0.1]),
            frame(3, 500, [0.0, 0.0, 1.0]),
        ])
        .unwrap();
    let hits = store.search(&[0.0, 1.0, 0.0], 10, -1.0);
    let files = store.best_per_file(&hits);
    let summary: Vec<(u64, Option<u32>)> = files.iter().map(|hit| (hit.key.file_id, hit.key.at_ms)).collect();
    assert_eq!(summary, [(1, Some(2000)), (2, None), (3, Some(500))]);
    assert!(files[0].score > files[1].score && files[1].score > files[2].score);
}

/// The target is under 100 ms for 200,000 vectors; this measures the Fast model's size (768
/// dimensions) in a test build, which is optimised less than a release build, while other
/// work may share the machine. `kestrel-cli vector-bench` measures a release build at both
/// models' sizes.
#[test]
fn searching_200k_vectors_is_quick() {
    let scratch = Scratch::new("bench");
    let (dim, count) = (768, 200_000);
    let mut store = Store::open(&scratch.0, dim, "bench").unwrap();
    let mut rng = Rng(11);
    for batch in 0..count / 10_000 {
        let rows: Vec<(Key, Vec<f32>)> = (0..10_000).map(|i| (key((batch * 10_000 + i) as u64), rng.vector(dim))).collect();
        store.append(&rows).unwrap();
    }
    let query = rng.vector(dim);
    let mut times = Vec::new();
    let mut hits = Vec::new();
    for _ in 0..5 {
        let started = std::time::Instant::now();
        hits = store.search(&query, 1000, -1.0);
        times.push(started.elapsed());
    }
    times.sort();
    println!("search of {count} x {dim} with the {} kernel: median {:?}, best {:?}", kernel_name(), times[2], times[0]);
    assert_eq!(hits.len(), 1000);
    assert!(times[2] < std::time::Duration::from_millis(1000), "median {:?}", times[2]);
}
