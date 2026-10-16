//! Turning the paths stored in an archive into paths Windows can hold, safely.
//!
//! An archive names its items however its maker liked. Extracted as they stand, some of those
//! names would write outside the destination folder (`../x`, `/etc/x`, `C:\x`, `\\server\x`,
//! `\\?\C:\x`: "zip slip"), and others would not be files at all on Windows: `CON` or `nul.txt`
//! open a device, `a:b` writes the stream `b` of a file `a`, and `name.` or `name ` are other
//! spellings of `name`. [`safe_path`] refuses the first kind outright, and [`safe_name`] renames
//! the second into ordinary file names.

/// The entry's path leads out of the folder it would be extracted into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Escapes;

/// Names Windows keeps for devices, with or without an extension. The superscript digits are
/// device names too.
const DEVICES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
    "COM7", "COM8", "COM9", "COM\u{b9}", "COM\u{b2}", "COM\u{b3}", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
    "LPT6", "LPT7", "LPT8", "LPT9", "LPT\u{b9}", "LPT\u{b2}", "LPT\u{b3}",
];

/// The longest name NTFS holds, in UTF-16 code units.
const MAX_NAME: usize = 255;

/// Whether a path's first part names a drive (`C:`, `C:x`), which would make it absolute or
/// relative to that drive's current folder.
fn has_drive(part: &str) -> bool {
    let bytes = part.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// A part made only of dots and spaces: `.` names the folder it is in, and `..`, or anything
/// Windows reads as it once trailing dots and spaces are dropped (`.. `, `...`), the one above.
fn dots_only(part: &str) -> Option<bool> {
    part.trim_end_matches([' ', '.']).is_empty().then(|| part.matches('.').count() >= 2)
}

/// The path an entry is extracted to, relative to the destination, as safe names. Forward and
/// back slashes both separate parts, as Explorer reads them. An entry that names nothing, such
/// as `./`, gives no parts. Anything that would leave the destination is refused rather than
/// repaired: an absolute path, a drive, a share or device path, or a step up (`..`).
pub fn safe_path(raw: &str) -> Result<Vec<String>, Escapes> {
    if raw.starts_with(['/', '\\']) {
        return Err(Escapes);
    }
    let mut parts = Vec::new();
    for (at, part) in raw.split(['/', '\\']).enumerate() {
        if at == 0 && has_drive(part) {
            return Err(Escapes);
        }
        if part.is_empty() {
            continue;
        }
        match dots_only(part) {
            Some(true) => return Err(Escapes),
            Some(false) => continue,
            None => parts.push(safe_name(part)),
        }
    }
    Ok(parts)
}

/// The parts of an entry's path as a folder view shows them: its own names, with the parts that
/// name no item (empty, `.`, `..`, a drive) left out. Only for browsing; extraction uses
/// [`safe_path`], which refuses what this leaves out.
pub fn shown_path(raw: &str) -> Vec<String> {
    raw.split(['/', '\\'])
        .enumerate()
        .filter(|(at, part)| !(part.is_empty() || dots_only(part).is_some() || (*at == 0 && has_drive(part))))
        .map(|(_, part)| part.to_owned())
        .collect()
}

/// A name Windows stores as an ordinary file or folder of that name:
/// - characters NTFS forbids (`< > : " | ? *`, and control characters) become `_`, so a colon
///   can never name an alternate data stream;
/// - trailing dots and spaces, which Windows drops, are dropped;
/// - a device name (`CON`, `nul.txt`, `COM1.log`) gets `_` after the device part (`CON_`,
///   `nul_.txt`);
/// - a name longer than NTFS allows is shortened, keeping its extension.
pub fn safe_name(name: &str) -> String {
    let mut out: String =
        name.chars().map(|c| if c < ' ' || r#"<>:"|?*/\"#.contains(c) { '_' } else { c }).collect();
    let kept = out.trim_end_matches([' ', '.']).len();
    out.truncate(kept);
    if out.is_empty() {
        out.push('_');
    }
    let stem_end = out.find('.').unwrap_or(out.len());
    let stem = out[..stem_end].trim_end_matches(' ');
    if DEVICES.iter().any(|device| device.eq_ignore_ascii_case(stem)) {
        out.insert(stem.len(), '_');
    }
    shorten(out)
}

/// Cuts a name down to [`MAX_NAME`] UTF-16 units, from the end of its stem.
fn shorten(name: String) -> String {
    if name.encode_utf16().count() <= MAX_NAME {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 16 => name.split_at(dot),
        _ => (name.as_str(), ""),
    };
    let room = MAX_NAME - ext.encode_utf16().count();
    let mut cut = String::new();
    let mut used = 0;
    for c in stem.chars() {
        if used + c.len_utf16() > room {
            break;
        }
        used += c.len_utf16();
        cut.push(c);
    }
    let cut = cut.trim_end_matches([' ', '.']).to_owned();
    format!("{cut}{ext}")
}

/// `name (2)`, `name (3)`, … with the number before the extension, as Windows numbers copies.
pub fn numbered(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// How names are compared: NTFS ignores case.
pub fn fold(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> Result<Vec<String>, Escapes> {
        safe_path(raw)
    }

    fn parts(list: &[&str]) -> Result<Vec<String>, Escapes> {
        Ok(list.iter().map(|part| part.to_string()).collect())
    }

    #[test]
    fn ordinary_paths_pass_through() {
        assert_eq!(path("docs/readme.txt"), parts(&["docs", "readme.txt"]));
        assert_eq!(path(r"docs\sub\a.txt"), parts(&["docs", "sub", "a.txt"]));
        assert_eq!(path("docs/"), parts(&["docs"]));
        assert_eq!(path("./docs//a.txt"), parts(&["docs", "a.txt"]));
        assert_eq!(path("café/naïve – 日本.txt"), parts(&["café", "naïve – 日本.txt"]));
        assert_eq!(path("./"), parts(&[]));
        assert_eq!(path(".hidden"), parts(&[".hidden"]));
    }

    #[test]
    fn paths_that_leave_the_destination_are_refused() {
        for raw in [
            "../evil.txt",
            "docs/../../evil.txt",
            r"docs\..\..\evil.txt",
            r"docs/..\..\evil.txt",
            "a/b/../../../evil.txt",
            "..",
            ".. /evil.txt",
            ".../evil.txt",
            "docs/. ./x",
            "/etc/passwd",
            r"\Windows\evil.dll",
            "C:/Windows/evil.dll",
            r"C:\Windows\evil.dll",
            "c:evil.txt",
            r"\\server\share\evil.txt",
            "//server/share/evil.txt",
            r"\\?\C:\evil.txt",
            r"\\.\PhysicalDrive0",
            r"\??\C:\evil.txt",
        ] {
            assert_eq!(path(raw), Err(Escapes), "{raw}");
        }
        // A step up that stays inside is still refused: no extractor should need one.
        assert_eq!(path("a/../b.txt"), Err(Escapes));
    }

    #[test]
    fn names_windows_cannot_hold_are_renamed() {
        assert_eq!(safe_name("CON"), "CON_");
        assert_eq!(safe_name("con"), "con_");
        assert_eq!(safe_name("nul.txt"), "nul_.txt");
        assert_eq!(safe_name("NUL.tar.gz"), "NUL_.tar.gz");
        assert_eq!(safe_name("COM1"), "COM1_");
        assert_eq!(safe_name("lpt9.log"), "lpt9_.log");
        assert_eq!(safe_name("COM\u{b9}"), "COM\u{b9}_");
        assert_eq!(safe_name("conin$"), "conin$_");
        assert_eq!(safe_name("aux .txt"), "aux_ .txt");
        assert_eq!(safe_name("CON."), "CON_");
        // Near misses are left alone.
        assert_eq!(safe_name("console.txt"), "console.txt");
        assert_eq!(safe_name("COM10"), "COM10");
        assert_eq!(safe_name("nul-note.txt"), "nul-note.txt");

        assert_eq!(safe_name("a:b"), "a_b", "a colon would write an alternate data stream");
        assert_eq!(safe_name("file.txt:Zone.Identifier"), "file.txt_Zone.Identifier");
        assert_eq!(safe_name("x::$DATA"), "x__$DATA");
        assert_eq!(safe_name(r#"what?<>|"*.txt"#), "what______.txt");
        assert_eq!(safe_name("tab\there"), "tab_here");
        assert_eq!(safe_name("trailing. . "), "trailing");
        assert_eq!(safe_name("name."), "name");
        assert_eq!(safe_name("name "), "name");
        assert_eq!(safe_name(" leading"), " leading", "leading spaces are allowed");

        let long = format!("{}.txt", "a".repeat(400));
        let short = safe_name(&long);
        assert_eq!(short.encode_utf16().count(), MAX_NAME);
        assert!(short.ends_with(".txt"));
    }

    #[test]
    fn path_parts_are_made_safe_one_by_one() {
        assert_eq!(path("docs/CON/a:b.txt"), parts(&["docs", "CON_", "a_b.txt"]));
        assert_eq!(path("dir./file "), parts(&["dir", "file"]));
        assert_eq!(path("x/C:/y"), parts(&["x", "C_", "y"]), "a drive part later on is only a name");
    }

    #[test]
    fn browsing_shows_the_names_as_they_are() {
        assert_eq!(shown_path("docs/a:b.txt"), ["docs", "a:b.txt"]);
        assert_eq!(shown_path("../evil.txt"), ["evil.txt"]);
        assert_eq!(shown_path(r"C:\x\y"), ["x", "y"]);
        assert_eq!(shown_path("/abs/z"), ["abs", "z"]);
    }

    #[test]
    fn copies_are_numbered_before_the_extension() {
        assert_eq!(numbered("a.txt", 2), "a (2).txt");
        assert_eq!(numbered("folder", 3), "folder (3)");
        assert_eq!(numbered(".gitignore", 2), ".gitignore (2)");
    }
}
