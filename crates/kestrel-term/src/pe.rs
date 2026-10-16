//! Whether a program is a windowed one, from its executable's header: closing a terminal ends
//! the console programs it started and leaves the windowed ones (an editor, say) open.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI`.
const WINDOWS_GUI: u16 = 2;
/// Offset of `Subsystem` in the optional header, the same in PE32 and PE32+.
const SUBSYSTEM_OFFSET: u64 = 68;

/// The subsystem an executable declares, read from its PE header.
pub fn subsystem(path: &Path) -> Option<u16> {
    let mut file = File::open(path).ok()?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(dos[60..64].try_into().ok()?);
    file.seek(SeekFrom::Start(u64::from(pe))).ok()?;
    let mut signature = [0u8; 4];
    file.read_exact(&mut signature).ok()?;
    if &signature != b"PE\0\0" {
        return None;
    }
    // The file header (20 bytes) comes first, then the optional header.
    file.seek(SeekFrom::Current(20 + SUBSYSTEM_OFFSET as i64)).ok()?;
    let mut value = [0u8; 2];
    file.read_exact(&mut value).ok()?;
    Some(u16::from_le_bytes(value))
}

/// Whether the executable is a windowed (GUI) program.
pub fn is_windowed(path: &Path) -> bool {
    subsystem(path) == Some(WINDOWS_GUI)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system32(name: &str) -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::path::PathBuf::from(root).join("System32").join(name)
    }

    #[test]
    fn console_and_windowed_programs_are_told_apart() {
        assert_eq!(subsystem(&system32("cmd.exe")), Some(3), "a console program");
        assert!(!is_windowed(&system32("cmd.exe")));
        assert!(!is_windowed(&system32("PING.EXE")));
        assert!(is_windowed(&system32("notepad.exe")) || is_windowed(&system32("mspaint.exe")) || is_windowed(&system32("calc.exe")));
        assert!(subsystem(&system32("kernel32.dll")).is_some());
        assert_eq!(subsystem(Path::new(r"C:\no\such\program.exe")), None);
    }
}
