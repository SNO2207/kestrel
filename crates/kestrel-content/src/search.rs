//! Searching one file: ripgrep's line searcher with the patterns compiled once, the first
//! matching line made into a snippet, and the reasons a file is passed over.

use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use serde::Serialize;

use crate::fsinfo;

/// How a pattern's letters compare.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    /// `todo` finds `TODO`.
    #[default]
    Insensitive,
    Sensitive,
    /// Insensitive unless the pattern has a capital letter, as ripgrep's `--smart-case`.
    Smart,
}

/// Text to look for in files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pattern {
    pub text: String,
    /// A regular expression rather than literal text.
    pub regex: bool,
    pub case: Case,
    /// Matches only whole words.
    pub whole_word: bool,
    /// Files containing it are left out.
    pub negate: bool,
}

/// How far a search goes.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Files larger than this are passed over.
    pub max_file_size: u64,
    /// The search stops once this many files have matched.
    pub max_found: usize,
    /// Matching lines counted per file; more are reported as "and more".
    pub max_counted: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_file_size: 50 << 20, max_found: 5_000, max_counted: 999 }
    }
}

/// The first matching line of a file, ready to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hit {
    /// One-based line number.
    pub line: u64,
    /// The line, its leading space trimmed and cut around the first match when long.
    pub text: String,
    /// Where the matches are in `text`, as UTF-16 offsets (`start`, `end`), as JavaScript
    /// strings count.
    pub ranges: Vec<(u32, u32)>,
    /// Matching lines in the file, up to the limit.
    pub lines: u32,
    /// More lines match than were counted.
    pub more: bool,
}

/// Why a file was passed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// Holds a NUL byte (after any UTF-16 is decoded), or is of a type that never holds text.
    Binary,
    TooLarge,
    /// Its contents are only in the cloud; opening it would download it.
    CloudOnly,
    /// It couldn't be opened or read.
    Unreadable,
}

/// What searching one file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every pattern matched as asked; the hit is the first pattern's first line, if any
    /// pattern looks for text rather than its absence.
    Match(Option<Hit>),
    NoMatch,
    Skipped(Skip),
    Cancelled,
}

/// Snippets longer than this, in characters, are cut around their first match.
const SNIPPET_CHARS: usize = 200;
/// Characters kept before the first match when a snippet is cut.
const SNIPPET_LEAD: usize = 48;

/// Extensions of files that never hold searchable text; skipped without being opened.
const BINARY_EXTENSIONS: &[&str] = &[
    "exe", "dll", "sys", "msi", "msix", "cab", "obj", "lib", "pdb", "o", "a", "so", "dylib", "class", "jar", "pyc", "wasm",
    "zip", "7z", "rar", "gz", "tgz", "bz2", "xz", "zst", "tar", "iso", "img", "vhd", "vhdx", "vmdk", "jpg", "jpeg", "png",
    "gif", "bmp", "webp", "heic", "heif", "avif", "tif", "tiff", "ico", "psd", "raw", "cr2", "cr3", "nef", "arw", "dng",
    "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "mpg", "mpeg", "mp3", "flac", "wav", "m4a", "aac", "ogg", "opus",
    "wma", "ttf", "otf", "woff", "woff2", "pdf", "docx", "xlsx", "pptx", "odt", "ods", "odp", "doc", "xls", "ppt", "db",
    "sqlite", "kidx", "bin", "dat", "pak", "dmp", "blend", "fbx", "uasset", "umap",
];

/// Patterns compiled for searching many files.
pub struct Search {
    find: Vec<RegexMatcher>,
    avoid: Vec<RegexMatcher>,
    pub limits: Limits,
}

fn compile(pattern: &Pattern) -> Result<RegexMatcher, String> {
    let mut builder = RegexMatcherBuilder::new();
    builder.line_terminator(Some(b'\n')).fixed_strings(!pattern.regex).word(pattern.whole_word);
    match pattern.case {
        Case::Insensitive => builder.case_insensitive(true),
        Case::Sensitive => builder.case_insensitive(false),
        Case::Smart => builder.case_smart(true),
    };
    builder.build(&pattern.text).map_err(|error| {
        if pattern.regex { format!("Not a valid regular expression: {error}") } else { error.to_string() }
    })
}

/// Stops a read when the search is cancelled, so a large file gives way at once.
struct Cancellable<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // Not `Interrupted`, which readers retry.
            return Err(io::Error::other("cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Collects the first matching line and counts the rest.
struct Collect<'m> {
    matcher: &'m RegexMatcher,
    /// Only whether anything matches is wanted.
    any: bool,
    max_counted: u32,
    first: Option<Hit>,
    lines: u32,
    more: bool,
    binary: bool,
}

impl Sink for Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.any {
            self.lines = 1;
            return Ok(false);
        }
        if self.lines >= self.max_counted {
            self.more = true;
            return Ok(false);
        }
        self.lines += 1;
        if self.first.is_none() {
            self.first = Some(snippet(self.matcher, found.bytes(), found.line_number().unwrap_or(0)));
        }
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

impl Search {
    /// Compiles the patterns. At least one is needed for a file to be read at all.
    pub fn new(patterns: &[Pattern], limits: Limits) -> Result<Self, String> {
        let mut find = Vec::new();
        let mut avoid = Vec::new();
        for pattern in patterns.iter().filter(|pattern| !pattern.text.is_empty()) {
            let compiled = compile(pattern)?;
            if pattern.negate { avoid.push(compiled) } else { find.push(compiled) }
        }
        Ok(Self { find, avoid, limits })
    }

    /// Whether files need reading at all: there is something to look for, or to avoid.
    pub fn reads(&self) -> bool {
        !self.find.is_empty() || !self.avoid.is_empty()
    }

    fn searcher() -> Searcher {
        SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true)
            // UTF-16 files with a byte-order mark are decoded to UTF-8 before searching.
            .bom_sniffing(true)
            .build()
    }

    /// Runs one matcher over the file from the start.
    fn run<'m>(&self, path: &str, matcher: &'m RegexMatcher, any: bool, cancel: &AtomicBool) -> Result<Collect<'m>, Skip> {
        // `fsinfo::open` refuses cloud-only files at the last moment, from their attributes.
        let file: File = fsinfo::open(path).ok_or(Skip::Unreadable)?;
        let reader = Cancellable { inner: file, cancel };
        let mut sink = Collect { matcher, any, max_counted: self.limits.max_counted, first: None, lines: 0, more: false, binary: false };
        let mut searcher = Self::searcher();
        let result = searcher.search_reader(matcher, reader, &mut sink);
        drop(searcher);
        if sink.binary {
            return Err(Skip::Binary);
        }
        match result {
            Ok(()) => Ok(sink),
            Err(_) if cancel.load(Ordering::Relaxed) => Ok(Collect { lines: 0, ..sink }),
            Err(_) => Err(Skip::Unreadable),
        }
    }

    /// Searches the file at `path`, of `size` bytes with attributes `attrs` as last seen. The
    /// attributes are read again just before the file is opened, so a file that went to the
    /// cloud since it was listed is still never opened.
    pub fn search_file(&self, path: &str, name: &str, size: u64, attrs: u32, cancel: &AtomicBool) -> Outcome {
        if cancel.load(Ordering::Relaxed) {
            return Outcome::Cancelled;
        }
        if attrs & fsinfo::CLOUD_ONLY != 0 {
            return Outcome::Skipped(Skip::CloudOnly);
        }
        if size > self.limits.max_file_size {
            return Outcome::Skipped(Skip::TooLarge);
        }
        if !self.reads() {
            return Outcome::Match(None);
        }
        let extension = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
        if extension.is_some_and(|ext| BINARY_EXTENSIONS.contains(&ext.as_str())) {
            return Outcome::Skipped(Skip::Binary);
        }
        match fsinfo::attributes(path) {
            None => return Outcome::Skipped(Skip::Unreadable),
            Some(now) if now.attrs & fsinfo::CLOUD_ONLY != 0 => return Outcome::Skipped(Skip::CloudOnly),
            Some(now) if now.size > self.limits.max_file_size => return Outcome::Skipped(Skip::TooLarge),
            Some(_) => {}
        }
        let mut hit = None;
        for (i, matcher) in self.find.iter().enumerate() {
            match self.run(path, matcher, i > 0, cancel) {
                Err(skip) => return Outcome::Skipped(skip),
                Ok(_) if cancel.load(Ordering::Relaxed) => return Outcome::Cancelled,
                Ok(found) if found.lines == 0 => return Outcome::NoMatch,
                Ok(found) => {
                    if i == 0 {
                        hit = found.first.map(|first| Hit { lines: found.lines, more: found.more, ..first });
                    }
                }
            }
        }
        for matcher in &self.avoid {
            match self.run(path, matcher, true, cancel) {
                Err(skip) => return Outcome::Skipped(skip),
                Ok(_) if cancel.load(Ordering::Relaxed) => return Outcome::Cancelled,
                Ok(found) if found.lines > 0 => return Outcome::NoMatch,
                Ok(_) => {}
            }
        }
        Outcome::Match(hit)
    }
}

/// The matching line as a snippet: decoded (invalid bytes replaced), its line break and
/// leading space dropped, cut around the first match when long, with the matches' places in
/// UTF-16 offsets.
pub fn snippet(matcher: &RegexMatcher, line: &[u8], number: u64) -> Hit {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    let line = &line[..end];
    let mut found: Vec<(usize, usize)> = Vec::new();
    let _ = matcher.find_iter(line, |m| {
        if m.start() < m.end() {
            found.push((m.start(), m.end()));
        }
        found.len() < 64
    });
    // Decoded piece by piece, so that replacing invalid bytes never shifts a match.
    let mut chars: Vec<char> = Vec::new();
    let mut marked: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for &(start, stop) in &found {
        chars.extend(String::from_utf8_lossy(&line[at..start]).chars());
        let from = chars.len();
        chars.extend(String::from_utf8_lossy(&line[start..stop]).chars());
        marked.push((from, chars.len()));
        at = stop;
    }
    chars.extend(String::from_utf8_lossy(&line[at..]).chars());
    // Tabs and other control characters show as spaces.
    for c in &mut chars {
        if c.is_control() {
            *c = ' ';
        }
    }
    let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
    let mut from = lead;
    let mut to = chars.len();
    let first = marked.first().map_or(from, |m| m.0);
    if to - from > SNIPPET_CHARS {
        if first > from + SNIPPET_LEAD {
            from = first - SNIPPET_LEAD;
        }
        to = to.min(from + SNIPPET_CHARS);
    }
    let mut text = String::new();
    let mut units = 0u32;
    if from > lead {
        text.push('\u{2026}');
        units += 1;
    }
    let mut ranges = Vec::new();
    let position = |index: usize, chars: &[char]| -> u32 {
        chars[from..index].iter().map(|c| c.len_utf16() as u32).sum::<u32>()
    };
    for &(start, stop) in &marked {
        if stop <= from || start >= to {
            continue;
        }
        let start = position(start.max(from), &chars) + units;
        let stop = position(stop.min(to), &chars) + units;
        ranges.push((start, stop));
    }
    text.extend(&chars[from..to]);
    if to < chars.len() {
        text.push('\u{2026}');
    }
    Hit { line: number, text, ranges, lines: 1, more: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(text: &str) -> RegexMatcher {
        compile(&Pattern { text: text.into(), ..Pattern::default() }).unwrap()
    }

    /// The marked parts of a snippet, read back as JavaScript would.
    fn marked(hit: &Hit) -> Vec<String> {
        let units: Vec<u16> = hit.text.encode_utf16().collect();
        hit.ranges.iter().map(|&(a, b)| String::from_utf16(&units[a as usize..b as usize]).unwrap()).collect()
    }

    #[test]
    fn a_snippet_marks_every_match_on_its_line() {
        let hit = snippet(&matcher("todo"), b"    // TODO: one todo, two TODO\r\n", 12);
        assert_eq!(hit.line, 12);
        assert_eq!(hit.text, "// TODO: one todo, two TODO");
        assert_eq!(marked(&hit), ["TODO", "todo", "TODO"]);
    }

    #[test]
    fn offsets_are_in_utf16_units_and_survive_bad_bytes() {
        // An emoji is two UTF-16 units; an invalid byte becomes one replacement character.
        let line = "\u{1F600} caf\u{e9} \u{fffd}x match".as_bytes().to_vec();
        let mut bad = b"\xff".to_vec();
        bad.extend_from_slice("\u{1F600} match".as_bytes());
        let hit = snippet(&matcher("match"), &line, 1);
        assert_eq!(marked(&hit), ["match"]);
        let hit = snippet(&matcher("match"), &bad, 1);
        assert_eq!(hit.text, "\u{fffd}\u{1F600} match");
        assert_eq!(marked(&hit), ["match"]);
    }

    #[test]
    fn long_lines_are_cut_around_the_first_match() {
        let line = format!("{}needle{}", "a".repeat(5000), "b".repeat(5000));
        let hit = snippet(&matcher("needle"), line.as_bytes(), 3);
        assert!(hit.text.starts_with('\u{2026}') && hit.text.ends_with('\u{2026}'), "{}", hit.text);
        assert!(hit.text.chars().count() <= SNIPPET_CHARS + 2);
        assert_eq!(marked(&hit), ["needle"]);
        // A match near the start keeps the start.
        let line = format!("needle {}", "c".repeat(400));
        let hit = snippet(&matcher("needle"), line.as_bytes(), 1);
        assert!(hit.text.starts_with("needle") && hit.text.ends_with('\u{2026}'));
    }

    #[test]
    fn patterns_compile_as_literal_or_regular_expressions() {
        let literal = compile(&Pattern { text: "a.c".into(), ..Pattern::default() }).unwrap();
        assert!(literal.is_match(b"xa.cx").unwrap());
        assert!(!literal.is_match(b"abc").unwrap());
        let regex = compile(&Pattern { text: r"a\.c|b+".into(), regex: true, ..Pattern::default() }).unwrap();
        assert!(regex.is_match(b"bbb").unwrap());
        let sensitive = compile(&Pattern { text: "Fn".into(), case: Case::Sensitive, ..Pattern::default() }).unwrap();
        assert!(!sensitive.is_match(b"fn main").unwrap());
        let smart = compile(&Pattern { text: "fn".into(), case: Case::Smart, ..Pattern::default() }).unwrap();
        assert!(smart.is_match(b"FN").unwrap());
        let smart = compile(&Pattern { text: "Fn".into(), case: Case::Smart, ..Pattern::default() }).unwrap();
        assert!(!smart.is_match(b"fn").unwrap());
        let word = compile(&Pattern { text: "cat".into(), whole_word: true, ..Pattern::default() }).unwrap();
        assert!(word.is_match(b"a cat sat").unwrap() && !word.is_match(b"concatenate").unwrap());
        let error = compile(&Pattern { text: "(".into(), regex: true, ..Pattern::default() }).unwrap_err();
        assert!(error.starts_with("Not a valid regular expression"), "{error}");
    }
}
