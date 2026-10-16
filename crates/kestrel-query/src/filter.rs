//! The filter box: narrowing a listing to the names that match what is typed.

use kestrel_index::fold;
use memchr::memmem;

use crate::glob;

/// Names containing the typed text, ignoring case; or, when the text holds `*` or `?`, names
/// that match it as a wildcard pattern over the whole name, as `*.rs` does in a search.
#[derive(Clone, Debug)]
pub struct NameFilter {
    folded: Vec<u8>,
    wildcard: bool,
}

impl NameFilter {
    pub fn new(text: &str) -> Self {
        let folded = fold(text.trim()).into_bytes();
        let wildcard = folded.iter().any(|&b| b == b'*' || b == b'?');
        Self { folded, wildcard }
    }

    /// Whether the filter lets everything through.
    pub fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }

    /// Whether a name, folded as the index folds names, passes.
    pub fn matches(&self, folded_name: &[u8]) -> bool {
        if self.wildcard {
            glob::matches(&self.folded, folded_name)
        } else {
            memmem::find(folded_name, &self.folded).is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passes(filter: &str, name: &str) -> bool {
        NameFilter::new(filter).matches(fold(name).as_bytes())
    }

    #[test]
    fn plain_text_matches_anywhere_in_the_name() {
        assert!(passes("port", "Report 2026.PDF"));
        assert!(passes("REPORT", "report.pdf"));
        assert!(!passes("reports", "report.pdf"));
        assert!(passes("", "anything"));
        assert!(NameFilter::new("  ").is_empty());
    }

    #[test]
    fn wildcards_match_the_whole_name() {
        assert!(passes("*.pdf", "Report.PDF"));
        assert!(!passes("*.pdf", "report.pdf.bak"));
        assert!(passes("img_????.jpg", "IMG_0042.jpg"));
        assert!(!passes("img_????.jpg", "img_42.jpg"));
        assert!(passes("re*rt*", "report.pdf"));
    }
}
