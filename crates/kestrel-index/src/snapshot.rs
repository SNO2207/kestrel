//! On-disk snapshot of an [`Index`], so a restart does not have to rescan the volume.
//!
//! The format is the columns written back to back, each prefixed with its element count. It is
//! native-endian and versioned; a snapshot from another version is rejected and the caller
//! rebuilds. The same format carries an index from the elevated broker to the app.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use bytemuck::Pod;

use crate::index::{ExtTable, Index, JournalCursor};

const MAGIC: &[u8; 8] = b"KSTRLIDX";
const VERSION: u32 = 2;

/// Largest stream a snapshot read from something other than a file may claim to hold, so a
/// corrupt length cannot turn into an enormous allocation.
const STREAM_LIMIT: u64 = 64 << 30;

pub fn save(index: &Index, path: &Path) -> io::Result<()> {
    // Written to a sibling file and renamed, so a crash never leaves a truncated snapshot.
    let tmp = path.with_extension("tmp");
    let mut out = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
    write_to(index, &mut out)?;
    out.into_inner()?.sync_all()?;
    fs::rename(&tmp, path)
}

/// Writes a snapshot of `index` to any writer.
pub fn write_to(index: &Index, mut out: &mut impl Write) -> io::Result<()> {
    out.write_all(MAGIC)?;
    out.write_all(&VERSION.to_ne_bytes())?;
    out.write_all(&index.base_len.to_ne_bytes())?;
    out.write_all(&index.live.to_ne_bytes())?;
    out.write_all(&[index.moved as u8])?;

    write_column(&mut out, &index.names)?;
    write_column(&mut out, &index.lower)?;
    write_column(&mut out, &index.name_off)?;
    write_column(&mut out, &index.parent)?;
    write_column(&mut out, &index.first_child)?;
    write_column(&mut out, &index.next_sibling)?;
    write_column(&mut out, &index.subtree_end)?;
    write_column(&mut out, &index.size)?;
    write_column(&mut out, &index.alloc)?;
    write_column(&mut out, &index.mtime)?;
    write_column(&mut out, &index.attrs)?;
    write_column(&mut out, &index.ext)?;
    write_column(&mut out, &index.file_id)?;
    write_column(&mut out, &index.name_rank)?;
    write_column(&mut out, &index.roots)?;

    let ext_names = index.exts.names.join("\0");
    write_column(&mut out, ext_names.as_bytes())?;
    // Entries added since the lookup was sorted live in a side table; fold them in so the
    // loaded index needs no rebuild.
    if index.file_ids_added.is_empty() && index.file_ids_covered == index.len() {
        write_column(&mut out, &index.file_ids)?;
    } else {
        write_column(&mut out, &index.sorted_file_ids())?;
    }
    let journal: String = index
        .journal
        .iter()
        .map(|c| format!("{}\t{}\t{}\n", c.root, c.journal_id, c.next_usn))
        .collect();
    write_column(&mut out, journal.as_bytes())?;
    out.flush()
}

pub fn load(path: &Path) -> io::Result<Index> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    read_from(&mut BufReader::with_capacity(1 << 20, file), length)
}

/// Reads a snapshot from any reader. `limit` bounds the bytes it may claim to hold.
pub fn read_from(input: &mut impl Read, limit: u64) -> io::Result<Index> {
    let mut file = Limited { input, remaining: limit.min(STREAM_LIMIT) };
    let mut header = [0u8; 21];
    file.read_exact(&mut header)?;
    if &header[..8] != MAGIC || header[8..12] != VERSION.to_ne_bytes() {
        return Err(invalid("not a Kestrel index snapshot of this version"));
    }
    let base_len = u32::from_ne_bytes(header[12..16].try_into().unwrap());
    let live = u32::from_ne_bytes(header[16..20].try_into().unwrap());
    let moved = header[20] != 0;

    let index = Index {
        names: read_column(&mut file)?,
        lower: read_column(&mut file)?,
        name_off: read_column(&mut file)?,
        parent: read_column(&mut file)?,
        first_child: read_column(&mut file)?,
        next_sibling: read_column(&mut file)?,
        subtree_end: read_column(&mut file)?,
        size: read_column(&mut file)?,
        alloc: read_column(&mut file)?,
        mtime: read_column(&mut file)?,
        attrs: read_column(&mut file)?,
        ext: read_column(&mut file)?,
        file_id: read_column(&mut file)?,
        name_rank: read_column(&mut file)?,
        roots: read_column(&mut file)?,
        exts: {
            let bytes: Vec<u8> = read_column(&mut file)?;
            let text = String::from_utf8(bytes).map_err(|_| invalid("corrupt extension table"))?;
            ExtTable::from_names(text.split('\0').map(Into::into).collect())
        },
        file_ids: read_column(&mut file)?,
        file_ids_added: Default::default(),
        file_ids_covered: 0,
        journal: {
            let bytes: Vec<u8> = read_column(&mut file)?;
            let text = String::from_utf8(bytes).map_err(|_| invalid("corrupt journal cursors"))?;
            text.lines().map(parse_cursor).collect::<Option<_>>().ok_or_else(|| invalid("corrupt journal cursors"))?
        },
        base_len,
        moved,
        live,
    };
    let mut index = index;
    index.file_ids_covered = index.len();
    validate(&index)?;
    Ok(index)
}

fn parse_cursor(line: &str) -> Option<JournalCursor> {
    let mut fields = line.split('\t');
    Some(JournalCursor {
        root: fields.next()?.to_owned(),
        journal_id: fields.next()?.parse().ok()?,
        next_usn: fields.next()?.parse().ok()?,
    })
}

/// Checks the invariants that unchecked accesses elsewhere rely on.
fn validate(index: &Index) -> io::Result<()> {
    let n = index.parent.len();
    let same_len = [
        index.first_child.len(),
        index.next_sibling.len(),
        index.subtree_end.len(),
        index.size.len(),
        index.alloc.len(),
        index.mtime.len(),
        index.attrs.len(),
        index.ext.len(),
        index.file_id.len(),
    ]
    .iter()
    .all(|&len| len == n);
    let arenas_ok = index.name_off.len() == n + 1
        && index.names.len() == index.lower.len()
        && index.name_off.last().is_some_and(|&end| end as usize == index.names.len())
        && index.name_off.windows(2).all(|w| w[0] < w[1])
        // Every name ends at a NUL, so name slices always fall on char boundaries.
        && index.name_off[1..].iter().all(|&end| {
            index.names[end as usize - 1] == 0 && index.lower[end as usize - 1] == 0
        })
        && std::str::from_utf8(&index.names).is_ok()
        && std::str::from_utf8(&index.lower).is_ok();
    let in_range = |id: u32| id == u32::MAX || (id as usize) < n;
    let links_ok = index.parent.iter().all(|&p| in_range(p))
        && index.first_child.iter().all(|&c| in_range(c))
        && index.next_sibling.iter().all(|&s| in_range(s))
        && index.roots.iter().all(|&r| (r as usize) < n)
        && index.ext.iter().all(|&e| (e as usize) < index.exts.names.len())
        && index.name_rank.len() <= n
        && index.file_ids.iter().all(|&packed| (packed as u32 as usize) < n)
        && index.base_len as usize <= n;
    if same_len && arenas_ok && links_ok {
        Ok(())
    } else {
        Err(invalid("index snapshot failed validation"))
    }
}

fn write_column<T: Pod>(out: &mut impl Write, column: &[T]) -> io::Result<()> {
    out.write_all(&(column.len() as u64).to_ne_bytes())?;
    out.write_all(bytemuck::cast_slice(column))
}

/// A reader that knows how many bytes may still come, so column lengths can be checked
/// before anything is allocated for them.
struct Limited<'a, R> {
    input: &'a mut R,
    remaining: u64,
}

impl<R: Read> Read for Limited<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.input.read(buffer)?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

fn read_column<T: Pod, R: Read>(file: &mut Limited<'_, R>) -> io::Result<Vec<T>> {
    let mut len = [0u8; 8];
    file.read_exact(&mut len)?;
    let len = u64::from_ne_bytes(len);
    // A corrupt length must not turn into a multi-gigabyte allocation.
    if len.saturating_mul(size_of::<T>() as u64) > file.remaining {
        return Err(invalid("index snapshot is truncated"));
    }
    let mut column = vec![T::zeroed(); len as usize];
    file.read_exact(bytemuck::cast_slice_mut(&mut column))?;
    Ok(column)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexBuilder, Meta, attr};

    #[test]
    fn round_trips() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        let dir = b.push(root, "Dir", Meta { attrs: attr::DIRECTORY, ..Meta::default() });
        b.push(dir, "File.TXT", Meta { size: 42, alloc: 4096, mtime: 7, attrs: 0x20, file_id: 9 });
        let mut original = b.finish();
        // Include a post-build mutation so the non-base region is covered too.
        original.insert(dir, "later.rs", Meta { size: 8, file_id: 77, ..Meta::default() });
        original.advance_journal("C:\\", 0xabc, 12345);

        let path = std::env::temp_dir().join(format!("kestrel-snap-{}.kidx", std::process::id()));
        save(&original, &path).unwrap();
        let loaded = load(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(loaded.len(), original.len());
        assert_eq!(loaded.live_count(), original.live_count());
        for id in 0..loaded.len() {
            assert_eq!(loaded.path(id), original.path(id));
            assert_eq!(loaded.size(id), original.size(id));
            assert_eq!(loaded.attrs(id), original.attrs(id));
        }
        let file = loaded.lookup(r"c:\dir\file.txt").unwrap();
        assert_eq!((loaded.alloc(file), loaded.mtime(file), loaded.file_id(file)), (4096, 7, 9));
        assert_eq!(loaded.ext_id("rs"), original.ext_id("rs"));
        assert!(loaded.ext_id("rs").is_some());
        assert_eq!(loaded.journal(), original.journal());
        assert_eq!(loaded.entries_with_file_id(9), [file]);
        let later = loaded.lookup(r"c:\dir\later.rs").unwrap();
        assert_eq!(loaded.entries_with_file_id(77), [later]);
    }

    #[test]
    fn streams_through_any_reader_and_writer() {
        let mut b = IndexBuilder::new();
        let root = b.add_root("C:");
        b.push(root, "streamed.txt", Meta { size: 3, file_id: 4, ..Meta::default() });
        let original = b.finish();
        let mut bytes = Vec::new();
        write_to(&original, &mut bytes).unwrap();
        let loaded = read_from(&mut bytes.as_slice(), bytes.len() as u64).unwrap();
        assert_eq!(loaded.path(loaded.lookup(r"c:\streamed.txt").unwrap()), r"C:\streamed.txt");
        assert!(read_from(&mut &bytes[..bytes.len() / 2], bytes.len() as u64 / 2).is_err());
    }

    #[test]
    fn rejects_garbage() {
        let path = std::env::temp_dir().join(format!("kestrel-bad-{}.kidx", std::process::id()));
        fs::write(&path, b"definitely not an index").unwrap();
        assert!(load(&path).is_err());
        fs::remove_file(&path).unwrap();
    }
}
