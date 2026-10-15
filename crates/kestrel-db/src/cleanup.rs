//! The duplicate finder's hash cache (`kestrel-cleanup`). A file is known by its volume and
//! file id; its hashes stand for as long as its size and last write time are what they were
//! when it was read, so a rescan only reads the files that changed. Each row also records when
//! a scan last saw it, so rows for files long gone can be dropped.

use rusqlite::params;

use crate::{Db, Migration, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_10_18_cleanup_hashes",
    sql: "CREATE TABLE cleanup_hashes (
              volume TEXT NOT NULL,
              file_id INTEGER NOT NULL,
              size INTEGER NOT NULL,
              mtime INTEGER NOT NULL,
              partial BLOB NOT NULL,
              full BLOB,
              seen INTEGER NOT NULL,
              PRIMARY KEY (volume, file_id)
          ) WITHOUT ROWID;",
}];

/// The hashes of one file, as they were for its size and last write time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedHash {
    /// The volume the file is on, folded, such as `c:`.
    pub volume: String,
    pub file_id: u64,
    pub size: u64,
    /// Last write time as a FILETIME.
    pub mtime: i64,
    /// BLAKE3 of the first and last 64 KB and the size.
    pub partial: [u8; 32],
    /// BLAKE3 of the whole file, once it has been read.
    pub full: Option<[u8; 32]>,
}

fn hash_of(blob: Vec<u8>) -> Option<[u8; 32]> {
    blob.try_into().ok()
}

impl Db {
    /// The cached hashes of these files, those that have any, in no particular order.
    pub fn cleanup_hashes(&self, keys: &[(String, u64)]) -> Result<Vec<CachedHash>> {
        self.with(|c| {
            let mut query =
                c.prepare_cached("SELECT size, mtime, partial, full FROM cleanup_hashes WHERE volume = ?1 AND file_id = ?2")?;
            let mut found = Vec::new();
            for (volume, file_id) in keys {
                let mut rows = query.query(params![volume, *file_id as i64])?;
                if let Some(row) = rows.next()? {
                    let partial: Vec<u8> = row.get(2)?;
                    let full: Option<Vec<u8>> = row.get(3)?;
                    let Some(partial) = hash_of(partial) else { continue };
                    found.push(CachedHash {
                        volume: volume.clone(),
                        file_id: *file_id,
                        size: row.get::<_, i64>(0)? as u64,
                        mtime: row.get(1)?,
                        partial,
                        full: full.and_then(hash_of),
                    });
                }
            }
            Ok(found)
        })
    }

    /// Stores hashes, replacing what was kept for the same files, and marks them seen at `seen`
    /// (seconds since the Unix epoch).
    pub fn cleanup_store_hashes(&self, rows: &[CachedHash], seen: i64) -> Result<()> {
        self.transaction(|t| {
            let mut insert = t.prepare_cached(
                "INSERT OR REPLACE INTO cleanup_hashes (volume, file_id, size, mtime, partial, full, seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for row in rows {
                insert.execute(params![
                    row.volume,
                    row.file_id as i64,
                    row.size as i64,
                    row.mtime,
                    row.partial.as_slice(),
                    row.full.as_ref().map(|full| full.as_slice()),
                    seen
                ])?;
            }
            Ok(())
        })
    }

    /// Forgets hashes no scan has seen since `seen` (seconds since the Unix epoch). Returns how many.
    pub fn cleanup_forget_hashes_before(&self, seen: i64) -> Result<usize> {
        self.with(|c| c.execute("DELETE FROM cleanup_hashes WHERE seen < ?1", params![seen]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(file_id: u64, full: Option<u8>) -> CachedHash {
        CachedHash { volume: "c:".into(), file_id, size: 10, mtime: 99, partial: [file_id as u8; 32], full: full.map(|b| [b; 32]) }
    }

    #[test]
    fn hashes_are_stored_replaced_and_forgotten() {
        let db = Db::open_in_memory().unwrap();
        let keys = [("c:".to_owned(), 1), ("c:".to_owned(), u64::MAX), ("d:".to_owned(), 1)];
        assert!(db.cleanup_hashes(&keys).unwrap().is_empty());

        // File ids use all 64 bits.
        db.cleanup_store_hashes(&[row(1, None), row(u64::MAX, Some(7))], 100).unwrap();
        let mut found = db.cleanup_hashes(&keys).unwrap();
        found.sort_by_key(|h| h.file_id);
        assert_eq!(found, [row(1, None), row(u64::MAX, Some(7))]);

        // The full hash arrives later, and a scan sees the file again.
        db.cleanup_store_hashes(&[row(1, Some(3))], 200).unwrap();
        assert_eq!(db.cleanup_hashes(&keys[..1]).unwrap(), [row(1, Some(3))]);

        assert_eq!(db.cleanup_forget_hashes_before(150).unwrap(), 1);
        assert_eq!(db.cleanup_hashes(&keys).unwrap(), [row(1, Some(3))]);
    }
}
