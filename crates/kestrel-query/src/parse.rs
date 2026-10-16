use std::fmt;

use kestrel_index::{EntryId, fold};

use crate::time::{
    Clock, civil_from_days, days_from_civil, month_start, unix_to_filetime, week_start,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub clauses: Vec<Clause>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clause {
    pub negate: bool,
    pub kind: ClauseKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClauseKind {
    /// Folded substring of the file name.
    Name(String),
    /// Folded substring of the full path, with `\` separators.
    Path(String),
    /// Folded wildcard pattern matched against the whole file name.
    Glob(String),
    /// Regular expression matched against the file name, case-insensitively.
    Regex(String),
    /// Lower-case extensions without the dot.
    Ext(Vec<String>),
    /// Inclusive range of sizes in bytes.
    Size(u64, u64),
    /// Half-open range of modification times as FILETIME ticks.
    Modified(i64, i64),
    Folders,
    Files,
    /// Absolute path of a folder to search beneath.
    Within(String),
    /// Items carrying the tag with this name. The index knows nothing of tags, so the caller
    /// resolves this to [`ClauseKind::Entries`] with [`resolve_tags`] before compiling.
    Tag(String),
    /// Exactly these entries, in ascending id order: a tag, once resolved.
    Entries(Vec<EntryId>),
    /// Photos and videos that look like this description (`like:"a dog on a sofa"`, or the
    /// whole query after a leading `~`), or like the picture at this path ("Find similar").
    /// Only the caller's models can tell, so it resolves this to [`ClauseKind::Entries`] with
    /// [`resolve_likes`] before compiling, as it does tags.
    Like(String),
}

/// Replaces every `like:` clause with the entries `resolve` finds for its description, sorted.
pub fn resolve_likes(
    query: &mut Query,
    mut resolve: impl FnMut(&str) -> Result<Vec<EntryId>, ParseError>,
) -> Result<(), ParseError> {
    for clause in &mut query.clauses {
        if let ClauseKind::Like(description) = &clause.kind {
            let mut entries = resolve(description)?;
            entries.sort_unstable();
            entries.dedup();
            clause.kind = ClauseKind::Entries(entries);
        }
    }
    Ok(())
}

/// The descriptions a query asks to look like, in order.
pub fn likes(query: &Query) -> Vec<String> {
    query
        .clauses
        .iter()
        .filter(|clause| !clause.negate)
        .filter_map(|clause| match &clause.kind {
            ClauseKind::Like(description) => Some(description.clone()),
            _ => None,
        })
        .collect()
}

/// Replaces every `tag:` clause with the entries `resolve` finds for its tag name, sorted.
pub fn resolve_tags(
    query: &mut Query,
    mut resolve: impl FnMut(&str) -> Result<Vec<EntryId>, ParseError>,
) -> Result<(), ParseError> {
    for clause in &mut query.clauses {
        if let ClauseKind::Tag(name) = &clause.kind {
            let mut entries = resolve(name)?;
            entries.sort_unstable();
            entries.dedup();
            clause.kind = ClauseKind::Entries(entries);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(message.into()))
}

struct Token {
    text: String,
    /// The token started with a quote, so it is literal text rather than a filter or wildcard.
    quoted: bool,
}

/// Splits on whitespace, treating double-quoted runs as part of the surrounding token.
fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut current: Option<Token> = None;
    let mut in_quotes = false;
    for c in input.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            current.get_or_insert(Token { text: String::new(), quoted: true });
        } else if c.is_whitespace() && !in_quotes {
            tokens.extend(current.take());
        } else {
            current.get_or_insert(Token { text: String::new(), quoted: false }).text.push(c);
        }
    }
    tokens.extend(current);
    tokens
}

/// Parses query text. Filters with an empty value (`ext:`) are ignored so that a query stays
/// valid while it is being typed; malformed values are reported. A query that starts with `~`
/// is a description as a whole (`~sunset at the beach`), the same as one `like:` clause.
pub fn parse(input: &str, clock: Clock) -> Result<Query, ParseError> {
    if let Some(description) = input.trim_start().strip_prefix('~') {
        let description = description.trim();
        let clauses = (!description.is_empty())
            .then(|| Clause { negate: false, kind: ClauseKind::Like(description.to_owned()) })
            .into_iter()
            .collect();
        return Ok(Query { clauses });
    }
    let mut clauses = Vec::new();
    for token in tokenize(input) {
        let (negate, text) = match token.text.strip_prefix('!') {
            Some(rest) if !token.quoted && !rest.is_empty() => (true, rest),
            _ => (false, token.text.as_str()),
        };
        if text.is_empty() {
            continue;
        }
        let kind = if token.quoted { literal(text) } else { clause(text, clock)? };
        clauses.extend(kind.map(|kind| Clause { negate, kind }));
    }
    Ok(Query { clauses })
}

fn literal(text: &str) -> Option<ClauseKind> {
    let folded = fold(text).replace('/', "\\");
    Some(if folded.contains('\\') { ClauseKind::Path(folded) } else { ClauseKind::Name(folded) })
}

fn clause(text: &str, clock: Clock) -> Result<Option<ClauseKind>, ParseError> {
    if let Some((key, value)) = text.split_once(':') {
        let filter = match key.to_ascii_lowercase().as_str() {
            "ext" => Some(extensions(value)),
            "type" | "kind" => Some(category(value)?),
            "size" => Some(size(value)?),
            "dm" | "modified" | "date" => Some(modified(value, clock)?),
            "in" | "folder" => Some(ClauseKind::Within(value.to_owned())),
            "regex" => Some(ClauseKind::Regex(value.to_owned())),
            "path" => Some(ClauseKind::Path(fold(value).replace('/', "\\"))),
            "name" => Some(ClauseKind::Name(fold(value))),
            "tag" => Some(ClauseKind::Tag(value.to_owned())),
            "like" => Some(ClauseKind::Like(value.trim().to_owned())),
            // Anything else, such as the drive letter in `C:\Users`, is ordinary text.
            _ => None,
        };
        if let Some(filter) = filter {
            return Ok((!value.is_empty()).then_some(filter));
        }
    }
    if text.contains(['*', '?']) && !text.contains(['\\', '/']) {
        return Ok(Some(ClauseKind::Glob(fold(text))));
    }
    Ok(literal(text))
}

fn extensions(value: &str) -> ClauseKind {
    let exts = value
        .split([';', ',', '|'])
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    ClauseKind::Ext(exts)
}

fn category(value: &str) -> Result<ClauseKind, ParseError> {
    let exts: &[&str] = match value.to_ascii_lowercase().as_str() {
        "folder" | "folders" | "dir" => return Ok(ClauseKind::Folders),
        "file" | "files" => return Ok(ClauseKind::Files),
        "image" | "images" | "picture" | "pictures" | "photo" | "photos" | "pic" => &[
            "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "avif", "tif", "tiff",
            "svg", "ico", "raw", "cr2", "cr3", "nef", "arw", "dng", "psd", "jxl",
        ],
        "video" | "videos" | "movie" | "movies" => &[
            "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "ts", "m2ts",
            "3gp",
        ],
        "audio" | "music" | "sound" => {
            &["mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "aiff", "mid"]
        }
        "doc" | "docs" | "document" | "documents" => &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "odt", "ods",
            "odp", "csv", "epub",
        ],
        "archive" | "archives" | "compressed" | "zip" => {
            &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "cab"]
        }
        "code" | "source" => &[
            "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "c", "h", "cpp", "hpp", "cc", "cs",
            "java", "kt", "go", "rb", "php", "swift", "lua", "sh", "ps1", "bat", "json", "toml",
            "yaml", "yml", "xml", "html", "css", "scss", "sql", "vue", "svelte", "zig",
        ],
        "exe" | "app" | "program" | "executable" => &["exe", "msi", "dll", "com", "scr", "msix"],
        other => return err(format!("Unknown type \"{other}\"")),
    };
    Ok(ClauseKind::Ext(exts.iter().map(|&e| e.to_owned()).collect()))
}

/// A comparison prefix split from its operand.
enum Cmp<'a> {
    Greater(&'a str),
    GreaterEq(&'a str),
    Less(&'a str),
    LessEq(&'a str),
    Range(&'a str, &'a str),
    Plain(&'a str),
}

fn comparison(value: &str) -> Cmp<'_> {
    if let Some(v) = value.strip_prefix(">=") {
        Cmp::GreaterEq(v)
    } else if let Some(v) = value.strip_prefix("<=") {
        Cmp::LessEq(v)
    } else if let Some(v) = value.strip_prefix('>') {
        Cmp::Greater(v)
    } else if let Some(v) = value.strip_prefix('<') {
        Cmp::Less(v)
    } else if let Some((from, to)) = value.split_once("..") {
        Cmp::Range(from, to)
    } else {
        Cmp::Plain(value.strip_prefix('=').unwrap_or(value))
    }
}

/// Parses `1.5gb` into `(bytes, bytes per unit)`.
fn byte_count(text: &str) -> Result<(u64, u64), ParseError> {
    let lower = text.to_ascii_lowercase();
    let digits = lower.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit: u64 = match &lower[digits.len()..] {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return err(format!("Unknown size unit in \"{text}\"")),
    };
    match digits.parse::<f64>() {
        Ok(n) if n >= 0.0 && n.is_finite() => Ok(((n * unit as f64) as u64, unit)),
        _ => err(format!("\"{text}\" is not a size")),
    }
}

fn size(value: &str) -> Result<ClauseKind, ParseError> {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    let (min, max) = match value.to_ascii_lowercase().as_str() {
        "empty" => (0, 0),
        "tiny" => (1, 10 * KB),
        "small" => (10 * KB + 1, 100 * KB),
        "medium" => (100 * KB + 1, MB),
        "large" => (MB + 1, 16 * MB),
        "huge" => (16 * MB + 1, 128 * MB),
        "gigantic" => (128 * MB + 1, u64::MAX),
        _ => match comparison(value) {
            Cmp::Greater(v) => (byte_count(v)?.0.saturating_add(1), u64::MAX),
            Cmp::GreaterEq(v) => (byte_count(v)?.0, u64::MAX),
            Cmp::Less(v) => match byte_count(v)?.0 {
                // Nothing is smaller than zero bytes.
                0 => (1, 0),
                n => (0, n - 1),
            },
            Cmp::LessEq(v) => (0, byte_count(v)?.0),
            Cmp::Range(from, to) => (byte_count(from)?.0, byte_count(to)?.0),
            // `size:10mb` means "shown as 10 MB": anything from 10 MB up to the next whole MB.
            Cmp::Plain(v) => {
                let (bytes, unit) = byte_count(v)?;
                (bytes, bytes.saturating_add(unit - 1))
            }
        },
    };
    Ok(ClauseKind::Size(min, max))
}

/// Parses `2026`, `2026-03` or `2026-03-05` into the half-open range of days it covers.
fn date_span(text: &str) -> Result<(i64, i64), ParseError> {
    let bad = || ParseError(format!("\"{text}\" is not a date (use 2026, 2026-03 or 2026-03-05)"));
    let mut parts = text.split('-');
    let mut next = |max: u32| -> Result<Option<u32>, ParseError> {
        match parts.next() {
            None => Ok(None),
            Some(p) => match p.parse::<u32>() {
                Ok(n) if (1..=max).contains(&n) => Ok(Some(n)),
                _ => Err(bad()),
            },
        }
    };
    let year = next(9999)?.ok_or_else(bad)? as i64;
    let month = next(12)?;
    let day = next(31)?;
    if parts.next().is_some() || year < 1601 {
        return Err(bad());
    }
    Ok(match (month, day) {
        (None, _) => (days_from_civil(year, 1, 1), days_from_civil(year + 1, 1, 1)),
        (Some(m), None) => {
            let start = days_from_civil(year, m, 1);
            (start, month_start(start, 1))
        }
        (Some(m), Some(d)) => {
            let start = days_from_civil(year, m, d);
            (start, start + 1)
        }
    })
}

fn modified(value: &str, clock: Clock) -> Result<ClauseKind, ParseError> {
    const MIN: i64 = 0;
    const MAX: i64 = i64::MAX;
    let today = clock.today();
    let days = |from: i64, to: i64| (clock.day_start(from), clock.day_start(to));
    let lower = value.to_ascii_lowercase();

    let named = match lower.as_str() {
        "today" => Some(days(today, today + 1)),
        "yesterday" => Some(days(today - 1, today)),
        "thisweek" => Some(days(week_start(today), today + 1)),
        "lastweek" => Some(days(week_start(today) - 7, week_start(today))),
        "thismonth" => Some(days(month_start(today, 0), today + 1)),
        "lastmonth" => Some(days(month_start(today, -1), month_start(today, 0))),
        "thisyear" => Some(days(year_start(today), today + 1)),
        "lastyear" => Some(days(year_start(year_start(today) - 1), year_start(today))),
        _ => None,
    };
    if let Some((from, to)) = named {
        return Ok(ClauseKind::Modified(from, to));
    }

    // `7d`, `24h`, `2w`, `3m`, `1y`: within that long before now.
    if let Some(unit) = lower.chars().last().filter(|c| "hdwmy".contains(*c))
        && let Ok(count) = lower[..lower.len() - 1].parse::<i64>()
    {
        let seconds = match unit {
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            'm' => 30 * 86_400,
            _ => 365 * 86_400,
        };
        let from = clock.now_unix.saturating_sub(count.saturating_mul(seconds));
        return Ok(ClauseKind::Modified(unix_to_filetime(from), MAX));
    }

    let (from, to) = match comparison(value) {
        Cmp::Greater(v) => (clock.day_start(date_span(v)?.1), MAX),
        Cmp::GreaterEq(v) => (clock.day_start(date_span(v)?.0), MAX),
        Cmp::Less(v) => (MIN, clock.day_start(date_span(v)?.0)),
        Cmp::LessEq(v) => (MIN, clock.day_start(date_span(v)?.1)),
        Cmp::Range(a, b) => (clock.day_start(date_span(a)?.0), clock.day_start(date_span(b)?.1)),
        Cmp::Plain(v) => days(date_span(v)?.0, date_span(v)?.1),
    };
    Ok(ClauseKind::Modified(from, to))
}

/// 1 January of the year containing `day`.
fn year_start(day: i64) -> i64 {
    days_from_civil(civil_from_days(day).0, 1, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-13 12:00 UTC, in a UTC-5 zone.
    const CLOCK: Clock = Clock { now_unix: 20_739 * 86_400 + 12 * 3600, utc_offset_secs: -5 * 3600 };

    fn kinds(input: &str) -> Vec<ClauseKind> {
        parse(input, CLOCK).unwrap().clauses.into_iter().map(|c| c.kind).collect()
    }

    fn local_midnight(year: i64, month: u32, day: u32) -> i64 {
        unix_to_filetime(days_from_civil(year, month, day) * 86_400 + 5 * 3600)
    }

    #[test]
    fn text_clauses() {
        assert_eq!(kinds("Report"), [ClauseKind::Name("report".into())]);
        assert_eq!(kinds(r"Src/Main"), [ClauseKind::Path(r"src\main".into())]);
        assert_eq!(kinds(r"C:\Users"), [ClauseKind::Path(r"c:\users".into())]);
        assert_eq!(kinds("*.RS"), [ClauseKind::Glob("*.rs".into())]);
        assert_eq!(kinds(r#""two words" x"#), [
            ClauseKind::Name("two words".into()),
            ClauseKind::Name("x".into())
        ]);
        // Quoted text is never a filter or a wildcard.
        assert_eq!(kinds(r#""size:1""#), [ClauseKind::Name("size:1".into())]);
        assert_eq!(kinds(r#""a*b""#), [ClauseKind::Name("a*b".into())]);
    }

    #[test]
    fn negation_and_incomplete_filters() {
        let q = parse("!ext:tmp foo ext: !", CLOCK).unwrap();
        assert_eq!(q.clauses.len(), 3);
        assert!(q.clauses[0].negate);
        assert_eq!(q.clauses[0].kind, ClauseKind::Ext(vec!["tmp".into()]));
        assert_eq!(q.clauses[2].kind, ClauseKind::Name("!".into()));
    }

    #[test]
    fn filters() {
        assert_eq!(kinds("ext:.RS;toml"), [ClauseKind::Ext(vec!["rs".into(), "toml".into()])]);
        assert_eq!(kinds("type:folder"), [ClauseKind::Folders]);
        assert_eq!(kinds(r#"in:"C:\Program Files""#), [ClauseKind::Within(
            r"C:\Program Files".into()
        )]);
        assert!(matches!(&kinds("type:image")[0], ClauseKind::Ext(e) if e.contains(&"heic".to_owned())));
        assert!(parse("type:nonsense", CLOCK).is_err());
    }

    #[test]
    fn tags_parse_and_resolve_to_entries() {
        assert_eq!(kinds(r#"tag:Urgent tag:"two words" tag:"#), [
            ClauseKind::Tag("Urgent".into()),
            ClauseKind::Tag("two words".into())
        ]);
        let mut query = parse("report !tag:done tag:work", CLOCK).unwrap();
        resolve_tags(&mut query, |name| match name {
            "work" => Ok(vec![9, 3, 3, 7]),
            "done" => Ok(vec![]),
            other => Err(ParseError(format!("no tag {other}"))),
        })
        .unwrap();
        let resolved: Vec<_> = query.clauses.iter().map(|c| (c.negate, c.kind.clone())).collect();
        assert_eq!(resolved, [
            (false, ClauseKind::Name("report".into())),
            (true, ClauseKind::Entries(vec![])),
            (false, ClauseKind::Entries(vec![3, 7, 9])),
        ]);
        let mut unknown = parse("tag:nope", CLOCK).unwrap();
        assert!(resolve_tags(&mut unknown, |_| Err(ParseError("No tag is named \"nope\"".into()))).is_err());
    }

    #[test]
    fn descriptions_parse_and_resolve_to_entries() {
        assert_eq!(kinds(r#"like:"a dog on a sofa" ext:jpg like:"#), [
            ClauseKind::Like("a dog on a sofa".into()),
            ClauseKind::Ext(vec!["jpg".into()])
        ]);
        // A leading ~ makes the whole query one description, filters and all.
        assert_eq!(kinds("  ~ sunset at the beach ext:png"), [ClauseKind::Like("sunset at the beach ext:png".into())]);
        assert_eq!(kinds("~"), []);
        assert_eq!(kinds("~dog"), [ClauseKind::Like("dog".into())]);
        // A tilde elsewhere is ordinary text, as in an 8.3 name.
        assert_eq!(kinds("PROGRA~1"), [ClauseKind::Name("progra~1".into())]);
        assert_eq!(kinds(r#"like:"C:\Photos\dog.jpg""#), [ClauseKind::Like(r"C:\Photos\dog.jpg".into())]);

        let mut query = parse(r#"like:dog !like:cat report"#, CLOCK).unwrap();
        assert_eq!(likes(&query), ["dog"], "a negated description is not one to rank by");
        resolve_likes(&mut query, |description| match description {
            "dog" => Ok(vec![8, 2, 8]),
            _ => Ok(vec![5]),
        })
        .unwrap();
        let resolved: Vec<_> = query.clauses.iter().map(|c| (c.negate, c.kind.clone())).collect();
        assert_eq!(resolved, [
            (false, ClauseKind::Entries(vec![2, 8])),
            (true, ClauseKind::Entries(vec![5])),
            (false, ClauseKind::Name("report".into())),
        ]);
    }

    #[test]
    fn sizes() {
        const GB: u64 = 1 << 30;
        assert_eq!(kinds("size:>1gb"), [ClauseKind::Size(GB + 1, u64::MAX)]);
        assert_eq!(kinds("size:<=1.5GB"), [ClauseKind::Size(0, GB + GB / 2)]);
        assert_eq!(kinds("size:10mb..1gb"), [ClauseKind::Size(10 << 20, GB)]);
        assert_eq!(kinds("size:10mb"), [ClauseKind::Size(10 << 20, (11 << 20) - 1)]);
        assert_eq!(kinds("size:123"), [ClauseKind::Size(123, 123)]);
        assert_eq!(kinds("size:empty"), [ClauseKind::Size(0, 0)]);
        assert!(parse("size:big", CLOCK).is_err());
        assert!(parse("size:>10xb", CLOCK).is_err());
    }

    #[test]
    fn dates_use_local_days() {
        assert_eq!(kinds("dm:today"), [ClauseKind::Modified(
            local_midnight(2026, 10, 13),
            local_midnight(2026, 10, 14)
        )]);
        assert_eq!(kinds("dm:thisweek"), [ClauseKind::Modified(
            local_midnight(2026, 10, 12),
            local_midnight(2026, 10, 14)
        )]);
        assert_eq!(kinds("dm:lastmonth"), [ClauseKind::Modified(
            local_midnight(2026, 9, 1),
            local_midnight(2026, 10, 1)
        )]);
        assert_eq!(kinds("dm:lastyear"), [ClauseKind::Modified(
            local_midnight(2025, 1, 1),
            local_midnight(2026, 1, 1)
        )]);
        assert_eq!(kinds("dm:2026-02"), [ClauseKind::Modified(
            local_midnight(2026, 2, 1),
            local_midnight(2026, 3, 1)
        )]);
        assert_eq!(kinds("dm:>2025"), [ClauseKind::Modified(local_midnight(2026, 1, 1), i64::MAX)]);
        assert_eq!(kinds("dm:2025-12-31..2026-01-01"), [ClauseKind::Modified(
            local_midnight(2025, 12, 31),
            local_midnight(2026, 1, 2)
        )]);
        assert_eq!(kinds("dm:7d"), [ClauseKind::Modified(
            unix_to_filetime(CLOCK.now_unix - 7 * 86_400),
            i64::MAX
        )]);
        assert!(parse("dm:2026-13", CLOCK).is_err());
        assert!(parse("dm:soon", CLOCK).is_err());
    }
}
