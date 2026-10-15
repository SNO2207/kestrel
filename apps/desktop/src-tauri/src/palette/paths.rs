//! Typing a path after `/`: the folders and files of the folder typed so far.
//!
//! `/C:\Users\me\Doc` lists the entries of `C:\Users\me` that match `Doc`, read from disk so
//! that the list is always current. `/` alone, or a start with no separator yet (`/dow`),
//! offers places to start from: drives, the user's folders and remembered folders, matched
//! against their whole paths. `~` stands for the user's folder, and `/` works as a separator.
//! Tab in the palette completes the chosen entry into the query (the front end does that).
//!
//! The items it returns are files and folders like any others, so they carry the `files`
//! provider's id: the front end opens them, and the palette remembers them, the same way.

use std::time::{Duration, Instant};

use kestrel_index::{attr, natural_cmp, walk};
use parking_lot::Mutex;

use super::files;
use super::{Hit, Item, Provider, Query, Scorer};
use crate::engine::normalize;
use crate::places;
use crate::visibility::Visibility;

/// How long the list of drives and user folders is reused: reading drive labels can be slow.
const PLACES_FRESH: Duration = Duration::from_secs(30);
/// Remembered folders offered with places.
const REMEMBERED_SHOWN: usize = 12;

/// A place to start from.
#[derive(Clone)]
struct Place {
    path: String,
    label: String,
}

#[derive(Default)]
pub struct PathsProvider {
    places: Mutex<Option<(Instant, Vec<Place>, String)>>,
}

impl Provider for PathsProvider {
    fn id(&self) -> &'static str {
        "paths"
    }

    fn prefix(&self) -> Option<char> {
        Some('/')
    }

    fn in_default(&self) -> bool {
        false
    }

    fn search(&self, query: &Query) -> Vec<Hit> {
        let (places, home) = self.places();
        let text = expand(query.text, &home);
        let hits = match split(&text) {
            Some((dir, rest)) => children(&dir, rest, query),
            None => starting_points(&text, places, query),
        };
        hits.into_iter().take(query.limit).collect()
    }
}

impl PathsProvider {
    fn places(&self) -> (Vec<Place>, String) {
        let mut cached = self.places.lock();
        if let Some((read, places, home)) = cached.as_ref()
            && read.elapsed() < PLACES_FRESH
        {
            return (places.clone(), home.clone());
        }
        let found = places::places();
        let mut list: Vec<Place> = found.drives.iter().map(|drive| Place { path: drive.path.clone(), label: drive_label(drive) }).collect();
        if !found.home.is_empty() {
            list.push(Place { path: found.home.clone(), label: "Home".into() });
        }
        list.extend(found.folders.iter().map(|folder| Place { path: folder.path.clone(), label: folder.name.into() }));
        *cached = Some((Instant::now(), list.clone(), found.home.clone()));
        (list, found.home)
    }
}

fn drive_label(drive: &places::Drive) -> String {
    match (drive.label.is_empty(), drive.kind) {
        (false, _) => drive.label.clone(),
        (true, "network") => "Network drive".into(),
        (true, "removable") => "Removable drive".into(),
        (true, "optical") => "Optical drive".into(),
        (true, _) => "Local disk".into(),
    }
}

/// Puts `\` for `/` and the user's folder for a leading `~`.
pub fn expand(text: &str, home: &str) -> String {
    let text = text.trim().replace('/', "\\");
    match text.strip_prefix('~') {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('\\')) => {
            format!("{}{rest}", home.trim_end_matches('\\'))
        }
        _ => text,
    }
}

/// Splits a typed path into the folder to list and the start of a name in it. `None` when
/// no folder has been typed yet.
pub fn split(text: &str) -> Option<(String, &str)> {
    if let Some(at) = text.rfind('\\') {
        return Some((text[..=at].to_owned(), &text[at + 1..]));
    }
    // `C:` is a drive, whose folder is its root.
    let bytes = text.as_bytes();
    (bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':').then(|| (format!("{text}\\"), ""))
}

fn hit(path: String, title: String, subtitle: Option<String>, dir: bool, matched: u32, highlights: Vec<u32>) -> Hit {
    let item = Item { provider: files::ID.into(), id: path.clone(), title, subtitle, path: Some(path), dir, ..Item::default() };
    Hit::new(item, matched, highlights)
}

/// The folder itself, then its entries that match `rest`: all of them, folders first and in
/// name order, when nothing has been typed after the separator.
fn children(dir: &str, rest: &str, query: &Query) -> Vec<Hit> {
    let dir = normalize(dir);
    let Ok(mut items) = walk::list(&dir) else { return Vec::new() };
    let visibility = Visibility { hidden: query.show_hidden };
    items.retain(|item| visibility.shows(item.meta.attrs));
    let is_dir = |attrs: u32| attrs & attr::DIRECTORY != 0;
    let join = |name: &str| if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") };

    let mut scorer = Scorer::new(rest);
    if scorer.is_empty() {
        items.sort_by(|a, b| {
            is_dir(b.meta.attrs)
                .cmp(&is_dir(a.meta.attrs))
                .then_with(|| natural_cmp(kestrel_index::fold(&a.name).as_bytes(), kestrel_index::fold(&b.name).as_bytes()))
        });
        let mut hits = vec![hit(dir.clone(), dir.clone(), Some("This folder".into()), true, 0, Vec::new())];
        hits.extend(
            items.iter().take(query.limit).map(|item| hit(join(&item.name), item.name.clone(), Some(dir.clone()), is_dir(item.meta.attrs), 0, Vec::new())),
        );
        for (order, hit) in hits.iter_mut().enumerate() {
            hit.order = order;
            hit.listed = true;
        }
        return hits;
    }
    let mut hits: Vec<Hit> = items
        .iter()
        .filter_map(|item| {
            let score = scorer.score(&item.name)?;
            let dir_entry = is_dir(item.meta.attrs);
            let path = join(&item.name);
            let matched = files::refine(score, &item.name, rest, &path, dir_entry);
            Some(hit(path, item.name.clone(), Some(dir.clone()), dir_entry, matched, Vec::new()))
        })
        .collect();
    query.keep_best(&mut hits);
    for hit in &mut hits {
        hit.highlights = scorer.highlights(&hit.item.title);
    }
    hits
}

/// Drives, the user's folders and remembered folders, matched against their whole paths.
fn starting_points(text: &str, places: Vec<Place>, query: &Query) -> Vec<Hit> {
    let remembered = query
        .recents
        .rows()
        .iter()
        .filter(|row| row.item.provider == files::ID && row.item.dir)
        .filter_map(|row| row.item.path.clone())
        .filter(|path| !places.iter().any(|place| place.path.eq_ignore_ascii_case(path)))
        .take(REMEMBERED_SHOWN)
        .map(|path| Place { label: "Recent folder".into(), path })
        .collect::<Vec<_>>();
    let mut scorer = Scorer::for_paths(text);
    let mut hits: Vec<Hit> = places
        .into_iter()
        .chain(remembered)
        .enumerate()
        .filter_map(|(order, place)| {
            let matched = scorer.score(&place.path)?;
            let highlights = scorer.highlights(&place.path);
            let mut hit = hit(place.path.clone(), place.path, Some(place.label), true, matched, highlights);
            hit.order = order;
            Some(hit)
        })
        .collect();
    hits.sort_by(|a, b| b.matched.cmp(&a.matched).then(a.order.cmp(&b.order)));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Recents;
    use std::fs;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn typed_paths_split_into_folder_and_name() {
        assert_eq!(split(r"C:\Users\me\Doc"), Some((r"C:\Users\me\".into(), "Doc")));
        assert_eq!(split(r"C:\"), Some((r"C:\".into(), "")));
        assert_eq!(split("c:"), Some((r"c:\".into(), "")));
        assert_eq!(split("docs"), None);
        assert_eq!(split(""), None);
        assert_eq!(expand(" ~/Projects/kes ", r"C:\Users\me"), r"C:\Users\me\Projects\kes");
        assert_eq!(expand("~", r"C:\Users\me\"), r"C:\Users\me");
        assert_eq!(expand("~notes", r"C:\Users\me"), "~notes", "only ~ on its own is the user's folder");
        assert_eq!(expand("C:/Windows", ""), r"C:\Windows");
    }

    fn search(text: &str) -> Vec<Hit> {
        let recents = Recents::new(Vec::new(), 0);
        let cancel = AtomicBool::new(false);
        let query = Query { text, scope: Some('/'), show_hidden: false, limit: 50, recents: &recents, cancel: &cancel };
        PathsProvider::default().search(&query)
    }

    #[test]
    fn a_typed_folder_lists_its_entries_and_filters_them_by_the_name_typed() {
        let root = std::env::temp_dir().join(format!("kestrel-palette-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Beta folder")).unwrap();
        fs::create_dir_all(root.join("alpha folder")).unwrap();
        fs::write(root.join("alpha.txt"), b"a").unwrap();
        fs::write(root.join("gamma.txt"), b"g").unwrap();
        let base = root.to_str().unwrap();

        let listed = search(&format!(r"{base}\"));
        let titles: Vec<&str> = listed.iter().map(|hit| hit.item.title.as_str()).collect();
        assert_eq!(titles, [base, "alpha folder", "Beta folder", "alpha.txt", "gamma.txt"], "the folder, then folders first");
        assert!(listed.iter().all(|hit| hit.item.provider == "files"));
        assert_eq!(listed[1].item.path.as_deref(), Some(format!(r"{base}\alpha folder").as_str()));

        let filtered = search(&format!("{}/alp", base.replace('\\', "/")));
        let titles: Vec<&str> = filtered.iter().map(|hit| hit.item.title.as_str()).collect();
        assert_eq!(titles.len(), 2, "{titles:?}");
        assert_eq!(filtered[0].item.title, "alpha folder", "a folder ranks first on an equal match");
        assert_eq!(filtered[0].highlights, [0, 1, 2]);
        assert!(search(&format!(r"{base}\nothing-like-this")).is_empty());
        assert!(search(r"Q:\no\such\folder\").is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_no_folder_typed_drives_and_places_are_offered() {
        let all = search("");
        assert!(all.iter().any(|hit| hit.item.title.eq_ignore_ascii_case(r"C:\")), "the system drive");
        let windows = search("c:");
        assert_eq!(windows[0].item.title.to_ascii_uppercase(), r"C:\", "a drive letter lists that drive");
    }
}
