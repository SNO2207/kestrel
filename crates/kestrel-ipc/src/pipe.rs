//! One end of a pipe opened for overlapped I/O, so every read and write can give up after a
//! time limit: a client that connects and says nothing, or stops reading, never holds a
//! server thread for longer than that, and a client never waits forever on an app that hangs.

use std::io;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
    GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, ResetEvent, WaitForSingleObject};

/// A pipe handle and the event its operations signal.
pub(crate) struct Pipe {
    pub(crate) handle: HANDLE,
    event: HANDLE,
}

// SAFETY: the handles are kernel handles, usable from any thread; one thread uses a `Pipe` at a
// time (it is never shared, only moved).
unsafe impl Send for Pipe {}

pub(crate) fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(INFINITE - 1).min(INFINITE - 1)
}

/// A manual-reset event for overlapped operations.
pub(crate) fn new_event() -> io::Result<HANDLE> {
    // SAFETY: plain FFI; the caller owns and closes the handle.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() { Err(io::Error::last_os_error()) } else { Ok(event) }
}

fn is_disconnect(code: u32) -> bool {
    code == ERROR_BROKEN_PIPE || code == ERROR_PIPE_NOT_CONNECTED || code == ERROR_NO_DATA
}

impl Pipe {
    /// Takes ownership of an overlapped pipe handle.
    pub(crate) fn new(handle: HANDLE) -> io::Result<Self> {
        match new_event() {
            Ok(event) => Ok(Self { handle, event }),
            Err(error) => {
                // SAFETY: the handle was given to this function to own.
                unsafe { CloseHandle(handle) };
                Err(error)
            }
        }
    }

    /// Waits for the overlapped operation `overlapped` to finish within `timeout`, cancelling it
    /// otherwise. Returns the bytes it transferred.
    fn finish(&self, overlapped: &mut OVERLAPPED, started: bool, timeout: Duration) -> io::Result<usize> {
        if started {
            // SAFETY: the event belongs to this pipe and is valid.
            let waited = unsafe { WaitForSingleObject(self.event, millis(timeout)) };
            if waited == WAIT_TIMEOUT {
                // SAFETY: cancels the operation described by `overlapped`, which stays in place
                // until GetOverlappedResult below has waited for it to end.
                unsafe {
                    CancelIoEx(self.handle, overlapped);
                    let mut done = 0u32;
                    GetOverlappedResult(self.handle, overlapped, &mut done, 1);
                }
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the other end took too long"));
            }
            if waited != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
        }
        let mut done = 0u32;
        // SAFETY: the operation has completed (or completed at once); `overlapped` is the one it used.
        if unsafe { GetOverlappedResult(self.handle, overlapped, &mut done, 1) } == 0 {
            // SAFETY: plain FFI, right after the call whose error it reads.
            let code = unsafe { GetLastError() };
            if code == ERROR_MORE_DATA {
                return Ok(done as usize);
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(done as usize)
    }

    fn overlapped(&self) -> OVERLAPPED {
        // SAFETY: OVERLAPPED is plain data, valid when zeroed.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = self.event;
        // SAFETY: the event belongs to this pipe.
        unsafe { ResetEvent(self.event) };
        overlapped
    }

    /// Reads what has arrived, up to `buffer`'s length, waiting at most `timeout` for something
    /// to. Returns 0 once the other end has closed.
    pub(crate) fn read_some(&self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut overlapped = self.overlapped();
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: `buffer` and `overlapped` stay in place until `finish` has seen the operation end.
        let ok = unsafe { ReadFile(self.handle, buffer.as_mut_ptr(), length, null_mut(), &mut overlapped) };
        // SAFETY: plain FFI, right after the call whose error it reads.
        let code = if ok == 0 { unsafe { GetLastError() } } else { 0 };
        if ok == 0 && code != ERROR_IO_PENDING && code != ERROR_MORE_DATA {
            return if is_disconnect(code) { Ok(0) } else { Err(io::Error::from_raw_os_error(code as i32)) };
        }
        match self.finish(&mut overlapped, ok == 0 && code == ERROR_IO_PENDING, timeout) {
            Err(error) if error.raw_os_error().is_some_and(|code| is_disconnect(code as u32)) => Ok(0),
            other => other,
        }
    }

    /// Writes all of `data`, giving up once `timeout` has passed.
    pub(crate) fn write_all(&self, mut data: &[u8], timeout: Duration) -> io::Result<()> {
        let started = Instant::now();
        while !data.is_empty() {
            let left = timeout.checked_sub(started.elapsed()).ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "the other end stopped reading"))?;
            let mut overlapped = self.overlapped();
            // Pipes take writes of up to their buffer size at a time comfortably; larger ones
            // are split so each waits only for the reader to make room.
            let chunk = &data[..data.len().min(64 * 1024)];
            // SAFETY: `chunk` and `overlapped` stay in place until `finish` has seen the operation end.
            let ok = unsafe { WriteFile(self.handle, chunk.as_ptr(), chunk.len() as u32, null_mut(), &mut overlapped) };
            // SAFETY: plain FFI, right after the call whose error it reads.
            let code = if ok == 0 { unsafe { GetLastError() } } else { 0 };
            if ok == 0 && code != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            let written = self.finish(&mut overlapped, ok == 0, left)?;
            if written == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "the pipe took nothing"));
            }
            data = &data[written..];
        }
        Ok(())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: both handles are owned by this value and closed exactly once.
        unsafe {
            CloseHandle(self.handle);
            CloseHandle(self.event);
        }
    }
}

/// Reads one line (up to `\n`, which is not returned) of at most `max` bytes, carrying over
/// whatever followed it in `pending`. `Ok(None)` when the other end closed first.
pub(crate) fn read_line(pipe: &Pipe, pending: &mut Vec<u8>, max: usize, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
    let started = Instant::now();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        if let Some(end) = pending.iter().position(|&b| b == b'\n') {
            if end > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, LineTooLong(max)));
            }
            let mut line: Vec<u8> = pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        if pending.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, LineTooLong(max)));
        }
        let left = timeout.checked_sub(started.elapsed()).ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "nothing arrived in time"))?;
        let read = pipe.read_some(&mut buffer, left)?;
        if read == 0 {
            return Ok(None);
        }
        pending.extend_from_slice(&buffer[..read]);
    }
}

/// A line longer than allowed.
#[derive(Debug)]
pub(crate) struct LineTooLong(pub(crate) usize);

impl std::fmt::Display for LineTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "longer than {} bytes", self.0)
    }
}

impl std::error::Error for LineTooLong {}

pub(crate) fn is_too_long(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|inner| inner.is::<LineTooLong>())
}
