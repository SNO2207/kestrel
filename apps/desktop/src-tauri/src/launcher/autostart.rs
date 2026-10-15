//! "Start Kestrel when you sign in": a value under the current user's `Run` key, which is
//! where Windows looks for programs to start at sign-in (and what Task Manager's Startup page
//! lists and lets people turn off). It is the user's own setting, needs no administrator rights,
//! and is only ever written when the user ticks the option; it is off by default, and tests
//! never turn it on.
//!
//! The entry starts Kestrel with [`BACKGROUND_ARG`], which keeps the window hidden when
//! "Keep running in the tray" is on, so signing in does not open a window.

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Passed by the sign-in entry: start in the tray rather than with the window open.
pub const BACKGROUND_ARG: &str = "--background";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The value's name: `Kestrel` for the app, and one per identity for development and test
/// builds, so they never touch the installed app's entry.
pub fn value_name(identifier: &str) -> String {
    if identifier == "dev.kestrel.explorer" { "Kestrel".into() } else { format!("Kestrel ({identifier})") }
}

/// The command the entry runs.
pub fn command_line(exe: &str) -> String {
    format!("\"{exe}\" {BACKGROUND_ARG}")
}

/// Whether the entry exists.
pub fn is_enabled(identifier: &str) -> bool {
    let key = wide(RUN_KEY);
    let name = wide(&value_name(identifier));
    let mut size = 0u32;
    // SAFETY: both strings are NUL-terminated; a null buffer with a size pointer only asks for
    // the size, which is all that is needed to know the value exists.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
    };
    status == ERROR_SUCCESS
}

/// Adds or removes the entry. Removing one that is not there is not an error.
pub fn set_enabled(identifier: &str, enabled: bool) -> Result<(), String> {
    let key = wide(RUN_KEY);
    let name = wide(&value_name(identifier));
    let status = if enabled {
        let exe = std::env::current_exe().map_err(|error| format!("Couldn't find Kestrel's program file: {error}"))?;
        let command = wide(&command_line(&exe.to_string_lossy()));
        // SAFETY: the strings are NUL-terminated and `command` outlives the call; the size is
        // in bytes and includes the terminator, as REG_SZ requires.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: both strings are NUL-terminated.
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
            status => status,
        }
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Windows didn't save the startup setting (error {status})."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests only read the registry; turning the option on is left to people.

    #[test]
    fn each_build_identity_has_its_own_entry() {
        assert_eq!(value_name("dev.kestrel.explorer"), "Kestrel");
        assert_eq!(value_name("dev.kestrel.explorer.slot3"), "Kestrel (dev.kestrel.explorer.slot3)");
        assert_eq!(command_line(r"C:\Program Files\Kestrel\kestrel.exe"), r#""C:\Program Files\Kestrel\kestrel.exe" --background"#);
    }

    #[test]
    fn a_missing_entry_reads_as_off() {
        assert!(!is_enabled("dev.kestrel.explorer.unit-test-that-never-writes"));
    }
}
