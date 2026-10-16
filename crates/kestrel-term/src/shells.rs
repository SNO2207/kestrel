//! The shells installed: PowerShell 7 when it is, Windows PowerShell, Command Prompt, and each
//! WSL distribution. The first one found is the default: PowerShell 7 if installed, else
//! Windows PowerShell.
//!
//! Programs are found where their installers put them (and PowerShell 7 also on the PATH,
//! absolute entries only), never in the folder a terminal opens in, so a project folder can't
//! slip in a `pwsh.exe` of its own.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pty::{Arg, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    PowerShell,
    Cmd,
    Wsl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Shell {
    /// `pwsh`, `powershell`, `cmd`, or `wsl:<distribution>`.
    pub id: String,
    /// As menus show it: "PowerShell 7", "Windows PowerShell", "Command Prompt", "Ubuntu (WSL)".
    pub name: String,
    pub kind: Kind,
    pub program: PathBuf,
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

/// `name` in an absolute folder of the PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

/// PowerShell 7, wherever its installer or the Store put it.
fn pwsh() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(PathBuf::from(&dir).join(r"PowerShell\7\pwsh.exe"));
            candidates.push(PathBuf::from(&dir).join(r"PowerShell\7-preview\pwsh.exe"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    if let Some(found) = on_path("pwsh.exe") {
        return Some(found);
    }
    // The Store's app execution alias is a reparse point that only CreateProcess follows.
    let alias = std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join(r"Microsoft\WindowsApps\pwsh.exe"))?;
    alias.symlink_metadata().is_ok().then_some(alias)
}

/// Every shell installed, the default first.
pub fn detect() -> Vec<Shell> {
    let mut shells = Vec::new();
    if let Some(program) = pwsh() {
        shells.push(Shell { id: "pwsh".into(), name: "PowerShell 7".into(), kind: Kind::PowerShell, program });
    }
    let windows_powershell = system_root().join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if windows_powershell.is_file() {
        shells.push(Shell { id: "powershell".into(), name: "Windows PowerShell".into(), kind: Kind::PowerShell, program: windows_powershell });
    }
    let cmd = system_root().join(r"System32\cmd.exe");
    if cmd.is_file() {
        shells.push(Shell { id: "cmd".into(), name: "Command Prompt".into(), kind: Kind::Cmd, program: cmd });
    }
    let wsl = system_root().join(r"System32\wsl.exe");
    if wsl.is_file() {
        for name in wsl_distributions() {
            shells.push(Shell { id: format!("wsl:{name}"), name: format!("{name} (WSL)"), kind: Kind::Wsl, program: wsl.clone() });
        }
    }
    shells
}

/// The shell `id` names, or the default when it names none that is installed.
pub fn pick<'a>(shells: &'a [Shell], id: Option<&str>) -> Option<&'a Shell> {
    id.and_then(|id| shells.iter().find(|shell| shell.id.eq_ignore_ascii_case(id))).or_else(|| shells.first())
}

/// The command that starts `shell` in `cwd`.
pub fn launch(shell: &Shell, cwd: &Path) -> Command {
    let args = match shell.kind {
        Kind::PowerShell => vec![Arg::Quoted("-NoLogo".into())],
        Kind::Cmd => Vec::new(),
        Kind::Wsl => {
            let distribution = shell.id.strip_prefix("wsl:").unwrap_or_default().to_owned();
            vec![
                Arg::Quoted("-d".into()),
                Arg::Quoted(distribution),
                Arg::Quoted("--cd".into()),
                Arg::Quoted(cwd.display().to_string()),
            ]
        }
    };
    Command { program: shell.program.clone(), args, cwd: cwd.to_path_buf(), env: Vec::new() }
}

/// Runs `command` in Command Prompt in `cwd`, which stays open afterwards, as the projects
/// hub's Run does in Windows Terminal. The command is looked for on the PATH only, never in the
/// project folder (`NoDefaultCurrentDirectoryInExePath`, cleared again once it ends).
pub fn run_in_cmd(command: &str, cwd: &Path) -> Command {
    let cmd = system_root().join(r"System32\cmd.exe");
    let line = format!("set NoDefaultCurrentDirectoryInExePath=1& {command}& set NoDefaultCurrentDirectoryInExePath=");
    Command { program: cmd, args: vec![Arg::Quoted("/k".into()), Arg::Raw(line)], cwd: cwd.to_path_buf(), env: Vec::new() }
}

/// What to type into a shell of `kind` to change to `folder` (and run it), or `None` when that
/// shell can't be taken there.
pub fn cd_command(kind: Kind, distribution: Option<&str>, folder: &str) -> Option<String> {
    if folder.is_empty() || folder.contains(['\r', '\n', '\0']) {
        return None;
    }
    match kind {
        Kind::PowerShell => {
            // Inside single quotes only quote marks are special, and PowerShell takes the
            // typographic ones for quote marks too: each is doubled.
            let mut quoted = String::with_capacity(folder.len() + 2);
            for c in folder.chars() {
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    quoted.push(c);
                }
                quoted.push(c);
            }
            Some(format!("Set-Location -LiteralPath '{quoted}'\r"))
        }
        Kind::Cmd => {
            // Command Prompt expands %NAME% even inside quotes, with no way to escape it while typing.
            if folder.contains(['"', '%']) {
                return None;
            }
            Some(format!("cd /d \"{folder}\"\r"))
        }
        Kind::Wsl => {
            let path = wsl_path(folder, distribution)?;
            Some(format!("cd '{}'\r", path.replace('\'', r"'\''")))
        }
    }
}

/// Where a Windows folder is inside WSL: `C:\Users\me` is `/mnt/c/Users/me`, and a folder of
/// the distribution itself (`\\wsl.localhost\Ubuntu\home\me`, `\\wsl$\Ubuntu\...`) its own path.
pub fn wsl_path(folder: &str, distribution: Option<&str>) -> Option<String> {
    let bytes = folder.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let rest = folder[2..].trim_start_matches(['\\', '/']).replace('\\', "/");
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return Some(if rest.is_empty() { format!("/mnt/{drive}") } else { format!("/mnt/{drive}/{}", rest.trim_end_matches('/')) });
    }
    let lower = folder.to_ascii_lowercase();
    for prefix in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if lower.starts_with(prefix) {
            let rest = &folder[prefix.len()..];
            let (name, path) = rest.split_once('\\').unwrap_or((rest, ""));
            if distribution.is_some_and(|wanted| !wanted.eq_ignore_ascii_case(name)) {
                return None;
            }
            return Some(format!("/{}", path.replace('\\', "/").trim_end_matches('/')));
        }
    }
    None
}

/// The WSL distributions registered for this account, the default first. Docker Desktop's own
/// are left out: they are not shells.
fn wsl_distributions() -> Vec<String> {
    let (default, all) = registry::lxss();
    order_distributions(default.as_deref(), all)
}

/// Keeps the distributions worth offering, the default (by registration id) first.
fn order_distributions(default: Option<&str>, all: Vec<(String, String)>) -> Vec<String> {
    let mut kept: Vec<(String, String)> =
        all.into_iter().filter(|(_, name)| !name.is_empty() && !name.to_ascii_lowercase().starts_with("docker-desktop")).collect();
    kept.sort_by_key(|(id, _)| !default.is_some_and(|default| default.eq_ignore_ascii_case(id)));
    kept.into_iter().map(|(_, name)| name).collect()
}

mod registry {
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    fn string_value(key: HKEY, sub: Option<&[u16]>, name: &str) -> Option<String> {
        let name = wide(name);
        let mut buffer = vec![0u16; 512];
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `buffer` holds `bytes` bytes; the strings are NUL-terminated.
        let status = unsafe {
            RegGetValueW(key, sub.map_or(std::ptr::null(), <[u16]>::as_ptr), name.as_ptr(), RRF_RT_REG_SZ, null_mut(), buffer.as_mut_ptr().cast(), &mut bytes)
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = (bytes as usize / 2).saturating_sub(1).min(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]).trim_end_matches('\0').to_owned())
    }

    /// `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`: the default distribution's id, and
    /// each distribution's id and name.
    pub fn lxss() -> (Option<String>, Vec<(String, String)>) {
        let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
        let mut key: HKEY = null_mut();
        // SAFETY: `path` is NUL-terminated; the key is closed below.
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return (None, Vec::new());
        }
        let default = string_value(key, None, "DefaultDistribution");
        let mut all = Vec::new();
        for index in 0..64u32 {
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            // SAFETY: `name` holds `length` characters.
            let status = unsafe {
                RegEnumKeyExW(key, index, name.as_mut_ptr(), &mut length, null_mut(), null_mut(), null_mut(), null_mut())
            };
            if status != ERROR_SUCCESS {
                break;
            }
            let id = String::from_utf16_lossy(&name[..length as usize]);
            let sub: Vec<u16> = name[..length as usize].iter().copied().chain([0]).collect();
            if let Some(distribution) = string_value(key, Some(&sub), "DistributionName") {
                all.push((id, distribution));
            }
        }
        // SAFETY: opened above.
        unsafe { RegCloseKey(key) };
        (default, all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_powershell_and_command_prompt_are_always_there() {
        let shells = detect();
        assert!(shells.iter().any(|s| s.id == "powershell"), "{shells:?}");
        assert!(shells.iter().any(|s| s.id == "cmd"));
        // The default is PowerShell 7 when installed, else Windows PowerShell.
        let default = pick(&shells, None).unwrap();
        let has_pwsh = shells.iter().any(|s| s.id == "pwsh");
        assert_eq!(default.id, if has_pwsh { "pwsh" } else { "powershell" });
        assert_eq!(pick(&shells, Some("CMD")).unwrap().id, "cmd");
        assert_eq!(pick(&shells, Some("wsl:Nowhere")).unwrap().id, default.id, "an uninstalled choice falls back");
        for shell in &shells {
            assert!(shell.program.is_absolute(), "{shell:?}");
        }
    }

    #[test]
    fn launch_lines() {
        let shell = Shell { id: "powershell".into(), name: "Windows PowerShell".into(), kind: Kind::PowerShell, program: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into() };
        assert_eq!(launch(&shell, Path::new(r"C:\a b")).command_line(), r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo");
        let wsl = Shell { id: "wsl:Ubuntu-24.04".into(), name: "Ubuntu-24.04 (WSL)".into(), kind: Kind::Wsl, program: r"C:\Windows\System32\wsl.exe".into() };
        assert_eq!(launch(&wsl, Path::new(r"C:\My Code")).command_line(), r#"C:\Windows\System32\wsl.exe -d Ubuntu-24.04 --cd "C:\My Code""#);
        let run = run_in_cmd("pnpm run dev", Path::new(r"C:\x"));
        assert!(run.command_line().ends_with(r"cmd.exe /k set NoDefaultCurrentDirectoryInExePath=1& pnpm run dev& set NoDefaultCurrentDirectoryInExePath="), "{}", run.command_line());
    }

    #[test]
    fn changing_folder_is_quoted_for_each_shell() {
        assert_eq!(cd_command(Kind::PowerShell, None, r"C:\it's").unwrap(), "Set-Location -LiteralPath 'C:\\it''s'\r");
        assert_eq!(cd_command(Kind::PowerShell, None, "C:\\a\u{2019}b; rm x").unwrap(), "Set-Location -LiteralPath 'C:\\a\u{2019}\u{2019}b; rm x'\r");
        assert_eq!(cd_command(Kind::PowerShell, None, r"C:\$env:x `n").unwrap(), "Set-Location -LiteralPath 'C:\\$env:x `n'\r");
        assert_eq!(cd_command(Kind::Cmd, None, r"C:\a & b").unwrap(), "cd /d \"C:\\a & b\"\r");
        assert_eq!(cd_command(Kind::Cmd, None, r"C:\100%PATH%"), None);
        assert_eq!(cd_command(Kind::Wsl, Some("Ubuntu"), r"C:\it's here").unwrap(), "cd '/mnt/c/it'\\''s here'\r");
        assert_eq!(cd_command(Kind::PowerShell, None, "C:\\a\nb"), None, "a line break would run what follows");
    }

    #[test]
    fn windows_folders_inside_wsl() {
        assert_eq!(wsl_path(r"C:\Users\me", None).unwrap(), "/mnt/c/Users/me");
        assert_eq!(wsl_path(r"D:\", None).unwrap(), "/mnt/d");
        assert_eq!(wsl_path(r"\\wsl.localhost\Ubuntu\home\me", Some("ubuntu")).unwrap(), "/home/me");
        assert_eq!(wsl_path(r"\\wsl$\Ubuntu\home\me\", Some("Ubuntu")).unwrap(), "/home/me");
        assert_eq!(wsl_path(r"\\wsl$\Debian\home", Some("Ubuntu")), None, "another distribution's folder");
        assert_eq!(wsl_path(r"\\server\share", None), None);
    }

    #[test]
    fn docker_desktop_is_no_shell_and_the_default_comes_first() {
        let all = vec![
            ("{a}".to_owned(), "docker-desktop".to_owned()),
            ("{b}".to_owned(), "Debian".to_owned()),
            ("{c}".to_owned(), "Ubuntu".to_owned()),
            ("{d}".to_owned(), "docker-desktop-data".to_owned()),
        ];
        assert_eq!(order_distributions(Some("{C}"), all.clone()), ["Ubuntu", "Debian"]);
        assert_eq!(order_distributions(None, all), ["Debian", "Ubuntu"]);
    }
}
