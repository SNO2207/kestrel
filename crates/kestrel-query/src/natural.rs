//! Natural-language searches, without any model: phrases such as "videos from last month over
//! 1 GB" or "code changed today in kestrel" are turned into the query language's clauses
//! (`type:video dm:lastmonth size:>1gb`), by rules.
//!
//! [`understand`] reads the text a word at a time and recognises:
//!
//! - kinds: "videos", "photos", "pdfs", "word documents", "rust files", "screenshots"...
//! - dates, after a verb or a preposition: "modified this week", "from last month", "taken in
//!   2023", "since march", "3 days ago", "in the past 2 weeks", "on monday", "recently";
//! - sizes: "over 1 GB", "under 10 mb", "between 1 and 2 gb", and "large", "huge", "small",
//!   "tiny" or "empty" before a kind ("large files");
//! - places: "in downloads", "on the desktop", "in my documents", and projects by name
//!   ("in kestrel"), through a [`Places`] resolver the app provides;
//! - contents: "containing invoice", "that mention 'fn main'".
//!
//! Words that are none of these stay as name terms, and clauses already written in the query
//! language (`ext:pdf`, `*.rs`, `"two words"`) are kept as they are. A single kind word alone
//! ("videos") is not enough to call the text a phrase, since files are often named so: unless
//! a date, a size, a place, contents or a kind of two words ("pdf files") is found too, the
//! text is searched as typed. Each part comes with a label in words, which the search box
//! shows as an editable chip; [`describe`] labels clauses typed in the query language alike.

use crate::time::{Clock, civil_from_days, days_from_civil, month_start, week_start};

/// What the parser asks of the app: where named folders and projects are.
pub trait Places {
    /// The folder a well-known name stands for ("downloads", "documents", "desktop",
    /// "pictures", "music", "videos", "home"), given in lower case.
    fn known_folder(&self, name: &str) -> Option<String>;
    /// The root folder of the project with this name, given in lower case.
    fn project(&self, name: &str) -> Option<String>;
}

/// A resolver that knows no places.
pub struct NoPlaces;

impl Places for NoPlaces {
    fn known_folder(&self, _: &str) -> Option<String> {
        None
    }
    fn project(&self, _: &str) -> Option<String> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// A kind of file: `type:video`, `ext:pdf`.
    Kind,
    Modified,
    Taken,
    Size,
    Place,
    Content,
    /// A word of the name.
    Name,
    /// A clause typed in the query language.
    Filter,
}

/// One part of a phrase as a clause, with what it means in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The clause in the query language, as it is searched: `dm:lastmonth`. Some parts are
    /// two clauses (`type:image screenshot`).
    pub clause: String,
    /// What it means: "Modified last month".
    pub label: String,
    pub kind: PartKind,
    /// The words it was read from, as typed.
    pub phrase: String,
}

/// A phrase, understood.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Understood {
    /// The query to run: the parts' clauses, in order, or the text as typed when it is no
    /// phrase.
    pub query: String,
    pub parts: Vec<Part>,
    /// Whether the text was read as a phrase and translated.
    pub natural: bool,
}

/// A word of the text, with where it came from.
#[derive(Clone, Debug)]
struct Word {
    /// As typed.
    raw: String,
    /// Lower case, without surrounding punctuation; empty for a syntax token.
    lower: String,
    /// A clause in the query language, a quoted string or a path: never part of a phrase.
    syntax: bool,
}

fn words(text: &str) -> Vec<Word> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
            current.push(c);
        } else if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
        .into_iter()
        .map(|raw| {
            let syntax = raw.contains(['"', '*', '?', '\\', '/'])
                || raw.starts_with('!')
                || crate::parse::is_filter(&raw)
                || raw.starts_with('\'') && raw.ends_with('\'') && raw.len() > 1;
            let lower = if syntax {
                String::new()
            } else {
                raw.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | '!' | '?' | '(' | ')')).to_lowercase()
            };
            Word { raw, lower, syntax }
        })
        .collect()
}

// ---------- vocabulary ----------

/// A kind of file: its words, its clause and its label.
struct KindWord {
    words: &'static [&'static str],
    clause: &'static str,
    label: &'static str,
}

const KINDS: &[KindWord] = &[
    KindWord { words: &["video", "videos", "movie", "movies", "film", "films", "clip", "clips"], clause: "type:video", label: "Videos" },
    KindWord {
        words: &["photo", "photos", "picture", "pictures", "image", "images", "pic", "pics", "pix"],
        clause: "type:image",
        label: "Pictures",
    },
    KindWord { words: &["screenshot", "screenshots", "screen shots"], clause: "type:image screenshot", label: "Screenshots" },
    KindWord { words: &["music", "song", "songs", "audio", "track", "tracks", "sound", "sounds"], clause: "type:audio", label: "Music" },
    KindWord { words: &["document", "documents", "doc", "docs"], clause: "type:doc", label: "Documents" },
    KindWord { words: &["pdf", "pdfs"], clause: "ext:pdf", label: "PDFs" },
    KindWord { words: &["word document", "word documents", "word doc", "word docs", "word file", "word files"], clause: "ext:doc;docx", label: "Word documents" },
    KindWord {
        words: &["spreadsheet", "spreadsheets", "workbook", "workbooks"],
        clause: "ext:xls;xlsx;csv;ods",
        label: "Spreadsheets",
    },
    KindWord {
        words: &["presentation", "presentations", "slides", "slide deck", "slide decks", "powerpoint", "powerpoints"],
        clause: "ext:ppt;pptx;odp;key",
        label: "Presentations",
    },
    KindWord {
        words: &["archive", "archives", "zip", "zips", "zip file", "zip files", "compressed file", "compressed files"],
        clause: "type:archive",
        label: "Archives",
    },
    KindWord { words: &["code", "source", "source code", "source file", "source files"], clause: "type:code", label: "Code" },
    KindWord { words: &["script", "scripts"], clause: "ext:ps1;sh;bat;cmd;py;js", label: "Scripts" },
    KindWord {
        words: &["installer", "installers", "setup file", "setup files", "setups"],
        clause: "ext:exe;msi;msix;msixbundle;appx",
        label: "Installers",
    },
    KindWord { words: &["program", "programs", "app", "apps", "executable", "executables"], clause: "type:exe", label: "Programs" },
    KindWord { words: &["text file", "text files", "note", "notes"], clause: "ext:txt;md;log", label: "Text files" },
    KindWord { words: &["ebook", "ebooks", "book", "books"], clause: "ext:epub;mobi;azw3;pdf", label: "Books" },
    KindWord { words: &["folder", "folders", "directory", "directories", "dir", "dirs"], clause: "type:folder", label: "Folders" },
    KindWord { words: &["file", "files", "item", "items"], clause: "type:file", label: "Files" },
];

/// Languages and formats named before "files" ("rust files"), and their extensions.
const LANGUAGES: &[(&str, &str, &str)] = &[
    ("rust", "rs", "Rust files"),
    ("python", "py", "Python files"),
    ("typescript", "ts;tsx", "TypeScript files"),
    ("javascript", "js;jsx;mjs;cjs", "JavaScript files"),
    ("c#", "cs", "C# files"),
    ("csharp", "cs", "C# files"),
    ("c++", "cpp;cc;cxx;hpp;h", "C++ files"),
    ("go", "go", "Go files"),
    ("java", "java", "Java files"),
    ("kotlin", "kt;kts", "Kotlin files"),
    ("markdown", "md;markdown", "Markdown files"),
    ("json", "json", "JSON files"),
    ("yaml", "yaml;yml", "YAML files"),
    ("html", "html;htm", "HTML files"),
    ("css", "css;scss", "CSS files"),
    ("powershell", "ps1;psm1", "PowerShell files"),
    ("excel", "xls;xlsx", "Excel files"),
    ("word", "doc;docx", "Word documents"),
];

/// Extensions recognised before "files" ("mp3 files", "log files").
const EXTENSIONS: &[&str] = &[
    "txt", "md", "log", "csv", "json", "xml", "yaml", "yml", "toml", "ini", "rs", "py", "ts", "tsx", "js", "jsx", "c", "h",
    "cpp", "cs", "go", "java", "kt", "rb", "php", "sh", "ps1", "bat", "html", "css", "sql", "pdf", "doc", "docx", "xls",
    "xlsx", "ppt", "pptx", "odt", "rtf", "epub", "jpg", "jpeg", "png", "gif", "webp", "heic", "svg", "bmp", "tif", "tiff",
    "raw", "dng", "psd", "mp4", "mkv", "mov", "avi", "wmv", "webm", "mp3", "flac", "wav", "m4a", "ogg", "zip", "7z", "rar",
    "tar", "gz", "iso", "exe", "msi", "dll", "lnk", "url", "ttf", "otf",
];

/// Words left out of a phrase: they join its parts, or are verbs and prepositions left over
/// when what followed them was no date or place.
const FILLER: &[&str] = &[
    "the", "a", "an", "my", "all", "any", "of", "that", "which", "were", "was", "are", "is", "and", "with", "find", "show",
    "me", "list", "get", "i", "have", "has", "ones", "some", "every", "folder", "for", "to", "by", "in", "on", "from", "at",
    "inside", "under", "within", "since", "before", "after", "during", "modified", "changed", "edited", "updated", "saved",
    "created", "made", "downloaded", "added", "written", "taken", "shot", "captured", "photographed",
];

const MONTHS: &[&[&str]] = &[
    &["january", "jan"],
    &["february", "feb"],
    &["march", "mar"],
    &["april", "apr"],
    &["may"],
    &["june", "jun"],
    &["july", "jul"],
    &["august", "aug"],
    &["september", "sep", "sept"],
    &["october", "oct"],
    &["november", "nov"],
    &["december", "dec"],
];
const MONTH_NAMES: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: &[&[&str]] = &[
    &["monday", "mon"],
    &["tuesday", "tue", "tues"],
    &["wednesday", "wed"],
    &["thursday", "thu", "thur", "thurs"],
    &["friday", "fri"],
    &["saturday", "sat"],
    &["sunday", "sun"],
];

/// Verbs that say which date a phrase is about.
const MODIFIED_VERBS: &[&str] = &["modified", "changed", "edited", "updated", "saved", "created", "made", "downloaded", "added", "written"];
const TAKEN_VERBS: &[&str] = &["taken", "shot", "captured", "photographed"];
/// Prepositions before a date: the plain ones, then those that bound it.
const DATE_PREPOSITIONS: &[&str] = &["from", "in", "on", "during", "of", "at"];
const PLACE_PREPOSITIONS: &[&str] = &["in", "inside", "under", "within", "from", "on", "at"];
const KNOWN_FOLDERS: &[(&str, &str)] = &[
    ("downloads", "downloads"),
    ("download", "downloads"),
    ("documents", "documents"),
    ("docs", "documents"),
    ("desktop", "desktop"),
    ("pictures", "pictures"),
    ("photos", "pictures"),
    ("music", "music"),
    ("videos", "videos"),
    ("home", "home"),
    ("profile", "home"),
];

fn number_word(word: &str) -> Option<i64> {
    Some(match word {
        "a" | "an" | "one" => 1,
        "two" | "couple" => 2,
        "three" | "few" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        _ => return word.parse::<i64>().ok().filter(|n| (1..=10_000).contains(n)),
    })
}

/// A unit of time: hour, day, week, month or year, as the query's `7d` writes it.
fn time_unit(word: &str) -> Option<char> {
    Some(match word {
        "hour" | "hours" | "hr" | "hrs" => 'h',
        "day" | "days" => 'd',
        "week" | "weeks" | "wk" | "wks" => 'w',
        "month" | "months" | "mo" | "mos" => 'm',
        "year" | "years" | "yr" | "yrs" => 'y',
        _ => return None,
    })
}

fn unit_name(unit: char, count: i64) -> String {
    let name = match unit {
        'h' => "hour",
        'd' => "day",
        'w' => "week",
        'm' => "month",
        _ => "year",
    };
    if count == 1 { name.to_owned() } else { format!("{count} {name}s") }
}

/// A size unit's multiplier name as the query writes it.
fn size_unit(word: &str) -> Option<&'static str> {
    Some(match word {
        "b" | "byte" | "bytes" => "b",
        "k" | "kb" | "kib" | "kilobyte" | "kilobytes" => "kb",
        "m" | "mb" | "mib" | "meg" | "megs" | "megabyte" | "megabytes" => "mb",
        "g" | "gb" | "gib" | "gig" | "gigs" | "gigabyte" | "gigabytes" => "gb",
        "t" | "tb" | "tib" | "terabyte" | "terabytes" => "tb",
        _ => return None,
    })
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || c == '.') && text.chars().filter(|&c| c == '.').count() <= 1 && text != "."
}

/// `1.5gb` or `1.5` `gb` at `at`: the amount in the query's words (`1.5gb`), and how many words.
fn size_at(words: &[Word], at: usize) -> Option<(String, usize)> {
    let word = &words.get(at)?.lower;
    let digits: String = word.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if !is_number(&digits) {
        return None;
    }
    let rest = &word[digits.len()..];
    if !rest.is_empty() {
        return size_unit(rest).map(|unit| (format!("{digits}{unit}"), 1));
    }
    let unit = size_unit(&words.get(at + 1)?.lower)?;
    Some((format!("{digits}{unit}"), 2))
}

/// `1.5gb` as people write it: `1.5 GB`.
fn size_label(amount: &str) -> String {
    let digits: String = amount.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let unit = &amount[digits.len()..];
    let unit = match unit {
        "b" => "bytes".to_owned(),
        other => other.to_uppercase(),
    };
    format!("{digits} {unit}")
}

// ---------- dates ----------

/// A date or span of dates a phrase names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum When {
    /// One of the query's named spans: `today`, `lastmonth`...
    Named(&'static str),
    /// Within this long before now: `7d`.
    Within(i64, char),
    Year(i64),
    Month(i64, u32),
    /// A single day, as days since 1970.
    Day(i64),
    /// Days `from..=to`.
    Days(i64, i64),
}

fn iso(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

impl When {
    /// The value of a `dm:` clause for this date.
    fn value(&self) -> String {
        match self {
            When::Named(name) => (*name).to_owned(),
            When::Within(count, unit) => format!("{count}{unit}"),
            When::Year(year) => format!("{year:04}"),
            When::Month(year, month) => format!("{year:04}-{month:02}"),
            When::Day(day) => iso(*day),
            When::Days(from, to) => format!("{}..{}", iso(*from), iso(*to)),
        }
    }

    /// The first day of the span, as a `dm:` value of the same precision.
    fn start(&self, clock: Clock) -> String {
        let today = clock.today();
        match self {
            When::Named(name) => iso(match *name {
                "today" => today,
                "yesterday" => today - 1,
                "thisweek" => week_start(today),
                "lastweek" => week_start(today) - 7,
                "thismonth" => month_start(today, 0),
                "lastmonth" => month_start(today, -1),
                "thisyear" => crate::parse::year_start(today),
                _ => crate::parse::year_start(crate::parse::year_start(today) - 1),
            }),
            When::Within(count, unit) => iso(today - within_days(*count, *unit)),
            When::Year(_) | When::Month(..) | When::Day(_) => self.value(),
            When::Days(from, _) => iso(*from),
        }
    }

    /// The last day of the span, as a `dm:` value of the same precision.
    fn end(&self, clock: Clock) -> String {
        let today = clock.today();
        match self {
            When::Named(name) => iso(match *name {
                "yesterday" => today - 1,
                "lastweek" => week_start(today) - 1,
                "lastmonth" => month_start(today, 0) - 1,
                "lastyear" => crate::parse::year_start(today) - 1,
                _ => today,
            }),
            When::Within(..) => iso(today),
            When::Year(_) | When::Month(..) | When::Day(_) => self.value(),
            When::Days(_, to) => iso(*to),
        }
    }

    /// The date in words, after "Modified" or "Taken".
    fn words(&self) -> String {
        match self {
            When::Named(name) => match *name {
                "today" => "today".into(),
                "yesterday" => "yesterday".into(),
                "thisweek" => "this week".into(),
                "lastweek" => "last week".into(),
                "thismonth" => "this month".into(),
                "lastmonth" => "last month".into(),
                "thisyear" => "this year".into(),
                _ => "last year".into(),
            },
            When::Within(count, unit) => format!("in the last {}", unit_name(*unit, *count)),
            When::Year(year) => format!("in {year}"),
            When::Month(year, month) => format!("in {} {year}", MONTH_NAMES[*month as usize - 1]),
            When::Day(day) => format!("on {}", day_words(*day)),
            When::Days(from, to) => format!("from {} to {}", day_words(*from), day_words(*to)),
        }
    }
}

fn within_days(count: i64, unit: char) -> i64 {
    match unit {
        'h' => 1,
        'd' => count,
        'w' => count * 7,
        'm' => count * 30,
        _ => count * 365,
    }
}

fn day_words(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{d} {} {y}", MONTH_NAMES[m as usize - 1])
}

fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

fn month_of(word: &str) -> Option<u32> {
    MONTHS.iter().position(|names| names.contains(&word)).map(|i| i as u32 + 1)
}

fn year_of(word: &str) -> Option<i64> {
    let year: i64 = word.parse().ok()?;
    (word.len() == 4 && (1970..=2100).contains(&year)).then_some(year)
}

/// `2024-03-05`, `2024-03` or `2024` written as a date.
fn iso_date(word: &str) -> Option<When> {
    let mut parts = word.split('-');
    let year = year_of(parts.next()?)?;
    let Some(month) = parts.next() else { return Some(When::Year(year)) };
    let month: u32 = month.parse().ok().filter(|m| (1..=12).contains(m))?;
    let Some(day) = parts.next() else { return Some(When::Month(year, month)) };
    let day: u32 = day.parse().ok().filter(|d| (1..=31).contains(d))?;
    parts.next().is_none().then(|| When::Day(days_from_civil(year, month, day)))
}

/// How a date phrase bounds the date it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    During,
    Since,
    Before,
    After,
}

/// A date at `at` (after any verb and preposition): what it names and how many words it took.
fn date_at(words: &[Word], at: usize, clock: Clock, introduced: bool) -> Option<(When, usize)> {
    let word = |i: usize| words.get(at + i).filter(|w| !w.syntax).map(|w| w.lower.as_str()).unwrap_or("");
    let today = clock.today();
    // Unambiguous by themselves.
    match word(0) {
        "today" => return Some((When::Named("today"), 1)),
        "yesterday" => return Some((When::Named("yesterday"), 1)),
        "recent" | "recently" | "lately" => return Some((When::Within(7, 'd'), 1)),
        _ => {}
    }
    // "this week", "last month", "past year", "previous week"; "the last 3 days", "past 24 hours".
    let (the, lead) = if word(0) == "the" { (1, word(1)) } else { (0, word(0)) };
    if matches!(lead, "this" | "last" | "past" | "previous" | "current") {
        let next = word(the + 1);
        if let Some(unit) = time_unit(next) {
            let rolling = the == 1 || lead == "past";
            return Some((
                match (lead, unit, rolling) {
                    (_, 'h', _) => When::Within(1, 'h'),
                    (_, 'd', _) => When::Named(if lead == "this" || lead == "current" { "today" } else { "yesterday" }),
                    (_, unit, true) if lead != "this" && lead != "current" => When::Within(1, unit),
                    ("this" | "current", 'w', _) => When::Named("thisweek"),
                    ("this" | "current", 'm', _) => When::Named("thismonth"),
                    ("this" | "current", _, _) => When::Named("thisyear"),
                    (_, 'w', _) => When::Named("lastweek"),
                    (_, 'm', _) => When::Named("lastmonth"),
                    _ => When::Named("lastyear"),
                },
                the + 2,
            ));
        }
        if lead != "this"
            && let Some(count) = number_word(next)
            && let Some(unit) = time_unit(word(the + 2))
        {
            return Some((When::Within(count, unit), the + 3));
        }
        // "last monday", "last march".
        if lead == "last" || lead == "previous" {
            if let Some(day) = WEEKDAYS.iter().position(|names| names.contains(&next)) {
                let back = (weekday(today) + 7 - day) % 7;
                return Some((When::Day(today - if back == 0 { 7 } else { back as i64 }), the + 2));
            }
            if let Some(month) = month_of(next) {
                let (year, current, _) = civil_from_days(today);
                return Some((When::Month(if month < current { year } else { year - 1 }, month), the + 2));
            }
        }
    }
    // "3 days ago", "a week ago".
    if let Some(count) = number_word(word(0))
        && let Some(unit) = time_unit(word(1))
        && word(2) == "ago"
    {
        return Some((
            match unit {
                'h' | 'd' => When::Day(today - if unit == 'h' { 0 } else { count }),
                'w' => {
                    let start = week_start(today - 7 * count);
                    When::Days(start, start + 6)
                }
                'm' => {
                    let (year, month, _) = civil_from_days(month_start(today, -count));
                    When::Month(year, month)
                }
                _ => When::Year(civil_from_days(today).0 - count),
            },
            3,
        ));
    }
    // The rest could be part of a name ("march madness", "report 2024") unless a verb or a
    // preposition introduces it.
    if !introduced {
        return None;
    }
    if let Some(when) = iso_date(word(0)) {
        return Some((when, 1));
    }
    if let Some(month) = month_of(word(0)) {
        if let Some(year) = year_of(word(1)) {
            return Some((When::Month(year, month), 2));
        }
        // The latest such month, this one included.
        let (year, current, _) = civil_from_days(today);
        return Some((When::Month(if month <= current { year } else { year - 1 }, month), 1));
    }
    if let Some(day) = WEEKDAYS.iter().position(|names| names.contains(&word(0))) {
        let back = (weekday(today) + 7 - day) % 7;
        return Some((When::Day(today - back as i64), 1));
    }
    None
}

// ---------- the parser ----------

struct Reader<'a> {
    words: &'a [Word],
    clock: Clock,
    places: &'a dyn Places,
}

/// What a recognised phrase adds, and whether it is enough to call the text a phrase.
struct Found {
    parts: Vec<Part>,
    used: usize,
    strong: bool,
}

impl Reader<'_> {
    fn plain(&self, at: usize) -> Option<&str> {
        self.words.get(at).filter(|w| !w.syntax).map(|w| w.lower.as_str())
    }

    fn phrase(&self, at: usize, used: usize) -> String {
        self.words[at..at + used].iter().map(|w| w.raw.as_str()).collect::<Vec<_>>().join(" ")
    }

    fn part(&self, at: usize, used: usize, kind: PartKind, clause: String, label: String) -> Part {
        Part { clause, label, kind, phrase: self.phrase(at, used) }
    }

    /// A kind of file at `at`: its part and how many words it took.
    fn kind(&self, at: usize) -> Option<(Part, usize)> {
        let one = self.plain(at)?;
        let two = self.plain(at + 1).map(|next| format!("{one} {next}"));
        // Two words first: "word documents", "rust files", "mp3 files".
        if let Some(two) = &two {
            if let Some(kind) = KINDS.iter().find(|kind| kind.words.contains(&two.as_str())) {
                return Some((self.part(at, 2, PartKind::Kind, kind.clause.into(), kind.label.into()), 2));
            }
            if matches!(self.plain(at + 1), Some("file" | "files")) {
                if let Some((_, exts, label)) = LANGUAGES.iter().find(|(name, ..)| *name == one) {
                    return Some((self.part(at, 2, PartKind::Kind, format!("ext:{exts}"), (*label).into()), 2));
                }
                let ext = one.trim_start_matches('.');
                if EXTENSIONS.contains(&ext) {
                    return Some((self.part(at, 2, PartKind::Kind, format!("ext:{ext}"), format!("{} files", ext.to_uppercase())), 2));
                }
            }
        }
        let kind = KINDS.iter().find(|kind| kind.words.contains(&one))?;
        Some((self.part(at, 1, PartKind::Kind, kind.clause.into(), kind.label.into()), 1))
    }

    /// A date phrase: an optional verb, an optional preposition, and a date.
    fn date(&self, at: usize) -> Option<Found> {
        let mut i = at;
        let mut taken = false;
        let mut verb = false;
        if let Some(word) = self.plain(i) {
            if TAKEN_VERBS.contains(&word) {
                taken = true;
                verb = true;
                i += 1;
            } else if MODIFIED_VERBS.contains(&word) {
                verb = true;
                i += 1;
            }
        }
        let mut bound = Bound::During;
        let mut preposition = false;
        match self.plain(i) {
            Some("since") => (bound, preposition) = (Bound::Since, true),
            Some("before") => (bound, preposition) = (Bound::Before, true),
            Some("after") => (bound, preposition) = (Bound::After, true),
            Some("over" | "within") if matches!(self.plain(i + 1), Some("the" | "last" | "past")) => preposition = true,
            Some(word) if DATE_PREPOSITIONS.contains(&word) => preposition = true,
            _ => {}
        }
        if preposition {
            i += 1;
        }
        // "older than 2 weeks", "newer than 3 days".
        if !preposition && matches!(self.plain(i), Some("older" | "newer")) && self.plain(i + 1) == Some("than") {
            let older = self.plain(i) == Some("older");
            let count = number_word(self.plain(i + 2)?)?;
            let unit = time_unit(self.plain(i + 3)?)?;
            let used = i + 4 - at;
            let (clause, label) = if older {
                let cutoff = iso(self.clock.today() - within_days(count, unit));
                (format!("dm:<{cutoff}"), format!("Not changed in the last {}", unit_name(unit, count)))
            } else {
                (format!("dm:{count}{unit}"), format!("Modified in the last {}", unit_name(unit, count)))
            };
            return Some(Found { parts: vec![self.part(at, used, PartKind::Modified, clause, label)], used, strong: true });
        }
        let (when, length) = date_at(self.words, i, self.clock, verb || preposition)?;
        let used = i + length - at;
        let (key, kind, word) = if taken { ("taken", PartKind::Taken, "Taken") } else { ("dm", PartKind::Modified, "Modified") };
        let (start, end) = (when.start(self.clock), when.end(self.clock));
        let (value, label) = match bound {
            Bound::During => (when.value(), format!("{word} {}", when.words())),
            Bound::Since => (format!(">={start}"), format!("{word} since {}", span_words(&start))),
            Bound::Before => (format!("<{start}"), format!("{word} before {}", span_words(&start))),
            Bound::After => (format!(">{end}"), format!("{word} after {}", span_words(&end))),
        };
        Some(Found { parts: vec![self.part(at, used, kind, format!("{key}:{value}"), label)], used, strong: true })
    }

    /// A size phrase: a comparison and an amount, or a size adjective before a kind.
    fn size(&self, at: usize) -> Option<Found> {
        let word = self.plain(at)?;
        let next = self.plain(at + 1).unwrap_or("");
        let (op, skip) = match (word, next) {
            ("over" | "above" | "exceeding", _) => (">", 1),
            ("more" | "larger" | "bigger" | "greater", "than") => (">", 2),
            ("at", "least") => (">=", 2),
            ("under" | "below", _) => ("<", 1),
            ("less" | "smaller" | "fewer", "than") => ("<", 2),
            ("at", "most") | ("up", "to") => ("<=", 2),
            ("between", _) => ("..", 1),
            ("large" | "big" | "huge" | "massive" | "giant" | "enormous" | "small" | "tiny" | "empty", _) => {
                // Only before a kind ("large files", "huge videos"): "small business" is a name.
                self.kind(at + 1)?;
                let (clause, label) = match word {
                    "large" | "big" => ("size:>100mb", "Over 100 MB"),
                    "huge" | "massive" | "giant" | "enormous" => ("size:>1gb", "Over 1 GB"),
                    "small" => ("size:<1mb", "Under 1 MB"),
                    "tiny" => ("size:<16kb", "Under 16 KB"),
                    _ => ("size:empty", "Empty"),
                };
                return Some(Found { parts: vec![self.part(at, 1, PartKind::Size, clause.into(), label.into())], used: 1, strong: true });
            }
            _ => return None,
        };
        if op == ".." {
            // "between 1 and 2 gb", "between 500 mb and 2 gb".
            let first = self.plain(at + 1)?;
            let (low, low_used) = size_at(self.words, at + 1).or_else(|| is_number(first).then(|| (first.to_owned(), 1)))?;
            if self.plain(at + 1 + low_used) != Some("and") {
                return None;
            }
            let (high, high_used) = size_at(self.words, at + 2 + low_used)?;
            let unit: String = high.chars().skip_while(|c| c.is_ascii_digit() || *c == '.').collect();
            let low = if low.chars().all(|c| c.is_ascii_digit() || c == '.') { format!("{low}{unit}") } else { low };
            let used = 2 + low_used + high_used;
            let label = format!("{} to {}", size_label(&low), size_label(&high));
            return Some(Found {
                parts: vec![self.part(at, used, PartKind::Size, format!("size:{low}..{high}"), label)],
                used,
                strong: true,
            });
        }
        let (amount, amount_used) = size_at(self.words, at + skip)?;
        let used = skip + amount_used;
        let shown = size_label(&amount);
        let label = match op {
            ">" => format!("Over {shown}"),
            ">=" => format!("{shown} or more"),
            "<" => format!("Under {shown}"),
            _ => format!("{shown} or less"),
        };
        Some(Found { parts: vec![self.part(at, used, PartKind::Size, format!("size:{op}{amount}"), label)], used, strong: true })
    }

    /// A place: a preposition, then a known folder or a project.
    fn place(&self, at: usize) -> Option<Found> {
        let preposition = self.plain(at)?;
        if !PLACE_PREPOSITIONS.contains(&preposition) {
            return None;
        }
        let mut i = at + 1;
        while matches!(self.plain(i), Some("my" | "the" | "our")) {
            i += 1;
        }
        let first = self.plain(i)?;
        let trailing = |end: usize| usize::from(matches!(self.plain(end), Some("folder" | "directory" | "project" | "repo" | "repository")));
        let found = |path: String, length: usize| {
            let used = i + length + trailing(i + length) - at;
            let label = format!("In {}", folder_title(&path));
            Found { parts: vec![self.part(at, used, PartKind::Place, within(&path), label)], used, strong: true }
        };
        // Projects, by their name: the longest run of up to four words that names one, written
        // with spaces, hyphens, underscores or nothing between them. A name of several words
        // wins over a known folder named by its first ("home lab" over "home").
        let project = |length: usize| -> Option<String> {
            let names: Vec<&str> = (i..i + length).map(|j| self.plain(j)).collect::<Option<_>>()?;
            [" ", "-", "_", ""].iter().find_map(|joiner| self.places.project(&names.join(joiner)))
        };
        for length in (2..=4).rev() {
            if let Some(root) = project(length) {
                return Some(found(root, length));
            }
        }
        if let Some((_, known)) = KNOWN_FOLDERS.iter().find(|(word, _)| *word == first)
            && let Some(path) = self.places.known_folder(known)
        {
            return Some(found(path, 1));
        }
        project(1).map(|root| found(root, 1))
    }

    /// Contents: "containing invoice", "that mention 'fn main'", "with the text TODO".
    fn content(&self, at: usize) -> Option<Found> {
        let mut i = at;
        match self.plain(i)? {
            "containing" | "contains" | "contain" | "mentioning" | "mentions" | "mention" | "saying" | "says" => i += 1,
            "with" | "including" if matches!(self.plain(i + 1), Some("text" | "the")) => {
                i += if self.plain(i + 1) == Some("the") { 2 } else { 1 };
                if self.plain(i) != Some("text") {
                    return None;
                }
                i += 1;
            }
            _ => return None,
        }
        if self.plain(i) == Some("the") && self.plain(i + 1) == Some("text") {
            i += 2;
        }
        let target = self.words.get(i)?;
        let text = target.raw.trim_matches(|c| c == '"' || c == '\'');
        if text.is_empty() || (target.syntax && !target.raw.starts_with(['"', '\''])) {
            return None;
        }
        let used = i + 1 - at;
        let clause = if text.contains(char::is_whitespace) || text.contains('"') {
            format!("content:\"{}\"", text.replace('"', ""))
        } else {
            format!("content:{text}")
        };
        Some(Found {
            parts: vec![self.part(at, used, PartKind::Content, clause, format!("Contains \u{201c}{text}\u{201d}"))],
            used,
            strong: true,
        })
    }
}

/// `in:"…"`, quoted so a path with spaces stays one clause.
fn within(path: &str) -> String {
    format!("in:\"{path}\"")
}

/// The last part of a folder's path, for a label.
fn folder_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('\\');
    trimmed.rsplit('\\').next().filter(|name| !name.is_empty()).unwrap_or(trimmed).to_owned()
}

/// Reads `text` as a phrase, with dates relative to `clock` and places found by `places`.
pub fn understand(text: &str, clock: Clock, places: &dyn Places) -> Understood {
    let words = words(text);
    let reader = Reader { words: &words, clock, places };
    let mut parts: Vec<Part> = Vec::new();
    let mut strong = false;
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        if word.syntax {
            parts.push(Part { clause: word.raw.clone(), label: describe(&word.raw), kind: PartKind::Filter, phrase: word.raw.clone() });
            at += 1;
            continue;
        }
        let found = reader
            .content(at)
            .or_else(|| reader.size(at))
            .or_else(|| reader.date(at))
            .or_else(|| reader.place(at))
            .or_else(|| reader.kind(at).map(|(part, used)| Found { strong: used > 1, parts: vec![part], used }));
        match found {
            Some(found) => {
                strong |= found.strong;
                parts.extend(found.parts);
                at += found.used;
            }
            None => {
                parts.push(Part { clause: word.raw.clone(), label: word.raw.clone(), kind: PartKind::Name, phrase: word.raw.clone() });
                at += 1;
            }
        }
    }
    if !strong {
        return as_typed(text);
    }
    // Joining words are dropped, and "files" after a kind says nothing more.
    let kinds = parts.iter().filter(|part| part.kind == PartKind::Kind).count();
    parts.retain(|part| match part.kind {
        PartKind::Name => !FILLER.contains(&part.label.to_lowercase().trim_matches(|c: char| c.is_ascii_punctuation())),
        PartKind::Kind => !(kinds > 1 && part.clause == "type:file"),
        _ => true,
    });
    merge_kinds(&mut parts);
    let query = parts.iter().map(|part| part.clause.as_str()).collect::<Vec<_>>().join(" ");
    Understood { query, parts, natural: true }
}

/// The text as typed, with a label for each clause in it.
fn as_typed(text: &str) -> Understood {
    let parts = words(text)
        .into_iter()
        .map(|word| {
            let kind = if word.syntax && crate::parse::is_filter(&word.raw) { PartKind::Filter } else { PartKind::Name };
            Part { label: if kind == PartKind::Filter { describe(&word.raw) } else { word.raw.clone() }, clause: word.raw.clone(), kind, phrase: word.raw }
        })
        .collect();
    Understood { query: text.trim().to_owned(), parts, natural: false }
}

/// Several kinds ("videos and photos") would each have to match; they become one list of
/// extensions that any may match.
fn merge_kinds(parts: &mut Vec<Part>) {
    let kinds: Vec<usize> = parts.iter().enumerate().filter(|(_, part)| part.kind == PartKind::Kind).map(|(i, _)| i).collect();
    if kinds.len() < 2 {
        return;
    }
    let mut exts: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    for &i in &kinds {
        for clause in parts[i].clause.split_whitespace() {
            match clause.split_once(':') {
                Some(("ext", list)) => exts.extend(list.split(';').map(str::to_owned)),
                Some(("type", category)) => exts.extend(category_extensions(category)),
                _ => extra.push(clause.to_owned()),
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    exts.retain(|ext| seen.insert(ext.clone()));
    let labels: Vec<String> = kinds.iter().map(|&i| parts[i].label.clone()).collect();
    let label = match labels.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {}", rest.join(", "), last.to_lowercase()),
        _ => labels.join(""),
    };
    let phrase = kinds.iter().map(|&i| parts[i].phrase.clone()).collect::<Vec<_>>().join(" ");
    let mut clause = format!("ext:{}", exts.join(";"));
    for other in extra {
        clause.push(' ');
        clause.push_str(&other);
    }
    let merged = Part { clause, label, kind: PartKind::Kind, phrase };
    let first = kinds[0];
    for &i in kinds.iter().rev() {
        parts.remove(i);
    }
    parts.insert(first, merged);
}

/// The extensions of a `type:` category, as `parse` knows them.
fn category_extensions(category: &str) -> Vec<String> {
    match crate::parse::parse(&format!("type:{category}"), Clock { now_unix: 0, utc_offset_secs: 0 }) {
        Ok(query) => match query.clauses.into_iter().next().map(|clause| clause.kind) {
            Some(crate::ClauseKind::Ext(exts)) => exts,
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

// ---------- labels for clauses ----------

/// What a clause in the query language means, in words: `dm:lastmonth` is "Modified last
/// month", `size:>1gb` "Over 1 GB". Unknown clauses are shown as written.
pub fn describe(clause: &str) -> String {
    if let Some(rest) = clause.strip_prefix('!').filter(|rest| !rest.is_empty()) {
        let inner = describe(rest);
        let mut chars = inner.chars();
        let lowered = match chars.next() {
            Some(first) if !inner.starts_with("PDF") && !inner.chars().nth(1).is_some_and(char::is_uppercase) => {
                first.to_lowercase().chain(chars).collect()
            }
            _ => inner,
        };
        return format!("Not {lowered}");
    }
    let Some((key, value)) = clause.split_once(':').filter(|_| crate::parse::is_filter(clause)) else {
        if clause.contains(['*', '?']) {
            return format!("Name like {clause}");
        }
        return clause.trim_matches('"').to_owned();
    };
    let value = value.trim_matches('"');
    match key.to_ascii_lowercase().as_str() {
        "type" | "kind" => match value.to_ascii_lowercase().as_str() {
            "image" | "images" | "picture" | "pictures" | "photo" | "photos" | "pic" => "Pictures".into(),
            "video" | "videos" | "movie" | "movies" => "Videos".into(),
            "audio" | "music" | "sound" => "Music".into(),
            "doc" | "docs" | "document" | "documents" => "Documents".into(),
            "archive" | "archives" | "compressed" | "zip" => "Archives".into(),
            "code" | "source" => "Code".into(),
            "exe" | "app" | "program" | "executable" => "Programs".into(),
            "folder" | "folders" | "dir" => "Folders".into(),
            "file" | "files" => "Files".into(),
            other => format!("Type {other}"),
        },
        "ext" => {
            let exts: Vec<String> = value.split([';', ',', '|']).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches('.').to_uppercase()).collect();
            match exts.len() {
                0 => clause.to_owned(),
                1..=3 => format!("{} files", exts.join(", ")),
                n => format!("{} and {} more types", exts[..2].join(", "), n - 2),
            }
        }
        "size" => size_words(value),
        "dm" | "modified" | "date" => format!("Modified {}", date_words(value)),
        "taken" | "dt" => format!("Taken {}", date_words(value)),
        "in" | "folder" => format!("In {}", folder_title(value)),
        "content" => format!("Contains \u{201c}{value}\u{201d}"),
        "tag" => format!("Tagged {value}"),
        "regex" => format!("Name matches {value}"),
        "path" => format!("Path contains {value}"),
        "name" => format!("Name contains {value}"),
        _ => clause.to_owned(),
    }
}

fn size_words(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let named = match lower.as_str() {
        "empty" => Some("Empty"),
        "tiny" => Some("Tiny (up to 10 KB)"),
        "small" => Some("Small (10 to 100 KB)"),
        "medium" => Some("Medium (100 KB to 1 MB)"),
        "large" => Some("Large (1 to 16 MB)"),
        "huge" => Some("Huge (16 to 128 MB)"),
        "gigantic" => Some("Gigantic (over 128 MB)"),
        _ => None,
    };
    if let Some(named) = named {
        return named.into();
    }
    let amount = |text: &str| {
        let unitless = text.chars().all(|c| c.is_ascii_digit() || c == '.');
        if unitless { format!("{text} bytes") } else { size_label(text) }
    };
    if let Some(v) = lower.strip_prefix(">=") {
        format!("{} or more", amount(v))
    } else if let Some(v) = lower.strip_prefix("<=") {
        format!("{} or less", amount(v))
    } else if let Some(v) = lower.strip_prefix('>') {
        format!("Over {}", amount(v))
    } else if let Some(v) = lower.strip_prefix('<') {
        format!("Under {}", amount(v))
    } else if let Some((from, to)) = lower.split_once("..") {
        format!("{} to {}", amount(from), amount(to))
    } else {
        format!("About {}", amount(lower.trim_start_matches('=')))
    }
}

/// `2026`, `2026-03` or `2026-03-05` in words: "2026", "March 2026", "5 March 2026".
fn span_words(text: &str) -> String {
    match iso_date(text) {
        Some(When::Year(year)) => year.to_string(),
        Some(When::Month(year, month)) => format!("{} {year}", MONTH_NAMES[month as usize - 1]),
        Some(When::Day(day)) => day_words(day),
        _ => text.to_owned(),
    }
}

/// A `dm:` value in words, after "Modified".
fn date_words(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let named = match lower.as_str() {
        "today" => Some("today"),
        "yesterday" => Some("yesterday"),
        "thisweek" => Some("this week"),
        "lastweek" => Some("last week"),
        "thismonth" => Some("this month"),
        "lastmonth" => Some("last month"),
        "thisyear" => Some("this year"),
        "lastyear" => Some("last year"),
        _ => None,
    };
    if let Some(named) = named {
        return named.into();
    }
    if let Some(unit) = lower.chars().last().filter(|c| "hdwmy".contains(*c))
        && let Ok(count) = lower[..lower.len() - 1].parse::<i64>()
    {
        return format!("in the last {}", unit_name(unit, count));
    }
    let span = span_words;
    let during = |text: &str| match iso_date(text) {
        Some(When::Day(_)) => format!("on {}", span(text)),
        _ => format!("in {}", span(text)),
    };
    if let Some(v) = lower.strip_prefix(">=") {
        format!("since {}", span(v))
    } else if let Some(v) = lower.strip_prefix("<=") {
        format!("up to {}", span(v))
    } else if let Some(v) = lower.strip_prefix('>') {
        format!("after {}", span(v))
    } else if let Some(v) = lower.strip_prefix('<') {
        format!("before {}", span(v))
    } else if let Some((from, to)) = lower.split_once("..") {
        format!("from {} to {}", span(from), span(to))
    } else {
        during(lower.trim_start_matches('='))
    }
}

/// The text's tokens as typed, split at spaces outside quotes: `content:"two words" ext:rs`
/// is two.
pub fn tokens(text: &str) -> Vec<String> {
    words(text).into_iter().map(|word| word.raw).collect()
}

// ---------- contents ----------

/// Splits what was typed while searching inside files into the text to look for and the
/// filter clauses that choose the files: `TODO ext:rs in:C:\src` looks for "TODO" in Rust
/// files under `C:\src`. A pattern quoted whole loses its quotes.
pub fn split_content(text: &str) -> (String, String) {
    let mut pattern: Vec<String> = Vec::new();
    let mut filters: Vec<String> = Vec::new();
    for word in words(text) {
        if crate::parse::is_filter(&word.raw) {
            filters.push(word.raw);
        } else {
            pattern.push(word.raw);
        }
    }
    let mut joined = pattern.join(" ");
    if joined.len() >= 2 && joined.starts_with('"') && joined.ends_with('"') && joined[1..joined.len() - 1].find('"').is_none() {
        joined = joined[1..joined.len() - 1].to_owned();
    }
    (joined, filters.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuesday 2026-10-13 12:00 UTC, in a UTC-5 zone: as `parse`'s tests.
    const CLOCK: Clock = Clock { now_unix: 20_739 * 86_400 + 12 * 3600, utc_offset_secs: -5 * 3600 };

    struct Fake;

    impl Places for Fake {
        fn known_folder(&self, name: &str) -> Option<String> {
            Some(match name {
                "downloads" => r"C:\Users\me\Downloads",
                "documents" => r"C:\Users\me\OneDrive\Documents",
                "desktop" => r"C:\Users\me\Desktop",
                "pictures" => r"C:\Users\me\Pictures",
                "home" => r"C:\Users\me",
                _ => return None,
            }
            .to_owned())
        }
        fn project(&self, name: &str) -> Option<String> {
            Some(match name {
                "kestrel" => r"C:\Users\me\Kestrel",
                "home-lab" => r"D:\src\home-lab",
                "tax tools" => r"D:\src\Tax Tools",
                _ => return None,
            }
            .to_owned())
        }
    }

    fn query(text: &str) -> String {
        let understood = understand(text, CLOCK, &Fake);
        assert!(understood.natural, "{text:?} was not read as a phrase: {understood:?}");
        // Whatever the phrase becomes must be a query that parses.
        crate::parse(&understood.query, CLOCK).unwrap_or_else(|e| panic!("{text:?} gave {:?}: {e}", understood.query));
        understood.query
    }

    fn labels(text: &str) -> Vec<String> {
        understand(text, CLOCK, &Fake).parts.into_iter().map(|part| part.label).collect()
    }

    fn as_typed(text: &str) {
        let understood = understand(text, CLOCK, &Fake);
        assert!(!understood.natural, "{text:?} was read as a phrase: {:?}", understood.query);
        assert_eq!(understood.query, text.trim());
    }

    #[test]
    fn the_examples() {
        assert_eq!(query("videos from last month over 1 GB"), "type:video dm:lastmonth size:>1gb");
        assert_eq!(query("photos taken in 2023"), "type:image taken:2023");
        assert_eq!(query("pdfs modified this week"), "ext:pdf dm:thisweek");
        assert_eq!(query("large files in downloads"), r#"size:>100mb type:file in:"C:\Users\me\Downloads""#);
        assert_eq!(query("screenshots from yesterday"), "type:image screenshot dm:yesterday");
        assert_eq!(query("code changed today in kestrel"), r#"type:code dm:today in:"C:\Users\me\Kestrel""#);
        assert_eq!(labels("videos from last month over 1 GB"), ["Videos", "Modified last month", "Over 1 GB"]);
        assert_eq!(labels("photos taken in 2023"), ["Pictures", "Taken in 2023"]);
        assert_eq!(labels("large files in downloads"), ["Over 100 MB", "Files", "In Downloads"]);
        assert_eq!(labels("code changed today in kestrel"), ["Code", "Modified today", "In Kestrel"]);
    }

    #[test]
    fn named_and_rolling_dates() {
        assert_eq!(query("files changed yesterday"), "type:file dm:yesterday");
        assert_eq!(query("docs edited this month"), "type:doc dm:thismonth");
        assert_eq!(query("songs added this year"), "type:audio dm:thisyear");
        assert_eq!(query("photos from last year"), "type:image dm:lastyear");
        assert_eq!(query("notes from last week"), "ext:txt;md;log dm:lastweek");
        // "the last week" and "past week" roll; "last week" is the calendar's.
        assert_eq!(query("videos from the last week"), "type:video dm:1w");
        assert_eq!(query("videos from the past week"), "type:video dm:1w");
        assert_eq!(query("pdfs from the past 3 days"), "ext:pdf dm:3d");
        assert_eq!(query("logs changed in the last 24 hours"), "logs dm:24h");
        assert_eq!(query("code modified over the last two weeks"), "type:code dm:2w");
        assert_eq!(query("archives from the past 6 months"), "type:archive dm:6m");
        assert_eq!(query("budget changed recently"), "budget dm:7d");
        assert_eq!(labels("pdfs from the past 3 days"), ["PDFs", "Modified in the last 3 days"]);
    }

    #[test]
    fn calendar_dates() {
        // Today is Tuesday 13 October 2026.
        assert_eq!(query("pictures taken in march"), "type:image taken:2026-03");
        assert_eq!(query("pictures taken in december"), "type:image taken:2025-12");
        assert_eq!(query("pictures taken in october"), "type:image taken:2026-10");
        assert_eq!(query("photos taken in march 2024"), "type:image taken:2024-03");
        assert_eq!(query("photos taken last march"), "type:image taken:2026-03");
        assert_eq!(query("photos taken last october"), "type:image taken:2025-10");
        assert_eq!(query("report modified on 2026-02-14"), "report dm:2026-02-14");
        assert_eq!(query("report from 2025-11"), "report dm:2025-11");
        assert_eq!(query("files changed on monday"), "type:file dm:2026-10-12");
        assert_eq!(query("files changed on tuesday"), "type:file dm:2026-10-13");
        assert_eq!(query("files changed last tuesday"), "type:file dm:2026-10-06");
        assert_eq!(query("files changed on friday"), "type:file dm:2026-10-09");
        assert_eq!(labels("report modified on 2026-02-14"), ["report", "Modified on 14 February 2026"]);
        assert_eq!(labels("photos taken in march 2024"), ["Pictures", "Taken in March 2024"]);
    }

    #[test]
    fn dates_some_time_ago() {
        assert_eq!(query("pdfs from 3 days ago"), "ext:pdf dm:2026-10-10");
        // A week before Tuesday 13 October falls in the week of Monday 5 October.
        assert_eq!(query("screenshots from a week ago"), "type:image screenshot dm:2026-10-05..2026-10-11");
        assert_eq!(labels("screenshots from a week ago")[1], "Modified from 5 October 2026 to 11 October 2026");
        assert_eq!(query("videos from two months ago"), "type:video dm:2026-08");
        assert_eq!(query("photos taken 2 years ago"), "type:image taken:2024");
        assert_eq!(labels("pdfs from 3 days ago"), ["PDFs", "Modified on 10 October 2026"]);
    }

    #[test]
    fn bounded_dates() {
        assert_eq!(query("pdfs since march"), "ext:pdf dm:>=2026-03");
        assert_eq!(query("photos taken before 2020"), "type:image taken:<2020");
        assert_eq!(query("files modified after 2025-06-01"), "type:file dm:>2025-06-01");
        assert_eq!(query("invoices since last month"), "invoices dm:>=2026-09-01");
        assert_eq!(query("invoices since yesterday"), "invoices dm:>=2026-10-12");
        assert_eq!(query("downloads older than 90 days"), "downloads dm:<2026-07-15");
        assert_eq!(query("logs newer than 2 days"), "logs dm:2d");
        assert_eq!(labels("photos taken before 2020"), ["Pictures", "Taken before 2020"]);
        assert_eq!(labels("pdfs since march")[1], "Modified since March 2026");
        assert_eq!(labels("invoices since last month")[1], "Modified since 1 September 2026");
        assert_eq!(labels("files modified after 2025-06-01")[1], "Modified after 1 June 2025");
        // A verb whose date is missing is no name term.
        assert_eq!(query("pdfs modified in downloads"), r#"ext:pdf in:"C:\Users\me\Downloads""#);
        assert_eq!(labels("downloads older than 90 days"), ["downloads", "Not changed in the last 90 days"]);
    }

    #[test]
    fn sizes() {
        assert_eq!(query("videos over 2gb"), "type:video size:>2gb");
        assert_eq!(query("videos larger than 1.5 GB"), "type:video size:>1.5gb");
        assert_eq!(query("photos under 500 kb"), "type:image size:<500kb");
        assert_eq!(query("files smaller than 10 megabytes"), "type:file size:<10mb");
        assert_eq!(query("isos at least 4 gigs"), "isos size:>=4gb");
        assert_eq!(query("backups at most 100mb"), "backups size:<=100mb");
        assert_eq!(query("archives between 1 and 2 gb"), "type:archive size:1gb..2gb");
        assert_eq!(query("archives between 500 mb and 2 gb"), "type:archive size:500mb..2gb");
        assert_eq!(query("huge videos"), "size:>1gb type:video");
        assert_eq!(query("tiny images"), "size:<16kb type:image");
        assert_eq!(query("empty files"), "size:empty type:file");
        assert_eq!(query("small pdfs"), "size:<1mb ext:pdf");
        assert_eq!(labels("archives between 1 and 2 gb"), ["Archives", "1 GB to 2 GB"]);
        assert_eq!(labels("isos at least 4 gigs"), ["isos", "4 GB or more"]);
        assert_eq!(labels("photos under 500 kb"), ["Pictures", "Under 500 KB"]);
    }

    #[test]
    fn kinds_of_two_words_are_phrases_by_themselves() {
        assert_eq!(query("pdf files"), "ext:pdf");
        assert_eq!(query("word documents"), "ext:doc;docx");
        assert_eq!(query("rust files"), "ext:rs");
        assert_eq!(query("mp3 files"), "ext:mp3");
        assert_eq!(query("source code"), "type:code");
        assert_eq!(query("excel files from last week"), "ext:xls;xlsx dm:lastweek");
        assert_eq!(labels("mp3 files"), ["MP3 files"]);
    }

    #[test]
    fn places_and_projects() {
        assert_eq!(query("pdfs in my documents"), r#"ext:pdf in:"C:\Users\me\OneDrive\Documents""#);
        assert_eq!(query("screenshots on the desktop"), r#"type:image screenshot in:"C:\Users\me\Desktop""#);
        assert_eq!(query("installers in the downloads folder"), r#"ext:exe;msi;msix;msixbundle;appx in:"C:\Users\me\Downloads""#);
        assert_eq!(query("readme in home-lab"), r#"readme in:"D:\src\home-lab""#);
        assert_eq!(query("code in home lab"), r#"type:code in:"D:\src\home-lab""#);
        assert_eq!(query("spreadsheets in tax tools"), r#"ext:xls;xlsx;csv;ods in:"D:\src\Tax Tools""#);
        assert_eq!(query("code in the kestrel repo"), r#"type:code in:"C:\Users\me\Kestrel""#);
        assert_eq!(labels("readme in home-lab"), ["readme", "In home-lab"]);
        // An unknown place is left as words, and alone makes no phrase.
        as_typed("notes in nowhere");
    }

    #[test]
    fn contents() {
        assert_eq!(query("notes containing invoice"), "ext:txt;md;log content:invoice");
        assert_eq!(query(r#"files that mention "fn main""#), r#"type:file content:"fn main""#);
        assert_eq!(query("scripts with the text TODO"), "ext:ps1;sh;bat;cmd;py;js content:TODO");
        assert_eq!(labels("notes containing invoice"), ["Text files", "Contains \u{201c}invoice\u{201d}"]);
    }

    #[test]
    fn plain_words_stay_name_terms() {
        assert_eq!(query("invoice pdfs from last year"), "invoice ext:pdf dm:lastyear");
        assert_eq!(query("tax return photos taken in 2023"), "tax return type:image taken:2023");
        // Joining words go; the rest of the name stays as typed.
        assert_eq!(query("show me all the Budget spreadsheets from this month"), "Budget ext:xls;xlsx;csv;ods dm:thismonth");
        assert_eq!(labels("invoice pdfs from last year"), ["invoice", "PDFs", "Modified last year"]);
    }

    #[test]
    fn clauses_typed_in_the_query_language_are_kept() {
        assert_eq!(query("ext:rs changed today"), "ext:rs dm:today");
        assert_eq!(query(r#""annual report" from last year"#), r#""annual report" dm:lastyear"#);
        assert_eq!(query("*.log over 10mb"), "*.log size:>10mb");
        assert_eq!(query(r"!ext:tmp files in downloads"), r#"!ext:tmp type:file in:"C:\Users\me\Downloads""#);
        assert_eq!(labels("ext:rs changed today"), ["RS files", "Modified today"]);
    }

    #[test]
    fn several_kinds_are_any_of_them() {
        let merged = query("videos and photos from last month");
        let (kinds, rest) = merged.split_once(' ').unwrap();
        assert_eq!(rest, "dm:lastmonth");
        assert!(kinds.starts_with("ext:mp4;") && kinds.contains(";jpg;"), "{kinds}");
        assert_eq!(labels("videos and photos from last month"), ["Videos or pictures", "Modified last month"]);
    }

    #[test]
    fn text_that_is_no_phrase_is_searched_as_typed() {
        for text in [
            "report",
            "videos",
            "photos 2023",
            "march madness",
            "report 2024-03",
            "small business plan",
            "big",
            "in",
            "ext:pdf",
            "today.txt",
            r"C:\Users\me notes",
            "may",
            "  quarterly budget  ",
            "",
        ] {
            as_typed(text);
        }
        // Clauses typed as such still get labels, for the chips.
        let typed = understand("budget ext:xlsx dm:thisweek", CLOCK, &Fake);
        assert!(!typed.natural);
        assert_eq!(typed.parts.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["budget", "XLSX files", "Modified this week"]);
        assert_eq!(typed.parts[1].kind, PartKind::Filter);
        assert_eq!(typed.parts[0].kind, PartKind::Name);
    }

    #[test]
    fn without_places_nothing_is_a_place() {
        let understood = understand("pdfs in downloads", CLOCK, &NoPlaces);
        assert!(!understood.natural);
        assert_eq!(understood.query, "pdfs in downloads");
    }

    #[test]
    fn every_part_keeps_the_words_it_came_from() {
        let parts = understand("Videos from LAST month over 1 GB", CLOCK, &Fake).parts;
        let phrases: Vec<&str> = parts.iter().map(|part| part.phrase.as_str()).collect();
        assert_eq!(phrases, ["Videos", "from LAST month", "over 1 GB"]);
        let kinds: Vec<PartKind> = parts.iter().map(|part| part.kind).collect();
        assert_eq!(kinds, [PartKind::Kind, PartKind::Modified, PartKind::Size]);
    }

    #[test]
    fn clauses_are_described() {
        for (clause, label) in [
            ("type:video", "Videos"),
            ("type:folder", "Folders"),
            ("ext:pdf", "PDF files"),
            ("ext:jpg;png", "JPG, PNG files"),
            ("ext:mp4;mkv;mov;avi;wmv", "MP4, MKV and 3 more types"),
            ("dm:today", "Modified today"),
            ("dm:lastmonth", "Modified last month"),
            ("dm:7d", "Modified in the last 7 days"),
            ("dm:1y", "Modified in the last year"),
            ("dm:2023", "Modified in 2023"),
            ("dm:2023-03", "Modified in March 2023"),
            ("dm:2023-03-05", "Modified on 5 March 2023"),
            ("dm:>=2026-03", "Modified since March 2026"),
            ("dm:<2020", "Modified before 2020"),
            ("dm:2025-12-31..2026-01-01", "Modified from 31 December 2025 to 1 January 2026"),
            ("taken:2023", "Taken in 2023"),
            ("size:>1gb", "Over 1 GB"),
            ("size:<=10mb", "10 MB or less"),
            ("size:1gb..2gb", "1 GB to 2 GB"),
            ("size:>1000", "Over 1000 bytes"),
            ("size:empty", "Empty"),
            ("size:10mb", "About 10 MB"),
            (r#"in:"C:\Users\me\Downloads""#, "In Downloads"),
            (r"in:C:\", r"In C:"),
            ("content:TODO", "Contains \u{201c}TODO\u{201d}"),
            ("tag:urgent", "Tagged urgent"),
            ("!ext:tmp", "Not TMP files"),
            ("!type:folder", "Not folders"),
            ("*.rs", "Name like *.rs"),
            ("report", "report"),
        ] {
            assert_eq!(describe(clause), label, "{clause}");
        }
    }

    #[test]
    fn contents_split_from_the_filters_that_choose_files() {
        assert_eq!(split_content("TODO ext:rs"), ("TODO".into(), "ext:rs".into()));
        assert_eq!(split_content(r"fn\s+main ext:rs in:C:\src"), (r"fn\s+main".into(), r"ext:rs in:C:\src".into()));
        assert_eq!(split_content(r#""fn main" !ext:md"#), ("fn main".into(), "!ext:md".into()));
        assert_eq!(split_content("two  words here"), ("two words here".into(), String::new()));
        assert_eq!(split_content("size:>1mb"), (String::new(), "size:>1mb".into()));
        assert_eq!(split_content(r#"say "hi" there"#), (r#"say "hi" there"#.into(), String::new()));
    }
}
