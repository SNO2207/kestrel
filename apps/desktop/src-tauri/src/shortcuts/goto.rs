//! Go to folder (Ctrl+G): a small fuzzy finder over the folders the user visits most, then
//! over every folder in the index.
//!
//! The last word typed is matched fuzzily against a folder's name: its letters in order, with
//! runs, the start of the name and the starts of words counting for more. Any earlier words,
//! and any word that looks like part of a path, must appear in the folder's full path, as
//! zoxide's keywords do. So `proj kes` finds `C:\Projects\Kestrel`, and so does `kstrl`.

use std::path::Path;
use std::sync::Arc;

use kestrel_db::shortcuts::frecency;
use kestrel_index::{EntryId, Index, attr, fold};
use rayon::prelude::*;
use serde::Serialize;
use tauri::State;

use super::{Data, exists, now};
use crate::engine::Engine;

/// Entries per parallel work unit when scanning the index.
const CHUNK: u32 = 65_536;
/// Most results shown.
const MAX_RESULTS: usize = 60;
/// How many starting points a fuzzy match tries; enough for any real name.
const MAX_STARTS: usize = 12;

/// What was typed, split into the parts that are matched differently.
#[derive(Debug, Default, PartialEq)]
pub struct Pattern {
    /// Folded words every match's full path must contain.
    path_words: Vec<String>,
    /// Folded word matched fuzzily against the folder's own name.
    name: Option<String>,
}

impl Pattern {
    pub fn new(text: &str) -> Self {
        let mut pattern = Pattern::default();
        let words: Vec<String> = text.split_whitespace().map(|word| fold(&word.replace('/', "\\"))).collect();
        // The last word that isn't a piece of a path names the folder; everything else must
        // be somewhere in its path.
        let name_at = words.iter().rposition(|word| !word.contains(['\\', ':']));
        for (at, word) in words.into_iter().enumerate() {
            if Some(at) == name_at {
                pattern.name = Some(word);
            } else {
                pattern.path_words.push(word);
            }
        }
        pattern
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.path_words.is_empty()
    }

    /// How well a folder matches, given its folded name and a way to get its folded full path
    /// (needed only when there are path words). `None` if it doesn't.
    fn score(&self, name: &[u8], path: impl FnOnce() -> Vec<u8>) -> Option<i32> {
        let named = match &self.name {
            Some(word) => fuzzy(word.as_bytes(), name)?,
            None => 0,
        };
        if !self.path_words.is_empty() {
            let path = path();
            if !self.path_words.iter().all(|word| contains(&path, word.as_bytes())) {
                return None;
            }
        }
        Some(named)
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window == needle)
}

fn is_boundary(byte: u8) -> bool {
    matches!(byte, b' ' | b'-' | b'_' | b'.' | b'\\' | b'(' | b'[')
}

/// Scores `needle` as a subsequence of `haystack`, both folded; `None` if it isn't one. Runs
/// of consecutive letters, a match at the very start and matches at word starts score more;
/// gaps and length cost a little.
pub fn fuzzy(needle: &[u8], haystack: &[u8]) -> Option<i32> {
    let Some(&first) = needle.first() else { return Some(0) };
    let mut best: Option<i32> = None;
    for start in haystack.iter().enumerate().filter(|&(_, &byte)| byte == first).map(|(at, _)| at).take(MAX_STARTS) {
        let Some(score) = fuzzy_from(needle, haystack, start) else { break };
        best = Some(best.map_or(score, |best| best.max(score)));
    }
    // The whole name typed is worth more the longer it is: one letter says little.
    let exact = if needle == haystack { (needle.len() as i32 * 24).min(200) } else { 0 };
    best.map(|score| score * 8 - haystack.len() as i32 + exact)
}

/// A greedy match of `needle` in `haystack` with its first letter at `start`.
fn fuzzy_from(needle: &[u8], haystack: &[u8], start: usize) -> Option<i32> {
    let mut score = 0;
    let mut previous: Option<usize> = None;
    let mut from = start;
    for &byte in needle {
        let at = from + haystack[from..].iter().position(|&candidate| candidate == byte)?;
        score += 1;
        match previous {
            Some(before) if at == before + 1 => score += 8,
            Some(before) => score -= (at - before - 1).min(8) as i32,
            None => {}
        }
        if at == 0 {
            score += 12;
        } else if is_boundary(haystack[at - 1]) {
            score += 6;
        }
        previous = Some(at);
        from = at + 1;
    }
    Some(score)
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub path: String,
    /// Among the frequent and recent folders, rather than only in the index.
    pub frecent: bool,
}

/// Folders of the index matching `pattern`, best first, at most `limit`.
pub fn search_index(index: &Index, pattern: &Pattern, limit: usize) -> Vec<String> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let ranges: Vec<(EntryId, EntryId)> =
        (0..index.len()).step_by(CHUNK as usize).map(|start| (start, (start + CHUNK).min(index.len()))).collect();
    // Each chunk keeps only its best few, so a one-letter query over millions stays cheap.
    let keep = limit * 2;
    let mut hits: Vec<(i32, EntryId)> = ranges
        .into_par_iter()
        .flat_map_iter(|(start, end)| {
            let mut found: Vec<(i32, EntryId)> = (start..end)
                .filter(|&id| index.is_dir(id) && !index.is_deleted(id))
                .filter_map(|id| {
                    let score = pattern.score(index.lower_bytes(id), || {
                        let mut path = Vec::new();
                        index.write_lower_path(id, &mut path);
                        path
                    })?;
                    // Hidden and system folders are rarely where anyone means to go.
                    let hidden = index.attrs(id) & (attr::HIDDEN | attr::SYSTEM) != 0;
                    Some((score - if hidden { 40 } else { 0 }, id))
                })
                .collect();
            found.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            found.truncate(keep);
            found
        })
        .collect();
    // Among equals, shallower folders first: `C:\Projects` before `C:\old\backup\Projects`.
    let mut ranked: Vec<(i32, String)> = {
        hits.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        hits.truncate(keep);
        hits.into_iter()
            .map(|(score, id)| {
                let path = index.path(id);
                let depth = path.matches('\\').count() as i32;
                (score - depth * 5, path)
            })
            .collect()
    };
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().take(limit).map(|(_, path)| path).collect()
}

/// The frequent and recent folders matching `pattern`, best first: how well each matches and
/// how frecent it is both count.
pub fn search_frecent(ranked: &[frecency::Ranked], pattern: &Pattern) -> Vec<String> {
    let mut hits: Vec<(f64, &str)> = ranked
        .iter()
        .filter_map(|folder| {
            let folded = fold(&folder.path);
            let name = folded.trim_end_matches('\\').rsplit('\\').next().unwrap_or(&folded);
            let matched = pattern.score(name.as_bytes(), || folded.clone().into_bytes())?;
            Some((f64::from(matched) + folder.score.ln_1p() * 24.0, folder.path.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits.into_iter().map(|(_, path)| path.to_owned()).collect()
}

/// A typed absolute path to a folder that exists, such as `C:\Windows`, which is offered first.
fn typed_folder(text: &str) -> Option<String> {
    let text = text.trim();
    let bytes = text.as_bytes();
    let absolute = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    (absolute && Path::new(text).is_dir()).then(|| crate::engine::normalize(text))
}

/// Folders for Go to folder: with nothing typed, the most frecent; otherwise matches among the
/// frecent folders first, then the rest of the index's folders.
#[tauri::command(async)]
pub fn goto_folders(db: Data, engine: State<'_, Arc<Engine>>, query: String, limit: Option<usize>) -> Result<Vec<Hit>, String> {
    let limit = limit.unwrap_or(MAX_RESULTS).min(MAX_RESULTS);
    let pattern = Pattern::new(&query);
    let ranked = frecency::ranked(&db, now())?;
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |hits: &mut Vec<Hit>, path: String, frecent: bool| {
        if hits.len() < limit && seen.insert(path.to_lowercase()) {
            hits.push(Hit { path, frecent });
        }
    };
    if let Some(path) = typed_folder(&query) {
        push(&mut hits, path, false);
    }
    let frecent: Vec<String> = if pattern.is_empty() {
        ranked.iter().map(|folder| folder.path.clone()).collect()
    } else {
        search_frecent(&ranked, &pattern)
    };
    for path in frecent {
        if hits.len() >= limit {
            break;
        }
        if !super::frecency::is_scratch(&path) && exists(&path) {
            push(&mut hits, path, true);
        }
    }
    if !pattern.is_empty() && hits.len() < limit {
        let found = search_index(&engine.index().read(), &pattern, limit);
        for path in found {
            push(&mut hits, path, false);
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta};

    fn dir() -> Meta {
        Meta { attrs: attr::DIRECTORY, ..Meta::default() }
    }

    fn sample() -> Index {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let projects = b.push(root, "Projects", dir());
        b.push(projects, "Kestrel", dir());
        b.push(projects, "kestrel-notes.txt", Meta::default());
        let old = b.push(root, "old", dir());
        let backup = b.push(old, "backup", dir());
        b.push(backup, "Projects", dir());
        b.push(backup, "Kestrel Old", dir());
        b.push(root, "Music", dir());
        b.push(root, "$Recycle.Bin", Meta { attrs: attr::DIRECTORY | attr::HIDDEN | attr::SYSTEM, ..Meta::default() });
        b.finish()
    }

    #[test]
    fn fuzzy_matches_prefer_runs_and_word_starts() {
        assert!(fuzzy(b"kstrl", b"kestrel").is_some());
        assert!(fuzzy(b"xyz", b"kestrel").is_none());
        assert!(fuzzy(b"lk", b"kestrel").is_none(), "letters must come in order");
        // A prefix beats letters scattered through a name.
        assert!(fuzzy(b"kes", b"kestrel").unwrap() > fuzzy(b"kes", b"keys and stuff").unwrap());
        // Word starts count: "dp" fits "Data Projects" better than "dumpster".
        assert!(fuzzy(b"dp", b"data projects").unwrap() > fuzzy(b"dp", b"dumpster").unwrap());
        // The best starting point is found, not just the first.
        assert!(fuzzy(b"rel", b"r-kestrel").unwrap() > fuzzy(b"rel", b"r-kestxyzl").unwrap_or(i32::MIN));
        // The exact name wins outright.
        assert!(fuzzy(b"music", b"music").unwrap() > fuzzy(b"music", b"musical").unwrap());
        assert_eq!(fuzzy(b"", b"anything"), Some(0));
    }

    #[test]
    fn patterns_split_names_from_path_words() {
        assert_eq!(Pattern::new("proj KES"), Pattern { path_words: vec!["proj".into()], name: Some("kes".into()) });
        assert_eq!(Pattern::new(r"kes c:\old"), Pattern { path_words: vec![r"c:\old".into()], name: Some("kes".into()) });
        assert_eq!(Pattern::new("C:/Projects"), Pattern { path_words: vec![r"c:\projects".into()], name: None });
        assert!(Pattern::new("   ").is_empty());
    }

    #[test]
    fn the_index_is_searched_for_folders_only() {
        let index = sample();
        let found = search_index(&index, &Pattern::new("kes"), 10);
        // Shallower first; the text file is not a folder.
        assert_eq!(found, [r"C:\Projects\Kestrel", r"C:\old\backup\Kestrel Old"]);
        assert_eq!(search_index(&index, &Pattern::new("kstrl"), 10)[0], r"C:\Projects\Kestrel");
        // Earlier words narrow by path.
        assert_eq!(search_index(&index, &Pattern::new("backup proj"), 10), [r"C:\old\backup\Projects"]);
        assert_eq!(search_index(&index, &Pattern::new("proj"), 10), [r"C:\Projects", r"C:\old\backup\Projects"]);
        // Hidden system folders sink below ordinary ones.
        let bins = search_index(&index, &Pattern::new("b"), 10);
        assert_eq!(bins.last().map(String::as_str), Some(r"C:\$Recycle.Bin"));
        assert!(search_index(&index, &Pattern::new("zzz"), 10).is_empty());
        assert!(search_index(&index, &Pattern::new(""), 10).is_empty());
        assert_eq!(search_index(&index, &Pattern::new("o"), 2).len(), 2);
    }

    #[test]
    fn frecent_folders_rank_by_match_and_frecency() {
        let folder = |path: &str, score: f64| frecency::Ranked { path: path.into(), score, rank: score, visits: 1, last_visit: 0 };
        let ranked = [folder(r"C:\Users\me\Downloads", 400.0), folder(r"C:\Projects\Kestrel", 40.0), folder(r"D:\Kestrel backup", 1.0)];
        assert_eq!(search_frecent(&ranked, &Pattern::new("kes")), [r"C:\Projects\Kestrel", r"D:\Kestrel backup"]);
        assert_eq!(search_frecent(&ranked, &Pattern::new("dl")), [r"C:\Users\me\Downloads"]);
        assert_eq!(search_frecent(&ranked, &Pattern::new(r"d:\ kes")), [r"D:\Kestrel backup"]);
        assert!(search_frecent(&ranked, &Pattern::new("music")).is_empty());
    }

    #[test]
    fn typed_paths_are_offered_when_they_are_folders() {
        let windows = std::env::var("SystemRoot").unwrap();
        assert_eq!(typed_folder(&format!("{windows}\\")).as_deref(), Some(windows.as_str()));
        assert_eq!(typed_folder(r"C:\no\such\folder\here"), None);
        assert_eq!(typed_folder("windows"), None);
    }
}
