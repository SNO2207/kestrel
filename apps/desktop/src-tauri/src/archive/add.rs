//! Adding files to a zip, as Explorer does when files are dropped on one or pasted into one
//! that is open as a folder.
//!
//! A zip can't safely be changed where it lies: a failure halfway would leave it broken. So a new
//! archive is written beside it, hidden under a temporary name: the entries it keeps are copied
//! across as they are stored (nothing is decompressed), then the new files are added, under the
//! folder they were dropped into. Only once that is complete and reads back is the old archive
//! sent to the Recycle Bin through the shell (`fileops`) and the new one renamed into its place.
//! The undo journal gets both steps, so an undo recycles the new archive and restores the old one
//! (`undo.rs` runs recycles before restores), and a redo swaps them back.
//!
//! An item already in the archive under a name being added is replaced, as Explorer's "Replace"
//! does; folders already there are kept and filled. Zips protected by a password are refused:
//! their entries would need the password to be read back.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use zip::{ZipArchive, ZipWriter};

use super::address::Format;
use super::compress::{self, Level, NEXT_PARTIAL, Partial};
use super::job::{Failure, Job};
use super::reader::io_text;
use crate::fileops::{self, Done, Options, Outcome, Request};

/// What adding files to a zip did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Added {
    /// The archive, at the same path as before.
    pub archive: String,
    /// The names of the items added at the top of the folder they went into, to select there.
    pub names: Vec<String>,
    /// How many entries already there were replaced by ones of the same name.
    pub replaced: usize,
}

/// Whether `path` names a zip by its extension (the only kind Kestrel adds to).
pub fn is_zip(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).and_then(Format::of) == Some(Format::Zip)
}

/// An entry's name compared as Windows compares file names, without a folder's trailing slash.
fn key(name: &str) -> String {
    name.trim_end_matches('/').replace('\\', "/").to_lowercase()
}

/// Adds `sources` to the zip at `zip`, inside its folder `inner` (parts of the path inside the
/// archive; none for its top). Returns what was added and the outcome to record for undo: the
/// new archive made, then the old one recycled. `options` are the shell's, for recycling.
pub fn add_to_zip(zip: &Path, inner: &[String], sources: &[PathBuf], options: Options, job: &Job) -> Result<(Added, Outcome), Failure> {
    if !is_zip(zip) {
        return Err(Failure::Io("Kestrel can add files to zip archives only.".into()));
    }
    if sources.is_empty() {
        return Err(Failure::Io("There's nothing to add.".into()));
    }
    let name = zip.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_owned();
    let folder = zip.parent().ok_or_else(|| Failure::Io(format!("{name} has no folder.")))?;
    for source in sources {
        if fs::canonicalize(source).ok() == fs::canonicalize(zip).ok() {
            return Err(Failure::Io(format!("{name} can't be added to itself.")));
        }
    }

    let mut archive = ZipArchive::new(BufReader::new(File::open(zip).map_err(|error| read_failure(zip, &error))?))
        .map_err(|error| Failure::Io(format!("{name} can't be read as a zip: {error}")))?;
    let prefix: String = inner.iter().map(|part| format!("{part}/")).collect();
    let partial_path = folder.join(format!(".{name}.{}-{}.partial", std::process::id(), NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed)));
    let mut items = compress::gather(sources, &[zip, &partial_path], job)?;
    for item in &mut items {
        item.name = format!("{prefix}{}", item.name);
    }
    // The folders the new items go into, which must be entries too unless already there.
    let mut parents: Vec<String> = Vec::new();
    for at in 0..inner.len() {
        parents.push(inner[..=at].join("/"));
    }

    let adding: std::collections::HashSet<String> = items.iter().filter(|item| !item.dir).map(|item| key(&item.name)).collect();
    let mut kept_dirs = std::collections::HashSet::new();
    let mut keep = Vec::new();
    let mut replaced = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|error| Failure::Io(format!("{name} can't be read: {error}")))?;
        if entry.encrypted() {
            return Err(Failure::Io(format!("{name} is protected by a password, so Kestrel can't add to it.")));
        }
        let entry_name = entry.name().map_err(|error| Failure::Io(format!("{name} can't be read: {error}")))?.into_owned();
        if adding.contains(&key(&entry_name)) {
            replaced += 1;
            continue;
        }
        if entry.is_dir() {
            kept_dirs.insert(key(&entry_name));
        }
        keep.push(index);
    }
    // A folder that is there already is filled, not added again.
    items.retain(|item| !(item.dir && kept_dirs.contains(&key(&item.name))));
    let missing_parents: Vec<String> = parents.into_iter().filter(|parent| !kept_dirs.contains(&key(parent))).collect();

    let file = File::create_new(&partial_path).map_err(|error| Failure::Io(format!("Couldn't write {}: {}", partial_path.display(), io_text(&error))))?;
    let mut partial = Partial { path: partial_path.clone(), keep: false };
    compress::hide(&partial_path);
    let mut writer = ZipWriter::new(BufWriter::new(file));
    for &index in &keep {
        job.check()?;
        let entry = archive.by_index_raw(index).map_err(|error| Failure::Io(format!("{name} can't be read: {error}")))?;
        writer.raw_copy_file(entry).map_err(compress::zip_error)?;
    }
    for parent in &missing_parents {
        let options = zip::write::SimpleFileOptions::default();
        writer.add_directory(format!("{parent}/"), options).map_err(compress::zip_error)?;
    }
    let total: u64 = items.iter().filter(|item| !item.dir).map(|item| item.size).sum();
    let mut done = 0;
    compress::zip_items(&mut writer, &items, Level::Normal, job, &mut done, total)?;
    let out = writer.finish().map_err(compress::zip_error)?;
    compress::finish_file(out)?;
    drop(archive);

    // The new archive must read back whole before the old one goes.
    let expected = keep.len() + missing_parents.len() + items.len();
    let check = File::open(&partial_path).map_err(|error| read_failure(&partial_path, &error)).and_then(|file| {
        ZipArchive::new(BufReader::new(file)).map_err(|error| Failure::Io(format!("The new {name} didn't read back: {error}")))
    })?;
    if check.len() != expected {
        return Err(Failure::Io(format!("The new {name} holds {} items, not {expected}.", check.len())));
    }
    drop(check);
    if job.cancelled() {
        return Err(Failure::Cancelled);
    }

    let original = zip.to_string_lossy().into_owned();
    let recycled = fileops::execute(Request::Delete { paths: vec![original.clone()], permanent: false }, options).map_err(Failure::Io)?;
    let when = recycled.results().find_map(|done| match done {
        Done::Recycled { when, .. } => Some(*when),
        _ => None,
    });
    let Some(when) = when.filter(|_| recycled.completed) else {
        return Err(Failure::Io(format!("{name} wasn't sent to the Recycle Bin, so it was left as it was.")));
    };
    if let Err(error) = fs::rename(&partial_path, zip) {
        // Put the old archive back rather than leave nothing where it was.
        let _ = fileops::run(vec![fileops::Op::Restore { path: original.clone(), deleted: when }], false, options);
        return Err(Failure::Io(format!("Couldn't put the new {name} in place: {}", io_text(&error))));
    }
    partial.keep = true;
    compress::show(zip);

    let mut names: Vec<String> = sources.iter().filter_map(|source| source.file_name().map(|name| name.to_string_lossy().into_owned())).collect();
    names.dedup();
    let outcome = Outcome {
        completed: true,
        done: vec![(0, Done::Created { path: original.clone() }), (1, Done::Recycled { path: original.clone(), when })],
        missing: Vec::new(),
    };
    Ok((Added { archive: original, names, replaced }, outcome))
}

fn read_failure(path: &Path, error: &io::Error) -> Failure {
    Failure::Io(format!("Couldn't read {}: {}", path.display(), io_text(error)))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::fileops::tests::{QUIET, sandbox, text};
    use crate::recyclebin;
    use crate::undo::Journal;

    /// A zip holding `entries` (names ending in `/` are folders), written by the zip crate.
    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, SimpleFileOptions::default()).unwrap();
            } else {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    /// Every entry of a zip: its name and, for files, what it holds.
    fn read_zip(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = (0..archive.len())
            .map(|index| {
                let mut entry = archive.by_index(index).unwrap();
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                (entry.name().unwrap().into_owned(), data)
            })
            .collect();
        entries.sort();
        entries
    }

    /// The items of the Recycle Bin that came from `path`.
    fn binned(path: &Path) -> Vec<recyclebin::BinItem> {
        let wanted = fileops::key(&text(path));
        recyclebin::items().unwrap().into_iter().filter(|item| fileops::key(&item.original) == wanted).collect()
    }

    /// Deletes from the Recycle Bin whatever of a test's zip is there when the test ends, passed
    /// or failed, so a failing run leaves nothing of its own in the owner's bin.
    struct Tidy(PathBuf);

    impl Drop for Tidy {
        fn drop(&mut self) {
            let ours: Vec<String> = binned(&self.0).into_iter().map(|item| item.id).collect();
            if !ours.is_empty() {
                let _ = recyclebin::erase(&ours, QUIET);
            }
        }
    }

    /// Nothing but `names` is left in `dir`: no partial archive, hidden or not.
    fn only(dir: &Path, names: &[&str]) {
        let mut found: Vec<String> = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        found.sort();
        let mut wanted: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        wanted.sort();
        assert_eq!(found, wanted);
    }

    /// Files and a folder added to a zip, and an entry replaced: the new archive holds the old
    /// entries as they were and the new ones; the old archive is in the Recycle Bin, and an undo
    /// puts it back in place of the new one (which then goes to the bin), and a redo swaps them
    /// back. What of the test's is left in the bin is deleted from it at the end.
    #[test]
    fn files_are_added_to_a_zip_and_the_old_one_is_recycled_and_undone() {
        let dir = sandbox("zip-add");
        let pid = std::process::id();
        let zip_name = format!("kestrel-zip-test-{pid}.zip");
        let zip = dir.join(&zip_name);
        let _tidy = Tidy(zip.clone());
        make_zip(&zip, &[("docs/", b""), ("docs/old.txt", b"old"), ("keep.txt", b"keep"), ("replace.txt", b"before")]);
        let before = fs::read(&zip).unwrap();
        let more = dir.join("more");
        fs::create_dir_all(more.join("deep")).unwrap();
        fs::write(more.join("deep").join("inner.txt"), b"inner").unwrap();
        fs::write(dir.join("replace.txt"), b"after").unwrap();
        fs::write(dir.join("new.txt"), b"new").unwrap();

        let sources = [dir.join("new.txt"), dir.join("replace.txt"), more.clone()];
        let (added, outcome) = add_to_zip(&zip, &[], &sources, QUIET, &Job::quiet()).unwrap();
        assert_eq!(added.names, ["new.txt", "replace.txt", "more"]);
        assert_eq!(added.replaced, 1);
        let entries = read_zip(&zip);
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["docs/", "docs/old.txt", "keep.txt", "more/", "more/deep/", "more/deep/inner.txt", "new.txt", "replace.txt"]);
        let data = |name: &str| entries.iter().find(|(entry, _)| entry == name).unwrap().1.clone();
        assert_eq!(
            (data("docs/old.txt"), data("keep.txt"), data("replace.txt"), data("more/deep/inner.txt")),
            (b"old".to_vec(), b"keep".to_vec(), b"after".to_vec(), b"inner".to_vec())
        );
        // Windows' own reader agrees.
        let listed = std::process::Command::new(r"C:\Windows\System32\tar.exe").arg("-tf").arg(&zip).output().unwrap();
        assert!(listed.status.success());
        assert!(String::from_utf8_lossy(&listed.stdout).contains("more/deep/inner.txt"));
        only(&dir, &[&zip_name, "more", "new.txt", "replace.txt"]);
        // The sources stay where they were.
        assert!(dir.join("new.txt").exists() && more.join("deep").join("inner.txt").exists());

        let recycled = binned(&zip);
        assert_eq!(recycled.len(), 1, "{recycled:?}");
        assert_eq!(recycled[0].size, before.len() as u64);
        assert!(matches!(outcome.results().collect::<Vec<_>>().as_slice(), [Done::Created { .. }, Done::Recycled { .. }]));

        let journal = Journal::default();
        journal.record_as(&outcome, format!("adding to {zip_name}"));
        assert_eq!(journal.state().undo.unwrap(), format!("adding to {zip_name}"));
        let after = fs::read(&zip).unwrap();
        let undone = journal.undo(QUIET).unwrap();
        assert!(undone.message.starts_with("Undid adding to"), "{}", undone.message);
        assert_eq!(fs::read(&zip).unwrap(), before, "the old archive is back");
        assert_eq!(binned(&zip).len(), 1, "the new one went to the bin in its place");
        let redone = journal.redo(QUIET).unwrap();
        assert!(redone.message.starts_with("Redid adding to"), "{}", redone.message);
        assert_eq!(fs::read(&zip).unwrap(), after, "the new archive is back");

        let ours: Vec<String> = binned(&zip).into_iter().map(|item| item.id).collect();
        assert_eq!(ours.len(), 1);
        recyclebin::erase(&ours, QUIET).unwrap();
        assert!(binned(&zip).is_empty(), "nothing of the test's is left in the bin");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A zip written by another program keeps its entries as they were, extra fields and all:
    /// Windows' own tar writes Info-ZIP's Unix fields, which the entries are copied with.
    #[test]
    fn a_zip_made_by_windows_tar_takes_files_too() {
        let dir = sandbox("zip-add-tar");
        let pid = std::process::id();
        let zip_name = format!("kestrel-zip-test-{pid}-tar.zip");
        let zip = dir.join(&zip_name);
        let _tidy = Tidy(zip.clone());
        fs::write(dir.join("first.txt"), b"first").unwrap();
        fs::write(dir.join("second.txt"), b"second").unwrap();
        let made = std::process::Command::new(r"C:\Windows\System32\tar.exe")
            .args(["--format", "zip", "-cf"])
            .arg(&zip)
            .arg("-C")
            .arg(&dir)
            .arg("first.txt")
            .status()
            .unwrap();
        assert!(made.success());
        add_to_zip(&zip, &[], &[dir.join("second.txt")], QUIET, &Job::quiet()).unwrap();
        assert_eq!(read_zip(&zip), [("first.txt".to_owned(), b"first".to_vec()), ("second.txt".to_owned(), b"second".to_vec())]);
        let listed = std::process::Command::new(r"C:\Windows\System32\tar.exe").arg("-tvf").arg(&zip).output().unwrap();
        assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
        let ours: Vec<String> = binned(&zip).into_iter().map(|item| item.id).collect();
        assert_eq!(ours.len(), 1);
        recyclebin::erase(&ours, QUIET).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Files pasted into a folder of a zip go into that folder, which is made if the archive
    /// only had files under it.
    #[test]
    fn files_go_into_the_folder_of_the_zip_they_were_pasted_into() {
        let dir = sandbox("zip-add-inner");
        let pid = std::process::id();
        let zip = dir.join(format!("kestrel-zip-test-{pid}-inner.zip"));
        let _tidy = Tidy(zip.clone());
        make_zip(&zip, &[("photos/2024/a.jpg", b"a")]);
        fs::write(dir.join("b.jpg"), b"b").unwrap();
        add_to_zip(&zip, &["photos".into(), "2024".into()], &[dir.join("b.jpg")], QUIET, &Job::quiet()).unwrap();
        let names: Vec<String> = read_zip(&zip).into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["photos/", "photos/2024/", "photos/2024/a.jpg", "photos/2024/b.jpg"]);
        let ours: Vec<String> = binned(&zip).into_iter().map(|item| item.id).collect();
        recyclebin::erase(&ours, QUIET).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that can't be read (another program holds it open, sharing nothing) stops the
    /// addition before anything changes: the zip is as it was, nothing is left beside it and
    /// nothing went to the Recycle Bin. So do a zip given itself, another kind of archive, a
    /// damaged zip and a cancel.
    #[test]
    fn a_failure_leaves_the_original_untouched() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = sandbox("zip-add-fail");
        let pid = std::process::id();
        let zip_name = format!("kestrel-zip-test-{pid}-fail.zip");
        let zip = dir.join(&zip_name);
        let _tidy = Tidy(zip.clone());
        make_zip(&zip, &[("keep.txt", b"keep")]);
        let before = fs::read(&zip).unwrap();
        let locked = dir.join("locked.txt");
        fs::write(&locked, b"locked").unwrap();
        fs::write(dir.join("fine.txt"), b"fine").unwrap();
        let holder = fs::OpenOptions::new().read(true).share_mode(0).open(&locked).unwrap();
        let failed = add_to_zip(&zip, &[], &[dir.join("fine.txt"), locked.clone()], QUIET, &Job::quiet());
        assert!(matches!(&failed, Err(Failure::Io(message)) if message.contains("locked.txt")), "{failed:?}");
        drop(holder);
        assert_eq!(fs::read(&zip).unwrap(), before);
        only(&dir, &["fine.txt", &zip_name, "locked.txt"]);
        assert!(binned(&zip).is_empty());

        assert!(add_to_zip(&zip, &[], &[zip.clone()], QUIET, &Job::quiet()).is_err(), "a zip into itself");
        let other = dir.join("notes.7z");
        fs::write(&other, b"7z").unwrap();
        assert!(add_to_zip(&other, &[], &[dir.join("fine.txt")], QUIET, &Job::quiet()).is_err(), "zips only");
        let broken = dir.join("broken.zip");
        fs::write(&broken, b"not a zip").unwrap();
        assert!(add_to_zip(&broken, &[], &[dir.join("fine.txt")], QUIET, &Job::quiet()).is_err());
        assert_eq!(fs::read(&broken).unwrap(), b"not a zip");
        // A cancelled job leaves things as they were too.
        let job = Job::quiet();
        job.cancel();
        assert_eq!(add_to_zip(&zip, &[], &[dir.join("fine.txt")], QUIET, &job).unwrap_err(), Failure::Cancelled);
        assert_eq!(fs::read(&zip).unwrap(), before);
        assert!(binned(&zip).is_empty() && binned(&broken).is_empty());
        only(&dir, &["broken.zip", "fine.txt", &zip_name, "locked.txt", "notes.7z"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_password_protected_zip_is_refused() {
        let dir = sandbox("zip-add-password");
        let zip = dir.join("secret.zip");
        let _tidy = Tidy(zip.clone());
        let mut writer = ZipWriter::new(File::create(&zip).unwrap());
        let options = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "pw");
        writer.start_file("hidden.txt", options).unwrap();
        writer.write_all(b"hidden").unwrap();
        writer.finish().unwrap();
        fs::write(dir.join("add.txt"), b"x").unwrap();
        let refused = add_to_zip(&zip, &[], &[dir.join("add.txt")], QUIET, &Job::quiet());
        assert!(matches!(&refused, Err(Failure::Io(message)) if message.contains("password")), "{refused:?}");
        assert!(binned(&zip).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
