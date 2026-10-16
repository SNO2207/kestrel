//! What uses the disk, worked out from the in-memory index alone: no folder is read and no file
//! is opened, so a whole drive is analysed in a fraction of a second and the results follow the
//! index as it changes.
//!
//! - [`treemap`]: a squarified treemap of a folder's subtree, laid out only as deep as its
//!   rectangles stay visible.
//! - [`category`]: what type of data a file is, by its extension and where it lives.
//! - [`overview`]: totals, breakdowns by type and by age, the largest files and folders, and
//!   the files changed recently.
//! - [`growth`]: daily samples of folder sizes, which samples to keep, and what grew since one.
//!
//! Sizes follow the index. A folder's size is what the index has rolled up from its files, so
//! a hard link is charged once when the index charged it once (an index read from the file
//! table charges a file's allocation to its first name only). Cloud-only placeholders, whose
//! contents are not on this device, have no allocation: they use no disk space and are counted
//! apart, as "in the cloud".

pub mod category;
pub mod growth;
pub mod overview;
pub mod treemap;

use kestrel_index::attr;

/// FILETIME ticks (100 ns since 1601-01-01 UTC) at the Unix epoch.
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const TICKS_PER_MS: i64 = 10_000;
pub const DAY_MS: i64 = 86_400_000;

/// A FILETIME as milliseconds since the Unix epoch; 0 for times before it (unknown).
pub fn unix_ms(filetime: i64) -> i64 {
    if filetime <= FILETIME_UNIX_EPOCH { 0 } else { (filetime - FILETIME_UNIX_EPOCH) / TICKS_PER_MS }
}

/// Milliseconds since the Unix epoch as a FILETIME.
pub fn filetime(unix_ms: i64) -> i64 {
    unix_ms * TICKS_PER_MS + FILETIME_UNIX_EPOCH
}

/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`: a placeholder whose contents are fetched when it is opened.
pub const RECALL_ON_OPEN: u32 = 0x4_0000;

/// Whether a file's contents live in the cloud rather than on this device (OneDrive's "online
/// only" files and other placeholders), so it takes no space here. Nothing here reads files,
/// so none is ever downloaded.
///
/// OneDrive marks its online-only files `RECALL_ON_DATA_ACCESS` (with `OFFLINE`), in folder
/// listings and in the drive's file table alike. `FILE_ATTRIBUTE_RECALL_ON_OPEN` is no sign of
/// one: it has the value of `FILE_ATTRIBUTE_EA`, which thousands of ordinary `System32` files
/// carry. Nor is having nothing allocated: in an index read from the file table, so have the
/// second names of hard links (charged to their first), files small enough to live inside the
/// table, and files Windows keeps compressed in a stream of their own (Edge's, for one).
pub fn is_cloud_only(attrs: u32) -> bool {
    attrs & (attr::OFFLINE | attr::RECALL_ON_DATA_ACCESS) != 0
}

/// How big an item is taken to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    /// Bytes allocated on disk: what the disk is used for. Cloud-only files have none.
    #[default]
    Allocated,
    /// Bytes of content, wherever it lives.
    Logical,
}

impl Measure {
    pub fn of(self, index: &kestrel_index::Index, id: kestrel_index::EntryId) -> u64 {
        match self {
            Measure::Allocated => index.alloc(id),
            Measure::Logical => index.size(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_told_from_files_with_extended_attributes_or_nothing_allocated() {
        const ARCHIVE: u32 = 0x20;
        const SPARSE: u32 = 0x200;
        // OneDrive's online-only files, as the owner's drive records them (0x401620).
        assert!(is_cloud_only(attr::RECALL_ON_DATA_ACCESS | attr::OFFLINE | attr::REPARSE_POINT | SPARSE | ARCHIVE));
        assert!(is_cloud_only(attr::OFFLINE));
        // System32's files with extended attributes (0x40020), whose bit "recall on open" shares.
        assert!(!is_cloud_only(RECALL_ON_OPEN | ARCHIVE));
        // A file OneDrive keeps on this device, and one Windows keeps compressed (Edge's, 0x620).
        assert!(!is_cloud_only(attr::REPARSE_POINT | ARCHIVE));
        assert!(!is_cloud_only(attr::REPARSE_POINT | SPARSE | ARCHIVE));
    }

    #[test]
    fn times_convert_both_ways() {
        assert_eq!(unix_ms(filetime(1_767_225_600_000)), 1_767_225_600_000);
        assert_eq!(unix_ms(0), 0, "no time recorded");
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Small indexes described as text, for the tests of every module.

    use std::collections::HashMap;

    use kestrel_index::{EntryId, Index, IndexBuilder, Meta, attr};

    /// One file: a path relative to the root `C:`, its size (allocated the same), its
    /// modification time and attributes.
    pub struct F<'a>(pub &'a str, pub u64, pub i64, pub u32);

    /// Builds `C:\` holding these files (folders made as needed) and these empty folders.
    pub fn build(files: &[F], empty_dirs: &[&str]) -> Index {
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let mut dirs: HashMap<String, u32> = HashMap::new();
        dirs.insert(String::new(), root);
        let mut dir = |builder: &mut IndexBuilder, path: &str| -> u32 {
            let mut parent = root;
            let mut sofar = String::new();
            for part in path.split('\\').filter(|p| !p.is_empty()) {
                sofar.push('\\');
                sofar.push_str(&part.to_lowercase());
                parent = *dirs
                    .entry(sofar.clone())
                    .or_insert_with(|| builder.push(parent, part, Meta { attrs: attr::DIRECTORY, ..Meta::default() }));
            }
            parent
        };
        for path in empty_dirs {
            dir(&mut builder, path);
        }
        for (n, F(path, size, mtime, attrs)) in files.iter().enumerate() {
            let (folder, name) = path.rsplit_once('\\').unwrap_or(("", path));
            let parent = dir(&mut builder, folder);
            // Placeholders have nothing on the disk; every other file is allocated in full.
            let cloud = attrs & (attr::OFFLINE | attr::RECALL_ON_DATA_ACCESS) != 0;
            let meta = Meta {
                size: *size,
                alloc: if cloud { 0 } else { *size },
                mtime: *mtime,
                attrs: *attrs,
                file_id: 1000 + n as u64,
            };
            builder.push(parent, name, meta);
        }
        builder.finish()
    }

    pub fn id(index: &Index, path: &str) -> EntryId {
        index.lookup(path).unwrap_or_else(|| panic!("{path} is not in the test index"))
    }
}
