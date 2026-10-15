//! The shortcuts in the command palette: pinned places, frequent folders, saved searches and
//! workspaces take part in every query, and `#` lists the tags.
//!
//! Each is a backend provider over the same data the sidebar shows (`kestrel_db::shortcuts`),
//! so the quick launcher, which never loads the sidebar, offers them too. What choosing one
//! does is the front end's half, `src/lib/paletteShortcuts.ts`.

use std::path::Path;
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::shortcuts::{frecency, pins, searches, tags, workspaces};

use super::frecency::is_scratch;
use super::{exists, now};
use crate::palette::{self, Hit, Item, Palette, Provider, Query};

pub const PINS: &str = "pins";
pub const FREQUENT: &str = "frequent";
pub const SEARCHES: &str = "searches";
pub const WORKSPACES: &str = "workspaces";
pub const TAGS: &str = "tags";

/// The user put their pins in the sidebar, so they rank above anything matching as well.
const PIN_BOOST: i32 = 25;
/// The most frequent folder is lifted this much, the others less by rank.
const FREQUENT_BOOST: i32 = 15;
/// Frequent folders offered: the Frequent list shows six, Go to folder ranks them all.
const FREQUENT_SHOWN: usize = 30;
const SAVED_BOOST: i32 = 10;

/// Registers every shortcuts provider with the palette.
pub fn register(palette: &Palette, db: &Arc<Db>) {
    palette.register(Arc::new(Pins(db.clone())));
    palette.register(Arc::new(Frequent(db.clone())));
    palette.register(Arc::new(Searches(db.clone())));
    palette.register(Arc::new(Workspaces(db.clone())));
    palette.register(Arc::new(Tags(db.clone())));
}

/// What a file or folder is called: its own name, or for a drive root, the root itself.
fn name_of(path: &str) -> String {
    match path.trim_end_matches('\\').rsplit_once('\\') {
        Some((_, name)) if !name.is_empty() => name.to_owned(),
        _ => path.to_owned(),
    }
}

/// The folder holding `path`, as the files provider shows it under a name.
fn parent_of(path: &str) -> Option<String> {
    Path::new(path).parent().and_then(Path::to_str).map(str::to_owned)
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// Logs a database failure and offers nothing, rather than failing the whole query.
fn or_nothing<T>(result: kestrel_db::Result<Vec<T>>, what: &str) -> Vec<T> {
    result.unwrap_or_else(|error| {
        eprintln!("the palette could not read the {what}: {error}");
        Vec::new()
    })
}

fn pin_item(board: &pins::Board, pin: &pins::Pin) -> Item {
    let present = exists(&pin.path);
    Item {
        provider: PINS.into(),
        id: pin.id.to_string(),
        title: pin.alias.clone().unwrap_or_else(|| name_of(&pin.path)),
        subtitle: Some(if present { pin.path.clone() } else { format!("Not found at {}", pin.path) }),
        hint: board.key_of(pin.id).map(|key| format!("Ctrl+{key}")),
        // A missing pin has no file to take an icon from; choosing it looks for it.
        icon: (!present).then(|| "shortcut:missing".into()),
        path: present.then(|| pin.path.clone()),
        dir: pin.dir,
        keywords: Some(pin.path.clone()),
        boost: PIN_BOOST,
    }
}

fn search_item(saved: searches::SavedSearch) -> Item {
    Item {
        provider: SEARCHES.into(),
        id: saved.id.to_string(),
        title: saved.name,
        subtitle: Some(match &saved.scope {
            Some(scope) => format!("In {scope}: {}", saved.query),
            None => format!("Everywhere: {}", saved.query),
        }),
        icon: Some("shortcut:search".into()),
        keywords: Some(saved.query),
        boost: SAVED_BOOST,
        ..Item::default()
    }
}

fn workspace_item(workspace: workspaces::Workspace) -> Item {
    let names: Vec<String> = workspace.tabs.iter().map(|tab| name_of(&tab.path)).collect();
    Item {
        provider: WORKSPACES.into(),
        id: workspace.id.to_string(),
        title: workspace.name,
        subtitle: Some(format!("{}: {}", count(names.len(), "tab", "tabs"), names.join(", "))),
        icon: Some("shortcut:workspace".into()),
        keywords: Some(workspace.tabs.iter().map(|tab| tab.path.as_str()).collect::<Vec<_>>().join(" ")),
        boost: SAVED_BOOST,
        ..Item::default()
    }
}

fn tag_item(tag: tags::Tag, items: usize) -> Item {
    Item {
        provider: TAGS.into(),
        id: tag.id.to_string(),
        title: tag.name,
        subtitle: Some(count(items, "item", "items")),
        icon: Some(format!("tag:{}", tag.color)),
        ..Item::default()
    }
}

struct Pins(Arc<Db>);

impl Pins {
    fn board(&self) -> Option<pins::Board> {
        pins::board(&self.0).map_err(|error| eprintln!("the palette could not read the pins: {error}")).ok()
    }
}

impl Provider for Pins {
    fn id(&self) -> &'static str {
        PINS
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let Some(board) = self.board() else { return Vec::new() };
        palette::rank_items(query, board.pins.iter().map(|pin| pin_item(&board, pin)))
    }

    /// A pin chosen before, under its name and number key now; gone once unpinned.
    fn current(&self, remembered: Item) -> Option<Item> {
        let board = self.board()?;
        let pin = board.pins.iter().find(|pin| pin.id.to_string() == remembered.id)?;
        Some(pin_item(&board, pin))
    }
}

struct Frequent(Arc<Db>);

impl Provider for Frequent {
    fn id(&self) -> &'static str {
        FREQUENT
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let ranked = or_nothing(frecency::ranked(&self.0, now()), "frequent folders");
        let items = ranked
            .into_iter()
            .filter(|folder| !is_scratch(&folder.path) && exists(&folder.path))
            .take(FREQUENT_SHOWN)
            .enumerate()
            .map(|(rank, folder)| Item {
                provider: FREQUENT.into(),
                id: folder.path.clone(),
                title: name_of(&folder.path),
                subtitle: parent_of(&folder.path),
                path: Some(folder.path.clone()),
                dir: true,
                keywords: Some(folder.path),
                boost: FREQUENT_BOOST - (rank * FREQUENT_BOOST as usize / FREQUENT_SHOWN) as i32,
                ..Item::default()
            });
        palette::rank_items(query, items)
    }
}

struct Searches(Arc<Db>);

impl Provider for Searches {
    fn id(&self) -> &'static str {
        SEARCHES
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let saved = or_nothing(searches::list(&self.0), "saved searches");
        palette::rank_items(query, saved.into_iter().map(search_item))
    }

    fn current(&self, remembered: Item) -> Option<Item> {
        searches::get(&self.0, remembered.id.parse().ok()?).ok().map(search_item)
    }
}

struct Workspaces(Arc<Db>);

impl Provider for Workspaces {
    fn id(&self) -> &'static str {
        WORKSPACES
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let saved = or_nothing(workspaces::list(&self.0), "workspaces");
        palette::rank_items(query, saved.into_iter().map(workspace_item))
    }

    fn current(&self, remembered: Item) -> Option<Item> {
        workspaces::get(&self.0, remembered.id.parse().ok()?).ok().map(workspace_item)
    }
}

struct Tags(Arc<Db>);

impl Provider for Tags {
    fn id(&self) -> &'static str {
        TAGS
    }

    fn prefix(&self) -> Option<char> {
        Some('#')
    }

    /// Short tag names would match most of what is typed; they wait for `#`.
    fn in_default(&self) -> bool {
        false
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let all = or_nothing(tags::with_counts(&self.0), "tags");
        palette::rank_items(query, all.into_iter().map(|(tag, items)| tag_item(tag, items)))
    }

    fn current(&self, remembered: Item) -> Option<Item> {
        let all = or_nothing(tags::with_counts(&self.0), "tags");
        all.into_iter().find(|(tag, _)| tag.id.to_string() == remembered.id).map(|(tag, items)| tag_item(tag, items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Request;

    fn ask(palette: &Palette, text: &str) -> Vec<String> {
        let request = Request { text: text.into(), session: "test".into(), items: Vec::new(), show_hidden: false, limit: None };
        palette.query(request).unwrap().hits.into_iter().map(|hit| format!("{}:{}", hit.item.provider, hit.item.id)).collect()
    }

    #[test]
    fn shortcuts_join_every_query_and_tags_answer_their_prefix() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let palette = Palette::new(db.clone());
        register(&palette, &db);
        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let system = format!(r"{windows}\System32");
        let gone = format!(r"{windows}\kestrel-no-such-folder");

        fn target(path: &str) -> pins::Target<'_> {
            pins::Target { path, volume: "c:", file_id: 0, dir: true }
        }
        let (pinned, _) = pins::add(&db, &target(&windows), None).unwrap();
        pins::set_alias(&db, pinned.id, Some("Operating system")).unwrap();
        let (missing, _) = pins::add(&db, &target(&gone), None).unwrap();
        frecency::record(&db, &system, now()).unwrap();
        frecency::record(&db, &std::env::temp_dir().join("kestrel-scratch").to_string_lossy(), now()).unwrap();
        let saved = searches::create(&db, "Big videos", "type:video size:>1gb", None).unwrap();
        let tab = workspaces::TabState { path: windows.clone(), view: None, sort: None };
        let writing = workspaces::save(&db, "Writing", &[tab], 0, now()).unwrap();
        let urgent = tags::create(&db, "Urgent", "red").unwrap();

        assert_eq!(ask(&palette, "operating"), [format!("pins:{}", pinned.id)], "by the pin's own name");
        assert_eq!(ask(&palette, "system32"), [format!("frequent:{system}")]);
        assert!(ask(&palette, "scratch").is_empty(), "scratch folders are never offered");
        assert_eq!(ask(&palette, "big vid"), [format!("searches:{}", saved.id)]);
        assert_eq!(ask(&palette, "video size"), [format!("searches:{}", saved.id)], "by the query saved");
        assert_eq!(ask(&palette, "writing"), [format!("workspaces:{}", writing.id)]);
        assert!(ask(&palette, "urgent").is_empty(), "tags wait for their prefix");
        assert_eq!(ask(&palette, "#urg"), [format!("tags:{}", urgent.id)]);
        assert_eq!(ask(&palette, "#"), [format!("tags:{}", urgent.id)], "every tag under the bare prefix");

        let request = Request { text: "kestrel-no-such".into(), session: "t".into(), items: Vec::new(), show_hidden: false, limit: None };
        let hits = palette.query(request).unwrap().hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item.id, missing.id.to_string());
        assert_eq!(hits[0].item.path, None, "a missing pin has no file to show");
        assert_eq!(hits[0].item.icon.as_deref(), Some("shortcut:missing"));
        assert_eq!(hits[0].item.hint.as_deref(), Some("Ctrl+2"), "numbered in sidebar order");
    }

    #[test]
    fn what_was_chosen_lately_shows_as_it_is_now_and_not_once_deleted() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let palette = Palette::new(db.clone());
        register(&palette, &db);
        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let (pin, _) = pins::add(&db, &pins::Target { path: &windows, volume: "c:", file_id: 0, dir: true }, None).unwrap();
        let saved = searches::create(&db, "Big videos", "type:video size:>1gb", None).unwrap();
        let tab = workspaces::TabState { path: windows.clone(), view: None, sort: None };
        let writing = workspaces::save(&db, "Writing", &[tab], 0, now()).unwrap();
        for (provider, id, title) in [(PINS, pin.id, "Windows"), (SEARCHES, saved.id, "Big videos"), (WORKSPACES, writing.id, "Writing")] {
            let used = kestrel_db::palette::Use { provider: provider.into(), id: id.to_string(), title: title.into(), ..Default::default() };
            palette.record(&used).unwrap();
        }
        pins::set_alias(&db, pin.id, Some("Operating system")).unwrap();
        searches::delete(&db, saved.id).unwrap();

        let request = Request { text: String::new(), session: "t".into(), items: Vec::new(), show_hidden: false, limit: None };
        let mut hits = palette.query(request).unwrap().hits;
        hits.sort_by(|a, b| a.item.provider.cmp(&b.item.provider));
        let shown: Vec<(&str, &str)> = hits.iter().map(|hit| (hit.item.provider.as_str(), hit.item.title.as_str())).collect();
        assert_eq!(shown, [(PINS, "Operating system"), (WORKSPACES, "Writing")], "the deleted search left out");
        assert_eq!(hits[0].item.hint.as_deref(), Some("Ctrl+1"), "with what was not remembered");
        assert_eq!(hits[1].item.icon.as_deref(), Some("shortcut:workspace"));
    }

    #[test]
    fn names_of_roots_and_folders() {
        assert_eq!(name_of(r"C:\"), r"C:\");
        assert_eq!(name_of(r"C:\Users\me\"), "me");
        assert_eq!(name_of(r"\\server\share"), "share");
        assert_eq!(parent_of(r"C:\Users\me").as_deref(), Some(r"C:\Users"));
        assert_eq!(count(1, "tab", "tabs"), "1 tab");
        assert_eq!(count(3, "tab", "tabs"), "3 tabs");
    }
}
