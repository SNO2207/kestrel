//! Fixtures for the tests: indexes built from a list of paths, and scratch folders under
//! %TEMP% laid out like a real machine (see `Locations::under`), with files aged as needed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kestrel_index::{Index, IndexBuilder, Meta, attr};

use crate::{Context, Locations, Tools, now};

/// An entry of a synthetic index, by its path below `C:`.
#[derive(Clone)]
pub struct Entry {
    path: &'static str,
    meta: Meta,
}

impl Entry {
    pub fn file(path: &'static str) -> Self {
        Entry { path, meta: Meta { size: 4096, alloc: 4096, mtime: now(), attrs: 0, file_id: 0 } }
    }

    pub fn dir(path: &'static str) -> Self {
        Entry { path, meta: Meta { attrs: attr::DIRECTORY, mtime: now(), ..Meta::default() } }
    }

    pub fn attrs(mut self, attrs: u32) -> Self {
        self.meta.attrs |= attrs;
        self
    }
}

/// An index of `C:` holding `entries`, with the folders above each made as needed.
pub fn synthetic(entries: &[Entry]) -> Index {
    let mut builder = IndexBuilder::new();
    let root = builder.add_root("C:");
    let mut ids: HashMap<String, u32> = HashMap::new();
    for entry in entries {
        let parts: Vec<&str> = entry.path.split('\\').collect();
        let mut parent = root;
        let mut sofar = String::new();
        for (i, part) in parts.iter().enumerate() {
            sofar.push('\\');
            sofar.push_str(part);
            let last = i == parts.len() - 1;
            let meta = if last { entry.meta } else { Meta { attrs: attr::DIRECTORY, mtime: now(), ..Meta::default() } };
            parent = *ids.entry(sofar.to_lowercase()).or_insert_with(|| builder.push(parent, part, meta));
        }
    }
    builder.finish()
}

/// A scratch folder under %TEMP%, removed when dropped.
pub struct Sandbox {
    pub base: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Sets the last write time of a file or folder.
pub fn set_mtime(path: &Path, time: SystemTime) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let file = fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    file.set_modified(time).unwrap();
}

impl Sandbox {
    /// A fresh scratch folder; `name` keeps tests running side by side apart.
    pub fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kestrel-cleanup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Sandbox { base }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    pub fn text(&self, rel: &str) -> String {
        self.path(rel).to_string_lossy().into_owned()
    }

    /// Writes a file of `len` bytes (enough to take clusters of its own beyond a few hundred),
    /// whose content depends on `seed`.
    pub fn write(&self, rel: &str, len: usize, seed: u8) -> PathBuf {
        let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect();
        self.write_bytes(rel, &data)
    }

    pub fn write_bytes(&self, rel: &str, data: &[u8]) -> PathBuf {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let path = self.path(rel);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Makes the file or folder at `rel` look untouched for `days`.
    pub fn age(&self, rel: &str, days: u64) {
        set_mtime(&self.path(rel), SystemTime::now() - Duration::from_secs(days * 86_400));
    }

    /// Makes `rel` and everything beneath it look untouched for `days`.
    pub fn age_tree(&self, rel: &str, days: u64) {
        let time = SystemTime::now() - Duration::from_secs(days * 86_400);
        let mut stack = vec![self.path(rel)];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            }
            set_mtime(&path, time);
        }
    }

    /// An index of the sandbox, crawled as the app crawls a drive.
    pub fn index(&self) -> Index {
        let mut builder = IndexBuilder::new();
        kestrel_index::walk::walk(self.base.to_str().unwrap(), &mut builder).unwrap();
        builder.finish()
    }

    pub fn locations(&self) -> Locations {
        Locations::under(&self.base)
    }

    /// A context for this sandbox: no projects, nothing running, no tools, an empty Recycle Bin.
    pub fn context(&self) -> Context {
        Context {
            locations: self.locations(),
            projects: Vec::new(),
            protected: Vec::new(),
            now: now(),
            running: HashSet::new(),
            tools: Tools::default(),
            recycle_bin: None,
            min_bytes: 0,
            large_file_bytes: 1 << 30,
        }
    }
}

/// Every item path of every recommendation, relative to the sandbox, with `/` for `\`.
pub fn item_paths(sandbox: &Sandbox, recs: &[crate::Recommendation]) -> Vec<String> {
    let base = sandbox.base.to_string_lossy().to_lowercase();
    let mut paths: Vec<String> = recs
        .iter()
        .flat_map(|rec| rec.items.iter())
        .map(|item| item.path.to_lowercase().strip_prefix(&base).unwrap_or(&item.path).trim_start_matches('\\').replace('\\', "/"))
        .collect();
    paths.sort();
    paths
}
