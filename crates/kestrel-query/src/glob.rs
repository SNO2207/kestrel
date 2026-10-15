/// Matches `text` against a wildcard pattern where `*` is any run of bytes and `?` is any one
/// byte. The whole of `text` must match. Both sides are expected to be folded already.
pub(crate) fn matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // Where to resume if the current attempt fails: just after the last `*`, and one byte
    // further into the text than last time.
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            backtrack = Some((p + 1, t));
            p += 1;
        } else if let Some((star_p, star_t)) = backtrack {
            p = star_p;
            t = star_t + 1;
            backtrack = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

/// The longest run of literal bytes in a pattern, used to pre-filter candidates.
pub(crate) fn longest_literal(pattern: &[u8]) -> &[u8] {
    pattern.split(|&b| b == b'*' || b == b'?').max_by_key(|run| run.len()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, text: &str) -> bool {
        matches(pattern.as_bytes(), text.as_bytes())
    }

    #[test]
    fn wildcards() {
        assert!(m("*.rs", "main.rs"));
        assert!(!m("*.rs", "main.rs.bak"));
        assert!(m("img_????.jpg", "img_0042.jpg"));
        assert!(!m("img_????.jpg", "img_42.jpg"));
        assert!(m("a*b*c", "a--b--b--c"));
        assert!(!m("a*b*c", "a--b--b--"));
        assert!(m("*", ""));
        assert!(m("**a**", "banana"));
        assert!(!m("?", ""));
    }

    #[test]
    fn literal_extraction() {
        assert_eq!(longest_literal(b"*.config.??"), b".config.");
        assert_eq!(longest_literal(b"*"), b"");
    }
}
