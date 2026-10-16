mod disk;
mod live;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use kestrel_index::live::{Outcome, Tracker};
use kestrel_index::{EntryId, Index, IndexBuilder, broker, mft, snapshot, walk};
use parking_lot::RwLock;
use kestrel_query::{Clock, Sort, SortKey, compile, parse, search, sort};

#[derive(Parser)]
#[command(name = "kestrel-cli", about = "Kestrel index and search from the command line")]
struct Cli {
    /// Index snapshot to read or write. Defaults to %LOCALAPPDATA%\Kestrel\index.kidx.
    #[arg(long, short, global = true)]
    snapshot: Option<PathBuf>,
    /// Ask the running Kestrel (current within a second, nothing to load) for search, ls, size
    /// and usage; the saved index is read only when Kestrel isn't running.
    #[arg(long, global = true)]
    live: bool,
    /// The running Kestrel's query pipe, when it isn't the one of your own Kestrel (also
    /// KESTREL_PIPE).
    #[arg(long, global = true)]
    pipe: Option<String>,
    /// Print what the running Kestrel answers as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a folder or drive and save the index.
    Index {
        /// Root to crawl, for example C:\
        root: String,
    },
    /// Run a query against the saved index.
    Search {
        /// Query text; see the kestrel-query documentation for the syntax.
        query: Vec<String>,
        #[arg(long, value_enum, default_value = "name")]
        sort: SortArg,
        #[arg(long)]
        desc: bool,
        /// Number of results to print.
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
    },
    /// List a folder from the saved index, with folder sizes.
    Ls { path: String },
    /// Apply the changes recorded in each drive's change journal since the index was saved.
    CatchUp,
    /// Ask the installed broker service to build an index of these roots.
    Remote {
        roots: Vec<String>,
        /// Save the result as the index.
        #[arg(long)]
        save: bool,
    },
    /// Build the index from a drive's master file table. Reading a drive needs administrator
    /// rights; reading a dump does not.
    Mft {
        /// Drive root, for example C:\
        root: String,
        /// Compare with the saved index built by crawling, matching entries by file id.
        #[arg(long)]
        compare: bool,
        /// Also copy the raw MFT to this file, so the parsing can be examined without elevation.
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Read the MFT from a dump instead of the drive.
        #[arg(long)]
        from_dump: Option<PathBuf>,
        /// Save the result as the index, replacing the saved one.
        #[arg(long)]
        save: bool,
    },
    /// Time index loading and a set of representative queries.
    Bench {
        /// Runs per query.
        #[arg(long, default_value_t = 25)]
        runs: usize,
    },
    /// Time the disk usage analysis over the saved index: treemap layouts, the overview's
    /// breakdowns and lists, and a growth sample.
    DiskBench {
        /// Folders to analyse; every root of the index when none are given.
        roots: Vec<String>,
        /// Runs per measurement.
        #[arg(long, default_value_t = 10)]
        runs: usize,
    },
    /// What the disk usage hub would show for a folder: its totals, types and ages.
    Usage { path: String },
    /// A folder's size and how many files and folders it holds.
    Size { path: String },
    /// The running Kestrel's projects (needs Kestrel running).
    Projects {
        /// Only projects whose name, folder, stack, tags, status or branch contain this.
        filter: Option<String>,
        #[arg(long, short = 'n', default_value_t = 100)]
        limit: usize,
    },
    /// One project's details: git, scripts, recent files (needs Kestrel running).
    Project {
        /// The project's folder or name.
        project: String,
    },
    /// Files opened recently (needs Kestrel running).
    Recent {
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
    },
    /// Whether Kestrel is running and answering, and its index's state.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    Name,
    Size,
    Modified,
    Type,
}

impl From<SortArg> for SortKey {
    fn from(arg: SortArg) -> Self {
        match arg {
            SortArg::Name => SortKey::Name,
            SortArg::Size => SortKey::Size,
            SortArg::Modified => SortKey::Modified,
            SortArg::Type => SortKey::Type,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let snapshot_path = match cli.snapshot {
        Some(path) => path,
        None => {
            let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
            PathBuf::from(base).join("Kestrel").join("index.kidx")
        }
    };
    let live = live::Live { pipe: kestrel_ipc::name::resolve(cli.pipe.as_deref())?, json: cli.json };
    /// Says the saved index is read instead, once Kestrel turned out not to be running.
    fn falling_back() {
        eprintln!("Kestrel isn't running; reading the saved index instead.");
    }

    match cli.command {
        Command::Search { query, sort: key, desc, limit } if cli.live => {
            let sort = match key {
                SortArg::Name => kestrel_ipc::protocol::SortBy::Name,
                SortArg::Size => kestrel_ipc::protocol::SortBy::Size,
                SortArg::Modified => kestrel_ipc::protocol::SortBy::Modified,
                SortArg::Type => kestrel_ipc::protocol::SortBy::Type,
            };
            if !live.search(query.join(" "), sort, desc, limit)? {
                falling_back();
                search_snapshot(&snapshot_path, &query, key, desc, limit)?;
            }
        }
        Command::Ls { path } if cli.live => {
            if !live.ls(path.clone())? {
                falling_back();
                ls_snapshot(&snapshot_path, &path)?;
            }
        }
        Command::Usage { path } if cli.live => {
            if !live.usage(path.clone())? {
                falling_back();
                disk::usage(&load(&snapshot_path)?, &path)?;
            }
        }
        Command::Size { path } => {
            if !(cli.live && live.size(path.clone())?) {
                if cli.live {
                    falling_back();
                }
                size_snapshot(&snapshot_path, &path)?;
            }
        }
        Command::Projects { filter, limit } => live.projects(filter, limit)?,
        Command::Project { project } => live.project(project)?,
        Command::Recent { limit } => live.recent(limit)?,
        Command::Status => live.status()?,
        Command::Search { query, sort: key, desc, limit } => search_snapshot(&snapshot_path, &query, key, desc, limit)?,
        Command::Ls { path } => ls_snapshot(&snapshot_path, &path)?,
        Command::Index { root } => {
            let started = Instant::now();
            // The journal position is taken before the crawl so that changes made during it
            // are replayed afterwards.
            let cursor = Tracker::open(&root).and_then(|tracker| tracker.position());
            let mut builder = IndexBuilder::new();
            let stats = walk::walk(&root, &mut builder).with_context(|| format!("crawling {root}"))?;
            let crawled = started.elapsed();
            let mut index = builder.finish();
            match cursor {
                Ok(cursor) => index.set_journal(vec![cursor]),
                Err(error) => println!("no change journal for {root} ({error}); changes will need a rescan"),
            }
            let built = started.elapsed();
            if let Some(dir) = snapshot_path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            snapshot::save(&index, &snapshot_path)?;
            let saved = started.elapsed();

            println!("{} files, {} folders, {} unreadable folders", stats.files, stats.dirs, stats.unreadable);
            println!("crawl  {:>8.2?}", crawled);
            println!("build  {:>8.2?}", built - crawled);
            println!("save   {:>8.2?}", saved - built);
            println!("total  {:>8.2?}", saved);
            println!("memory {}", human(index.memory_bytes() as u64));
            let root_id = index.roots()[0];
            println!("size   {} logical, {} on disk", human(index.size(root_id)), human(index.alloc(root_id)));
            println!("saved  {}", snapshot_path.display());
        }
        Command::CatchUp => {
            let started = Instant::now();
            let index = RwLock::new(load(&snapshot_path)?);
            let loaded = started.elapsed();
            let roots: Vec<String> = index.read().journal().iter().map(|c| c.root.clone()).collect();
            if roots.is_empty() {
                bail!("the index has no journal position; run `kestrel-cli index` again");
            }
            for root in roots {
                let began = Instant::now();
                match Tracker::open(&root)?.catch_up(&index)? {
                    Outcome::Current { changes, folders, stats } => println!(
                        "{root}  {changes} journal records, {folders} folders re-read, {} added, {} moved, {} removed, {} updated in {:.2?}",
                        stats.inserted, stats.moved, stats.removed, stats.updated, began.elapsed()
                    ),
                    Outcome::Lost(reason) => println!("{root}  needs a rescan: {reason}"),
                }
            }
            let caught_up = started.elapsed();
            snapshot::save(&index.read(), &snapshot_path)?;
            println!("load {:.2?}, catch up {:.2?}, save {:.2?}", loaded, caught_up - loaded, started.elapsed() - caught_up);
        }
        Command::Mft { root, compare, dump, from_dump, save } => {
            let started = Instant::now();
            let cursor = Tracker::open(&root).and_then(|tracker| tracker.position()).ok();
            let mut builder = IndexBuilder::new();
            let stats = match &from_dump {
                Some(path) => mft::scan_dump(path, &root, &mut builder),
                None => mft::scan_volume(&root, &mut builder),
            }
            .with_context(|| format!("reading the MFT of {root}"))?;
            let scanned = started.elapsed();
            let mut index = builder.finish();
            let built = started.elapsed();
            if let Some(cursor) = cursor {
                index.set_journal(vec![cursor]);
            }
            let root_id = index.roots()[0];
            println!(
                "{} records read, {} in use, {} torn; {} files and {} folders",
                stats.scan.records, stats.scan.parsed, stats.scan.torn, stats.files, stats.dirs
            );
            println!("{} entries, {} in memory", index.live_count(), human(index.memory_bytes() as u64));
            println!("size   {} logical, {} on disk", human(index.size(root_id)), human(index.alloc(root_id)));
            println!("read and parse {:>8.2?}", scanned);
            println!("build          {:>8.2?}", built - scanned);
            println!("total          {:>8.2?}", built);

            if let Some(path) = dump {
                let began = Instant::now();
                mft::dump_volume(&root, &path).with_context(|| format!("dumping the MFT to {}", path.display()))?;
                println!("dumped to {} in {:.2?}", path.display(), began.elapsed());
            }
            if compare {
                compare_indexes(&load(&snapshot_path)?, &index);
            }
            if save {
                snapshot::save(&index, &snapshot_path)?;
                println!("saved  {}", snapshot_path.display());
            }
        }
        Command::Remote { roots, save } => {
            let started = Instant::now();
            let Some(index) = broker::scan(&roots)? else { bail!("the broker is not installed or not running") };
            let root_id = index.roots()[0];
            println!("{} entries from the broker in {:.2?}", index.live_count(), started.elapsed());
            println!("size   {} logical, {} on disk", human(index.size(root_id)), human(index.alloc(root_id)));
            println!("journal positions: {:?}", index.journal());
            if save {
                snapshot::save(&index, &snapshot_path)?;
                println!("saved  {}", snapshot_path.display());
            }
        }
        Command::Bench { runs } => bench(&snapshot_path, runs.max(1))?,
        Command::DiskBench { roots, runs } => disk::bench(&load(&snapshot_path)?, &roots, runs.max(1))?,
        Command::Usage { path } => disk::usage(&load(&snapshot_path)?, &path)?,
    }
    Ok(())
}

fn load(path: &PathBuf) -> Result<Index> {
    snapshot::load(path)
        .with_context(|| format!("loading {} (run `kestrel-cli index C:\\` first)", path.display()))
}

fn search_snapshot(snapshot_path: &PathBuf, query: &[String], key: SortArg, desc: bool, limit: usize) -> Result<()> {
    let index = load(snapshot_path)?;
    let text = query.join(" ");
    let started = Instant::now();
    let compiled = compile(&parse(&text, Clock::local())?, &index)?;
    let mut ids = search(&index, &compiled, &AtomicBool::new(false)).unwrap_or_default();
    let searched = started.elapsed();
    sort(&index, &mut ids, Sort { key: key.into(), descending: desc, folders_first: false });
    let sorted = started.elapsed();
    for &id in ids.iter().take(limit) {
        println!("{:>10}  {}", human(index.size(id)), index.path(id));
    }
    println!("{} results in {:.2?} (+{:.2?} to sort)", ids.len(), searched, sorted - searched);
    Ok(())
}

fn ls_snapshot(snapshot_path: &PathBuf, path: &str) -> Result<()> {
    let index = load(snapshot_path)?;
    let Some(dir) = index.lookup(path) else { bail!("{path} is not in the index") };
    let mut ids: Vec<EntryId> = index.children(dir).collect();
    sort(&index, &mut ids, Sort { key: SortKey::Size, descending: true, folders_first: false });
    for &id in &ids {
        let marker = if index.is_dir(id) { "\\" } else { "" };
        println!("{:>10}  {}{marker}", human(index.size(id)), index.name(id));
    }
    println!("{} items, {} total", ids.len(), human(index.size(dir)));
    Ok(())
}

fn size_snapshot(snapshot_path: &PathBuf, path: &str) -> Result<()> {
    let index = load(snapshot_path)?;
    let Some(item) = index.lookup(path) else { bail!("{path} is not in the index") };
    let (mut files, mut folders) = (0u64, 0u64);
    let mut stack = vec![item];
    while let Some(dir) = stack.pop() {
        for child in index.children(dir) {
            if index.is_dir(child) {
                folders += 1;
                stack.push(child);
            } else {
                files += 1;
            }
        }
    }
    if !index.is_dir(item) {
        files = 1;
    }
    live::print_size(&index.path(item), index.size(item), index.alloc(item), files, folders);
    Ok(())
}

fn bench(snapshot_path: &PathBuf, runs: usize) -> Result<()> {
    let started = Instant::now();
    let index = load(snapshot_path)?;
    println!(
        "loaded {} entries in {:.2?} ({} in memory)\n",
        index.live_count(),
        started.elapsed(),
        human(index.memory_bytes() as u64)
    );

    let root = index.path(index.roots()[0]);
    let scoped = format!("in:{root}Users main");
    let queries = [
        "e",
        "main",
        "readme",
        "node_modules",
        "kestrel index snapshot",
        "*.dll",
        "img_????.jpg",
        "ext:rs",
        "type:image",
        "type:video size:>100mb",
        "size:>1gb",
        "dm:7d type:code",
        r"src\main",
        r"regex:^img_\d+\.jpe?g$",
        scoped.as_str(),
    ];
    println!("{:<28} {:>9} {:>10} {:>10} {:>10}", "query", "results", "min", "median", "p95");
    let clock = Clock::local();
    let never = AtomicBool::new(false);
    for text in queries {
        let compiled = compile(&parse(text, clock)?, &index)?;
        let mut times = Vec::with_capacity(runs);
        let mut count = 0;
        for _ in 0..runs {
            let started = Instant::now();
            count = search(&index, &compiled, &never).map_or(0, |ids| ids.len());
            times.push(started.elapsed());
        }
        times.sort();
        println!(
            "{:<28} {:>9} {:>10.2?} {:>10.2?} {:>10.2?}",
            text,
            count,
            times[0],
            times[times.len() / 2],
            times[(times.len() * 95 / 100).min(times.len() - 1)]
        );
    }

    // Sorting is measured on the largest result set a user could plausibly ask for.
    let everything = search(&index, &compile(&parse("", clock)?, &index)?, &never).unwrap_or_default();
    println!("\nsorting all {} entries", everything.len());
    for key in [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Type] {
        let mut ids = everything.clone();
        let started = Instant::now();
        sort(&index, &mut ids, Sort { key, descending: false, folders_first: false });
        println!("  by {:<9} {:>10.2?}", format!("{key:?}"), started.elapsed());
    }

    let listing = time_listing(&index);
    println!("\nlargest folder listing: {} items in {:.2?}", listing.0, listing.1);
    Ok(())
}

/// Compares a crawled index with one built from the MFT, matching entries by file id, and
/// prints what differs.
fn compare_indexes(crawled: &Index, from_mft: &Index) {
    const EXAMPLES: usize = 12;
    let (mut matched, mut no_id, mut mtime) = (0u64, 0u64, 0u64);
    let (mut missing, mut size, mut moved): (Vec<String>, Vec<String>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new());
    let mut missing_count = 0u64;
    let mut size_count = 0u64;
    let mut moved_count = 0u64;
    for id in 0..crawled.len() {
        if crawled.is_deleted(id) {
            continue;
        }
        let file_id = crawled.file_id(id);
        if file_id == 0 {
            no_id += 1;
            continue;
        }
        let candidates = from_mft.entries_with_file_id(file_id);
        // Hard links share a file id and often a name too (one DLL in both System32 and
        // WinSxS), so prefer the link at the same path, then any link with the same name.
        let same_name: Vec<EntryId> =
            candidates.iter().copied().filter(|&c| from_mft.lower_bytes(c) == crawled.lower_bytes(id)).collect();
        let crawled_path = crawled.path(id);
        let same_path = same_name.iter().copied().find(|&c| from_mft.path(c).eq_ignore_ascii_case(&crawled_path));
        let Some(other) = same_path.or(same_name.first().copied()) else {
            missing_count += 1;
            if missing.len() < EXAMPLES {
                missing.push(crawled.path(id));
            }
            continue;
        };
        matched += 1;
        if !crawled.is_dir(id) && crawled.size(id) != from_mft.size(other) {
            size_count += 1;
            if size.len() < EXAMPLES {
                size.push(format!("{}  crawl {} / mft {}", crawled.path(id), crawled.size(id), from_mft.size(other)));
            }
        }
        if !crawled.is_dir(id) && crawled.mtime(id) != from_mft.mtime(other) {
            mtime += 1;
        }
        if !crawled.path(id).eq_ignore_ascii_case(&from_mft.path(other)) {
            moved_count += 1;
            if moved.len() < EXAMPLES {
                moved.push(format!("{}  vs  {}", crawled.path(id), from_mft.path(other)));
            }
        }
    }

    // Entries only the MFT has, grouped by the top two levels of their path.
    let mut extra = 0u64;
    let mut extra_by_place: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for id in 0..from_mft.len() {
        if from_mft.is_deleted(id) || !crawled.entries_with_file_id(from_mft.file_id(id)).is_empty() {
            continue;
        }
        extra += 1;
        let path = from_mft.path(id);
        let place: String = path.split('\\').take(3).collect::<Vec<_>>().join("\\");
        *extra_by_place.entry(place).or_default() += 1;
    }
    let mut places: Vec<_> = extra_by_place.into_iter().collect();
    places.sort_by(|a, b| b.1.cmp(&a.1));

    println!("
comparison with the crawled index ({} entries)", crawled.live_count());
    println!("  matched by file id and name      {matched}");
    println!("  crawled entries without an id    {no_id}");
    println!("  crawled but missing from MFT     {missing_count}");
    for path in &missing {
        println!("      {path}");
    }
    println!("  files whose size differs         {size_count}");
    for line in &size {
        println!("      {line}");
    }
    println!("  files whose date differs         {mtime}");
    println!("  entries at a different path      {moved_count}");
    for line in &moved {
        println!("      {line}");
    }
    println!("  only in the MFT                  {extra}");
    for (place, count) in places.iter().take(EXAMPLES) {
        println!("      {count:>9}  {place}");
    }
    let (a, b) = (crawled.roots()[0], from_mft.roots()[0]);
    println!("  total size, crawl / mft          {} / {}", human(crawled.size(a)), human(from_mft.size(b)));
    println!("  on disk, crawl / mft             {} / {}", human(crawled.alloc(a)), human(from_mft.alloc(b)));
}

/// Lists and name-sorts the folder with the most direct children.
fn time_listing(index: &Index) -> (usize, Duration) {
    let mut counts = vec![0u32; index.len() as usize];
    for id in 0..index.len() {
        let parent = index.parent(id);
        if parent != kestrel_index::NO_ENTRY && !index.is_deleted(id) {
            counts[parent as usize] += 1;
        }
    }
    let busiest = (0..index.len()).max_by_key(|&id| counts[id as usize]).unwrap_or(0);
    let started = Instant::now();
    let mut ids: Vec<EntryId> = index.children(busiest).collect();
    sort(index, &mut ids, Sort { key: SortKey::Name, descending: false, folders_first: true });
    (ids.len(), started.elapsed())
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}
