//! The search box's searches: phrases in plain words ("videos from last month over 1 GB",
//! `kestrel_query::natural`), searches inside files (`kestrel-content`), and dates photos were
//! taken, behind one command, `search_run`, that the window calls for every search.
//!
//! A search's text is read as a phrase first; what it becomes is shown in the search box's
//! chips, each a clause the owner can edit. A query with `content:` clauses, or any text typed
//! while "Search inside files" is on, reads files: the other clauses choose which (the index's
//! results for them, or, with none, every file in the folder on screen, walked from disk with
//! `.gitignore` honoured inside repositories), and the matches arrive as they are found
//! (`content.rs`). Anything else is the index's usual search, with tags, the media library's
//! clauses and descriptions resolved first (`resolve.rs`).
//!
//! `taken:` is the media library's, which answers it from what it has read. A search limited to
//! a folder the library doesn't read, and a search inside files, read the date from each photo
//! instead (`taken.rs`), so `taken:` works in any folder.

mod content;
pub mod places;
pub(crate) use content::{Chosen, Files, run_to_end};
pub mod resolve;
pub mod taken;

use std::collections::HashMap;
use std::sync::Arc;

use kestrel_content::{Case, Limits, Pattern, Search};
use kestrel_index::{DirItem, Meta};
use kestrel_query::natural::{self, PartKind, Understood};
use kestrel_query::{Clock, ClauseKind, MediaClause, parse};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, Window};

use crate::engine::{Engine, Listing, Row, SortSpec};
use crate::projects::Projects;
use crate::visibility::Visibility;
use content::{Filters, Plan};
pub use content::{Searches, Status};

/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// How the search box's options say to search.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Options {
    /// "Search inside files": the text is looked for in the files' contents.
    pub inside: bool,
    /// The text is a regular expression.
    pub regex: bool,
    pub match_case: bool,
    pub whole_word: bool,
    /// `.gitignore` files are honoured inside repositories.
    pub gitignore: bool,
    /// Files larger than this many megabytes are passed over; 0 for the default.
    pub max_size_mb: u64,
}

/// One chip of the search box: a clause and what it means.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Chip {
    pub clause: String,
    pub label: String,
    /// `kind`, `modified`, `taken`, `size`, `place`, `content`, `media` (a media library fact),
    /// `like` (what a picture shows), `name` or `filter`.
    pub kind: &'static str,
    /// The words it was read from.
    pub phrase: String,
}

/// What a search found, and how its text was read.
#[derive(Serialize)]
pub struct Answer {
    pub listing: Listing,
    /// The text was read as a phrase.
    pub natural: bool,
    /// The query that ran.
    pub query: String,
    pub chips: Vec<Chip>,
    /// For a search inside files, how far it got.
    pub content: Option<Status>,
}

fn kind_name(kind: PartKind) -> &'static str {
    match kind {
        PartKind::Kind => "kind",
        PartKind::Modified => "modified",
        PartKind::Taken => "taken",
        PartKind::Size => "size",
        PartKind::Place => "place",
        PartKind::Content => "content",
        PartKind::Media => "media",
        PartKind::Like => "like",
        PartKind::Name => "name",
        PartKind::Filter => "filter",
    }
}

fn chips(understood: &Understood) -> Vec<Chip> {
    understood
        .parts
        .iter()
        .map(|part| Chip { clause: part.clause.clone(), label: part.label.clone(), kind: kind_name(part.kind), phrase: part.phrase.clone() })
        .collect()
}

/// How the window asked to search.
pub struct Ask<'a> {
    pub text: &'a str,
    /// The folder the search is limited to ("Search this folder"), if any.
    pub scope: Option<&'a str>,
    /// The folder the tab is in, if it is one: where a search inside files with no other
    /// clause looks, even when searching everywhere.
    pub folder: Option<&'a str>,
    pub visibility: Visibility,
    pub options: &'a Options,
    /// Whether the media library reads the folder at a path (`Media::covers`): `taken:` in a
    /// search limited to one it doesn't is read from the photos.
    pub library: &'a dyn Fn(&str) -> bool,
}

/// What to do with a search's text.
pub enum Decided {
    /// The index's usual search, of this query.
    Index { query: String },
    /// Read files.
    Content { plan_query: String, patterns: Vec<Pattern>, taken: Vec<(i64, i64, bool)> },
}

/// Reads the text: as a phrase, or with "Search inside files" on, as text to look for and
/// the clauses that choose files. Returns what it was read as, and what to do. The query
/// service's search reads its text the same way (devtools/query.rs).
pub fn decide(ask: &Ask<'_>, clock: Clock, places: &dyn natural::Places) -> Result<(Understood, Decided), String> {
    let options = ask.options;
    let case = if options.match_case { Case::Sensitive } else { Case::Insensitive };
    let pattern = |text: &str, negate: bool| Pattern { text: text.to_owned(), regex: options.regex, case, whole_word: options.whole_word, negate };
    let (understood, query) = if options.inside {
        let (text, filters) = natural::split_content(ask.text);
        let mut understood = natural::understand(&filters, clock, &natural::NoPlaces);
        if text.is_empty() {
            return Ok((understood, Decided::Index { query: filters }));
        }
        understood.parts.insert(0, natural::Part {
            clause: String::new(),
            label: format!("Contains \u{201c}{text}\u{201d}"),
            kind: PartKind::Content,
            phrase: text.clone(),
        });
        let rest = understood.query.clone();
        return Ok((understood, Decided::Content { plan_query: rest, patterns: vec![pattern(&text, false)], taken: Vec::new() }));
    } else {
        let understood = natural::understand(ask.text, clock, places);
        let query = understood.query.clone();
        (understood, query)
    };
    let parsed = parse(&query, clock).map_err(|e| e.to_string())?;
    let is_taken = |kind: &ClauseKind| matches!(kind, ClauseKind::Media(MediaClause::Taken(..)));
    let contents = parsed.clauses.iter().any(|clause| matches!(clause.kind, ClauseKind::Content(_)));
    // The media library answers `taken:` everywhere and in the folders it reads; elsewhere, and
    // in a search inside files, each photo's own date is read.
    let read_taken = contents || ask.scope.is_some_and(|scope| !(ask.library)(scope));
    let reads = contents || (read_taken && parsed.clauses.iter().any(|clause| is_taken(&clause.kind)));
    if !reads {
        return Ok((understood, Decided::Index { query }));
    }
    let mut patterns = Vec::new();
    let mut taken = Vec::new();
    for clause in &parsed.clauses {
        match &clause.kind {
            ClauseKind::Content(text) => patterns.push(pattern(text, clause.negate)),
            &ClauseKind::Media(MediaClause::Taken(from, to)) => taken.push((from, to, clause.negate)),
            _ => {}
        }
    }
    // The rest of the text, without the clauses that read files, chooses the files.
    let rest: Vec<String> = natural::tokens(&query)
        .into_iter()
        .filter(|token| {
            let key = token.trim_start_matches('!').split_once(':').map(|(key, _)| key.to_ascii_lowercase());
            !matches!(key.as_deref(), Some("content" | "taken" | "dt"))
        })
        .collect();
    Ok((understood, Decided::Content { plan_query: rest.join(" "), patterns, taken }))
}

/// A row of index results as an item to search.
fn item_of(row: Row) -> Option<(String, DirItem)> {
    if row.dir {
        return None;
    }
    let meta = Meta { size: row.size, alloc: row.alloc, mtime: row.mtime * 10_000 + FILETIME_UNIX_EPOCH, attrs: row.attrs, file_id: 0 };
    Some((row.location?, DirItem { name: row.name, meta, alias: false }))
}

/// The name of a folder, for the status line.
fn folder_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('\\');
    trimmed.rsplit('\\').next().filter(|name| !name.is_empty() && !name.ends_with(':')).unwrap_or(trimmed).to_owned()
}

/// Decides which files a search inside files reads; `choose` runs the index's search of the
/// other clauses when they choose the files (the window's way keeps a result set for a moment,
/// the query service's reads the index in place).
pub(crate) fn plan(
    engine: &Engine,
    ask: &Ask<'_>,
    plan_query: &str,
    patterns: Vec<Pattern>,
    taken: Vec<(i64, i64, bool)>,
    choose: impl FnOnce(&str) -> Result<Chosen, String>,
) -> Result<Plan, String> {
    let options = ask.options;
    let max_size = if options.max_size_mb == 0 { Limits::default().max_file_size } else { options.max_size_mb << 20 };
    let search = Search::new(&patterns, Limits { max_file_size: max_size, ..Limits::default() })?;
    let parsed = parse(plan_query, Clock::local()).map_err(|e| e.to_string())?;
    let root = ask.scope.or(ask.folder);
    // In a folder the index holds, the other clauses' results are the files; anywhere else
    // (and with no other clause) the folder is walked from disk.
    let indexed = root.is_some_and(|root| engine.index().read().lookup(&crate::engine::normalize(root)).is_some());
    let has_clauses = !parsed.clauses.is_empty();
    let files = if has_clauses && (ask.scope.is_none() || indexed) {
        Files::Chosen(choose(plan_query)?)
    } else if let Some(root) = root {
        Files::Folder { root: crate::engine::normalize(root), visibility: ask.visibility, filters: Filters::new(&parsed)? }
    } else {
        return Err("Open a folder to search inside its files, or add a filter such as ext:txt to search the files it finds.".into());
    };
    let place = match (&files, root) {
        (Files::Chosen(chosen), _) => format!("{} files", chosen.len()),
        (Files::Folder { .. }, Some(root)) => folder_name(root),
        _ => String::new(),
    };
    Ok(Plan { search, files, gitignore: options.gitignore, taken, place, dawdle: content::slowness() })
}

type Shared<'a> = State<'a, Arc<Engine>>;

/// Runs the search box's search for a tab. With `update`, a search inside files already
/// running for the same text and options answers with what it found so far instead of
/// starting again.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub fn search_run(
    app: AppHandle,
    window: Window,
    engine: Shared,
    db: crate::shortcuts::Data,
    semantic: crate::semantic::Shared,
    media: State<Arc<crate::media::Media>>,
    searches: State<Searches>,
    projects: State<Arc<Projects>>,
    tab: u32,
    text: String,
    scope: Option<String>,
    folder: Option<String>,
    sort: SortSpec,
    show_hidden: Option<bool>,
    show_protected: Option<bool>,
    options: Options,
    update: bool,
) -> Result<Answer, String> {
    let visibility = Visibility::from_request(show_hidden).with_protected(show_protected);
    let library = |path: &str| media.covers(path);
    let ask = Ask { text: &text, scope: scope.as_deref(), folder: folder.as_deref(), visibility, options: &options, library: &library };
    let places = places::Resolver::new(projects.inner().clone());
    let (understood, decided) = decide(&ask, Clock::local(), &places)?;
    let label = window.label().to_owned();
    match decided {
        Decided::Index { query } => {
            searches.cancel(&label, tab);
            let listing = resolve::index_search(&engine, &db, &semantic, &query, ask.scope, sort, visibility)?;
            Ok(Answer { listing, natural: understood.natural, query, chips: chips(&understood), content: None })
        }
        Decided::Content { plan_query, patterns, taken } => {
            let key = serde_json::to_string(&(&text, &scope, &folder, &options, show_hidden, show_protected)).unwrap_or_default();
            if !(update && searches.is_current(&label, tab, &key)) {
                // The search this one replaces stops reading now.
                searches.supersede(&label, tab, &key);
                // Chosen again only when what chooses them (or the index) changed, not the text.
                let chosen_key = serde_json::to_string(&(&plan_query, &scope, show_hidden, show_protected, engine.revision())).unwrap_or_default();
                let choose = |plan_query: &str| -> Result<Chosen, String> {
                    searches.chosen(&label, tab, chosen_key, || {
                        let mut resolver = resolve::Resolver::prepare(&db, &semantic, plan_query)?;
                        let listing =
                            engine.search_resolving(plan_query, ask.scope, sort, visibility, |query, index| resolver.resolve(query, index).map(|_| ()))?;
                        let rows = engine.rows(listing.set, 0, listing.total);
                        engine.release(listing.set);
                        Ok(rows?.into_iter().filter_map(item_of).collect())
                    })
                };
                let plan = plan(&engine, &ask, &plan_query, patterns, taken, choose)?;
                let to = label.clone();
                let tell = move |progress: content::ProgressEvent| {
                    let _ = app.emit_to(to.as_str(), "search-progress", progress);
                };
                searches.start(tell, &label, tab, key, plan);
            }
            let (listing, status) = searches.snapshot(&engine, &label, tab, sort).ok_or("The search stopped")?;
            Ok(Answer { listing, natural: understood.natural, query: understood.query.clone(), chips: chips(&understood), content: Some(status) })
        }
    }
}

/// Stops the tab's search inside files. With `forget`, as when the tab leaves the search or
/// closes, what it found goes too; without, it stays to be shown (the Stop button).
#[tauri::command]
pub fn search_cancel(window: Window, searches: State<Searches>, tab: u32, forget: Option<bool>) {
    if forget.unwrap_or(true) {
        searches.cancel(window.label(), tab);
    } else {
        searches.stop(window.label(), tab);
    }
}

/// Stands scratch folders in for the known folders, and names projects (development builds
/// only), for the end-to-end tests: `folders` by kind (`downloads`, `documents`...). The
/// Downloads triage page then watches the folder standing in for Downloads.
#[tauri::command]
pub fn search_dev_places(app: AppHandle, folders: HashMap<String, String>, projects: Vec<(String, String)>) -> Result<(), String> {
    crate::projects::launch::development_only()?;
    places::stand_in(folders, projects);
    crate::triage::watch(&app);
    Ok(())
}

/// Makes every file of a search inside files wait `ms` milliseconds before it is read
/// (development builds only), so the end-to-end tests can watch results arrive; 0 stops it.
#[tauri::command]
pub fn search_dev_slow(ms: u64) -> Result<(), String> {
    crate::projects::launch::development_only()?;
    content::slow_down(ms);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCK: Clock = Clock { now_unix: 20_739 * 86_400 + 12 * 3600, utc_offset_secs: 0 };
    const BY_NAME: SortSpec = SortSpec { key: crate::engine::SortField::Name, desc: false };

    /// The media library reads nothing in these tests.
    fn nowhere(_: &str) -> bool {
        false
    }

    fn ask<'a>(text: &'a str, options: &'a Options) -> Ask<'a> {
        Ask { text, scope: None, folder: Some(r"C:\work"), visibility: Visibility::ALL, options, library: &nowhere }
    }

    #[test]
    fn plain_searches_go_to_the_index() {
        let options = Options::default();
        let (understood, decided) = decide(&ask("report ext:pdf", &options), CLOCK, &natural::NoPlaces).unwrap();
        assert!(!understood.natural);
        assert!(matches!(decided, Decided::Index { query } if query == "report ext:pdf"));
        let (understood, decided) = decide(&ask("pdfs from last week", &options), CLOCK, &natural::NoPlaces).unwrap();
        assert!(understood.natural);
        assert!(matches!(decided, Decided::Index { query } if query == "ext:pdf dm:lastweek"));
    }

    #[test]
    fn content_clauses_read_files_and_the_rest_chooses_them() {
        let options = Options { match_case: true, ..Options::default() };
        let (_, decided) = decide(&ask(r#"content:"fn main" !content:test ext:rs taken:2023"#, &options), CLOCK, &natural::NoPlaces).unwrap();
        let Decided::Content { plan_query, patterns, taken } = decided else { panic!() };
        assert_eq!(plan_query, "ext:rs");
        assert_eq!(patterns.iter().map(|p| (p.text.as_str(), p.negate, p.case)).collect::<Vec<_>>(), [
            ("fn main", false, Case::Sensitive),
            ("test", true, Case::Sensitive)
        ]);
        assert_eq!(taken.len(), 1);
        // A phrase can ask for contents too.
        let (understood, decided) = decide(&ask("notes containing invoice", &Options::default()), CLOCK, &natural::NoPlaces).unwrap();
        assert!(understood.natural);
        let Decided::Content { plan_query, patterns, .. } = decided else { panic!() };
        assert_eq!((plan_query.as_str(), patterns[0].text.as_str()), ("ext:txt;md;log", "invoice"));
    }

    #[test]
    fn taken_is_the_media_librarys_except_in_a_folder_it_does_not_read() {
        let options = Options::default();
        // Everywhere, the library answers.
        let (_, decided) = decide(&ask("taken:2023 ext:jpg", &options), CLOCK, &natural::NoPlaces).unwrap();
        assert!(matches!(decided, Decided::Index { query } if query == "taken:2023 ext:jpg"));
        // In a folder it reads, too.
        let photos = |path: &str| path.to_ascii_lowercase().starts_with(r"c:\photos");
        let inside = Ask { scope: Some(r"C:\Photos\2023"), library: &photos, ..ask("taken:2023", &options) };
        assert!(matches!(decide(&inside, CLOCK, &natural::NoPlaces).unwrap().1, Decided::Index { .. }));
        // In one it doesn't, each photo's date is read, and the other clauses choose the photos.
        let outside = Ask { scope: Some(r"D:\Camera"), library: &photos, ..ask("!taken:2023 ext:jpg", &options) };
        let Decided::Content { plan_query, patterns, taken } = decide(&outside, CLOCK, &natural::NoPlaces).unwrap().1 else { panic!() };
        assert_eq!((plan_query.as_str(), patterns.len(), taken.len(), taken[0].2), ("ext:jpg", 0, 1, true));
        // Other media clauses are always the library's.
        assert!(matches!(decide(&outside_of("width:>100", &options, &photos), CLOCK, &natural::NoPlaces).unwrap().1, Decided::Index { .. }));
    }

    fn outside_of<'a>(text: &'a str, options: &'a Options, library: &'a dyn Fn(&str) -> bool) -> Ask<'a> {
        Ask { scope: Some(r"D:\Camera"), library, ..ask(text, options) }
    }

    #[test]
    fn inside_files_the_words_are_the_text_to_find() {
        let options = Options { inside: true, regex: true, ..Options::default() };
        let (understood, decided) = decide(&ask(r"fn\s+main ext:rs", &options), CLOCK, &natural::NoPlaces).unwrap();
        let Decided::Content { plan_query, patterns, .. } = decided else { panic!() };
        assert_eq!((plan_query.as_str(), patterns[0].text.as_str(), patterns[0].regex), ("ext:rs", r"fn\s+main", true));
        assert_eq!(chips(&understood).iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Contains \u{201c}fn\\s+main\u{201d}", "RS files"]);
        // Only filters: a plain search of them.
        let (_, decided) = decide(&ask("ext:rs", &options), CLOCK, &natural::NoPlaces).unwrap();
        assert!(matches!(decided, Decided::Index { query } if query == "ext:rs"));
    }

    /// A search inside a scratch folder, from start to finish, as the command runs it.
    #[test]
    fn a_folder_is_searched_and_its_matches_shown_as_results() {
        let root = std::env::temp_dir().join(format!("kestrel-search-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\nthe needle\n").unwrap();
        std::fs::write(root.join("sub").join("b.md"), "needle first\n").unwrap();
        std::fs::write(root.join("c.txt"), "nothing\n").unwrap();
        let root_text = root.to_str().unwrap().to_owned();
        let engine = Engine::default();
        let options = Options { inside: true, gitignore: true, ..Options::default() };
        let asked = Ask { text: "needle", scope: None, folder: Some(&root_text), visibility: Visibility::ALL, options: &options, library: &nowhere };
        let (_, decided) = decide(&asked, CLOCK, &natural::NoPlaces).unwrap();
        let Decided::Content { plan_query, patterns, taken } = decided else { panic!() };
        let planned = plan(&engine, &asked, &plan_query, patterns, taken, |_| Ok(Arc::new(Vec::new()))).unwrap();
        assert!(matches!(planned.files, Files::Folder { .. }));
        assert_eq!(planned.place, format!("kestrel-search-run-{}", std::process::id()));
        // Run it as the background thread does, then show it as the window would.
        let searches = Searches::default();
        searches.run_here("main", 1, planned);
        let (listing, status) = searches.snapshot(&engine, "main", 1, BY_NAME).unwrap();
        assert!(status.done);
        assert_eq!((status.counts.matched, listing.total), (2, 2));
        let rows = engine.rows(listing.set, 0, 10).unwrap();
        let shown: Vec<(&str, u64)> = rows.iter().map(|row| (row.name.as_str(), row.hit.as_ref().unwrap().line)).collect();
        assert_eq!(shown, [("a.txt", 2), ("b.md", 1)]);
        assert_eq!(rows[1].location.as_deref(), Some(root.join("sub").to_str().unwrap()));
        // The set acts as any results do: narrowed by name, grouped, its paths known.
        let narrowed = engine.filter_set(listing.set, &kestrel_query::NameFilter::new("*.md"), BY_NAME, None).unwrap().0;
        assert_eq!(engine.paths(narrowed.set, &[(0, 0)]).unwrap(), [root.join("sub").join("b.md").to_str().unwrap().to_owned()]);
        let (_, starts) = engine.group_set(listing.set, BY_NAME, Some(kestrel_query::Grouping { by: kestrel_query::GroupBy::Type, descending: false }), &crate::engine::Marks::default()).unwrap();
        assert_eq!(starts.len(), 2, "md and txt");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
