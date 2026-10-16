//! Searching inside files, with ripgrep's crates: `grep-searcher` reads each file line by line
//! (UTF-16 with a byte-order mark is decoded first; a NUL byte marks a binary file, which is
//! passed over), `grep-regex` matches literal text or a regular expression with or without
//! case, whole words or not, and `ignore` matches `.gitignore` patterns inside repositories.
//!
//! [`Run`] searches the files of a folder, walked from disk with Kestrel's own listing (junctions
//! are never followed, `.git` folders never entered), or a list of files an index search
//! chose, on a pool of threads of its own, and hands each match over as it is found, with its
//! first matching line made into a snippet ([`Hit`]). It stops at once when cancelled (a read
//! in progress gives way too), and once enough files have matched.
//!
//! A file whose contents are only in the cloud (OneDrive's online-only files) is never opened:
//! its attributes are checked when it is listed and again just before it would be opened
//! ([`fsinfo::open`]), and a `.gitignore` in the cloud is taken as empty rather than read.

pub mod fsinfo;
mod ignores;
mod run;
mod search;

pub use ignores::Ignores;
pub use run::{Counts, Filter, Found, Progress, Run, Source};
pub use search::{Case, Hit, Limits, Outcome, Pattern, Search, Skip, snippet};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::*;

    const HIDDEN: u32 = 0x2;

    /// A folder of its own under the temp folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        /// A folder no other test shares: tests run side by side in one process, and several
        /// make the same fixture.
        fn new(name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("kestrel-content-{name}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }

        fn root(&self) -> String {
            self.0.to_str().unwrap().to_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Attributes set by a test would keep the folder from going.
            for entry in walk_all(&self.0) {
                set_attributes(&entry, 0x80);
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn walk_all(dir: &Path) -> Vec<PathBuf> {
        let mut all = Vec::new();
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                all.extend(walk_all(&path));
            } else {
                all.push(path);
            }
        }
        all
    }

    fn set_attributes(path: &Path, attrs: u32) {
        use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
        let wide: Vec<u16> = path.to_str().unwrap().encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        unsafe { SetFileAttributesW(wide.as_ptr(), attrs) };
    }

    fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
        let mut bytes = if big_endian { vec![0xFE, 0xFF] } else { vec![0xFF, 0xFE] };
        for unit in text.encode_utf16() {
            bytes.extend(if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
        }
        bytes
    }

    fn pattern(text: &str) -> Pattern {
        Pattern { text: text.into(), ..Pattern::default() }
    }

    /// Runs a search and returns what matched, by path below `root`, with each hit's line.
    fn search(root: &str, source: Source<'_>, patterns: &[Pattern], limits: Limits, gitignore: bool) -> (BTreeMap<String, Option<u64>>, Counts) {
        let search = Search::new(patterns, limits).unwrap();
        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let found = Mutex::new(BTreeMap::new());
        let run = Run { search: &search, gitignore, filter: None, cancel: &cancel, progress: &progress };
        run.run(source, &|hit: Found| {
            let rel = hit.path()[root.len() + 1..].to_owned();
            found.lock().insert(rel, hit.hit.map(|h| h.line));
        });
        (found.into_inner(), progress.counts())
    }

    fn shows_all(_: u32) -> bool {
        true
    }

    fn fixture() -> Scratch {
        let scratch = Scratch::new("fixture");
        scratch.write("plain.txt", b"first line\nthe Needle is here\nneedle again\n");
        scratch.write("wide-le.txt", &utf16("one\ntwo\nfind the needle\n", false));
        scratch.write("wide-be.txt", &utf16("needle at the top\n", true));
        scratch.write("latin.txt", b"caf\xe9 \xff needle \xfe\n");
        scratch.write("nothing.txt", b"no match in here\n");
        let mut binary = b"needle before a nul\n".to_vec();
        binary.extend([0u8, 1, 2, 3]);
        scratch.write("blob.txt", &binary);
        scratch.write("photo.jpg", b"needle in a file type that never holds text");
        scratch.write("deep\\down\\below\\far.md", b"# far\nneedle\n");
        scratch.write("repo\\.git\\HEAD", b"ref: refs/heads/main\n");
        scratch.write("repo\\.gitignore", b"build/\n*.tmp\n");
        scratch.write("repo\\kept.rs", b"// needle\n");
        scratch.write("repo\\scratch.tmp", b"needle\n");
        scratch.write("repo\\build\\out.txt", b"needle\n");
        scratch.write("repo\\.git\\needle.txt", b"needle\n");
        let hidden = scratch.write("secret.txt", b"needle\n");
        set_attributes(&hidden, HIDDEN);
        scratch
    }

    #[test]
    fn folders_are_searched_in_every_encoding_and_binary_files_are_passed_over() {
        let scratch = fixture();
        let root = scratch.root();
        let (found, counts) = search(&root, Source::Folder { root: root.clone(), shows: &shows_all }, &[pattern("needle")], Limits::default(), true);
        let expected: BTreeMap<String, Option<u64>> = [
            ("plain.txt", 2),
            ("wide-le.txt", 3),
            ("wide-be.txt", 1),
            ("latin.txt", 1),
            ("deep\\down\\below\\far.md", 2),
            ("repo\\kept.rs", 1),
            ("secret.txt", 1),
        ]
        .into_iter()
        .map(|(path, line)| (path.to_owned(), Some(line)))
        .collect();
        assert_eq!(found, expected);
        // blob.txt has a NUL; photo.jpg is never opened.
        assert_eq!(counts.binary, 2, "{counts:?}");
        // build/ and *.tmp, inside the repository; .git is never entered.
        assert_eq!(counts.ignored, 2, "{counts:?}");
        assert_eq!(counts.matched, 7);
        assert!(!counts.full);
    }

    #[test]
    fn gitignore_can_be_switched_off_and_hidden_files_left_out() {
        let scratch = fixture();
        let root = scratch.root();
        let visible = |attrs: u32| attrs & HIDDEN == 0;
        let (found, counts) = search(&root, Source::Folder { root: root.clone(), shows: &visible }, &[pattern("needle")], Limits::default(), false);
        assert!(found.contains_key("repo\\scratch.tmp") && found.contains_key("repo\\build\\out.txt"));
        assert!(!found.contains_key("secret.txt"));
        assert!(!found.keys().any(|path| path.contains(".git\\")), "{found:?}");
        assert_eq!(counts.ignored, 0);
    }

    #[test]
    fn the_hit_marks_the_match_on_its_line() {
        let scratch = fixture();
        let path = format!(r"{}\wide-le.txt", scratch.root());
        let search = Search::new(&[pattern("NEEDLE")], Limits::default()).unwrap();
        let Outcome::Match(Some(hit)) = search.search_file(&path, "wide-le.txt", 100, 0, &AtomicBool::new(false)) else { panic!() };
        assert_eq!((hit.line, hit.text.as_str(), hit.ranges.as_slice(), hit.lines), (3, "find the needle", &[(9, 15)][..], 1));
        let plain = format!(r"{}\plain.txt", scratch.root());
        let Outcome::Match(Some(hit)) = search.search_file(&plain, "plain.txt", 100, 0, &AtomicBool::new(false)) else { panic!() };
        assert_eq!((hit.line, hit.lines, hit.more), (2, 2, false));
        // Case matters when asked to.
        let exact = Search::new(&[Pattern { text: "Needle".into(), case: Case::Sensitive, ..Pattern::default() }], Limits::default()).unwrap();
        let Outcome::Match(Some(hit)) = exact.search_file(&plain, "plain.txt", 100, 0, &AtomicBool::new(false)) else { panic!() };
        assert_eq!((hit.line, hit.lines), (2, 1));
    }

    #[test]
    fn several_patterns_must_all_match_and_negated_ones_must_not() {
        let scratch = Scratch::new("patterns");
        scratch.write("both.txt", b"alpha\nbeta\n");
        scratch.write("alpha.txt", b"alpha only\n");
        scratch.write("all.txt", b"alpha beta gamma\n");
        let root = scratch.root();
        let patterns = [pattern("alpha"), pattern("beta"), Pattern { negate: true, ..pattern("gamma") }];
        let (found, _) = search(&root, Source::Folder { root: root.clone(), shows: &shows_all }, &patterns, Limits::default(), false);
        assert_eq!(found.keys().collect::<Vec<_>>(), ["both.txt"]);
        // A regular expression across the alternatives.
        let regex = [Pattern { text: r"gam+a|only$".into(), regex: true, ..Pattern::default() }];
        let (found, _) = search(&root, Source::Folder { root: root.clone(), shows: &shows_all }, &regex, Limits::default(), false);
        assert_eq!(found.keys().collect::<Vec<_>>(), ["all.txt", "alpha.txt"]);
    }

    #[test]
    fn size_and_count_limits() {
        let scratch = Scratch::new("limits");
        scratch.write("big.txt", &[b"needle\n".as_slice(), &vec![b'x'; 4096]].concat());
        for n in 0..30 {
            scratch.write(&format!("small{n:02}.txt"), b"needle\n");
        }
        let root = scratch.root();
        let limits = Limits { max_file_size: 1024, max_found: 10, max_counted: 5 };
        let (found, counts) = search(&root, Source::Folder { root: root.clone(), shows: &shows_all }, &[pattern("needle")], limits, false);
        assert_eq!(found.len(), 10);
        assert!(counts.full);
        assert_eq!(counts.matched, 10);
        let (_, counts) = search(&root, Source::Files(std::sync::Arc::new(vec![(root.clone(), item("big.txt", 4103))])), &[pattern("needle")], limits, false);
        assert_eq!((counts.too_large, counts.matched), (1, 0));
        // Matching lines are counted up to the limit.
        let many = scratch.write("many.txt", "needle\n".repeat(20).as_bytes());
        let search = Search::new(&[pattern("needle")], limits).unwrap();
        let Outcome::Match(Some(hit)) = search.search_file(many.to_str().unwrap(), "many.txt", 140, 0, &AtomicBool::new(false)) else { panic!() };
        assert_eq!((hit.lines, hit.more), (5, true));
    }

    fn item(name: &str, size: u64) -> kestrel_index::DirItem {
        kestrel_index::DirItem { name: name.into(), meta: kestrel_index::Meta { size, attrs: 0x20, ..Default::default() }, alias: false }
    }

    #[test]
    fn chosen_files_are_searched_with_their_repository_rules() {
        let scratch = fixture();
        let root = scratch.root();
        let files = vec![
            (root.clone(), item("plain.txt", 50)),
            (format!(r"{root}\repo"), item("kept.rs", 10)),
            (format!(r"{root}\repo"), item("scratch.tmp", 7)),
            (format!(r"{root}\repo\build"), item("out.txt", 7)),
            (root.clone(), item("nothing.txt", 17)),
        ];
        let (found, counts) = search(&root, Source::Files(std::sync::Arc::new(files.clone())), &[pattern("needle")], Limits::default(), true);
        assert_eq!(found.keys().collect::<Vec<_>>(), ["plain.txt", "repo\\kept.rs"]);
        assert_eq!(counts.ignored, 2);
        assert_eq!(counts.searched, 3);
        let (found, _) = search(&root, Source::Files(std::sync::Arc::new(files)), &[pattern("needle")], Limits::default(), false);
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn files_only_in_the_cloud_are_never_opened() {
        use std::os::windows::fs::OpenOptionsExt;
        let scratch = Scratch::new("cloud");
        let path = scratch.write("online.txt", b"needle\n");
        // Held open by nobody else: opening it would succeed, so a skip proves it wasn't tried.
        set_attributes(&path, 0x1000);
        let searcher = Search::new(&[pattern("needle")], Limits::default()).unwrap();
        let text = path.to_str().unwrap();
        let cancel = AtomicBool::new(false);
        // As listed, and as found again just before opening when the listing was stale.
        assert_eq!(searcher.search_file(text, "online.txt", 7, 0x1000, &cancel), Outcome::Skipped(Skip::CloudOnly));
        assert_eq!(searcher.search_file(text, "online.txt", 7, 0x20, &cancel), Outcome::Skipped(Skip::CloudOnly));
        assert!(fsinfo::open(text).is_none());
        // A file held open without sharing can't be read: that is reported as such.
        set_attributes(&path, 0x80);
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        assert_eq!(searcher.search_file(text, "online.txt", 7, 0x20, &cancel), Outcome::Skipped(Skip::Unreadable));
        drop(held);
        let root = scratch.root();
        set_attributes(&path, 0x1000);
        let (found, counts) = search(&root, Source::Folder { root: root.clone(), shows: &shows_all }, &[pattern("needle")], Limits::default(), false);
        assert!(found.is_empty());
        assert_eq!(counts.cloud, 1);
    }

    #[test]
    fn a_cancelled_search_stops() {
        let scratch = Scratch::new("cancel");
        for n in 0..400 {
            scratch.write(&format!("d{}\\f{n:03}.txt", n % 20), b"needle\n");
        }
        let root = scratch.root();
        let search = Search::new(&[pattern("needle")], Limits::default()).unwrap();
        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let seen = Mutex::new(0usize);
        let run = Run { search: &search, gitignore: false, filter: None, cancel: &cancel, progress: &progress };
        run.run(Source::Folder { root: root.clone(), shows: &shows_all }, &|_| {
            let mut seen = seen.lock();
            *seen += 1;
            // Cancelled as a keystroke would, after the first few results.
            if *seen == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        let seen = *seen.lock();
        assert!((3..100).contains(&seen), "{seen} results after cancelling");
        // Cancelled before it starts, nothing is read.
        let progress = Progress::default();
        let run = Run { search: &search, gitignore: false, filter: None, cancel: &cancel, progress: &progress };
        run.run(Source::Folder { root, shows: &shows_all }, &|_| panic!("nothing should be found"));
        assert_eq!(progress.counts().searched, 0);
        // A read in progress gives way: a large file searched after cancelling is not read.
        let big = scratch.write("big.txt", &vec![b'x'; 8 << 20]);
        assert_eq!(search.search_file(big.to_str().unwrap(), "big.txt", 8 << 20, 0, &cancel), Outcome::Cancelled);
    }

    #[test]
    fn a_filter_chooses_files_before_they_are_read() {
        let scratch = fixture();
        let root = scratch.root();
        let search = Search::new(&[], Limits::default()).unwrap();
        assert!(!search.reads());
        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let found = Mutex::new(Vec::new());
        let only_markdown = |path: &str, _: &kestrel_index::DirItem| path.ends_with(".md");
        let run = Run { search: &search, gitignore: true, filter: Some(&only_markdown), cancel: &cancel, progress: &progress };
        run.run(Source::Folder { root: root.clone(), shows: &shows_all }, &|hit| found.lock().push((hit.item.name.clone(), hit.hit)));
        assert_eq!(found.into_inner(), [("far.md".to_owned(), None)]);
    }
}
