//! Downloads one model file: with progress, resuming after an interruption, cancellable at any
//! moment, and checked against its SHA-256 before it takes its place.
//!
//! It goes through WinHTTP, Windows' own HTTP stack (TLS, the system's proxy settings and
//! redirects included), so nothing else is needed to fetch from Hugging Face. Bytes go to
//! `<file>.part` beside the destination; a later attempt asks only for what is missing
//! (`Range`), hashing what is already there first. A file whose hash is wrong is deleted, never
//! kept, and the failure says so.
//!
//! Nothing here runs unless the owner clicks a button that says what, from where and how big:
//! the app calls [`fetch`] from that button only. [`Policy::LoopbackOnly`] refuses every host
//! but this computer before any connection is made; test instances of the app run with it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::*;

/// Bytes read from the connection at a time.
const CHUNK: usize = 256 * 1024;

/// One file to fetch.
#[derive(Clone, Debug)]
pub struct Target {
    pub url: String,
    pub dest: PathBuf,
    pub size: u64,
    /// Lower-case hex.
    pub sha256: String,
}

/// Which hosts may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    AnyHost,
    /// Only `localhost`, `127.0.0.1` and `[::1]`: what tests and test instances use.
    LoopbackOnly,
}

/// Why a download stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The owner cancelled; what arrived is kept, and the next attempt resumes from it.
    Cancelled,
    /// The whole file arrived but its SHA-256 is not the published one; it was deleted.
    Checksum { expected: String, got: String },
    /// The server or the network failed; what arrived is kept for the next attempt.
    Network(String),
    /// The file couldn't be written.
    Disk(String),
    /// The address is not one this download may use.
    Refused(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Cancelled => f.write_str("Cancelled"),
            Failure::Checksum { expected, got } => write!(
                f,
                "The file arrived damaged: its SHA-256 is {got}, not {expected}. It was deleted; try again."
            ),
            Failure::Network(why) | Failure::Disk(why) | Failure::Refused(why) => f.write_str(why),
        }
    }
}

/// Lets another thread stop a download at once, even one waiting on the network.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    /// The request in progress, which closing aborts. Whoever takes it closes it.
    request: Mutex<Option<usize>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.request.lock().take() {
            // SAFETY: the handle was registered by `fetch` and taken out here, so nobody else
            // closes it; closing a request another thread waits on makes that wait fail.
            unsafe { WinHttpCloseHandle(handle as *mut _) };
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Allows another download with the same token.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// A WinHTTP handle closed when dropped.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: an open handle this value owns.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The parts of an `http` or `https` address.
#[derive(Debug, PartialEq, Eq)]
struct Url {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Option<Url> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').map(str::parse).transpose().ok()?;
        (format!("[{host}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port.parse().ok()?)),
            None => (authority.to_owned(), None),
        }
    };
    Some(Url { secure, port: port.unwrap_or(if secure { 443 } else { 80 }), host: host.to_ascii_lowercase(), path: path.to_owned() })
}

/// Whether `host` is this computer.
pub fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost")
}

/// Whether `url` names this computer, over plain or secure HTTP.
pub fn is_loopback_url(url: &str) -> bool {
    parse_url(url).is_some_and(|url| is_loopback(&url.host))
}

fn last_error(what: &str) -> Failure {
    // SAFETY: reads the calling thread's last error code.
    let code = unsafe { GetLastError() };
    let why = match code {
        ERROR_WINHTTP_OPERATION_CANCELLED => return Failure::Cancelled,
        ERROR_WINHTTP_TIMEOUT => "the server stopped answering".to_owned(),
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "the server's name couldn't be found (is this PC online?)".to_owned(),
        ERROR_WINHTTP_CANNOT_CONNECT => "the server couldn't be reached".to_owned(),
        ERROR_WINHTTP_CONNECTION_ERROR => "the connection was lost".to_owned(),
        ERROR_WINHTTP_SECURE_FAILURE => "the server's certificate couldn't be checked".to_owned(),
        code => format!("WinHTTP error {code}"),
    };
    Failure::Network(format!("The download stopped ({what}): {why}. It resumes where it left off."))
}

fn disk(path: &Path, error: std::io::Error) -> Failure {
    Failure::Disk(format!("Couldn't write {}: {error}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of a whole file, as lower-case hex.
pub fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

/// The partial file a download writes to.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// One HTTP GET in progress. Dropping it closes the request (unless a cancel already did),
/// then the connection, then the session: children before their parents.
struct Response<'a> {
    cancel: &'a Cancel,
    request: usize,
    status: u32,
    content_length: Option<u64>,
    content_range: Option<String>,
    _connection: Handle,
    _session: Handle,
}

impl Drop for Response<'_> {
    fn drop(&mut self) {
        close_request(self.cancel);
    }
}

fn header_text(request: *mut core::ffi::c_void, level: u32) -> Option<String> {
    let mut buffer = vec![0u16; 256];
    let mut length = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its length in bytes are passed together; the call writes at most
    // that many bytes and says how many.
    let ok = unsafe { WinHttpQueryHeaders(request, level, std::ptr::null(), buffer.as_mut_ptr().cast(), &mut length, std::ptr::null_mut()) };
    (ok != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize / 2]))
}

fn get<'a>(url: &Url, from: u64, cancel: &'a Cancel) -> Result<Response<'a>, Failure> {
    let agent = wide("Kestrel");
    // SAFETY: every string passed is NUL-terminated and outlives its call; each handle is owned
    // by a `Handle` (or, for the request, registered with `cancel`) and closed exactly once.
    unsafe {
        let session = Handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0));
        if session.0.is_null() {
            return Err(last_error("opening"));
        }
        WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 60_000);
        let host = wide(url.host.trim_start_matches('[').trim_end_matches(']'));
        let connection = Handle(WinHttpConnect(session.0, host.as_ptr(), url.port, 0));
        if connection.0.is_null() {
            return Err(last_error("connecting"));
        }
        let verb = wide("GET");
        let path = wide(&url.path);
        let flags = if url.secure { WINHTTP_FLAG_SECURE } else { 0 };
        let request = WinHttpOpenRequest(connection.0, verb.as_ptr(), path.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), flags);
        if request.is_null() {
            return Err(last_error("asking"));
        }
        *cancel.request.lock() = Some(request as usize);
        let mut response = Response {
            cancel,
            request: request as usize,
            status: 0,
            content_length: None,
            content_range: None,
            _connection: connection,
            _session: session,
        };
        // A cancel that came before the request was registered found nothing to close.
        if cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        if from > 0 {
            let range = wide(&format!("Range: bytes={from}-"));
            WinHttpAddRequestHeaders(request, range.as_ptr(), u32::MAX, WINHTTP_ADDREQ_FLAG_ADD);
        }
        if WinHttpSendRequest(request, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
            return Err(last_error("sending"));
        }
        if WinHttpReceiveResponse(request, std::ptr::null_mut()) == 0 {
            return Err(last_error("waiting for the server"));
        }
        let mut status = 0u32;
        let mut size = 4u32;
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut size,
            std::ptr::null_mut(),
        );
        response.status = status;
        response.content_length = header_text(request, WINHTTP_QUERY_CONTENT_LENGTH).and_then(|text| text.trim().parse().ok());
        response.content_range = header_text(request, WINHTTP_QUERY_CONTENT_RANGE);
        Ok(response)
    }
}

fn close_request(cancel: &Cancel) {
    if let Some(handle) = cancel.request.lock().take() {
        // SAFETY: the request registered by `get`, taken out so it is closed once.
        unsafe { WinHttpCloseHandle(handle as *mut _) };
    }
}

/// Downloads `target`, calling `progress(bytes there, size)` as it goes. Returns at once when
/// the destination already holds the right file.
pub fn fetch(target: &Target, cancel: &Cancel, policy: Policy, progress: &mut dyn FnMut(u64, u64)) -> Result<(), Failure> {
    let result = fetch_inner(target, cancel, policy, progress);
    close_request(cancel);
    result
}

fn fetch_inner(target: &Target, cancel: &Cancel, policy: Policy, progress: &mut dyn FnMut(u64, u64)) -> Result<(), Failure> {
    let url = parse_url(&target.url).ok_or_else(|| Failure::Refused(format!("{} is not a web address", target.url)))?;
    if policy == Policy::LoopbackOnly && !is_loopback(&url.host) {
        return Err(Failure::Refused(format!("This copy of Kestrel downloads only from this computer, not {}", url.host)));
    }
    let expected = target.sha256.to_ascii_lowercase();
    if target.dest.is_file() {
        if std::fs::metadata(&target.dest).map(|m| m.len()).ok() == Some(target.size)
            && sha256_of(&target.dest).ok().as_deref() == Some(expected.as_str())
        {
            progress(target.size, target.size);
            return Ok(());
        }
        std::fs::remove_file(&target.dest).map_err(|e| disk(&target.dest, e))?;
    }
    if let Some(parent) = target.dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| disk(parent, e))?;
    }
    let part = part_path(&target.dest);
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part).map_err(|e| disk(&part, e))?;
    let mut have = file.metadata().map_err(|e| disk(&part, e))?.len();
    let mut hasher = Sha256::new();
    if have > target.size {
        file.set_len(0).map_err(|e| disk(&part, e))?;
        have = 0;
    }
    // What arrived before is hashed first, so the whole file's hash comes out at the end.
    let mut buffer = vec![0u8; CHUNK];
    let mut hashed = 0;
    while hashed < have {
        if cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        let want = (have - hashed).min(CHUNK as u64) as usize;
        file.read_exact(&mut buffer[..want]).map_err(|e| disk(&part, e))?;
        hasher.update(&buffer[..want]);
        hashed += want as u64;
    }
    progress(have, target.size);

    if have < target.size {
        let response = get(&url, have, cancel)?;
        match response.status {
            206 if have > 0 => {
                let starts = response.content_range.as_deref().is_some_and(|range| range.trim().starts_with(&format!("bytes {have}-")));
                if !starts {
                    return Err(Failure::Network(format!("The server resumed at the wrong place ({:?}).", response.content_range)));
                }
            }
            200 | 206 => {
                // The server sends the whole file: start again from nothing.
                if have > 0 {
                    file.set_len(0).map_err(|e| disk(&part, e))?;
                    hasher = Sha256::new();
                    have = 0;
                    progress(0, target.size);
                }
            }
            status => return Err(Failure::Network(format!("The server answered {status} for {}.", target.url))),
        }
        if let Some(length) = response.content_length
            && length != target.size - have
        {
            return Err(Failure::Network(format!(
                "The server offers {} bytes, not the {} published; nothing more was downloaded.",
                length + have,
                target.size
            )));
        }
        file.seek(SeekFrom::Start(have)).map_err(|e| disk(&part, e))?;
        loop {
            if cancel.is_cancelled() {
                return Err(Failure::Cancelled);
            }
            // Waits for some bytes, then reads what has arrived: a read of a whole buffer would
            // wait until it filled, and progress (and what a cancel keeps) would move only by
            // buffers on a slow line.
            let mut available = 0u32;
            // SAFETY: an out parameter; the request stays open while this runs unless a cancel
            // closes it, which makes the call fail rather than misbehave.
            if unsafe { WinHttpQueryDataAvailable(response.request as *mut _, &mut available) } == 0 {
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
                return Err(last_error("waiting for data"));
            }
            let mut read = 0u32;
            let wanted = (available as usize).clamp(1, buffer.len()) as u32;
            // SAFETY: the buffer is at least `wanted` bytes long; as above for the request.
            let ok = unsafe { WinHttpReadData(response.request as *mut _, buffer.as_mut_ptr().cast(), wanted, &mut read) };
            if ok == 0 {
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
                return Err(last_error("reading"));
            }
            if read == 0 {
                break;
            }
            let bytes = &buffer[..read as usize];
            if have + bytes.len() as u64 > target.size {
                return Err(Failure::Network("The server sent more than the published size.".into()));
            }
            file.write_all(bytes).map_err(|e| disk(&part, e))?;
            hasher.update(bytes);
            have += bytes.len() as u64;
            progress(have, target.size);
        }
        file.flush().map_err(|e| disk(&part, e))?;
        if have < target.size {
            return Err(Failure::Network(format!(
                "The connection closed after {have} of {} bytes. It resumes where it left off.",
                target.size
            )));
        }
    }
    drop(file);
    let got = hex(&hasher.finalize());
    if got != expected {
        let _ = std::fs::remove_file(&part);
        return Err(Failure::Checksum { expected, got });
    }
    std::fs::rename(&part, &target.dest).map_err(|e| disk(&target.dest, e))?;
    Ok(())
}

#[cfg(test)]
mod tests;
