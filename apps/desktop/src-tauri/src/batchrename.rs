//! Batch rename: new names for many items from one set of rules (find and replace, prefix and
//! suffix, numbering, case), with a preview that flags conflicts and invalid names before
//! anything changes. Applying renames everything as one undoable operation.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tauri::{State, Window};

use crate::fileops::{self, Done, file_name, key, parent, validate_name};
use crate::undo::Journal;

/// The longest name NTFS allows, in UTF-16 units.
const MAX_NAME: usize = 255;
/// Names Windows reserves for devices, with or without an extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
    "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    /// Each Word Capitalised.
    Title,
}

/// Which part of each name the rules change.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    /// The name without its extension.
    #[default]
    Name,
    Extension,
    /// The whole name, extension included.
    Both,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rules {
    pub find: String,
    pub replace: String,
    /// `find` is a regular expression and `replace` may refer to its groups as `$1`.
    pub regex: bool,
    pub match_case: bool,
    pub prefix: String,
    pub suffix: String,
    /// Replaces the name when not empty: each run of `#` becomes the item's number padded to
    /// the run's length, and `*` stands for the name itself. Without a `#`, the number is
    /// added at the end.
    pub numbering: String,
    /// The first item's number.
    pub start: u32,
    pub case: Case,
    pub part: Part,
}

/// One item's name before and after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Preview {
    pub path: String,
    pub old: String,
    pub new: String,
    /// Why the new name can't be used, if it can't.
    pub problem: Option<String>,
}

/// Rules ready to apply.
pub struct Renamer<'a> {
    rules: &'a Rules,
    find: Option<Regex>,
}

impl<'a> Renamer<'a> {
    pub fn new(rules: &'a Rules) -> Result<Self, String> {
        let find = if rules.find.is_empty() {
            None
        } else {
            let pattern = if rules.regex { rules.find.clone() } else { regex::escape(&rules.find) };
            let compiled = RegexBuilder::new(&pattern)
                .case_insensitive(!rules.match_case)
                .build()
                .map_err(|e| format!("The pattern isn't valid: {}", e.to_string().lines().last().unwrap_or_default().trim()))?;
            Some(compiled)
        };
        Ok(Self { rules, find })
    }

    /// The new name for an item, the `index`th of those being renamed.
    pub fn name(&self, old: &str, is_dir: bool, index: usize) -> String {
        let (stem, ext) = split(old, is_dir);
        let join = |stem: &str, ext: &str| if ext.is_empty() { stem.to_owned() } else { format!("{stem}.{ext}") };
        let number = u64::from(self.rules.start) + index as u64;
        match self.rules.part {
            Part::Name => join(&self.apply(stem, number), ext),
            Part::Extension => join(stem, &self.apply(ext, number)),
            Part::Both => self.apply(old, number),
        }
    }

    fn apply(&self, text: &str, number: u64) -> String {
        let rules = self.rules;
        let mut text = match &self.find {
            Some(find) if rules.regex => find.replace_all(text, rules.replace.as_str()).into_owned(),
            Some(find) => find.replace_all(text, NoExpand(&rules.replace)).into_owned(),
            None => text.to_owned(),
        };
        if !rules.numbering.is_empty() {
            text = numbered(&rules.numbering, &text, number);
        }
        let text = format!("{}{text}{}", rules.prefix, rules.suffix);
        match rules.case {
            Case::Keep => text,
            Case::Lower => text.to_lowercase(),
            Case::Upper => text.to_uppercase(),
            Case::Title => title_case(&text),
        }
    }
}

/// Splits a name into the part before its extension and the extension, without the dot.
/// Folders have no extension, and neither does a name that starts with its only dot.
pub fn split(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !is_dir && !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

/// Fills a numbering pattern: runs of `#` take the number, zero-padded to the run's length,
/// and `*` takes `original`.
fn numbered(pattern: &str, original: &str, number: u64) -> String {
    if !pattern.contains('#') {
        return format!("{} {number}", pattern.replace('*', original));
    }
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '#' => {
                let mut width = 1;
                while chars.peek() == Some(&'#') {
                    chars.next();
                    width += 1;
                }
                out.push_str(&format!("{number:0width$}"));
            }
            '*' => out.push_str(original),
            other => out.push(other),
        }
    }
    out
}

fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    for c in text.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = c.is_whitespace() || "-_.([{".contains(c);
    }
    out
}

/// Why Windows would refuse `name`, if it would.
fn invalid(name: &str) -> Option<String> {
    if let Err(problem) = validate_name(name) {
        return Some(problem);
    }
    if name.encode_utf16().count() > MAX_NAME {
        return Some("The name is too long.".into());
    }
    let device = name.split('.').next().unwrap_or_default().trim_end().to_lowercase();
    if RESERVED.contains(&device.as_str()) {
        return Some(format!("Windows reserves the name {}.", device.to_uppercase()));
    }
    None
}

/// The new names `rules` give the items at `paths`, in the order given, each checked for
/// conflicts with the others and with what is already in its folder.
pub fn preview(paths: &[String], rules: &Rules) -> Result<Vec<Preview>, String> {
    let renamer = Renamer::new(rules)?;
    let mut previews: Vec<Preview> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let old = file_name(path).to_owned();
            let new = renamer.name(&old, Path::new(path).is_dir(), index);
            Preview { path: path.clone(), problem: if new == old { None } else { invalid(&new) }, old, new }
        })
        .collect();

    // Each folder's names, less those of the items being renamed, which are freed.
    let renamed: HashSet<String> = paths.iter().map(|path| key(path)).collect();
    let mut taken: HashMap<String, HashSet<String>> = HashMap::new();
    let mut claimed: HashMap<String, usize> = HashMap::new();
    for preview in &previews {
        let folder = key(&parent(&preview.path));
        taken.entry(folder.clone()).or_insert_with(|| {
            std::fs::read_dir(parent(&preview.path))
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                        .map(|name| name.to_lowercase())
                        .filter(|name| !renamed.contains(&format!("{folder}\\{name}")))
                        .collect()
                })
                .unwrap_or_default()
        });
        *claimed.entry(format!("{folder}\\{}", preview.new.to_lowercase())).or_default() += 1;
    }
    for preview in &mut previews {
        if preview.problem.is_some() {
            continue;
        }
        let folder = key(&parent(&preview.path));
        let new = preview.new.to_lowercase();
        if claimed[&format!("{folder}\\{new}")] > 1 {
            preview.problem = Some(format!("Another item would also be named {}.", preview.new));
        } else if taken[&folder].contains(&new) {
            preview.problem = Some(format!("{} already exists.", preview.new));
        }
    }
    Ok(previews)
}

#[tauri::command(async)]
pub fn preview_renames(paths: Vec<String>, rules: Rules) -> Result<Vec<Preview>, String> {
    crate::archive::refuse_entries(&paths)?;
    preview(&paths, &rules)
}

/// Renames every item at `paths` by `rules`, as one undoable operation, and returns the new
/// names. Nothing is renamed if any new name has a problem.
#[tauri::command(async)]
pub fn batch_rename(window: Window, journal: State<Journal>, paths: Vec<String>, rules: Rules) -> Result<Vec<String>, String> {
    crate::archive::refuse_entries(&paths)?;
    let previews = preview(&paths, &rules)?;
    if let Some(bad) = previews.iter().find(|preview| preview.problem.is_some()) {
        return Err(format!("{}: {}", bad.old, bad.problem.as_deref().unwrap_or_default()));
    }
    let renames: Vec<(String, String)> =
        previews.into_iter().filter(|preview| preview.new != preview.old).map(|preview| (preview.path, preview.new)).collect();
    if renames.is_empty() {
        return Ok(Vec::new());
    }
    let outcome = fileops::rename_all(renames, crate::interactive(&window))?;
    journal.record(&outcome);
    Ok(outcome
        .results()
        .filter_map(|done| match done {
            Done::Renamed { to, .. } => Some(file_name(to).to_owned()),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{sandbox, text};
    use std::fs;

    fn rules(change: impl FnOnce(&mut Rules)) -> Rules {
        let mut rules = Rules { start: 1, ..Rules::default() };
        change(&mut rules);
        rules
    }

    fn rename(rules: &Rules, names: &[&str]) -> Vec<String> {
        let renamer = Renamer::new(rules).unwrap();
        names.iter().enumerate().map(|(index, name)| renamer.name(name, false, index)).collect()
    }

    #[test]
    fn finds_and_replaces_plain_text() {
        let plain = rules(|r| {
            r.find = "IMG".into();
            r.replace = "Photo$1".into();
        });
        // Case-insensitive by default, and `$1` is literal outside regex mode.
        assert_eq!(rename(&plain, &["img_01.JPG", "IMG_02.jpg", "notes.txt"]), ["Photo$1_01.JPG", "Photo$1_02.jpg", "notes.txt"]);
        let exact = rules(|r| {
            r.find = "IMG".into();
            r.replace = "x".into();
            r.match_case = true;
        });
        assert_eq!(rename(&exact, &["img_IMG.png"]), ["img_x.png"]);
        // The extension is left alone unless asked for.
        let dots = rules(|r| {
            r.find = ".".into();
            r.replace = " ".into();
        });
        assert_eq!(rename(&dots, &["a.b.c.txt"]), ["a b c.txt"]);
    }

    #[test]
    fn replaces_with_regular_expressions() {
        let swap = rules(|r| {
            r.find = r"^(\w+)-(\d+)$".into();
            r.replace = "$2-$1".into();
            r.regex = true;
        });
        assert_eq!(rename(&swap, &["draft-7.md", "final.md"]), ["7-draft.md", "final.md"]);
        let bad = rules(|r| {
            r.find = "(".into();
            r.regex = true;
        });
        let Err(problem) = Renamer::new(&bad) else { panic!("an unclosed group is refused") };
        assert!(problem.starts_with("The pattern isn't valid"), "{problem}");
    }

    #[test]
    fn numbers_items_from_a_start_value() {
        let photos = rules(|r| {
            r.numbering = "Photo ###".into();
            r.start = 9;
        });
        assert_eq!(rename(&photos, &["a.jpg", "b.jpg", "c.png"]), ["Photo 009.jpg", "Photo 010.jpg", "Photo 011.png"]);
        let keep = rules(|r| r.numbering = "* (#)".into());
        assert_eq!(rename(&keep, &["a.jpg", "b.jpg"]), ["a (1).jpg", "b (2).jpg"]);
        let plain = rules(|r| r.numbering = "Scan".into());
        assert_eq!(rename(&plain, &["x.pdf"]), ["Scan 1.pdf"]);
        // Numbers outgrow their padding rather than being cut.
        let narrow = rules(|r| {
            r.numbering = "#".into();
            r.start = 10;
        });
        assert_eq!(rename(&narrow, &["x.txt"]), ["10.txt"]);
    }

    #[test]
    fn adds_prefixes_suffixes_and_changes_case() {
        let both = rules(|r| {
            r.prefix = "2026 ".into();
            r.suffix = " final".into();
            r.case = Case::Title;
        });
        assert_eq!(rename(&both, &["quarterly REPORT.docx"]), ["2026 Quarterly Report Final.docx"]);
        let upper = rules(|r| r.case = Case::Upper);
        assert_eq!(rename(&upper, &["readme.md"]), ["README.md"]);
    }

    #[test]
    fn applies_to_the_extension_or_the_whole_name() {
        let ext = rules(|r| {
            r.case = Case::Lower;
            r.part = Part::Extension;
        });
        assert_eq!(rename(&ext, &["Photo.JPG", "Makefile"]), ["Photo.jpg", "Makefile"]);
        let whole = rules(|r| {
            r.case = Case::Upper;
            r.part = Part::Both;
        });
        assert_eq!(rename(&whole, &["a.txt"]), ["A.TXT"]);
        // Folders have no extension; a leading dot is not one either.
        let renamer = Renamer::new(&ext).unwrap();
        assert_eq!(renamer.name("v1.2", true, 0), "v1.2");
        assert_eq!(split(".gitignore", false), (".gitignore", ""));
        assert_eq!(split("archive.tar.gz", false), ("archive.tar", "gz"));
    }

    #[test]
    fn preview_flags_conflicts_and_invalid_names() {
        let dir = sandbox("batch-preview");
        for name in ["a.txt", "b.txt", "c.txt", "taken.txt", "1.txt", "2.txt", "3.txt"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        let paths: Vec<String> = ["a.txt", "b.txt", "c.txt"].iter().map(|name| text(&dir.join(name))).collect();
        let problems = |rules: &Rules| -> Vec<Option<String>> {
            preview(&paths, rules).unwrap().into_iter().map(|preview| preview.problem).collect()
        };

        // Every item to the same name: each is flagged.
        let collide = rules(|r| {
            r.find = r"^[abc]$".into();
            r.replace = "x".into();
            r.regex = true;
        });
        assert_eq!(problems(&collide), vec![Some("Another item would also be named x.txt.".to_owned()); 3]);

        // A name already in the folder, from an item that isn't being renamed.
        let onto = rules(|r| {
            r.find = "c".into();
            r.replace = "taken".into();
        });
        assert_eq!(problems(&onto), [None, None, Some("taken.txt already exists.".to_owned())]);

        // Renaming onto a name another selected item gives up is fine (a chain).
        let shift = rules(|r| {
            r.find = "a".into();
            r.replace = "b".into();
        });
        let shifted = preview(&paths[..1], &shift).unwrap();
        assert_eq!(shifted[0].problem.as_deref(), Some("b.txt already exists."), "b isn't selected here");
        let numbered: Vec<String> = ["1.txt", "2.txt", "3.txt"].iter().map(|name| text(&dir.join(name))).collect();
        let chain = rules(|r| {
            r.numbering = "#".into();
            r.start = 2;
        });
        let chained = preview(&numbered, &chain).unwrap();
        assert_eq!(chained.iter().map(|p| p.new.as_str()).collect::<Vec<_>>(), ["2.txt", "3.txt", "4.txt"]);
        assert!(chained.iter().all(|p| p.problem.is_none()), "{chained:?}");

        let bad = rules(|r| r.suffix = "?".into());
        assert_eq!(problems(&bad)[0].as_deref(), Some("A name can't contain ?"));
        let device = rules(|r| r.numbering = "con".into());
        let renamer = Renamer::new(&device).unwrap();
        assert_eq!(renamer.name("a.txt", false, 0), "con 1.txt");
        assert_eq!(invalid("CON.txt").as_deref(), Some("Windows reserves the name CON."));
        assert_eq!(invalid(&"x".repeat(256)).as_deref(), Some("The name is too long."));
        // An unchanged name is no problem.
        assert_eq!(problems(&Rules::default()), [None, None, None]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
