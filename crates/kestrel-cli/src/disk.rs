//! The disk usage analysis from the command line: how long it takes over a real index, and what
//! it says about a folder, to compare with what other tools say.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kestrel_analyze::{growth, overview, treemap};
use kestrel_index::{EntryId, Index};

use super::human;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Runs `work` `runs` times; the last result, and the fastest and median times.
fn time<T>(runs: usize, mut work: impl FnMut() -> T) -> (T, Duration, Duration) {
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let started = Instant::now();
        last = Some(work());
        times.push(started.elapsed());
    }
    times.sort();
    (last.expect("at least one run"), times[0], times[times.len() / 2])
}

fn find(index: &Index, path: &str) -> Result<EntryId> {
    index.lookup(path).with_context(|| format!("{path} is not in the index"))
}

pub fn bench(index: &Index, roots: &[String], runs: usize) -> Result<()> {
    let targets: Vec<EntryId> = if roots.is_empty() {
        index.roots().to_vec()
    } else {
        roots.iter().map(|path| find(index, path)).collect::<Result<_>>()?
    };
    println!("{} entries in the index, {} runs each", index.live_count(), runs);
    for root in targets {
        println!("\n{}  {} on disk, {} logical", index.path(root), human(index.alloc(root)), human(index.size(root)));
        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (2560.0, 1440.0)] {
            let options = treemap::Options { width, height, ..treemap::Options::default() };
            let (map, min, median) = time(runs, || treemap::layout(index, root, &options));
            let opened = map.tiles.iter().filter(|tile| tile.open).count();
            let deepest = map.tiles.iter().map(|tile| tile.depth).max().unwrap_or(0);
            println!(
                "  treemap {width:>4}x{height:<4}  {:>6} tiles, {:>5} folders opened, {:>2} levels{}   min {min:>9.2?}  median {median:>9.2?}",
                map.tiles.len(),
                opened,
                deepest + 1,
                if map.truncated { " (tile budget reached)" } else { "" },
            );
        }
        let options = overview::Options { now_ms: now_ms(), top: 50 };
        let (found, min, median) = time(runs, || overview::overview(index, root, &options));
        println!(
            "  overview (types, ages, largest, recent)  {} files, {} folders   min {min:>9.2?}  median {median:>9.2?}",
            found.totals.local.files, found.totals.folders
        );
    }
    let (rows, min, median) = time(runs, || growth::sample(index, index.roots(), &growth::SampleOptions::default()));
    println!(
        "\ngrowth sample of every root  {} folders, {} files   min {min:>9.2?}  median {median:>9.2?}",
        rows.iter().filter(|row| !row.file).count(),
        rows.iter().filter(|row| row.file).count()
    );
    Ok(())
}

pub fn usage(index: &Index, path: &str) -> Result<()> {
    let root = find(index, path)?;
    let started = Instant::now();
    let found = overview::overview(index, root, &overview::Options { now_ms: now_ms(), top: 10 });
    let took = started.elapsed();
    let totals = &found.totals;
    println!("{}", index.path(root));
    println!("  on disk     {:>12}  ({} bytes)", human(totals.local.bytes), totals.local.bytes);
    println!("  logical     {:>12}  ({} bytes)", human(totals.local.logical), totals.local.logical);
    println!("  files       {:>12}", totals.local.files);
    println!("  folders     {:>12}", totals.folders);
    println!("  in the cloud {:>11}  in {} files", human(totals.cloud_bytes), totals.cloud_files);
    println!("\n  by type");
    for row in found.categories.iter().filter(|row| row.amount.files > 0 || row.cloud_files > 0) {
        println!("    {:<34} {:>10} on disk {:>10} logical {:>9} files", row.label, human(row.amount.bytes), human(row.amount.logical), row.amount.files);
    }
    println!("\n  by age");
    for row in &found.ages {
        println!("    {:<34} {:>10} on disk {:>9} files", row.label, human(row.amount.bytes), row.amount.files);
    }
    println!("\n  largest files");
    for item in &found.largest_files {
        println!("    {:>10}  {}", human(item.bytes), item.path);
    }
    println!("\n  largest folders");
    for item in &found.largest_folders {
        println!("    {:>10}  {}", human(item.bytes), item.path);
    }
    println!("\nanalysed in {took:.2?}");

    // Files with several names: how much allocation the index charges more than once.
    let mut charged: std::collections::HashMap<u64, (u32, u64)> = std::collections::HashMap::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for child in index.children(dir) {
            if index.is_dir(child) {
                stack.push(child);
            } else if index.file_id(child) != 0 && index.alloc(child) > 0 {
                let entry = charged.entry(index.file_id(child)).or_default();
                entry.0 += 1;
                entry.1 = entry.1.max(index.alloc(child));
            }
        }
    }
    let (files, twice): (u64, u64) = charged
        .values()
        .filter(|(names, _)| *names > 1)
        .fold((0, 0), |(files, bytes), (names, alloc)| (files + 1, bytes + (*names as u64 - 1) * alloc));
    println!("files charged under more than one name: {files}, {} counted more than once", human(twice));
    Ok(())
}
