use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use kestrel_index::{EntryId, Index, NO_ENTRY};
use memchr::memmem::Finder;
use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};

use crate::glob;
use crate::parse::{ClauseKind, ParseError, Query};

/// Entries per parallel work unit. Small enough to balance across cores and to make
/// cancellation prompt; large enough that scheduling overhead is negligible.
const CHUNK: u32 = 32_768;

/// One bit per interned extension id.
struct ExtSet(Box<[u64; 1024]>);

impl ExtSet {
    fn contains(&self, ext: u16) -> bool {
        self.0[ext as usize / 64] >> (ext % 64) & 1 != 0
    }
}

enum Test {
    Name(Finder<'static>),
    Path(Finder<'static>),
    Glob(Vec<u8>),
    Regex(Regex),
    Ext(ExtSet),
    Size(u64, u64),
    Modified(i64, i64),
    Folders,
    Files,
    Under(EntryId),
    /// One of these entries, sorted: a resolved tag.
    Among(Vec<EntryId>),
    /// A filter that can match nothing, such as an extension that no file has.
    Never,
}

impl Test {
    /// Rough relative cost, so that cheap tests reject candidates before expensive ones run.
    fn cost(&self) -> u8 {
        match self {
            Test::Never => 0,
            Test::Folders | Test::Files | Test::Size(..) | Test::Modified(..) | Test::Ext(_) => 1,
            Test::Under(_) | Test::Among(_) => 2,
            Test::Name(_) => 3,
            Test::Glob(_) => 4,
            Test::Regex(_) => 5,
            Test::Path(_) => 6,
        }
    }
}

struct Clause {
    negate: bool,
    test: Test,
}

/// A literal that every match must contain in its folded name, searched for across the whole
/// name arena at once.
struct Anchor {
    finder: Finder<'static>,
    /// The clause that a hit fully proves, which then need not be re-tested.
    proves: Option<usize>,
}

/// Builds folded full paths for path clauses. Entries are scanned in id order, where siblings
/// are adjacent, so the parent's path is kept and only the name is replaced.
#[derive(Default)]
struct PathScratch {
    buffer: Vec<u8>,
    /// The directory whose path, with trailing separator, occupies `buffer[..prefix_len]`.
    parent: Option<EntryId>,
    prefix_len: usize,
}

impl PathScratch {
    fn lower_path(&mut self, index: &Index, id: EntryId) -> &[u8] {
        let parent = index.parent(id);
        if parent == NO_ENTRY {
            self.parent = None;
            self.buffer.clear();
            index.write_lower_path(id, &mut self.buffer);
            return &self.buffer;
        }
        if self.parent != Some(parent) {
            self.buffer.clear();
            index.write_lower_path(parent, &mut self.buffer);
            if self.buffer.last() != Some(&b'\\') {
                self.buffer.push(b'\\');
            }
            self.parent = Some(parent);
            self.prefix_len = self.buffer.len();
        }
        self.buffer.truncate(self.prefix_len);
        self.buffer.extend_from_slice(index.lower_bytes(id));
        &self.buffer
    }
}

/// A query bound to a particular index and ready to run.
pub struct Compiled {
    clauses: Vec<Clause>,
    anchor: Option<Anchor>,
    /// Folder that results are restricted to, when the query names one.
    scope: Option<EntryId>,
    /// Every possible match, when the query names a set of entries (a tag): then only those
    /// are tested, rather than the whole index.
    candidates: Option<Vec<EntryId>>,
}

/// Binds a parsed query to `index`, resolving extensions and folders to ids.
pub fn compile(query: &Query, index: &Index) -> Result<Compiled, ParseError> {
    let mut clauses = Vec::with_capacity(query.clauses.len());
    let mut scope = None;
    let mut candidates: Option<Vec<EntryId>> = None;
    for clause in &query.clauses {
        let test = match &clause.kind {
            ClauseKind::Name(text) => Test::Name(Finder::new(text.as_bytes()).into_owned()),
            ClauseKind::Path(text) => Test::Path(Finder::new(text.as_bytes()).into_owned()),
            ClauseKind::Glob(pattern) => Test::Glob(pattern.clone().into_bytes()),
            ClauseKind::Regex(pattern) => Test::Regex(
                RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| ParseError(format!("Invalid regex: {e}")))?,
            ),
            ClauseKind::Ext(exts) => {
                let mut set = ExtSet(Box::new([0; 1024]));
                let mut any = false;
                for id in exts.iter().filter_map(|e| index.ext_id(e)) {
                    set.0[id as usize / 64] |= 1 << (id % 64);
                    any = true;
                }
                if any { Test::Ext(set) } else { Test::Never }
            }
            &ClauseKind::Size(min, max) => Test::Size(min, max),
            &ClauseKind::Modified(from, to) => Test::Modified(from, to),
            ClauseKind::Folders => Test::Folders,
            ClauseKind::Files => Test::Files,
            ClauseKind::Within(path) => match index.lookup(path) {
                Some(dir) => {
                    if !clause.negate {
                        scope = Some(dir);
                    }
                    Test::Under(dir)
                }
                None => Test::Never,
            },
            ClauseKind::Tag(name) => {
                return Err(ParseError(format!("Tags must be resolved before searching (tag:{name})")));
            }
            ClauseKind::Like(description) => {
                return Err(ParseError(format!("Descriptions must be resolved before searching (like:{description})")));
            }
            ClauseKind::Entries(ids) => {
                if !clause.negate && candidates.as_ref().is_none_or(|known: &Vec<EntryId>| ids.len() < known.len()) {
                    candidates = Some(ids.clone());
                }
                Test::Among(ids.clone())
            }
        };
        clauses.push(Clause { negate: clause.negate, test });
    }
    clauses.sort_by_key(|c| c.test.cost());

    // The anchor is the longest literal that every match must contain in its name.
    let mut anchor: Option<(&[u8], Option<usize>)> = None;
    for (i, clause) in clauses.iter().enumerate().filter(|(_, c)| !c.negate) {
        let candidate: (&[u8], Option<usize>) = match &clause.test {
            Test::Name(finder) => (finder.needle(), Some(i)),
            Test::Glob(pattern) => (glob::longest_literal(pattern), None),
            // A path clause can match through an ancestor's name, so it says nothing about the
            // entry's own name.
            _ => continue,
        };
        if candidate.0.len() > anchor.map_or(0, |a| a.0.len()) {
            anchor = Some(candidate);
        }
    }
    let anchor = anchor
        .map(|(literal, proves)| Anchor { finder: Finder::new(literal).into_owned(), proves });
    Ok(Compiled { clauses, anchor, scope, candidates })
}

impl Compiled {
    /// Whether `id` satisfies every clause except `skip`.
    fn matches(
        &self,
        index: &Index,
        id: EntryId,
        skip: Option<usize>,
        path: &mut PathScratch,
    ) -> bool {
        if index.is_deleted(id) {
            return false;
        }
        self.clauses.iter().enumerate().all(|(i, clause)| {
            if skip == Some(i) {
                return true;
            }
            let hit = match &clause.test {
                Test::Name(finder) => finder.find(index.lower_bytes(id)).is_some(),
                Test::Path(finder) => finder.find(path.lower_path(index, id)).is_some(),
                Test::Glob(pattern) => glob::matches(pattern, index.lower_bytes(id)),
                Test::Regex(regex) => regex.is_match(index.name_bytes(id)),
                Test::Ext(set) => set.contains(index.ext(id)),
                &Test::Size(min, max) => (min..=max).contains(&index.size(id)),
                &Test::Modified(from, to) => (from..to).contains(&index.mtime(id)),
                Test::Folders => index.is_dir(id),
                Test::Files => !index.is_dir(id),
                &Test::Under(dir) => index.is_under(id, dir),
                Test::Among(ids) => ids.binary_search(&id).is_ok(),
                Test::Never => false,
            };
            hit != clause.negate
        })
    }

    /// The id ranges that can contain matches.
    fn ranges(&self, index: &Index) -> Vec<Range<EntryId>> {
        let all = 0..index.len();
        match self.scope.and_then(|dir| index.subtree_range(dir)) {
            // A folder's descendants are contiguous, apart from entries added since the build.
            Some(subtree) => vec![subtree, index.base_len()..index.len()],
            None => vec![all],
        }
    }

    fn scan(&self, index: &Index, range: Range<EntryId>) -> Vec<EntryId> {
        let mut out = Vec::new();
        let mut path = PathScratch::default();
        let Some(anchor) = &self.anchor else {
            out.extend(range.filter(|&id| self.matches(index, id, None, &mut path)));
            return out;
        };

        // Search the folded names of the whole chunk as one haystack, then map each hit back
        // to the entry containing it. Names are NUL-separated and needles never contain NUL,
        // so a hit cannot straddle two names.
        let offsets = index.name_offsets();
        let base = offsets[range.start as usize] as usize;
        let haystack = &index.lower_arena()[base..offsets[range.end as usize] as usize];
        let mut next = range.start;
        let mut position = 0;
        while let Some(found) = anchor.finder.find(&haystack[position..]) {
            let hit = (base + position + found) as u32;
            let candidates = &offsets[next as usize..range.end as usize];
            let id = next + candidates.partition_point(|&start| start <= hit) as u32 - 1;
            if self.matches(index, id, anchor.proves, &mut path) {
                out.push(id);
            }
            next = id + 1;
            if next >= range.end {
                break;
            }
            position = offsets[next as usize] as usize - base;
        }
        out
    }
}

/// Runs a query and returns matching entries in ascending id order, or `None` if `cancel` was
/// set while it ran.
pub fn search(index: &Index, query: &Compiled, cancel: &AtomicBool) -> Option<Vec<EntryId>> {
    // A tag names its few entries outright; testing just those beats scanning millions.
    if let Some(candidates) = &query.candidates {
        let mut path = PathScratch::default();
        let found = candidates
            .iter()
            .copied()
            .filter(|&id| id < index.len() && query.matches(index, id, None, &mut path))
            .collect();
        return (!cancel.load(Ordering::Relaxed)).then_some(found);
    }
    let chunks: Vec<Range<EntryId>> = query
        .ranges(index)
        .into_iter()
        .flat_map(|range| {
            let end = range.end;
            range.step_by(CHUNK as usize).map(move |start| start..end.min(start + CHUNK))
        })
        .collect();
    let parts: Vec<Vec<EntryId>> = chunks
        .into_par_iter()
        .map(|chunk| {
            if cancel.load(Ordering::Relaxed) { Vec::new() } else { query.scan(index, chunk) }
        })
        .collect();
    (!cancel.load(Ordering::Relaxed)).then(|| parts.concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Clock;
    use kestrel_index::{IndexBuilder, Meta, attr};

    const CLOCK: Clock = Clock { now_unix: 1_800_000_000, utc_offset_secs: 0 };

    fn file(size: u64) -> Meta {
        Meta { size, alloc: size, mtime: crate::time::unix_to_filetime(1_799_999_000), ..Meta::default() }
    }

    fn dir() -> Meta {
        Meta { attrs: attr::DIRECTORY, ..Meta::default() }
    }

    fn sample() -> Index {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let projects = b.push(root, "Projects", dir());
        let kestrel = b.push(projects, "Kestrel", dir());
        let src = b.push(kestrel, "src", dir());
        b.push(src, "main.rs", file(1200));
        b.push(src, "lib.rs", file(800));
        b.push(kestrel, "Cargo.toml", file(300));
        b.push(kestrel, "README.md", file(5000));
        let photos = b.push(root, "Photos", dir());
        b.push(photos, "IMG_0001.JPG", file(3 << 20));
        b.push(photos, "IMG_0002.jpg", file(4 << 20));
        b.push(photos, "holiday video.MP4", file(2 << 30));
        b.push(photos, "main street.png", Meta { mtime: 1, ..file(900) });
        b.finish()
    }

    fn run(index: &Index, text: &str) -> Vec<String> {
        let query = compile(&crate::parse(text, CLOCK).unwrap(), index).unwrap();
        let ids = search(index, &query, &AtomicBool::new(false)).unwrap();
        assert!(ids.is_sorted(), "results must be in id order");
        let mut names: Vec<String> = ids.iter().map(|&id| index.name(id).to_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn substring_and_conjunction() {
        let idx = sample();
        assert_eq!(run(&idx, "MAIN"), ["main street.png", "main.rs"]);
        assert_eq!(run(&idx, "main rs"), ["main.rs"]);
        assert_eq!(run(&idx, "main !rs"), ["main street.png"]);
        assert_eq!(run(&idx, r#""main street""#), ["main street.png"]);
        assert!(run(&idx, "nothing-matches-this").is_empty());
    }

    #[test]
    fn filters() {
        let idx = sample();
        assert_eq!(run(&idx, "ext:rs"), ["lib.rs", "main.rs"]);
        assert_eq!(run(&idx, "type:image"), ["IMG_0001.JPG", "IMG_0002.jpg", "main street.png"]);
        assert_eq!(run(&idx, "type:video size:>1gb"), ["holiday video.MP4"]);
        assert_eq!(run(&idx, "type:folder"), ["C:", "Kestrel", "Photos", "Projects", "src"]);
        assert_eq!(run(&idx, "type:file size:<1kb !dm:1y"), ["main street.png"]);
        assert_eq!(run(&idx, "type:file size:<1kb dm:1y"), ["Cargo.toml", "lib.rs"]);
        assert!(run(&idx, "ext:nope").is_empty());
        assert_eq!(run(&idx, "!ext:nope type:file").len(), 8);
    }

    #[test]
    fn globs_regex_and_paths() {
        let idx = sample();
        assert_eq!(run(&idx, "img_????.jpg"), ["IMG_0001.JPG", "IMG_0002.jpg"]);
        assert_eq!(run(&idx, "*.rs"), ["lib.rs", "main.rs"]);
        assert_eq!(run(&idx, r"regex:^img_\d+\.jpg$"), ["IMG_0001.JPG", "IMG_0002.jpg"]);
        assert_eq!(run(&idx, r"src\main"), ["main.rs"]);
        assert_eq!(run(&idx, r"kestrel\"), ["Cargo.toml", "README.md", "lib.rs", "main.rs", "src"]);
        assert!(compile(&crate::parse("regex:(", CLOCK).unwrap(), &idx).is_err());
    }

    #[test]
    fn scoped_search_sees_entries_added_after_the_build() {
        let mut idx = sample();
        assert_eq!(run(&idx, r"in:C:\Photos main"), ["main street.png"]);
        assert_eq!(run(&idx, r"!in:C:\Photos main"), ["main.rs"]);
        assert!(run(&idx, r"in:C:\Missing main").is_empty());

        let src = idx.lookup(r"C:\Projects\Kestrel\src").unwrap();
        idx.insert(src, "main_test.rs", file(10));
        let photos = idx.lookup(r"C:\Photos").unwrap();
        idx.insert(photos, "mainsail.jpg", file(10));
        assert_eq!(run(&idx, r"in:C:\Projects main"), ["main.rs", "main_test.rs"]);

        // After a folder moves, subtree ranges are no longer trusted but scoping still works.
        let root = idx.roots()[0];
        idx.rename(photos, root, "Pictures");
        assert_eq!(run(&idx, r"in:C:\Pictures main"), ["main street.png", "mainsail.jpg"]);
        assert_eq!(run(&idx, r"in:C:\Projects main"), ["main.rs", "main_test.rs"]);

        idx.remove(idx.lookup(r"C:\Projects\Kestrel\src\main.rs").unwrap());
        assert_eq!(run(&idx, r"in:C:\Projects main"), ["main_test.rs"]);
    }

    /// Runs `text` with `tag:` names resolved through `tags`, a list of (name, paths).
    fn run_tagged(index: &Index, text: &str, tags: &[(&str, &[&str])]) -> Result<Vec<String>, ParseError> {
        let mut query = crate::parse(text, CLOCK).unwrap();
        crate::resolve_tags(&mut query, |name| {
            let (_, paths) = tags
                .iter()
                .find(|(tag, _)| tag.eq_ignore_ascii_case(name))
                .ok_or_else(|| ParseError(format!("No tag is named \"{name}\"")))?;
            Ok(paths.iter().filter_map(|path| index.lookup(path)).collect())
        })?;
        let compiled = compile(&query, index)?;
        let ids = search(index, &compiled, &AtomicBool::new(false)).unwrap();
        assert!(ids.is_sorted(), "results must be in id order");
        let mut names: Vec<String> = ids.iter().map(|&id| index.name(id).to_owned()).collect();
        names.sort();
        Ok(names)
    }

    #[test]
    fn tag_clauses_match_the_tagged_entries() {
        let mut idx = sample();
        let work: &[&str] = &[r"C:\Projects\Kestrel\src\main.rs", r"C:\Projects\Kestrel\README.md", r"C:\Photos"];
        let holiday: &[&str] = &[r"C:\Photos\IMG_0001.JPG", r"C:\Photos\holiday video.MP4"];
        let tags = [("work", work), ("holiday", holiday)];

        assert_eq!(run_tagged(&idx, "tag:work", &tags).unwrap(), ["Photos", "README.md", "main.rs"]);
        // Tags combine with every other clause, and with each other.
        assert_eq!(run_tagged(&idx, "tag:WORK type:file", &tags).unwrap(), ["README.md", "main.rs"]);
        assert_eq!(run_tagged(&idx, "tag:work ext:rs", &tags).unwrap(), ["main.rs"]);
        assert_eq!(run_tagged(&idx, r"tag:holiday in:C:\Photos size:>1gb", &tags).unwrap(), ["holiday video.MP4"]);
        assert!(run_tagged(&idx, "tag:work tag:holiday", &tags).unwrap().is_empty());
        assert_eq!(run_tagged(&idx, "type:image !tag:holiday", &tags).unwrap(), ["IMG_0002.jpg", "main street.png"]);
        assert!(run_tagged(&idx, "tag:missing", &tags).is_err());

        // A tagged entry that has since been deleted is not a result.
        idx.remove(idx.lookup(r"C:\Projects\Kestrel\README.md").unwrap());
        assert_eq!(run_tagged(&idx, "tag:work", &tags).unwrap(), ["Photos", "main.rs"]);
        // Without resolving, a tag clause can't be compiled.
        assert!(compile(&crate::parse("tag:work", CLOCK).unwrap(), &idx).is_err());
    }

    #[test]
    fn cancellation_returns_none() {
        let idx = sample();
        let query = compile(&crate::parse("main", CLOCK).unwrap(), &idx).unwrap();
        assert!(search(&idx, &query, &AtomicBool::new(true)).is_none());
    }
}
