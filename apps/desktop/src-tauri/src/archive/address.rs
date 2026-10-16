//! Archive formats, and addresses inside archives.
//!
//! An archive is browsed like a folder, at an address made of the archive's own path followed
//! by the path inside it: `C:\Downloads\photos.zip\2024\beach.jpg`. Nothing on disk has that
//! path, which is what tells it apart: [`split`] walks up from the address to the first item
//! that exists and, if that is an archive file, returns it with the rest of the address.

use std::fs;

/// How a tar stream, or a single compressed file, is compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Plain,
    Gzip,
    Bzip2,
    Xz,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar(Codec),
    /// One compressed file, such as `notes.txt.gz`.
    Single(Codec),
}

/// Extensions, longest first, and what they say about a file.
const EXTENSIONS: &[(&str, Format)] = &[
    (".tar.gz", Format::Tar(Codec::Gzip)),
    (".tar.bz2", Format::Tar(Codec::Bzip2)),
    (".tar.xz", Format::Tar(Codec::Xz)),
    (".tgz", Format::Tar(Codec::Gzip)),
    (".tbz2", Format::Tar(Codec::Bzip2)),
    (".tbz", Format::Tar(Codec::Bzip2)),
    (".txz", Format::Tar(Codec::Xz)),
    (".tar", Format::Tar(Codec::Plain)),
    (".zip", Format::Zip),
    (".7z", Format::SevenZ),
    (".rar", Format::Rar),
    (".gz", Format::Single(Codec::Gzip)),
    (".bz2", Format::Single(Codec::Bzip2)),
    (".xz", Format::Single(Codec::Xz)),
];

impl Format {
    /// The format a file's name says it has.
    pub fn of(name: &str) -> Option<Format> {
        let lower = name.to_ascii_lowercase();
        EXTENSIONS
            .iter()
            .find(|(ext, _)| lower.len() > ext.len() && lower.ends_with(ext))
            .map(|&(_, format)| format)
    }

    /// The name without the extension that says what it is: `photos.tar.gz` gives `photos`.
    pub fn stem(name: &str) -> &str {
        let lower = name.to_ascii_lowercase();
        EXTENSIONS
            .iter()
            .find(|(ext, _)| lower.len() > ext.len() && lower.ends_with(ext))
            .map_or(name, |(ext, _)| &name[..name.len() - ext.len()])
    }
}

/// Whether a path is on a drive or a share, rather than a device or object-manager path.
fn is_plain(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    let share = path.starts_with(r"\\") && !path.starts_with(r"\\?\") && !path.starts_with(r"\\.\");
    (drive || share) && !path.contains('\0')
}

/// The folder holding `path`, and the last part of `path`; `None` at a drive or share root.
fn parent_of(path: &str) -> Option<(String, &str)> {
    let (parent, name) = path.rsplit_once('\\')?;
    if name.is_empty() || parent.is_empty() || parent == r"\" {
        return None;
    }
    let parent = if parent.ends_with(':') { format!(r"{parent}\") } else { parent.to_owned() };
    Some((parent, name))
}

/// The archive an address lies in, as its path on disk, and the parts of the address inside it
/// (none for the archive itself). `None` for anything that is not in an archive: a path that
/// exists, or one whose nearest existing ancestor is not an archive file.
pub fn split(path: &str) -> Option<(String, Vec<String>)> {
    let path = crate::engine::normalize(path);
    if !is_plain(&path) {
        return None;
    }
    let mut current = path;
    let mut inner: Vec<String> = Vec::new();
    loop {
        // Asking for an item's metadata never reads its data, so an online-only file is not
        // downloaded by this.
        if let Ok(meta) = fs::metadata(&current) {
            let name = current.rsplit('\\').next().unwrap_or_default();
            if meta.is_file() && Format::of(name).is_some() {
                inner.reverse();
                return Some((current, inner));
            }
            return None;
        }
        let (parent, name) = parent_of(&current)?;
        inner.push(name.to_owned());
        current = parent;
    }
}

/// Whether `path` names something inside an archive (not the archive file itself).
pub fn is_entry(path: &str) -> bool {
    split(path).is_some_and(|(_, inner)| !inner.is_empty())
}

/// Whether `path` is listed as a folder inside an archive: the archive itself, or a folder in it.
pub fn is_location(path: &str) -> bool {
    split(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_known_by_their_extensions() {
        assert_eq!(Format::of("a.zip"), Some(Format::Zip));
        assert_eq!(Format::of("A.ZIP"), Some(Format::Zip));
        assert_eq!(Format::of("a.7z"), Some(Format::SevenZ));
        assert_eq!(Format::of("a.rar"), Some(Format::Rar));
        assert_eq!(Format::of("a.tar"), Some(Format::Tar(Codec::Plain)));
        assert_eq!(Format::of("a.tar.gz"), Some(Format::Tar(Codec::Gzip)));
        assert_eq!(Format::of("a.tgz"), Some(Format::Tar(Codec::Gzip)));
        assert_eq!(Format::of("a.tar.bz2"), Some(Format::Tar(Codec::Bzip2)));
        assert_eq!(Format::of("a.tar.xz"), Some(Format::Tar(Codec::Xz)));
        assert_eq!(Format::of("notes.txt.gz"), Some(Format::Single(Codec::Gzip)));
        assert_eq!(Format::of("a.xz"), Some(Format::Single(Codec::Xz)));
        assert_eq!(Format::of("a.txt"), None);
        assert_eq!(Format::of(".zip"), None, "a name that is only an extension");
        assert_eq!(Format::stem("photos.tar.gz"), "photos");
        assert_eq!(Format::stem("Photos.ZIP"), "Photos");
        assert_eq!(Format::stem("notes.txt.gz"), "notes.txt");
    }

    #[test]
    fn addresses_split_at_the_archive() {
        let dir = std::env::temp_dir().join(format!("kestrel-archive-address-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("real.zip")).unwrap();
        fs::write(dir.join("photos.zip"), b"PK").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        let base = dir.to_str().unwrap();

        let (archive, inner) = split(&format!(r"{base}\photos.zip\2024\beach.jpg")).unwrap();
        assert_eq!(archive, format!(r"{base}\photos.zip"));
        assert_eq!(inner, ["2024", "beach.jpg"]);
        assert_eq!(split(&format!(r"{base}\photos.zip")).unwrap().1, Vec::<String>::new());
        assert_eq!(split(&format!("{base}/photos.zip/a")).unwrap().1, ["a"], "forward slashes too");
        assert!(is_entry(&format!(r"{base}\photos.zip\a")));
        assert!(!is_entry(&format!(r"{base}\photos.zip")));
        assert!(is_location(&format!(r"{base}\photos.zip")));

        // A folder named like an archive is a folder; other files and missing paths are nothing.
        assert_eq!(split(&format!(r"{base}\real.zip\x")), None);
        assert_eq!(split(&format!(r"{base}\notes.txt\x")), None);
        assert_eq!(split(&format!(r"{base}\missing\x")), None);
        assert_eq!(split(base), None);
        assert_eq!(split(r"\\?\C:\x.zip\y"), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
