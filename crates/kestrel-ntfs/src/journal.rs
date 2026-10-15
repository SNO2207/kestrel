//! The NTFS change journal (the USN journal): a log, kept by the file system, of every file
//! created, deleted, renamed or modified on a volume.
//!
//! Windows lets ordinary users read it through a handle to any directory on the volume, with
//! one restriction: file names are left out. Callers therefore use it to learn *which*
//! folders changed and re-read those folders to learn *how*.

use std::io;

/// Why a journal record was written. Several reasons accumulate on one record.
pub mod reason {
    pub const DATA_OVERWRITE: u32 = 0x1;
    pub const DATA_EXTEND: u32 = 0x2;
    pub const DATA_TRUNCATION: u32 = 0x4;
    pub const FILE_CREATE: u32 = 0x100;
    pub const FILE_DELETE: u32 = 0x200;
    pub const RENAME_OLD_NAME: u32 = 0x1000;
    pub const RENAME_NEW_NAME: u32 = 0x2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x8000;
    pub const HARD_LINK_CHANGE: u32 = 0x1_0000;
    pub const COMPRESSION_CHANGE: u32 = 0x2_0000;
    pub const ENCRYPTION_CHANGE: u32 = 0x4_0000;
    pub const REPARSE_POINT_CHANGE: u32 = 0x10_0000;
    pub const CLOSE: u32 = 0x8000_0000;

    /// Changes that alter what a folder listing shows: names, existence, sizes, dates and
    /// attributes. Security, extended attributes, object ids and alternate streams do not.
    pub const VISIBLE: u32 = DATA_OVERWRITE
        | DATA_EXTEND
        | DATA_TRUNCATION
        | FILE_CREATE
        | FILE_DELETE
        | RENAME_OLD_NAME
        | RENAME_NEW_NAME
        | BASIC_INFO_CHANGE
        | HARD_LINK_CHANGE
        | COMPRESSION_CHANGE
        | ENCRYPTION_CHANGE
        | REPARSE_POINT_CHANGE;
}

/// The journal's identity and extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalState {
    /// Changes when the journal is deleted and recreated, which loses its history.
    pub id: u64,
    /// The oldest change still held; older ones have been discarded to bound its size.
    pub first_usn: i64,
    /// Where the next change will be written.
    pub next_usn: i64,
}

/// One journal record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub usn: i64,
    /// File reference number of the file or folder that changed.
    pub file_id: u64,
    /// File reference number of the folder containing it.
    pub parent_id: u64,
    pub reason: u32,
    pub attrs: u32,
    /// Only present when the journal is read with administrator rights.
    pub name: Option<String>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// Parses the records in a buffer returned by a journal read (after its leading next-USN).
/// Version 2 records carry 64-bit file references, version 3 records 128-bit ones, of which
/// NTFS only uses the low 64 bits. A malformed record ends parsing rather than panicking.
pub fn parse_records(mut bytes: &[u8]) -> Vec<Change> {
    let mut changes = Vec::new();
    while let Some(length) = u32_at(bytes, 0).map(|l| l as usize) {
        if length < 8 || length > bytes.len() {
            break;
        }
        let record = &bytes[..length];
        // Field offsets differ only in the width of the two file references.
        let layout = match u16_at(record, 4) {
            Some(2) => Some((8, 16, 24, 40, 52, 56)),
            Some(3) => Some((8, 24, 40, 56, 68, 72)),
            _ => None,
        };
        let parsed = layout.and_then(|(file, parent, usn, reason, attrs, name_len)| {
            let name_bytes = u16_at(record, name_len)? as usize;
            let name_at = u16_at(record, name_len + 2)? as usize;
            let name = (name_bytes > 0)
                .then(|| record.get(name_at..name_at + name_bytes))
                .flatten()
                .map(|raw| {
                    let units: Vec<u16> =
                        raw.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
                    String::from_utf16_lossy(&units)
                });
            Some(Change {
                usn: u64_at(record, usn)? as i64,
                file_id: u64_at(record, file)?,
                parent_id: u64_at(record, parent)?,
                reason: u32_at(record, reason)?,
                attrs: u32_at(record, attrs)?,
                name,
            })
        });
        changes.extend(parsed);
        bytes = &bytes[length..];
    }
    changes
}

#[cfg(windows)]
pub use platform::*;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
        READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
    };

    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// The requested position has been discarded from the journal.
    pub const ERROR_JOURNAL_ENTRY_DELETED: i32 = 1181;
    const BUFFER_BYTES: usize = 256 * 1024;

    /// An open change journal for one volume.
    pub struct Journal {
        handle: HANDLE,
    }

    // SAFETY: the handle is only used through thread-safe Win32 calls.
    unsafe impl Send for Journal {}
    unsafe impl Sync for Journal {}

    impl Drop for Journal {
        fn drop(&mut self) {
            // SAFETY: opened by `open` and closed exactly once.
            unsafe { CloseHandle(self.handle) };
        }
    }

    impl Journal {
        /// Opens the journal of the volume whose root is `root`, such as `C:\`. Needs no
        /// special rights: the handle is to the root folder, opened only to read attributes.
        pub fn open(root: &str) -> io::Result<Self> {
            let path: Vec<u16> = format!("{}\\", root.trim_end_matches(['\\', '/'])).encode_utf16().chain([0]).collect();
            // SAFETY: `path` is NUL-terminated and outlives the call.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        fn control(&self, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
            let mut returned = 0u32;
            // SAFETY: both buffers are valid for their stated lengths during the call.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    code,
                    if input.is_empty() { null() } else { input.as_ptr().cast() },
                    input.len() as u32,
                    output.as_mut_ptr().cast(),
                    output.len() as u32,
                    &mut returned,
                    null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: plain FFI call.
                Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
            } else {
                Ok(returned as usize)
            }
        }

        pub fn state(&self) -> io::Result<JournalState> {
            let mut out = [0u8; size_of::<USN_JOURNAL_DATA_V0>()];
            self.control(FSCTL_QUERY_USN_JOURNAL, &[], &mut out)?;
            // SAFETY: the buffer holds a USN_JOURNAL_DATA_V0 written by the call above.
            let data: USN_JOURNAL_DATA_V0 = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
            Ok(JournalState { id: data.UsnJournalID, first_usn: data.FirstUsn, next_usn: data.NextUsn })
        }

        /// Reads every change from `from` up to the present and returns them with the position
        /// to read from next time. `from` must be a position this journal returned, or its
        /// `first_usn`. Fails with [`ERROR_JOURNAL_ENTRY_DELETED`] if that position has been
        /// discarded, in which case the changes since then are lost and a rescan is needed.
        pub fn read(&self, journal_id: u64, from: i64) -> io::Result<(Vec<Change>, i64)> {
            let mut changes = Vec::new();
            let mut buffer = vec![0u8; BUFFER_BYTES];
            let mut start = from;
            loop {
                let request = READ_USN_JOURNAL_DATA_V0 {
                    StartUsn: start,
                    ReasonMask: u32::MAX,
                    ReturnOnlyOnClose: 0,
                    Timeout: 0,
                    BytesToWaitFor: 0,
                    UsnJournalID: journal_id,
                };
                // SAFETY: READ_USN_JOURNAL_DATA_V0 is plain data.
                let input = unsafe {
                    std::slice::from_raw_parts((&raw const request).cast::<u8>(), size_of_val(&request))
                };
                // Ordinary users get the unprivileged form, without names; administrators, and
                // Windows versions without that form, get the full one.
                let returned = match self.control(FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, input, &mut buffer) {
                    Err(e) if matches!(e.raw_os_error(), Some(ERROR_INVALID_FUNCTION | ERROR_ACCESS_DENIED)) => {
                        self.control(FSCTL_READ_USN_JOURNAL, input, &mut buffer)?
                    }
                    other => other?,
                };
                if returned < 8 {
                    break;
                }
                let next = i64::from_le_bytes(buffer[..8].try_into().unwrap());
                changes.extend(parse_records(&buffer[8..returned]));
                if next <= start || returned == 8 {
                    start = next.max(start);
                    break;
                }
                start = next;
            }
            Ok((changes, start))
        }
    }

    /// Whether a read failed because the requested position has fallen out of the journal.
    pub fn is_gap(error: &io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_JOURNAL_ENTRY_DELETED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_record(file_id: u64, parent_id: u64, usn: i64, reason: u32, name: &str) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let length = (60 + name.len()).next_multiple_of(8);
        let mut record = vec![0u8; length];
        record[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&2u16.to_le_bytes());
        record[8..16].copy_from_slice(&file_id.to_le_bytes());
        record[16..24].copy_from_slice(&parent_id.to_le_bytes());
        record[24..32].copy_from_slice(&usn.to_le_bytes());
        record[40..44].copy_from_slice(&reason.to_le_bytes());
        record[52..56].copy_from_slice(&0x20u32.to_le_bytes());
        record[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        record[58..60].copy_from_slice(&60u16.to_le_bytes());
        record[60..60 + name.len()].copy_from_slice(&name);
        record
    }

    #[test]
    fn parses_version_2_records_with_and_without_names() {
        let mut buffer = v2_record(0x0005_0000_0000_1234, 0x0005_0000_0000_0005, 4096, reason::FILE_CREATE, "notes.txt");
        buffer.extend(v2_record(0x0001_0000_0000_0042, 0x0005_0000_0000_0005, 4200, reason::FILE_DELETE | reason::CLOSE, ""));
        let changes = parse_records(&buffer);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].file_id, 0x0005_0000_0000_1234);
        assert_eq!(changes[0].parent_id, 0x0005_0000_0000_0005);
        assert_eq!(changes[0].name.as_deref(), Some("notes.txt"));
        assert_eq!(changes[0].attrs, 0x20);
        assert_eq!((changes[1].usn, changes[1].name.as_deref()), (4200, None));
        assert_eq!(changes[1].reason & reason::FILE_DELETE, reason::FILE_DELETE);
    }

    #[test]
    fn stops_at_a_truncated_or_corrupt_record() {
        let mut buffer = v2_record(1, 5, 10, reason::FILE_CREATE, "a");
        let good = buffer.len();
        buffer.extend(v2_record(2, 5, 20, reason::FILE_CREATE, "b"));
        assert_eq!(parse_records(&buffer[..good + 20]).len(), 1);
        buffer[good..good + 4].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(parse_records(&buffer).len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_system_drive_journal_without_administrator_rights() {
        let journal = Journal::open(r"C:\").unwrap();
        let state = journal.state().unwrap();
        assert!(state.next_usn >= state.first_usn);

        // Make a change and find it in the journal by its file id.
        let dir = std::env::temp_dir().join(format!("kestrel-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-probe.txt");
        std::fs::write(&file, b"probe").unwrap();
        let file_id = file_id(&file);
        let (changes, next) = journal.read(state.id, state.next_usn).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(next > state.next_usn);
        assert!(
            changes.iter().any(|c| c.file_id == file_id && c.reason & reason::FILE_CREATE != 0),
            "the new file's creation is in the journal"
        );
    }

    #[cfg(windows)]
    fn file_id(path: &std::path::Path) -> u64 {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
        let file = std::fs::OpenOptions::new().read(true).custom_flags(0).open(path).unwrap();
        // SAFETY: the handle is valid while `file` lives; `info` is a valid out-pointer.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            assert_ne!(GetFileInformationByHandle(file.as_raw_handle(), &mut info), 0);
            (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64
        }
    }
}
