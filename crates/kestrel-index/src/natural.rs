use std::cmp::Ordering;

/// Natural ordering over folded name bytes: digit runs compare by numeric value, so `file2`
/// sorts before `file10`. Total order; equal only for identical inputs.
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (ra, rb) = (trim_zeros(&a[si..i]), trim_zeros(&b[sj..j]));
            let ord = ra.len().cmp(&rb.len()).then_with(|| ra.cmp(rb));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            if a[i] != b[j] {
                return a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let first = digits.iter().position(|&d| d != b'0').unwrap_or(digits.len());
    &digits[first..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_by_value() {
        let mut names = vec!["file10.txt", "file2.txt", "file1.txt", "file02.txt", "a", "file"];
        names.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
        assert_eq!(names, ["a", "file", "file1.txt", "file02.txt", "file2.txt", "file10.txt"]);
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        let a = b"99999999999999999999999999999999";
        let b = b"100000000000000000000000000000000";
        assert_eq!(natural_cmp(a, b), Ordering::Less);
    }

    #[test]
    fn equal_only_when_identical() {
        assert_eq!(natural_cmp(b"a1", b"a1"), Ordering::Equal);
        assert_ne!(natural_cmp(b"a1", b"a01"), Ordering::Equal);
    }
}
