//! Enlarging each fixed NTFS drive's change journal, as part of the elevated install.
//!
//! Kestrel keeps its index current from each drive's change journal and, after a restart,
//! catches up from what the journal still holds. Windows keeps 32 MB of journal on a system
//! drive by default, which a busy drive can cycle through within days; after that, the drive
//! has to be scanned again. `kestrel-broker install --journal-mb N` raises each fixed NTFS
//! drive's journal to N MB. A journal that is already at least that large is left alone, so
//! the option never shrinks one.

use std::ptr::{null, null_mut};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_QUERY_USN_JOURNAL, USN_JOURNAL_DATA_V0,
};

/// The smallest size accepted: Windows' own default for a system drive.
pub const MIN_MB: u64 = 32;
/// The largest size accepted, 16 GB, far beyond what any drive's history needs.
pub const MAX_MB: u64 = 16 * 1024;
const MB: u64 = 1 << 20;
/// `GetDriveTypeW`'s answer for a fixed drive.
const DRIVE_FIXED: u32 = 3;
/// `ERROR_JOURNAL_NOT_ACTIVE`: the drive has no change journal yet.
const JOURNAL_NOT_ACTIVE: i32 = 1179;

/// The size given with `--journal-mb N` (or `--journal-mb=N`), if any.
pub fn requested_mb(args: &[String]) -> Result<Option<u64>> {
    let mut value = None;
    for (at, arg) in args.iter().enumerate() {
        if arg == "--journal-mb" {
            value = Some(args.get(at + 1).context("--journal-mb needs a size in megabytes")?.as_str());
        } else if let Some(given) = arg.strip_prefix("--journal-mb=") {
            value = Some(given);
        }
    }
    let Some(value) = value else { return Ok(None) };
    let mb: u64 = value
        .trim()
        .parse()
        .with_context(|| format!("--journal-mb needs a whole number of megabytes, not {value:?}"))?;
    if !(MIN_MB..=MAX_MB).contains(&mb) {
        bail!("--journal-mb must be between {MIN_MB} and {MAX_MB} megabytes");
    }
    Ok(Some(mb))
}

/// The request that sizes a journal to `mb` megabytes. NTFS extends the journal, and once it
/// is full discards its oldest records, `AllocationDelta` bytes at a time; an eighth of the
/// maximum keeps most of the history while doing that rarely.
pub fn create_request(mb: u64) -> CREATE_USN_JOURNAL_DATA {
    let size = mb * MB;
    CREATE_USN_JOURNAL_DATA { MaximumSize: size, AllocationDelta: (size / 8).max(MB) }
}

/// The request to send for a drive whose journal currently holds up to `current` bytes
/// (`None` if it has no journal), or `None` if it is already large enough.
pub fn plan(current: Option<u64>, mb: u64) -> Option<CREATE_USN_JOURNAL_DATA> {
    let request = create_request(mb);
    match current {
        Some(size) if size >= request.MaximumSize => None,
        _ => Some(request),
    }
}

/// What happened to one drive's journal.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Enlarged { from: u64, to: u64 },
    Created { size: u64 },
    AlreadyLarge { size: u64 },
}

/// The fixed drives formatted NTFS, as `C:`.
pub fn fixed_ntfs_drives() -> Vec<String> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| format!("{}:", char::from(b'A' + bit)))
        .filter(|drive| {
            let root: Vec<u16> = format!("{drive}\\").encode_utf16().chain([0]).collect();
            let mut file_system = [0u16; 32];
            // SAFETY: `root` is NUL-terminated; the buffer's length is passed with it.
            let described = unsafe {
                GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED
                    && GetVolumeInformationW(
                        root.as_ptr(),
                        null_mut(),
                        0,
                        null_mut(),
                        null_mut(),
                        null_mut(),
                        file_system.as_mut_ptr(),
                        file_system.len() as u32,
                    ) != 0
            };
            described && String::from_utf16_lossy(&file_system).trim_end_matches('\0') == "NTFS"
        })
        .collect()
}

/// Closes a handle when dropped.
struct Volume(HANDLE);

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

impl Volume {
    /// Opens a drive such as `C:` for changing its journal. Needs administrator rights.
    fn open(drive: &str) -> Result<Self> {
        let path: Vec<u16> = format!(r"\\.\{drive}").encode_utf16().chain([0]).collect();
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("opening {drive}"));
        }
        Ok(Self(handle))
    }

    fn control(&self, code: u32, input: &[u8], output: &mut [u8]) -> std::io::Result<()> {
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for the lengths passed and outlive the call.
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }

    /// The journal's maximum size, or `None` if the drive has no journal.
    fn journal_size(&self) -> Result<Option<u64>> {
        let mut out = [0u8; size_of::<USN_JOURNAL_DATA_V0>()];
        match self.control(FSCTL_QUERY_USN_JOURNAL, &[], &mut out) {
            Ok(()) => {
                // SAFETY: the buffer holds a USN_JOURNAL_DATA_V0 written by the call above.
                let data: USN_JOURNAL_DATA_V0 = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
                Ok(Some(data.MaximumSize))
            }
            Err(e) if e.raw_os_error() == Some(JOURNAL_NOT_ACTIVE) => Ok(None),
            Err(e) => Err(e).context("reading the change journal's size"),
        }
    }
}

/// Raises `drive`'s change journal to `mb` megabytes, unless it is already that large.
pub fn enlarge(drive: &str, mb: u64) -> Result<Change> {
    let volume = Volume::open(drive)?;
    let current = volume.journal_size()?;
    let Some(request) = plan(current, mb) else {
        return Ok(Change::AlreadyLarge { size: current.unwrap_or_default() });
    };
    // SAFETY: CREATE_USN_JOURNAL_DATA is plain data, viewed as bytes for the call.
    let input = unsafe {
        std::slice::from_raw_parts((&raw const request).cast::<u8>(), size_of::<CREATE_USN_JOURNAL_DATA>())
    };
    volume.control(FSCTL_CREATE_USN_JOURNAL, input, &mut []).context("resizing the change journal")?;
    Ok(match current {
        Some(from) => Change::Enlarged { from, to: request.MaximumSize },
        None => Change::Created { size: request.MaximumSize },
    })
}

/// Enlarges every fixed NTFS drive's journal, reporting each. A drive that fails is reported
/// and skipped: the broker works without a larger journal.
pub fn enlarge_all(mb: u64) {
    for drive in fixed_ntfs_drives() {
        match enlarge(&drive, mb) {
            Ok(Change::Enlarged { from, to }) => println!("{drive} change journal: {} MB to {} MB", from / MB, to / MB),
            Ok(Change::Created { size }) => println!("{drive} change journal: created at {} MB", size / MB),
            Ok(Change::AlreadyLarge { size }) => println!("{drive} change journal: already {} MB, left as it is", size / MB),
            Err(e) => eprintln!("{drive} change journal: not changed ({e:#})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn reads_the_journal_size_option() {
        assert_eq!(requested_mb(&args("kestrel-broker install")).unwrap(), None);
        assert_eq!(requested_mb(&args("kestrel-broker install --journal-mb 512")).unwrap(), Some(512));
        assert_eq!(requested_mb(&args("kestrel-broker install --journal-mb=1024")).unwrap(), Some(1024));
        assert_eq!(requested_mb(&args("kestrel-broker install --allow S-1-5 --journal-mb 64")).unwrap(), Some(64));
        assert!(requested_mb(&args("kestrel-broker install --journal-mb")).is_err(), "a size is required");
        assert!(requested_mb(&args("kestrel-broker install --journal-mb lots")).is_err());
        assert!(requested_mb(&args("kestrel-broker install --journal-mb -5")).is_err());
        assert!(requested_mb(&args("kestrel-broker install --journal-mb 8")).is_err(), "below Windows' default");
        assert!(requested_mb(&args("kestrel-broker install --journal-mb 99999")).is_err());
        let message = requested_mb(&args("kestrel-broker install --journal-mb 4")).unwrap_err().to_string();
        assert_eq!(message, "--journal-mb must be between 32 and 16384 megabytes");
    }

    #[test]
    fn builds_the_create_request() {
        let request = create_request(512);
        assert_eq!(request.MaximumSize, 512 * 1024 * 1024);
        assert_eq!(request.AllocationDelta, 64 * 1024 * 1024);
        // The structure NTFS expects: two little-endian 64-bit sizes, maximum first.
        assert_eq!(size_of::<CREATE_USN_JOURNAL_DATA>(), 16);
        // SAFETY: plain data viewed as bytes.
        let bytes = unsafe { std::slice::from_raw_parts((&raw const request).cast::<u8>(), 16) };
        assert_eq!(&bytes[..8], &(512u64 << 20).to_le_bytes());
        assert_eq!(&bytes[8..], &(64u64 << 20).to_le_bytes());
        assert_eq!(create_request(MIN_MB).AllocationDelta, 4 << 20);
    }

    /// Read-only: lists drives without opening any for writing.
    #[test]
    fn finds_the_system_drive() {
        let drives = fixed_ntfs_drives();
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
        assert!(drives.contains(&system), "{drives:?}");
    }

    #[test]
    fn never_shrinks_a_journal() {
        let mb = 1 << 20;
        assert_eq!(plan(Some(32 * mb), 512).map(|r| r.MaximumSize), Some(512 * mb));
        assert_eq!(plan(None, 64).map(|r| r.MaximumSize), Some(64 * mb), "a drive without a journal gets one");
        assert!(plan(Some(512 * mb), 512).is_none());
        assert!(plan(Some(2048 * mb), 512).is_none());
    }
}
