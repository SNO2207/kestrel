//! The daily sample of folder sizes (`kestrel_db::disk`) and what grew since one.
//!
//! Once the index is ready, and every so often after, today's sample is taken if there is none
//! yet (a few milliseconds over millions of entries), and samples older than the retention
//! policy allows are deleted: daily ones for 90 days, then one a week.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

use kestrel_analyze::DAY_MS;
use kestrel_analyze::growth::{self, Growth, SampleOptions, SampleRow};
use kestrel_db::Db;
use kestrel_db::disk as store;
use serde::Serialize;

use super::{locate, now_ms, top_of};
use crate::engine::Engine;

/// How often to look whether today's sample is due, once the index is ready...
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
/// ...and while it is not.
const WAIT_FOR_INDEX: Duration = Duration::from_secs(15);
/// What the hub compares with: the sample a day, a week and a month old.
const WINDOWS: [(i64, &str); 3] = [(1, "Last day"), (7, "Last week"), (30, "Last month")];
/// Changes listed of each kind.
const TOP: usize = 15;

/// Today, in days since the Unix epoch (UTC).
fn today() -> i64 {
    now_ms().div_euclid(DAY_MS)
}

fn to_store(row: SampleRow) -> store::Row {
    store::Row { path: row.path, bytes: row.bytes, file: row.file, mtime: row.mtime }
}

fn from_store(row: store::Row) -> SampleRow {
    SampleRow { path: row.path, bytes: row.bytes, file: row.file, mtime: row.mtime }
}

/// Takes today's sample of every drive if there is none yet, then deletes the samples the
/// retention policy no longer keeps. Returns whether a sample was taken.
pub fn sample_if_due(engine: &Engine, db: &Db, today: i64, taken_at: i64) -> Result<bool, String> {
    let mut days = store::days(db)?;
    if days.contains(&today) {
        return Ok(false);
    }
    let rows: Vec<store::Row> = {
        let index = engine.index().read();
        growth::sample(&index, index.roots(), &SampleOptions::default()).into_iter().map(to_store).collect()
    };
    store::save(db, today, taken_at, &rows)?;
    days.push(today);
    let keep = growth::days_to_keep(&days, today);
    let old: Vec<i64> = days.into_iter().filter(|day| !keep.contains(day)).collect();
    store::delete_days(db, &old)?;
    Ok(true)
}

/// Keeps the samples coming for as long as the app runs.
pub fn run(engine: &Engine, db: &Db) {
    loop {
        if engine.status().state != "ready" {
            std::thread::sleep(WAIT_FOR_INDEX);
            continue;
        }
        if let Err(error) = sample_if_due(engine, db, today(), now_ms()) {
            eprintln!("could not record today's disk usage sample: {error}");
        }
        std::thread::sleep(CHECK_EVERY);
    }
}

#[derive(Serialize)]
pub struct WindowView {
    days: i64,
    label: &'static str,
    /// `None` while no sample is that old.
    growth: Option<Growth>,
}

#[derive(Serialize)]
pub struct GrowthView {
    path: String,
    today: i64,
    /// The oldest sample kept, in days since the Unix epoch.
    first_day: Option<i64>,
    samples: usize,
    /// The thresholds of the samples, so the hub can say what "under the threshold" means.
    folder_min: u64,
    file_min: u64,
    windows: Vec<WindowView>,
}

/// What grew within the folder at `path` over the last day, week and month.
pub fn growth(engine: &Engine, db: &Db, path: &str) -> Result<GrowthView, String> {
    growth_on(engine, db, path, today())
}

fn growth_on(engine: &Engine, db: &Db, path: &str, today: i64) -> Result<GrowthView, String> {
    let options = SampleOptions::default();
    let days = store::days(db)?;
    // The comparison needs today's sizes taken the way the samples were: from the drive's root.
    let (shown, scope_now, current) = {
        let index = engine.index().read();
        let root = locate(&index, path)?;
        let current = growth::sample(&index, &[top_of(&index, root)], &options);
        (index.path(root), index.alloc(root), current)
    };
    let mut baselines: HashMap<i64, Vec<SampleRow>> = HashMap::new();
    let mut windows = Vec::with_capacity(WINDOWS.len());
    for (window, label) in WINDOWS {
        let growth = match growth::baseline_day(&days, today, window) {
            Some(day) => {
                // Windows often share a sample (while history is short), which is read once.
                let baseline = match baselines.entry(day) {
                    Entry::Occupied(known) => known.into_mut(),
                    Entry::Vacant(slot) => slot.insert(store::rows(db, day)?.into_iter().map(from_store).collect()),
                };
                Some(growth::compare(&shown, scope_now, day, baseline, &current, &options, TOP))
            }
            None => None,
        };
        windows.push(WindowView { days: window, label, growth });
    }
    Ok(GrowthView {
        path: shown,
        today,
        first_day: days.first().copied(),
        samples: days.len(),
        folder_min: options.folder_min,
        file_min: options.file_min,
        windows,
    })
}

/// What the development hook is asked to do with the samples.
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    /// Return a sample of the index as it is now, without keeping it.
    Current,
    /// Keep `rows` as the sample of `days_ago` days before today.
    Save,
    /// Delete the sample of `days_ago` days before today.
    Forget,
}

/// Development builds only: lets the end-to-end suite give the hub a history to compare with.
pub fn probe(engine: &Engine, db: &Db, action: Probe, days_ago: i64, rows: Vec<SampleRow>) -> Result<Vec<SampleRow>, String> {
    let day = today() - days_ago;
    match action {
        Probe::Current => {
            let index = engine.index().read();
            return Ok(growth::sample(&index, index.roots(), &SampleOptions::default()));
        }
        Probe::Save => store::save(db, day, now_ms(), &rows.into_iter().map(to_store).collect::<Vec<_>>())?,
        Probe::Forget => {
            store::delete_days(db, &[day])?;
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta, attr};

    const MB: u64 = 1 << 20;

    fn engine_with(downloads_mb: u64) -> Engine {
        let engine = Engine::default();
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let users = builder.push(root, "Users", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        let downloads = builder.push(users, "Downloads", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        let size = downloads_mb * MB;
        builder.push(downloads, "big.iso", Meta { size, alloc: size, mtime: 1, ..Meta::default() });
        engine.install(builder.finish());
        engine
    }

    #[test]
    fn a_sample_a_day_is_kept_and_compared_with() {
        let db = Db::open_in_memory().unwrap();
        let day = 20_300;
        assert!(sample_if_due(&engine_with(200), &db, day - 8, 0).unwrap());
        assert!(!sample_if_due(&engine_with(200), &db, day - 8, 0).unwrap(), "one sample a day");
        assert!(sample_if_due(&engine_with(250), &db, day - 1, 0).unwrap());

        let view = growth_on(&engine_with(300), &db, r"C:\Users", day).unwrap();
        assert_eq!((view.samples, view.first_day), (2, Some(day - 8)));
        let [last_day, last_week, last_month] = &view.windows[..] else { panic!() };
        let last_day = last_day.growth.as_ref().unwrap();
        assert_eq!((last_day.since_day, last_day.before, last_day.now), (day - 1, Some(250 * MB), 300 * MB));
        assert_eq!(last_day.files[0].grew, 50 * MB);
        let last_week = last_week.growth.as_ref().unwrap();
        assert_eq!((last_week.since_day, last_week.before), (day - 8, Some(200 * MB)));
        assert_eq!(last_week.folders.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), [r"C:\Users\Downloads"]);
        assert!(last_month.growth.is_none(), "no sample is a month old");
    }

    #[test]
    fn old_samples_thin_out_to_one_a_week() {
        let db = Db::open_in_memory().unwrap();
        let engine = engine_with(100);
        let start = 21_000;
        for day in start..start + 120 {
            sample_if_due(&engine, &db, day, 0).unwrap();
        }
        let days = store::days(&db).unwrap();
        let today = start + 119;
        assert!(days.iter().filter(|&&d| d > today - growth::DAILY_FOR_DAYS).count() == 90);
        let older = days.iter().filter(|&&d| d <= today - growth::DAILY_FOR_DAYS).count();
        assert!((4..=6).contains(&older), "{days:?}");
        assert!(store::rows(&db, days[0]).unwrap().iter().any(|row| row.path == r"C:\"));
    }
}
