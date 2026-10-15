//! What the rules ask of the machine besides the index: known folders, which programs are
//! running, which cleanup tools are installed, and how full the Recycle Bin is. All of it only
//! reads; [`Tools::with`] and the plain fields of `Context` stand in for it in tests.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The cleanup tools recommendations can offer, by the name they run as.
pub const TOOL_NAMES: &[&str] = &["cargo", "pnpm", "pip", "dotnet", "go"];

/// Which cleanup tools are installed, and where.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    found: HashMap<String, PathBuf>,
}

impl Tools {
    /// Looks for each tool on the PATH (absolute folders only: a relative entry would be read
    /// against whatever folder the app runs in), and for Cargo in its usual home too.
    pub fn detect() -> Self {
        let mut found = HashMap::new();
        for name in TOOL_NAMES {
            if let Some(path) = on_path(name) {
                found.insert(name.to_string(), path);
            }
        }
        if !found.contains_key("cargo")
            && let Some(home) = std::env::var_os("CARGO_HOME").or_else(|| {
                std::env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join(".cargo").into_os_string())
            })
        {
            let cargo = PathBuf::from(home).join(r"bin\cargo.exe");
            if cargo.is_file() {
                found.insert("cargo".into(), cargo);
            }
        }
        Tools { found }
    }

    /// Tools said to be installed, for tests: each at a path named after it.
    pub fn with(names: &[&str]) -> Self {
        Tools { found: names.iter().map(|name| (name.to_string(), PathBuf::from(name))).collect() }
    }

    pub fn has(&self, name: &str) -> bool {
        self.found.contains_key(name)
    }

    /// Where the tool is, to run it by its full path.
    pub fn path(&self, name: &str) -> Option<&Path> {
        self.found.get(name).map(PathBuf::as_path)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.found.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Searches the PATH for `name` with the usual program extensions.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| [".exe", ".cmd", ".bat"].into_iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|candidate| candidate.is_file())
}

/// What the Recycle Bin holds, on every drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinContents {
    pub bytes: u64,
    pub items: u64,
}

#[cfg(windows)]
pub use real::*;

#[cfg(windows)]
mod real {
    use std::collections::HashSet;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    use windows_sys::Win32::UI::Shell::{SHGetKnownFolderPath, SHQUERYRBINFO, SHQueryRecycleBinW};
    use windows_sys::core::GUID;

    use super::BinContents;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// The path of a known folder, such as Downloads, wherever the owner moved it.
    pub fn known_folder(id: &GUID) -> Option<String> {
        let mut raw = null_mut();
        // SAFETY: `raw` receives a CoTaskMem-allocated, NUL-terminated string on success and is
        // freed below in every case, as the API requires.
        unsafe {
            let hr = SHGetKnownFolderPath(id, 0, null_mut(), &mut raw);
            let path = (hr >= 0 && !raw.is_null()).then(|| {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(raw, len))
            });
            CoTaskMemFree(raw.cast());
            path
        }
    }

    /// The long form of a path (`C:\Users\LONGNA~1` becomes `C:\Users\longname`), as the index
    /// holds names; the path itself when it can't be expanded.
    pub fn long_path(path: &str) -> String {
        let input = wide(path);
        let mut buffer = vec![0u16; 1024];
        // SAFETY: `input` is NUL-terminated and `buffer` holds the length passed.
        let len = unsafe { GetLongPathNameW(input.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len == 0 || len >= buffer.len() {
            return path.to_owned();
        }
        String::from_utf16_lossy(&buffer[..len])
    }

    /// The executable names of the running processes, in lower case.
    pub fn running_programs() -> HashSet<String> {
        let mut names = HashSet::new();
        // SAFETY: the snapshot handle is checked and closed; `entry` is a valid, sized
        // PROCESSENTRY32W for every call that fills it.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return names;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                names.insert(String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase());
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        names
    }

    /// How much the Recycle Bin holds on every drive. Only reads.
    pub fn recycle_bin() -> Option<BinContents> {
        let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, i64Size: 0, i64NumItems: 0 };
        // SAFETY: a null root asks about every drive; `info` is valid and sized.
        let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
        (hr >= 0).then(|| BinContents { bytes: info.i64Size.max(0) as u64, items: info.i64NumItems.max(0) as u64 })
    }

    /// Lowers this thread's I/O and memory priority, so reading files for hashes gives way to
    /// everything else the machine does.
    pub fn background_priority() {
        // SAFETY: plain call on the current thread's pseudo-handle.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
}

/// For callers that only want to know whether one of `programs` runs.
pub fn any_running(running: &HashSet<String>, programs: &[&str]) -> bool {
    programs.iter().any(|program| running.contains(&program.to_lowercase()))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_seen_running() {
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(running_programs().contains(&name), "{name}");
        assert!(any_running(&running_programs(), &[&name.to_uppercase()]));
    }

    #[test]
    fn tools_said_to_be_installed_are_found_by_name() {
        let tools = Tools::with(&["cargo"]);
        assert!(tools.has("cargo") && !tools.has("pnpm"));
        assert_eq!(tools.path("cargo"), Some(Path::new("cargo")));
        // The test itself runs under Cargo, so the real detection finds it.
        assert!(Tools::detect().has("cargo"));
    }
}
