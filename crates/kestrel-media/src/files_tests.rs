//! Tests against real files the tests make themselves (photos with EXIF, a video, an image with
//! text), in scratch folders under the temp folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kestrel_db::Db;
use kestrel_index::{IndexBuilder, Meta, walk};
use parking_lot::Mutex;

use crate::fixtures::{self, Exif, quarter_colour, quarters};
use crate::pipeline::{Config, Input, Item, Needs, ONLINE_ONLY, Pipeline, Stage};
use crate::{Apartment, Kind, decode, props};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kestrel-media-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// FILETIME of a UTC date and time.
fn filetime(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
    // Days from 1970-01-01 to the date (Howard Hinnant's algorithm).
    let (y, m) = if month <= 2 { (year - 1, month + 12) } else { (year, month) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (i64::from(m) - 3) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    crate::FILETIME_UNIX_EPOCH + (days * 86_400 + hour * 3600 + minute * 60) * 10_000_000
}

fn camera_exif() -> Exif {
    Exif {
        make: Some("Kestrel".into()),
        model: Some("Test Camera 1".into()),
        taken: Some("2024:07:14 10:30:00".into()),
        orientation: Some(6),
        gps: Some((51.5007, -0.1246)),
    }
}

#[test]
fn photo_facts_come_from_the_property_system() {
    let _com = Apartment::enter();
    let dir = scratch("photo-facts");
    let photo = dir.join("IMG_0001.jpg");
    fixtures::write_jpeg(&photo, &quarters(640, 480), &camera_exif()).unwrap();

    let facts = props::read(&text(&photo), Kind::Photo).unwrap();
    // Stored 640 by 480, turned a quarter: shown 480 by 640.
    assert_eq!((facts.width, facts.height, facts.orientation), (Some(480), Some(640), Some(6)));
    assert_eq!(facts.make.as_deref(), Some("Kestrel"));
    assert_eq!(facts.model.as_deref(), Some("Test Camera 1"));
    // EXIF keeps local time without a zone; Windows reads it as this computer's local time.
    let taken = facts.taken.expect("a date taken");
    let utc = filetime(2024, 7, 14, 10, 30);
    assert!((taken - utc).abs() <= 14 * 3600 * 10_000_000, "{taken} vs {utc}");
    let (latitude, longitude) = (facts.latitude.unwrap(), facts.longitude.unwrap());
    assert!((latitude - 51.5007).abs() < 1e-4 && (longitude + 0.1246).abs() < 1e-4, "{latitude}, {longitude}");
    assert_eq!(facts.duration_ms, None);

    // Decoded turned as shown: blue (bottom left) comes to the top left.
    let shown = decode::image(&text(&photo), 256, 6).unwrap();
    assert_eq!((shown.width, shown.height), (192, 256));
    let corners = [shown.at(40, 40), shown.at(150, 40), shown.at(40, 210), shown.at(150, 210)].map(quarter_colour);
    assert_eq!(corners, ['b', 'r', 'w', 'g']);
    // Dimensions and picture agree; had a handler reported the stored size, they'd be swapped.
    let mut checked = facts.clone();
    assert!(!crate::pipeline::agree(&mut checked, &shown));
    let mut stored = crate::Facts { width: Some(640), height: Some(480), ..facts.clone() };
    assert!(crate::pipeline::agree(&mut stored, &shown));
    assert_eq!((stored.width, stored.height), (Some(480), Some(640)));

    // A plain PNG has dimensions and nothing else.
    let plain = dir.join("plain.png");
    std::fs::write(&plain, quarters(300, 200).to_png().unwrap()).unwrap();
    let facts = props::read(&text(&plain), Kind::Photo).unwrap();
    assert_eq!((facts.width, facts.height, facts.make, facts.latitude), (Some(300), Some(200), None, None));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_large_photo_decodes_small_quickly() {
    let _com = Apartment::enter();
    let dir = scratch("large-photo");
    let photo = dir.join("large.jpg");
    fixtures::write_jpeg(&photo, &quarters(6000, 4000), &Exif::default()).unwrap();
    let started = Instant::now();
    let small = decode::image(&text(&photo), 256, 1).unwrap();
    let took = started.elapsed();
    assert_eq!((small.width, small.height), (256, 171));
    assert!(took < Duration::from_secs(2), "{took:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn video_facts_and_frames_come_from_windows() {
    let _com = Apartment::enter();
    let dir = scratch("video");
    let video = dir.join("clip.mp4");
    fixtures::write_video(&video, 320, 240, 15, 30, |_| quarters(320, 240)).unwrap();
    assert!(std::fs::metadata(&video).unwrap().len() > 1000);

    let facts = props::read(&text(&video), Kind::Video).unwrap();
    assert_eq!((facts.width, facts.height), (Some(320), Some(240)));
    let duration = facts.duration_ms.expect("a length");
    assert!((1900..=2100).contains(&duration), "{duration} ms");
    let rate = facts.frame_rate.expect("a frame rate");
    assert!((rate - 15.0).abs() < 0.1, "{rate}");
    assert_eq!(facts.codec.as_deref(), Some("H.264"));

    let frame = decode::video_frame(&text(&video), 0.5, 160).unwrap();
    assert_eq!((frame.bitmap.width, frame.bitmap.height), (160, 120));
    let b = &frame.bitmap;
    let corners = [b.at(20, 20), b.at(140, 20), b.at(20, 100), b.at(140, 100)].map(quarter_colour);
    assert_eq!(corners, ['r', 'g', 'b', 'w'], "the frame is the right way up");
    assert!(frame.duration_ms.is_some_and(|ms| (1900..=2100).contains(&ms)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn text_in_an_image_is_recognised() {
    let _com = Apartment::enter();
    let dir = scratch("ocr");
    let picture = dir.join("receipt.png");
    let drawn = fixtures::text_bitmap(&["Kestrel receipt", "COSTCO WHOLESALE", "Order 42.17"], 900, 300).unwrap();
    std::fs::write(&picture, drawn.to_png().unwrap()).unwrap();
    let decoded = decode::image(&text(&picture), 2600, 1).unwrap();
    let recognizer = crate::ocr::Recognizer::for_user().expect("this computer has a recogniser for its language");
    assert!(!recognizer.languages().is_empty());
    let read = recognizer.recognize(&decoded).unwrap();
    let lower = read.to_lowercase();
    assert!(lower.contains("costco"), "{read}");
    assert!(lower.contains("42.17"), "{read}");
    assert!(lower.contains("receipt"), "{read}");
    assert_eq!(read.lines().count(), 3, "{read}");
    // A blank picture has no text.
    assert_eq!(recognizer.recognize(&crate::Bitmap::filled(400, 300, [255, 255, 255, 255])).unwrap(), "");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------- the pipeline ----------

/// A stage that remembers which items it ran on, and how large the picture it got was.
#[derive(Default)]
struct Recording {
    runs: Mutex<Vec<(i64, String, Option<(u32, u32)>)>>,
    delay: Duration,
}

impl Stage for Recording {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn version(&self) -> u32 {
        1
    }

    fn needs(&self) -> Needs {
        Needs { pixels: 64 }
    }

    fn run(&self, item: &Item, input: &Input) -> Result<(), String> {
        std::thread::sleep(self.delay);
        let size = input.picture.map(|p| (p.width, p.height));
        self.runs.lock().push((item.id, item.path.clone(), size));
        Ok(())
    }
}

impl Recording {
    fn count(&self) -> usize {
        self.runs.lock().len()
    }

    fn by_path(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for (_, path, _) in self.runs.lock().iter() {
            *counts.entry(path.rsplit('\\').next().unwrap().to_owned()).or_default() += 1;
        }
        counts
    }
}

fn pipeline(db: &Arc<Db>, stage: &Arc<Recording>, conditions: impl Fn() -> Option<String> + Send + Sync + 'static) -> Pipeline {
    let pipeline = Pipeline::new(db.clone(), Config { workers: 2, conditions: Box::new(conditions), recheck: Duration::from_millis(50) });
    pipeline.register(stage.clone());
    pipeline.start();
    pipeline
}

fn candidates(dir: &Path) -> Vec<crate::pipeline::Candidate> {
    let mut builder = IndexBuilder::new();
    walk::walk(&text(dir), &mut builder).unwrap();
    crate::scan::scan(&builder.finish(), &[text(dir)])
}

#[test]
fn renames_and_moves_are_never_read_again_and_changes_always_are() {
    let _com = Apartment::enter();
    let dir = scratch("keys");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    fixtures::write_jpeg(&dir.join("a.jpg"), &quarters(200, 100), &Exif { make: Some("First".into()), ..Exif::default() }).unwrap();
    fixtures::write_jpeg(&dir.join("b.jpg"), &quarters(100, 200), &Exif { orientation: Some(8), ..Exif::default() }).unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Recording::default());
    let pipeline = pipeline(&db, &stage, || None);

    assert_eq!(pipeline.plan(candidates(&dir)).unwrap(), (2, 0, 0));
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.by_path(), HashMap::from([("a.jpg".to_owned(), 1), ("b.jpg".to_owned(), 1)]));
    // The stage got the picture fitted to what it asked for, turned as shown.
    let sizes: Vec<_> = stage.runs.lock().iter().map(|(_, path, size)| (path.ends_with("b.jpg"), size.unwrap())).collect();
    assert!(sizes.contains(&(false, (64, 32))) && sizes.contains(&(true, (64, 32))), "{sizes:?}");
    let progress = pipeline.progress();
    assert_eq!((progress.total, progress.done, progress.queued, progress.state), (2, 2, 0, "idle"));

    // Renamed, then moved into a subfolder: the same items, nothing read again.
    std::fs::rename(dir.join("a.jpg"), dir.join("renamed.jpg")).unwrap();
    std::fs::rename(dir.join("b.jpg"), dir.join("sub").join("b.jpg")).unwrap();
    assert_eq!(pipeline.plan(candidates(&dir)).unwrap(), (0, 2, 0));
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.count(), 2, "{:?}", stage.by_path());
    let items = pipeline.items();
    assert!(items.iter().any(|item| item.path.ends_with("renamed.jpg")));

    // Changed content: read again, facts and all.
    std::thread::sleep(Duration::from_millis(20));
    fixtures::write_jpeg(&dir.join("renamed.jpg"), &quarters(300, 100), &Exif { make: Some("Second".into()), ..Exif::default() }).unwrap();
    assert_eq!(pipeline.plan(candidates(&dir)).unwrap(), (0, 1, 0));
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.by_path().get("renamed.jpg"), Some(&1));
    assert_eq!(stage.count(), 3);
    let item = items.iter().find(|item| item.path.ends_with("renamed.jpg")).unwrap();
    let facts = kestrel_db::media::facts(&db, item.id).unwrap().unwrap();
    assert_eq!((facts.make.as_deref(), facts.width), (Some("Second"), Some(300)));

    // Deleted: gone, and counted no more.
    std::fs::remove_file(dir.join("sub").join("b.jpg")).unwrap();
    assert_eq!(pipeline.plan(candidates(&dir)).unwrap(), (0, 0, 1));
    assert_eq!(pipeline.progress().total, 1);

    // A new pipeline over the same library knows all of it: nothing is read again.
    pipeline.stop();
    let again = Arc::new(Recording::default());
    let restarted = self::pipeline(&db, &again, || None);
    restarted.plan(candidates(&dir)).unwrap();
    assert!(restarted.wait_idle(Duration::from_secs(30)));
    assert_eq!(again.count(), 0);
    restarted.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Holds a file open with no sharing, so anything that tried to read it would fail and say so.
fn hold(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).share_mode(0).open(path).unwrap()
}

fn set_attributes(path: &Path, attributes: u32) {
    let wide: Vec<u16> = text(path).encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    assert_ne!(unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attributes) }, 0);
}

#[test]
fn online_only_files_are_never_opened() {
    let _com = Apartment::enter();
    let dir = scratch("cloud");
    for name in ["local.jpg", "cloud.jpg", "offline.jpg"] {
        fixtures::write_jpeg(&dir.join(name), &quarters(64, 64), &camera_exif()).unwrap();
    }
    // The index is made up: it says cloud.jpg is a OneDrive placeholder (online only). On disk
    // offline.jpg carries the offline attribute, which the index has not heard of yet.
    let mut builder = IndexBuilder::new();
    let root = builder.add_root(&text(&dir));
    for name in ["local.jpg", "cloud.jpg", "offline.jpg"] {
        let mut meta: Meta = walk::info(&text(&dir.join(name))).unwrap();
        if name == "cloud.jpg" {
            meta.attrs |= 0x40_0000 | 0x1000;
        }
        builder.push(root, name, meta);
    }
    let index = builder.finish();
    set_attributes(&dir.join("offline.jpg"), 0x1000);
    // Both are held open with no sharing: reading either would fail, and the failure be recorded.
    let held = [hold(&dir.join("cloud.jpg")), hold(&dir.join("offline.jpg"))];

    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Recording::default());
    let pipeline = pipeline(&db, &stage, || None);
    pipeline.plan(crate::scan::scan(&index, &[text(&dir)])).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.by_path(), HashMap::from([("local.jpg".to_owned(), 1)]));
    let progress = pipeline.progress();
    assert_eq!((progress.total, progress.done), (3, 1));
    assert_eq!(progress.skipped.len(), 1, "{:?}", progress.skipped);
    assert_eq!((progress.skipped[0].reason.as_str(), progress.skipped[0].count), (ONLINE_ONLY, 2));
    // Nothing tried and failed: no record names either file.
    for known in kestrel_db::media::known(&db).unwrap() {
        let local = known.path.ends_with("local.jpg");
        assert_eq!(known.stages.is_empty(), !local, "{}: {:?}", known.path, known.stages);
    }
    drop(held);
    pipeline.stop();
    set_attributes(&dir.join("offline.jpg"), 0x80);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_pass_pauses_on_demand_and_waits_for_full_screen_apps() {
    let _com = Apartment::enter();
    let dir = scratch("pause");
    for n in 0..8 {
        fixtures::write_jpeg(&dir.join(format!("p{n}.jpg")), &quarters(32, 32), &Exif::default()).unwrap();
    }
    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Recording { delay: Duration::from_millis(150), ..Recording::default() });
    let game = Arc::new(AtomicBool::new(false));
    let playing = game.clone();
    let pipeline = pipeline(&db, &stage, move || playing.load(Ordering::Relaxed).then(|| crate::conditions::GAME.to_owned()));
    pipeline.plan(candidates(&dir)).unwrap();
    let started = Instant::now();
    while stage.count() == 0 && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(10));
    }

    pipeline.set_paused(true);
    // What was in hand finishes; nothing more starts.
    std::thread::sleep(Duration::from_millis(400));
    let at_pause = stage.count();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(stage.count(), at_pause, "nothing runs while paused");
    assert!(at_pause < 8);
    let progress = pipeline.progress();
    assert_eq!(progress.state, "paused");
    assert_eq!(progress.queued + progress.done, 8);

    // Resumed, but a game is running: the pass waits, and says why.
    game.store(true, Ordering::Relaxed);
    pipeline.set_paused(false);
    std::thread::sleep(Duration::from_millis(400));
    let progress = pipeline.progress();
    assert_eq!((progress.state, progress.reason.as_deref()), ("waiting", Some(crate::conditions::GAME)));
    assert_eq!(stage.count(), at_pause);

    game.store(false, Ordering::Relaxed);
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.count(), 8);
    assert_eq!(pipeline.progress().state, "idle");
    pipeline.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn items_on_screen_are_read_first() {
    let _com = Apartment::enter();
    let dir = scratch("hurry");
    let base = std::time::SystemTime::now() - Duration::from_secs(3600);
    for n in 0..6 {
        let path = dir.join(format!("h{n}.jpg"));
        fixtures::write_jpeg(&path, &quarters(32, 32), &Exif::default()).unwrap();
        // h0 is the oldest, so it would be read last.
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(base + Duration::from_secs(n * 60)).unwrap();
    }
    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Recording::default());
    let pipeline = Pipeline::new(db.clone(), Config { workers: 1, conditions: Box::new(|| None), recheck: Duration::from_millis(50) });
    pipeline.register(stage.clone());
    pipeline.set_paused(true);
    pipeline.start();
    let found = candidates(&dir);
    let oldest = found.iter().find(|c| c.path.ends_with("h0.jpg")).map(|c| (c.volume.clone(), c.file_id)).unwrap();
    pipeline.plan(found).unwrap();
    assert_eq!(pipeline.hurry(&[oldest.clone(), ("z:".into(), 1)]), 1, "only items with work left are hurried");
    pipeline.set_paused(false);
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    let order: Vec<String> = stage.runs.lock().iter().map(|(_, path, _)| path.rsplit('\\').next().unwrap().to_owned()).collect();
    assert_eq!(order.first().map(String::as_str), Some("h0.jpg"), "{order:?}");
    assert_eq!(order[1..], ["h5.jpg", "h4.jpg", "h3.jpg", "h2.jpg", "h1.jpg"], "then newest first");
    assert_eq!(pipeline.hurry(&[oldest]), 0, "nothing left to do");
    pipeline.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failures_are_recorded_with_their_reason_and_not_retried() {
    let _com = Apartment::enter();
    let dir = scratch("broken");
    std::fs::write(dir.join("broken.jpg"), b"not a picture at all").unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Recording::default());
    let pipeline = pipeline(&db, &stage, || None);
    pipeline.plan(candidates(&dir)).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    // The picture couldn't be decoded, so the stage that needs it was never run.
    assert_eq!(stage.count(), 0);
    let progress = pipeline.progress();
    assert_eq!((progress.total, progress.done, progress.queued), (1, 0, 0));
    assert_eq!(progress.skipped.len(), 1);
    assert!(!progress.skipped[0].reason.is_empty());
    // Planned again unchanged: not tried again.
    pipeline.plan(candidates(&dir)).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(pipeline.progress().skipped[0].count, 1);
    pipeline.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_text_stage_stores_what_it_reads_for_searches() {
    let _com = Apartment::enter();
    let dir = scratch("text-stage");
    let drawn = fixtures::text_bitmap(&["Gate 42 boarding", "KESTREL AIRWAYS"], 800, 220).unwrap();
    std::fs::write(dir.join("ticket.png"), drawn.to_png().unwrap()).unwrap();
    fixtures::write_jpeg(&dir.join("blank.jpg"), &crate::Bitmap::filled(320, 240, [200, 200, 200, 255]), &Exif::default()).unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let pipeline = Pipeline::new(db.clone(), Config { workers: 1, conditions: Box::new(|| None), recheck: Duration::from_millis(50) });
    pipeline.register(Arc::new(crate::ocr::TextStage::new(db.clone())));
    pipeline.start();
    pipeline.plan(candidates(&dir)).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(60)));
    let progress = pipeline.progress();
    assert_eq!((progress.done, progress.skipped.len()), (2, 0), "{progress:?}");

    let mut builder = IndexBuilder::new();
    walk::walk(&text(&dir), &mut builder).unwrap();
    let index = builder.finish();
    let search = |typed: &str| {
        let mut query = kestrel_query::parse(typed, kestrel_query::Clock::local()).unwrap();
        crate::query::resolve(&db, &mut query, &index).unwrap();
        let compiled = kestrel_query::compile(&query, &index).unwrap();
        let ids = kestrel_query::search(&index, &compiled, &AtomicBool::new(false)).unwrap();
        let mut names: Vec<String> = ids.iter().map(|&id| index.name(id).to_owned()).collect();
        names.sort();
        names
    };
    assert_eq!(search("text:kestrel"), ["ticket.png"]);
    assert_eq!(search("text:\"gate 42\""), ["ticket.png"]);
    assert_eq!(search("!text:gate *.*"), ["blank.jpg"]);
    assert_eq!(search("width:>500"), ["ticket.png"]);
    assert_eq!(search("height:240 kind:photo"), ["blank.jpg"]);
    assert_eq!(search("kind:video"), Vec::<String>::new());
    assert_eq!(search("geo:no"), ["blank.jpg", "ticket.png"]);
    pipeline.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A stage whose model comes and goes: not ready, ready at one version, then at another.
#[derive(Default)]
struct Modelled {
    /// 0 while no model is loaded.
    version: std::sync::atomic::AtomicU32,
    runs: Mutex<Vec<(String, u32)>>,
    /// Makes the next run take the model away while it runs.
    lose_model: AtomicBool,
}

impl Stage for Modelled {
    fn name(&self) -> &'static str {
        "modelled"
    }

    fn version(&self) -> u32 {
        self.version.load(Ordering::SeqCst)
    }

    fn ready(&self) -> bool {
        self.version() != 0
    }

    fn needs(&self) -> Needs {
        Needs { pixels: 32 }
    }

    fn run(&self, item: &Item, _: &Input) -> Result<(), String> {
        if self.lose_model.swap(false, Ordering::SeqCst) {
            self.version.store(0, Ordering::SeqCst);
            return Err("the model went".into());
        }
        let name = item.path.rsplit('\\').next().unwrap().to_owned();
        self.runs.lock().push((name, self.version()));
        Ok(())
    }
}

#[test]
fn a_stage_waits_until_it_is_ready_and_runs_again_for_another_version() {
    let _com = Apartment::enter();
    let dir = scratch("ready");
    fixtures::write_jpeg(&dir.join("a.jpg"), &quarters(120, 80), &Exif::default()).unwrap();
    fixtures::write_jpeg(&dir.join("b.jpg"), &quarters(80, 120), &Exif::default()).unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let stage = Arc::new(Modelled::default());
    let pipeline = Pipeline::new(db.clone(), Config { workers: 1, conditions: Box::new(|| None), recheck: Duration::from_millis(50) });
    pipeline.register(stage.clone());
    pipeline.start();

    // Not ready: the metadata stage runs, this one has no work and nothing is queued for it.
    pipeline.plan(candidates(&dir)).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert!(stage.runs.lock().is_empty());
    assert_eq!(pipeline.progress().queued, 0);

    // Ready: everything is queued for it once asked.
    stage.version.store(7, Ordering::SeqCst);
    pipeline.requeue();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.runs.lock().len(), 2);
    assert!(stage.runs.lock().iter().all(|(_, version)| *version == 7));

    // Another version (another model): everything again.
    stage.version.store(8, Ordering::SeqCst);
    pipeline.requeue();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.runs.lock().len(), 4);

    // The model goes while a changed file is read: that failure isn't recorded, and the file
    // is read once the model is back.
    std::thread::sleep(Duration::from_millis(20));
    fixtures::write_jpeg(&dir.join("a.jpg"), &quarters(160, 80), &Exif::default()).unwrap();
    stage.lose_model.store(true, Ordering::SeqCst);
    pipeline.plan(candidates(&dir)).unwrap();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.runs.lock().len(), 4);
    assert!(pipeline.progress().skipped.is_empty(), "{:?}", pipeline.progress().skipped);
    stage.version.store(8, Ordering::SeqCst);
    pipeline.requeue();
    assert!(pipeline.wait_idle(Duration::from_secs(30)));
    assert_eq!(stage.runs.lock().last().unwrap(), &("a.jpg".to_owned(), 8));
    assert_eq!(stage.runs.lock().len(), 5);
    pipeline.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}
