//! WSL: the Linux distributions installed for this account, shown in This PC and the sidebar
//! and browsed like folders through WSL's own share (`\\wsl.localhost\<distro>`, or `\\wsl$`
//! before Windows had that name).
//!
//! Distributions are found in the registry (`HKCU\Software\Microsoft\Windows\CurrentVersion\
//! Lxss`, one key per distribution), which starts nothing: asking the share would start WSL's
//! virtual machine. Nothing here touches a distribution until the owner opens it.
//!
//! Inside one, folders are listed from disk by the crawler (the index holds no WSL files), with
//! Linux's conventions: names starting with a dot are hidden (shown with "Show hidden items",
//! as `ls -a` would), names holding characters Windows forbids are shown as Linux has them (WSL
//! hands them over as private-use characters, `U+F000` plus the character, which the window
//! turns back), and each item's permissions (`rwxr-xr-x`) are read from WSL's own metadata
//! where the share gives it (`wsl_modes`). Open in Terminal there opens the distribution's shell
//! in that folder.
//!
//! Development builds let the end-to-end tests stand scratch folders in for distributions
//! (`wsl_dev_distros`); no test ever starts one.

use std::sync::OnceLock;

use kestrel_index::walk;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Listing, SortSpec};
use crate::projects::launch::{self, Launch};
use crate::visibility::Visibility;

const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
/// `State`: the distribution is installed (others are being installed or removed).
const INSTALLED: u32 = 1;
/// `Flags`: the distribution runs in WSL 2's virtual machine.
const VM_MODE: u32 = 0x8;
/// The first build of Windows with `\\wsl.localhost`.
const LOCALHOST_BUILD: u32 = 21_354;
const HIDDEN: u32 = 0x2;

/// A distribution, as This PC and the sidebar list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Distro {
    pub name: String,
    /// Its root: `\\wsl.localhost\Ubuntu`.
    pub path: String,
    /// 1 or 2.
    #[serde(default)]
    pub version: u8,
    /// The one `wsl` starts without `-d`.
    #[serde(default)]
    pub default: bool,
}

/// What the registry reader needs, so it can be tried on made-up keys.
pub trait Registry {
    /// The names of the subkeys of `key` (under HKEY_CURRENT_USER).
    fn subkeys(&self, key: &str) -> Vec<String>;
    fn string(&self, key: &str, value: &str) -> Option<String>;
    fn number(&self, key: &str, value: &str) -> Option<u32>;
}

/// The share distributions are reached through on this build of Windows.
pub fn share_for_build(build: u32) -> &'static str {
    if build >= LOCALHOST_BUILD { r"\\wsl.localhost" } else { r"\\wsl$" }
}

/// The distributions the registry lists, by name, installed ones only.
pub fn read(registry: &dyn Registry, share: &str) -> Vec<Distro> {
    let default = registry.string(LXSS, "DefaultDistribution").map(|id| id.to_lowercase());
    let mut found: Vec<Distro> = registry
        .subkeys(LXSS)
        .into_iter()
        .filter_map(|id| {
            let key = format!(r"{LXSS}\{id}");
            let name = registry.string(&key, "DistributionName")?.trim().to_owned();
            // A distribution being installed or removed has another state; one without a
            // state is taken as installed, as older WSL wrote none.
            if name.is_empty() || registry.number(&key, "State").is_some_and(|state| state != INSTALLED) {
                return None;
            }
            let version = if registry.number(&key, "Flags").unwrap_or(0) & VM_MODE != 0 { 2 } else { 1 };
            Some(Distro { path: format!(r"{share}\{name}"), name, version, default: default.as_deref() == Some(&id.to_lowercase()) })
        })
        .collect();
    found.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    found
}

/// The registry itself, read only.
struct Real;

mod native {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey, RegEnumKeyExW,
        RegOpenKeyExW, RegQueryValueExW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// An open key, closed when dropped.
    pub struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the key was opened by `open` and is closed once, here.
            unsafe { RegCloseKey(self.0) };
        }
    }

    pub fn open(path: &str, machine: bool) -> Option<Key> {
        let name = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        let root = if machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
        // SAFETY: `name` is NUL-terminated and `key` receives the opened key.
        let status = unsafe { RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS).then_some(Key(key))
    }

    impl Key {
        pub fn subkeys(&self) -> Vec<String> {
            let mut names = Vec::new();
            for index in 0.. {
                let mut buffer = [0u16; 256];
                let mut len = buffer.len() as u32;
                // SAFETY: `buffer` holds `len` characters; the other out-parameters are optional.
                let status = unsafe {
                    RegEnumKeyExW(self.0, index, buffer.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
                };
                if status != ERROR_SUCCESS {
                    break;
                }
                names.push(String::from_utf16_lossy(&buffer[..len as usize]));
            }
            names
        }

        fn raw(&self, value: &str) -> Option<(u32, Vec<u8>)> {
            let name = wide(value);
            let mut kind = 0u32;
            let mut size = 0u32;
            // SAFETY: asks for the value's size only; every pointer is valid for the call.
            let status = unsafe { RegQueryValueExW(self.0, name.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size) };
            if status != ERROR_SUCCESS || size > 64 * 1024 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            // SAFETY: `data` holds `size` bytes, as the call is told.
            let status = unsafe { RegQueryValueExW(self.0, name.as_ptr(), std::ptr::null(), &mut kind, data.as_mut_ptr(), &mut size) };
            data.truncate(size as usize);
            (status == ERROR_SUCCESS).then_some((kind, data))
        }

        pub fn string(&self, value: &str) -> Option<String> {
            let (kind, data) = self.raw(value)?;
            if kind != REG_SZ && kind != REG_EXPAND_SZ {
                return None;
            }
            let units: Vec<u16> = data.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
            let end = units.iter().position(|&unit| unit == 0).unwrap_or(units.len());
            Some(String::from_utf16_lossy(&units[..end]))
        }

        pub fn number(&self, value: &str) -> Option<u32> {
            let (kind, data) = self.raw(value)?;
            (kind == REG_DWORD && data.len() == 4).then(|| u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        }
    }
}

impl Registry for Real {
    fn subkeys(&self, key: &str) -> Vec<String> {
        native::open(key, false).map(|key| key.subkeys()).unwrap_or_default()
    }
    fn string(&self, key: &str, value: &str) -> Option<String> {
        native::open(key, false)?.string(value)
    }
    fn number(&self, key: &str, value: &str) -> Option<u32> {
        native::open(key, false)?.number(value)
    }
}

/// This build of Windows, from the registry.
fn windows_build() -> u32 {
    native::open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", true)
        .and_then(|key| key.string("CurrentBuildNumber"))
        .and_then(|build| build.trim().parse().ok())
        .unwrap_or(0)
}

/// Distributions the tests stand in.
fn standins() -> &'static Mutex<Option<Vec<Distro>>> {
    static STANDINS: OnceLock<Mutex<Option<Vec<Distro>>>> = OnceLock::new();
    STANDINS.get_or_init(Mutex::default)
}

/// The installed distributions; none when WSL is not installed.
pub fn distros() -> Vec<Distro> {
    if let Some(standing) = standins().lock().clone() {
        return standing;
    }
    read(&Real, share_for_build(windows_build()))
}

/// The distribution a path is in, and the path inside it with Linux's separators (`/home/me`).
pub fn locate(path: &str) -> Option<(Distro, String)> {
    let inside = |root: &str| -> Option<String> {
        let root = root.trim_end_matches('\\');
        let start = path.get(..root.len()).filter(|start| start.eq_ignore_ascii_case(root))?;
        let rest = &path[start.len()..];
        if !rest.is_empty() && !rest.starts_with('\\') {
            return None;
        }
        let linux = rest.trim_end_matches('\\').replace('\\', "/");
        Some(if linux.is_empty() { "/".to_owned() } else { linux })
    };
    if let Some(standing) = standins().lock().as_ref() {
        return standing.iter().find_map(|distro| inside(&distro.path).map(|linux| (distro.clone(), linux)));
    }
    // Any path on WSL's share is a distribution's, listed in the registry or not.
    for share in [r"\\wsl.localhost\", r"\\wsl$\"] {
        let Some(rest) = path.get(..share.len()).filter(|start| start.eq_ignore_ascii_case(share)).map(|_| &path[share.len()..]) else { continue };
        let name = rest.split('\\').next().filter(|name| !name.is_empty())?;
        let distro = Distro { name: name.to_owned(), path: format!("{}{name}", &path[..share.len()]), version: 0, default: false };
        return inside(&distro.path.clone()).map(|linux| (distro, linux));
    }
    None
}

/// Lists a folder inside a distribution, hiding dot files as Linux does. `None` if the path is
/// not in one.
pub fn list(engine: &Engine, path: &str, spec: SortSpec, visibility: Visibility) -> Option<Result<Listing, String>> {
    locate(path)?;
    let dir = crate::engine::normalize(path);
    let listed = walk::list(&dir).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => crate::access::LOCKED.to_owned(),
        _ => format!("Can't open {dir}: {error}"),
    });
    Some(listed.map(|mut items| {
        for item in &mut items {
            if item.name.starts_with('.') {
                item.meta.attrs |= HIDDEN;
            }
        }
        engine.show_live(dir, items, spec, visibility)
    }))
}

/// Open in Terminal inside a distribution: its shell, in that folder.
pub fn terminal(folder: &str) -> Option<Launch> {
    let (distro, linux) = locate(folder)?;
    // Windows Terminal reads `;` as the start of another command.
    let shell = format!("wsl.exe -d {} --cd {}", launch::quote(&distro.name), launch::quote(&linux)).replace(';', "\\;");
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\".to_owned());
    Some(match launch::terminal_path() {
        Some(wt) => Launch { file: wt.display().to_string(), params: shell, dir: home },
        None => Launch { file: "wsl.exe".to_owned(), params: shell.trim_start_matches("wsl.exe ").to_owned(), dir: home },
    })
}

// ---------- permissions ----------

/// `FileStatLxInformation`: what WSL keeps of a file's Linux metadata.
const FILE_STAT_LX_INFORMATION: u32 = 70;
const LX_HAS_MODE: u32 = 0x4;

#[repr(C)]
#[derive(Default)]
struct StatLx {
    file_id: i64,
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    allocation_size: i64,
    end_of_file: i64,
    file_attributes: u32,
    reparse_tag: u32,
    number_of_links: u32,
    effective_access: u32,
    lx_flags: u32,
    lx_uid: u32,
    lx_gid: u32,
    lx_mode: u32,
    lx_device_id_major: u32,
    lx_device_id_minor: u32,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: *mut core::ffi::c_void,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *mut core::ffi::c_void,
    security_quality_of_service: *mut core::ffi::c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: isize,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationByName(
        attributes: *const ObjectAttributes,
        status: *mut IoStatusBlock,
        information: *mut core::ffi::c_void,
        length: u32,
        class: u32,
    ) -> i32;
}

/// `OBJ_CASE_INSENSITIVE`.
const CASE_INSENSITIVE: u32 = 0x40;

/// The NT name of a path: `\??\C:\x`, or `\??\UNC\server\share\x`.
fn nt_path(path: &str) -> String {
    match path.strip_prefix(r"\\") {
        Some(share) => format!(r"\??\UNC\{share}"),
        None => format!(r"\??\{path}"),
    }
}

/// A file's Linux mode bits, read by name (nothing is opened), when its file system keeps them.
fn lx_mode(path: &str) -> Option<u32> {
    let mut name: Vec<u16> = nt_path(path).encode_utf16().collect();
    let bytes = u16::try_from(name.len() * 2).ok()?;
    let unicode = UnicodeString { length: bytes, maximum_length: bytes, buffer: name.as_mut_ptr() };
    let attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: std::ptr::null_mut(),
        object_name: &unicode,
        attributes: CASE_INSENSITIVE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut status = IoStatusBlock { status: 0, information: 0 };
    let mut stat = StatLx::default();
    // SAFETY: every structure is laid out as ntdll expects and outlives the call; `stat` is as
    // large as the information class fills, and `name` stays alive while `unicode` points at it.
    let result = unsafe {
        NtQueryInformationByName(&attributes, &mut status, (&mut stat as *mut StatLx).cast(), size_of::<StatLx>() as u32, FILE_STAT_LX_INFORMATION)
    };
    (result >= 0 && stat.lx_flags & LX_HAS_MODE != 0).then_some(stat.lx_mode)
}

/// Mode bits as `ls -l` shows them: `drwxr-xr-x`.
pub fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut text = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        text.push(if bits & 4 != 0 { 'r' } else { '-' });
        text.push(if bits & 2 != 0 { 'w' } else { '-' });
        let exec = bits & 1 != 0;
        let special = match shift {
            6 => mode & 0o4000 != 0,
            3 => mode & 0o2000 != 0,
            _ => mode & 0o1000 != 0,
        };
        text.push(match (exec, special, shift) {
            (true, true, 0) => 't',
            (false, true, 0) => 'T',
            (true, true, _) => 's',
            (false, true, _) => 'S',
            (true, false, _) => 'x',
            (false, false, _) => '-',
        });
    }
    text
}

/// The installed distributions, for This PC and the sidebar.
#[tauri::command(async)]
pub fn wsl_distros() -> Vec<Distro> {
    distros()
}

/// Each item's Linux permissions (`-rw-r--r--`), where WSL's share gives them; `None` for the
/// others. Only paths inside a distribution are asked about.
#[tauri::command(async)]
pub fn wsl_modes(paths: Vec<String>) -> Vec<Option<String>> {
    paths.iter().map(|path| locate(path).and_then(|_| lx_mode(path)).map(mode_string)).collect()
}

/// Stands scratch folders in for distributions (development builds only); `None` goes back to
/// the registry.
#[tauri::command]
pub fn wsl_dev_distros(distros: Option<Vec<Distro>>) -> Result<(), String> {
    launch::development_only()?;
    *standins().lock() = distros;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Made-up registry keys.
    #[derive(Default)]
    struct Fake {
        keys: HashMap<String, Vec<String>>,
        strings: HashMap<(String, String), String>,
        numbers: HashMap<(String, String), u32>,
    }

    impl Registry for Fake {
        fn subkeys(&self, key: &str) -> Vec<String> {
            self.keys.get(key).cloned().unwrap_or_default()
        }
        fn string(&self, key: &str, value: &str) -> Option<String> {
            self.strings.get(&(key.to_owned(), value.to_owned())).cloned()
        }
        fn number(&self, key: &str, value: &str) -> Option<u32> {
            self.numbers.get(&(key.to_owned(), value.to_owned())).copied()
        }
    }

    fn distro(fake: &mut Fake, id: &str, name: &str, state: Option<u32>, flags: u32) {
        let key = format!(r"{LXSS}\{id}");
        fake.keys.entry(LXSS.to_owned()).or_default().push(id.to_owned());
        fake.strings.insert((key.clone(), "DistributionName".into()), name.into());
        fake.strings.insert((key.clone(), "BasePath".into()), format!(r"C:\Users\me\AppData\Local\Packages\{name}\LocalState"));
        if let Some(state) = state {
            fake.numbers.insert((key.clone(), "State".into()), state);
        }
        fake.numbers.insert((key, "Flags".into()), flags);
    }

    #[test]
    fn distributions_are_read_from_the_lxss_key() {
        let mut fake = Fake::default();
        distro(&mut fake, "{AAAA}", "Ubuntu-24.04", Some(1), 0xF);
        distro(&mut fake, "{BBBB}", "Debian", Some(1), 0x7);
        distro(&mut fake, "{CCCC}", "Installing", Some(3), 0xF);
        distro(&mut fake, "{DDDD}", "Old", None, 0x7);
        fake.strings.insert((LXSS.to_owned(), "DefaultDistribution".into()), "{aaaa}".into());
        let found = read(&fake, r"\\wsl.localhost");
        assert_eq!(found, [
            Distro { name: "Ubuntu-24.04".into(), path: r"\\wsl.localhost\Ubuntu-24.04".into(), version: 2, default: true },
            Distro { name: "Debian".into(), path: r"\\wsl.localhost\Debian".into(), version: 1, default: false },
            Distro { name: "Old".into(), path: r"\\wsl.localhost\Old".into(), version: 1, default: false },
        ]);
        // Without WSL there is no key, and nothing to list.
        assert!(read(&Fake::default(), r"\\wsl$").is_empty());
    }

    #[test]
    fn the_share_depends_on_the_build() {
        assert_eq!(share_for_build(26_200), r"\\wsl.localhost");
        assert_eq!(share_for_build(19_045), r"\\wsl$");
        assert!(windows_build() >= 10_000, "this machine's build is read");
    }

    #[test]
    fn this_machines_registry_is_read_without_failing() {
        // Whatever is installed here: the real key is read, and every entry is well formed.
        for distro in read(&Real, r"\\wsl.localhost") {
            assert!(distro.path.ends_with(&distro.name));
        }
    }

    #[test]
    fn paths_inside_a_distribution_are_located() {
        let (distro, linux) = locate(r"\\wsl.localhost\Ubuntu\home\me").unwrap();
        assert_eq!((distro.name.as_str(), distro.path.as_str(), linux.as_str()), ("Ubuntu", r"\\wsl.localhost\Ubuntu", "/home/me"));
        assert_eq!(locate(r"\\WSL$\Debian").unwrap().1, "/");
        assert_eq!(locate(r"\\wsl.localhost\Debian\").unwrap().1, "/");
        assert!(locate(r"\\wsl.localhost\").is_none());
        assert!(locate(r"\\server\share\x").is_none());
        assert!(locate(r"C:\Users").is_none());
    }

    #[test]
    fn the_terminal_opens_the_distributions_shell_there() {
        let launch = terminal(r"\\wsl.localhost\Ubuntu\home\me\my project").unwrap();
        assert!(launch.params.ends_with(r#"wsl.exe -d Ubuntu --cd "/home/me/my project""#) || launch.params == r#"-d Ubuntu --cd "/home/me/my project""#, "{}", launch.params);
        assert!(terminal(r"C:\Users").is_none());
    }

    #[test]
    fn modes_read_as_ls_shows_them() {
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o041777), "drwxrwxrwt");
        assert_eq!(mode_string(0o100600), "-rw-------");
    }

    #[test]
    fn files_without_linux_metadata_have_no_mode() {
        let path = std::env::temp_dir().join(format!("kestrel-wsl-mode-{}.txt", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(lx_mode(path.to_str().unwrap()), None);
        assert_eq!(nt_path(r"C:\x"), r"\??\C:\x");
        assert_eq!(nt_path(r"\\wsl.localhost\Ubuntu\x"), r"\??\UNC\wsl.localhost\Ubuntu\x");
        std::fs::remove_file(&path).unwrap();
    }
}
