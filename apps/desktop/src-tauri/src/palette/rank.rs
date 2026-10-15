//! Fuzzy matching and the ranking rules the palette applies on top of it.
//!
//! Matching is nucleo's: the query is split into words that must all match, each fuzzily
//! (its characters in order, gaps allowed), case-insensitively and with accents optional, and
//! word starts, consecutive runs and early matches score higher. `^word`, `word$`, `'word` and
//! `!word` work as in fzf: prefix, suffix, substring and exclusion.
//!
//! On top of the match score the palette favours what was used often and lately (see
//! [`recency_weight`]), and lets a provider nudge its items up or down by a percentage.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// How much recency can add: a weight of 1 multiplies the match score by `1 + RECENCY_GAIN`.
pub const RECENCY_GAIN: f64 = 0.8;
/// Uses at which an item counts as "used a lot": the weight from frequency saturates past it.
const FREQUENT_USES: f64 = 4.0;
/// After this long without a use, an item keeps half of its recency weight.
const HALF_LIFE_MS: f64 = 7.0 * 24.0 * 3600.0 * 1000.0;

/// A query compiled for matching, with the scratch space matching needs. One per thread.
pub struct Scorer {
    query: String,
    pattern: Pattern,
    matcher: Matcher,
    /// Whether the query is matched against paths, whose separators start words.
    paths: bool,
    chars: Vec<char>,
    indices: Vec<u32>,
    /// Each word of the query that must appear, folded, for the quick check in [`Scorer::may_match`].
    needles: Vec<Vec<u8>>,
}

impl Scorer {
    /// A scorer for names and titles.
    pub fn new(query: &str) -> Self {
        Self::build(query, false)
    }

    /// A scorer for full paths, where `\` and `/` separate words.
    pub fn for_paths(query: &str) -> Self {
        Self::build(query, true)
    }

    fn build(query: &str, paths: bool) -> Self {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let needles = pattern
            .atoms
            .iter()
            .filter(|atom| !atom.negative)
            .map(|atom| {
                let text: String = atom.needle_text().chars().collect();
                kestrel_index::fold(&text).into_bytes()
            })
            .collect();
        Self {
            query: query.to_owned(),
            pattern,
            matcher: Matcher::new(config(paths)),
            paths,
            chars: Vec::new(),
            indices: Vec::new(),
            needles,
        }
    }

    /// A scorer for the same query with scratch space of its own, for another thread.
    pub fn fork(&self) -> Self {
        Self::build(&self.query, self.paths)
    }

    /// Whether the query has nothing to match, so every item matches with a score of zero.
    pub fn is_empty(&self) -> bool {
        self.pattern.atoms.is_empty()
    }

    /// A quick check on a folded ASCII name: `false` means [`Scorer::score`] cannot match it.
    /// Names with other characters may still match through accent folding, so they always pass.
    pub fn may_match(&self, folded: &[u8]) -> bool {
        self.needles.iter().all(|needle| in_order(needle, folded)) || !folded.is_ascii()
    }

    /// The match score of `text`, or `None` if it does not match.
    pub fn score(&mut self, text: &str) -> Option<u32> {
        if self.is_empty() {
            return Some(0);
        }
        let haystack = Utf32Str::new(text, &mut self.chars);
        self.pattern.score(haystack, &mut self.matcher)
    }

    /// Where the query matches `text`, as sorted UTF-16 offsets, which is how the UI counts.
    pub fn highlights(&mut self, text: &str) -> Vec<u32> {
        if self.is_empty() {
            return Vec::new();
        }
        self.indices.clear();
        let haystack = Utf32Str::new(text, &mut self.chars);
        if self.pattern.indices(haystack, &mut self.matcher, &mut self.indices).is_none() {
            return Vec::new();
        }
        self.indices.sort_unstable();
        self.indices.dedup();
        utf16_offsets(text, &self.indices)
    }
}

fn config(paths: bool) -> Config {
    let mut config = if paths { Config::DEFAULT.match_paths() } else { Config::DEFAULT };
    // People type the start of what they want; a match nearer the start is the likelier one.
    config.prefer_prefix = true;
    config
}

/// Whether the bytes of `needle` appear in `haystack` in order.
fn in_order(needle: &[u8], haystack: &[u8]) -> bool {
    let mut rest = haystack;
    for &byte in needle {
        match memchr::memchr(byte, rest) {
            Some(at) => rest = &rest[at + 1..],
            None => return false,
        }
    }
    true
}

/// Converts sorted char positions in `text` to UTF-16 offsets.
fn utf16_offsets(text: &str, chars: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(chars.len());
    let mut wanted = chars.iter().peekable();
    let mut utf16 = 0u32;
    for (position, c) in text.chars().enumerate() {
        let Some(&&next) = wanted.peek() else { break };
        if next as usize == position {
            out.push(utf16);
            wanted.next();
        }
        utf16 += c.len_utf16() as u32;
    }
    out
}

/// How strongly an item's past use should lift it, from 0 (never or long ago) to just under 1
/// (used often and lately). Frequency saturates after a few uses, and the whole weight halves
/// every week without a use, so a folder used daily stays on top while last month's one-off
/// fades.
pub fn recency_weight(uses: u32, age_ms: i64) -> f64 {
    if uses == 0 {
        return 0.0;
    }
    let frequency = 1.0 - (-(uses as f64) / FREQUENT_USES).exp();
    let decay = 0.5f64.powf(age_ms.max(0) as f64 / HALF_LIFE_MS);
    frequency * decay
}

/// The score an item ranks by: its match score, lifted by recency and by its provider's boost
/// (a percentage, which may be negative).
pub fn ranked(score: u32, recency: f64, boost_percent: i32) -> u32 {
    let boost = 1.0 + f64::from(boost_percent.clamp(-90, 400)) / 100.0;
    let lifted = f64::from(score) * (1.0 + RECENCY_GAIN * recency.clamp(0.0, 1.0)) * boost;
    lifted.round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(query: &str, names: &[&str]) -> Vec<String> {
        let mut scorer = Scorer::new(query);
        let mut scored: Vec<(u32, &str)> =
            names.iter().filter_map(|name| scorer.score(name).map(|score| (score, *name))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        scored.into_iter().map(|(_, name)| name.to_owned()).collect()
    }

    #[test]
    fn whole_words_and_word_starts_beat_scattered_letters() {
        let ranked = order("doc", &["Documents", "my-docs", "random.txt", "adobe.cfg"]);
        assert_eq!(ranked[0], "Documents", "{ranked:?}");
        let at = |name: &str| ranked.iter().position(|n| n == name).unwrap();
        assert!(at("my-docs") < at("adobe.cfg"), "{ranked:?}");
        assert!(!ranked.contains(&"random.txt".to_owned()), "letters missing or out of order never match");
    }

    #[test]
    fn matching_ignores_case_and_accents_and_takes_every_word() {
        let mut scorer = Scorer::new("RESUME pdf");
        assert!(scorer.score("Résumé 2026.pdf").is_some());
        assert!(scorer.score("Résumé 2026.docx").is_none(), "every word must match");
        let mut excluded = Scorer::new("report !draft");
        assert!(excluded.score("report final.txt").is_some());
        assert!(excluded.score("report draft.txt").is_none());
    }

    #[test]
    fn the_quick_check_never_rejects_a_match() {
        let names = ["Documents", "my-docs", "Résumé.pdf", "naïve notes", "x", "DOC"];
        for query in ["doc", "dcs", "resume", "nn", "d o c", "^my", "!x"] {
            let mut scorer = Scorer::new(query);
            for name in names {
                if scorer.score(name).is_some() {
                    let folded = kestrel_index::fold(name);
                    assert!(scorer.may_match(folded.as_bytes()), "{query} on {name}");
                }
            }
        }
        assert!(!Scorer::new("doc").may_match(b"random.txt"));
    }

    #[test]
    fn highlights_are_utf16_offsets_of_the_matched_characters() {
        let mut scorer = Scorer::new("rep");
        assert_eq!(scorer.highlights("Report.txt"), [0, 1, 2]);
        // The emoji takes two UTF-16 units, so the letters after it move along by two.
        assert_eq!(scorer.highlights("😀 report"), [3, 4, 5]);
        assert!(scorer.highlights("nothing").is_empty());
        assert!(Scorer::new("").highlights("anything").is_empty());
        let mut words = Scorer::new("tab new");
        assert_eq!(words.highlights("New tab"), [0, 1, 2, 4, 5, 6], "sorted, whatever the word order");
    }

    #[test]
    fn an_empty_query_matches_everything_equally() {
        let mut scorer = Scorer::new("   ");
        assert!(scorer.is_empty());
        assert_eq!(scorer.score("anything"), Some(0));
        assert!(scorer.may_match(b"anything"));
    }

    #[test]
    fn path_scoring_treats_separators_as_word_starts() {
        let mut paths = Scorer::for_paths("dl");
        let mut names = Scorer::new("dl");
        let path = r"C:\Users\me\Downloads\list";
        assert!(paths.score(path).unwrap() >= names.score(path).unwrap());
    }

    #[test]
    fn recency_rises_with_use_and_fades_with_time() {
        const DAY: i64 = 24 * 3600 * 1000;
        assert_eq!(recency_weight(0, 0), 0.0);
        assert!(recency_weight(1, 0) < recency_weight(5, 0));
        assert!(recency_weight(5, 0) < recency_weight(50, 0));
        assert!(recency_weight(50, 0) < 1.0);
        assert!(recency_weight(5, DAY) > recency_weight(5, 30 * DAY));
        let week = recency_weight(5, 7 * DAY) / recency_weight(5, 0);
        assert!((week - 0.5).abs() < 1e-9, "half after a week, got {week}");
        assert_eq!(recency_weight(3, -DAY), recency_weight(3, 0), "a clock that went back counts as now");
    }

    #[test]
    fn recency_and_boosts_scale_the_match_score() {
        let mut scorer = Scorer::new("proj");
        let good = scorer.score("Projects").unwrap();
        let fair = scorer.score("old-projects-backup").unwrap();
        assert!(good > fair);
        // Among close matches the one used lately wins; at most it gains RECENCY_GAIN.
        assert!(ranked(fair, recency_weight(20, 0), 0) > ranked(good, 0.0, 0));
        assert!(ranked(fair, 1.0, 0) <= (f64::from(fair) * (1.0 + RECENCY_GAIN)).round() as u32);
        assert!(ranked(good, 0.2, 0) < ranked(good, 0.4, 0));
        assert_eq!(ranked(0, 1.0, 50), 0, "an item matched by nothing but an empty query stays at zero");
        assert_eq!(ranked(100, 0.0, 0), 100);
        assert_eq!(ranked(100, 0.0, 20), 120);
        assert_eq!(ranked(100, 0.0, -1000), 10, "boosts are clamped");
        assert_eq!(ranked(100, 1.0, 0), 180);
    }
}
