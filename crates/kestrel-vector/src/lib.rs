//! A flat store of f16 vectors with a key per row, memory-mapped and searched by brute force.
//!
//! Two files in one folder, written side by side:
//!
//! - `vectors.kvec`: a 64-byte header, then every row's vector as `dim` little-endian f16s,
//!   one after another. It is mapped read-only and scanned in parallel; nothing else is in the
//!   way of the scan, so the loop reads memory front to back.
//! - `keys.kkey`: the same header shape, then one 40-byte [`Key`] per row: which file (volume
//!   and file id), its size and modification time when it was embedded, for a video the moment
//!   the frame was taken from, and flags (a tombstone, or a frame).
//!
//! Rows are only ever appended. A row whose file changed or went is tombstoned in place (one
//! flag written), and [`Store::compact`] rewrites both files without them. Both headers carry a
//! generation that compaction bumps, so a crash between its two renames is noticed when the
//! store is next opened (the store then starts empty rather than pairing the wrong rows), and a
//! row half-written by a crash during an append is cut off.
//!
//! Vectors are L2-normalised as they are stored and queries as they are searched, so a dot
//! product is the cosine similarity. A search over 200,000 rows of 1,152 dimensions reads 460 MB
//! and takes a few tens of milliseconds on this machine's 24 cores (`kestrel-cli vector-bench`).

mod kernel;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use half::f16;
use memmap2::Mmap;
use rayon::prelude::*;

pub use kernel::kernel_name;

const HEADER: usize = 64;
const KEY_BYTES: usize = 40;
const VERSION: u32 = 1;
const VEC_MAGIC: &[u8; 8] = b"KVEC\x00\x00\x00\x01";
const KEY_MAGIC: &[u8; 8] = b"KKEY\x00\x00\x00\x01";
/// The longest model tag a header holds.
const TAG_BYTES: usize = 40;
/// Rows a search hands to one thread at a time.
const CHUNK: usize = 2048;

const FLAG_DEAD: u32 = 1;
const FLAG_FRAME: u32 = 2;

/// What a row is the vector of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    /// Which volume the file is on, as the caller numbers volumes (Kestrel uses the drive
    /// letter's code), since file ids are unique only within one.
    pub volume: u32,
    pub file_id: u64,
    /// The file's size and modification time (FILETIME ticks) when it was embedded: a file that
    /// no longer has both is embedded again.
    pub size: u64,
    pub mtime: i64,
    /// For a frame of a video, where in it, in milliseconds.
    pub at_ms: Option<u32>,
}

impl Key {
    /// The file this row belongs to, whatever its version.
    pub fn file(&self) -> (u32, u64) {
        (self.volume, self.file_id)
    }

    fn encode(&self, dead: bool) -> [u8; KEY_BYTES] {
        let mut out = [0u8; KEY_BYTES];
        let flags = if dead { FLAG_DEAD } else { 0 } | if self.at_ms.is_some() { FLAG_FRAME } else { 0 };
        out[0..4].copy_from_slice(&self.volume.to_le_bytes());
        out[4..8].copy_from_slice(&flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.file_id.to_le_bytes());
        out[16..24].copy_from_slice(&self.size.to_le_bytes());
        out[24..32].copy_from_slice(&self.mtime.to_le_bytes());
        out[32..36].copy_from_slice(&self.at_ms.unwrap_or(0).to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> (Key, bool) {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default());
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default());
        let flags = u32_at(4);
        let key = Key {
            volume: u32_at(0),
            file_id: u64_at(8),
            size: u64_at(16),
            mtime: u64_at(24) as i64,
            at_ms: (flags & FLAG_FRAME != 0).then(|| u32_at(32)),
        };
        (key, flags & FLAG_DEAD != 0)
    }
}

/// A row that matched a search, with its cosine similarity to the query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub row: u32,
    pub score: f32,
}

/// The best row of each file among a search's hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileHit {
    pub key: Key,
    pub row: u32,
    pub score: f32,
}

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    /// The files hold vectors of another model or size; the caller starts a new store.
    Mismatch(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Io(error) => write!(f, "{error}"),
            OpenError::Mismatch(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        OpenError::Io(error)
    }
}

struct Header {
    dim: u32,
    generation: u64,
    tag: String,
}

impl Header {
    fn encode(&self, magic: &[u8; 8]) -> [u8; HEADER] {
        let mut out = [0u8; HEADER];
        out[0..8].copy_from_slice(magic);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&self.dim.to_le_bytes());
        out[16..24].copy_from_slice(&self.generation.to_le_bytes());
        let tag = self.tag.as_bytes();
        out[24..24 + tag.len()].copy_from_slice(tag);
        out
    }

    fn decode(bytes: &[u8], magic: &[u8; 8]) -> Option<Header> {
        if bytes.len() < HEADER || &bytes[0..8] != magic || u32::from_le_bytes(bytes[8..12].try_into().ok()?) != VERSION {
            return None;
        }
        let tag = &bytes[24..24 + TAG_BYTES];
        let end = tag.iter().position(|&b| b == 0).unwrap_or(TAG_BYTES);
        Some(Header {
            dim: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            generation: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
            tag: String::from_utf8_lossy(&tag[..end]).into_owned(),
        })
    }
}

/// The store. Searches take `&self` and run on rayon's pool; changes take `&mut self`, so a
/// caller sharing one puts it behind a read-write lock.
pub struct Store {
    dir: PathBuf,
    dim: usize,
    tag: String,
    generation: u64,
    vectors: File,
    keys_file: File,
    /// The vectors file as mapped; covers `mapped_rows` rows.
    map: Option<Mmap>,
    mapped_rows: usize,
    keys: Vec<Key>,
    dead: Vec<bool>,
    live: usize,
    /// Every row of each file, dead or alive, in row order.
    by_file: HashMap<(u32, u64), Vec<u32>>,
}

fn vectors_path(dir: &Path) -> PathBuf {
    dir.join("vectors.kvec")
}

fn keys_path(dir: &Path) -> PathBuf {
    dir.join("keys.kkey")
}

fn read_header(file: &mut File, magic: &[u8; 8]) -> io::Result<Option<Header>> {
    let mut bytes = [0u8; HEADER];
    file.seek(SeekFrom::Start(0))?;
    match file.read_exact(&mut bytes) {
        Ok(()) => Ok(Header::decode(&bytes, magic)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

fn open_rw(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

/// Scales `vector` to unit length in place; a zero vector stays zero.
pub fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
}

impl Store {
    /// Opens the store in `dir`, creating it if there is none. Vectors of another size or
    /// another model (`tag`, at most 40 bytes) are an [`OpenError::Mismatch`].
    pub fn open(dir: &Path, dim: usize, tag: &str) -> Result<Store, OpenError> {
        if tag.len() > TAG_BYTES || dim == 0 {
            return Err(OpenError::Mismatch(format!("bad dimension {dim} or tag {tag:?}")));
        }
        std::fs::create_dir_all(dir)?;
        let mut vectors = open_rw(&vectors_path(dir))?;
        let mut keys_file = open_rw(&keys_path(dir))?;
        let (vec_header, key_header) = (read_header(&mut vectors, VEC_MAGIC)?, read_header(&mut keys_file, KEY_MAGIC)?);
        let empty = vectors.metadata()?.len() == 0 && keys_file.metadata()?.len() == 0;
        let generation = match (vec_header, key_header) {
            _ if empty => {
                let header = Header { dim: dim as u32, generation: 1, tag: tag.to_owned() };
                vectors.write_all(&header.encode(VEC_MAGIC))?;
                keys_file.write_all(&header.encode(KEY_MAGIC))?;
                1
            }
            (Some(v), Some(k)) if v.generation == k.generation && v.dim == k.dim && v.tag == k.tag => {
                if v.dim as usize != dim || v.tag != tag {
                    return Err(OpenError::Mismatch(format!(
                        "the store holds {} vectors of {} dimensions, not {tag} of {dim}",
                        v.tag, v.dim
                    )));
                }
                v.generation
            }
            _ => return Err(OpenError::Mismatch("the store's two files don't belong together".into())),
        };
        let mut store = Store {
            dir: dir.to_owned(),
            dim,
            tag: tag.to_owned(),
            generation,
            vectors,
            keys_file,
            map: None,
            mapped_rows: 0,
            keys: Vec::new(),
            dead: Vec::new(),
            live: 0,
            by_file: HashMap::new(),
        };
        store.load()?;
        Ok(store)
    }

    /// Opens the store in `dir`, or, when what is there belongs to another model or is damaged,
    /// starts it again empty.
    pub fn open_or_reset(dir: &Path, dim: usize, tag: &str) -> io::Result<Store> {
        match Store::open(dir, dim, tag) {
            Ok(store) => Ok(store),
            Err(OpenError::Io(error)) => Err(error),
            Err(OpenError::Mismatch(_)) => {
                for path in [vectors_path(dir), keys_path(dir)] {
                    match std::fs::remove_file(&path) {
                        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                        _ => {}
                    }
                }
                Store::open(dir, dim, tag).map_err(|error| match error {
                    OpenError::Io(error) => error,
                    OpenError::Mismatch(why) => io::Error::other(why),
                })
            }
        }
    }

    /// Reads the key table and maps the vectors, cutting off a row half-written by a crash.
    fn load(&mut self) -> io::Result<()> {
        // A file with a mapped view can't be shortened.
        self.map = None;
        self.mapped_rows = 0;
        let row_bytes = (self.dim * 2) as u64;
        let vec_rows = (self.vectors.metadata()?.len().saturating_sub(HEADER as u64) / row_bytes) as usize;
        let key_rows = (self.keys_file.metadata()?.len().saturating_sub(HEADER as u64) / KEY_BYTES as u64) as usize;
        let rows = vec_rows.min(key_rows);
        self.vectors.set_len(HEADER as u64 + rows as u64 * row_bytes)?;
        self.keys_file.set_len((HEADER + rows * KEY_BYTES) as u64)?;
        let mut table = vec![0u8; rows * KEY_BYTES];
        self.keys_file.seek(SeekFrom::Start(HEADER as u64))?;
        self.keys_file.read_exact(&mut table)?;
        self.keys.clear();
        self.dead.clear();
        self.by_file.clear();
        self.live = 0;
        for (row, bytes) in table.chunks_exact(KEY_BYTES).enumerate() {
            let (key, dead) = Key::decode(bytes);
            self.push_key(row as u32, key, dead);
        }
        self.remap()
    }

    fn push_key(&mut self, row: u32, key: Key, dead: bool) {
        self.by_file.entry(key.file()).or_default().push(row);
        self.keys.push(key);
        self.dead.push(dead);
        if !dead {
            self.live += 1;
        }
    }

    fn remap(&mut self) -> io::Result<()> {
        self.map = None;
        self.mapped_rows = 0;
        if self.keys.is_empty() {
            return Ok(());
        }
        // SAFETY: the file is this store's own and is only ever appended to, through `self`;
        // rows already mapped never change. Another process editing the file is outside the
        // contract (the data folder is the app's).
        let map = unsafe { Mmap::map(&self.vectors)? };
        self.mapped_rows = (map.len().saturating_sub(HEADER)) / (self.dim * 2);
        self.map = Some(map);
        Ok(())
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Rows, tombstoned ones included.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Rows not tombstoned.
    pub fn live(&self) -> usize {
        self.live
    }

    /// Distinct files with at least one live row.
    pub fn live_files(&self) -> usize {
        self.by_file.values().filter(|rows| rows.iter().any(|&row| !self.dead[row as usize])).count()
    }

    pub fn key(&self, row: u32) -> Option<Key> {
        self.keys.get(row as usize).copied()
    }

    pub fn is_live(&self, row: u32) -> bool {
        self.dead.get(row as usize).is_some_and(|dead| !dead)
    }

    /// The live rows of a file.
    pub fn rows_of(&self, volume: u32, file_id: u64) -> Vec<u32> {
        self.by_file.get(&(volume, file_id)).map_or_else(Vec::new, |rows| rows.iter().copied().filter(|&row| self.is_live(row)).collect())
    }

    /// Whether the file is stored as it is now: a live row with this size and time.
    pub fn has_current(&self, volume: u32, file_id: u64, size: u64, mtime: i64) -> bool {
        self.rows_of(volume, file_id).iter().any(|&row| {
            let key = self.keys[row as usize];
            key.size == size && key.mtime == mtime
        })
    }

    /// A row's vector, widened.
    pub fn vector(&self, row: u32) -> Option<Vec<f32>> {
        let bits = self.row_bits(row as usize)?;
        Some(bits.iter().map(|&b| f16::from_bits(b).to_f32()).collect())
    }

    fn row_bits(&self, row: usize) -> Option<&[u16]> {
        if row >= self.mapped_rows {
            return None;
        }
        let map = self.map.as_ref()?;
        let start = HEADER + row * self.dim * 2;
        bytemuck::try_cast_slice(&map[start..start + self.dim * 2]).ok()
    }

    /// Appends rows, normalising each vector, and returns the first new row's number. A row's
    /// vector must have the store's dimension.
    pub fn append(&mut self, rows: &[(Key, Vec<f32>)]) -> io::Result<u32> {
        let first = self.keys.len() as u32;
        if rows.is_empty() {
            return Ok(first);
        }
        let mut vector_bytes = Vec::with_capacity(rows.len() * self.dim * 2);
        let mut key_bytes = Vec::with_capacity(rows.len() * KEY_BYTES);
        for (key, vector) in rows {
            if vector.len() != self.dim {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("a vector of {} dimensions, not {}", vector.len(), self.dim)));
            }
            let mut unit = vector.clone();
            normalize(&mut unit);
            vector_bytes.extend(unit.iter().flat_map(|&v| f16::from_f32(v).to_bits().to_le_bytes()));
            key_bytes.extend_from_slice(&key.encode(false));
        }
        // Vectors first: a crash between the two writes leaves a vector with no key, which the
        // next open cuts off, rather than a key whose vector is missing.
        self.vectors.seek(SeekFrom::Start(HEADER as u64 + first as u64 * (self.dim * 2) as u64))?;
        self.vectors.write_all(&vector_bytes)?;
        self.keys_file.seek(SeekFrom::Start((HEADER + first as usize * KEY_BYTES) as u64))?;
        self.keys_file.write_all(&key_bytes)?;
        for (offset, (key, _)) in rows.iter().enumerate() {
            self.push_key(first + offset as u32, *key, false);
        }
        self.remap()?;
        Ok(first)
    }

    /// Marks a row as gone. Returns whether it was live.
    pub fn tombstone(&mut self, row: u32) -> io::Result<bool> {
        if !self.is_live(row) {
            return Ok(false);
        }
        let key = self.keys[row as usize];
        self.keys_file.seek(SeekFrom::Start((HEADER + row as usize * KEY_BYTES) as u64))?;
        self.keys_file.write_all(&key.encode(true)[..8])?;
        self.dead[row as usize] = true;
        self.live -= 1;
        Ok(true)
    }

    /// Tombstones every row of a file; returns how many there were.
    pub fn tombstone_file(&mut self, volume: u32, file_id: u64) -> io::Result<usize> {
        let mut gone = 0;
        for row in self.rows_of(volume, file_id) {
            gone += usize::from(self.tombstone(row)?);
        }
        Ok(gone)
    }

    /// Writes everything to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.vectors.sync_data()?;
        self.keys_file.sync_data()
    }

    /// The share of rows that are tombstones, which says when compacting is worth it.
    pub fn dead_share(&self) -> f32 {
        if self.keys.is_empty() { 0.0 } else { (self.keys.len() - self.live) as f32 / self.keys.len() as f32 }
    }

    /// Rewrites both files without the tombstoned rows. Row numbers change; returns how many
    /// rows went.
    pub fn compact(&mut self) -> io::Result<usize> {
        let removed = self.keys.len() - self.live;
        if removed == 0 {
            return Ok(0);
        }
        let generation = self.generation + 1;
        let header = Header { dim: self.dim as u32, generation, tag: self.tag.clone() };
        let (vec_tmp, key_tmp) = (self.dir.join("vectors.kvec.new"), self.dir.join("keys.kkey.new"));
        {
            let mut vectors = io::BufWriter::new(File::create(&vec_tmp)?);
            let mut keys = io::BufWriter::new(File::create(&key_tmp)?);
            vectors.write_all(&header.encode(VEC_MAGIC))?;
            keys.write_all(&header.encode(KEY_MAGIC))?;
            for row in 0..self.keys.len() {
                if self.dead[row] {
                    continue;
                }
                let bits = self.row_bits(row).ok_or_else(|| io::Error::other("a row is past the mapped vectors"))?;
                vectors.write_all(bytemuck::cast_slice(bits))?;
                keys.write_all(&self.keys[row].encode(false))?;
            }
            vectors.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            keys.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        // Windows refuses to replace a file that is mapped, so the map goes first; assigning the
        // handles drops the old files' (Rust opens files with delete sharing, so holding the
        // new ones does not stop them being renamed).
        self.map = None;
        self.mapped_rows = 0;
        self.vectors = File::open(&vec_tmp)?;
        self.keys_file = File::open(&key_tmp)?;
        // A failure between the two renames leaves files of two generations, which the next
        // open reports as a mismatch (and `open_or_reset` starts again).
        let renamed = std::fs::rename(&vec_tmp, vectors_path(&self.dir))
            .and_then(|()| std::fs::rename(&key_tmp, keys_path(&self.dir)));
        self.vectors = open_rw(&vectors_path(&self.dir))?;
        self.keys_file = open_rw(&keys_path(&self.dir))?;
        renamed?;
        self.generation = generation;
        self.load()?;
        Ok(removed)
    }

    /// The `limit` live rows most similar to `query` with a score of at least `min_score`, best
    /// first. `query` is normalised here.
    pub fn search(&self, query: &[f32], limit: usize, min_score: f32) -> Vec<Hit> {
        self.search_where(query, limit, min_score, |_| true)
    }

    /// [`Store::search`] over the live rows `allow` accepts.
    pub fn search_where(&self, query: &[f32], limit: usize, min_score: f32, allow: impl Fn(&Key) -> bool + Sync) -> Vec<Hit> {
        if query.len() != self.dim || limit == 0 || self.mapped_rows == 0 {
            return Vec::new();
        }
        let mut unit = query.to_vec();
        normalize(&mut unit);
        let rows = self.mapped_rows.min(self.keys.len());
        let chunks = rows.div_ceil(CHUNK);
        let best = (0..chunks)
            .into_par_iter()
            .fold(
                || TopK::new(limit),
                |mut top, chunk| {
                    let end = ((chunk + 1) * CHUNK).min(rows);
                    for row in chunk * CHUNK..end {
                        if self.dead[row] || !allow(&self.keys[row]) {
                            continue;
                        }
                        let Some(bits) = self.row_bits(row) else { continue };
                        let score = kernel::dot(&unit, bits);
                        if score >= min_score {
                            top.offer(Hit { row: row as u32, score });
                        }
                    }
                    top
                },
            )
            .reduce(|| TopK::new(limit), TopK::merge);
        best.into_sorted()
    }

    /// The best row of each file among the `limit` best rows, best first: a video with many
    /// frames is one result, at its best moment.
    pub fn best_per_file(&self, hits: &[Hit]) -> Vec<FileHit> {
        let mut seen: HashMap<(u32, u64), usize> = HashMap::new();
        let mut out: Vec<FileHit> = Vec::new();
        for hit in hits {
            let Some(key) = self.key(hit.row) else { continue };
            match seen.get(&key.file()) {
                Some(&at) if out[at].score >= hit.score => {}
                Some(&at) => out[at] = FileHit { key, row: hit.row, score: hit.score },
                None => {
                    seen.insert(key.file(), out.len());
                    out.push(FileHit { key, row: hit.row, score: hit.score });
                }
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.row.cmp(&b.row)));
        out
    }
}

/// A hit ordered so that a `BinaryHeap` keeps the worst on top.
#[derive(PartialEq)]
struct Worst(Hit);

impl Eq for Worst {}

impl PartialOrd for Worst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Worst {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lower scores are "greater", so they sit on top to be dropped; ties keep earlier rows.
        other.0.score.total_cmp(&self.0.score).then(self.0.row.cmp(&other.0.row))
    }
}

/// The best `limit` hits seen.
struct TopK {
    limit: usize,
    heap: BinaryHeap<Worst>,
}

impl TopK {
    fn new(limit: usize) -> Self {
        TopK { limit, heap: BinaryHeap::with_capacity(limit.min(4096) + 1) }
    }

    fn offer(&mut self, hit: Hit) {
        if self.heap.len() < self.limit {
            self.heap.push(Worst(hit));
        } else if let Some(worst) = self.heap.peek()
            && Worst(hit).cmp(worst) == Ordering::Less
        {
            self.heap.pop();
            self.heap.push(Worst(hit));
        }
    }

    fn merge(mut self, other: TopK) -> TopK {
        for Worst(hit) in other.heap {
            self.offer(hit);
        }
        self
    }

    fn into_sorted(self) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self.heap.into_iter().map(|worst| worst.0).collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.row.cmp(&b.row)));
        hits
    }
}

#[cfg(test)]
mod tests;
