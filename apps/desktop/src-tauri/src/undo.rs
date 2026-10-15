//! Undo and redo. Every file operation Kestrel performs is recorded with its exact results (see
//! `fileops::Done`), so it can be reversed precisely: a rename by renaming back, a move by
//! moving back, a copy or a new folder by sending what was created to the Recycle Bin (never
//! erasing it), and a deletion to the Recycle Bin by restoring exactly those items. Each undo
//! or redo is one shell operation, with the shell's usual progress and conflict windows.
//!
//! Permanent deletions can't be undone. After one, the history before it is dropped, because
//! undoing older operations around the gap could act on the wrong items, and the UI says why.

use std::path::Path;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{State, WebviewWindow};

use crate::fileops::{self, Done, Op, Options, Outcome, file_name, parent};

/// Operations remembered; older ones fall off the end.
const LIMIT: usize = 100;

/// Ops that reverse an entry's steps, each with the position of its step in the entry.
type Steps = Vec<(usize, Op)>;

#[derive(Clone, Debug)]
struct Entry {
    id: u64,
    /// What the operation was, such as "rename of report.txt".
    label: String,
    steps: Vec<Done>,
}

#[derive(Default)]
struct History {
    next: u64,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// What the latest operation erased for good, such as "report.txt" or "3 items".
    erased: Option<String>,
}

impl History {
    fn entry(&mut self, label: String, steps: Vec<Done>) -> Entry {
        self.next += 1;
        Entry { id: self.next, label, steps }
    }
}

/// The undo and redo history, shared by every window and tab.
#[derive(Default)]
pub struct Journal {
    history: Mutex<History>,
    /// Held while an undo or redo runs, so two never overlap.
    busy: Mutex<()>,
}

/// What the Undo and Redo commands would do now, for menus.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UndoState {
    /// The operation an undo would reverse, such as "rename of report.txt".
    pub undo: Option<String>,
    pub redo: Option<String>,
    /// Set when the latest operation was a permanent deletion, which can't be undone.
    pub erased: Option<String>,
}

/// What an undo or redo did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Says what happened, such as "Undid rename of report.txt".
    pub message: String,
    /// The folder holding the items that were put back, for selecting them.
    pub folder: Option<String>,
    pub names: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Undo,
    Redo,
}

impl Direction {
    fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }
}

/// The item an op acts on, which must still exist for the op to make sense. Items restored
/// from the Recycle Bin are looked up there instead.
fn source(op: &Op) -> Option<&str> {
    match op {
        Op::Rename { path, .. } | Op::Recycle { path } | Op::Erase { path } => Some(path),
        Op::Move { source, .. } | Op::Copy { source, .. } => Some(source),
        Op::NewFolder { .. } | Op::Restore { .. } => None,
    }
}

/// The folder an op puts its item in, which must still exist: the shell can't move into a
/// folder that isn't there, and an op whose folder is gone would fail the whole undo.
fn destination(op: &Op) -> Option<String> {
    match op {
        Op::Move { folder, .. } | Op::Copy { folder, .. } => Some(folder.clone()),
        Op::NewFolder { parent, .. } => Some(parent.clone()),
        Op::Restore { path, .. } => Some(parent(path)),
        Op::Rename { .. } | Op::Recycle { .. } | Op::Erase { .. } => None,
    }
}

/// The operation that reverses one result, or `None` if it can't be reversed.
fn inverse(step: &Done) -> Option<Op> {
    Some(match step {
        Done::Renamed { from, to } => Op::Rename { path: to.clone(), name: file_name(from).to_owned() },
        Done::Moved { from, to, merged: false } => {
            Op::Move { source: to.clone(), folder: parent(from), name: Some(file_name(from).to_owned()) }
        }
        Done::Copied { to, merged: false, .. } => Op::Recycle { path: to.clone() },
        Done::Created { path } | Done::Restored { path } => Op::Recycle { path: path.clone() },
        Done::Recycled { path, when } => Op::Restore { path: path.clone(), deleted: *when },
        // A merged folder holds things that were there before, and an erased item is gone.
        Done::Moved { merged: true, .. } | Done::Copied { merged: true, .. } | Done::Erased { .. } => return None,
    })
}

/// The name an item had before the operation.
fn subject(step: &Done) -> &str {
    match step {
        Done::Renamed { from, .. } | Done::Moved { from, .. } | Done::Copied { from, .. } => file_name(from),
        Done::Created { path } | Done::Recycled { path, .. } | Done::Erased { path } | Done::Restored { path } => {
            file_name(path)
        }
    }
}

/// "report.txt", or "3 items".
fn subjects(steps: &[&Done]) -> String {
    match steps {
        [one] => subject(one).to_owned(),
        many => format!("{} items", many.len()),
    }
}

/// Describes an operation for menus and notices, such as "rename of report.txt".
fn describe(steps: &[Done]) -> String {
    let verb = match steps.first() {
        Some(Done::Renamed { .. }) => "rename",
        Some(Done::Moved { .. }) => "move",
        Some(Done::Copied { .. }) => "copy",
        Some(Done::Created { .. }) => "creation",
        Some(Done::Recycled { .. }) => "delete",
        Some(Done::Restored { .. }) => "restore",
        Some(Done::Erased { .. }) | None => "permanent delete",
    };
    format!("{verb} of {}", subjects(&steps.iter().collect::<Vec<_>>()))
}

/// Where a result left its item, if it is still somewhere the user can see.
fn location(step: &Done) -> Option<&str> {
    match step {
        Done::Renamed { to, .. } | Done::Moved { to, .. } | Done::Copied { to, .. } => Some(to),
        Done::Created { path } | Done::Restored { path } => Some(path),
        Done::Recycled { .. } | Done::Erased { .. } => None,
    }
}

impl Journal {
    /// Records what an operation did, so it can be undone. Operations that did nothing are not
    /// recorded; one that erased anything ends the history, since nothing brings that back.
    pub fn record(&self, outcome: &Outcome) {
        let steps: Vec<Done> = outcome.results().cloned().collect();
        if steps.is_empty() {
            return;
        }
        let mut history = self.history.lock();
        history.redo.clear();
        let erased: Vec<&Done> = steps.iter().filter(|step| matches!(step, Done::Erased { .. })).collect();
        if !erased.is_empty() {
            history.undo.clear();
            history.erased = Some(subjects(&erased));
            return;
        }
        history.erased = None;
        let entry = history.entry(describe(&steps), steps);
        history.undo.push(entry);
        if history.undo.len() > LIMIT {
            history.undo.remove(0);
        }
    }

    pub fn state(&self) -> UndoState {
        let history = self.history.lock();
        UndoState {
            undo: history.undo.last().map(|entry| entry.label.clone()),
            redo: history.redo.last().map(|entry| entry.label.clone()),
            erased: history.erased.clone(),
        }
    }

    pub fn undo(&self, options: Options) -> Result<Report, String> {
        self.reverse(Direction::Undo, options)
    }

    pub fn redo(&self, options: Options) -> Result<Report, String> {
        self.reverse(Direction::Redo, options)
    }

    /// Reverses the latest entry on the undo or redo stack and moves what was reversed onto
    /// the other one. Whatever could not be reversed stays where it was.
    fn reverse(&self, direction: Direction, options: Options) -> Result<Report, String> {
        let undoing = direction == Direction::Undo;
        let _busy = self.busy.try_lock().ok_or(if undoing {
            "An undo is already running."
        } else {
            "A redo is already running."
        })?;
        let entry = {
            let history = self.history.lock();
            if let (true, Some(erased)) = (undoing, &history.erased) {
                let them = if erased.ends_with(" items") { "they" } else { "it" };
                let were = if erased.ends_with(" items") { "were" } else { "was" };
                return Err(format!("{erased} {were} deleted permanently, so {them} can't be undone."));
            }
            let stack = if undoing { &history.undo } else { &history.redo };
            stack.last().cloned().ok_or(if undoing { "There's nothing to undo." } else { "There's nothing to redo." })?
        };

        // The steps that can be reversed, each with its position in the entry.
        let reversible: Steps =
            entry.steps.iter().enumerate().filter_map(|(at, step)| Some((at, inverse(step)?))).collect();
        if reversible.is_empty() {
            // Nothing in it can ever be reversed, so it would only block older entries.
            self.drop_entry(direction, entry.id);
            return Err(format!("The {} merged folders, so it can't be undone.", entry.label));
        }
        // Items changed elsewhere since, such as renamed or deleted in another program, are no
        // longer where the history says, and the folders some would go back to may have been
        // removed; they are left out rather than blocking the rest.
        let (present, gone): (Steps, Steps) =
            reversible.into_iter().partition(|(_, op)| source(op).is_none_or(|path| Path::new(path).exists()));
        let (present, homeless): (Steps, Steps) =
            present.into_iter().partition(|(_, op)| destination(op).is_none_or(|folder| Path::new(&folder).is_dir()));
        if present.is_empty() {
            self.drop_entry(direction, entry.id);
            let mut folders: Vec<String> = homeless.iter().filter_map(|(_, op)| destination(op)).collect();
            folders.sort();
            folders.dedup();
            let why = match folders.as_slice() {
                [] => "its items are no longer where they were".to_owned(),
                [folder] => format!("{folder} no longer exists"),
                _ => "the folders its items go back to no longer exist".to_owned(),
            };
            return Err(format!("Couldn't {} the {}: {why}.", direction.verb(), entry.label));
        }
        let (positions, ops): (Vec<usize>, Vec<Op>) = present.into_iter().unzip();
        let outcome = if ops.iter().all(|op| matches!(op, Op::Rename { .. })) {
            let renames = ops
                .into_iter()
                .map(|op| match op {
                    Op::Rename { path, name } => (path, name),
                    _ => unreachable!("checked above"),
                })
                .collect();
            fileops::rename_all(renames, options)?
        } else {
            fileops::run(ops, false, options)?
        };

        let mut reversed = vec![false; entry.steps.len()];
        let mut results = Vec::new();
        for (op, done) in &outcome.done {
            reversed[positions[*op]] = true;
            results.push(done.clone());
        }
        // Items no longer in the Recycle Bin, or no longer anywhere the history knows, can never
        // come back; forget them.
        for &op in &outcome.missing {
            reversed[positions[op]] = true;
        }
        for (at, _) in gone.iter().chain(&homeless) {
            reversed[*at] = true;
        }
        let left: Vec<Done> =
            entry.steps.iter().zip(&reversed).filter(|(_, done)| !**done).map(|(step, _)| step.clone()).collect();
        let reversible_left = left.iter().any(|step| inverse(step).is_some());

        let mut guard = self.history.lock();
        let next = (!results.is_empty()).then(|| guard.entry(entry.label.clone(), results.clone()));
        let history = &mut *guard;
        let (from, to) = if undoing {
            (&mut history.undo, &mut history.redo)
        } else {
            (&mut history.redo, &mut history.undo)
        };
        if let Some(at) = from.iter().position(|e| e.id == entry.id) {
            if reversible_left {
                from[at].steps = left;
            } else {
                from.remove(at);
            }
        }
        if let Some(next) = next {
            to.push(next);
        }
        drop(guard);

        let verb = if undoing { "Undid" } else { "Redid" };
        let mut message = if results.is_empty() {
            format!("Nothing was {}", if undoing { "undone" } else { "redone" })
        } else if reversible_left || !outcome.completed || !gone.is_empty() || !homeless.is_empty() || !outcome.missing.is_empty() {
            format!("{verb} part of the {}", entry.label)
        } else {
            format!("{verb} {}", entry.label)
        };
        let count = |n: usize| if n == 1 { "1 item was".to_owned() } else { format!("{n} items were") };
        let mut notes = Vec::new();
        if !outcome.missing.is_empty() {
            notes.push(format!("{} no longer in the Recycle Bin", count(outcome.missing.len())));
        }
        if !gone.is_empty() {
            notes.push(format!("{} no longer where {} left {}", count(gone.len()), if undoing { "the operation" } else { "the undo" }, if gone.len() == 1 { "it" } else { "them" }));
        }
        match homeless.len() {
            0 => {}
            1 => notes.push("1 item's folder no longer exists".to_owned()),
            n => notes.push(format!("{n} items' folders no longer exist")),
        }
        if !notes.is_empty() {
            message.push_str(&format!(": {}", notes.join(", and ")));
        }

        let placed: Vec<&str> = results.iter().filter_map(location).collect();
        let folder = placed.first().map(|path| parent(path));
        let names = placed
            .iter()
            .filter(|path| Some(parent(path)) == folder)
            .map(|path| file_name(path).to_owned())
            .collect();
        Ok(Report { message, folder, names })
    }

    fn drop_entry(&self, direction: Direction, id: u64) {
        let mut history = self.history.lock();
        let stack = if direction == Direction::Undo { &mut history.undo } else { &mut history.redo };
        stack.retain(|entry| entry.id != id);
    }
}

#[tauri::command]
pub fn undo_state(journal: State<Journal>) -> UndoState {
    journal.state()
}

/// Reverses the latest operation, with the shell's progress and conflict windows.
#[tauri::command(async)]
pub fn undo(window: WebviewWindow, journal: State<Journal>) -> Result<Report, String> {
    journal.undo(crate::interactive(&window))
}

/// Repeats the latest undone operation.
#[tauri::command(async)]
pub fn redo(window: WebviewWindow, journal: State<Journal>) -> Result<Report, String> {
    journal.redo(crate::interactive(&window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileops::tests::{QUIET, sandbox, text};
    use crate::fileops::{Request, Transfer, Window, execute};
    use std::fs;

    fn names_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    #[test]
    fn undoes_and_redoes_a_rename() {
        let dir = sandbox("undo-rename");
        fs::write(dir.join("report.txt"), b"x").unwrap();
        let journal = Journal::default();
        assert_eq!(journal.undo(QUIET).unwrap_err(), "There's nothing to undo.");

        let outcome = execute(Request::Rename { path: text(&dir.join("report.txt")), new_name: "final.txt".into() }, QUIET).unwrap();
        journal.record(&outcome);
        assert_eq!(journal.state(), UndoState { undo: Some("rename of report.txt".into()), redo: None, erased: None });

        let report = journal.undo(QUIET).unwrap();
        assert_eq!(report.message, "Undid rename of report.txt");
        assert_eq!((report.folder.as_deref(), report.names.as_slice()), (Some(text(&dir).as_str()), ["report.txt".to_owned()].as_slice()));
        assert_eq!(names_in(&dir), ["report.txt"]);
        assert_eq!(journal.state(), UndoState { undo: None, redo: Some("rename of report.txt".into()), erased: None });

        assert_eq!(journal.redo(QUIET).unwrap().message, "Redid rename of report.txt");
        assert_eq!(names_in(&dir), ["final.txt"]);
        // And back again: the history keeps alternating.
        journal.undo(QUIET).unwrap();
        assert_eq!(names_in(&dir), ["report.txt"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undoes_and_redoes_a_move() {
        let dir = sandbox("undo-move");
        fs::create_dir_all(dir.join("src/folder")).unwrap();
        fs::create_dir_all(dir.join("dst")).unwrap();
        fs::write(dir.join("src/a.txt"), b"a").unwrap();
        fs::write(dir.join("src/folder/b.txt"), b"b").unwrap();
        let journal = Journal::default();
        let sources = vec![text(&dir.join("src/a.txt")), text(&dir.join("src/folder"))];
        journal.record(&execute(Request::Transfer { sources, destination: text(&dir.join("dst")), kind: Transfer::Move }, QUIET).unwrap());
        assert_eq!(names_in(&dir.join("dst")), ["a.txt", "folder"]);
        assert_eq!(journal.state().undo.as_deref(), Some("move of 2 items"));

        let report = journal.undo(QUIET).unwrap();
        assert_eq!(report.message, "Undid move of 2 items");
        assert_eq!(names_in(&dir.join("src")), ["a.txt", "folder"]);
        assert!(dir.join("src/folder/b.txt").exists());
        assert!(names_in(&dir.join("dst")).is_empty());

        journal.redo(QUIET).unwrap();
        assert_eq!(names_in(&dir.join("dst")), ["a.txt", "folder"]);
        assert!(names_in(&dir.join("src")).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undoes_a_batch_of_renames_that_swap_names() {
        let dir = sandbox("undo-batch");
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let path = |name: &str| text(&dir.join(name));
        let journal = Journal::default();
        // a and b swap, c becomes d.
        let renames = vec![(path("a.txt"), "b.txt".into()), (path("b.txt"), "a.txt".into()), (path("c.txt"), "d.txt".into())];
        journal.record(&fileops::rename_all(renames, QUIET).unwrap());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "b.txt");
        assert_eq!(journal.state().undo.as_deref(), Some("rename of 3 items"));

        journal.undo(QUIET).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), name);
        }
        assert_eq!(names_in(&dir), ["a.txt", "b.txt", "c.txt"]);
        journal.redo(QUIET).unwrap();
        assert_eq!(names_in(&dir), ["a.txt", "b.txt", "d.txt"]);
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "a.txt");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reverses_each_kind_of_result() {
        let when = Window { from: 1, to: 2 };
        let cases = [
            (Done::Created { path: r"C:\t\New folder".into() }, Some(Op::Recycle { path: r"C:\t\New folder".into() })),
            (Done::Restored { path: r"C:\t\a.txt".into() }, Some(Op::Recycle { path: r"C:\t\a.txt".into() })),
            (
                Done::Copied { from: r"C:\s\a.txt".into(), to: r"C:\t\a - Copy.txt".into(), merged: false },
                Some(Op::Recycle { path: r"C:\t\a - Copy.txt".into() }),
            ),
            (Done::Recycled { path: r"C:\t\a.txt".into(), when }, Some(Op::Restore { path: r"C:\t\a.txt".into(), deleted: when })),
            (
                Done::Moved { from: r"C:\s\a.txt".into(), to: r"C:\t\a (2).txt".into(), merged: false },
                Some(Op::Move { source: r"C:\t\a (2).txt".into(), folder: r"C:\s".into(), name: Some("a.txt".into()) }),
            ),
            (Done::Moved { from: r"C:\s\d".into(), to: r"C:\t\d".into(), merged: true }, None),
            (Done::Erased { path: r"C:\t\a.txt".into() }, None),
        ];
        for (step, expected) in cases {
            assert_eq!(inverse(&step), expected, "{step:?}");
        }
        assert_eq!(describe(&[Done::Created { path: r"C:\t\New folder".into() }]), "creation of New folder");
    }

    #[test]
    fn a_permanent_deletion_cannot_be_undone() {
        let dir = sandbox("undo-erase");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join("gone.txt"), b"x").unwrap();
        let journal = Journal::default();
        journal.record(&execute(Request::Rename { path: text(&dir.join("a.txt")), new_name: "b.txt".into() }, QUIET).unwrap());
        journal.record(&execute(Request::Delete { paths: vec![text(&dir.join("gone.txt"))], permanent: true }, QUIET).unwrap());
        assert_eq!(journal.state(), UndoState { undo: None, redo: None, erased: Some("gone.txt".into()) });
        assert_eq!(journal.undo(QUIET).unwrap_err(), "gone.txt was deleted permanently, so it can't be undone.");
        assert_eq!(names_in(&dir), ["b.txt"], "nothing older is undone either");

        // A later operation can be undone again.
        journal.record(&execute(Request::Rename { path: text(&dir.join("b.txt")), new_name: "c.txt".into() }, QUIET).unwrap());
        assert_eq!(journal.state().erased, None);
        journal.undo(QUIET).unwrap();
        assert_eq!(names_in(&dir), ["b.txt"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn items_changed_elsewhere_since_are_left_out() {
        let dir = sandbox("undo-gone");
        for name in ["a.txt", "c.txt", "x.txt", "y.txt"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let path = |name: &str| text(&dir.join(name));
        let journal = Journal::default();
        journal.record(&execute(Request::Rename { path: path("a.txt"), new_name: "b.txt".into() }, QUIET).unwrap());
        journal.record(&execute(Request::Rename { path: path("c.txt"), new_name: "d.txt".into() }, QUIET).unwrap());
        // Another program deletes the renamed file.
        fs::remove_file(dir.join("d.txt")).unwrap();
        assert_eq!(
            journal.undo(QUIET).unwrap_err(),
            "Couldn't undo the rename of c.txt: its items are no longer where they were."
        );
        assert_eq!(journal.state().undo.as_deref(), Some("rename of a.txt"), "older entries stay reachable");
        journal.undo(QUIET).unwrap();
        assert!(dir.join("a.txt").exists());

        // Of a batch, whatever is still there is undone.
        journal.record(&fileops::rename_all(vec![(path("x.txt"), "x2.txt".into()), (path("y.txt"), "y2.txt".into())], QUIET).unwrap());
        fs::rename(dir.join("y2.txt"), dir.join("elsewhere.txt")).unwrap();
        let report = journal.undo(QUIET).unwrap();
        assert_eq!(report.message, "Undid part of the rename of 2 items: 1 item was no longer where the operation left it");
        assert_eq!(names_in(&dir), ["a.txt", "elsewhere.txt", "x.txt"]);
        assert_eq!(journal.state().undo, None, "the missing item is forgotten");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_move_whose_folder_is_gone_does_not_block_older_history() {
        let dir = sandbox("undo-folder-gone");
        for folder in ["Drafts", "Notes", "Final"] {
            fs::create_dir_all(dir.join(folder)).unwrap();
        }
        fs::write(dir.join("Drafts/a.txt"), b"a").unwrap();
        fs::write(dir.join("Notes/b.txt"), b"b").unwrap();
        fs::write(dir.join("c.txt"), b"c").unwrap();
        let path = |name: &str| text(&dir.join(name));
        let journal = Journal::default();
        journal.record(&execute(Request::Rename { path: path("c.txt"), new_name: "d.txt".into() }, QUIET).unwrap());
        let move_ = |sources: Vec<String>| Request::Transfer { sources, destination: path("Final"), kind: Transfer::Move };
        journal.record(&execute(move_(vec![path("Drafts/a.txt")]), QUIET).unwrap());

        // Another program removes the folder the file came from.
        fs::remove_dir(dir.join("Drafts")).unwrap();
        assert_eq!(journal.undo(QUIET).unwrap_err(), format!("Couldn't undo the move of a.txt: {} no longer exists.", path("Drafts")));
        assert_eq!(journal.state().undo.as_deref(), Some("rename of c.txt"), "older entries stay reachable");
        journal.undo(QUIET).unwrap();
        assert!(dir.join("c.txt").exists());

        // Of a batch, whatever has a folder to go back to goes back.
        fs::create_dir_all(dir.join("Drafts")).unwrap();
        fs::rename(dir.join("Final/a.txt"), dir.join("Drafts/a.txt")).unwrap();
        journal.record(&execute(move_(vec![path("Drafts/a.txt"), path("Notes/b.txt")]), QUIET).unwrap());
        fs::remove_dir(dir.join("Drafts")).unwrap();
        let report = journal.undo(QUIET).unwrap();
        assert_eq!(report.message, "Undid part of the move of 2 items: 1 item's folder no longer exists");
        assert_eq!(names_in(&dir.join("Notes")), ["b.txt"]);
        assert_eq!(names_in(&dir.join("Final")), ["a.txt"], "the other item stays where it is");
        assert_eq!(journal.state().undo, None, "the item without a folder is forgotten");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_merged_move_is_left_alone() {
        let dir = sandbox("undo-merge");
        fs::create_dir_all(dir.join("src/shared")).unwrap();
        fs::create_dir_all(dir.join("dst/shared")).unwrap();
        fs::write(dir.join("src/shared/new.txt"), b"n").unwrap();
        fs::write(dir.join("dst/shared/old.txt"), b"o").unwrap();
        let journal = Journal::default();
        let request = Request::Transfer { sources: vec![text(&dir.join("src/shared"))], destination: text(&dir.join("dst")), kind: Transfer::Move };
        journal.record(&execute(request, QUIET).unwrap());
        assert_eq!(names_in(&dir.join("dst/shared")), ["new.txt", "old.txt"]);
        assert_eq!(journal.undo(QUIET).unwrap_err(), "The move of shared merged folders, so it can't be undone.");
        assert_eq!(names_in(&dir.join("dst/shared")), ["new.txt", "old.txt"], "nothing moved");
        assert_eq!(journal.state().undo, None, "the entry is dropped so older ones stay reachable");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Sends two tiny files named kestrel-test-* to the real Recycle Bin and brings both back
    /// through the undo and redo being tested, so the Recycle Bin ends as it began.
    #[test]
    fn restores_recycled_items_from_the_recycle_bin() {
        let dir = sandbox("undo-recycle");
        let pid = std::process::id();
        let deleted = dir.join(format!("kestrel-test-{pid}-deleted.txt"));
        let copied = dir.join(format!("kestrel-test-{pid}-copied.txt"));
        fs::write(&deleted, b"deleted").unwrap();
        fs::write(&copied, b"copied").unwrap();
        let journal = Journal::default();

        // A deletion to the Recycle Bin, undone by restoring that very item.
        let outcome = execute(Request::Delete { paths: vec![text(&deleted)], permanent: false }, QUIET).unwrap();
        assert!(matches!(outcome.results().collect::<Vec<_>>().as_slice(), [Done::Recycled { .. }]), "{outcome:?}");
        assert!(!deleted.exists());
        journal.record(&outcome);
        let report = journal.undo(QUIET).unwrap();
        let name = format!("kestrel-test-{pid}-deleted.txt");
        assert_eq!(report.message, format!("Undid delete of {name}"));
        assert_eq!(report.names, [name]);
        assert_eq!(fs::read(&deleted).unwrap(), b"deleted");

        // A copy in place, undone by recycling the duplicate and redone by restoring it.
        let request = Request::Transfer { sources: vec![text(&copied)], destination: text(&dir), kind: Transfer::Copy };
        journal.record(&execute(request, QUIET).unwrap());
        let duplicate = dir.join(format!("kestrel-test-{pid}-copied - Copy.txt"));
        assert!(duplicate.exists());
        journal.undo(QUIET).unwrap();
        assert!(!duplicate.exists());
        assert_eq!(journal.state().redo.as_deref(), Some(format!("copy of kestrel-test-{pid}-copied.txt").as_str()));
        let report = journal.redo(QUIET).unwrap();
        assert_eq!(report.names, [format!("kestrel-test-{pid}-copied - Copy.txt")]);
        assert_eq!(fs::read(&duplicate).unwrap(), b"copied");

        // Nothing of ours is left in the Recycle Bin.
        let left = std::thread::spawn(move || {
            // SAFETY: COM is initialised on this thread for the call and uninitialised after.
            unsafe {
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED).ok().unwrap();
                let left = crate::recycle::entries()
                    .unwrap()
                    .into_iter()
                    .filter(|entry| entry.original.contains(&format!("kestrel-test-{pid}-")))
                    .count();
                windows::Win32::System::Com::CoUninitialize();
                left
            }
        })
        .join()
        .unwrap();
        assert_eq!(left, 0);
        // Nor any of the bin's own records of them.
        assert_eq!(crate::recycle::records_mentioning(&format!("kestrel-test-{pid}-")), Vec::<String>::new());
        fs::remove_dir_all(&dir).unwrap();
    }
}
