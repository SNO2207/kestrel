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
    /// A fact the media library holds about photos and videos (`width:`, `taken:`, `text:`...).
    /// The index knows none of them, so the caller resolves this to [`ClauseKind::Entries`]
    /// with [`resolve_media`] before compiling, as it does tags.
    Media(MediaClause),
    /// Photos and videos that look like this description (`like:"a dog on a sofa"`, or the
    /// whole query after a leading `~`), or like the picture at this path ("Find similar").
    /// Only the caller's models can tell, so it resolves this to [`ClauseKind::Entries`] with
    /// [`resolve_likes`] before compiling, as it does tags.
    Like(String),
}

/// What a media clause asks of the media library.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaClause {
    /// Width as shown, in pixels, inclusive.
    Width(u32, u32),
    /// Height as shown, in pixels, inclusive.
    Height(u32, u32),
    /// When the photo was taken or the video recorded: a half-open range of FILETIMEs, read
    /// as `dm:` reads dates.
    Taken(i64, i64),
    /// The camera's make or model contains this text.
    Camera(String),
    /// Length in milliseconds, inclusive.
    Duration(u64, u64),
    /// `kind:photo` or `kind:video`: what the media library holds of that kind.
    Kind(MediaKind),
    /// `geo:yes`: a GPS position is known; `geo:no`: none is.
    Geo(bool),
    /// Words recognised in an image (`text:costco`, `text:"order 42.17"`).
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
}

/// Replaces every media clause with the entries `resolve` finds for it, sorted.
pub fn resolve_media(
    query: &mut Query,
    mut resolve: impl FnMut(&MediaClause) -> Result<Vec<EntryId>, ParseError>,
) -> Result<(), ParseError> {
    for clause in &mut query.clauses {
        if let ClauseKind::Media(media) = &clause.kind {
            let mut entries = resolve(media)?;
            entries.sort_unstable();
            entries.dedup();
            clause.kind = ClauseKind::Entries(entries);
        }
    }
    Ok(())
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

/// Whether a query has clauses only the media library can answer.
pub fn has_media(query: &Query) -> bool {
    query.clauses.iter().any(|clause| matches!(clause.kind, ClauseKind::Media(_)))
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

/// The media library's keys, whose values are read only once typed.
const MEDIA_KEYS: &[&str] = &["width", "height", "taken", "camera", "duration", "length", "geo", "text"];

fn clause(text: &str, clock: Clock) -> Result<Option<ClauseKind>, ParseError> {
    if let Some((key, value)) = text.split_once(':') {
        if value.is_empty() && MEDIA_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
            return Ok(None);
        }
        let filter = match key.to_ascii_lowercase().as_str() {
            "ext" => Some(extensions(value)),
            // `kind:photo` and `kind:video` are the media library's; any other kind is a type.
            "kind" if media_kind(value).is_some() => media_kind(value).map(|kind| ClauseKind::Media(MediaClause::Kind(kind))),
            "type" | "kind" => Some(category(value)?),
            "width" => {
                let (min, max) = pixels(value)?;
                Some(ClauseKind::Media(MediaClause::Width(min, max)))
            }
            "height" => {
                let (min, max) = pixels(value)?;
                Some(ClauseKind::Media(MediaClause::Height(min, max)))
            }
            "taken" => Some(ClauseKind::Media(taken(value, clock)?)),
            "camera" => Some(ClauseKind::Media(MediaClause::Camera(value.to_owned()))),
            "duration" | "length" => Some(ClauseKind::Media(duration(value)?)),
            "geo" => Some(ClauseKind::Media(MediaClause::Geo(yes_or_no(value)?))),
            "text" => Some(ClauseKind::Media(MediaClause::Text(value.to_owned()))),
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

/// `taken:` reads dates as `dm:` does.
fn taken(value: &str, clock: Clock) -> Result<MediaClause, ParseError> {
    match modified(value, clock)? {
        ClauseKind::Modified(from, to) => Ok(MediaClause::Taken(from, to)),
        _ => err(format!("\"{value}\" is not a date")),
    }
}

/// `kind:photo`, `kind:videos` and their other spellings.
fn media_kind(value: &str) -> Option<MediaKind> {
    match value.to_ascii_lowercase().as_str() {
        "photo" | "photos" => Some(MediaKind::Photo),
        "video" | "videos" => Some(MediaKind::Video),
        _ => None,
    }
}

/// A whole number of pixels, or a comparison or range of them: `1920`, `>1000`, `<=600`,
/// `1000..2000`.
fn pixels(value: &str) -> Result<(u32, u32), ParseError> {
    let number = |text: &str| -> Result<u32, ParseError> {
        let text = text.trim_end_matches("px");
        text.parse::<u32>().map_err(|_| ParseError(format!("\"{text}\" is not a number of pixels")))
    };
    Ok(match comparison(value) {
        Cmp::Greater(v) => (number(v)?.saturating_add(1), u32::MAX),
        Cmp::GreaterEq(v) => (number(v)?, u32::MAX),
        Cmp::Less(v) => match number(v)? {
            0 => (1, 0),
            n => (0, n - 1),
        },
        Cmp::LessEq(v) => (0, number(v)?),
        Cmp::Range(from, to) => (number(from)?, number(to)?),
        Cmp::Plain(v) => (number(v)?, number(v)?),
    })
}

/// A length such as `90`, `90s`, `1.5m` or `2h`, as milliseconds and the milliseconds of its
/// unit. A plain number is seconds.
fn length(text: &str) -> Result<(u64, u64), ParseError> {
    let lower = text.to_ascii_lowercase();
    let digits = lower.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit: u64 = match &lower[digits.len()..] {
        "" | "s" | "sec" | "secs" => 1000,
        "m" | "min" | "mins" => 60_000,
        "h" | "hr" | "hrs" => 3_600_000,
        "ms" => 1,
        _ => return err(format!("Unknown unit of time in \"{text}\" (use s, m or h)")),
    };
    match digits.parse::<f64>() {
        Ok(n) if n >= 0.0 && n.is_finite() => Ok(((n * unit as f64) as u64, unit)),
        _ => err(format!("\"{text}\" is not a length of time")),
    }
}

/// `duration:>1m`, `duration:30s..2m`, `duration:<10s`; plain `duration:1m` means "shown as 1
/// minute": from one minute up to the next whole minute.
fn duration(value: &str) -> Result<MediaClause, ParseError> {
    let (min, max) = match comparison(value) {
        Cmp::Greater(v) => (length(v)?.0.saturating_add(1), u64::MAX),
        Cmp::GreaterEq(v) => (length(v)?.0, u64::MAX),
        Cmp::Less(v) => match length(v)?.0 {
            0 => (1, 0),
            n => (0, n - 1),
        },
        Cmp::LessEq(v) => (0, length(v)?.0),
        Cmp::Range(from, to) => (length(from)?.0, length(to)?.0),
        Cmp::Plain(v) => {
            let (ms, unit) = length(v)?;
            (ms, ms.saturating_add(unit - 1))
        }
    };
    Ok(MediaClause::Duration(min, max))
}

fn yes_or_no(value: &str) -> Result<bool, ParseError> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" | "1" | "on" => Ok(true),
        "no" | "n" | "false" | "0" | "off" => Ok(false),
        other => err(format!("Use geo:yes or geo:no, not geo:{other}")),
    }
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
    fn media_clauses_parse_and_resolve_to_entries() {
        use MediaClause::*;
        let media = |input: &str| -> Vec<MediaClause> {
            kinds(input)
                .into_iter()
                .map(|kind| match kind {
                    ClauseKind::Media(media) => media,
                    other => panic!("{input}: {other:?} is not a media clause"),
                })
                .collect()
        };
        assert_eq!(media("width:1920 height:>1000 width:<=600 height:100..200"), [
            Width(1920, 1920),
            Height(1001, u32::MAX),
            Width(0, 600),
            Height(100, 200)
        ]);
        assert_eq!(media("width:<1"), [Width(0, 0)]);
        assert_eq!(media("duration:>1m duration:30s..2m duration:<10s"), [
            Duration(60_001, u64::MAX),
            Duration(30_000, 120_000),
            Duration(0, 9_999)
        ]);
        assert_eq!(media("duration:90 duration:1m length:1.5h"), [
            Duration(90_000, 90_999),
            Duration(60_000, 119_999),
            Duration(5_400_000, 9_000_000 - 1)
        ]);
        assert_eq!(media(r#"camera:pixel camera:"canon eos" geo:yes geo:No"#), [
            Camera("pixel".into()),
            Camera("canon eos".into()),
            Geo(true),
            Geo(false)
        ]);
        assert_eq!(media(r#"text:costco text:"order 42.17""#), [Text("costco".into()), Text("order 42.17".into())]);
        assert_eq!(media("kind:photo kind:Videos"), [Kind(MediaKind::Photo), Kind(MediaKind::Video)]);
        // `taken:` reads dates as `dm:` does.
        assert_eq!(media("taken:2026-02"), [Taken(local_midnight(2026, 2, 1), local_midnight(2026, 3, 1))]);
        assert_eq!(media("taken:>2025"), [Taken(local_midnight(2026, 1, 1), i64::MAX)]);
        // Other kinds are types, as before; `type:photo` is a type too.
        assert!(matches!(&kinds("kind:image")[0], ClauseKind::Ext(e) if e.contains(&"png".to_owned())));
        assert!(matches!(&kinds("type:photo")[0], ClauseKind::Ext(e) if e.contains(&"jpg".to_owned())));
        // Empty values are ignored while being typed; bad ones are reported.
        assert!(kinds("width: text: taken: geo:").is_empty());
        for bad in ["width:wide", "duration:5x", "geo:maybe", "taken:soon", "height:>"] {
            assert!(parse(bad, CLOCK).is_err(), "{bad}");
        }

        let mut query = parse("beach !geo:yes text:costco", CLOCK).unwrap();
        assert!(has_media(&query));
        resolve_media(&mut query, |clause| match clause {
            Geo(true) => Ok(vec![5, 1, 5]),
            Text(words) if words == "costco" => Ok(vec![9]),
            other => Err(ParseError(format!("unexpected {other:?}"))),
        })
        .unwrap();
        assert!(!has_media(&query));
        let resolved: Vec<_> = query.clauses.iter().map(|c| (c.negate, c.kind.clone())).collect();
        assert_eq!(resolved, [
            (false, ClauseKind::Name("beach".into())),
            (true, ClauseKind::Entries(vec![1, 5])),
            (false, ClauseKind::Entries(vec![9])),
        ]);
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
