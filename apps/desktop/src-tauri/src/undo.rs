//! Undo and redo. Every file operation Kestrel performs is recorded with its exact results (see
//! `fileops::Done`), so it can be reversed precisely: a rename by renaming back, a move by
//! moving back, a copy or a new folder by sending what was created to the Recycle Bin (never
//! erasing it), and a deletion to the Recycle Bin by restoring exactly those items. Each undo
//! or redo is one shell operation, with the shell's usual progress and conflict windows.
//!
//! Permanent deletions can't be undone. After one, the history before it is dropped, because
//! undoing older operations around the gap could act on the wrong items, and the UI says why.
//!
//! The history survives a restart: every change to it is written to the app's database
//! (`kestrel_db::undo`) as it happens, and the next start brings it back, leaving out the
//! operations none of whose items are still where they were left.
//!
//! Because it lasts for days, an item is known by more than its path: each result is kept
//! with a [`Mark`] of what the operation left (its file id, and what it held if an undo would
//! send it to the Recycle Bin). An undo leaves alone whatever no longer matches: another item
//! saved under the same name, a folder other programs have since put files in, a copy edited
//! since.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::undo::{Stack, StoredEntry, UndoChange};
use kestrel_index::{Meta, attr, walk};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{State, Window};

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
    /// What each step left, as it was just after; `None` where nothing could be read (or for
    /// operations stored before marks were kept).
    marks: Vec<Option<Mark>>,
}

impl Entry {
    fn mark(&self, step: usize) -> Option<&Mark> {
        self.marks.get(step).and_then(Option::as_ref)
    }
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
    fn entry(&mut self, label: String, steps: Vec<Done>, marks: Vec<Option<Mark>>) -> Entry {
        self.next += 1;
        Entry { id: self.next, label, steps, marks }
    }
}

/// The undo and redo history, shared by every window and tab.
#[derive(Default)]
pub struct Journal {
    history: Mutex<History>,
    /// Held while an undo or redo runs, so two never overlap.
    busy: Mutex<()>,
    /// Where the history is kept between runs; without one it lasts as long as the app.
    store: Option<Store>,
}

/// The history as kept in the database.
struct Store {
    db: Arc<Db>,
    /// What the database holds: each operation's stack and number of steps (which an undo
    /// that leaves some steps behind reduces), and the erased note. Only differences are
    /// written, so a large batch rename is stored once, not again with every later operation.
    saved: Mutex<Saved>,
}

#[derive(Default)]
struct Saved {
    entries: HashMap<u64, (Stack, usize)>,
    erased: Option<String>,
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

// ---------- telling what an operation left from whatever is there later ----------

/// What an item an operation left was like just after, so that reversing the operation, maybe
/// days and restarts later, acts on that item only. A path can't tell: by then another item
/// may have been saved under the name, or other programs may have put their work in a folder
/// the operation made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// The file id: the same item, not another one at the same path.
    id: u64,
    /// What it held. Kept only for items an undo or redo would send to the Recycle Bin, which
    /// must still hold exactly that; renaming or moving an item back is harmless whatever it
    /// holds now.
    content: Option<Content>,
}

/// A file's size and last write time; for a folder, the items at any depth inside it, the
/// bytes of its files and the newest file's last write time. A folder's own time is left out:
/// it changes as items come and go, even when they then go again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Content {
    entries: u64,
    bytes: u64,
    newest: i64,
}

/// What `path`, read as `meta`, holds now.
fn content_of(path: &str, meta: &Meta) -> Content {
    if meta.attrs & attr::DIRECTORY == 0 {
        return Content { entries: 0, bytes: meta.size, newest: meta.mtime };
    }
    let mut content = Content::default();
    let mut folders = vec![path.trim_end_matches('\\').to_owned()];
    while let Some(folder) = folders.pop() {
        // A folder that can't be read counts as empty, the same way each time it is looked at.
        let Ok(items) = walk::list(&folder) else { continue };
        for item in items {
            content.entries += 1;
            if item.meta.attrs & attr::DIRECTORY == 0 {
                content.bytes += item.meta.size;
                content.newest = content.newest.max(item.meta.mtime);
            } else if !item.alias {
                folders.push(format!(r"{folder}\{}", item.name));
            }
        }
    }
    content
}

/// Whether reversing `step` sends what it left to the Recycle Bin.
fn recycles(step: &Done) -> bool {
    matches!(inverse(step), Some(Op::Recycle { .. }))
}

/// The marks of what each step left, read from the disk now, just after the operation.
fn marks(steps: &[Done]) -> Vec<Option<Mark>> {
    steps
        .iter()
        .map(|step| {
            let path = location(step)?;
            let meta = walk::info(path)?;
            Some(Mark { id: meta.file_id, content: recycles(step).then(|| content_of(path, &meta)) })
        })
        .collect()
}

/// How the item an op would act on compares with what the operation left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// The item the operation left, as it was left (or, for a restore, nothing to check here:
    /// the Recycle Bin is asked).
    Same,
    /// Nothing at its path, or another item.
    Gone,
    /// The same item, holding something else now, which sending it to the Recycle Bin would
    /// take with it. Also an item to recycle that has no mark, which can't be told apart.
    Changed,
}

/// Compares the item `op` acts on with `mark`, what the operation left. Without a mark only the
/// path can be checked, which is enough to rename or move an item back but not to recycle it.
fn found(op: &Op, mark: Option<&Mark>) -> Found {
    let Some(path) = source(op) else { return Found::Same };
    let Some(meta) = walk::info(path) else { return Found::Gone };
    let recycling = matches!(op, Op::Recycle { .. });
    match mark {
        Some(mark) if mark.id != meta.file_id => Found::Gone,
        Some(Mark { content: Some(content), .. }) if recycling && content_of(path, &meta) != *content => Found::Changed,
        Some(Mark { content: None, .. }) | None if recycling => Found::Changed,
        _ => Found::Same,
    }
}

/// Whether the folder `op` puts its item in is there.
fn has_home(op: &Op) -> bool {
    destination(op).is_none_or(|folder| Path::new(&folder).is_dir())
}

impl Journal {
    /// Records what an operation did, so it can be undone. Operations that did nothing are not
    /// recorded; one that erased anything ends the history, since nothing brings that back.
    pub fn record(&self, outcome: &Outcome) {
        self.record_labelled(outcome, None);
    }

    /// [`Journal::record`], under a name of the caller's, such as "extraction of photos.zip",
    /// for operations Kestrel carries out itself rather than through the shell.
    pub fn record_as(&self, outcome: &Outcome, label: String) {
        self.record_labelled(outcome, Some(label));
    }

    fn record_labelled(&self, outcome: &Outcome, label: Option<String>) {
        let steps: Vec<Done> = outcome.results().cloned().collect();
        if steps.is_empty() {
            return;
        }
        let erased: Vec<&Done> = steps.iter().filter(|step| matches!(step, Done::Erased { .. })).collect();
        if !erased.is_empty() {
            let erased = subjects(&erased);
            let mut history = self.history.lock();
            history.redo.clear();
            history.undo.clear();
            history.erased = Some(erased);
            self.persist(&history);
            return;
        }
        // Read before taking the history: a copied folder is walked.
        let marks = marks(&steps);
        let mut history = self.history.lock();
        history.redo.clear();
        history.erased = None;
        let label = label.unwrap_or_else(|| describe(&steps));
        let entry = history.entry(label, steps, marks);
        history.undo.push(entry);
        if history.undo.len() > LIMIT {
            history.undo.remove(0);
        }
        self.persist(&history);
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
        // Items changed elsewhere since, such as renamed, deleted or replaced in another program,
        // are no longer what the history says; items an undo would recycle that hold something
        // else now (a folder others have saved files in, a copy edited since) are not the
        // operation's alone any more; and the folders some would go back to may have been
        // removed. They are all left out rather than blocking the rest.
        let (mut present, mut gone, mut changed, mut homeless) = (Steps::new(), Steps::new(), Steps::new(), Steps::new());
        for (at, op) in reversible {
            match found(&op, entry.mark(at)) {
                Found::Gone => gone.push((at, op)),
                Found::Changed => changed.push((at, op)),
                Found::Same if !has_home(&op) => homeless.push((at, op)),
                Found::Same => present.push((at, op)),
            }
        }
        if present.is_empty() {
            self.drop_entry(direction, entry.id);
            let mut folders: Vec<String> = homeless.iter().filter_map(|(_, op)| destination(op)).collect();
            folders.sort();
            folders.dedup();
            let why = match folders.as_slice() {
                [] if gone.is_empty() && changed.len() == 1 => "it has changed since".to_owned(),
                [] if gone.is_empty() => "its items have changed since".to_owned(),
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
        for (at, _) in gone.iter().chain(&changed).chain(&homeless) {
            reversed[*at] = true;
        }
        let (left, left_marks): (Vec<Done>, Vec<Option<Mark>>) = (0..entry.steps.len())
            .filter(|&at| !reversed[at])
            .map(|at| (entry.steps[at].clone(), entry.mark(at).copied()))
            .unzip();
        let reversible_left = left.iter().any(|step| inverse(step).is_some());
        // What the undo or redo left, read before taking the history.
        let next_marks = marks(&results);

        let mut guard = self.history.lock();
        let next = (!results.is_empty()).then(|| guard.entry(entry.label.clone(), results.clone(), next_marks));
        let history = &mut *guard;
        let (from, to) = if undoing {
            (&mut history.undo, &mut history.redo)
        } else {
            (&mut history.redo, &mut history.undo)
        };
        if let Some(at) = from.iter().position(|e| e.id == entry.id) {
            if reversible_left {
                from[at].steps = left;
                from[at].marks = left_marks;
            } else {
                from.remove(at);
            }
        }
        if let Some(next) = next {
            to.push(next);
        }
        self.persist(&guard);
        drop(guard);

        let verb = if undoing { "Undid" } else { "Redid" };
        let mut message = if results.is_empty() {
            format!("Nothing was {}", if undoing { "undone" } else { "redone" })
        } else if reversible_left
            || !outcome.completed
            || !gone.is_empty()
            || !changed.is_empty()
            || !homeless.is_empty()
            || !outcome.missing.is_empty()
        {
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
        if !changed.is_empty() {
            notes.push(format!("{} changed since, so left as {}", count(changed.len()), if changed.len() == 1 { "it was" } else { "they were" }));
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
        self.persist(&history);
    }
}

// ---------- keeping the history between runs ----------

impl Journal {
    /// A journal kept in `db`. Call [`Journal::restore`] once to bring back the history of
    /// earlier runs; operations recorded before that are numbered after every stored one.
    pub fn with_store(db: Arc<Db>) -> Self {
        let mut journal = Self::default();
        journal.history.get_mut().next = db.undo_last_id().unwrap_or_else(|error| {
            eprintln!("could not read the undo history: {error}");
            0
        });
        journal.store = Some(Store { db, saved: Mutex::new(Saved::default()) });
        journal
    }

    /// Writes whatever changed in `history` since the last write. A failure is reported and
    /// tried again with the next change; it never stops an operation.
    fn persist(&self, history: &History) {
        let Some(store) = &self.store else { return };
        let mut saved = store.saved.lock();
        let mut now = HashMap::new();
        let mut written = Vec::new();
        for (stack, entries) in [(Stack::Undo, &history.undo), (Stack::Redo, &history.redo)] {
            for entry in entries {
                let shape = (stack, entry.steps.len());
                now.insert(entry.id, shape);
                if saved.entries.get(&entry.id) == Some(&shape) {
                    continue;
                }
                match (serde_json::to_string(&entry.steps), serde_json::to_string(&entry.marks)) {
                    (Ok(steps), Ok(marks)) => {
                        written.push(StoredEntry { id: entry.id, stack, label: entry.label.clone(), steps, marks: Some(marks) })
                    }
                    (Err(error), _) | (_, Err(error)) => eprintln!("could not save the undo history: {error}"),
                }
            }
        }
        let removed: Vec<u64> = saved.entries.keys().filter(|id| !now.contains_key(id)).copied().collect();
        if written.is_empty() && removed.is_empty() && saved.erased == history.erased {
            return;
        }
        let change = UndoChange { removed: &removed, written: &written, erased: history.erased.as_deref() };
        match store.db.update_undo(&change, LIMIT) {
            Ok(()) => {
                saved.entries = now;
                saved.erased = history.erased.clone();
            }
            Err(error) => eprintln!("could not save the undo history: {error}"),
        }
    }

    /// Brings back the history kept from earlier runs. An operation none of whose items are
    /// still as it left them (renamed, deleted, replaced or filled since, by Kestrel or anything
    /// else, or gone from the Recycle Bin) can never be reversed, and is left out without a
    /// word; one with some of its items still there stays, and its undo reverses those and says
    /// what was missing, as it would have before the restart. Called once, at start, on a
    /// thread of its own.
    pub fn restore(&self) {
        self.restore_checking(still_reversible);
    }

    /// [`Journal::restore`], with `check` saying which of the operations brought back can still
    /// be reversed.
    ///
    /// The history is brought back first, at once, and checked afterwards without holding it:
    /// looking at the disk and in the Recycle Bin can take seconds (a network share that is
    /// away, a large bin), and meanwhile the menus ask what Undo would do and every file
    /// operation records itself. An operation that turns out not to match is on offer only for
    /// that moment, and an undo then checks its items itself anyway.
    fn restore_checking(&self, check: impl FnOnce(&[(&[Done], &[Option<Mark>])]) -> Vec<bool>) {
        let Some(store) = &self.store else { return };
        let restored: Vec<(u64, Vec<Done>, Vec<Option<Mark>>)> = {
            let mut history = self.history.lock();
            let (stored, erased) = match (store.db.undo_entries(), store.db.undo_erased()) {
                (Ok(stored), Ok(erased)) => (stored, erased),
                (Err(error), _) | (_, Err(error)) => return eprintln!("could not read the undo history: {error}"),
            };
            let parsed: Vec<(StoredEntry, Option<Vec<Done>>)> = stored
                .into_iter()
                .map(|entry| {
                    let steps = serde_json::from_str(&entry.steps).ok();
                    (entry, steps)
                })
                .collect();
            {
                // What the database holds now; whatever is not kept is removed from it below.
                let mut saved = store.saved.lock();
                for (entry, steps) in &parsed {
                    saved.entries.insert(entry.id, (entry.stack, steps.as_ref().map_or(usize::MAX, Vec::len)));
                }
                saved.erased = erased.clone();
            }
            let (mut undo, mut redo) = (Vec::new(), Vec::new());
            for (stored, steps) in parsed {
                let Some(steps) = steps else { continue };
                // Operations stored before marks were kept, or with marks that don't fit, are
                // known by their paths only.
                let marks = stored
                    .marks
                    .as_deref()
                    .and_then(|marks| serde_json::from_str::<Vec<Option<Mark>>>(marks).ok())
                    .filter(|marks| marks.len() == steps.len())
                    .unwrap_or_else(|| vec![None; steps.len()]);
                let entry = Entry { id: stored.id, label: stored.label, steps, marks };
                if stored.stack == Stack::Undo { undo.push(entry) } else { redo.push(entry) }
            }
            let ids: HashSet<u64> = undo.iter().chain(&redo).map(|entry| entry.id).collect();
            // Anything recorded meanwhile is newer: it goes on top, and it ended the redo history.
            let fresh = history.undo.is_empty() && history.redo.is_empty() && history.erased.is_none();
            undo.append(&mut history.undo);
            let excess = undo.len().saturating_sub(LIMIT);
            undo.drain(..excess);
            history.undo = undo;
            if fresh {
                history.redo = redo;
                history.erased = erased;
            }
            history.next = history.next.max(history.undo.iter().chain(&history.redo).map(|entry| entry.id).max().unwrap_or(0));
            self.persist(&history);
            history
                .undo
                .iter()
                .chain(&history.redo)
                .filter(|entry| ids.contains(&entry.id))
                .map(|entry| (entry.id, entry.steps.clone(), entry.marks.clone()))
                .collect()
        };
        if restored.is_empty() {
            return;
        }

        let operations: Vec<(&[Done], &[Option<Mark>])> =
            restored.iter().map(|(_, steps, marks)| (steps.as_slice(), marks.as_slice())).collect();
        let valid = check(&operations);
        let stale: HashSet<u64> = restored.iter().zip(valid).filter(|(_, keep)| !keep).map(|((id, ..), _)| *id).collect();
        if stale.is_empty() {
            return;
        }
        let mut history = self.history.lock();
        history.undo.retain(|entry| !stale.contains(&entry.id));
        history.redo.retain(|entry| !stale.contains(&entry.id));
        self.persist(&history);
    }
}

/// For each operation, given by its steps and their marks, whether reversing it would still
/// find any of its items as the operation left them, by the checks [`Journal::reverse`] makes.
fn still_reversible(operations: &[(&[Done], &[Option<Mark>])]) -> Vec<bool> {
    let ops: Vec<Vec<(Op, Option<&Mark>)>> = operations
        .iter()
        .map(|(steps, marks)| {
            steps
                .iter()
                .enumerate()
                .filter_map(|(at, step)| Some((inverse(step)?, marks.get(at).and_then(Option::as_ref))))
                .collect()
        })
        .collect();
    // Deleted items are looked for in the Recycle Bin all at once.
    let wanted: Vec<(String, fileops::Window)> = ops
        .iter()
        .flatten()
        .filter_map(|(op, _)| match op {
            Op::Restore { path, deleted } => Some((path.clone(), *deleted)),
            _ => None,
        })
        .collect();
    let mut in_bin = in_recycle_bin(&wanted).into_iter();
    let in_place = |op: &Op, mark: Option<&Mark>| found(op, mark) == Found::Same && has_home(op);
    ops.iter()
        .map(|ops| {
            // Every Restore takes its answer in turn, so all of them are asked, not just the first.
            ops.iter().fold(false, |any, (op, mark)| {
                let in_bin = !matches!(op, Op::Restore { .. }) || in_bin.next().unwrap_or(true);
                in_place(op, *mark) && in_bin || any
            })
        })
        .collect()
}

/// Whether each item deleted from a path within a window of time is still in the Recycle Bin.
/// If the bin can't be read, every item counts as there: the undo itself will say otherwise.
fn in_recycle_bin(wanted: &[(String, fileops::Window)]) -> Vec<bool> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    if wanted.is_empty() {
        return Vec::new();
    }
    let all_there = || vec![true; wanted.len()];
    std::thread::spawn({
        let wanted = wanted.to_vec();
        move || {
            // SAFETY: COM is initialised on this thread for the search and uninitialised after
            // the last interface pointer from it is dropped.
            unsafe {
                if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
                    return None;
                }
                let found = crate::recycle::find(&wanted).ok().map(|items| items.iter().map(Option::is_some).collect());
                CoUninitialize();
                found
            }
        }
    })
    .join()
    .ok()
    .flatten()
    .unwrap_or_else(all_there)
}

/// What Undo and Redo would do, for the context menu. Off the window's thread, like every
/// command that takes a lock file operations also take.
#[tauri::command(async)]
pub fn undo_state(journal: State<Journal>) -> UndoState {
    journal.state()
}

/// Reverses the latest operation, with the shell's progress and conflict windows.
#[tauri::command(async)]
pub fn undo(window: Window, journal: State<Journal>) -> Result<Report, String> {
    journal.undo(crate::interactive(&window))
}

/// Repeats the latest undone operation.
#[tauri::command(async)]
pub fn redo(window: Window, journal: State<Journal>) -> Result<Report, String> {
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

    /// A journal kept in a database file of its own in `dir`, as the app keeps one in its data
    /// folder, brought back from whatever that file already holds.
    fn stored_journal(dir: &std::path::Path) -> (Journal, Arc<Db>) {
        let db = Arc::new(Db::open(&dir.join("kestrel.db")).unwrap());
        let journal = Journal::with_store(db.clone());
        journal.restore();
        (journal, db)
    }

    fn rename(path: &std::path::Path, name: &str) -> Outcome {
        execute(Request::Rename { path: text(path), new_name: name.into() }, QUIET).unwrap()
    }

    #[test]
    fn the_history_survives_a_restart_without_what_no_longer_matches_the_disk() {
        let dir = sandbox("undo-restart");
        let files = dir.join("files");
        fs::create_dir_all(&files).unwrap();
        for name in ["one.txt", "two.txt", "three.txt"] {
            fs::write(files.join(name), name).unwrap();
        }
        {
            let (journal, _) = stored_journal(&dir);
            journal.record(&rename(&files.join("one.txt"), "uno.txt"));
            journal.record(&rename(&files.join("two.txt"), "dos.txt"));
            journal.record(&rename(&files.join("three.txt"), "tres.txt"));
            journal.undo(QUIET).unwrap();
            assert_eq!(names_in(&files), ["dos.txt", "three.txt", "uno.txt"]);
        }
        // While the app is closed, another program deletes the second renamed file.
        fs::remove_file(files.join("dos.txt")).unwrap();

        let (journal, db) = stored_journal(&dir);
        let state = journal.state();
        assert_eq!(state, UndoState { undo: Some("rename of one.txt".into()), redo: Some("rename of three.txt".into()), erased: None });
        assert_eq!(db.undo_entries().unwrap().len(), 2, "the operation that can't be undone any more is gone for good");
        journal.redo(QUIET).unwrap();
        assert_eq!(names_in(&files), ["tres.txt", "uno.txt"]);
        journal.undo(QUIET).unwrap();
        assert_eq!(journal.undo(QUIET).unwrap().message, "Undid rename of one.txt");
        assert_eq!(names_in(&files), ["one.txt", "three.txt"]);
        assert_eq!(journal.state().undo, None);
        drop((journal, db));
        // And again, after one more restart: the stacks are as the last run left them.
        let (journal, _) = stored_journal(&dir);
        assert_eq!(journal.state().redo.as_deref(), Some("rename of one.txt"));
        drop(journal);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_stored_history_keeps_the_newest_hundred_and_drops_what_is_gone() {
        let dir = sandbox("undo-bounded");
        let gone = text(&dir.join("gone"));
        {
            let (journal, db) = stored_journal(&dir);
            for n in 0..120 {
                let step = Done::Renamed { from: format!(r"{gone}\{n}.txt"), to: format!(r"{gone}\{n}-renamed.txt") };
                journal.record(&Outcome { completed: true, done: vec![(0, step)], missing: Vec::new() });
            }
            let stored = db.undo_entries().unwrap();
            assert_eq!(stored.len(), LIMIT);
            assert_eq!(stored.last().unwrap().label, "rename of 119.txt");
            assert_eq!(stored.first().unwrap().label, "rename of 20.txt");
        }
        // None of those files exists, so none of the operations comes back.
        let (journal, db) = stored_journal(&dir);
        assert_eq!(journal.state(), UndoState::default());
        assert!(db.undo_entries().unwrap().is_empty());
        // New operations are numbered after the ones that were stored, and kept.
        fs::write(dir.join("new.txt"), b"x").unwrap();
        journal.record(&rename(&dir.join("new.txt"), "newer.txt"));
        assert!(db.undo_entries().unwrap()[0].id > 120);
        drop((journal, db));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_permanent_deletion_still_blocks_undo_after_a_restart() {
        let dir = sandbox("undo-restart-erase");
        fs::write(dir.join("gone.txt"), b"x").unwrap();
        {
            let (journal, _) = stored_journal(&dir);
            journal.record(&execute(Request::Delete { paths: vec![text(&dir.join("gone.txt"))], permanent: true }, QUIET).unwrap());
        }
        let (journal, _) = stored_journal(&dir);
        assert_eq!(journal.state().erased.as_deref(), Some("gone.txt"));
        assert_eq!(journal.undo(QUIET).unwrap_err(), "gone.txt was deleted permanently, so it can't be undone.");
        drop(journal);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Sends one tiny file named kestrel-test-* to the real Recycle Bin and, after a restart,
    /// brings it back through the restored history, so the Recycle Bin ends as it began.
    #[test]
    fn a_deletion_is_undone_from_the_recycle_bin_after_a_restart() {
        let dir = sandbox("undo-restart-recycle");
        let pid = std::process::id();
        let name = format!("kestrel-test-{pid}-restart.txt");
        fs::write(dir.join(&name), b"deleted").unwrap();
        {
            let (journal, _) = stored_journal(&dir.join("..").join(format!("kestrel-fileops-undo-restart-recycle-db-{pid}")));
            journal.record(&execute(Request::Delete { paths: vec![text(&dir.join(&name))], permanent: false }, QUIET).unwrap());
            assert!(!dir.join(&name).exists());
        }
        let store = dir.join("..").join(format!("kestrel-fileops-undo-restart-recycle-db-{pid}"));
        let (journal, _) = stored_journal(&store);
        assert_eq!(journal.state().undo, Some(format!("delete of {name}")), "found in the Recycle Bin, so kept");
        assert_eq!(journal.undo(QUIET).unwrap().message, format!("Undid delete of {name}"));
        assert_eq!(fs::read(dir.join(&name)).unwrap(), b"deleted");
        assert_eq!(crate::recycle::records_mentioning(&format!("kestrel-test-{pid}-restart")), Vec::<String>::new());
        drop(journal);
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&store).unwrap();
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

    /// Restoring looks at the disk and in the Recycle Bin, which can take seconds (a share
    /// that is away, a large bin). The history used to stay locked all that time: the menus'
    /// question froze the window, and every file operation hung recording itself.
    #[test]
    fn restoring_never_holds_up_the_menus_or_new_operations() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = sandbox("undo-restore-unlocked");
        let files = dir.join("files");
        fs::create_dir_all(&files).unwrap();
        for name in ["keep.txt", "gone.txt", "new.txt"] {
            fs::write(files.join(name), name).unwrap();
        }
        {
            let (journal, _) = stored_journal(&dir);
            journal.record(&rename(&files.join("keep.txt"), "kept.txt"));
            journal.record(&rename(&files.join("gone.txt"), "went.txt"));
        }
        // While the app is closed, another program deletes the second renamed file.
        fs::remove_file(files.join("went.txt")).unwrap();

        let db = Arc::new(Db::open(&dir.join("kestrel.db")).unwrap());
        let journal = Arc::new(Journal::with_store(db));
        let (checking, check_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let restoring = {
            let journal = journal.clone();
            std::thread::spawn(move || {
                journal.restore_checking(move |operations| {
                    checking.send(()).unwrap();
                    released.recv().unwrap();
                    still_reversible(operations)
                })
            })
        };
        check_started.recv().unwrap();
        // The check is under way: the menus and a new operation must not wait for it.
        let (answered, answer) = mpsc::channel();
        {
            let (journal, files) = (journal.clone(), files.clone());
            std::thread::spawn(move || {
                let state = journal.state();
                journal.record(&rename(&files.join("new.txt"), "newer.txt"));
                answered.send(state).unwrap();
            });
        }
        let state = answer.recv_timeout(Duration::from_secs(10));
        release.send(()).unwrap();
        restoring.join().unwrap();
        let state = state.expect("the history was free while the restore looked at the disk");
        assert_eq!(state.undo.as_deref(), Some("rename of gone.txt"), "the stored history is on offer at once");

        // Once checked, the operation whose file is gone is dropped; the new one is on top.
        assert_eq!(journal.state().undo.as_deref(), Some("rename of new.txt"));
        journal.undo(QUIET).unwrap();
        assert_eq!(journal.state().undo.as_deref(), Some("rename of keep.txt"));
        drop(journal);
        // The database agrees, after one more restart.
        let (journal, db) = stored_journal(&dir);
        let ids: Vec<u64> = db.undo_entries().unwrap().iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(journal.state().undo.as_deref(), Some("rename of keep.txt"));
        assert_eq!(journal.state().redo.as_deref(), Some("rename of new.txt"));
        drop((journal, db));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The history lasts for days. Matching items by path alone, an undo after a restart could
    /// rename an unrelated file saved under the same name, or send a folder others had filled
    /// since to the Recycle Bin. Nothing here is recycled: each undo is refused.
    #[test]
    fn an_undo_never_acts_on_what_the_operation_did_not_leave() {
        let dir = sandbox("undo-identity");
        let work = dir.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("a.txt"), b"original").unwrap();
        fs::write(work.join("x.txt"), b"x").unwrap();
        let pid = std::process::id();
        let folder = format!("kestrel-test-{pid}-made");
        {
            let (journal, _) = stored_journal(&dir);
            // A copy in place, a rename, and a new folder.
            let copy = Request::Transfer { sources: vec![text(&work.join("a.txt"))], destination: text(&work), kind: Transfer::Copy };
            journal.record(&execute(copy, QUIET).unwrap());
            journal.record(&rename(&work.join("x.txt"), "y.txt"));
            journal.record(&execute(Request::NewFolder { parent: text(&work), name: folder.clone() }, QUIET).unwrap());

            // Another program saves a document in the new folder: undoing the creation would
            // take it to the Recycle Bin.
            fs::write(work.join(&folder).join("report.txt"), b"a day's work").unwrap();
            let refused = journal.undo(QUIET).unwrap_err();
            assert_eq!(refused, format!("Couldn't undo the creation of {folder}: it has changed since."));
            assert!(work.join(&folder).join("report.txt").exists());
        }
        // While the app is closed: the copy is edited, and the renamed file is replaced by an
        // unrelated one of the same name.
        fs::write(work.join("a - Copy.txt"), b"edited since").unwrap();
        fs::remove_file(work.join("y.txt")).unwrap();
        fs::write(work.join("y.txt"), b"someone else's").unwrap();

        let (journal, db) = stored_journal(&dir);
        assert_eq!(journal.state(), UndoState::default(), "nothing left matches what the operations left");
        assert!(db.undo_entries().unwrap().is_empty());
        assert_eq!(fs::read(work.join("a - Copy.txt")).unwrap(), b"edited since");
        assert_eq!(fs::read(work.join("y.txt")).unwrap(), b"someone else's");
        assert!(!work.join("x.txt").exists());
        drop((journal, db));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Operations stored before marks were kept can still be renamed or moved back by path,
    /// but nothing of theirs is sent to the Recycle Bin: whether it is still what the operation
    /// made can't be told.
    #[test]
    fn stored_operations_without_marks_are_never_recycled() {
        let dir = sandbox("undo-unmarked");
        let work = dir.join("work");
        fs::create_dir_all(work.join("Old folder")).unwrap();
        fs::write(work.join("b.txt"), b"b").unwrap();
        {
            let db = Db::open(&dir.join("kestrel.db")).unwrap();
            let created = serde_json::to_string(&[Done::Created { path: text(&work.join("Old folder")) }]).unwrap();
            let renamed = serde_json::to_string(&[Done::Renamed { from: text(&work.join("a.txt")), to: text(&work.join("b.txt")) }]).unwrap();
            let written = [
                StoredEntry { id: 1, stack: Stack::Undo, label: "rename of a.txt".into(), steps: renamed, marks: None },
                StoredEntry { id: 2, stack: Stack::Undo, label: "creation of Old folder".into(), steps: created, marks: None },
            ];
            db.update_undo(&UndoChange { written: &written, ..UndoChange::default() }, LIMIT).unwrap();
        }
        let (journal, _) = stored_journal(&dir);
        assert_eq!(journal.state().undo.as_deref(), Some("rename of a.txt"));
        journal.undo(QUIET).unwrap();
        assert!(work.join("a.txt").exists() && work.join("Old folder").is_dir());
        drop(journal);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Within a session the usual sequence still works: a new folder, an item moved in, then
    /// both undone. The folder is empty again, as the operation left it, so it goes to the
    /// Recycle Bin, and the redo brings it back, so the bin ends as it began.
    #[test]
    fn a_new_folder_emptied_again_is_still_undone() {
        let dir = sandbox("undo-emptied");
        let pid = std::process::id();
        let folder = format!("kestrel-test-{pid}-emptied");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        let journal = Journal::default();
        journal.record(&execute(Request::NewFolder { parent: text(&dir), name: folder.clone() }, QUIET).unwrap());
        let into = Request::Transfer { sources: vec![text(&dir.join("a.txt"))], destination: text(&dir.join(&folder)), kind: Transfer::Move };
        journal.record(&execute(into, QUIET).unwrap());
        journal.undo(QUIET).unwrap();
        assert!(dir.join("a.txt").exists());
        assert_eq!(journal.undo(QUIET).unwrap().message, format!("Undid creation of {folder}"));
        assert!(!dir.join(&folder).exists());
        journal.redo(QUIET).unwrap();
        assert!(dir.join(&folder).is_dir());
        assert_eq!(crate::recycle::records_mentioning(&folder), Vec::<String>::new());
        fs::remove_dir_all(&dir).unwrap();
    }
}
