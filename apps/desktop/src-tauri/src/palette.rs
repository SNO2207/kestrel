//! The command palette's backend: one ranked list from many providers.
//!
//! The palette (Ctrl+K in the window, and the quick launcher anywhere in Windows) shows one
//! fuzzy-ranked list mixing actions, files and folders from the index, and whatever other
//! features add. Each kind of item comes from a *provider*. A provider lives either here, in
//! the backend, when its items come from the backend's data (the index, the database), or in
//! the front end (`src/lib/palette.ts`) when they are the front end's own (the app's actions).
//! Either way the items are ranked here, together, by one matcher ([`rank`]), so scores are
//! comparable across providers and the matched characters can be highlighted.
//!
//! # Adding a provider (for features such as projects and tags)
//!
//! Backend: implement [`Provider`] and register it once at startup:
//!
//! ```ignore
//! app.state::<Arc<Palette>>().register(Arc::new(MyProvider { db: db.clone() }));
//! ```
//!
//! A provider with a short list of items can build them all and call [`rank_items`]; one with
//! many (like [`files::FilesProvider`]) matches them itself with a [`Scorer`]. Its items need a
//! front end half too, which says what choosing one does: see `PaletteProvider` in
//! `src/lib/palette.ts`, where the provider is registered under the same id.
//!
//! # Prefixes
//!
//! A query starting with one of [`PREFIXES`] is narrowed to the providers that claim that
//! character: `>` actions, `/` paths, `@` projects and `#` tags. Without one, every provider
//! that takes part in the default mix ([`Provider::in_default`]) contributes.
//!
//! # Recency
//!
//! Items chosen from the palette, and folders visited, are remembered in the database
//! (`kestrel_db::palette`). Remembered items rank higher ([`rank::recency_weight`]), and the
//! empty palette lists them first.

pub mod files;
pub mod paths;
pub mod rank;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use kestrel_db::Db;
use kestrel_db::palette::{self as memory, Recent, Use};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tauri::State;

pub use rank::Scorer;

/// The characters that narrow the palette to some providers when typed first.
pub const PREFIXES: [char; 4] = ['>', '/', '@', '#'];
/// Results returned when the request does not say.
const DEFAULT_LIMIT: usize = 50;
/// The most results a request may ask for.
const MAX_LIMIT: usize = 500;
/// Remembered items listed when the palette opens with nothing typed.
const RECENT_SHOWN: usize = 8;
/// How long the remembered items are kept in memory before being read again, so that their
/// ages stay current.
const RECENTS_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// Something the palette can show and the user can choose.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Item {
    /// The provider that offers it, which also carries it out when chosen.
    pub provider: String,
    /// The provider's id for the item: a path for files and folders, a command name for actions.
    pub id: String,
    /// What is shown, and what the query is matched against.
    pub title: String,
    /// A second line, such as the folder a file is in.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Shown at the right: a keyboard shortcut for an action.
    #[serde(default)]
    pub hint: Option<String>,
    /// The name of an icon for items that are not files or folders.
    #[serde(default)]
    pub icon: Option<String>,
    /// The file or folder the item stands for, which the UI shows with its system icon.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub dir: bool,
    /// More words the query may match, never shown, such as "toggle" for "Show hidden items".
    #[serde(default, skip_serializing)]
    pub keywords: Option<String>,
    /// A percentage added to the match score (negative to take away), so a provider can rank
    /// its items above or below others that match as well.
    #[serde(default)]
    pub boost: i32,
}

impl Item {
    /// The key it is remembered under.
    pub fn key(&self) -> String {
        memory::key(&self.provider, &self.id)
    }
}

/// An item that matched, with how well.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub item: Item,
    /// What it ranks by: the match score lifted by recency and the item's boost.
    pub score: u32,
    /// The match score alone.
    #[serde(skip)]
    pub matched: u32,
    /// Where the query matched the title, as UTF-16 offsets, for highlighting.
    pub highlights: Vec<u32>,
    /// It was chosen or visited before, which helped its rank.
    pub recent: bool,
    /// Breaks ties: the order its provider listed it in.
    #[serde(skip)]
    pub order: usize,
    /// Part of a listing whose order is the point, such as a folder's contents in name order,
    /// which recency must not reshuffle.
    #[serde(skip)]
    pub listed: bool,
}

impl Hit {
    pub fn new(item: Item, matched: u32, highlights: Vec<u32>) -> Self {
        Self { item, score: matched, matched, highlights, recent: false, order: 0, listed: false }
    }

    /// How much recency lifts this hit.
    fn recency(&self, recents: &Recents) -> f64 {
        if self.listed { 0.0 } else { recents.weight(&self.item.key()) }
    }
}

/// What the palette remembers, read once and shared by the queries that follow.
pub struct Recents {
    rows: Vec<Recent>,
    by_key: HashMap<String, usize>,
    now: i64,
}

impl Recents {
    pub fn new(rows: Vec<Recent>, now: i64) -> Self {
        let by_key = rows.iter().enumerate().map(|(at, row)| (row.key.clone(), at)).collect();
        Self { rows, by_key, now }
    }

    /// How much an item's past use lifts it, from 0 to 1. See [`rank::recency_weight`].
    pub fn weight(&self, key: &str) -> f64 {
        self.by_key.get(key).map_or(0.0, |&at| {
            let row = &self.rows[at];
            rank::recency_weight(row.uses, self.now - row.last_used)
        })
    }

    /// Every remembered item, most recently used first.
    pub fn rows(&self) -> &[Recent] {
        &self.rows
    }
}

/// One query, as providers see it.
pub struct Query<'a> {
    /// What to match, without the prefix.
    pub text: &'a str,
    /// The prefix typed, if any: a provider that also takes part in the default mix may list
    /// more under its own prefix, or do more to answer it (the projects provider starts
    /// looking for projects only when asked by `@`).
    pub scope: Option<char>,
    /// Hidden files and folders are wanted ("Show hidden items" is on).
    pub show_hidden: bool,
    /// The most results wanted from each provider.
    pub limit: usize,
    pub recents: &'a Recents,
    cancel: &'a AtomicBool,
}

impl Query<'_> {
    /// A newer query from the same palette has started, so this one's results will be thrown
    /// away. Providers that take a while check this and stop early.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// What a hit will rank by once recency and its boost are applied. Providers sort by this
    /// before cutting their list to [`Query::limit`], so a remembered item is not cut early.
    pub fn rank_of(&self, hit: &Hit) -> u32 {
        rank::ranked(hit.matched, hit.recency(self.recents), hit.item.boost)
    }

    /// Sorts hits best first, as they will rank, and keeps the first [`Query::limit`].
    pub fn keep_best(&self, hits: &mut Vec<Hit>) {
        let mut ranked: Vec<(u32, Hit)> = hits.drain(..).map(|hit| (self.rank_of(&hit), hit)).collect();
        ranked.sort_by(|(a_rank, a), (b_rank, b)| {
            b_rank.cmp(a_rank).then(a.order.cmp(&b.order)).then_with(|| a.item.title.cmp(&b.item.title))
        });
        hits.extend(ranked.into_iter().take(self.limit).map(|(_, hit)| hit));
    }
}

/// A source of palette items. See the module documentation.
pub trait Provider: Send + Sync + 'static {
    /// Its name, such as `files`, which its items normally carry in [`Item::provider`].
    fn id(&self) -> &'static str;

    /// The prefix that narrows the palette to this provider, one of [`PREFIXES`].
    fn prefix(&self) -> Option<char> {
        None
    }

    /// Whether it contributes when no prefix is typed.
    fn in_default(&self) -> bool {
        true
    }

    /// Its best matches for `query`, at most `query.limit` (see [`Query::keep_best`]), each
    /// with highlights. It is not asked when nothing is typed and there is no prefix (the
    /// palette lists remembered items then); under its own prefix with nothing typed after it,
    /// it may list what it would show first.
    ///
    /// Each item's [`Item::provider`] names the front end handler that carries it out, which
    /// is normally this provider's own id; the paths provider hands its items to `files`.
    fn search(&self, query: &Query) -> Vec<Hit>;

    /// A remembered item of this provider as it is now, for the empty palette's list of what
    /// was used lately (which keeps only the title, subtitle and path): `None` if it is gone.
    /// The default shows it as it was remembered, which suits items that never change, and
    /// files and folders, which are dropped once their path is gone.
    fn current(&self, remembered: Item) -> Option<Item> {
        Some(remembered)
    }
}

/// Ranks a short list of items against the query: what a provider with a few hundred items
/// at most calls from [`Provider::search`]. An item matches by its title or, more weakly, by its
/// keywords. With an empty query every item is returned, in the order given.
pub fn rank_items(query: &Query, items: impl IntoIterator<Item = Item>) -> Vec<Hit> {
    let mut scorer = Scorer::new(query.text);
    let mut hits: Vec<Hit> = items
        .into_iter()
        .enumerate()
        .filter_map(|(order, item)| {
            let by_title = scorer.score(&item.title);
            // A keyword match counts for less than the same match in the title.
            let by_keywords = item.keywords.as_deref().and_then(|words| scorer.score(words)).map(|score| score * 3 / 4);
            let matched = by_title.max(by_keywords)?;
            let highlights = if by_title.is_some() { scorer.highlights(&item.title) } else { Vec::new() };
            Some(Hit { order, ..Hit::new(item, matched, highlights) })
        })
        .collect();
    query.keep_best(&mut hits);
    hits
}

/// The request the UI sends for each keystroke.
#[derive(Deserialize)]
pub struct Request {
    /// Everything typed, prefix included.
    pub text: String,
    /// Which palette is asking (`main` or `quick`): each one's newer query cancels its older.
    #[serde(default)]
    pub session: String,
    /// The front end's own providers' items, to rank with the rest.
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// The prefix typed, if any.
    pub scope: Option<char>,
    /// The backend providers that took part, so the UI can tell "nothing matches" from "nothing
    /// provides this prefix yet".
    pub providers: Vec<&'static str>,
    pub took_us: u64,
}

/// Splits what was typed into its prefix, if any, and the text to match.
pub fn split_prefix(text: &str) -> (Option<char>, &str) {
    let trimmed = text.trim_start();
    match trimmed.chars().next() {
        Some(first) if PREFIXES.contains(&first) => (Some(first), trimmed[first.len_utf8()..].trim_start()),
        _ => (None, trimmed),
    }
}

/// The palette: its providers and its memory. Managed by Tauri as `State<Arc<Palette>>`.
pub struct Palette {
    db: Arc<Db>,
    providers: RwLock<Vec<Arc<dyn Provider>>>,
    /// The remembered items, read once and kept for a while, or until an item is recorded or
    /// forgotten.
    recents: Mutex<Option<(Instant, Arc<Recents>)>>,
    /// The cancel flag of each palette's query in flight.
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl Palette {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db, providers: RwLock::default(), recents: Mutex::default(), running: Mutex::default() }
    }

    /// Adds a provider. A provider registered again under the same id replaces the first.
    pub fn register(&self, provider: Arc<dyn Provider>) {
        let mut providers = self.providers.write();
        providers.retain(|existing| existing.id() != provider.id());
        providers.push(provider);
    }

    fn recents(&self) -> Arc<Recents> {
        let mut cached = self.recents.lock();
        if let Some((read, recents)) = cached.as_ref()
            && read.elapsed() < RECENTS_FRESH
        {
            return recents.clone();
        }
        let rows = memory::recent(&self.db).unwrap_or_else(|error| {
            eprintln!("the palette could not read what was used recently: {error}");
            Vec::new()
        });
        let recents = Arc::new(Recents::new(rows, now_ms()));
        *cached = Some((Instant::now(), recents.clone()));
        recents
    }

    /// Remembers that an item was chosen, or a folder visited.
    pub fn record(&self, item: &Use) -> Result<(), String> {
        memory::record(&self.db, item, now_ms())?;
        *self.recents.lock() = None;
        Ok(())
    }

    pub fn forget(&self, provider: &str, id: &str) -> Result<(), String> {
        memory::forget(&self.db, provider, id)?;
        *self.recents.lock() = None;
        Ok(())
    }

    /// Runs a query against every provider it concerns and ranks the results together.
    pub fn query(&self, request: Request) -> Result<Results, String> {
        let started = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.running.lock().insert(request.session.clone(), cancel.clone()) {
            previous.store(true, Ordering::Relaxed);
        }
        let (scope, text) = split_prefix(&request.text);
        let limit = request.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let recents = self.recents();
        let query = Query { text, scope, show_hidden: request.show_hidden, limit, recents: &recents, cancel: &cancel };
        let registered: Vec<Arc<dyn Provider>> = self.providers.read().clone();
        let active: Vec<&Arc<dyn Provider>> = registered
            .iter()
            .filter(|provider| match scope {
                Some(prefix) => provider.prefix() == Some(prefix),
                None => provider.in_default(),
            })
            .collect();

        let mut hits = rank_items(&query, request.items);
        // Nothing typed and no prefix: the palette lists what was used lately, not every
        // provider's first items.
        let opening = scope.is_none() && text.is_empty();
        for (rank, provider) in active.iter().enumerate().filter(|_| !opening) {
            hits.extend(provider.search(&query).into_iter().enumerate().map(|(position, mut hit)| {
                hit.order = (rank + 1) * MAX_LIMIT * 4 + position;
                hit
            }));
            if query.cancelled() {
                return Err("Superseded by a newer query".into());
            }
        }
        if opening {
            // A remembered item of a backend provider is listed only while that provider takes
            // part; anything else is the front end's to keep or drop.
            let shown = |provider: &str| {
                !registered.iter().any(|p| p.id() == provider) || active.iter().any(|p| p.id() == provider)
            };
            let current = |item: Item| match registered.iter().find(|p| p.id() == item.provider) {
                Some(provider) => provider.current(item),
                None => Some(item),
            };
            hits.extend(remembered(&recents, shown, current));
        }
        let hits = finish(hits, &recents, limit);
        let providers = active.iter().map(|provider| provider.id()).collect();
        Ok(Results { hits, scope, providers, took_us: started.elapsed().as_micros() as u64 })
    }
}

/// The remembered items an empty palette lists first: those `shown` allows, as `current`
/// says they are now (it drops those that are gone), and files and folders only while they
/// still exist.
fn remembered(recents: &Recents, shown: impl Fn(&str) -> bool, current: impl Fn(Item) -> Option<Item>) -> Vec<Hit> {
    recents
        .rows()
        .iter()
        .filter(|row| shown(&row.item.provider))
        .filter_map(|row| {
            current(Item {
                provider: row.item.provider.clone(),
                id: row.item.id.clone(),
                title: row.item.title.clone(),
                subtitle: row.item.subtitle.clone(),
                path: row.item.path.clone(),
                dir: row.item.dir,
                ..Item::default()
            })
        })
        .filter(|item| item.path.as_deref().is_none_or(|path| std::path::Path::new(path).exists()))
        .take(RECENT_SHOWN)
        .enumerate()
        .map(|(order, item)| Hit { order, ..Hit::new(item, 0, Vec::new()) })
        .collect()
}

/// Applies recency and boosts, drops duplicates (an item found by two routes keeps its best
/// showing), and sorts: best first, then the more recently used, then as listed.
fn finish(hits: Vec<Hit>, recents: &Recents, limit: usize) -> Vec<Hit> {
    let mut best: HashMap<String, Hit> = HashMap::with_capacity(hits.len());
    let mut weights: HashMap<String, f64> = HashMap::new();
    for mut hit in hits {
        let key = hit.item.key();
        let weight = hit.recency(recents);
        hit.recent = recents.weight(&key) > 0.0;
        hit.score = rank::ranked(hit.matched, weight, hit.item.boost);
        weights.insert(key.clone(), weight);
        match best.get(&key) {
            // The first one listed wins a tie: the front end's own copy of an item is fresher
            // than the remembered one.
            Some(kept) if (kept.score, !kept.highlights.is_empty()) >= (hit.score, !hit.highlights.is_empty()) => {}
            _ => {
                best.insert(key, hit);
            }
        }
    }
    let mut hits = one_per_path(best.into_values().collect());
    let weight = |hit: &Hit| weights.get(&hit.item.key()).copied().unwrap_or(0.0);
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| weight(b).total_cmp(&weight(a)))
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.item.title.cmp(&b.item.title))
    });
    hits.truncate(limit);
    hits
}

/// Keeps one hit per file or folder. The same folder can come from several providers (the
/// index, a pin, the frequent folders, the projects); the best-ranked showing stands for all
/// of them, which is normally the one whose provider says the most about it (a pin with its
/// number key, a project), since those are boosted.
fn one_per_path(hits: Vec<Hit>) -> Vec<Hit> {
    let mut by_path: HashMap<String, usize> = HashMap::new();
    let mut kept: Vec<Option<Hit>> = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(path) = hit.item.path.as_deref().map(|path| path.trim_end_matches('\\').to_lowercase()) else {
            kept.push(Some(hit));
            continue;
        };
        match by_path.get(&path) {
            Some(&at) => {
                let Some(previous) = kept[at].as_ref() else { continue };
                let better = (hit.score, std::cmp::Reverse(hit.order)) > (previous.score, std::cmp::Reverse(previous.order));
                if better {
                    kept[at] = Some(hit);
                }
            }
            None => {
                by_path.insert(path, kept.len());
                kept.push(Some(hit));
            }
        }
    }
    kept.into_iter().flatten().collect()
}

// ---------- commands ----------

/// Ranks what the palette should show for `request.text`.
#[tauri::command(async)]
pub fn palette_query(palette: State<Arc<Palette>>, request: Request) -> Result<Results, String> {
    palette.query(request)
}

/// Remembers that an item was chosen from the palette, or a folder visited, so it ranks higher.
#[tauri::command(async)]
pub fn palette_record(palette: State<Arc<Palette>>, item: Use) -> Result<(), String> {
    palette.record(&item)
}

/// Forgets a remembered item.
#[tauri::command(async)]
pub fn palette_forget(palette: State<Arc<Palette>>, provider: String, id: String) -> Result<(), String> {
    palette.forget(&provider, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(provider: &str, id: &str, title: &str) -> Item {
        Item { provider: provider.into(), id: id.into(), title: title.into(), ..Item::default() }
    }

    struct Fixed(&'static str, Option<char>, Vec<Item>);

    impl Provider for Fixed {
        fn id(&self) -> &'static str {
            self.0
        }
        fn prefix(&self) -> Option<char> {
            self.1
        }
        fn in_default(&self) -> bool {
            self.1 != Some('#')
        }
        fn search(&self, query: &Query) -> Vec<Hit> {
            rank_items(query, self.2.clone())
        }
    }

    fn palette() -> Palette {
        let palette = Palette::new(Arc::new(Db::open_in_memory().unwrap()));
        palette.register(Arc::new(Fixed(
            "projects",
            Some('@'),
            vec![item("projects", "kestrel", "Kestrel"), item("projects", "website", "Company website")],
        )));
        palette.register(Arc::new(Fixed("tags", Some('#'), vec![item("tags", "red", "Red"), item("tags", "work", "Work")])));
        palette
    }

    fn ask(palette: &Palette, text: &str, items: Vec<Item>) -> Vec<String> {
        let request = Request { text: text.into(), session: "test".into(), items, show_hidden: false, limit: None };
        palette.query(request).unwrap().hits.into_iter().map(|hit| format!("{}:{}", hit.item.provider, hit.item.id)).collect()
    }

    #[test]
    fn prefixes_are_split_off_and_narrow_the_providers() {
        assert_eq!(split_prefix("  > new tab"), (Some('>'), "new tab"));
        assert_eq!(split_prefix("/C:\\Users"), (Some('/'), "C:\\Users"));
        assert_eq!(split_prefix("report"), (None, "report"));
        assert_eq!(split_prefix(""), (None, ""));

        let palette = palette();
        let actions = vec![item("actions", "new-tab", "New tab"), item("actions", "work-offline", "Work offline")];
        assert_eq!(ask(&palette, "#wor", Vec::new()), ["tags:work"], "only the tags provider");
        assert_eq!(ask(&palette, "@", Vec::new()), ["projects:kestrel", "projects:website"], "listed in order");
        let mixed = ask(&palette, "w", actions);
        assert!(mixed.contains(&"projects:website".to_owned()) && mixed.contains(&"actions:work-offline".to_owned()));
        assert!(!mixed.contains(&"tags:work".to_owned()), "tags take no part without their prefix");
    }

    /// A folder's contents, as the paths provider lists them with nothing typed.
    struct Listing;

    impl Provider for Listing {
        fn id(&self) -> &'static str {
            "listing"
        }
        fn prefix(&self) -> Option<char> {
            Some('/')
        }
        fn search(&self, _query: &Query) -> Vec<Hit> {
            ["a", "b", "c"]
                .iter()
                .enumerate()
                .map(|(order, id)| Hit { order, listed: true, ..Hit::new(item("listing", id, id), 0, Vec::new()) })
                .collect()
        }
    }

    #[test]
    fn listings_keep_their_order_whatever_was_used() {
        let palette = palette();
        palette.register(Arc::new(Listing));
        palette.record(&Use { provider: "listing".into(), id: "c".into(), title: "c".into(), ..Use::default() }).unwrap();
        assert_eq!(ask(&palette, "/", Vec::new()), ["listing:a", "listing:b", "listing:c"]);
    }

    #[test]
    fn registering_an_id_again_replaces_the_provider() {
        let palette = palette();
        palette.register(Arc::new(Fixed("tags", Some('#'), vec![item("tags", "blue", "Blue")])));
        assert_eq!(ask(&palette, "#", Vec::new()), ["tags:blue"]);
    }

    #[test]
    fn remembered_items_rank_higher_and_head_the_empty_palette() {
        let palette = palette();
        let actions = || {
            vec![
                item("actions", "show-hidden", "Show hidden items"),
                item("actions", "show-extensions", "Show file extensions"),
            ]
        };
        assert_eq!(ask(&palette, ">show", actions()), ["actions:show-hidden", "actions:show-extensions"]);
        let used = Use { provider: "actions".into(), id: "show-extensions".into(), title: "Show file extensions".into(), ..Use::default() };
        palette.record(&used).unwrap();
        palette.record(&used).unwrap();
        assert_eq!(ask(&palette, ">show", actions()), ["actions:show-extensions", "actions:show-hidden"]);

        // Opened with nothing typed: the remembered action first, deduplicated against the
        // front end's own copy of it.
        let empty = ask(&palette, "", actions());
        assert_eq!(empty, ["actions:show-extensions", "actions:show-hidden"]);
        palette.forget("actions", "SHOW-EXTENSIONS").unwrap();
        assert_eq!(ask(&palette, "", actions()), ["actions:show-hidden", "actions:show-extensions"]);
    }

    #[test]
    fn remembered_files_that_are_gone_are_not_listed() {
        let palette = palette();
        let here = std::env::temp_dir();
        let gone = here.join("kestrel-palette-no-such-folder");
        for path in [&here, &gone] {
            let path = path.to_string_lossy().into_owned();
            palette.record(&Use { provider: "files".into(), id: path.clone(), title: path.clone(), path: Some(path), dir: true, ..Use::default() }).unwrap();
        }
        palette.register(Arc::new(Fixed("files", None, Vec::new())));
        let listed = ask(&palette, "", Vec::new());
        assert_eq!(listed, [format!("files:{}", here.to_string_lossy())]);
    }

    #[test]
    fn keywords_match_more_weakly_than_titles_and_are_not_highlighted() {
        let palette = palette();
        let items = vec![
            Item { keywords: Some("toggle hidden".into()), ..item("actions", "show-hidden", "Show hidden items") },
            item("actions", "toggle-preview", "Toggle preview pane"),
        ];
        let request = Request { text: ">toggle".into(), session: "t".into(), items, show_hidden: false, limit: None };
        let hits = palette.query(request).unwrap().hits;
        assert_eq!(hits[0].item.id, "toggle-preview");
        assert_eq!(hits[0].highlights, [0, 1, 2, 3, 4, 5]);
        assert_eq!(hits[1].item.id, "show-hidden");
        assert!(hits[1].highlights.is_empty());
    }

    /// Saved things, which can be renamed or deleted after they were chosen.
    struct Saved;

    impl Provider for Saved {
        fn id(&self) -> &'static str {
            "saved"
        }
        fn search(&self, _query: &Query) -> Vec<Hit> {
            Vec::new()
        }
        fn current(&self, remembered: Item) -> Option<Item> {
            (remembered.id == "kept").then(|| Item { title: "Renamed".into(), icon: Some("search".into()), ..remembered })
        }
    }

    #[test]
    fn remembered_items_are_shown_as_they_are_now_and_not_once_gone() {
        let palette = palette();
        palette.register(Arc::new(Saved));
        for id in ["gone", "kept"] {
            palette.record(&Use { provider: "saved".into(), id: id.into(), title: "Old name".into(), ..Use::default() }).unwrap();
        }
        let request = Request { text: String::new(), session: "t".into(), items: Vec::new(), show_hidden: false, limit: None };
        let hits = palette.query(request).unwrap().hits;
        let shown: Vec<(&str, &str)> = hits.iter().map(|hit| (hit.item.id.as_str(), hit.item.title.as_str())).collect();
        assert_eq!(shown, [("kept", "Renamed")]);
        assert_eq!(hits[0].item.icon.as_deref(), Some("search"), "with what was not remembered");
    }

    #[test]
    fn a_folder_offered_by_several_providers_is_listed_once() {
        let palette = palette();
        let at = |provider: &str, path: &str, boost: i32| Item {
            path: Some(path.into()),
            dir: true,
            boost,
            ..item(provider, path, "Falcon")
        };
        palette.register(Arc::new(Fixed("index", None, vec![at("index", r"C:\Code\Falcon", 0), at("index", r"C:\Old\Falcon", 0)])));
        palette.register(Arc::new(Fixed("pins", None, vec![at("pins", r"c:\code\falcon\", 25)])));
        assert_eq!(ask(&palette, "falcon", Vec::new()), [r"pins:c:\code\falcon\", r"index:C:\Old\Falcon"]);
    }

    #[test]
    fn boosts_lift_a_providers_items_and_limits_hold() {
        let palette = palette();
        let items: Vec<Item> = (0..120).map(|i| item("actions", &format!("a{i}"), &format!("Item {i}"))).collect();
        let request = Request { text: "item".into(), session: "t".into(), items: items.clone(), show_hidden: false, limit: Some(10) };
        assert_eq!(palette.query(request).unwrap().hits.len(), 10);
        let mut boosted = items;
        boosted[77].boost = 50;
        let request = Request { text: "item".into(), session: "t".into(), items: boosted, show_hidden: false, limit: Some(3) };
        assert_eq!(palette.query(request).unwrap().hits[0].item.id, "a77");
    }
}
