//! Tests of extraction, compression and browsing, with archives the tests build themselves:
//! ordinary ones with the `zip`, `tar` and 7z writers, malicious ones byte by byte. Everything
//! happens in scratch folders under the temp folder.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::address::{Codec, Format};
use super::browse;
use super::compress::{self, Kind as Out, Level};
use super::extract::{self, Conflicts, Extracted};
use super::job::{Failure, Job};
use super::motw::{self, Zone};
use super::reader;
use crate::fileops::tests::QUIET;

// ---------- scratch folders and trees ----------

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kestrel-archive-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every item under `dir`, by path with `/`, with its bytes, or `None` for a folder.
fn tree_of(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for item in fs::read_dir(dir).unwrap() {
            let item = item.unwrap();
            let name = format!("{prefix}{}", item.file_name().to_string_lossy());
            if item.file_type().unwrap().is_dir() {
                out.insert(name.clone(), None);
                walk(&item.path(), &format!("{name}/"), out);
            } else {
                out.insert(name, Some(fs::read(item.path()).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out);
    out
}

/// [`tree_of`] with each file's size and CRC in place of its bytes, for comparing trees.
fn shape_of(dir: &Path) -> BTreeMap<String, Option<(usize, u32)>> {
    tree_of(dir).into_iter().map(|(path, bytes)| (path, bytes.map(|bytes| (bytes.len(), crc32(&bytes))))).collect()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir).unwrap().map(|item| item.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn format_of(path: &Path) -> Format {
    reader::sniff(path, Format::of(&path.file_name().unwrap().to_string_lossy()).unwrap())
}

/// Extracts all of `archive` into `destination`, asking the shell quietly about clashes.
fn extract_with(archive: &Path, destination: &Path, password: Option<&str>, job: &Job) -> Result<Extracted, Failure> {
    let name = archive.file_name().unwrap().to_string_lossy().into_owned();
    let zone = motw::zone_of(archive);
    let request = extract::Request {
        archive,
        format: format_of(archive),
        entries: None,
        name: &name,
        password,
        items: None,
        base: &[],
        destination,
        zone: zone.map(|zone| (zone, archive)),
        conflicts: Conflicts::Ask(QUIET),
    };
    extract::extract(&request, job)
}

fn extract_all(archive: &Path, destination: &Path) -> Result<Extracted, Failure> {
    extract_with(archive, destination, None, &Job::quiet())
}

/// A small source tree with Unicode names, empty folders and a binary file.
fn sample_tree(root: &Path) {
    fs::create_dir_all(root.join("Project/empty")).unwrap();
    fs::create_dir_all(root.join("Project/deep/er/still")).unwrap();
    fs::create_dir_all(root.join("Project/日本語")).unwrap();
    fs::write(root.join("Project/readme.txt"), "hello archive\r\n").unwrap();
    fs::write(root.join("Project/café – naïve.txt"), "unicode ✓").unwrap();
    fs::write(root.join("Project/日本語/ファイル.md"), "# 見出し").unwrap();
    fs::write(root.join("Project/zero.txt"), b"").unwrap();
    let binary: Vec<u8> = (0..300_000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    fs::write(root.join("Project/deep/er/still/data.bin"), binary).unwrap();
    fs::write(root.join("notes.md"), "notes").unwrap();
}

/// The window a test's previews stand for: one of its own, so tests running side by side
/// don't take each other's previews for a window moving on.
fn me() -> String {
    format!("{:?}", std::thread::current().id())
}

/// Removes the cache and opened-files folders this test's thread used.
fn tidy_scratch() {
    for what in ["cache", "opened"] {
        let _ = fs::remove_dir_all(browse::test_scratch(what));
    }
}

/// Nothing an extraction might leave behind is in `dir`: no staging folder, no partial file.
fn assert_nothing_staged(dir: &Path) {
    for name in names_in(dir) {
        assert!(!name.contains("(extracting") && !name.ends_with(".partial"), "{name} was left in {}", dir.display());
    }
}

// ---------- archives built byte by byte ----------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

struct RawEntry<'a> {
    name: &'a [u8],
    data: Vec<u8>,
    /// The uncompressed size to declare.
    size: u32,
    /// 0 stored, 8 deflate.
    method: u16,
    crc: u32,
}

impl<'a> RawEntry<'a> {
    fn stored(name: &'a str, data: &[u8]) -> Self {
        RawEntry { name: name.as_bytes(), data: data.to_vec(), size: data.len() as u32, method: 0, crc: crc32(data) }
    }
}

/// A zip with exactly these names and sizes, which a zip library would refuse to write.
fn raw_zip(entries: &[RawEntry]) -> Vec<u8> {
    let (date, time) = (((2024 - 1980) << 9) | (1 << 5) | 1, 12 << 11);
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let fixed = |sig: u32| {
            let mut header = Vec::new();
            header.extend(sig.to_le_bytes());
            header
        };
        let mut local = fixed(0x0403_4b50);
        local.extend(20u16.to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(entry.method.to_le_bytes());
        local.extend((time as u16).to_le_bytes());
        local.extend((date as u16).to_le_bytes());
        local.extend(entry.crc.to_le_bytes());
        local.extend((entry.data.len() as u32).to_le_bytes());
        local.extend(entry.size.to_le_bytes());
        local.extend((entry.name.len() as u16).to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(entry.name);
        out.extend(local);
        out.extend(&entry.data);

        let mut header = fixed(0x0201_4b50);
        header.extend(20u16.to_le_bytes());
        header.extend(20u16.to_le_bytes());
        header.extend(0u16.to_le_bytes());
        header.extend(entry.method.to_le_bytes());
        header.extend((time as u16).to_le_bytes());
        header.extend((date as u16).to_le_bytes());
        header.extend(entry.crc.to_le_bytes());
        header.extend((entry.data.len() as u32).to_le_bytes());
        header.extend(entry.size.to_le_bytes());
        header.extend((entry.name.len() as u16).to_le_bytes());
        header.extend([0u8; 8]);
        header.extend(0u32.to_le_bytes());
        header.extend(offset.to_le_bytes());
        header.extend(entry.name);
        central.extend(header);
    }
    let start = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// A tar entry with exactly this name, which the tar library would refuse to write.
fn raw_tar_entry(builder: &mut tar::Builder<Vec<u8>>, name: &str, kind: tar::EntryType, link: &str, data: &[u8]) {
    let mut header = tar::Header::new_old();
    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
    header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
    header.set_entry_type(kind);
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(1_700_000_000);
    header.set_cksum();
    builder.append(&header, data).unwrap();
}

/// RAR 5: a variable-length integer.
fn vint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// RAR 5: one header block, its CRC first, then its data.
fn rar_block(fields: &[Vec<u8>], data: &[u8]) -> Vec<u8> {
    let body: Vec<u8> = fields.concat();
    let mut head = vint(body.len() as u64);
    head.extend(body);
    let mut out = crc32(&head).to_le_bytes().to_vec();
    out.extend(head);
    out.extend(data);
    out
}

/// A RAR 5 archive of stored files and folders. `encrypted` gives each file an encryption
/// record, as a password-protected archive has.
fn raw_rar(files: &[(&str, Option<&[u8]>)], encrypted: bool) -> Vec<u8> {
    let mut out = vec![0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00];
    out.extend(rar_block(&[vint(1), vint(0), vint(0)], &[]));
    for (name, data) in files {
        let dir = data.is_none();
        let data = data.unwrap_or_default();
        let extra = if encrypted && !dir {
            let mut record = vint(0x01);
            record.extend(vint(0));
            record.extend(vint(0));
            record.push(15);
            record.extend([7u8; 32]);
            let mut sized = vint(record.len() as u64);
            sized.extend(record);
            sized
        } else {
            Vec::new()
        };
        let mut flags = if dir { 0 } else { 0x2 };
        if !extra.is_empty() {
            flags |= 0x1;
        }
        let mut fields = vec![vint(2), vint(flags)];
        if !extra.is_empty() {
            fields.push(vint(extra.len() as u64));
        }
        if !dir {
            fields.push(vint(data.len() as u64));
        }
        fields.push(vint(if dir { 0x1 | 0x2 } else { 0x2 | 0x4 }));
        fields.push(vint(data.len() as u64));
        fields.push(vint(if dir { 0x10 } else { 0x20 }));
        fields.push(1_700_000_000u32.to_le_bytes().to_vec());
        if !dir {
            fields.push(crc32(data).to_le_bytes().to_vec());
        }
        fields.push(vint(0));
        fields.push(vint(0));
        fields.push(vint(name.len() as u64));
        fields.push(name.as_bytes().to_vec());
        fields.push(extra);
        out.extend(rar_block(&fields, data));
    }
    out.extend(rar_block(&[vint(5), vint(0), vint(0)], &[]));
    out
}

// ---------- round trips ----------

#[test]
fn compressing_then_extracting_gives_the_same_tree() {
    let dir = sandbox("round-trip");
    let src = dir.join("src");
    sample_tree(&src);
    let expected = shape_of(&src);
    let sources = [src.join("Project"), src.join("notes.md")];
    for (kind, level) in [(Out::Zip, Level::Normal), (Out::Zip, Level::Store), (Out::SevenZ, Level::Fast), (Out::TarGz, Level::Maximum)] {
        let name = format!("Project {level:?}{}", kind.extension());
        let request = compress::Request { sources: &sources, folder: &dir, name: &name, kind, level };
        let archive = compress::compress(&request, &Job::quiet()).unwrap();
        assert_eq!(archive, dir.join(&name));
        let out = dir.join(format!("out-{kind:?}-{level:?}"));
        let extracted = extract_all(&archive, &out).unwrap();
        assert!(extracted.destination_created && extracted.completed);
        assert_eq!(extracted.created, std::slice::from_ref(&out));
        assert_eq!(shape_of(&out), expected, "{kind:?} at {level:?}");
        // A second archive of the same name is numbered, never written over.
        let again = compress::compress(&request, &Job::quiet()).unwrap();
        assert_eq!(again.file_name().unwrap().to_string_lossy(), format!("Project {level:?} (2){}", kind.extension()));
        fs::remove_file(again).unwrap();
    }
    assert_nothing_staged(&dir);
    assert_eq!(shape_of(&src), expected, "the sources are untouched");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn zip_names_are_utf8_with_the_flag_set() {
    let dir = sandbox("utf8-flag");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "names.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    // Every central directory header: flags at +8, name length at +28, name at +46.
    let bytes = fs::read(&archive).unwrap();
    let mut seen = Vec::new();
    for at in 0..bytes.len().saturating_sub(46) {
        if bytes[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            continue;
        }
        let flags = u16::from_le_bytes([bytes[at + 8], bytes[at + 9]]);
        let len = u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]) as usize;
        let name = String::from_utf8(bytes[at + 46..at + 46 + len].to_vec()).expect("names are UTF-8");
        if !name.is_ascii() {
            assert_ne!(flags & (1 << 11), 0, "{name} lacks the UTF-8 flag");
        }
        seen.push(name);
    }
    assert!(seen.contains(&"Project/empty/".to_owned()), "empty folders are kept: {seen:?}");
    assert!(seen.contains(&"Project/日本語/ファイル.md".to_owned()), "{seen:?}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn every_format_extracts() {
    let dir = sandbox("formats");
    let src = dir.join("src");
    sample_tree(&src);
    let expected = shape_of(&src);
    let mut tar = tar::Builder::new(Vec::new());
    tar.append_dir_all("Project", src.join("Project")).unwrap();
    tar.append_path_with_name(src.join("notes.md"), "notes.md").unwrap();
    let tar = tar.into_inner().unwrap();

    let write = |name: &str, bytes: &[u8]| {
        fs::write(dir.join(name), bytes).unwrap();
        dir.join(name)
    };
    let gzip = |data: &[u8]| {
        let mut out = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        out.write_all(data).unwrap();
        out.finish().unwrap()
    };
    let bzip = |data: &[u8]| {
        let mut out = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        out.write_all(data).unwrap();
        out.finish().unwrap()
    };
    let xz = |data: &[u8]| {
        let mut out = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        out.write_all(data).unwrap();
        out.finish().unwrap()
    };
    let archives = [
        write("plain.tar", &tar),
        write("gzip.tar.gz", &gzip(&tar)),
        write("gzip.tgz", &gzip(&tar)),
        write("bzip.tar.bz2", &bzip(&tar)),
        write("xz.tar.xz", &xz(&tar)),
    ];
    for archive in &archives {
        let out = dir.join(format!("out-{}", archive.file_name().unwrap().to_string_lossy()));
        extract_all(archive, &out).unwrap_or_else(|failure| panic!("{}: {failure:?}", archive.display()));
        assert_eq!(shape_of(&out), expected, "{}", archive.display());
    }

    // Single compressed files hold one file, named after the archive.
    for (archive, codec) in [
        (write("notes.txt.gz", &gzip(b"single gzip")), Codec::Gzip),
        (write("notes.txt.bz2", &bzip(b"single bzip2")), Codec::Bzip2),
        (write("notes.txt.xz", &xz(b"single xz")), Codec::Xz),
    ] {
        assert_eq!(format_of(&archive), Format::Single(codec));
        let out = dir.join(format!("single-{codec:?}"));
        extract_all(&archive, &out).unwrap();
        assert_eq!(names_in(&out), ["notes.txt"]);
    }
    assert_eq!(fs::read(dir.join("single-Gzip/notes.txt")).unwrap(), b"single gzip");

    // RAR, through Windows' own tar.exe.
    let rar = write(
        "stored.rar",
        &raw_rar(&[("docs", None), ("docs/hello.txt", Some(b"hello from rar")), ("café.txt", Some(b"unicode"))], false),
    );
    assert_eq!(format_of(&rar), Format::Rar);
    let out = dir.join("out-rar");
    extract_all(&rar, &out).unwrap();
    assert_eq!(fs::read(out.join("docs/hello.txt")).unwrap(), b"hello from rar");
    assert_eq!(fs::read(out.join("café.txt")).unwrap(), b"unicode");

    // A 7z named .zip is read as what it is.
    let seven = compress::compress(
        &compress::Request { sources: &[src.join("notes.md")], folder: &dir, name: "really.7z", kind: Out::SevenZ, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    let misnamed = dir.join("misnamed.zip");
    fs::rename(&seven, &misnamed).unwrap();
    extract_all(&misnamed, &dir.join("out-misnamed")).unwrap();
    assert_eq!(fs::read(dir.join("out-misnamed/notes.md")).unwrap(), b"notes");
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- safety ----------

#[test]
fn entries_that_escape_the_destination_are_refused_and_nothing_is_written() {
    let dir = sandbox("zip-slip");
    for (at, name) in [
        "../evil.txt",
        "ok/../../evil.txt",
        r"ok\..\..\evil.txt",
        r"ok/..\..\evil.txt",
        "/evil.txt",
        r"\evil.txt",
        "C:/evil.txt",
        r"C:\evil.txt",
        "c:evil.txt",
        r"\\server\share\evil.txt",
        "//server/share/evil.txt",
        r"\\?\C:\evil.txt",
    ]
    .iter()
    .enumerate()
    {
        let archive = dir.join(format!("slip{at}.zip"));
        fs::write(&archive, raw_zip(&[RawEntry::stored("good.txt", b"good"), RawEntry::stored(name, b"evil")])).unwrap();
        let out = dir.join(format!("out{at}"));
        assert_eq!(extract_all(&archive, &out).unwrap_err(), Failure::Unsafe((*name).to_owned()), "{name}");
        assert!(!out.exists(), "{name}: nothing is extracted, not even the safe item");
    }
    // The same in a tar, which the tar library would never write.
    let mut tar = tar::Builder::new(Vec::new());
    raw_tar_entry(&mut tar, "good.txt", tar::EntryType::Regular, "", b"good");
    raw_tar_entry(&mut tar, "../evil.txt", tar::EntryType::Regular, "", b"evil");
    fs::write(dir.join("slip.tar"), tar.into_inner().unwrap()).unwrap();
    assert_eq!(extract_all(&dir.join("slip.tar"), &dir.join("out-tar")).unwrap_err(), Failure::Unsafe("../evil.txt".into()));
    // And in a RAR.
    fs::write(dir.join("slip.rar"), raw_rar(&[("good.txt", Some(b"good")), ("../evil.txt", Some(b"evil"))], false)).unwrap();
    assert_eq!(extract_all(&dir.join("slip.rar"), &dir.join("out-rar")).unwrap_err(), Failure::Unsafe("../evil.txt".into()));

    assert!(!dir.join("evil.txt").exists() && !std::env::temp_dir().join("evil.txt").exists());
    assert_nothing_staged(&dir);
    let problem = Failure::Unsafe("../evil.txt".into()).problem("slip.zip");
    assert_eq!(problem.message, "slip.zip wasn't extracted: its item \u{201c}../evil.txt\u{201d} would be written outside the destination folder.");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn names_windows_cannot_hold_are_renamed_never_written_as_devices_or_streams() {
    let dir = sandbox("names");
    let archive = dir.join("names.zip");
    let entries = [
        RawEntry::stored("CON", b"con"),
        RawEntry::stored("nul.txt", b"nul"),
        RawEntry::stored("COM1.log", b"com1"),
        RawEntry::stored("docs/LPT1/inside.txt", b"inside"),
        RawEntry::stored("notes.txt:hidden", b"stream on a file"),
        RawEntry::stored("x/a:b.txt", b"stream"),
        RawEntry::stored("x/a_b.txt", b"plain"),
        RawEntry::stored("trailing. ", b"dots"),
        RawEntry::stored("what?<>|.txt", b"chars"),
        RawEntry::stored("Same.txt", b"upper"),
        RawEntry::stored("same.txt", b"lower"),
    ];
    fs::write(&archive, raw_zip(&entries)).unwrap();
    let out = dir.join("out");
    let extracted = extract_all(&archive, &out).unwrap();
    let tree = tree_of(&out);
    let text = |name: &str| tree.get(name).cloned().flatten().map(|bytes| String::from_utf8(bytes).unwrap());
    assert_eq!(text("CON_").as_deref(), Some("con"));
    assert_eq!(text("nul_.txt").as_deref(), Some("nul"));
    assert_eq!(text("COM1_.log").as_deref(), Some("com1"));
    assert_eq!(text("docs/LPT1_/inside.txt").as_deref(), Some("inside"));
    assert_eq!(text("notes.txt_hidden").as_deref(), Some("stream on a file"));
    assert!(!out.join("notes.txt").exists(), "no file notes.txt with a stream `hidden`");
    assert_eq!(text("x/a_b.txt").as_deref(), Some("stream"));
    assert_eq!(text("x/a_b (2).txt").as_deref(), Some("plain"), "a clash made by renaming is numbered");
    assert_eq!(text("trailing").as_deref(), Some("dots"));
    assert_eq!(text("what____.txt").as_deref(), Some("chars"));
    assert_eq!(text("Same.txt").as_deref(), Some("upper"));
    assert_eq!(text("same (2).txt").as_deref(), Some("lower"), "names that differ only in case are both kept");
    assert!(!out.join("x/a").exists(), "no file `a` with a stream `b.txt`");
    assert_eq!(extracted.renamed, 10);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn links_in_tar_and_7z_are_never_created_as_links() {
    let dir = sandbox("links");
    let mut tar = tar::Builder::new(Vec::new());
    raw_tar_entry(&mut tar, "docs/a.txt", tar::EntryType::Regular, "", b"inside");
    raw_tar_entry(&mut tar, "docs/symlink", tar::EntryType::Symlink, "../../outside.txt", b"");
    raw_tar_entry(&mut tar, "docs/abs-symlink", tar::EntryType::Symlink, "C:/Windows/win.ini", b"");
    raw_tar_entry(&mut tar, "docs/hard-inside", tar::EntryType::Link, "docs/a.txt", b"");
    raw_tar_entry(&mut tar, "docs/hard-outside", tar::EntryType::Link, "../outside.txt", b"");
    raw_tar_entry(&mut tar, "docs/hard-missing", tar::EntryType::Link, "docs/none.txt", b"");
    fs::write(dir.join("links.tar"), tar.into_inner().unwrap()).unwrap();
    let out = dir.join("out");
    let extracted = extract_all(&dir.join("links.tar"), &out).unwrap();
    assert_eq!(names_in(&out.join("docs")), ["a.txt", "hard-inside"]);
    assert_eq!(extracted.links_skipped, 4);
    // The hard link inside became a file of its own, not a second name for a.txt.
    assert_eq!(fs::read(out.join("docs/hard-inside")).unwrap(), b"inside");
    fs::write(out.join("docs/a.txt"), b"changed").unwrap();
    assert_eq!(fs::read(out.join("docs/hard-inside")).unwrap(), b"inside");
    let meta = fs::symlink_metadata(out.join("docs/hard-inside")).unwrap();
    assert!(meta.file_type().is_file() && !meta.file_type().is_symlink());

    // A 7z with a Unix symbolic link, and one with a Windows reparse point.
    let seven = dir.join("links.7z");
    {
        let mut writer = sevenz_rust2::ArchiveWriter::create(&seven).unwrap();
        writer.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file("real.txt"), Some(&b"real"[..])).unwrap();
        for (name, attributes) in [("unix-link", 0x8000 | (0o120_777 << 16)), ("reparse-link", 0x400 | 0x20)] {
            let mut entry = sevenz_rust2::ArchiveEntry::new_file(name);
            entry.has_windows_attributes = true;
            entry.windows_attributes = attributes;
            writer.push_archive_entry(entry, Some(&b"../../outside.txt"[..])).unwrap();
        }
        writer.finish().unwrap();
    }
    let out = dir.join("out-7z");
    let extracted = extract_all(&seven, &out).unwrap();
    assert_eq!(names_in(&out), ["real.txt"]);
    assert_eq!(extracted.links_skipped, 2);
    assert!(!dir.join("outside.txt").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_archive_that_declares_more_than_the_drive_holds_is_refused_before_writing() {
    let dir = sandbox("space");
    // A thousand entries that each say they hold nearly 4 GiB, in two bytes of empty deflate.
    let names: Vec<String> = (0..1100).map(|n| format!("big{n}.bin")).collect();
    let entries: Vec<RawEntry> =
        names.iter().map(|name| RawEntry { name: name.as_bytes(), data: vec![0x03, 0x00], size: 0xFFFF_FFF0, method: 8, crc: 0 }).collect();
    fs::write(dir.join("huge.zip"), raw_zip(&entries)).unwrap();
    let failure = extract_all(&dir.join("huge.zip"), &dir.join("out")).unwrap_err();
    assert!(matches!(failure, Failure::Space { needed, .. } if needed > 4_000_000_000_000), "{failure:?}");
    assert!(failure.problem("huge.zip").message.starts_with("There isn't enough free space on"));
    assert!(!dir.join("out").exists());
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn decompression_bombs_stop_and_leave_nothing() {
    let dir = sandbox("bombs");
    // An entry that declares 10 bytes and holds a thousand.
    let data = vec![b'a'; 1000];
    let mut deflated = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    deflated.write_all(&data).unwrap();
    let liar = RawEntry { name: b"liar.txt", data: deflated.finish().unwrap(), size: 10, method: 8, crc: crc32(&data) };
    fs::write(dir.join("liar.zip"), raw_zip(&[RawEntry::stored("first.txt", b"first"), liar])).unwrap();
    let failure = extract_all(&dir.join("liar.zip"), &dir.join("out-liar")).unwrap_err();
    assert!(matches!(failure, Failure::Bomb(_) | Failure::Corrupt(_)), "{failure:?}");
    assert!(!dir.join("out-liar").exists());

    // 200 MB of zeros in a few kilobytes of xz: stopped at the budget (64 MB in tests), in a
    // single file and in a tar, whose listing alone would decompress it all.
    let zeros = vec![0u8; 1 << 20];
    let mut single = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    for _ in 0..200 {
        single.write_all(&zeros).unwrap();
    }
    let single = single.finish().unwrap();
    assert!(single.len() < 100_000, "{} bytes", single.len());
    fs::write(dir.join("zeros.bin.xz"), &single).unwrap();
    let failure = extract_all(&dir.join("zeros.bin.xz"), &dir.join("out-single")).unwrap_err();
    assert!(matches!(&failure, Failure::Bomb(text) if text.contains("expands to more than")), "{failure:?}");
    assert!(!dir.join("out-single").exists());

    let mut tar = tar::Builder::new(lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(200 << 20);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "zeros.bin", std::io::repeat(0).take(200 << 20)).unwrap();
    let tar = tar.into_inner().unwrap().finish().unwrap();
    fs::write(dir.join("zeros.tar.xz"), tar).unwrap();
    let failure = extract_all(&dir.join("zeros.tar.xz"), &dir.join("out-tar")).unwrap_err();
    assert!(matches!(failure, Failure::Bomb(_)), "{failure:?}");
    assert!(!dir.join("out-tar").exists());
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn files_from_a_marked_archive_get_the_same_zone() {
    let dir = sandbox("motw");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "download.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    // Unmarked, nothing is marked.
    extract_all(&archive, &dir.join("plain")).unwrap();
    assert_eq!(motw::zone_of(&dir.join("plain/Project/readme.txt")), None);
    // Marked as downloaded from the Internet, as a browser marks it.
    let mut stream = archive.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/download.zip\r\n").unwrap();
    let out = dir.join("marked");
    extract_all(&archive, &out).unwrap();
    for (path, content) in tree_of(&out) {
        if content.is_some() {
            assert_eq!(motw::zone_of(&out.join(&path)), Some(Zone(3)), "{path}");
        }
    }
    // So do copies taken out for the preview and for opening.
    let entry = format!(r"{}\Project\readme.txt", archive.display());
    let preview = browse::preview_file(&entry, &me()).unwrap().unwrap();
    assert_eq!(motw::zone_of(Path::new(&preview)), Some(Zone(3)));
    let opened = browse::copy_to_open(&entry, None, &Job::quiet()).unwrap().unwrap();
    assert_eq!(fs::read(&opened).unwrap(), b"hello archive\r\n");
    assert_eq!(motw::zone_of(&opened), Some(Zone(3)));
    // Opened again unchanged, the same copy is opened; changed since, a new copy is made.
    assert_eq!(browse::copy_to_open(&entry, None, &Job::quiet()).unwrap().unwrap(), opened);
    fs::write(&opened, b"edited in an app").unwrap();
    let fresh = browse::copy_to_open(&entry, None, &Job::quiet()).unwrap().unwrap();
    assert_ne!(fresh, opened);
    assert_eq!(fs::read(&opened).unwrap(), b"edited in an app", "an edited copy is never written over");
    tidy_scratch();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn encrypted_entries_need_the_password_which_is_never_guessed() {
    let dir = sandbox("encrypted");
    let zip_path = dir.join("secret.zip");
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "correct horse");
        zip.start_file("secret.txt", options).unwrap();
        zip.write_all(b"the secret").unwrap();
        zip.finish().unwrap();
    }
    let seven_path = dir.join("secret.7z");
    {
        let mut writer = sevenz_rust2::ArchiveWriter::create(&seven_path).unwrap();
        writer.set_content_methods(vec![
            sevenz_rust2::encoder_options::AesEncoderOptions::new(sevenz_rust2::Password::from("correct horse")).into(),
            sevenz_rust2::encoder_options::Lzma2Options::from_level(1).into(),
        ]);
        writer.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file("secret.txt"), Some(&b"the secret"[..])).unwrap();
        writer.finish().unwrap();
    }
    // A 7z whose list of contents is encrypted too: nothing can be listed without the password.
    let hidden_path = dir.join("hidden.7z");
    {
        let mut writer = sevenz_rust2::ArchiveWriter::create(&hidden_path).unwrap();
        writer.set_content_methods(vec![
            sevenz_rust2::encoder_options::AesEncoderOptions::new(sevenz_rust2::Password::from("correct horse")).into(),
            sevenz_rust2::encoder_options::Lzma2Options::from_level(1).into(),
        ]);
        writer.set_encrypt_header(true);
        writer.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file("secret.txt"), Some(&b"the secret"[..])).unwrap();
        writer.finish().unwrap();
    }
    let engine = crate::engine::Engine::default();
    let by_name = crate::engine::SortSpec { key: crate::engine::SortField::Name, desc: false };
    let listing = super::list(&engine, &hidden_path.to_string_lossy(), by_name, crate::visibility::Visibility::ALL, None).unwrap();
    assert!(listing.err().unwrap().contains("Extract it to enter the password"));
    for archive in [&zip_path, &seven_path, &hidden_path] {
        let out = dir.join("out");
        let quiet = Job::quiet();
        assert_eq!(extract_with(archive, &out, None, &quiet).unwrap_err(), Failure::Password, "{}", archive.display());
        assert_eq!(extract_with(archive, &out, Some("wrong"), &quiet).unwrap_err(), Failure::WrongPassword, "{}", archive.display());
        assert!(!out.exists());
        extract_with(archive, &out, Some("correct horse"), &quiet).unwrap();
        assert_eq!(fs::read(out.join("secret.txt")).unwrap(), b"the secret");
        fs::remove_dir_all(&out).unwrap();
    }
    // The preview never asks for one.
    let entry = format!(r"{}\secret.txt", zip_path.display());
    assert!(browse::preview_file(&entry, &me()).unwrap().unwrap_err().contains("password"));
    // Kestrel can't read encrypted RAR archives, and says so.
    fs::write(dir.join("secret.rar"), raw_rar(&[("secret.txt", Some(b"the secret"))], true)).unwrap();
    let failure = extract_all(&dir.join("secret.rar"), &dir.join("out-rar")).unwrap_err();
    assert!(matches!(&failure, Failure::Unsupported(what) if what.contains("password-protected")), "{failure:?}");
    assert!(!dir.join("out-rar").exists());
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_damaged_archive_says_so_and_leaves_nothing_half_written() {
    let dir = sandbox("corrupt");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "good.zip", kind: Out::Zip, level: Level::Store },
        &Job::quiet(),
    )
    .unwrap();
    let bytes = fs::read(&archive).unwrap();
    // A byte of the binary file's data flipped: its CRC fails, after other files were written.
    let mut flipped = bytes.clone();
    let at = bytes.windows(8).position(|window| window == &fs::read(src.join("Project/deep/er/still/data.bin")).unwrap()[1000..1008]).unwrap();
    flipped[at] ^= 0xFF;
    fs::write(dir.join("flipped.zip"), flipped).unwrap();
    let failure = extract_all(&dir.join("flipped.zip"), &dir.join("out-flipped")).unwrap_err();
    assert!(matches!(&failure, Failure::Corrupt(_)), "{failure:?}");
    assert!(failure.problem("flipped.zip").message.starts_with("flipped.zip is damaged or incomplete"));
    assert!(!dir.join("out-flipped").exists());

    // Cut short: the list of contents at the end is gone.
    fs::write(dir.join("cut.zip"), &bytes[..bytes.len() / 2]).unwrap();
    assert!(matches!(extract_all(&dir.join("cut.zip"), &dir.join("out-cut")).unwrap_err(), Failure::Corrupt(_)));
    // A tar.gz cut short fails part way through its stream.
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    tar.append_dir_all("Project", src.join("Project")).unwrap();
    let gz = tar.into_inner().unwrap().finish().unwrap();
    fs::write(dir.join("cut.tar.gz"), &gz[..gz.len() * 2 / 3]).unwrap();
    assert!(matches!(extract_all(&dir.join("cut.tar.gz"), &dir.join("out-tgz")).unwrap_err(), Failure::Corrupt(_)));
    // Not an archive at all.
    fs::write(dir.join("text.7z"), b"just some text").unwrap();
    assert!(matches!(extract_all(&dir.join("text.7z"), &dir.join("out-7z")).unwrap_err(), Failure::Corrupt(_)));
    for out in ["out-cut", "out-tgz", "out-7z"] {
        assert!(!dir.join(out).exists(), "{out}");
    }
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cancelled_job_leaves_nothing_behind() {
    let dir = sandbox("cancel");
    let src = dir.join("src");
    sample_tree(&src);
    // Big enough to be reading when the cancel arrives.
    let big: Vec<u8> = (0..8 << 20).map(|n: u32| (n.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
    fs::write(src.join("Project/big.bin"), &big).unwrap();
    let cancelling = || {
        let job = Arc::new(std::sync::OnceLock::<Arc<Job>>::new());
        let handle = job.clone();
        let created = Arc::new(Job::new(1, move |progress| {
            if progress.done > 0
                && let Some(job) = handle.get()
            {
                job.cancel();
            }
        }));
        let _ = job.set(created.clone());
        created
    };
    for kind in [Out::Zip, Out::SevenZ, Out::TarGz] {
        let name = format!("cancelled{}", kind.extension());
        let request = compress::Request { sources: &[src.join("Project")], folder: &dir, name: &name, kind, level: Level::Store };
        assert_eq!(compress::compress(&request, &cancelling()).unwrap_err(), Failure::Cancelled, "{kind:?}");
        assert!(!dir.join(&name).exists());
    }
    assert_eq!(names_in(&dir), ["src"], "no partial archive is left");

    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "whole.zip", kind: Out::Zip, level: Level::Store },
        &Job::quiet(),
    )
    .unwrap();
    // Into a new folder, and into an existing one.
    fs::create_dir_all(dir.join("existing")).unwrap();
    fs::write(dir.join("existing/mine.txt"), b"mine").unwrap();
    for out in [dir.join("new"), dir.join("existing")] {
        assert_eq!(extract_with(&archive, &out, None, &cancelling()).unwrap_err(), Failure::Cancelled);
    }
    assert!(!dir.join("new").exists());
    assert_eq!(names_in(&dir.join("existing")), ["mine.txt"]);
    assert_nothing_staged(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn extracting_into_a_folder_keeps_what_is_there_and_reports_only_what_it_made() {
    let dir = sandbox("merge");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project"), src.join("notes.md")], folder: &dir, name: "a.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    let out = dir.join("out");
    fs::create_dir_all(out.join("Project/deep")).unwrap();
    fs::write(out.join("Project/deep/mine.txt"), b"mine").unwrap();
    fs::write(out.join("notes.md"), b"older notes").unwrap();
    let extracted = extract_all(&archive, &out).unwrap();
    assert!(!extracted.destination_created);
    // Folders that were there are merged into, item by item; the clash went to the shell,
    // which, told not to ask, replaces.
    assert_eq!(fs::read(out.join("Project/deep/mine.txt")).unwrap(), b"mine");
    assert_eq!(fs::read(out.join("notes.md")).unwrap(), b"notes");
    let made: Vec<String> = extracted.created.iter().map(|path| path.strip_prefix(&out).unwrap().to_string_lossy().replace('\\', "/")).collect();
    assert!(made.contains(&"Project/readme.txt".to_owned()) && made.contains(&"Project/deep/er".to_owned()), "{made:?}");
    assert!(!made.contains(&"Project".to_owned()) && !made.contains(&"Project/deep".to_owned()), "folders that were there aren't claimed");
    assert!(made.contains(&"notes.md".to_owned()));
    assert_nothing_staged(&out);
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- browsing ----------

#[test]
fn archives_are_listed_as_folders_with_archives_inside_them_too() {
    use crate::engine::{Engine, SortField, SortSpec};
    use crate::visibility::Visibility;
    let dir = sandbox("browse");
    let src = dir.join("src");
    sample_tree(&src);
    let by_name = SortSpec { key: SortField::Name, desc: false };
    let inner = compress::compress(
        &compress::Request { sources: &[src.join("notes.md")], folder: &src.join("Project"), name: "inner.7z", kind: Out::SevenZ, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "outer.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    fs::remove_file(inner).unwrap();
    let engine = Engine::default();
    let base = archive.to_string_lossy().into_owned();
    let names = |listing: &crate::engine::Listing| -> Vec<(String, bool)> {
        engine.rows(listing.set, 0, 100).unwrap().into_iter().map(|row| (row.name, row.dir)).collect()
    };

    let root = super::list(&engine, &base, by_name, Visibility::ALL, None).unwrap().unwrap();
    assert_eq!(root.archive.as_deref(), Some(base.as_str()));
    assert_eq!(names(&root), [("Project".to_owned(), true)]);
    // Typed in another case, the address is given back as the archive spells it.
    let project = super::list(&engine, &format!(r"{base}\project"), by_name, Visibility::ALL, None).unwrap().unwrap();
    assert_eq!(project.path, format!(r"{base}\Project"));
    let listed = names(&project);
    assert_eq!(listed[..3], [("deep".to_owned(), true), ("empty".to_owned(), true), ("日本語".to_owned(), true)]);
    assert!(listed.contains(&("inner.7z".to_owned(), false)));
    let rows = engine.rows(project.set, 0, 100).unwrap();
    let readme = rows.iter().find(|row| row.name == "readme.txt").unwrap();
    assert_eq!((readme.size, readme.ext.as_str()), (15, "txt"));
    assert_eq!(readme.path, format!(r"{base}\Project\readme.txt"));
    let deep = rows.iter().find(|row| row.name == "deep").unwrap();
    assert_eq!(deep.size, 300_000, "a folder's size is what it holds");

    // An archive inside it opens like a folder too.
    let nested = super::list(&engine, &format!(r"{base}\Project\inner.7z"), by_name, Visibility::ALL, None).unwrap().unwrap();
    assert_eq!(names(&nested), [("notes.md".to_owned(), false)]);
    let preview = browse::preview_file(&format!(r"{base}\Project\inner.7z\notes.md"), &me()).unwrap().unwrap();
    assert_eq!(fs::read(&preview).unwrap(), b"notes");

    // Missing items and files are not folders.
    assert!(super::list(&engine, &format!(r"{base}\Project\missing"), by_name, Visibility::ALL, None).unwrap().err().unwrap().contains("Can't find"));
    assert!(super::list(&engine, &format!(r"{base}\Project\readme.txt"), by_name, Visibility::ALL, None).unwrap().err().unwrap().contains("isn't a folder"));
    // A real folder is not the archive's to list.
    assert!(super::list(&engine, &src.to_string_lossy(), by_name, Visibility::ALL, None).is_none());

    // The preview's details, for an item, a folder, and the archive itself.
    let details = browse::preview_details(&engine, &format!(r"{base}\Project\readme.txt"), &me()).unwrap().unwrap();
    assert_eq!((details.size, details.dir), (Some(15), false));
    let folder = browse::preview_details(&engine, &format!(r"{base}\Project"), &me()).unwrap().unwrap();
    let summary = folder.folder.unwrap();
    assert_eq!((summary.files, summary.folders), (4, 3));
    let itself = browse::preview_details(&engine, &base, &me()).unwrap().unwrap();
    assert!(!itself.dir && itself.folder.is_some_and(|summary| summary.folders == 1));
    tidy_scratch();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn items_are_copied_out_for_the_clipboard_and_drags_with_every_check() {
    let dir = sandbox("materialize");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "a.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    let base = archive.to_string_lossy().into_owned();
    let paths = vec![format!(r"{base}\Project\readme.txt"), format!(r"{base}\Project\deep"), src.join("notes.md").to_string_lossy().into_owned()];
    let files = super::materialize(paths.clone()).unwrap();
    assert_eq!(fs::read(&files[0]).unwrap(), b"hello archive\r\n");
    assert!(Path::new(&files[1]).join("er/still/data.bin").is_file(), "a folder comes out whole");
    assert_eq!(files[2], paths[2], "files on disk pass through");
    assert_eq!(Path::new(&files[0]).parent(), Path::new(&files[1]).parent(), "items of one folder come out together");

    // Changing items inside an archive is refused, with words.
    assert_eq!(super::refuse_entries(&paths[..1]).unwrap_err(), super::READ_ONLY);
    assert!(super::refuse_entries(std::slice::from_ref(&base)).is_ok(), "the archive itself is an ordinary file");
    assert!(super::refuse_location(&base).is_err() && super::refuse_location(&format!(r"{base}\Project")).is_err());
    assert!(super::refuse_location(&dir.to_string_lossy()).is_ok());
    tidy_scratch();
    fs::remove_dir_all(&dir).unwrap();
}

/// Extracting goes into the undo journal as what it made: an undo sends that to the Recycle
/// Bin while it is as extracted, and the redo brings it back, so the bin ends as it began.
#[test]
fn an_extraction_is_undone_through_the_recycle_bin_and_redone() {
    let dir = sandbox("undo");
    let src = dir.join("src");
    sample_tree(&src);
    let archive = compress::compress(
        &compress::Request { sources: &[src.join("Project")], folder: &dir, name: "a.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    let pid = std::process::id();
    let out = dir.join(format!("kestrel-test-{pid}-extracted"));
    let journal = crate::undo::Journal::default();
    let extracted = extract_all(&archive, &out).unwrap();
    super::record(&journal, &extracted.created, extracted.completed, "extraction of a.zip".into());
    assert_eq!(journal.state().undo.as_deref(), Some("extraction of a.zip"));

    // Changed since: left alone.
    fs::write(out.join("Project/added.txt"), b"mine").unwrap();
    let refused = journal.undo(QUIET).unwrap_err();
    assert_eq!(refused, "Couldn't undo the extraction of a.zip: it has changed since.");
    assert!(out.join("Project/added.txt").exists());

    // As extracted: recycled, then restored.
    let extracted = extract_all(&archive, &dir.join(format!("kestrel-test-{pid}-again"))).unwrap();
    super::record(&journal, &extracted.created, extracted.completed, "extraction of a.zip".into());
    assert_eq!(journal.undo(QUIET).unwrap().message, "Undid extraction of a.zip");
    assert!(!dir.join(format!("kestrel-test-{pid}-again")).exists());
    assert_eq!(journal.redo(QUIET).unwrap().message, "Redid extraction of a.zip");
    assert!(dir.join(format!("kestrel-test-{pid}-again/Project/readme.txt")).is_file());
    assert_eq!(crate::recycle::records_mentioning(&format!("kestrel-test-{pid}-again")), Vec::<String>::new());
    fs::remove_dir_all(&dir).unwrap();
}

/// What Kestrel writes, Windows' own reader (libarchive, as Explorer uses) reads back: the
/// folders, empty ones included, are folders, and no 7z folder is marked as a deletion.
#[test]
fn windows_reads_the_archives_kestrel_makes() {
    let dir = sandbox("interop");
    let src = dir.join("src");
    sample_tree(&src);
    // tar.exe prints names in the console's code page, which can't show every name: the
    // Unicode ones are left out here (the round trip covers them).
    fs::remove_file(src.join("Project/café – naïve.txt")).unwrap();
    fs::remove_dir_all(src.join("Project/日本語")).unwrap();
    for kind in [Out::Zip, Out::SevenZ, Out::TarGz] {
        let name = format!("made{}", kind.extension());
        let archive = compress::compress(
            &compress::Request { sources: &[src.join("Project")], folder: &dir, name: &name, kind, level: Level::Fast },
            &Job::quiet(),
        )
        .unwrap();
        if kind == Out::SevenZ {
            let reader = sevenz_rust2::ArchiveReader::open(&archive, sevenz_rust2::Password::empty()).unwrap();
            assert!(reader.archive().files.iter().all(|file| !file.is_anti_item), "no folder is an anti-item");
        }
        let output = std::process::Command::new(std::env::var("SystemRoot").unwrap() + r"\System32\tar.exe")
            .arg("-tf")
            .arg(&archive)
            .output()
            .unwrap();
        assert!(output.status.success(), "{kind:?}: {}", String::from_utf8_lossy(&output.stderr));
        let listed = String::from_utf8_lossy(&output.stdout).replace('\\', "/");
        for expected in ["Project/empty", "Project/deep/er/still/data.bin", "Project/readme.txt", "Project/zero.txt"] {
            assert!(listed.lines().any(|line| line.trim_end_matches('/') == expected), "{kind:?} lacks {expected}:\n{listed}");
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- reading streams, once and only as far as needed ----------

/// A tar.gz of `first.txt`, then `size` bytes of zeros in `big.bin`, then `after.txt`.
fn stream_with_big_middle(path: &Path, size: u64) {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let mut add = |name: &str, len: u64, data: &mut dyn Read| {
        let mut header = tar::Header::new_gnu();
        header.set_size(len);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, data).unwrap();
    };
    add("first.txt", 5, &mut &b"first"[..]);
    add("big.bin", size, &mut std::io::repeat(0).take(size));
    add("after.txt", 5, &mut &b"after"[..]);
    fs::write(path, tar.into_inner().unwrap().finish().unwrap()).unwrap();
}

fn decompressed() -> u64 {
    reader::DECOMPRESSED.with(|counted| counted.get())
}

fn readings_of(path: &Path) -> usize {
    let key = path.to_string_lossy().to_lowercase();
    browse::TREE_READS.lock().iter().filter(|read| **read == key).count()
}

fn by_name() -> crate::engine::SortSpec {
    crate::engine::SortSpec { key: crate::engine::SortField::Name, desc: false }
}

/// Taking one item out of a listed stream reuses the listing and stops reading once it has the
/// item, rather than decompressing the whole archive twice; an item far into it isn't
/// previewed at all.
#[test]
fn an_item_is_taken_out_of_a_stream_without_reading_it_through() {
    let dir = sandbox("stream-item");
    let archive = dir.join("backup.tar.gz");
    stream_with_big_middle(&archive, 3 << 20);
    let engine = crate::engine::Engine::default();
    let base = archive.to_string_lossy().into_owned();
    super::list(&engine, &base, by_name(), crate::visibility::Visibility::ALL, None).unwrap().unwrap();
    assert_eq!(readings_of(&archive), 1);

    let before = decompressed();
    let first = browse::preview_file(&format!(r"{base}\first.txt"), &me()).unwrap().unwrap();
    assert_eq!(fs::read(first).unwrap(), b"first");
    let read = decompressed() - before;
    assert!(read < 64 << 10, "{read} bytes decompressed to take out the first item");
    assert_eq!(readings_of(&archive), 1, "the listing was not read again");

    // The item after 3 MB of data lies too far in to preview (1 MB in tests, 256 MB really).
    let refused = browse::preview_file(&format!(r"{base}\after.txt"), &me()).unwrap().unwrap_err();
    assert!(refused.contains("into a compressed archive"), "{refused}");
    // Opening it still works, and reads as far as it, once.
    let before = decompressed();
    let opened = browse::copy_to_open(&format!(r"{base}\after.txt"), None, &Job::quiet()).unwrap().unwrap();
    assert_eq!(fs::read(opened).unwrap(), b"after");
    assert!(decompressed() - before < (3 << 20) + (64 << 10), "read once, not twice");
    tidy_scratch();
    fs::remove_dir_all(&dir).unwrap();
}

/// Several callers asking for the same stream's listing at once read it once between them.
#[test]
fn a_stream_asked_for_at_once_is_read_once() {
    let dir = sandbox("stream-once");
    let archive = dir.join("shared.tar.gz");
    stream_with_big_middle(&archive, 4 << 20);
    let path = archive.to_string_lossy().into_owned();
    let start = Arc::new(std::sync::Barrier::new(4));
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let (path, start) = (path.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                let located = browse::locate(&path, true, browse::Reading::All(&Job::quiet())).unwrap().unwrap();
                located.tree.nodes.len()
            })
        })
        .collect();
    for caller in callers {
        assert_eq!(caller.join().unwrap(), 4, "the archive and its three items");
    }
    assert_eq!(readings_of(&archive), 1);
    fs::remove_dir_all(&dir).unwrap();
}

/// A listing the window no longer wants stops: while reading, and while waiting for another
/// caller's reading of the same archive.
#[test]
fn a_listing_can_be_cancelled() {
    let dir = sandbox("stream-cancel");
    let archive = dir.join("big.tar.gz");
    stream_with_big_middle(&archive, 2 << 20);
    let path = archive.to_string_lossy().into_owned();
    let given_up = Job::quiet_until(|| true);
    let failure = browse::locate(&path, true, browse::Reading::All(&given_up)).unwrap().err();
    assert_eq!(failure, Some(Failure::Cancelled), "reading stops at the cancel");
    assert_eq!(readings_of(&archive), 1);

    // Another window is reading it; this one gives up waiting when cancelled.
    let held = browse::hold_reading(&archive).unwrap();
    let jobs = Arc::new(super::Jobs::default());
    let listing = {
        let (jobs, path) = (jobs.clone(), path.clone());
        std::thread::spawn(move || {
            let ticket = super::Ticket { jobs: &jobs, window: "main", id: 7 };
            let engine = crate::engine::Engine::default();
            super::list(&engine, &path, by_name(), crate::visibility::Visibility::ALL, Some(ticket)).unwrap().err()
        })
    };
    let started = std::time::Instant::now();
    while !jobs.cancel("main", 7) {
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "the listing never started");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(listing.join().unwrap().as_deref(), Some("Cancelled."));
    assert!(!jobs.cancel("main", 7), "a cancelled listing is forgotten");
    assert_eq!(readings_of(&archive), 1, "the waiting listing read nothing");
    drop(held);
    fs::remove_dir_all(&dir).unwrap();
}

/// The palette asks with every key typed: it looks inside archives already at hand, and never
/// starts reading a stream through.
#[test]
fn the_palette_only_looks_inside_archives_at_hand() {
    let dir = sandbox("stream-palette");
    let archive = dir.join("unread.tar.gz");
    stream_with_big_middle(&archive, 1 << 20);
    let path = archive.to_string_lossy().into_owned();
    assert!(super::entries(&path).is_none());
    assert_eq!(readings_of(&archive), 0, "typing its path read nothing");
    let engine = crate::engine::Engine::default();
    super::list(&engine, &path, by_name(), crate::visibility::Visibility::ALL, None).unwrap().unwrap();
    let names: Vec<String> = super::entries(&path).unwrap().into_iter().map(|item| item.name).collect();
    assert_eq!(names, ["first.txt", "big.bin", "after.txt"], "once opened in a tab, it is at hand");

    // A zip's list of contents is at its end, cheap to read.
    let src = dir.join("src");
    sample_tree(&src);
    let zip = compress::compress(
        &compress::Request { sources: &[src.join("notes.md")], folder: &dir, name: "cheap.zip", kind: Out::Zip, level: Level::Fast },
        &Job::quiet(),
    )
    .unwrap();
    assert_eq!(super::entries(&zip.to_string_lossy()).unwrap().len(), 1);
    fs::remove_dir_all(&dir).unwrap();
}

/// Moving on to another item makes the preview of the last one give way; asking twice for the
/// same item (the pane and its protocol do) doesn't.
#[test]
fn a_preview_gives_way_to_the_next_item() {
    let (window, other) = (me(), format!("{} other", me()));
    let first = browse::preview_job(r"C:\a.tar.gz\one.txt", &window);
    let again = browse::preview_job(r"C:\A.tar.gz\ONE.txt", &window);
    let elsewhere = browse::preview_job(r"C:\a.tar.gz\one.txt", &other);
    assert!(!first.cancelled() && !again.cancelled());
    let next = browse::preview_job(r"C:\a.tar.gz\two.txt", &window);
    assert!(first.cancelled() && again.cancelled());
    assert!(!next.cancelled());
    assert!(!elsewhere.cancelled(), "another window's preview is its own");
}

// ---------- names from other systems ----------

/// A zip made on a Japanese system by Explorer stores names in Shift-JIS without the UTF-8
/// flag. Read in the OEM code page, as Explorer reads it, `表.txt` is one file: as CP437 it was
/// `ò\.txt`, a folder holding `.txt`, since its second byte is a backslash.
#[test]
fn zip_names_without_the_utf8_flag_are_read_in_the_oem_code_page() {
    assert_eq!(reader::decode_name(&[0x95, 0x5C, b'.', b't', b'x', b't'], 932).as_deref(), Some("表.txt"));
    assert_eq!(reader::decode_name(&[0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2], 866).as_deref(), Some("Привет"));
    assert_eq!(reader::decode_name("utf8 ✓".as_bytes(), 932).as_deref(), Some("utf8 ✓"), "UTF-8 is taken as it is");

    let dir = sandbox("oem-names");
    let archive = dir.join("japanese.zip");
    let name = [0x95, 0x5C, b'.', b't', b'x', b't'];
    fs::write(&archive, raw_zip(&[RawEntry { name: &name, data: b"hyo".to_vec(), size: 3, method: 0, crc: crc32(b"hyo") }])).unwrap();
    reader::TEST_CODE_PAGE.with(|page| page.set(Some(932)));
    let engine = crate::engine::Engine::default();
    let listing = super::list(&engine, &archive.to_string_lossy(), by_name(), crate::visibility::Visibility::ALL, None).unwrap().unwrap();
    let rows: Vec<(String, bool)> = engine.rows(listing.set, 0, 10).unwrap().into_iter().map(|row| (row.name, row.dir)).collect();
    let out = dir.join("out");
    let extracted = extract_all(&archive, &out);
    reader::TEST_CODE_PAGE.with(|page| page.set(None));
    assert_eq!(rows, [("表.txt".to_owned(), false)]);
    extracted.unwrap();
    assert_eq!(names_in(&out), ["表.txt"]);
    assert_eq!(fs::read(out.join("表.txt")).unwrap(), b"hyo");
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- merging into what is there ----------

/// A folder the shell merged into, one already in the destination, isn't what the extraction
/// made: recorded as made, an undo would recycle it with the owner's own items in it.
#[test]
fn a_folder_merged_into_is_never_recorded_as_made() {
    let dir = sandbox("merge-link");
    let real = dir.join("real");
    fs::create_dir_all(real.join("Vacation")).unwrap();
    fs::write(real.join("Vacation/precious.jpg"), b"the owner's photo").unwrap();
    let out = dir.join("Pictures");
    fs::create_dir_all(&out).unwrap();
    // A junction in the destination with the name of the archive's folder: a linked folder.
    let shell = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let made = std::process::Command::new(shell)
        .args(["/d", "/c", "mklink", "/J"])
        .arg(out.join("Vacation"))
        .arg(real.join("Vacation"))
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let archive = dir.join("vacation-extra.zip");
    fs::write(&archive, raw_zip(&[RawEntry::stored("Vacation/new1.jpg", b"new"), RawEntry::stored("notes.txt", b"notes")])).unwrap();

    let extracted = extract_all(&archive, &out).unwrap();
    assert!(!extracted.created.contains(&out.join("Vacation")), "the folder merged into: {:?}", extracted.created);
    assert!(extracted.created.contains(&out.join("notes.txt")), "{:?}", extracted.created);
    assert_eq!(fs::read(real.join("Vacation/precious.jpg")).unwrap(), b"the owner's photo");
    assert_nothing_staged(&out);
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- copies opened from archives ----------

/// A password-protected item is decrypted afresh, with its password, each time it is opened,
/// into a folder of its own whose copies go when Kestrel closes; copies nothing has changed
/// for a week are removed at start, and recent ones stay.
#[test]
fn copies_opened_from_archives_dont_outlast_their_use() {
    let dir = sandbox("opened-copies");
    let zip_path = dir.join("secrets.zip");
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "correct horse");
        zip.start_file("tax-2025.pdf", options).unwrap();
        zip.write_all(b"%PDF the secret").unwrap();
        zip.finish().unwrap();
    }
    let entry = format!(r"{}\tax-2025.pdf", zip_path.display());
    let quiet = Job::quiet();
    assert_eq!(browse::copy_to_open(&entry, None, &quiet).unwrap().unwrap_err(), Failure::Password);
    let opened = browse::copy_to_open(&entry, Some("correct horse"), &quiet).unwrap().unwrap();
    assert_eq!(fs::read(&opened).unwrap(), b"%PDF the secret");
    assert!(opened.starts_with(browse::test_scratch("opened").join("protected")), "{}", opened.display());
    let again = browse::copy_to_open(&entry, None, &quiet).unwrap().unwrap_err();
    assert_eq!(again, Failure::Password, "the decrypted copy is never handed out again without the password");
    assert_eq!(browse::copy_to_open(&entry, Some("wrong"), &quiet).unwrap().unwrap_err(), Failure::WrongPassword);
    browse::forget_protected_copies();
    assert!(!opened.exists(), "gone when Kestrel closes");

    // Plain copies: one nothing has touched for eight days goes at start, a fresh one stays.
    let root = browse::test_scratch("opened");
    let (stale, fresh) = (root.join("old a1b2c3d4"), root.join("new e5f6a7b8"));
    for folder in [&stale, &fresh] {
        fs::create_dir_all(folder).unwrap();
        fs::write(folder.join("report.docx"), b"copy").unwrap();
    }
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
    fs::File::options().write(true).open(stale.join("report.docx")).unwrap().set_modified(long_ago).unwrap();
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS, which opening a folder needs.
        let folder = fs::File::options().write(true).custom_flags(0x0200_0000).open(&stale).unwrap();
        folder.set_modified(long_ago).unwrap();
    }
    browse::tidy();
    assert!(!stale.exists() && fresh.join("report.docx").exists());
    tidy_scratch();
    fs::remove_dir_all(&dir).unwrap();
}
