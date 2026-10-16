//! Network, as Explorer has it: Map network drive and Disconnect network drive (Windows' own
//! dialogs, `WNetConnectionDialog1` and `WNetDisconnectDialog1`, or the disconnect picker,
//! `WNetDisconnectDialog`), the shell's "Add a network location" wizard, the computers of the
//! Network location, the shares of a computer, and the network locations the wizard made.
//!
//! Computers are found with `WNetOpenEnum`, walking from the network providers to their domains
//! and workgroups to the servers in them. That can take long (each container asks the network,
//! and a browser that doesn't answer takes many seconds), so the walk runs on a thread of its
//! own: a call waits only so long ([`Network::browse`]), returns what has been found by then,
//! and the finished walk is kept for a minute and announced (`network-changed`). The provider
//! is a trait so tests can stand in a slow one. A server's shares are asked for with a time limit
//! too: a server that is away takes about 21 seconds to say so.
//!
//! Development builds can record the dialogs instead of showing them, for the end-to-end tests
//! (`network_dev_record`), and give the Network location computers of the tests' own.

use std::collections::VecDeque;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State, Window};
use windows_sys::Win32::Foundation::{HANDLE, HWND, NO_ERROR};
use windows_sys::Win32::NetworkManagement::WNet::{
    CONNDLG_PERSIST, CONNDLG_USE_MRU, CONNECTDLGSTRUCTW, DISC_UPDATE_PROFILE, DISCDLGSTRUCTW, NETRESOURCEW, RESOURCE_GLOBALNET,
    RESOURCETYPE_DISK, RESOURCEUSAGE_CONTAINER, WNetCloseEnum, WNetConnectionDialog1W, WNetDisconnectDialog, WNetDisconnectDialog1W,
    WNetEnumResourceW, WNetOpenEnumW,
};
use windows_sys::Win32::UI::Shell::FOLDERID_NetHood;

use super::{dev_only, from_wide, known_folder, wide};

/// How long the Network location waits for the walk before showing what it has.
const BROWSE_WAIT: Duration = Duration::from_secs(3);
/// How long a finished walk is reused.
const KEEP_FOR: Duration = Duration::from_secs(60);
/// How long a server's shares are waited for.
const SHARES_WAIT: Duration = Duration::from_secs(4);
/// `WNetConnectionDialog1` and friends say "cancelled" this way.
const CANCELLED: u32 = u32::MAX;
/// What `WNetOpenEnum` reports about each resource (WinSock's names for them).
const DISPLAY_DOMAIN: u32 = 1;
const DISPLAY_SERVER: u32 = 2;
const DISPLAY_SHARE: u32 = 3;
const DISPLAY_NETWORK: u32 = 6;
/// `ERROR_NO_MORE_ITEMS`, the end of an enumeration.
const NO_MORE_ITEMS: u32 = 259;
/// `ERROR_MORE_DATA`: the buffer was too small for the next item.
const MORE_DATA: u32 = 234;

// ---------- the network's resources ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A network provider, such as "Microsoft Windows Network".
    Network,
    /// A domain or workgroup.
    Domain,
    Server,
    Share,
    Other,
}

/// One resource `WNetOpenEnum` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// Its remote name: `\\SERVER`, `WORKGROUP`, or the provider's name.
    pub name: String,
    pub provider: String,
    pub comment: String,
    pub kind: Kind,
    /// It holds other resources.
    pub container: bool,
}

/// Where resources come from: the network itself, or a test's stand-in.
pub trait Provider: Send + Sync + 'static {
    /// The resources inside `container`; `None` asks for the top of the network.
    fn children(&self, container: Option<&Resource>) -> Result<Vec<Resource>, String>;
}

/// The network as `WNetOpenEnum` sees it, disks only.
pub struct Wnet;

impl Provider for Wnet {
    fn children(&self, container: Option<&Resource>) -> Result<Vec<Resource>, String> {
        let (name, provider) = container.map_or((Vec::new(), Vec::new()), |c| (wide(&c.name), wide(&c.provider)));
        let parent = container.map(|c| NETRESOURCEW {
            dwScope: RESOURCE_GLOBALNET,
            dwType: RESOURCETYPE_DISK,
            dwDisplayType: match c.kind {
                Kind::Network => DISPLAY_NETWORK,
                Kind::Domain => DISPLAY_DOMAIN,
                Kind::Server => DISPLAY_SERVER,
                Kind::Share => DISPLAY_SHARE,
                Kind::Other => 0,
            },
            dwUsage: RESOURCEUSAGE_CONTAINER,
            lpLocalName: null_mut(),
            lpRemoteName: name.as_ptr().cast_mut(),
            lpComment: null_mut(),
            lpProvider: if provider.len() > 1 { provider.as_ptr().cast_mut() } else { null_mut() },
        });
        let mut handle: HANDLE = null_mut();
        // SAFETY: `parent` and the strings it points into outlive the call, which only reads
        // them; the handle is closed below.
        let opened = unsafe {
            WNetOpenEnumW(RESOURCE_GLOBALNET, RESOURCETYPE_DISK, 0, parent.as_ref().map_or(null(), |p| p as *const _), &mut handle)
        };
        if opened != NO_ERROR {
            return Err(format!("Windows couldn't look at the network (error {opened})."));
        }
        let mut found = Vec::new();
        // Room for many resources at once; a bigger buffer is asked for when one doesn't fit.
        let mut buffer: Vec<u64> = vec![0; 16 * 1024 / 8];
        let result = loop {
            let mut count = u32::MAX;
            let mut size = (buffer.len() * 8) as u32;
            // SAFETY: the buffer is as large as `size` says, aligned for NETRESOURCEW, and the
            // handle is open.
            let status = unsafe { WNetEnumResourceW(handle, &mut count, buffer.as_mut_ptr().cast(), &mut size) };
            match status {
                NO_ERROR => {
                    // SAFETY: on success the buffer starts with `count` NETRESOURCEW, whose
                    // strings point into the same buffer.
                    let items = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<NETRESOURCEW>(), count as usize) };
                    for item in items {
                        // SAFETY: each string is null or NUL-terminated within the buffer.
                        let (name, provider, comment) = unsafe { (from_wide(item.lpRemoteName), from_wide(item.lpProvider), from_wide(item.lpComment)) };
                        let kind = match item.dwDisplayType {
                            DISPLAY_NETWORK => Kind::Network,
                            DISPLAY_DOMAIN => Kind::Domain,
                            DISPLAY_SERVER => Kind::Server,
                            DISPLAY_SHARE => Kind::Share,
                            _ => Kind::Other,
                        };
                        let container = item.dwUsage & RESOURCEUSAGE_CONTAINER != 0;
                        if !name.is_empty() {
                            found.push(Resource { name, provider, comment, kind, container });
                        }
                    }
                }
                NO_MORE_ITEMS => break Ok(found),
                MORE_DATA => buffer.resize((size as usize).div_ceil(8).max(buffer.len() * 2), 0),
                error => break Err(format!("Windows couldn't look at the network (error {error}).")),
            }
        };
        // SAFETY: the handle came from WNetOpenEnumW above.
        unsafe { WNetCloseEnum(handle) };
        result
    }
}

/// A computer on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Computer {
    /// Its name, without the backslashes.
    pub name: String,
    /// `\\NAME`, its address.
    pub path: String,
    /// The domain or workgroup it was found in.
    pub group: String,
    pub comment: String,
}

/// What the Network location shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Neighbourhood {
    pub computers: Vec<Computer>,
    /// The walk is over; otherwise more may come (`network-changed`).
    pub complete: bool,
    /// Why nothing could be found, if the network said.
    pub error: Option<String>,
}

/// Walks the network from the top to its servers, never into them, adding each computer to
/// `found` as it turns up. Returns the first error met, if nothing was found at all.
fn walk(provider: &dyn Provider, found: &Mutex<Vec<Computer>>) -> Option<String> {
    let mut first_error = None;
    // Breadth first, so the computers of a workgroup that answers come before a slow one's.
    let mut queue: VecDeque<(Option<Resource>, String)> = VecDeque::from([(None, String::new())]);
    let mut visited = 0;
    while let Some((container, group)) = queue.pop_front() {
        // A network that names containers without end is not walked for ever.
        visited += 1;
        if visited > 512 {
            break;
        }
        match provider.children(container.as_ref()) {
            Ok(children) => {
                for child in children {
                    match child.kind {
                        Kind::Server => {
                            let name = child.name.trim_start_matches('\\').to_owned();
                            let mut list = found.lock();
                            if !name.is_empty() && !list.iter().any(|known| known.name.eq_ignore_ascii_case(&name)) {
                                list.push(Computer { path: format!(r"\\{name}"), name, group: group.clone(), comment: child.comment });
                            }
                        }
                        Kind::Share => {}
                        _ if child.container => {
                            let group = if child.kind == Kind::Domain { child.name.clone() } else { group.clone() };
                            queue.push_back((Some(child), group));
                        }
                        _ => {}
                    }
                }
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error
}

#[derive(Default)]
struct Walks {
    /// The last finished walk, and when it finished.
    finished: Option<(Instant, Neighbourhood)>,
    /// The walk under way, with what it has found so far.
    running: Option<Arc<Mutex<Vec<Computer>>>>,
}

/// The Network location's computers: walked on a thread of its own, kept for a while.
pub struct Network {
    provider: Mutex<Arc<dyn Provider>>,
    state: Mutex<Walks>,
    done: Condvar,
}

impl Default for Network {
    fn default() -> Self {
        Self::with_provider(Arc::new(Wnet))
    }
}

impl Network {
    pub fn with_provider(provider: Arc<dyn Provider>) -> Self {
        Self { provider: Mutex::new(provider), state: Mutex::default(), done: Condvar::new() }
    }

    /// Uses another provider from now on, forgetting what the last one found.
    pub fn set_provider(&self, provider: Arc<dyn Provider>) {
        *self.provider.lock() = provider;
        self.state.lock().finished = None;
    }

    /// The computers on the network: the last walk's if it finished less than a minute ago
    /// (unless `fresh`), else those a walk finds within `wait`. A walk still running carries on
    /// and calls `finished` with everything it found once it is over.
    pub fn browse(self: &Arc<Self>, wait: Duration, fresh: bool, finished: impl FnOnce(Neighbourhood) + Send + 'static) -> Neighbourhood {
        let deadline = Instant::now() + wait;
        let mut state = self.state.lock();
        if !fresh
            && let Some((at, known)) = &state.finished
            && at.elapsed() < KEEP_FOR
        {
            return known.clone();
        }
        let found = match &state.running {
            Some(found) => found.clone(),
            None => {
                let found = Arc::new(Mutex::new(Vec::new()));
                state.running = Some(found.clone());
                let (network, provider, walking) = (self.clone(), self.provider.lock().clone(), found.clone());
                let started = std::thread::Builder::new().name("kestrel-network".into()).spawn(move || {
                    let error = walk(provider.as_ref(), &walking);
                    let mut computers = walking.lock().clone();
                    computers.sort_by_key(|computer| computer.name.to_lowercase());
                    let error = if computers.is_empty() { error } else { None };
                    let result = Neighbourhood { computers, complete: true, error };
                    let mut state = network.state.lock();
                    state.running = None;
                    state.finished = Some((Instant::now(), result.clone()));
                    network.done.notify_all();
                    drop(state);
                    finished(result);
                });
                if started.is_err() {
                    state.running = None;
                    return Neighbourhood { error: Some("Couldn't look at the network.".into()), ..Neighbourhood::default() };
                }
                found
            }
        };
        // Wait for this walk to end, but no longer than asked.
        while state.running.as_ref().is_some_and(|running| Arc::ptr_eq(running, &found)) {
            if self.done.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        if state.running.as_ref().is_some_and(|running| Arc::ptr_eq(running, &found)) {
            let mut computers = found.lock().clone();
            computers.sort_by_key(|computer| computer.name.to_lowercase());
            return Neighbourhood { computers, complete: false, error: None };
        }
        state.finished.as_ref().map(|(_, known)| known.clone()).unwrap_or_default()
    }
}

/// A computer of the tests' own, found after `delay`.
struct Fake {
    computers: Vec<String>,
    delay: Duration,
}

impl Provider for Fake {
    fn children(&self, container: Option<&Resource>) -> Result<Vec<Resource>, String> {
        let resource = |name: &str, kind: Kind, container: bool| Resource { name: name.into(), provider: "Test".into(), comment: String::new(), kind, container };
        match container.map(|c| c.kind) {
            None => Ok(vec![resource("Test network", Kind::Network, true)]),
            Some(Kind::Network) => Ok(vec![resource("TESTGROUP", Kind::Domain, true)]),
            Some(Kind::Domain) => {
                std::thread::sleep(self.delay);
                Ok(self.computers.iter().map(|name| resource(&format!(r"\\{name}"), Kind::Server, true)).collect())
            }
            _ => Ok(Vec::new()),
        }
    }
}

// ---------- shares and network locations ----------

/// A server's disk shares, waited for no longer than [`SHARES_WAIT`]; `None` if it didn't
/// answer in time.
pub fn shares(server: &str, show_hidden: bool) -> Option<Vec<String>> {
    let (send, receive) = mpsc::channel();
    let server = format!(r"\\{}", server.trim_start_matches('\\').trim_end_matches('\\'));
    std::thread::Builder::new()
        .name("kestrel-shares".into())
        .spawn(move || {
            let _ = send.send(crate::address::shares(&server, show_hidden));
        })
        .ok()?;
    receive.recv_timeout(SHARES_WAIT).ok()
}

/// A network location: a shortcut to a share or a web folder, made by "Add a network location".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Place {
    pub name: String,
    /// Where it leads.
    pub target: String,
}

/// The network locations in `folder` (the account's Network Shortcuts): folders holding a
/// `target.lnk`, as the wizard makes them, and plain shortcuts. `read` reads a shortcut's target.
pub fn places_in(folder: &Path, read: impl Fn(&Path) -> Option<String>) -> Vec<Place> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut places: Vec<Place> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let target = if path.is_dir() {
                read(&path.join("target.lnk"))?
            } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")) {
                read(&path)?
            } else {
                return None;
            };
            let name = if path.is_dir() { path.file_name()?.to_string_lossy().into_owned() } else { name };
            Some(Place { name, target })
        })
        .collect();
    places.sort_by_key(|place| place.name.to_lowercase());
    places
}

// ---------- the dialogs ----------

/// A dialog asked for, as the development hook records it instead of showing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Call {
    /// The Windows function that would show it.
    pub function: &'static str,
    /// The drive or share it was given, if any.
    pub subject: Option<String>,
}

/// Dialogs recorded instead of shown, while the hook is on.
static RECORDED: Mutex<Option<Vec<Call>>> = Mutex::new(None);

/// Records `call` and returns true while the hook is on; otherwise the dialog is shown.
fn recorded(call: Call) -> bool {
    match RECORDED.lock().as_mut() {
        Some(calls) => {
            calls.push(call);
            true
        }
        None => false,
    }
}

fn owner(window: &Window) -> HWND {
    window.hwnd().map_or(null_mut(), |hwnd| hwnd.0 as HWND)
}

/// The drive letter `Z:` of a path on a drive, as Windows' disconnect dialog takes it.
fn drive_of(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':').then(|| format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
}

// ---------- commands ----------

/// Map network drive: Windows' own dialog (`WNetConnectionDialog1`), with `remote` filled in
/// if given. Returns the drive it mapped, such as `Z:\`, or `None` if cancelled.
#[tauri::command(async)]
pub fn network_map_drive(window: Window, remote: Option<String>) -> Result<Option<String>, String> {
    if recorded(Call { function: "WNetConnectionDialog1", subject: remote.clone() }) {
        return Ok(None);
    }
    let remote = remote.filter(|text| !text.trim().is_empty()).map(|text| wide(text.trim()));
    let mut resource = NETRESOURCEW {
        dwType: RESOURCETYPE_DISK,
        lpRemoteName: remote.as_ref().map_or(null_mut(), |text| text.as_ptr().cast_mut()),
        ..NETRESOURCEW::default()
    };
    let mut dialog = CONNECTDLGSTRUCTW {
        cbStructure: size_of::<CONNECTDLGSTRUCTW>() as u32,
        hwndOwner: owner(&window),
        lpConnRes: &mut resource,
        dwFlags: CONNDLG_USE_MRU | if remote.is_some() { CONNDLG_PERSIST } else { 0 },
        dwDevNum: 0,
    };
    // SAFETY: the structure and the strings it points to outlive the modal dialog.
    let status = unsafe { WNetConnectionDialog1W(&mut dialog) };
    match status {
        NO_ERROR if dialog.dwDevNum > 0 && dialog.dwDevNum <= 26 => Ok(Some(format!("{}:\\", (b'A' + dialog.dwDevNum as u8 - 1) as char))),
        NO_ERROR | CANCELLED => Ok(None),
        error => Err(format!("Windows couldn't map the drive (error {error}).")),
    }
}

/// Disconnect network drive: for one drive (`WNetDisconnectDialog1`, which asks only if files
/// are open on it), or Windows' list of mapped drives to choose from (`WNetDisconnectDialog`).
#[tauri::command(async)]
pub fn network_disconnect_drive(window: Window, path: Option<String>) -> Result<(), String> {
    let drive = path.as_deref().and_then(drive_of);
    let function = if drive.is_some() { "WNetDisconnectDialog1" } else { "WNetDisconnectDialog" };
    if recorded(Call { function, subject: drive.clone() }) {
        return Ok(());
    }
    let status = match drive {
        Some(drive) => {
            let local = wide(&drive);
            let dialog = DISCDLGSTRUCTW {
                cbStructure: size_of::<DISCDLGSTRUCTW>() as u32,
                hwndOwner: owner(&window),
                lpLocalName: local.as_ptr().cast_mut(),
                lpRemoteName: null_mut(),
                dwFlags: DISC_UPDATE_PROFILE,
            };
            // SAFETY: the structure and its string outlive the call.
            unsafe { WNetDisconnectDialog1W(&dialog) }
        }
        // SAFETY: a window handle (or none) and a resource type: nothing else to keep alive.
        None => unsafe { WNetDisconnectDialog(owner(&window), RESOURCETYPE_DISK) },
    };
    match status {
        NO_ERROR | CANCELLED => Ok(()),
        error => Err(format!("Windows couldn't disconnect the drive (error {error}).")),
    }
}

/// Add a network location: the shell's own wizard, which makes a shortcut among the account's
/// network locations.
#[tauri::command(async)]
pub fn network_add_location() -> Result<(), String> {
    if recorded(Call { function: "AddNetPlaceRunDll", subject: None }) {
        return Ok(());
    }
    let system = std::env::var("SystemRoot").map_or_else(|_| r"C:\Windows\System32".to_owned(), |root| format!(r"{root}\System32"));
    std::process::Command::new(format!(r"{system}\rundll32.exe"))
        .arg(format!(r"{system}\shwebsvc.dll,AddNetPlaceRunDll"))
        .current_dir(&system)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Couldn't start the network location wizard: {error}"))
}

/// The computers of the Network location (see [`Network::browse`]). `fresh` walks the network
/// again, as Refresh does.
#[tauri::command(async)]
pub fn network_computers(app: AppHandle, network: State<Arc<Network>>, fresh: Option<bool>) -> Neighbourhood {
    network.browse(BROWSE_WAIT, fresh.unwrap_or(false), move |found| {
        let _ = app.emit("network-changed", found);
    })
}

/// A computer's disk shares, as `\\SERVER\share` paths.
#[tauri::command(async)]
pub fn network_shares(server: String, show_hidden: Option<bool>) -> Result<Vec<String>, String> {
    let name = server.trim_start_matches('\\').trim_end_matches('\\').to_owned();
    if name.is_empty() || name.contains('\\') {
        return Err(format!("{server} isn't a computer's name."));
    }
    shares(&name, show_hidden.unwrap_or(false))
        .map(|shares| shares.into_iter().map(|share| format!(r"\\{name}\{share}")).collect())
        .ok_or_else(|| format!("{name} didn't answer."))
}

/// The network locations made by "Add a network location".
#[tauri::command(async)]
pub fn network_places() -> Vec<Place> {
    let Some(folder) = known_folder(&FOLDERID_NetHood) else { return Vec::new() };
    crate::parity::in_apartment(move || Ok(places_in(Path::new(&folder), crate::parity::links::read_target))).unwrap_or_default()
}

/// Development builds only: records the dialogs asked for from now on instead of showing them
/// (`on`), or shows them again and returns what was recorded (`!on`).
#[tauri::command(async)]
pub fn network_dev_record(on: bool) -> Result<Vec<Call>, String> {
    dev_only()?;
    let mut recorded = RECORDED.lock();
    Ok(if on { recorded.replace(Vec::new()).unwrap_or_default() } else { recorded.take().unwrap_or_default() })
}

/// Development builds only: the dialogs recorded so far, which are forgotten.
#[tauri::command(async)]
pub fn network_dev_take() -> Result<Vec<Call>, String> {
    dev_only()?;
    Ok(RECORDED.lock().as_mut().map(std::mem::take).unwrap_or_default())
}

/// Development builds only: the Network location finds these computers, after `delay_ms`,
/// instead of asking the network; `None` asks the network again.
#[tauri::command(async)]
pub fn network_dev_computers(network: State<Arc<Network>>, computers: Option<Vec<String>>, delay_ms: Option<u64>) -> Result<(), String> {
    dev_only()?;
    let provider: Arc<dyn Provider> = match computers {
        Some(computers) => Arc::new(Fake { computers, delay: Duration::from_millis(delay_ms.unwrap_or(0)) }),
        None => Arc::new(Wnet),
    };
    network.set_provider(provider);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A network of two workgroups, the second of whose browsers takes `slow` to answer.
    struct Slow {
        slow: Duration,
        asked: AtomicUsize,
    }

    impl Provider for Slow {
        fn children(&self, container: Option<&Resource>) -> Result<Vec<Resource>, String> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            let item = |name: &str, kind: Kind, container: bool| Resource { name: name.into(), provider: "Microsoft Windows Network".into(), comment: format!("{name}'s"), kind, container };
            let tree: HashMap<&str, Vec<Resource>> = HashMap::from([
                ("", vec![item("Microsoft Windows Network", Kind::Network, true), item("Web Client Network", Kind::Network, true)]),
                ("Microsoft Windows Network", vec![item("FAST", Kind::Domain, true), item("SLOW", Kind::Domain, true)]),
                ("Web Client Network", vec![]),
                ("FAST", vec![item(r"\\NAS", Kind::Server, true), item(r"\\desk", Kind::Server, true)]),
                ("SLOW", vec![item(r"\\FAR", Kind::Server, true), item(r"\\nas", Kind::Server, true)]),
                (r"\\NAS", vec![item(r"\\NAS\photos", Kind::Share, false)]),
            ]);
            let key = container.map_or("", |c| c.name.as_str());
            if key == "SLOW" {
                std::thread::sleep(self.slow);
            }
            if key == "Web Client Network" {
                return Err("no browser".into());
            }
            Ok(tree.get(key).cloned().unwrap_or_default())
        }
    }

    #[test]
    fn the_walk_finds_servers_once_each_and_never_goes_into_them() {
        let provider = Slow { slow: Duration::ZERO, asked: AtomicUsize::new(0) };
        let found = Mutex::new(Vec::new());
        assert_eq!(walk(&provider, &found), Some("no browser".into()));
        let mut names: Vec<String> = found.into_inner().into_iter().map(|c| format!("{} {} {}", c.path, c.group, c.comment)).collect();
        names.sort_by_key(|name| name.to_lowercase());
        assert_eq!(names, [r"\\desk FAST \\desk's", r"\\FAR SLOW \\FAR's", r"\\NAS FAST \\NAS's"]);
        // The top, two providers, two workgroups: servers are never opened.
        assert_eq!(provider.asked.load(Ordering::Relaxed), 5);
    }

    /// A browser that takes its time never holds the Network location up: the call returns what
    /// was found within its time limit, the walk finishes on its own and is announced, and the
    /// next call within a minute gets the finished walk at once.
    #[test]
    fn a_slow_network_is_waited_for_no_longer_than_the_limit() {
        let network = Arc::new(Network::with_provider(Arc::new(Slow { slow: Duration::from_millis(1500), asked: AtomicUsize::new(0) })));
        let (send, announced) = mpsc::channel();
        let started = Instant::now();
        let early = network.browse(Duration::from_millis(300), false, move |found| send.send(found).unwrap());
        let waited = started.elapsed();
        assert!(waited < Duration::from_millis(1000), "waited {waited:?}");
        assert!(!early.complete);
        let early_names: Vec<&str> = early.computers.iter().map(|c| c.name.as_str()).collect();
        assert!(early_names.iter().all(|name| ["desk", "NAS"].contains(name)), "{early_names:?}");

        // Asking again while the walk runs joins it rather than starting another.
        let joined = network.browse(Duration::from_millis(50), false, |_| panic!("only the first walk announces"));
        assert!(!joined.complete);

        let finished = announced.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(finished.complete && finished.error.is_none());
        assert_eq!(finished.computers.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["desk", "FAR", "NAS"]);
        let again = Instant::now();
        assert_eq!(network.browse(Duration::from_millis(300), false, |_| {}), finished);
        assert!(again.elapsed() < Duration::from_millis(100), "kept, not walked again");
    }

    #[test]
    fn a_network_with_nothing_on_it_says_why() {
        struct Nothing;
        impl Provider for Nothing {
            fn children(&self, _: Option<&Resource>) -> Result<Vec<Resource>, String> {
                Err("The list of servers for this workgroup is not currently available.".into())
            }
        }
        let network = Arc::new(Network::with_provider(Arc::new(Nothing)));
        let found = network.browse(Duration::from_secs(2), false, |_| {});
        assert!(found.complete && found.computers.is_empty());
        assert_eq!(found.error.as_deref(), Some("The list of servers for this workgroup is not currently available."));
    }

    #[test]
    fn the_real_network_answers_within_its_limit() {
        // Whatever this machine's network holds: only that asking it never takes much longer
        // than the limit, and reports something either way.
        let network = Arc::new(Network::default());
        let started = Instant::now();
        let found = network.browse(Duration::from_millis(1500), false, |_| {});
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(found.computers.iter().all(|c| c.path.starts_with(r"\\")));
    }

    #[test]
    fn network_locations_are_read_from_their_shortcuts() {
        let dir = std::env::temp_dir().join(format!("kestrel-test-nethood-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Team share (nas)")).unwrap();
        std::fs::write(dir.join("Team share (nas)").join("target.lnk"), b"link").unwrap();
        std::fs::create_dir_all(dir.join("Empty")).unwrap();
        std::fs::write(dir.join("Photos.lnk"), b"link").unwrap();
        std::fs::write(dir.join("desktop.ini"), b"ini").unwrap();
        let read = |link: &Path| Some(format!("target of {}", link.parent()?.file_name()?.to_string_lossy()));
        let places = places_in(&dir, read);
        let summary: Vec<(&str, &str)> = places.iter().map(|place| (place.name.as_str(), place.target.as_str())).collect();
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(summary.len(), 3, "{summary:?}");
        assert_eq!(summary[0].0, "Empty");
        assert_eq!(summary[1], ("Photos", format!("target of {base}").as_str()));
        assert_eq!(summary[2], ("Team share (nas)", "target of Team share (nas)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dialogs_are_recorded_while_the_hook_is_on() {
        assert_eq!(drive_of(r"Z:\"), Some("Z:".into()));
        assert_eq!(drive_of(r"z:\folder"), Some("Z:".into()));
        assert_eq!(drive_of(r"\\nas\share"), None);
        *RECORDED.lock() = Some(Vec::new());
        assert!(recorded(Call { function: "WNetConnectionDialog1", subject: None }));
        assert_eq!(RECORDED.lock().take().unwrap().len(), 1);
        assert!(!recorded(Call { function: "WNetConnectionDialog1", subject: None }), "off again: shown, not recorded");
    }
}
