/// Appends a case-folded copy of `src` to `dst` that has exactly the same byte length.
///
/// ASCII is lower-cased. A non-ASCII char is replaced by its lowercase form only when that form
/// is a single char with the same UTF-8 length; otherwise it is kept as is. Preserving length is
/// what lets the display arena and the search arena share one offset table.
pub fn fold_into(src: &str, dst: &mut Vec<u8>) {
    let start = dst.len();
    if src.is_ascii() {
        dst.extend_from_slice(src.as_bytes());
        dst[start..].make_ascii_lowercase();
        return;
    }
    let mut buf = [0u8; 4];
    for c in src.chars() {
        let mut lower = c.to_lowercase();
        let folded = match (lower.next(), lower.next()) {
            (Some(l), None) if l.len_utf8() == c.len_utf8() => l,
            _ => c,
        };
        dst.extend_from_slice(folded.encode_utf8(&mut buf).as_bytes());
    }
    debug_assert_eq!(dst.len() - start, src.len());
}

/// Case-folds `src` the same way the index folds names. Queries must be folded with this.
pub fn fold(src: &str) -> String {
    let mut out = Vec::with_capacity(src.len());
    fold_into(src, &mut out);
    // SAFETY: fold_into only writes ASCII bytes or whole chars encoded with encode_utf8.
    unsafe { String::from_utf8_unchecked(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_lowercased() {
        assert_eq!(fold("README.md"), "readme.md");
    }

    #[test]
    fn length_is_preserved_for_non_ascii() {
        for s in ["Ünïcödé.TXT", "İstanbul", "ΑΒΓ δ", "日本語.PNG", "ẞtraße"] {
            assert_eq!(fold(s).len(), s.len(), "{s}");
        }
        assert_eq!(fold("ÜBER"), "über");
        // U+0130 lowercases to two chars, so it is kept unchanged.
        assert!(fold("İ").starts_with('İ'));
    }
}
