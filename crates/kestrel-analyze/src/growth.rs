//! What grew: once a day the sizes of the larger folders (a few levels deep) and of the large
//! files are written down, and the index now is compared with the sample taken a day, a week
//! or a month ago.
//!
//! A sample records only what is above a size threshold, so a folder or file missing from an
//! older sample was smaller than the threshold then (or did not exist): its growth is known only
//! to be at least what it has now beyond the threshold, and it is reported that way.
//!
//! Samples are kept daily for 90 days, then one a week ([`days_to_keep`]).

use std::collections::HashMap;

use kestrel_index::{EntryId, Index};
use serde::{Deserialize, Serialize};

use crate::{DAY_MS, unix_ms};

/// Samples newer than this many days are all kept; older ones, one a week.
pub const DAILY_FOR_DAYS: i64 = 90;
/// A folder that holds this share of its parent's growth stands for the parent in the list.
const PASS_ON_SHARE: f64 = 0.8;

/// One folder or file of a sample.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleRow {
    pub path: String,
    /// Size on disk.
    pub bytes: u64,
    pub file: bool,
    /// A file's modification time, in milliseconds since the Unix epoch; 0 for folders.
    pub mtime: i64,
}

#[derive(Clone, Debug)]
pub struct SampleOptions {
    /// Folders at least this big are recorded.
    pub folder_min: u64,
    /// Folder levels below each root recorded, at most.
    pub max_depth: u32,
    /// Files at least this big are recorded, wherever they are.
    pub file_min: u64,
}

impl Default for SampleOptions {
    fn default() -> Self {
        Self { folder_min: 64 << 20, max_depth: 5, file_min: 100 << 20 }
    }
}

/// Takes a sample of the subtrees of `roots` (drives, usually): every root itself, the folders
/// at least `folder_min` big down to `max_depth` levels, and every file at least `file_min`.
pub fn sample(index: &Index, roots: &[EntryId], options: &SampleOptions) -> Vec<SampleRow> {
    let mut rows = Vec::new();
    let mut stack: Vec<(EntryId, u32)> = roots.iter().map(|&root| (root, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        rows.push(SampleRow { path: index.path(dir), bytes: index.alloc(dir), file: false, mtime: 0 });
        if depth >= options.max_depth {
            continue;
        }
        // A folder's subfolders are smaller than it, so only big folders are looked inside.
        for child in index.children(dir) {
            if index.is_dir(child) && index.alloc(child) >= options.folder_min {
                stack.push((child, depth + 1));
            }
        }
    }
    // Large files are few; one pass over the whole index finds them.
    for id in 0..index.len() {
        if index.is_deleted(id) || index.is_dir(id) || index.alloc(id) < options.file_min {
            continue;
        }
        if roots.iter().any(|&root| index.is_under(id, root)) {
            rows.push(SampleRow { path: index.path(id), bytes: index.alloc(id), file: true, mtime: unix_ms(index.mtime(id)) });
        }
    }
    rows
}

/// How one folder or file changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub path: String,
    /// Size on disk at the time of the sample; `None` when it was under the threshold.
    pub before: Option<u64>,
    pub now: u64,
    /// How much it grew: exactly when `before` is known, otherwise at least this much.
    pub grew: u64,
    pub file: bool,
    pub mtime: i64,
}

/// What grew within a part of the disk since a sample.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Growth {
    /// The day of the sample compared with, in days since the Unix epoch.
    pub since_day: i64,
    /// The scope's size on disk then, if it was recorded, and now.
    pub before: Option<u64>,
    pub now: u64,
    /// Folders that grew most, leaving out those whose growth is nearly all one subfolder's.
    pub folders: Vec<Change>,
    /// Large files that grew.
    pub files: Vec<Change>,
    /// Large files written since the sample that it did not record.
    pub new_files: Vec<Change>,
}

fn key(path: &str) -> String {
    path.trim_end_matches('\\').to_lowercase()
}

/// Whether `path` is `scope` or lies beneath it; both as given by [`key`].
fn within(path: &str, scope: &str) -> bool {
    path == scope || (path.len() > scope.len() && path.starts_with(scope) && path.as_bytes()[scope.len()] == b'\\')
}

/// Compares the sample `current`, taken from the index now, with `baseline`, taken on day
/// `since_day`, within the folder `scope`, whose size on disk is now `scope_now`. Lists at most
/// `top` of each kind of change.
pub fn compare(
    scope: &str,
    scope_now: u64,
    since_day: i64,
    baseline: &[SampleRow],
    current: &[SampleRow],
    options: &SampleOptions,
    top: usize,
) -> Growth {
    let scope_key = key(scope);
    let then: HashMap<String, &SampleRow> = baseline.iter().map(|row| (key(&row.path), row)).collect();
    let since_ms = since_day * DAY_MS;
    let mut folders: Vec<(String, Change)> = Vec::new();
    let mut files = Vec::new();
    let mut new_files = Vec::new();
    for row in current {
        let row_key = key(&row.path);
        if !within(&row_key, &scope_key) || row_key == scope_key {
            continue;
        }
        let before = then.get(&row_key).filter(|old| old.file == row.file).map(|old| old.bytes);
        let threshold = if row.file { options.file_min } else { options.folder_min };
        let grew = match before {
            Some(before) => row.bytes.saturating_sub(before),
            None => row.bytes.saturating_sub(threshold),
        };
        let change = Change { path: row.path.clone(), before, now: row.bytes, grew, file: row.file, mtime: row.mtime };
        match (row.file, before) {
            (false, _) if grew > 0 => folders.push((row_key, change)),
            (true, Some(_)) if grew > 0 => files.push(change),
            // Not recorded then: new, or rewritten (and so moved past the threshold) since.
            (true, None) if row.mtime >= since_ms => new_files.push(change),
            _ => {}
        }
    }

    // A folder whose growth is nearly all one subfolder's only passes it on.
    let mut child_growth: HashMap<String, u64> = HashMap::new();
    for (row_key, change) in &folders {
        if let Some((parent, _)) = row_key.rsplit_once('\\') {
            let largest = child_growth.entry(parent.to_owned()).or_default();
            *largest = (*largest).max(change.grew);
        }
    }
    let mut folders: Vec<Change> = folders
        .into_iter()
        .filter(|(row_key, change)| (child_growth.get(row_key).copied().unwrap_or(0) as f64) < PASS_ON_SHARE * change.grew as f64)
        .map(|(_, change)| change)
        .collect();
    let order = |a: &Change, b: &Change| b.grew.cmp(&a.grew).then_with(|| a.path.cmp(&b.path));
    folders.sort_by(order);
    folders.truncate(top);
    files.sort_by(order);
    files.truncate(top);
    new_files.sort_by(|a, b| b.now.cmp(&a.now).then_with(|| a.path.cmp(&b.path)));
    new_files.truncate(top);
    Growth {
        since_day,
        before: then.get(&scope_key).filter(|row| !row.file).map(|row| row.bytes),
        now: scope_now,
        folders,
        files,
        new_files,
    }
}

/// The latest of the sampled `days` at least `window` days before `today`: what "the last
/// day", "week" or "month" is measured from. `None` while there is no sample that old.
pub fn baseline_day(days: &[i64], today: i64, window: i64) -> Option<i64> {
    days.iter().copied().filter(|&day| day <= today - window).max()
}

/// Which of the sampled `days` to keep on `today`: every day of the last [`DAILY_FOR_DAYS`],
/// and before that the first sample of each week.
pub fn days_to_keep(days: &[i64], today: i64) -> Vec<i64> {
    let mut first_of_week: HashMap<i64, i64> = HashMap::new();
    for &day in days.iter().filter(|&&day| day <= today - DAILY_FOR_DAYS) {
        let first = first_of_week.entry(day.div_euclid(7)).or_insert(day);
        *first = (*first).min(day);
    }
    let mut keep: Vec<i64> = days.iter().copied().filter(|&day| day > today - DAILY_FOR_DAYS).collect();
    keep.extend(first_of_week.into_values());
    keep.sort_unstable();
    keep.dedup();
    keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{F, build, id};
    use crate::filetime;

    const GB: u64 = 1 << 30;
    const OPTIONS: SampleOptions = SampleOptions { folder_min: GB / 10, max_depth: 4, file_min: GB / 2 };

    fn folder(path: &str, gb: f64) -> SampleRow {
        SampleRow { path: path.to_owned(), bytes: (gb * GB as f64) as u64, file: false, mtime: 0 }
    }

    fn file(path: &str, gb: f64, day: i64) -> SampleRow {
        SampleRow { path: path.to_owned(), bytes: (gb * GB as f64) as u64, file: true, mtime: day * DAY_MS + 3_600_000 }
    }

    fn baseline() -> Vec<SampleRow> {
        vec![
            folder(r"C:\", 500.0),
            folder(r"C:\Users", 100.0),
            folder(r"C:\Users\me", 80.0),
            folder(r"C:\Users\me\Downloads", 10.0),
            folder(r"C:\Windows", 30.0),
            file(r"C:\Users\me\Downloads\big.iso", 4.0, 50),
            file(r"C:\Users\me\vm.vhdx", 20.0, 90),
        ]
    }

    fn current() -> Vec<SampleRow> {
        vec![
            folder(r"C:\", 515.0),
            folder(r"C:\Users", 115.0),
            folder(r"C:\Users\me", 95.0),
            folder(r"C:\Users\me\Downloads", 19.0),
            folder(r"C:\Users\me\Videos", 5.0),
            folder(r"C:\Windows", 29.0),
            file(r"C:\Users\me\Downloads\big.iso", 4.0, 50),
            file(r"C:\Users\me\Downloads\new.mkv", 5.0, 105),
            file(r"C:\Users\me\vm.vhdx", 21.0, 106),
            // Moved in from elsewhere, written long ago: not new.
            file(r"C:\Users\me\moved.zip", 1.0, 20),
        ]
    }

    #[test]
    fn growth_is_measured_against_the_sample_and_passed_down_to_where_it_happened() {
        let growth = compare(r"C:\", 515 * GB, 100, &baseline(), &current(), &OPTIONS, 10);
        assert_eq!((growth.since_day, growth.before, growth.now), (100, Some(500 * GB), 515 * GB));
        let folders: Vec<(&str, Option<u64>, u64)> = growth.folders.iter().map(|c| (c.path.as_str(), c.before, c.grew)).collect();
        // Users grew by what me did, so me stands for it; me's growth is shared by Downloads and
        // Videos, so all three are listed. Videos was under the threshold then, so it grew by at
        // least what it has beyond it. Windows shrank.
        assert_eq!(
            folders,
            [
                (r"C:\Users\me", Some(80 * GB), 15 * GB),
                (r"C:\Users\me\Downloads", Some(10 * GB), 9 * GB),
                (r"C:\Users\me\Videos", None, 5 * GB - GB / 10),
            ]
        );
        let files: Vec<(&str, u64)> = growth.files.iter().map(|c| (c.path.as_str(), c.grew)).collect();
        assert_eq!(files, [(r"C:\Users\me\vm.vhdx", GB)]);
        let new: Vec<(&str, u64)> = growth.new_files.iter().map(|c| (c.path.as_str(), c.now)).collect();
        assert_eq!(new, [(r"C:\Users\me\Downloads\new.mkv", 5 * GB)]);
    }

    #[test]
    fn growth_is_limited_to_the_scope() {
        let growth = compare(r"C:\Users\me\Downloads", 19 * GB, 100, &baseline(), &current(), &OPTIONS, 10);
        assert_eq!((growth.before, growth.now), (Some(10 * GB), 19 * GB));
        assert!(growth.folders.is_empty() && growth.files.is_empty());
        assert_eq!(growth.new_files.len(), 1);
        // Paths are compared without regard to case, and a scope never matches a mere prefix.
        let growth = compare(r"c:\users\ME", 95 * GB, 100, &baseline(), &current(), &OPTIONS, 10);
        assert_eq!(growth.before, Some(80 * GB));
        let growth = compare(r"C:\Users\m", 0, 100, &baseline(), &current(), &OPTIONS, 10);
        assert!(growth.before.is_none() && growth.folders.is_empty() && growth.new_files.is_empty());
    }

    #[test]
    fn windows_start_from_the_latest_sample_old_enough() {
        let days = [100, 103, 106, 107];
        assert_eq!(baseline_day(&days, 107, 1), Some(106));
        assert_eq!(baseline_day(&days, 107, 7), Some(100));
        assert_eq!(baseline_day(&days, 107, 30), None);
        assert_eq!(baseline_day(&[107], 107, 1), None, "today's own sample is no history");
    }

    #[test]
    fn samples_are_kept_daily_for_ninety_days_then_weekly() {
        let today = 20_000;
        let days: Vec<i64> = (today - 200..=today).collect();
        let keep = days_to_keep(&days, today);
        // The last 89 days and today, every one.
        assert!((today - DAILY_FOR_DAYS + 1..=today).all(|day| keep.contains(&day)));
        let older: Vec<i64> = keep.iter().copied().filter(|&day| day <= today - DAILY_FOR_DAYS).collect();
        // One for every week, the first sample of it.
        let weeks: Vec<i64> = older.iter().map(|day| day.div_euclid(7)).collect();
        assert!(weeks.windows(2).all(|pair| pair[1] == pair[0] + 1), "{older:?}");
        assert!(older.iter().all(|day| day.rem_euclid(7) == 0 || *day == today - 200), "{older:?}");
        assert_eq!(keep.len(), DAILY_FOR_DAYS as usize + older.len());
        // Keeping is stable: the kept days, pruned again later, lose only what has aged out.
        let later = days_to_keep(&keep, today + 1);
        assert!(later.iter().all(|day| keep.contains(day)));
        assert_eq!(days_to_keep(&[], today), Vec::<i64>::new());
    }

    #[test]
    fn a_sample_records_big_folders_a_few_levels_deep_and_big_files() {
        const MB: u64 = 1 << 20;
        let now = filetime(1_767_225_600_000);
        let index = build(
            &[
                F(r"Users\me\Videos\movie.mkv", 300 * MB, now, 0),
                F(r"Users\me\Videos\clip.mp4", 20 * MB, now, 0),
                F(r"Users\me\small\a\b\c\deep.bin", 90 * MB, now, 0),
                F(r"Users\me\tiny\x.txt", MB, now, 0),
            ],
            &[],
        );
        let options = SampleOptions { folder_min: 64 * MB, max_depth: 3, file_min: 100 * MB };
        let rows = sample(&index, &[id(&index, r"C:\")], &options);
        let mut paths: Vec<(&str, bool)> = rows.iter().map(|row| (row.path.as_str(), row.file)).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                (r"C:\", false),
                (r"C:\Users", false),
                (r"C:\Users\me", false),
                (r"C:\Users\me\Videos", false),
                (r"C:\Users\me\Videos\movie.mkv", true),
                (r"C:\Users\me\small", false),
            ]
        );
        let movie = rows.iter().find(|row| row.file).unwrap();
        assert_eq!((movie.bytes, movie.mtime), (300 * MB, 1_767_225_600_000));
    }
}
