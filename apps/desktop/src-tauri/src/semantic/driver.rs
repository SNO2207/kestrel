//! Stands in for the media pipeline until the embedding stage joins it: a background thread that
//! finds the photos and videos in the scopes, from the index, and hands them to the stage.
//!
//! It does what the pipeline is to do for every stage: runs at background priority (CPU, disk
//! and memory) on a few threads of its own; pauses when the owner says so, and stops a pass at
//! once when the model, the scopes or the pause change; never opens a cloud-only placeholder
//! (left out here by the index's attributes, and checked again on disk by the stage); and works
//! incrementally by file key (volume, file id, size, time), so a renamed or moved photo is never
//! read again while an edited one is. Files that went, or changed, lose their vectors first.
//!
//! A pass runs when something it depends on changed: at once for the model, scopes or pause; at
//! most every ten seconds for the index, which changes all the time. Photos go first, a batch
//! at a time, then videos one by one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kestrel_db::semantic as data;
use kestrel_embed::ocr::Ocr;
use kestrel_embed::video::Plan;
use kestrel_index::{EntryId, attr};
use kestrel_vector::Store;
use parking_lot::RwLock;

use super::stage::{EmbedStage, FileKey, IMAGE_EXTS, MediaItem, MediaKind, Target, VIDEO_EXTS, is_placeholder, volume_code};
use super::{Loaded, Semantic};
use crate::engine::{Engine, normalize};

/// Photos described at a time.
const BATCH: usize = 16;
/// Smaller pictures are icons and the like, not photos. (A short video can be smaller.)
const MIN_BYTES: u64 = 4 * 1024;
/// When only the index changed, a pass at most this often.
const PASS_GAP: Duration = Duration::from_secs(10);
/// Entries looked at under one hold of the index.
const STEP: u32 = 200_000;
/// Compact the store when this share of its rows are tombstones.
const COMPACT_SHARE: f32 = 0.25;
/// Threads that decode and read text.
const THREADS: usize = 4;

/// Puts the calling thread in Windows' background mode (low CPU, disk and memory priority), or
/// takes it out.
fn background(on: bool) {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END};
    // SAFETY: changes only the calling thread's own priority.
    unsafe { SetThreadPriority(GetCurrentThread(), if on { THREAD_MODE_BACKGROUND_BEGIN } else { THREAD_MODE_BACKGROUND_END }) };
}

enum Pass {
    Finished,
    Interrupted,
}

pub fn run(semantic: Arc<Semantic>) {
    let Ok(pool) = rayon::ThreadPoolBuilder::new()
        .num_threads(THREADS)
        .thread_name(|i| format!("kestrel-semantic-{i}"))
        .start_handler(|_| background(true))
        .build()
    else {
        return;
    };
    let ocr = Ocr::new().ok();
    // Give the window a moment to open before the model loads.
    semantic.wait(Duration::from_secs(3));
    let mut last: Option<(String, u64, u64)> = None;
    let mut started: Option<Instant> = None;
    loop {
        if semantic.active().is_none() {
            semantic.set_indexing(|s| *s = super::Indexing { state: "off", ..Default::default() });
            last = None;
            semantic.wait(Duration::from_secs(5));
            continue;
        }
        if semantic.paused() {
            semantic.set_indexing(|s| {
                s.state = "paused";
                s.current = None;
            });
            semantic.wait(Duration::from_secs(5));
            continue;
        }
        // Loading reads a gigabyte or two: not in background mode, a search may be waiting.
        if semantic.current().is_none() {
            semantic.set_indexing(|s| s.state = "loading");
        }
        let loaded = match semantic.ready() {
            Ok(loaded) => loaded,
            // Deleted or switched while it loaded: look again at once.
            Err(_) if semantic.current().is_none() && semantic.runtime_is_off() => continue,
            Err(error) => {
                semantic.set_indexing(|s| {
                    s.state = "failed";
                    s.error = Some(error);
                });
                semantic.wait(Duration::from_secs(10));
                continue;
            }
        };
        let (epoch, revision) = (semantic.epoch(), semantic.engine.revision());
        let changed = last.as_ref().is_none_or(|(model, _, seen)| *model != loaded.model || *seen != epoch);
        let index_moved = last.as_ref().is_some_and(|(_, seen, _)| *seen != revision);
        let due = changed || (index_moved && started.is_none_or(|at| at.elapsed() >= PASS_GAP));
        if !due {
            semantic.set_indexing(|s| {
                if s.state != "done" {
                    s.state = "done";
                    s.current = None;
                }
            });
            drop(loaded);
            semantic.wait(Duration::from_secs(3));
            continue;
        }
        started = Some(Instant::now());
        background(true);
        let outcome = pass(&semantic, &loaded, &pool, ocr.as_ref(), epoch);
        background(false);
        if let Pass::Finished = outcome {
            last = Some((loaded.model.clone(), revision, epoch));
        }
    }
}

/// The photos and videos in `scopes`, from the index: files of a media type, not hidden or
/// system, not online-only, not tiny.
pub fn collect(engine: &Engine, scopes: &[String]) -> Vec<MediaItem> {
    struct Walk {
        next: EntryId,
        scopes: Vec<EntryId>,
        kinds: HashMap<u16, MediaKind>,
        found: Vec<MediaItem>,
    }
    engine.in_steps(
        |index| {
            let kinds = IMAGE_EXTS
                .iter()
                .map(|ext| (ext, MediaKind::Image))
                .chain(VIDEO_EXTS.iter().map(|ext| (ext, MediaKind::Video)))
                .filter_map(|(ext, kind)| Some((index.ext_id(ext)?, kind)))
                .collect();
            let scopes = scopes.iter().filter_map(|scope| index.lookup(&normalize(scope))).filter(|&dir| index.is_dir(dir)).collect();
            Walk { next: 0, scopes, kinds, found: Vec::new() }
        },
        |walk, index| {
            let end = walk.next.saturating_add(STEP).min(index.len());
            if !walk.scopes.is_empty() {
                for id in walk.next..end {
                    if index.is_deleted(id) || index.is_dir(id) {
                        continue;
                    }
                    let Some(&kind) = walk.kinds.get(&index.ext(id)) else { continue };
                    let attrs = index.attrs(id);
                    if attrs & (attr::HIDDEN | attr::SYSTEM) != 0 || is_placeholder(attrs) || index.file_id(id) == 0 {
                        continue;
                    }
                    if kind == MediaKind::Image && index.size(id) < MIN_BYTES {
                        continue;
                    }
                    if !walk.scopes.iter().any(|&dir| index.is_under(id, dir)) {
                        continue;
                    }
                    let path = index.path(id);
                    let Some(volume) = volume_code(&path) else { continue };
                    let key = FileKey { volume, file_id: index.file_id(id), size: index.size(id), mtime: index.mtime(id) };
                    walk.found.push(MediaItem { path, key, kind });
                }
            }
            walk.next = end;
            end >= index.len()
        },
        |walk, _| walk.found,
    )
}

/// Tombstones the vectors of files that are no longer among `media` or have changed since.
pub fn sweep(store: &RwLock<Store>, media: &[MediaItem]) -> usize {
    let current: HashMap<(u32, u64), (u64, i64)> =
        media.iter().map(|item| ((item.key.volume, item.key.file_id), (item.key.size, item.key.mtime))).collect();
    let stale: Vec<u32> = {
        let store = store.read();
        (0..store.len() as u32)
            .filter(|&row| store.is_live(row))
            .filter(|&row| {
                let Some(key) = store.key(row) else { return false };
                current.get(&key.file()) != Some(&(key.size, key.mtime))
            })
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

fn pass(semantic: &Semantic, loaded: &Loaded, pool: &rayon::ThreadPool, ocr: Option<&Ocr>, epoch: u64) -> Pass {
    let stopped = || semantic.epoch() != epoch || semantic.paused();
    let media = collect(&semantic.engine, &semantic.scopes());
    sweep(&loaded.store, &media);
    // Text read from files that are no longer described goes too.
    if let Ok(texts) = data::text_files(&semantic.db) {
        let keep: std::collections::HashSet<(u32, u64)> = media.iter().map(|item| (item.key.volume, item.key.file_id)).collect();
        for (volume, file_id) in texts.keys().filter(|file| !keep.contains(file)) {
            let _ = data::forget_text(&semantic.db, *volume, *file_id);
        }
    }
    let skips = data::skips(&semantic.db, &loaded.model).unwrap_or_default();
    let text_files = data::text_files(&semantic.db).unwrap_or_default();
    let stage = EmbedStage {
        target: Target { model: &loaded.model, embedder: &loaded.embedder, side: loaded.side, store: &loaded.store },
        db: &semantic.db,
        ocr,
        plan: Plan::default(),
        pool,
    };
    let (images, videos): (Vec<MediaItem>, Vec<MediaItem>) =
        media.iter().filter(|item| stage.wants(item, &skips)).cloned().partition(|item| item.kind == MediaKind::Image);
    let queued = images.len() + videos.len();
    semantic.set_indexing(|s| {
        s.state = if queued == 0 { "done" } else { "working" };
        s.media = media.len();
        s.queued = queued;
        s.done = 0;
        s.current = None;
        s.error = None;
    });
    for batch in images.chunks(BATCH) {
        if stopped() {
            return Pass::Interrupted;
        }
        semantic.set_indexing(|s| s.current = batch.first().map(|item| item.path.clone()));
        stage.run_images(batch, &text_files);
        semantic.set_indexing(|s| {
            s.done += batch.len();
            s.queued = s.queued.saturating_sub(batch.len());
        });
    }
    for item in &videos {
        if stopped() {
            return Pass::Interrupted;
        }
        semantic.set_indexing(|s| s.current = Some(item.path.clone()));
        stage.run_video(item);
        semantic.set_indexing(|s| {
            s.done += 1;
            s.queued = s.queued.saturating_sub(1);
        });
    }
    {
        let mut store = loaded.store.write();
        if store.dead_share() > COMPACT_SHARE && store.len() > 64 {
            let _ = store.compact();
        }
        let _ = store.flush();
    }
    semantic.set_indexing(|s| {
        s.state = "done";
        s.current = None;
        s.queued = 0;
    });
    semantic.status_changed(true);
    Pass::Finished
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, walk};

    #[test]
    fn media_in_scope_are_found_and_the_rest_left_out() {
        let root = std::env::temp_dir().join(format!("kestrel-semantic-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("photos/2024")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        let big = vec![7u8; 8 * 1024];
        for name in ["photos/2024/beach.JPG", "photos/clip.mp4", "photos/notes.txt", "elsewhere/other.png"] {
            std::fs::write(root.join(name), &big).unwrap();
        }
        std::fs::write(root.join("photos/icon.png"), b"tiny").unwrap();
        // A short video of plain colours can be smaller than an icon; it still counts.
        std::fs::write(root.join("photos/short.mov"), b"small").unwrap();
        let root_str = root.to_str().unwrap().to_owned();
        let mut builder = IndexBuilder::new();
        walk::walk(&root_str, &mut builder).unwrap();
        let engine = Engine::default();
        engine.install(builder.finish());
        let photos = format!(r"{root_str}\photos");
        let mut found: Vec<(String, MediaKind)> = collect(&engine, std::slice::from_ref(&photos))
            .into_iter()
            .map(|item| {
                assert_ne!(item.key.file_id, 0);
                (item.path.rsplit('\\').next().unwrap().to_owned(), item.kind)
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found, [
            ("beach.JPG".to_owned(), MediaKind::Image),
            ("clip.mp4".to_owned(), MediaKind::Video),
            ("short.mov".to_owned(), MediaKind::Video)
        ]);
        assert!(collect(&engine, &[]).is_empty());
        assert!(collect(&engine, &[format!(r"{root_str}\missing")]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn vectors_of_files_that_went_or_changed_are_tombstoned() {
        let dir = std::env::temp_dir().join(format!("kestrel-semantic-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = RwLock::new(Store::open(&dir, 2, "t").unwrap());
        let key = |id: u64, mtime: i64| kestrel_vector::Key { volume: 67, file_id: id, size: 10, mtime, at_ms: None };
        store.write().append(&[(key(1, 1), vec![1.0, 0.0]), (key(2, 1), vec![0.0, 1.0]), (key(3, 1), vec![1.0, 1.0])]).unwrap();
        let item = |id: u64, mtime: i64| MediaItem {
            path: format!(r"C:\x\{id}.jpg"),
            key: FileKey { volume: 67, file_id: id, size: 10, mtime },
            kind: MediaKind::Image,
        };
        // File 1 is unchanged, file 2 was edited, file 3 went.
        assert_eq!(sweep(&store, &[item(1, 1), item(2, 9)]), 2);
        let store = store.into_inner();
        assert_eq!(store.live(), 1);
        assert!(store.has_current(67, 1, 10, 1));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
