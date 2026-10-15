//! File operations carried out by the Windows shell's own copy engine, so progress, conflict
//! prompts, elevation, the Recycle Bin and Explorer's undo all behave as they do in Explorer.
//!
//! A progress sink listens to every operation and records what actually happened to each item
//! the caller asked for: the name a copy really got ("name - Copy", or "name (2)" after the user
//! chose to keep both), where a moved item ended up, which items reached the Recycle Bin. New
//! items are selected, and the undo history is built, from these results rather than guesses.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI,
    FOF_RENAMEONCOLLISION, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink,
    IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, Ref, implement};

use crate::recycle;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// The user pressed Cancel in a shell dialog.
const CANCELLED: [HRESULT; 2] = [HRESULT(0x800704C7_u32 as i32), HRESULT(0x80270000_u32 as i32)];
/// `COPYENGINE_S_USER_IGNORED`: the user chose to skip the item. A success code, but nothing
/// happened to the item.
const SKIPPED: HRESULT = HRESULT(0x0027_0005);
/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

#[derive(Debug)]
pub enum Request {
    /// Moves items to the Recycle Bin, or erases them outright when `permanent`.
    Delete { paths: Vec<String>, permanent: bool },
    Rename { path: String, new_name: String },
    NewFolder { parent: String, name: String },
    Transfer { sources: Vec<String>, destination: String, kind: Transfer },
}

/// One item's part in an operation. Several are queued and carried out together, as one shell
/// operation with one progress window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Copies `source` into `folder`, as `name` when given.
    Copy { source: String, folder: String, name: Option<String> },
    Move { source: String, folder: String, name: Option<String> },
    Rename { path: String, name: String },
    NewFolder { parent: String, name: String },
    Recycle { path: String },
    Erase { path: String },
    /// Moves the item deleted from `path` within `deleted` back out of the Recycle Bin.
    Restore { path: String, deleted: Window },
}

/// A span of time in FILETIME ticks (100 ns since 1601, UTC) within which something happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: i64,
    pub to: i64,
}

impl Window {
    pub fn contains(&self, time: i64) -> bool {
        (self.from..=self.to).contains(&time)
    }
}

/// What actually happened to one of the items an operation was asked to handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    /// `merged` is set when a folder was copied or moved into a folder of the same name that
    /// already existed, so the result also holds things that were there before.
    Copied { from: String, to: String, merged: bool },
    Moved { from: String, to: String, merged: bool },
    Renamed { from: String, to: String },
    Created { path: String },
    /// Sent to the Recycle Bin from `path` at a time within `when`.
    Recycled { path: String, when: Window },
    Erased { path: String },
    /// Back from the Recycle Bin, at `path`.
    Restored { path: String },
}

/// The result of an operation.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Nothing was cancelled or skipped by the user.
    pub completed: bool,
    /// What happened, in the order the shell did it, each with the index of the queued `Op`
    /// that produced it.
    pub done: Vec<(usize, Done)>,
    /// Indices of `Restore` ops whose item was no longer in the Recycle Bin.
    pub missing: Vec<usize>,
}

impl Outcome {
    pub fn results(&self) -> impl Iterator<Item = &Done> {
        self.done.iter().map(|(_, done)| done)
    }
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Window that owns the shell's dialogs, as a raw `HWND`.
    pub owner: Option<isize>,
    /// Suppress every dialog and answer prompts with their defaults. For tests.
    pub quiet: bool,
}

/// Balances `CoInitializeEx` on the operation's thread.
struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful CoInitializeEx on this thread.
        unsafe { CoUninitialize() };
    }
}

/// Checks a single file or folder name the way Windows would reject it.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        return Err("Type a name.".into());
    }
    if let Some(bad) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        return Err(format!("A name can't contain {bad}"));
    }
    Ok(())
}

/// The first of `base`, `base (2)`, `base (3)`… that does not exist in `parent`.
pub fn unused_name(parent: &str, base: &str) -> String {
    (1..)
        .map(|n| if n == 1 { base.to_owned() } else { format!("{base} ({n})") })
        .find(|name| !Path::new(parent).join(name).exists())
        .expect("an unused name exists")
}

/// The current time as FILETIME ticks.
pub fn now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    FILETIME_UNIX_EPOCH + (since.as_nanos() / 100) as i64
}

/// The last component of a path.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The folder containing `path`, with a drive root keeping its separator (`C:\`).
pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['\\', '/']) {
        Some(at) if trimmed[..at].ends_with(':') => trimmed[..=at].to_owned(),
        Some(at) => trimmed[..at].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// Performs `request`, waits for it, and reports exactly what happened.
pub fn execute(request: Request, options: Options) -> Result<Outcome, String> {
    let mut rename_on_collision = false;
    let ops = match request {
        Request::Delete { paths, permanent } => paths
            .into_iter()
            .map(|path| if permanent { Op::Erase { path } } else { Op::Recycle { path } })
            .collect(),
        Request::Rename { path, new_name } => vec![Op::Rename { path, name: new_name }],
        Request::NewFolder { parent, name } => vec![Op::NewFolder { parent, name }],
        Request::Transfer { sources, destination, kind } => {
            let in_place = |source: &String| {
                Path::new(source).parent().is_some_and(|parent| same_folder(parent, &destination))
            };
            // Copying into the folder the items already live in makes "name - Copy", as
            // Explorer does, instead of asking to overwrite each item with itself.
            rename_on_collision = kind == Transfer::Copy && sources.iter().all(in_place);
            sources
                .into_iter()
                // Moving an item to where it already is does nothing.
                .filter(|source| kind == Transfer::Copy || !in_place(source))
                .map(|source| match kind {
                    Transfer::Copy => Op::Copy { source, folder: destination.clone(), name: None },
                    Transfer::Move => Op::Move { source, folder: destination.clone(), name: None },
                })
                .collect()
        }
    };
    run(ops, rename_on_collision, options)
}

/// Performs `ops` as one shell operation and reports exactly what happened.
///
/// The shell's copy engine needs a single-threaded apartment and pumps its own messages while
/// it works, so each operation gets a thread of its own and several can run at once.
pub fn run(ops: Vec<Op>, rename_on_collision: bool, options: Options) -> Result<Outcome, String> {
    if ops.is_empty() {
        return Ok(Outcome { completed: true, ..Outcome::default() });
    }
    std::thread::Builder::new()
        .name("kestrel-fileop".into())
        .spawn(move || run_on_this_thread(&ops, rename_on_collision, options))
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "The file operation crashed".to_owned())?
}

/// Renames several items, in an order that never needs a name another item still holds.
///
/// Renames that swap names (a to b and b to a) go through a temporary name, which takes a
/// second shell operation because the shell queues items that already exist. The two steps
/// are reported as one rename per item. Results are tagged with their index in `renames`.
pub fn rename_all(renames: Vec<(String, String)>, options: Options) -> Result<Outcome, String> {
    let (first, second) = plan_renames(&renames);
    let (order, first): (Vec<usize>, Vec<Op>) = first.into_iter().unzip();
    let mut outcome = run(first, false, options)?;
    for (op, _) in &mut outcome.done {
        *op = order[*op];
    }
    if second.is_empty() || !outcome.completed {
        return Ok(outcome);
    }
    let more = run(second.into_iter().map(|(_, op)| op).collect(), false, options)?;
    // Fold "a to temp" and "temp to b" into "a to b".
    for (_, done) in more.done {
        let Done::Renamed { from: temp, to } = done else { continue };
        let earlier = outcome.done.iter_mut().find(|(_, d)| matches!(d, Done::Renamed { to: t, .. } if key(t) == key(&temp)));
        if let Some((_, Done::Renamed { to: target, .. })) = earlier {
            *target = to;
        }
    }
    outcome.completed = more.completed;
    Ok(outcome)
}

/// Renames in the order to run them, each with its index in the list they were planned from.
pub type Planned = Vec<(usize, Op)>;

/// Orders renames so that none targets a name still held by another item in the batch, and
/// breaks cycles through temporary names. Returns the renames to run first and, for cycles,
/// the renames from temporary to final names to run afterwards.
pub fn plan_renames(renames: &[(String, String)]) -> (Planned, Planned) {
    let mut pending: Vec<(usize, &String, &String)> =
        renames.iter().enumerate().map(|(at, (path, name))| (at, path, name)).collect();
    let mut first = Vec::new();
    let mut second = Vec::new();
    let mut temp = 0;
    while !pending.is_empty() {
        // A rename is blocked while another pending item still holds its target name.
        let blocked = |at: usize| {
            let (_, path, name) = pending[at];
            let wanted = key(&format!("{}\\{name}", parent(path)));
            pending.iter().enumerate().any(|(other, (_, path, _))| other != at && key(path) == wanted)
        };
        match (0..pending.len()).find(|&at| !blocked(at)) {
            Some(at) => {
                let (index, path, name) = pending.remove(at);
                first.push((index, Op::Rename { path: path.clone(), name: name.clone() }));
            }
            None => {
                // Every remaining rename waits on another: a cycle. Park one item.
                let (index, path, name) = pending.remove(0);
                temp += 1;
                let parked = format!("{}.kestrel-rename-{temp}-{}", file_name(path), std::process::id());
                let parked_path = format!("{}\\{parked}", parent(path).trim_end_matches('\\'));
                second.push((index, Op::Rename { path: parked_path, name: name.clone() }));
                first.push((index, Op::Rename { path: path.clone(), name: parked }));
            }
        }
    }
    (first, second)
}

fn shell_item(path: &str) -> Result<IShellItem, String> {
    // The shell's parser rejects forward slashes, which other parts of Windows accept.
    let path = &path.replace('/', "\\");
    // SAFETY: the path is a valid wide string for the duration of the call.
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_str()), None) }
        .map_err(|e| format!("{path}: {}", e.message()))
}

/// How paths are compared: Windows file names ignore case.
pub fn key(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// The file system path of a shell item, or its parsing name if it has none.
fn item_path(item: &IShellItem) -> Option<String> {
    [SIGDN_FILESYSPATH, SIGDN_DESKTOPABSOLUTEPARSING].into_iter().find_map(|form| {
        // SAFETY: the returned string is owned by us and freed once copied.
        unsafe {
            let name = item.GetDisplayName(form).ok()?;
            let text = name.to_string().ok();
            CoTaskMemFree(Some(name.0 as *const _));
            text
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Copy,
    Move,
    Rename,
    Delete,
    New,
}

/// One item the shell reported finishing.
#[derive(Debug)]
struct Event {
    kind: Kind,
    /// The key of the item the shell was asked to handle.
    source: String,
    /// What it became, if anything new exists now.
    result: Option<String>,
}

/// Receives the shell's reports while an operation runs. Only the items that were queued are
/// recorded: a folder's contents are reported too, but they travel with the folder.
#[implement(IFileOperationProgressSink)]
struct Sink {
    watched: HashSet<String>,
    events: Arc<Mutex<Vec<Event>>>,
    /// Some item was skipped by the user.
    skipped: Arc<Mutex<bool>>,
}

impl Sink {
    fn record(&self, kind: Kind, item: &Ref<IShellItem>, hr: HRESULT, created: &Ref<IShellItem>) {
        let Some(source) = item.ok().ok().and_then(item_path) else { return };
        self.push(kind, key(&source), hr, created);
    }

    fn push(&self, kind: Kind, source: String, hr: HRESULT, created: &Ref<IShellItem>) {
        if !self.watched.contains(&source) {
            return;
        }
        if hr == SKIPPED {
            if let Ok(mut skipped) = self.skipped.lock() {
                *skipped = true;
            }
            return;
        }
        if hr.is_err() {
            return;
        }
        let result = created.ok().ok().and_then(item_path);
        if let Ok(mut events) = self.events.lock() {
            events.push(Event { kind, source, result });
        }
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _hr: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreRenameItem(&self, _flags: u32, _item: Ref<IShellItem>, _name: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _name: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(Kind::Rename, &item, hr, &created);
        Ok(())
    }

    fn PreMoveItem(&self, _flags: u32, _item: Ref<IShellItem>, _folder: Ref<IShellItem>, _name: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _folder: Ref<IShellItem>,
        _name: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(Kind::Move, &item, hr, &created);
        Ok(())
    }

    fn PreCopyItem(&self, _flags: u32, _item: Ref<IShellItem>, _folder: Ref<IShellItem>, _name: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _folder: Ref<IShellItem>,
        _name: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(Kind::Copy, &item, hr, &created);
        Ok(())
    }

    fn PreDeleteItem(&self, _flags: u32, _item: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostDeleteItem(&self, _flags: u32, item: Ref<IShellItem>, hr: HRESULT, created: Ref<IShellItem>) -> windows::core::Result<()> {
        // `created` is the item in the Recycle Bin, absent when the item was erased.
        self.record(Kind::Delete, &item, hr, &created);
        Ok(())
    }

    fn PreNewItem(&self, _flags: u32, _folder: Ref<IShellItem>, _name: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _flags: u32,
        folder: Ref<IShellItem>,
        name: &PCWSTR,
        _template: &PCWSTR,
        _attributes: u32,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let Some(folder) = folder.ok().ok().and_then(item_path) else { return Ok(()) };
        // SAFETY: the shell passes a valid NUL-terminated name for the duration of the call.
        let Ok(name) = (unsafe { name.to_string() }) else { return Ok(()) };
        self.push(Kind::New, new_item_key(&folder, &name), hr, &created);
        Ok(())
    }

    fn UpdateProgress(&self, _total: u32, _so_far: u32) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

fn new_item_key(folder: &str, name: &str) -> String {
    key(&format!("{}\\{name}", folder.trim_end_matches('\\')))
}

/// An op as queued: what the sink will report it under and how to read the report.
struct Queued {
    op: usize,
    key: String,
    /// The folder or file the item would land on already existed, for spotting merges.
    target_existed: Option<String>,
}

/// Completes each restore that happened by removing the Recycle Bin's record of the item.
fn forget_restored(ops: &[Op], done: &[(usize, Done)], bin_items: &HashMap<usize, String>) {
    for (op, result) in done {
        if let (Done::Restored { .. }, Op::Restore { path, .. }, Some(bin_item)) = (result, &ops[*op], bin_items.get(op)) {
            recycle::forget(bin_item, path);
        }
    }
}

fn run_on_this_thread(ops: &[Op], rename_on_collision: bool, options: Options) -> Result<Outcome, String> {
    let describe = |e: windows::core::Error| e.message();
    let erase = ops.iter().any(|op| matches!(op, Op::Erase { .. }));
    let recycle = ops.iter().any(|op| matches!(op, Op::Recycle { .. }));
    if erase && recycle {
        return Err("An operation can't both recycle and erase items".into());
    }
    let started = now();
    // SAFETY: COM is initialised for this thread before any COM call and uninitialised by
    // `Apartment` after the last interface pointer is dropped; every pointer passed to the
    // shell below outlives the call it is passed to.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(describe)?;
        let _apartment = Apartment;

        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(describe)?;
        let mut flags: FILEOPERATION_FLAGS = FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD;
        flags |= if erase { FOF_WANTNUKEWARNING } else { FOF_ALLOWUNDO };
        if recycle {
            flags |= FOFX_RECYCLEONDELETE;
        }
        if rename_on_collision {
            flags |= FOF_RENAMEONCOLLISION;
        }
        if options.quiet {
            flags |= FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI;
        }
        if let Some(owner) = options.owner {
            operation.SetOwnerWindow(HWND(owner as _)).map_err(describe)?;
        }

        let mut queued: Vec<Queued> = Vec::new();
        let mut missing = Vec::new();
        // Where each item being restored sits in the Recycle Bin, by op.
        let mut bin_items: HashMap<usize, String> = HashMap::new();
        // Items being restored are found in the Recycle Bin all at once.
        let wanted: Vec<(String, Window)> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Restore { path, deleted } => Some((path.clone(), *deleted)),
                _ => None,
            })
            .collect();
        let mut found = if wanted.is_empty() { Vec::new() } else { recycle::find(&wanted).map_err(describe)? }.into_iter();

        for (index, op) in ops.iter().enumerate() {
            let mut target_existed = None;
            let item = match op {
                Op::Copy { source, folder, name } | Op::Move { source, folder, name } => {
                    let item = shell_item(source)?;
                    let target = format!("{}\\{}", folder.trim_end_matches('\\'), name.as_deref().unwrap_or(file_name(source)));
                    if Path::new(&target).exists() {
                        target_existed = Some(key(&target));
                    }
                    let name = name.as_deref().map(HSTRING::from);
                    let name = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
                    let folder = shell_item(folder)?;
                    if matches!(op, Op::Copy { .. }) {
                        operation.CopyItem(&item, &folder, name, None)
                    } else {
                        operation.MoveItem(&item, &folder, name, None)
                    }
                    .map_err(describe)?;
                    item
                }
                Op::Rename { path, name } => {
                    validate_name(name)?;
                    let item = shell_item(path)?;
                    operation.RenameItem(&item, &HSTRING::from(name.as_str()), None).map_err(describe)?;
                    item
                }
                Op::NewFolder { parent, name } => {
                    validate_name(name)?;
                    let folder = shell_item(parent)?;
                    operation
                        .NewItem(&folder, FILE_ATTRIBUTE_DIRECTORY, &HSTRING::from(name.as_str()), PCWSTR::null(), None)
                        .map_err(describe)?;
                    let folder_path = item_path(&folder).unwrap_or_else(|| parent.clone());
                    queued.push(Queued { op: index, key: new_item_key(&folder_path, name), target_existed: None });
                    continue;
                }
                Op::Recycle { path } | Op::Erase { path } => {
                    let item = shell_item(path)?;
                    operation.DeleteItem(&item, None).map_err(describe)?;
                    item
                }
                Op::Restore { path, .. } => {
                    let Some(Some(entry)) = found.next() else {
                        missing.push(index);
                        continue;
                    };
                    let folder = shell_item(&parent(path)).map_err(|_| {
                        format!("{} can't be put back because its folder {} no longer exists", file_name(path), parent(path))
                    })?;
                    operation
                        .MoveItem(&entry, &folder, &HSTRING::from(file_name(path)), None)
                        .map_err(describe)?;
                    if let Some(bin_item) = item_path(&entry) {
                        bin_items.insert(index, bin_item);
                    }
                    entry
                }
            };
            let key = item_path(&item).map(|path| key(&path)).unwrap_or_default();
            queued.push(Queued { op: index, key, target_existed });
        }
        if queued.is_empty() {
            return Ok(Outcome { completed: missing.is_empty(), missing, ..Outcome::default() });
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let skipped = Arc::new(Mutex::new(false));
        let sink: IFileOperationProgressSink = Sink {
            watched: queued.iter().map(|q| q.key.clone()).collect(),
            events: events.clone(),
            skipped: skipped.clone(),
        }
        .into();
        let cookie = operation.Advise(&sink).map_err(describe)?;
        operation.SetOperationFlags(flags).map_err(describe)?;
        let performed = operation.PerformOperations();
        let _ = operation.Unadvise(cookie);
        let aborted = match performed {
            Ok(()) => operation.GetAnyOperationsAborted().map_err(describe)?.as_bool(),
            Err(e) if CANCELLED.contains(&e.code()) => true,
            Err(e) => return Err(describe(e)),
        };

        let when = Window { from: started, to: now() };
        let events = std::mem::take(&mut *events.lock().map_err(|_| "The file operation crashed")?);
        let done = interpret(ops, &queued, events, when);
        forget_restored(ops, &done, &bin_items);
        let skipped = *skipped.lock().map_err(|_| "The file operation crashed")?;
        Ok(Outcome { completed: !aborted && !skipped && missing.is_empty(), done, missing })
    }
}

/// Turns the shell's reports into results, each tagged with the op it belongs to.
fn interpret(ops: &[Op], queued: &[Queued], events: Vec<Event>, when: Window) -> Vec<(usize, Done)> {
    let by_key: HashMap<&str, &Queued> = queued.iter().map(|q| (q.key.as_str(), q)).collect();
    let mut seen = HashSet::new();
    let mut done = Vec::new();
    for event in events {
        let Some(queued) = by_key.get(event.source.as_str()) else { continue };
        // A merged folder is reported again for each item merged into it; the first report
        // is the one for the item itself.
        if !seen.insert(queued.op) {
            continue;
        }
        let op = &ops[queued.op];
        let merged = |to: &str| queued.target_existed.as_deref() == Some(key(to).as_str()) && Path::new(to).is_dir();
        // Paths as the caller gave them, with the separators Windows itself uses.
        let given = |path: &String| path.replace('/', "\\");
        let result = match (event.kind, op, event.result) {
            (Kind::Copy, Op::Copy { source, .. }, Some(to)) => Done::Copied { merged: merged(&to), from: given(source), to },
            (Kind::Move, Op::Restore { .. }, Some(to)) => Done::Restored { path: to },
            (Kind::Move, Op::Move { source, .. }, Some(to)) => Done::Moved { merged: merged(&to), from: given(source), to },
            (Kind::Rename, Op::Rename { path, .. }, Some(to)) => Done::Renamed { from: given(path), to },
            (Kind::New, Op::NewFolder { .. }, Some(path)) => Done::Created { path },
            (Kind::Delete, Op::Recycle { path }, Some(_)) => Done::Recycled { path: given(path), when },
            // The shell erases what it can't recycle, such as items too large for the bin,
            // after asking.
            (Kind::Delete, Op::Recycle { path } | Op::Erase { path }, _) => Done::Erased { path: given(path) },
            _ => continue,
        };
        done.push((queued.op, result));
    }
    done
}

/// Whether `folder` and `other` name the same folder, ignoring case and a trailing separator.
fn same_folder(folder: &Path, other: &str) -> bool {
    let trim = |text: &str| text.trim_end_matches(['\\', '/']).to_lowercase();
    folder.to_str().is_some_and(|folder| trim(folder) == trim(other))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    pub const QUIET: Options = Options { owner: None, quiet: true };

    /// A scratch folder under the temp directory; every test stays inside its own.
    pub fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-fileops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn validates_names() {
        assert!(validate_name("notes.txt").is_ok());
        assert!(validate_name("  ").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("what?").is_err());
    }

    #[test]
    fn splits_paths() {
        assert_eq!(file_name(r"C:\a\b.txt"), "b.txt");
        assert_eq!(file_name(r"C:\a\dir\"), "dir");
        assert_eq!(parent(r"C:\a\b.txt"), r"C:\a");
        assert_eq!(parent(r"C:\b.txt"), r"C:\");
        assert_eq!(key(r"C:/A/B\"), r"c:\a\b");
    }

    #[test]
    fn creates_renames_and_erases() {
        let dir = sandbox("basic");
        assert_eq!(unused_name(&text(&dir), "New folder"), "New folder");
        let created = execute(Request::NewFolder { parent: text(&dir), name: "New folder".into() }, QUIET).unwrap();
        assert!(dir.join("New folder").is_dir());
        assert_eq!(created.done, [(0, Done::Created { path: text(&dir.join("New folder")) })]);
        assert_eq!(unused_name(&text(&dir), "New folder"), "New folder (2)");

        let renamed =
            execute(Request::Rename { path: text(&dir.join("New folder")), new_name: "Renamed".into() }, QUIET).unwrap();
        assert!(dir.join("Renamed").is_dir() && !dir.join("New folder").exists());
        let expected = Done::Renamed { from: text(&dir.join("New folder")), to: text(&dir.join("Renamed")) };
        assert_eq!(renamed.results().collect::<Vec<_>>(), [&expected]);
        assert!(execute(Request::Rename { path: text(&dir.join("Renamed")), new_name: "a:b".into() }, QUIET).is_err());

        fs::write(dir.join("Renamed/file.txt"), b"x").unwrap();
        let erased = execute(Request::Delete { paths: vec![text(&dir.join("Renamed"))], permanent: true }, QUIET).unwrap();
        assert!(!dir.join("Renamed").exists());
        assert_eq!(erased.results().collect::<Vec<_>>(), [&Done::Erased { path: text(&dir.join("Renamed")) }]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_and_moves_report_where_items_went() {
        let dir = sandbox("transfer");
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("dst")).unwrap();
        fs::write(dir.join("src/a.txt"), b"alpha").unwrap();
        fs::write(dir.join("src/nested/b.txt"), b"beta").unwrap();
        // Forward slashes, which Windows accepts elsewhere, work too.
        let sources = vec![text(&dir.join("src/a.txt")), text(&dir.join("src/nested"))];
        let native = |path: &str| path.replace('/', "\\");

        let copy = Request::Transfer { sources: sources.clone(), destination: text(&dir.join("dst")), kind: Transfer::Copy };
        let copied = execute(copy, QUIET).unwrap();
        assert!(copied.completed);
        assert_eq!(fs::read(dir.join("dst/a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dir.join("dst/nested/b.txt")).unwrap(), b"beta");
        assert!(dir.join("src/a.txt").exists(), "a copy leaves the source in place");
        // Only the two items asked for are reported, not the folder's contents.
        let mut results: Vec<Done> = copied.results().cloned().collect();
        results.sort_by_key(|done| format!("{done:?}"));
        assert_eq!(
            results,
            [
                Done::Copied { from: native(&sources[0]), to: text(&dir.join("dst").join("a.txt")), merged: false },
                Done::Copied { from: native(&sources[1]), to: text(&dir.join("dst").join("nested")), merged: false },
            ]
        );

        // Copying into the same folder keeps the original and reports the duplicate's name.
        let duplicate = Request::Transfer {
            sources: vec![text(&dir.join("src/a.txt"))],
            destination: text(&dir.join("src")),
            kind: Transfer::Copy,
        };
        let duplicated = execute(duplicate, QUIET).unwrap();
        let [(_, Done::Copied { to, .. })] = duplicated.done.as_slice() else { panic!("{duplicated:?}") };
        assert_eq!(file_name(to), "a - Copy.txt");
        assert!(Path::new(to).exists());
        assert_eq!(fs::read_dir(dir.join("src")).unwrap().count(), 3, "a.txt, its duplicate and nested");

        // Copying again into a folder that has the folder already merges into it.
        let again = Request::Transfer { sources: vec![sources[1].clone()], destination: text(&dir.join("dst")), kind: Transfer::Copy };
        let merged = execute(again, QUIET).unwrap();
        assert!(matches!(merged.done.as_slice(), [(_, Done::Copied { merged: true, .. })]), "{merged:?}");

        fs::create_dir_all(dir.join("moved")).unwrap();
        let mv = Request::Transfer { sources: sources.clone(), destination: text(&dir.join("moved")), kind: Transfer::Move };
        let moved = execute(mv, QUIET).unwrap();
        assert!(dir.join("moved/a.txt").exists() && dir.join("moved/nested/b.txt").exists());
        assert!(!dir.join("src/a.txt").exists() && !dir.join("src/nested").exists());
        let tos: HashSet<String> = moved
            .results()
            .map(|done| match done {
                Done::Moved { to, merged: false, .. } => key(to),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(tos, HashSet::from([key(&text(&dir.join("moved").join("a.txt"))), key(&text(&dir.join("moved").join("nested")))]));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renames_in_an_order_that_never_collides() {
        let dir = sandbox("rename-order");
        for name in ["1.txt", "2.txt", "a.txt", "b.txt"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let path = |name: &str| text(&dir.join(name));
        // A chain (1 to 2, 2 to 3) and a swap (a and b), all at once.
        let renames = vec![
            (path("1.txt"), "2.txt".to_owned()),
            (path("2.txt"), "3.txt".to_owned()),
            (path("a.txt"), "b.txt".to_owned()),
            (path("b.txt"), "a.txt".to_owned()),
        ];
        let (first, second) = plan_renames(&renames);
        assert_eq!(first[0], (1, Op::Rename { path: path("2.txt"), name: "3.txt".into() }), "the chain's end goes first");
        assert_eq!(second.len(), 1, "the swap needs one temporary name");

        let outcome = rename_all(renames, QUIET).unwrap();
        assert!(outcome.completed);
        for (name, content) in [("2.txt", "1.txt"), ("3.txt", "2.txt"), ("a.txt", "b.txt"), ("b.txt", "a.txt")] {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), content);
        }
        let mut results: Vec<(String, String)> = outcome
            .results()
            .map(|done| match done {
                Done::Renamed { from, to } => (file_name(from).to_owned(), file_name(to).to_owned()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        results.sort();
        let expected = [("1.txt", "2.txt"), ("2.txt", "3.txt"), ("a.txt", "b.txt"), ("b.txt", "a.txt")];
        assert_eq!(results, expected.map(|(a, b)| (a.to_owned(), b.to_owned())));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 4, "no temporary names are left behind");
        fs::remove_dir_all(&dir).unwrap();
    }
}
