//! Finding the photos and videos under the scopes, from the index alone (no disk access), in
//! steps short enough to let go of the index between them: the live index takes it to write
//! every 300 ms, and a long reader would hold up everyone behind that writer.
//!
//! Hidden files, and anything inside a hidden folder, a folder whose name starts with a dot,
//! or a dependency, build or cache folder (`node_modules`, `target`, `AppData`...) are left out:
//! a Documents folder with code in it holds thousands of icons nobody would call photos.

use std::collections::HashSet;

use kestrel_index::{EntryId, Index, NO_ENTRY, attr};

use crate::pipeline::Candidate;
use crate::{Kind, kind_of_extension, volume_of};

/// Folders never looked in, compared with folded names.
const EXCLUDED: &[&str] = &[
    "node_modules",
    "bower_components",
    "site-packages",
    "__pycache__",
    "target",
    "obj",
    "venv",
    "appdata",
    "$recycle.bin",
    "system volume information",
];

/// A scan in progress: [`Scanner::step`] until it says it is done, then [`Scanner::finish`].
pub struct Scanner {
    /// The kind of each extension id in the index, by id.
    kinds: Vec<Option<Kind>>,
    scopes: HashSet<EntryId>,
    /// Folders known to be excluded (or inside one), and known to be fine, so each folder's
    /// ancestors are walked once.
    excluded: HashSet<EntryId>,
    fine: HashSet<EntryId>,
    next: u32,
    found: Vec<Candidate>,
}

/// Whether a folder's own name or attributes keep the scan out of it.
fn excluded_folder(index: &Index, dir: EntryId) -> bool {
    let name = index.lower_bytes(dir);
    index.attrs(dir) & attr::HIDDEN != 0 || name.first() == Some(&b'.') || EXCLUDED.iter().any(|excluded| excluded.as_bytes() == name)
}

impl Scanner {
    /// A scan of `index` for media under the folders `scopes` (those not in the index are
    /// passed over).
    pub fn new(index: &Index, scopes: &[String]) -> Self {
        let kinds = (0..index.ext_count()).map(|id| kind_of_extension(index.ext_name(id as u16))).collect();
        let scopes = scopes.iter().filter_map(|scope| index.lookup(scope)).filter(|&dir| index.is_dir(dir)).collect();
        Scanner { kinds, scopes, excluded: HashSet::new(), fine: HashSet::new(), next: 0, found: Vec::new() }
    }

    /// Whether the folder `dir` lies in a scope with nothing excluded on the way.
    fn admits(&mut self, index: &Index, dir: EntryId) -> bool {
        let mut walked = Vec::new();
        let mut at = dir;
        let verdict = loop {
            if at == NO_ENTRY {
                break false;
            }
            if self.fine.contains(&at) {
                break true;
            }
            if self.excluded.contains(&at) {
                break false;
            }
            if self.scopes.contains(&at) {
                break true;
            }
            if excluded_folder(index, at) {
                break false;
            }
            walked.push(at);
            at = index.parent(at);
        };
        let remember = if verdict { &mut self.fine } else { &mut self.excluded };
        remember.extend(walked);
        verdict
    }

    /// Looks at up to `count` more entries. True when the whole index has been looked at.
    pub fn step(&mut self, index: &Index, count: u32) -> bool {
        if self.scopes.is_empty() {
            return true;
        }
        let end = self.next.saturating_add(count).min(index.len());
        for id in self.next..end {
            let Some(kind) = self.kinds.get(index.ext(id) as usize).copied().flatten() else { continue };
            let attrs = index.attrs(id);
            if attrs & (attr::DIRECTORY | attr::DELETED | attr::HIDDEN) != 0 || index.size(id) == 0 {
                continue;
            }
            if !self.admits(index, index.parent(id)) {
                continue;
            }
            let path = index.path(id);
            let Some(volume) = volume_of(&path) else { continue };
            self.found.push(Candidate { volume, file_id: index.file_id(id), size: index.size(id), mtime: index.mtime(id), attrs, path, kind, entry: id });
        }
        self.next = end;
        end >= index.len()
    }

    pub fn finish(self) -> Vec<Candidate> {
        self.found
    }
}

/// Scans the whole index at once, for tests and small indexes.
pub fn scan(index: &Index, scopes: &[String]) -> Vec<Candidate> {
    let mut scanner = Scanner::new(index, scopes);
    while !scanner.step(index, 1 << 16) {}
    scanner.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, Meta};

    #[test]
    fn finds_media_under_scopes_and_nowhere_else() {
        let mut builder = IndexBuilder::new();
        let root = builder.add_root("C:");
        let dir = Meta { attrs: attr::DIRECTORY, ..Meta::default() };
        let file = |id: u64| Meta { size: 1000, alloc: 4096, mtime: 133_000_000_000_000_000, attrs: 0x20, file_id: id };
        let users = builder.push(root, "Users", dir);
        let me = builder.push(users, "me", dir);
        let pictures = builder.push(me, "Pictures", dir);
        builder.push(pictures, "beach.JPG", file(1));
        builder.push(pictures, "clip.mp4", file(2));
        builder.push(pictures, "notes.txt", file(3));
        builder.push(pictures, "empty.png", Meta { size: 0, ..file(4) });
        builder.push(pictures, "secret.png", Meta { attrs: 0x22, ..file(5) });
        let trip = builder.push(pictures, "Trip", dir);
        builder.push(trip, "day1.heic", file(6));
        let hidden = builder.push(pictures, ".thumbnails", dir);
        builder.push(hidden, "t.jpg", file(7));
        let code = builder.push(pictures, "app", dir);
        let modules = builder.push(code, "node_modules", dir);
        builder.push(modules, "icon.png", file(8));
        let elsewhere = builder.push(me, "Music", dir);
        builder.push(elsewhere, "cover.jpg", file(9));
        builder.push(pictures, "cloud.jpg", Meta { attrs: 0x40_0000 | 0x1000, ..file(10) });
        let index = builder.finish();

        let found = scan(&index, &[r"C:\Users\me\Pictures".to_owned(), r"C:\Nowhere".to_owned()]);
        let mut names: Vec<(String, u64, Kind)> = found.iter().map(|c| (c.path.clone(), c.file_id, c.kind)).collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(names, [
            (r"C:\Users\me\Pictures\Trip\day1.heic".to_owned(), 6, Kind::Photo),
            (r"C:\Users\me\Pictures\beach.JPG".to_owned(), 1, Kind::Photo),
            (r"C:\Users\me\Pictures\clip.mp4".to_owned(), 2, Kind::Video),
            // Online-only files are listed, with their attributes, for the pipeline to pass over.
            (r"C:\Users\me\Pictures\cloud.jpg".to_owned(), 10, Kind::Photo),
        ]);
        assert!(found.iter().all(|c| c.volume == "c:"));
        assert!(scan(&index, &[]).is_empty());

        // In small steps, the same.
        let mut scanner = Scanner::new(&index, &[r"C:\Users\me\Pictures".to_owned()]);
        let mut steps = 0;
        while !scanner.step(&index, 3) {
            steps += 1;
        }
        assert!(steps > 3);
        assert_eq!(scanner.finish().len(), 4);
    }
}
