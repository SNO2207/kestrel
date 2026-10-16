//! The Mark of the Web: the `Zone.Identifier` stream Windows attaches to downloaded files.
//!
//! When an archive carries one, Explorer gives every file it extracts the same zone, which is
//! what makes SmartScreen check a downloaded program and Office open a downloaded document in
//! Protected View. Kestrel does the same for everything it writes out of an archive: extracted
//! files, files opened from an archive, and files previewed or copied out of one.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

/// A security zone, as `Zone.Identifier` gives it: 3 is the Internet, 4 untrusted sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone(pub u32);

fn stream_of(path: &Path) -> OsString {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    stream
}

/// The zone of a file's mark, if it has one.
pub fn zone_of(path: &Path) -> Option<Zone> {
    let bytes = fs::read(stream_of(path)).ok()?;
    // The stream is plain text, ANSI or UTF-8, occasionally UTF-16 with a byte-order mark.
    let text = match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(utf16) => {
            String::from_utf16_lossy(&utf16.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect::<Vec<_>>())
        }
        None => String::from_utf8_lossy(&bytes).into_owned(),
    };
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        key.trim().eq_ignore_ascii_case("ZoneId").then(|| value.trim().parse().ok().map(Zone)).flatten()
    })
}

/// Marks `path` as coming from `zone`, by way of the archive at `source`, as Explorer marks
/// what it extracts. The archive's path is noted only when plain ASCII, since the stream is
/// read as ANSI text.
pub fn mark(path: &Path, zone: Zone, source: &Path) -> io::Result<()> {
    let mut text = format!("[ZoneTransfer]\r\nZoneId={}\r\n", zone.0);
    if let Some(source) = source.to_str().filter(|source| source.is_ascii()) {
        text.push_str(&format!("ReferrerUrl={source}\r\n"));
    }
    fs::write(stream_of(path), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_is_read_back_with_its_zone() {
        let dir = std::env::temp_dir().join(format!("kestrel-archive-motw-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.zip");
        fs::write(&file, b"PK").unwrap();
        assert_eq!(zone_of(&file), None);
        fs::write(stream_of(&file), "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/a.zip\r\n").unwrap();
        assert_eq!(zone_of(&file), Some(Zone(3)));

        let out = dir.join("out.txt");
        fs::write(&out, b"x").unwrap();
        mark(&out, Zone(3), &file).unwrap();
        assert_eq!(zone_of(&out), Some(Zone(3)));
        let text = fs::read_to_string(stream_of(&out)).unwrap();
        assert!(text.contains(&format!("ReferrerUrl={}", file.display())), "{text}");
        assert_eq!(fs::read(&out).unwrap(), b"x", "the file's own data is untouched");
        fs::remove_dir_all(&dir).unwrap();
    }
}
