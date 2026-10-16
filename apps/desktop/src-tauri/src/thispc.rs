//! This PC: the user's folders and every drive, with what Explorer shows of each (name, label
//! and letter, file system, space used and free), plus mapped network drives, connected or not.
//!
//! Asking a drive about itself can take a long time: a network drive whose server is away
//! answers after half a minute, a disc drive spins up first. So every drive is asked on a
//! thread of its own and `this_pc` waits for the answers only briefly; a drive that has not
//! answered by then is listed as still being read, and its details follow in a `drive-details`
//! event when they come. A drive whose question is still out is not asked again meanwhile.
//!
//! A background thread compares the drive letters (and the remembered network connections)
//! with what they were every second and a half and emits `drives-changed` when a drive comes
//! or goes, so the page and the folder tree follow USB sticks and mapped drives.

use std::collections::{HashMap, HashSet};
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Window};
use windows_sys::Win32::Foundation::{ERROR_CONNECTION_UNAVAIL, ERROR_MORE_DATA, ERROR_NOT_READY, GetLastError, HANDLE, NO_ERROR};
use windows_sys::Win32::NetworkManagement::WNet::{
    NETRESOURCEW, RESOURCE_REMEMBERED, RESOURCETYPE_DISK, WNetCloseEnum, WNetEnumResourceW, WNetGetConnectionW,
    WNetOpenEnumW,
};
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode};

use crate::places;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// How long `this_pc` waits for the drives' answers before listing the slow ones as being read.
const DETAILS_WAIT: Duration = Duration::from_millis(800);
/// How often the drive letters are compared with what they were.
const WATCH_EVERY: Duration = Duration::from_millis(1500);

/// A drive as This PC shows it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Drive {
    /// The root, such as `C:\`.
    pub path: String,
    pub letter: String,
    /// `fixed`, `removable`, `optical`, `network` or `ram`.
    pub kind: &'static str,
    /// The volume's own label; empty when it has none.
    pub label: String,
    /// `NTFS`, `FAT32`, `exFAT`, `ReFS`, `CDFS`, `UDF`...; empty when unknown.
    pub file_system: String,
    pub total: u64,
    /// Free space this account can use.
    pub free: u64,
    /// For a mapped network drive, the share it stands for (`\\server\share`).
    pub remote: Option<String>,
    /// `ready`; `reading` while its answer is on its way; `no-media` for an empty card reader
    /// or disc drive; `disconnected` for a remembered network drive not connected now;
    /// `unavailable` when it answered with an error.
    pub state: &'static str,
    /// The name Explorer gives it: `Windows (C:)`, `Local Disk (D:)`, `USB Drive (E:)`,
    /// `projects (\\nas) (Z:)`.
    pub name: String,
}

#[derive(Serialize)]
pub struct ThisPc {
    /// Desktop, Documents, Downloads, Music, Pictures and Videos, wherever they live (OneDrive
    /// included), in Explorer's order.
    pub folders: Vec<places::Folder>,
    pub drives: Vec<Drive>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn text_of(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

fn kind_of(drive_type: u32) -> Option<&'static str> {
    Some(match drive_type {
        DRIVE_FIXED => "fixed",
        DRIVE_REMOVABLE => "removable",
        DRIVE_REMOTE => "network",
        DRIVE_CDROM => "optical",
        DRIVE_RAMDISK => "ram",
        _ => return None,
    })
}

/// What Explorer calls a drive: its label, or a name for its kind, then its letter. A network
/// drive is named after its share and server.
pub fn drive_name(kind: &str, label: &str, letter: &str, remote: Option<&str>) -> String {
    if let Some(remote) = remote.filter(|_| kind == "network") {
        let trimmed = remote.trim_end_matches('\\');
        if let Some((server, share)) = trimmed.rsplit_once('\\').filter(|(server, _)| server.len() > 2) {
            return format!("{share} ({server}) ({letter}:)");
        }
        return format!("{trimmed} ({letter}:)");
    }
    let base = match (label.is_empty(), kind) {
        (false, _) => label,
        (true, "removable") => "USB Drive",
        (true, "optical") => "DVD Drive",
        (true, "network") => "Network Drive",
        (true, "ram") => "RAM Disk",
        (true, _) => "Local Disk",
    };
    format!("{base} ({letter}:)")
}

/// The share a drive letter is mapped to, and whether it is connected now. `None` if the
/// letter is not a network drive.
fn connection(letter: &str) -> Option<(String, bool)> {
    let local = wide(&format!("{letter}:"));
    let mut buffer = vec![0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `local` is NUL-terminated; `buffer` holds `len` characters.
    let result = unsafe { WNetGetConnectionW(local.as_ptr(), buffer.as_mut_ptr(), &mut len) };
    match result {
        NO_ERROR => Some((text_of(&buffer), true)),
        ERROR_CONNECTION_UNAVAIL => Some((text_of(&buffer), false)),
        _ => None,
    }
}

/// The network drives Windows remembers, connected or not, as letter and share.
pub fn remembered_connections() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut handle: HANDLE = null_mut();
    // SAFETY: plain enumeration; the handle is closed below, and every buffer is sized as passed.
    unsafe {
        if WNetOpenEnumW(RESOURCE_REMEMBERED, RESOURCETYPE_DISK, 0, null(), &mut handle) != NO_ERROR {
            return found;
        }
        // NETRESOURCEW holds pointers into the same buffer, which is aligned for them.
        let mut buffer = vec![0u64; 4096];
        loop {
            let mut count = u32::MAX;
            let mut size = (buffer.len() * 8) as u32;
            let result = WNetEnumResourceW(handle, &mut count, buffer.as_mut_ptr().cast(), &mut size);
            if result == ERROR_MORE_DATA {
                buffer = vec![0u64; size as usize / 8 + 1];
                continue;
            }
            if result != NO_ERROR {
                break;
            }
            let resources = std::slice::from_raw_parts(buffer.as_ptr().cast::<NETRESOURCEW>(), count as usize);
            for resource in resources {
                let read = |text: *mut u16| {
                    if text.is_null() {
                        return String::new();
                    }
                    let len = (0..).take_while(|&i| *text.add(i) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
                };
                let local = read(resource.lpLocalName);
                let remote = read(resource.lpRemoteName);
                if let Some(letter) = local.strip_suffix(':').filter(|l| l.len() == 1) {
                    found.push((letter.to_ascii_uppercase(), remote));
                }
            }
        }
        WNetCloseEnum(handle);
    }
    found
}

/// The letters of every drive Windows has now, plus remembered network drives not connected.
fn letters() -> Vec<(String, u32)> {
    // SAFETY: plain FFI call with no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut letters: Vec<(String, u32)> = (0..26u8)
        .filter(|bit| mask >> bit & 1 != 0)
        .filter_map(|bit| {
            let letter = ((b'A' + bit) as char).to_string();
            let root = wide(&format!("{letter}:\\"));
            // SAFETY: `root` is NUL-terminated.
            let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
            kind_of(drive_type).map(|_| (letter, drive_type))
        })
        .collect();
    for (letter, _) in remembered_connections() {
        if !letters.iter().any(|(known, _)| *known == letter) {
            letters.push((letter, DRIVE_REMOTE));
        }
    }
    letters.sort();
    letters
}

/// A drive's details, asked of Windows. Blocks for as long as the drive takes to answer.
pub fn probe(letter: &str, drive_type: u32) -> Drive {
    let kind = kind_of(drive_type).unwrap_or("fixed");
    let path = format!("{letter}:\\");
    let mut drive = Drive {
        path: path.clone(),
        letter: letter.to_owned(),
        kind,
        label: String::new(),
        file_system: String::new(),
        total: 0,
        free: 0,
        remote: None,
        state: "ready",
        name: String::new(),
    };
    if kind == "network"
        && let Some((remote, connected)) = connection(letter)
    {
        drive.remote = Some(remote);
        if !connected {
            drive.state = "disconnected";
        }
    }
    if drive.state == "ready" {
        let root = wide(&path);
        let mut label = [0u16; 261];
        let mut file_system = [0u16; 261];
        // SAFETY: `root` is NUL-terminated and each buffer holds the length passed. The error
        // mode keeps Windows from asking for a disk on this thread.
        let read = unsafe {
            SetThreadErrorMode(SEM_FAILCRITICALERRORS, null_mut());
            GetVolumeInformationW(
                root.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                null_mut(),
                null_mut(),
                null_mut(),
                file_system.as_mut_ptr(),
                file_system.len() as u32,
            )
        };
        if read == 0 {
            // SAFETY: plain FFI call.
            let error = unsafe { GetLastError() };
            drive.state = if error == ERROR_NOT_READY && matches!(kind, "removable" | "optical") { "no-media" } else { "unavailable" };
        } else {
            drive.label = text_of(&label);
            drive.file_system = text_of(&file_system);
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: `root` is NUL-terminated and the out-pointers are valid.
            if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, null_mut()) } != 0 {
                drive.total = total;
                drive.free = free;
            }
        }
    }
    drive.name = drive_name(kind, &drive.label, letter, drive.remote.as_deref());
    drive
}

/// What is known of a drive before it has answered.
fn placeholder(letter: &str, drive_type: u32) -> Drive {
    let kind = kind_of(drive_type).unwrap_or("fixed");
    Drive {
        path: format!("{letter}:\\"),
        letter: letter.to_owned(),
        kind,
        label: String::new(),
        file_system: String::new(),
        total: 0,
        free: 0,
        remote: None,
        state: "reading",
        name: drive_name(kind, "", letter, None),
    }
}

type Probe = fn(&str, u32) -> Drive;
type Late = Arc<dyn Fn(Drive) + Send + Sync>;

/// Asks drives about themselves, each on its own thread, and remembers the answers.
pub struct Drives {
    probe: Probe,
    known: Mutex<HashMap<String, Drive>>,
    asking: Mutex<HashSet<String>>,
    /// Told of answers that came after `details` stopped waiting.
    late: OnceLock<Late>,
}

impl Default for Drives {
    fn default() -> Self {
        Self::with_probe(probe)
    }
}

impl Drives {
    fn with_probe(probe: Probe) -> Self {
        Self { probe, known: Mutex::default(), asking: Mutex::default(), late: OnceLock::new() }
    }

    /// Where answers that come late are sent; the app emits them as `drive-details`.
    pub fn on_late(&self, late: impl Fn(Drive) + Send + Sync + 'static) {
        let _ = self.late.set(Arc::new(late));
    }

    /// The details of these drives, waiting at most `wait` for them. A drive that has not
    /// answered in time is given as last known, or as being read.
    fn details(self: &Arc<Self>, letters: &[(String, u32)], wait: Duration) -> Vec<Drive> {
        let (sender, answers) = mpsc::channel::<Drive>();
        let started = Instant::now();
        let mut waiting = 0;
        for (letter, drive_type) in letters {
            if !self.asking.lock().insert(letter.clone()) {
                continue;
            }
            waiting += 1;
            let (this, sender, asked, drive_type) = (self.clone(), sender.clone(), letter.clone(), *drive_type);
            let spawned = std::thread::Builder::new().name(format!("kestrel-drive-{letter}")).spawn(move || {
                let drive = (this.probe)(&asked, drive_type);
                this.known.lock().insert(asked.clone(), drive.clone());
                this.asking.lock().remove(&asked);
                // The page stopped waiting: tell it separately.
                if (sender.send(drive.clone()).is_err() || started.elapsed() >= wait)
                    && let Some(late) = this.late.get()
                {
                    late(drive);
                }
            });
            if spawned.is_err() {
                self.asking.lock().remove(letter);
                waiting -= 1;
            }
        }
        drop(sender);
        let deadline = started + wait;
        for _ in 0..waiting {
            let left = deadline.saturating_duration_since(Instant::now());
            if answers.recv_timeout(left).is_err() {
                break;
            }
        }
        // A drive asked again keeps its last answer until the new one comes.
        let known = self.known.lock();
        letters
            .iter()
            .map(|(letter, drive_type)| known.get(letter).cloned().unwrap_or_else(|| placeholder(letter, *drive_type)))
            .collect()
    }
}

/// The user's folders, in Explorer's order.
fn folders() -> Vec<places::Folder> {
    const ORDER: [&str; 6] = ["desktop", "documents", "downloads", "music", "pictures", "videos"];
    let mut folders = places::places().folders;
    folders.sort_by_key(|folder| ORDER.iter().position(|kind| *kind == folder.kind).unwrap_or(ORDER.len()));
    folders
}

/// This PC: the user's folders and every drive.
#[tauri::command(async)]
pub fn this_pc(drives: tauri::State<Arc<Drives>>) -> ThisPc {
    ThisPc { folders: folders(), drives: drives.details(&letters(), DETAILS_WAIT) }
}

/// Ejects a removable drive or disc through the drive's own Windows menu command, which the
/// shell helper runs (shell extensions never load into Kestrel; see shellmenu.rs).
#[tauri::command(async)]
pub fn eject_drive(window: Window, path: String) -> Result<(), String> {
    let letter = path.trim_end_matches(['\\', '/']).trim_end_matches(':');
    let letter = letter.to_ascii_uppercase();
    let drive = letters().into_iter().find(|(known, _)| *known == letter);
    match drive.and_then(|(_, drive_type)| kind_of(drive_type)) {
        Some("removable" | "optical") => {}
        Some(_) => return Err(format!("{letter}: can't be ejected.")),
        None => return Err(format!("There's no drive {letter}: now.")),
    }
    let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
    crate::shellmenu::invoke_verb("eject", &[format!("{letter}:\\")], owner)
}

/// Emits `drives-changed` whenever a drive letter appears or goes, or a network drive is
/// mapped or forgotten. Runs for the life of the app.
pub fn watch(app: AppHandle) {
    let spawned = std::thread::Builder::new().name("kestrel-drive-watch".into()).spawn(move || {
        let current = || {
            // SAFETY: plain FFI call with no arguments.
            let mask = unsafe { GetLogicalDrives() };
            let mut remembered = remembered_connections();
            remembered.sort();
            (mask, remembered)
        };
        let mut last = current();
        loop {
            std::thread::sleep(WATCH_EVERY);
            let now = current();
            if now != last {
                last = now;
                let _ = app.emit("drives-changed", ());
            }
        }
    });
    if let Err(error) = spawned {
        eprintln!("Drives coming and going won't be noticed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_are_named_as_explorer_names_them() {
        assert_eq!(drive_name("fixed", "Windows", "C", None), "Windows (C:)");
        assert_eq!(drive_name("fixed", "", "D", None), "Local Disk (D:)");
        assert_eq!(drive_name("removable", "", "E", None), "USB Drive (E:)");
        assert_eq!(drive_name("optical", "", "F", None), "DVD Drive (F:)");
        assert_eq!(drive_name("network", "Data", "Z", Some(r"\\nas\projects")), r"projects (\\nas) (Z:)");
        assert_eq!(drive_name("network", "", "Y", None), "Network Drive (Y:)");
    }

    /// Read-only: asks the system drive about itself, as This PC does.
    #[test]
    fn the_system_drive_reports_its_file_system_and_space() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = system.trim_end_matches(':').to_owned();
        let drives = Arc::new(Drives::default());
        let listed = drives.details(&letters(), Duration::from_secs(10));
        let drive = listed.iter().find(|drive| drive.letter == letter).expect("the system drive is listed");
        assert_eq!((drive.kind, drive.state), ("fixed", "ready"), "{drive:?}");
        assert!(!drive.file_system.is_empty(), "{drive:?}");
        assert!(drive.total > 0 && drive.free <= drive.total, "{drive:?}");
        assert!(drive.name.ends_with(&format!("({letter}:)")), "{drive:?}");
    }

    fn slow(letter: &str, drive_type: u32) -> Drive {
        if letter == "Z" {
            std::thread::sleep(Duration::from_millis(1500));
        }
        Drive { state: "ready", total: 100, free: 40, file_system: "NTFS".into(), ..placeholder(letter, drive_type) }
    }

    /// A drive that takes its time (an unreachable server) is listed as being read rather than
    /// holding up the rest, and its answer is passed on when it comes.
    #[test]
    fn a_slow_drive_never_holds_up_the_others() {
        let drives = Arc::new(Drives::with_probe(slow));
        let (late, arrived) = mpsc::channel();
        let late = Mutex::new(late);
        drives.on_late(move |drive| {
            let _ = late.lock().send(drive);
        });
        let letters = [("C".to_owned(), DRIVE_FIXED), ("Z".to_owned(), DRIVE_REMOTE)];
        let started = Instant::now();
        let first = drives.details(&letters, Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_millis(1200), "took {:?}", started.elapsed());
        assert_eq!(first[0].state, "ready");
        assert_eq!((first[1].letter.as_str(), first[1].state), ("Z", "reading"));
        // Asked again while the first question is out: no second question, no wait.
        let again = drives.details(&letters, Duration::from_millis(300));
        assert_eq!(again[1].state, "reading");
        let answer = arrived.recv_timeout(Duration::from_secs(5)).expect("the late answer is passed on");
        assert_eq!((answer.letter.as_str(), answer.state), ("Z", "ready"));
        assert_eq!(drives.details(&letters, Duration::from_millis(300))[1].free, 40);
    }
}
