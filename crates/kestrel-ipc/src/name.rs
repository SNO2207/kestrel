//! Where the app listens: `\\.\pipe\kestrel-query-<account SID>-<hash of the data folder>`.
//!
//! The account is in the name so that two people signed in to one machine never compete for
//! it (pipe names are shared by the whole machine), and the data folder so that test instances
//! of Kestrel, each with a folder of its own (`KESTREL_DATA_DIR`), answer separately. A client
//! finds the owner's own Kestrel by working out the same name from the default data folder.

use std::io;
use std::path::{Path, PathBuf};

use crate::security;

/// Every name starts with this.
pub const PREFIX: &str = r"\\.\pipe\kestrel-query-";

/// Kestrel's data folder, as the app works it out: `KESTREL_DATA_DIR`, or
/// `%LOCALAPPDATA%\Kestrel`.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KESTREL_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Kestrel")
}

/// FNV-1a of the folder's path, compared without case or a trailing separator: stable across
/// runs and builds, as the app's instance claim uses (`instance.rs`).
pub fn folder_hash(data_dir: &Path) -> u64 {
    data_dir
        .to_string_lossy()
        .to_lowercase()
        .trim_end_matches(['\\', '/'])
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The pipe of the Kestrel whose data folder is `data_dir`, for the account `sid`.
pub fn pipe_name_for(sid: &str, data_dir: &Path) -> String {
    format!("{PREFIX}{sid}-{:016x}", folder_hash(data_dir))
}

/// The pipe of the Kestrel whose data folder is `data_dir`, for this account.
pub fn pipe_name(data_dir: &Path) -> io::Result<String> {
    Ok(pipe_name_for(&security::current_user_sid()?, data_dir))
}

/// Whether `name` is a pipe on this machine (`\\.\pipe\...`): a client must never be pointed
/// at another machine's, which would hand that machine this account's credentials.
pub fn is_local_pipe(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(r"\\.\pipe\") && lower.len() > r"\\.\pipe\".len() && !name[r"\\.\pipe\".len()..].contains(['/'])
}

/// The pipe a client should use: `explicit` (a `--pipe` option), else `KESTREL_PIPE`, else
/// the one worked out from the data folder. A bare name gets the `\\.\pipe\` prefix.
pub fn resolve(explicit: Option<&str>) -> io::Result<String> {
    let given = explicit.map(str::to_owned).or_else(|| std::env::var("KESTREL_PIPE").ok().filter(|name| !name.trim().is_empty()));
    match given {
        Some(name) => {
            let name = name.trim();
            let full = if name.starts_with(r"\\") { name.to_owned() } else { format!(r"\\.\pipe\{name}") };
            if is_local_pipe(&full) {
                Ok(full)
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not a pipe on this computer")))
            }
        }
        None => pipe_name(&default_data_dir()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_account_and_the_data_folder() {
        let sid = "S-1-5-21-1-2-3-1001";
        let default = pipe_name_for(sid, Path::new(r"C:\Users\me\AppData\Local\Kestrel"));
        assert!(default.starts_with(r"\\.\pipe\kestrel-query-S-1-5-21-1-2-3-1001-"), "{default}");
        // Spelled differently, the same folder.
        assert_eq!(default, pipe_name_for(sid, Path::new(r"c:\users\ME\AppData\Local\Kestrel\")));
        assert_ne!(default, pipe_name_for(sid, Path::new(r"C:\Temp\kestrel-slots\slot4")));
        assert_ne!(default, pipe_name_for("S-1-5-21-1-2-3-1002", Path::new(r"C:\Users\me\AppData\Local\Kestrel")));
    }

    #[test]
    fn the_hash_matches_the_instance_claim() {
        // The same FNV-1a the app's single-instance mutex is named by: "" hashes to the basis.
        assert_eq!(folder_hash(Path::new("")), 0xcbf2_9ce4_8422_2325);
        assert_eq!(folder_hash(Path::new("a")), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn only_local_pipes_are_used() {
        assert!(is_local_pipe(r"\\.\pipe\kestrel-query-x"));
        assert!(is_local_pipe(r"\\.\PIPE\x"));
        assert!(!is_local_pipe(r"\\server\pipe\kestrel-query-x"));
        assert!(!is_local_pipe(r"\\.\pipe\"));
        assert!(!is_local_pipe(r"C:\x"));
        assert_eq!(resolve(Some("kestrel-test")).unwrap(), r"\\.\pipe\kestrel-test");
        assert_eq!(resolve(Some(r"\\.\pipe\abc")).unwrap(), r"\\.\pipe\abc");
        assert!(resolve(Some(r"\\evil\pipe\abc")).is_err());
    }
}
