//! Building an index straight from a volume's master file table, which takes seconds where a
//! folder-by-folder crawl takes tens of seconds. Reading a live volume needs administrator
//! rights; a dump of the MFT can be read by anyone.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use kestrel_ntfs::mft::{FIRST_USER_RECORD, Name, ROOT_RECORD, Record, record_number, sequence_number};
use kestrel_ntfs::volume::{self, MftLayout, ScanStats, Source};

use crate::builder::IndexBuilder;
use crate::index::{Meta, NO_ENTRY, attr};

#[derive(Clone, Copy, Debug, Default)]
pub struct MftScan {
    pub scan: ScanStats,
    pub files: u64,
    pub dirs: u64,
}

/// Adds the volume at `root` (such as `C:\`) to `builder` by reading its MFT directly.
/// Requires administrator rights.
#[cfg(windows)]
pub fn scan_volume(root: &str, builder: &mut IndexBuilder) -> io::Result<MftScan> {
    let volume = volume::Volume::open(root)?;
    let layout = volume.layout().clone();
    build(&volume, &layout, root, builder)
}

/// Copies the MFT of the volume at `root` to a file. Requires administrator rights.
#[cfg(windows)]
pub fn dump_volume(root: &str, path: &Path) -> io::Result<()> {
    let volume = volume::Volume::open(root)?;
    volume::write_dump(&volume, volume.layout(), path)
}

/// Adds a volume to `builder` from an MFT dump, as though `root` were its root.
pub fn scan_dump(path: &Path, root: &str, builder: &mut IndexBuilder) -> io::Result<MftScan> {
    let (file, layout) = volume::open_dump(path)?;
    build(&file, &layout, root, builder)
}

/// A file reference: record number in the low 48 bits, sequence number in the top 16.
fn reference(record: &Record) -> u64 {
    (record.sequence as u64) << 48 | record.number
}

fn meta_of(record: &Record) -> Meta {
    let data = record.data.unwrap_or_default();
    Meta {
        size: data.size,
        alloc: data.alloc,
        mtime: record.modified.unwrap_or(0),
        attrs: record.attrs.unwrap_or(if record.is_dir { attr::DIRECTORY } else { 0 }),
        file_id: reference(record),
    }
}

/// What the merge needs to know about a record, indexed by record number.
struct Records {
    /// Builder entry for the record's first name, or `NO_ENTRY`.
    first_entry: Vec<u32>,
    sequence: Vec<u16>,
    /// Every entry and the folder reference it names as its parent, resolved last.
    links: Vec<(u32, u64)>,
}

impl Records {
    fn add(&mut self, builder: &mut IndexBuilder, number: u64, names: &[Name], meta: Meta, count_alloc: bool) -> Vec<u32> {
        let mut ids = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            // Hard links share one allocation, so only the first name is charged for it.
            let alloc = if i == 0 && count_alloc { meta.alloc } else { 0 };
            let id = builder.push(NO_ENTRY, &name.name, Meta { alloc, ..meta });
            self.links.push((id, name.parent));
            ids.push(id);
        }
        if let (Some(&first), Some(slot)) = (ids.first(), self.first_entry.get_mut(number as usize))
            && *slot == NO_ENTRY
        {
            *slot = first;
        }
        ids
    }
}

fn build(source: &impl Source, layout: &MftLayout, root: &str, builder: &mut IndexBuilder) -> io::Result<MftScan> {
    let capacity = (layout.valid_bytes / layout.record_size as u64) as usize;
    let mut records = Records { first_entry: vec![NO_ENTRY; capacity], sequence: vec![0; capacity], links: Vec::with_capacity(capacity) };
    let root_id = builder.add_root(root.trim_end_matches(['\\', '/']));
    let mut stats = MftScan::default();
    // Files whose size lives in an extension record, and records whose names all do.
    let mut sizeless: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut nameless: HashMap<u64, Record> = HashMap::new();
    let mut extensions: Vec<Record> = Vec::new();

    stats.scan = volume::scan(source, layout, |batch| {
        for record in batch {
            let number = record.number;
            if let Some(slot) = records.sequence.get_mut(number as usize) {
                *slot = record.sequence;
            }
            if record.base.is_some() {
                if !record.names.is_empty() || record.data.is_some() {
                    extensions.push(record);
                }
                continue;
            }
            if number == ROOT_RECORD {
                builder.set_file_id(root_id, reference(&record));
                records.first_entry[number as usize] = root_id;
                continue;
            }
            // The file system's own metadata files, and the $Extend folder's contents through
            // it, are not shown.
            if number < FIRST_USER_RECORD {
                continue;
            }
            if record.is_dir {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            if record.names.is_empty() {
                nameless.insert(number, record);
                continue;
            }
            let ids = records.add(builder, number, &record.names, meta_of(&record), true);
            if record.data.is_none() && !record.is_dir {
                sizeless.insert(number, ids);
            }
        }
    })?;

    // Extension records hold what did not fit in their base record: more names, or the start
    // of a fragmented file's data stream. A file with many hard links can have its names in
    // one extension and its size in another, in either order, so every size is applied before
    // any extra name is added; names added later then copy the right size.
    let mut extra_names: Vec<(u64, &[Name])> = Vec::new();
    for extension in &extensions {
        let base = extension.base.expect("only extension records are kept");
        if let Some(data) = extension.data {
            if let Some(record) = nameless.get_mut(&base) {
                record.data = Some(data);
            }
            for (i, &id) in sizeless.get(&base).into_iter().flatten().enumerate() {
                let meta = builder.meta_mut(id);
                meta.size = data.size;
                meta.alloc = if i == 0 { data.alloc } else { 0 };
            }
        }
        if !extension.names.is_empty() {
            extra_names.push((base, &extension.names));
        }
    }
    for (base, names) in extra_names {
        let first = records.first_entry.get(base as usize).copied().unwrap_or(NO_ENTRY);
        let (meta, charged) = match nameless.get(&base) {
            Some(record) => (meta_of(record), first != NO_ENTRY),
            None if first != NO_ENTRY => (builder.meta(first), true),
            None => continue,
        };
        records.add(builder, base, names, meta, !charged);
    }

    // Parents are resolved last, once every folder has an entry. A reference whose sequence
    // number no longer matches points at a folder that was deleted and its record reused, so
    // the entry is left unattached and dropped.
    for &(id, parent) in &records.links {
        let number = record_number(parent) as usize;
        let Some(&folder) = records.first_entry.get(number) else { continue };
        if folder != NO_ENTRY && builder.meta(folder).attrs & attr::DIRECTORY != 0 && records.sequence[number] == sequence_number(parent) {
            builder.set_parent(id, folder);
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Writes a dump containing the given records at their record numbers.
    fn dump(records: &[(u64, Vec<u8>)]) -> std::path::PathBuf {
        let count = records.iter().map(|(n, _)| n + 1).max().unwrap();
        let path = std::env::temp_dir().join(format!("kestrel-mft-{}-{count}.dump", std::process::id()));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"KSTRLMFT").unwrap();
        for value in [1024u64, 4096, count * 1024] {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        let mut body = vec![0u8; count as usize * 1024];
        for (number, bytes) in records {
            body[*number as usize * 1024..(*number as usize + 1) * 1024].copy_from_slice(bytes);
        }
        file.write_all(&body).unwrap();
        path
    }

    #[test]
    fn builds_a_tree_from_records() {
        use kestrel_ntfs::mft::testing::RecordBuilder;
        const ROOT: u64 = 0x0005_0000_0000_0005;
        const PROJECTS: u64 = 0x0001_0000_0000_0020;
        let folder = |name: &str, parent: u64, sequence: u16| {
            RecordBuilder::new(1024, 0x3, sequence, 0).standard_info(10, 0).file_name(parent, name, 3).finish()
        };
        let file = |names: &[(&str, u64)], size: u64| {
            let mut builder = RecordBuilder::new(1024, 0x1, 1, 0);
            builder.standard_info(20, 0x20);
            for (name, parent) in names {
                builder.file_name(*parent, name, 1);
            }
            builder.data_runs(0, size, size.next_multiple_of(4096), 0, None, &[0x11, 0x01, 0x05, 0x00]).finish()
        };
        let path = dump(&[
            (5, RecordBuilder::new(1024, 0x3, 5, 0).standard_info(1, 0).file_name(ROOT, ".", 3).finish()),
            // The $Extend folder and something inside it: metadata, never shown.
            (11, folder("$Extend", ROOT, 11)),
            (16, RecordBuilder::new(1024, 0x1, 1, 0).file_name(0x000B_0000_0000_000B, "$UsnJrnl", 1).finish()),
            (32, folder("Projects", ROOT, 1)),
            // Two hard links to one file: both are listed, the allocation is charged once.
            (33, file(&[("a.txt", PROJECTS), ("link-to-a.txt", ROOT)], 5000)),
            // A file in a folder whose record has since been reused (sequence 7, not 1).
            (34, file(&[("orphan.txt", 0x0007_0000_0000_0020)], 10)),
        ]);
        let mut builder = IndexBuilder::new();
        let stats = scan_dump(&path, r"C:\", &mut builder).unwrap();
        fs::remove_file(&path).unwrap();
        let index = builder.finish();

        assert_eq!((stats.files, stats.dirs, stats.scan.parsed), (3, 1, 6));
        let a = index.lookup(r"C:\Projects\a.txt").unwrap();
        let link = index.lookup(r"C:\link-to-a.txt").unwrap();
        assert_eq!((index.size(a), index.size(link)), (5000, 5000));
        assert_eq!(index.alloc(a) + index.alloc(link), 8192);
        assert_eq!(index.file_id(a), 0x0001_0000_0000_0021);
        assert_eq!(index.file_id(index.roots()[0]), ROOT);
        assert!(index.lookup(r"C:\$Extend").is_none());
        assert_eq!(index.live_count(), 4, "root, Projects and the two links; the orphan is dropped");
    }

    #[test]
    fn sizes_and_names_spread_over_extension_records_are_merged_in_any_order() {
        use kestrel_ntfs::mft::testing::RecordBuilder;
        const ROOT: u64 = 0x0005_0000_0000_0005;
        let runs = [0x11, 0x01, 0x05, 0x00];
        let base = |names: &[&str]| {
            let mut record = RecordBuilder::new(1024, 0x1, 1, 0);
            record.standard_info(30, 0x20);
            for name in names {
                record.file_name(ROOT, name, 1);
            }
            record.finish()
        };
        let extension = |base: u64| RecordBuilder::new(1024, 0x1, 1, base);
        let path = dump(&[
            (5, RecordBuilder::new(1024, 0x3, 5, 0).standard_info(1, 0).file_name(ROOT, ".", 3).finish()),
            // Every name and the size live in extensions; the size comes first.
            (40, base(&[])),
            (41, extension(40).data_runs(0, 5000, 8192, 0, None, &runs).finish()),
            (42, extension(40).file_name(ROOT, "a.dll", 1).file_name(ROOT, "b.dll", 1).finish()),
            // One name in the base, another in an extension, and the size last of all.
            (50, base(&["c.dll"])),
            (51, extension(50).file_name(ROOT, "d.dll", 1).finish()),
            (52, extension(50).data_runs(0, 7000, 8192, 0, None, &runs).finish()),
        ]);
        let mut builder = IndexBuilder::new();
        scan_dump(&path, r"C:\", &mut builder).unwrap();
        fs::remove_file(&path).unwrap();
        let index = builder.finish();

        let entry = |name: &str| index.lookup(&format!(r"C:\{name}")).unwrap_or_else(|| panic!("{name} missing"));
        for (name, size) in [("a.dll", 5000), ("b.dll", 5000), ("c.dll", 7000), ("d.dll", 7000)] {
            assert_eq!(index.size(entry(name)), size, "{name}");
        }
        // Each file's allocation is charged to exactly one of its names.
        assert_eq!(index.alloc(entry("a.dll")) + index.alloc(entry("b.dll")), 8192);
        assert_eq!(index.alloc(entry("c.dll")) + index.alloc(entry("d.dll")), 8192);
    }
}
