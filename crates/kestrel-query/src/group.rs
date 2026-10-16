//! Explorer's "Group by": which group an item falls in, the groups' order and their names.
//!
//! Grouping happens after sorting and keeps the sort inside each group: every row gets the
//! ordinal of its group, and a stable counting sort by ordinal lines the groups up. That is
//! linear in the number of rows, so a million search results group in a few milliseconds, and
//! the boundaries fall out of the same pass. The names and the edges are Explorer's own, read
//! from its resources (`propsys.dll.mui`): dates in the user's local time and week, sizes in
//! the ranges Explorer's Size column groups by.

use kestrel_index::{EntryId, Index, attr};
use rayon::prelude::*;

use crate::sort::ext_ranks;
use crate::time::{civil_from_days, days_from_civil, month_start};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    Name,
    Modified,
    Type,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grouping {
    pub by: GroupBy,
    /// Groups run from the last ordinal to the first: newest, largest or Z first.
    pub descending: bool,
}

/// A run of rows that share a group: from `start` up to the next group's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub start: u32,
    pub label: String,
}

// ---------- attributes ----------

/// Attribute bits and the letters Explorer's Attributes column shows for them, in its order
/// (`RHSDAXNTLCOIEVPUMM` in propsys.dll.mui). Both recall bits show as M.
pub const ATTRIBUTE_LETTERS: [(u32, u8); 18] = [
    (0x1, b'R'),
    (0x2, b'H'),
    (0x4, b'S'),
    (0x10, b'D'),
    (0x20, b'A'),
    (0x2_0000, b'X'),
    (0x80, b'N'),
    (0x100, b'T'),
    (0x400, b'L'),
    (0x800, b'C'),
    (0x1000, b'O'),
    (0x2000, b'I'),
    (0x4000, b'E'),
    (0x8000, b'V'),
    (0x8_0000, b'P'),
    (0x10_0000, b'U'),
    (0x4_0000, b'M'),
    (0x40_0000, b'M'),
];

/// The letters Explorer shows for `attrs`, such as `HSA`.
pub fn attribute_letters(attrs: u32) -> String {
    let mut letters = String::new();
    for &(bit, letter) in &ATTRIBUTE_LETTERS {
        if attrs & bit != 0 && !letters.ends_with(letter as char) {
            letters.push(letter as char);
        }
    }
    letters
}

/// A number that orders like [`attribute_letters`] reads: the first eight letters, big-endian,
/// so a shorter string sorts before any longer one it begins.
pub fn attribute_key(attrs: u32) -> u64 {
    attribute_letters(attrs).bytes().take(8).enumerate().fold(0, |key, (i, byte)| key | (byte as u64) << (56 - 8 * i))
}

// ---------- names ----------

pub const NAME_LABELS: [&str; 5] = ["0 - 9", "A - H", "I - P", "Q - Z", "Other"];

/// The letter a Latin character is written with, without its accent: `é` is filed under E.
fn base_letter(c: char) -> Option<char> {
    // U+00C0 to U+017F (Latin-1 Supplement letters and Latin Extended-A), one letter each;
    // `.` marks the signs that are not letters.
    const LATIN: &str = concat!(
        "AAAAAAACEEEEIIII", // C0
        "DNOOOOO.OUUUUYTS", // D0
        "AAAAAAACEEEEIIII", // E0
        "DNOOOOO.OUUUUYTY", // F0
        "AAAAAACCCCCCCCDD", // 100
        "DDEEEEEEEEEEGGGG", // 110
        "GGGGHHHHIIIIIIII", // 120
        "IIJJJJKKKLLLLLLL", // 130
        "LLLNNNNNNNNNOOOO", // 140
        "OOOORRRRRRSSSSSS", // 150
        "SSTTTTTTUUUUUUUU", // 160
        "UUUUWWYYYZZZZZZS", // 170
    );
    if c.is_ascii_alphabetic() {
        return Some(c.to_ascii_uppercase());
    }
    let offset = (c as u32).checked_sub(0xC0)? as usize;
    LATIN.as_bytes().get(offset).filter(|&&b| b != b'.').map(|&b| b as char)
}

/// The group of a name, by its first character: `0 - 9`, `A - H`, `I - P`, `Q - Z` or `Other`.
pub fn name_bucket(name: &str) -> u32 {
    let Some(first) = name.chars().next() else { return 4 };
    if first.is_ascii_digit() {
        return 0;
    }
    match base_letter(first) {
        Some('A'..='H') => 1,
        Some('I'..='P') => 2,
        Some('Q'..='Z') => 3,
        _ => 4,
    }
}

// ---------- sizes ----------

/// Folders have no size in Explorer's Size column, so they are grouped as "Unspecified".
pub const SIZE_LABELS: [&str; 8] = [
    "Unspecified",
    "Empty (0 KB)",
    "Tiny (0 - 16 KB)",
    "Small (16 KB - 1 MB)",
    "Medium (1 - 128 MB)",
    "Large (128 MB - 1 GB)",
    "Huge (1 - 4 GB)",
    "Gigantic (>4 GB)",
];

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;

/// The size group of a file, each range including its upper edge: 16 KB exactly is tiny.
pub fn size_bucket(size: u64, dir: bool) -> u32 {
    match size {
        _ if dir => 0,
        0 => 1,
        s if s <= 16 * KB => 2,
        s if s <= MB => 3,
        s if s <= 128 * MB => 4,
        s if s <= GB => 5,
        s if s <= 4 * GB => 6,
        _ => 7,
    }
}

// ---------- dates ----------

/// Date groups, oldest first. An item without a date is "Unspecified".
pub const DATE_LABELS: [&str; 15] = [
    "Unspecified",
    "A long time ago",
    "Earlier this year",
    "Last month",
    "Earlier this month",
    "Last week",
    "Earlier this week",
    "Yesterday",
    "Today",
    "Tomorrow",
    "Later this week",
    "Next week",
    "Later this month",
    "Later this year",
    "Sometime in the future",
];

/// Where each date group starts, in local time, for one day: `today`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateEdges {
    /// `(first FILETIME in the group, group ordinal)`, in ascending order of time. A group
    /// that is empty today (Earlier this week, on the first day of a week) starts where the
    /// next one does.
    starts: Vec<(i64, u32)>,
}

impl DateEdges {
    /// The edges for the local day `today` (days since 1970-01-01). `first_weekday` is the
    /// first day of the week, 0 for Monday to 6 for Sunday, as Windows numbers them;
    /// `midnight` turns a local day into the FILETIME at which it begins.
    pub fn new(today: i64, first_weekday: u32, midnight: impl Fn(i64) -> i64) -> Self {
        // 1970-01-01 was a Thursday, which is 3 counting from Monday.
        let weekday = (today + 3).rem_euclid(7);
        let week = today - (weekday - first_weekday as i64).rem_euclid(7);
        let (year, _, _) = civil_from_days(today);

        // Going back, each group starts at its own edge or at the start of the group after it,
        // whichever is earlier, so groups never overlap.
        let past = [
            (today, 8),
            (today - 1, 7),
            (week, 6),
            (week - 7, 5),
            (month_start(today, 0), 4),
            (month_start(today, -1), 3),
            (days_from_civil(year, 1, 1), 2),
        ];
        // Going forward, each group ends at its own edge or at the end of the one before.
        let future = [
            (today + 1, 9),
            (today + 2, 10),
            (week + 7, 11),
            (week + 14, 12),
            (month_start(today, 1), 13),
            (days_from_civil(year + 1, 1, 1), 14),
        ];
        let mut starts = vec![(i64::MIN, 1)];
        let mut floor = i64::MAX;
        for (day, ordinal) in past {
            floor = floor.min(day);
            starts.push((midnight(floor), ordinal));
        }
        let mut ceiling = i64::MIN;
        for (day, ordinal) in future {
            ceiling = ceiling.max(day);
            starts.push((midnight(ceiling), ordinal));
        }
        starts.sort();
        Self { starts }
    }

    /// The edges for today on this machine: its time zone, with daylight saving time applied
    /// to each edge, and the user's first day of the week.
    #[cfg(windows)]
    pub fn local() -> Self {
        let today = crate::time::Clock::local().today();
        Self::new(today, local::first_weekday(), local::midnight)
    }

    /// The date group of a modification time (FILETIME); 0 or less means no date.
    pub fn bucket(&self, mtime: i64) -> u32 {
        if mtime <= 0 {
            return 0;
        }
        let after = self.starts.partition_point(|&(start, _)| start <= mtime);
        self.starts[after.saturating_sub(1)].1
    }
}

#[cfg(windows)]
mod local {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK};
    use windows_sys::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};

    use crate::time::{civil_from_days, unix_to_filetime};

    /// The user's first day of the week, 0 for Monday to 6 for Sunday; Monday if unknown.
    pub fn first_weekday() -> u32 {
        let mut buffer = [0u16; 4];
        // SAFETY: a null locale name means the user's default locale; `buffer` holds as many
        // characters as the call is told it does.
        let written = unsafe { GetLocaleInfoEx(std::ptr::null(), LOCALE_IFIRSTDAYOFWEEK, buffer.as_mut_ptr(), buffer.len() as i32) };
        match (written, buffer[0]) {
            (1.., digit @ 0x30..=0x36) => (digit - 0x30) as u32,
            _ => 0,
        }
    }

    /// The FILETIME at which the local day `day` begins, by the time zone's rules for that
    /// day rather than today's offset.
    pub fn midnight(day: i64) -> i64 {
        let (year, month, date) = civil_from_days(day);
        let local = SYSTEMTIME {
            wYear: year.clamp(1601, 30827) as u16,
            wMonth: month as u16,
            wDayOfWeek: 0,
            wDay: date as u16,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        // SAFETY: all-zero is a valid value for these plain-data structs.
        let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
        let mut filetime: FILETIME = unsafe { std::mem::zeroed() };
        // SAFETY: the pointers are to live locals; a null zone means the current time zone.
        let converted = unsafe {
            TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) != 0
                && SystemTimeToFileTime(&utc, &mut filetime) != 0
        };
        if converted {
            (filetime.dwHighDateTime as i64) << 32 | filetime.dwLowDateTime as i64
        } else {
            unix_to_filetime(day * 86_400)
        }
    }
}

// ---------- grouping rows ----------

/// Rearranges `rows` so each group's rows are together, keeping their order within a group,
/// and returns where each group starts with its ordinal. `ordinals[i]` is row i's group, below
/// `count`; `descending` puts the highest ordinal first.
pub fn arrange<T: Clone>(rows: &mut [T], ordinals: &[u32], count: usize, descending: bool) -> Vec<(u32, u32)> {
    debug_assert_eq!(rows.len(), ordinals.len());
    let mut sizes = vec![0u32; count];
    for &ordinal in ordinals {
        sizes[ordinal as usize] += 1;
    }
    let order: Vec<usize> = if descending { (0..count).rev().collect() } else { (0..count).collect() };
    let mut next = vec![0u32; count];
    let mut runs = Vec::new();
    let mut at = 0u32;
    for &ordinal in &order {
        if sizes[ordinal] > 0 {
            runs.push((at, ordinal as u32));
        }
        next[ordinal] = at;
        at += sizes[ordinal];
    }
    let original = rows.to_vec();
    for (row, &ordinal) in original.into_iter().zip(ordinals) {
        let slot = &mut next[ordinal as usize];
        rows[*slot as usize] = row;
        *slot += 1;
    }
    runs
}

/// What one row is grouped by, for rows that are not in an index (a folder listed from disk).
pub struct Facts<'a> {
    pub name: &'a str,
    /// Lower-case extension, empty for none.
    pub ext: &'a str,
    pub dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// The label of a Type group: the Type column's own words.
pub fn type_label(dir: bool, ext: &str) -> String {
    match (dir, ext) {
        (true, _) => "Folder".to_owned(),
        (false, "") => "File".to_owned(),
        (false, ext) => format!("{} file", ext.to_ascii_uppercase()),
    }
}

/// Groups rows described by `facts`, rearranging `rows` to match.
pub fn group_rows<'f, T: Clone>(rows: &mut [T], facts: impl Fn(&T) -> Facts<'f>, grouping: Grouping, edges: &DateEdges) -> Vec<Group> {
    let (ordinals, labels): (Vec<u32>, Vec<String>) = match grouping.by {
        GroupBy::Name => (rows.iter().map(|row| name_bucket(facts(row).name)).collect(), owned(&NAME_LABELS)),
        GroupBy::Size => (
            rows.iter()
                .map(|row| {
                    let f = facts(row);
                    size_bucket(f.size, f.dir)
                })
                .collect(),
            owned(&SIZE_LABELS),
        ),
        GroupBy::Modified => (rows.iter().map(|row| edges.bucket(facts(row).mtime)).collect(), owned(&DATE_LABELS)),
        GroupBy::Type => {
            // Folders, then files without an extension, then extensions in alphabetical order.
            let mut exts: Vec<String> = rows.iter().map(&facts).filter(|f| !f.dir && !f.ext.is_empty()).map(|f| f.ext.to_owned()).collect();
            exts.sort_unstable();
            exts.dedup();
            let ordinal = |f: Facts<'_>| match (f.dir, f.ext) {
                (true, _) => 0,
                (false, "") => 1,
                (false, ext) => 2 + exts.binary_search_by(|probe| probe.as_str().cmp(ext)).unwrap_or(0) as u32,
            };
            let ordinals = rows.iter().map(|row| ordinal(facts(row))).collect();
            let labels = [type_label(true, ""), type_label(false, "")].into_iter().chain(exts.iter().map(|ext| type_label(false, ext))).collect();
            (ordinals, labels)
        }
    };
    let runs = arrange(rows, &ordinals, labels.len(), grouping.descending);
    runs.into_iter().map(|(start, ordinal)| Group { start, label: labels[ordinal as usize].clone() }).collect()
}

/// Groups entries of an index, rearranging `ids` to match. Each entry's group is worked out in
/// parallel: the lookups are scattered across the index, so they dominate the time.
pub fn group(index: &Index, ids: &mut [EntryId], grouping: Grouping, edges: &DateEdges) -> Vec<Group> {
    let dir = |id: EntryId| index.attrs(id) & attr::DIRECTORY != 0;
    let each = |ordinal: &(dyn Fn(EntryId) -> u32 + Sync)| -> Vec<u32> { ids.par_iter().map(|&id| ordinal(id)).collect() };
    let (ordinals, labels): (Vec<u32>, Vec<String>) = match grouping.by {
        // Folded names begin with the same letter as the names shown, in lower case.
        GroupBy::Name => (
            each(&|id| name_bucket(std::str::from_utf8(index.lower_bytes(id)).unwrap_or(""))),
            owned(&NAME_LABELS),
        ),
        GroupBy::Size => (each(&|id| size_bucket(index.size(id), dir(id))), owned(&SIZE_LABELS)),
        GroupBy::Modified => (each(&|id| edges.bucket(index.mtime(id))), owned(&DATE_LABELS)),
        GroupBy::Type => {
            let rank = ext_ranks(index);
            let ordinals = each(&|id| match (dir(id), index.ext(id)) {
                (true, _) => 0,
                (false, 0) => 1,
                (false, ext) => 2 + rank[ext as usize] as u32,
            });
            let mut labels = vec![String::new(); 2 + rank.len()];
            labels[0] = type_label(true, "");
            labels[1] = type_label(false, "");
            for (ext, &position) in rank.iter().enumerate() {
                labels[2 + position as usize] = type_label(false, index.ext_name(ext as u16));
            }
            (ordinals, labels)
        }
    };
    let runs = arrange(ids, &ordinals, labels.len(), grouping.descending);
    runs.into_iter().map(|(start, ordinal)| Group { start, label: labels[ordinal as usize].clone() }).collect()
}

fn owned(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|&label| label.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::unix_to_filetime;
    use kestrel_index::{IndexBuilder, Meta};

    const HOUR: i64 = 3600;
    const SECOND: i64 = 10_000_000;

    /// Local time two hours ahead of UTC, all year.
    const OFFSET: i64 = 2 * HOUR;
    fn midnight(day: i64) -> i64 {
        unix_to_filetime(day * 86_400 - OFFSET)
    }
    /// A local date and time as a FILETIME.
    fn at(year: i64, month: u32, day: u32, hour: i64, minute: i64, second: i64) -> i64 {
        midnight(days_from_civil(year, month, day)) + (hour * HOUR + minute * 60 + second) * SECOND
    }
    fn label(edges: &DateEdges, mtime: i64) -> &'static str {
        DATE_LABELS[edges.bucket(mtime) as usize]
    }

    #[test]
    fn date_groups_change_at_local_midnight() {
        // Wednesday 14 October 2026; weeks start on Monday.
        let edges = DateEdges::new(days_from_civil(2026, 10, 14), 0, midnight);
        assert_eq!(label(&edges, at(2026, 10, 14, 0, 0, 0)), "Today");
        assert_eq!(label(&edges, at(2026, 10, 14, 23, 59, 59)), "Today");
        assert_eq!(label(&edges, at(2026, 10, 13, 23, 59, 59)), "Yesterday");
        assert_eq!(label(&edges, at(2026, 10, 13, 0, 0, 0)), "Yesterday");
        assert_eq!(label(&edges, at(2026, 10, 12, 23, 59, 59)), "Earlier this week");
        assert_eq!(label(&edges, at(2026, 10, 12, 0, 0, 0)), "Earlier this week");
        assert_eq!(label(&edges, at(2026, 10, 11, 23, 59, 59)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 5, 0, 0, 0)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 4, 23, 59, 59)), "Earlier this month");
        assert_eq!(label(&edges, at(2026, 10, 1, 0, 0, 0)), "Earlier this month");
        assert_eq!(label(&edges, at(2026, 9, 30, 23, 59, 59)), "Last month");
        assert_eq!(label(&edges, at(2026, 9, 1, 0, 0, 0)), "Last month");
        assert_eq!(label(&edges, at(2026, 8, 31, 23, 59, 59)), "Earlier this year");
        assert_eq!(label(&edges, at(2026, 1, 1, 0, 0, 0)), "Earlier this year");
        assert_eq!(label(&edges, at(2025, 12, 31, 23, 59, 59)), "A long time ago");
        assert_eq!(label(&edges, 1), "A long time ago");
        assert_eq!(label(&edges, 0), "Unspecified");
        // The day starts at local midnight, which is 22:00 the day before in UTC.
        let utc_midnight = unix_to_filetime(days_from_civil(2026, 10, 14) * 86_400);
        assert_eq!(label(&edges, utc_midnight), "Today", "02:00 local");
        assert_eq!(label(&edges, utc_midnight - 2 * HOUR * SECOND - SECOND), "Yesterday", "23:59:59 local the day before");
    }

    #[test]
    fn future_dates_have_groups_of_their_own() {
        let edges = DateEdges::new(days_from_civil(2026, 10, 14), 0, midnight);
        assert_eq!(label(&edges, at(2026, 10, 15, 0, 0, 0)), "Tomorrow");
        assert_eq!(label(&edges, at(2026, 10, 16, 0, 0, 0)), "Later this week");
        assert_eq!(label(&edges, at(2026, 10, 18, 23, 59, 59)), "Later this week");
        assert_eq!(label(&edges, at(2026, 10, 19, 0, 0, 0)), "Next week");
        assert_eq!(label(&edges, at(2026, 10, 26, 0, 0, 0)), "Later this month");
        assert_eq!(label(&edges, at(2026, 11, 1, 0, 0, 0)), "Later this year");
        assert_eq!(label(&edges, at(2027, 1, 1, 0, 0, 0)), "Sometime in the future");
    }

    #[test]
    fn the_week_starts_on_the_users_first_day() {
        // Wednesday 14 October 2026 again, with weeks starting on Sunday: this week began on
        // the 11th, which is "Last week" when weeks start on Monday.
        let edges = DateEdges::new(days_from_civil(2026, 10, 14), 6, midnight);
        assert_eq!(label(&edges, at(2026, 10, 11, 0, 0, 0)), "Earlier this week");
        assert_eq!(label(&edges, at(2026, 10, 10, 23, 59, 59)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 4, 0, 0, 0)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 3, 23, 59, 59)), "Earlier this month");
    }

    #[test]
    fn groups_never_overlap_at_the_start_of_a_week_or_month() {
        // Monday 2 November 2026, weeks starting Monday: yesterday was last month, this week
        // has no earlier days, and last week began in October.
        let edges = DateEdges::new(days_from_civil(2026, 11, 2), 0, midnight);
        assert_eq!(label(&edges, at(2026, 11, 2, 8, 0, 0)), "Today");
        assert_eq!(label(&edges, at(2026, 11, 1, 12, 0, 0)), "Yesterday");
        assert_eq!(label(&edges, at(2026, 10, 31, 12, 0, 0)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 26, 0, 0, 0)), "Last week");
        assert_eq!(label(&edges, at(2026, 10, 25, 23, 59, 59)), "Last month");
        assert_eq!(label(&edges, at(2026, 10, 1, 0, 0, 0)), "Last month");
        assert_eq!(label(&edges, at(2026, 9, 30, 23, 0, 0)), "Earlier this year");
        let used: Vec<u32> = (0..60).map(|day| edges.bucket(midnight(days_from_civil(2026, 9, 10) + day) + 1)).collect();
        assert!(!used.contains(&6) && !used.contains(&4), "Earlier this week and Earlier this month are empty");
        assert!(used.windows(2).all(|pair| pair[0] <= pair[1]), "groups follow time: {used:?}");

        // New Year's Day: last month and yesterday are last year, and "Earlier this year" is empty.
        let edges = DateEdges::new(days_from_civil(2027, 1, 1), 0, midnight);
        assert_eq!(label(&edges, at(2026, 12, 31, 23, 59, 59)), "Yesterday");
        assert_eq!(label(&edges, at(2026, 12, 15, 0, 0, 0)), "Last month");
        assert_eq!(label(&edges, at(2026, 11, 30, 0, 0, 0)), "A long time ago");
    }

    #[test]
    fn size_groups_include_their_upper_edge() {
        let label = |size, dir| SIZE_LABELS[size_bucket(size, dir) as usize];
        assert_eq!(label(0, false), "Empty (0 KB)");
        assert_eq!(label(1, false), "Tiny (0 - 16 KB)");
        assert_eq!(label(16 * KB, false), "Tiny (0 - 16 KB)");
        assert_eq!(label(16 * KB + 1, false), "Small (16 KB - 1 MB)");
        assert_eq!(label(MB, false), "Small (16 KB - 1 MB)");
        assert_eq!(label(MB + 1, false), "Medium (1 - 128 MB)");
        assert_eq!(label(128 * MB, false), "Medium (1 - 128 MB)");
        assert_eq!(label(128 * MB + 1, false), "Large (128 MB - 1 GB)");
        assert_eq!(label(GB, false), "Large (128 MB - 1 GB)");
        assert_eq!(label(GB + 1, false), "Huge (1 - 4 GB)");
        assert_eq!(label(4 * GB, false), "Huge (1 - 4 GB)");
        assert_eq!(label(4 * GB + 1, false), "Gigantic (>4 GB)");
        assert_eq!(label(123 * GB, true), "Unspecified", "folders are not grouped by size");
    }

    #[test]
    fn names_group_by_first_letter_accents_included() {
        let label = |name| NAME_LABELS[name_bucket(name) as usize];
        assert_eq!(label("2026 taxes"), "0 - 9");
        assert_eq!(label("apple"), "A - H");
        assert_eq!(label("Hotel"), "A - H");
        assert_eq!(label("éclair"), "A - H");
        assert_eq!(label("Île"), "I - P");
        assert_eq!(label("pear"), "I - P");
        assert_eq!(label("quince"), "Q - Z");
        assert_eq!(label("Ångström"), "A - H");
        assert_eq!(label("Ž"), "Q - Z");
        assert_eq!(label("_build"), "Other");
        assert_eq!(label("日本"), "Other");
        assert_eq!(label(""), "Other");
    }

    #[test]
    fn attribute_letters_follow_explorer() {
        assert_eq!(attribute_letters(0x20), "A");
        assert_eq!(attribute_letters(0x27), "RHSA");
        assert_eq!(attribute_letters(0x16), "HSD");
        assert_eq!(attribute_letters(0x40_0000 | 0x4_0000 | 0x20), "AM");
        assert!(attribute_key(0) < attribute_key(0x20));
        assert!(attribute_key(0x20) < attribute_key(0x26), "A < HSA");
        assert!(attribute_key(0x26) < attribute_key(0x21), "HSA < RA");
    }

    #[test]
    fn arranging_is_stable_and_reports_boundaries() {
        let mut rows = ['a', 'b', 'c', 'd', 'e', 'f'];
        let ordinals = [2, 0, 2, 1, 0, 2];
        let runs = arrange(&mut rows, &ordinals, 4, false);
        assert_eq!(rows, ['b', 'e', 'd', 'a', 'c', 'f']);
        assert_eq!(runs, [(0, 0), (2, 1), (3, 2)]);
        let mut rows = ['a', 'b', 'c', 'd', 'e', 'f'];
        let runs = arrange(&mut rows, &ordinals, 4, true);
        assert_eq!(rows, ['a', 'c', 'f', 'd', 'b', 'e']);
        assert_eq!(runs, [(0, 2), (3, 1), (4, 0)]);
    }

    #[test]
    fn index_entries_group_with_their_sort_kept() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let file = |size| Meta { size, ..Meta::default() };
        b.push(root, "zeta.txt", file(2 * MB));
        b.push(root, "alpha.TXT", file(10));
        b.push(root, "Music", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        b.push(root, "beta.rs", file(0));
        b.push(root, "readme", file(20 * KB));
        let index = b.finish();
        let mut ids: Vec<EntryId> = index.children(index.roots()[0]).collect();
        let by_name = crate::Sort { key: crate::SortKey::Name, descending: false, folders_first: true };
        crate::sort(&index, &mut ids, by_name);
        let edges = DateEdges::new(days_from_civil(2026, 10, 14), 0, midnight);
        let names = |ids: &[EntryId]| ids.iter().map(|&id| index.name(id).to_owned()).collect::<Vec<_>>();

        let groups = group(&index, &mut ids, Grouping { by: GroupBy::Type, descending: false }, &edges);
        assert_eq!(names(&ids), ["Music", "readme", "beta.rs", "alpha.TXT", "zeta.txt"]);
        let labels: Vec<(u32, &str)> = groups.iter().map(|g| (g.start, g.label.as_str())).collect();
        assert_eq!(labels, [(0, "Folder"), (1, "File"), (2, "RS file"), (3, "TXT file")]);

        crate::sort(&index, &mut ids, by_name);
        let groups = group(&index, &mut ids, Grouping { by: GroupBy::Size, descending: true }, &edges);
        assert_eq!(names(&ids), ["zeta.txt", "readme", "alpha.TXT", "beta.rs", "Music"]);
        let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Medium (1 - 128 MB)", "Small (16 KB - 1 MB)", "Tiny (0 - 16 KB)", "Empty (0 KB)", "Unspecified"]);

        crate::sort(&index, &mut ids, by_name);
        let groups = group(&index, &mut ids, Grouping { by: GroupBy::Name, descending: false }, &edges);
        assert_eq!(names(&ids), ["alpha.TXT", "beta.rs", "Music", "readme", "zeta.txt"]);
        assert_eq!(groups, [
            Group { start: 0, label: "A - H".into() },
            Group { start: 2, label: "I - P".into() },
            Group { start: 3, label: "Q - Z".into() },
        ]);
    }

    #[test]
    fn rows_outside_the_index_group_the_same_way() {
        let mut rows = [("b.txt", "txt", false, 3u64), ("Docs", "", true, 0), ("a.TXT", "txt", false, 0), ("make", "", false, 5)];
        let edges = DateEdges::new(0, 0, midnight);
        let groups = group_rows(
            &mut rows,
            |row| Facts { name: row.0, ext: row.1, dir: row.2, size: row.3, mtime: 0 },
            Grouping { by: GroupBy::Type, descending: false },
            &edges,
        );
        assert_eq!(rows.map(|row| row.0), ["Docs", "make", "b.txt", "a.TXT"]);
        assert_eq!(groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(), ["Folder", "File", "TXT file"]);
    }

    #[cfg(windows)]
    #[test]
    fn local_edges_put_now_in_today() {
        let edges = DateEdges::local();
        assert_eq!(DATE_LABELS[edges.bucket(crate::time::unix_to_filetime(crate::time::Clock::local().now_unix)) as usize], "Today");
        assert!(local::first_weekday() <= 6);
    }
}
