//! The client: `kestrel-cli --live` and `kestrel-mcp` ask the running app through it.

use std::io;
use std::ptr::null;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, GetLastError,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};

use crate::pipe::{Pipe, is_too_long, millis, read_line};
use crate::protocol::{Failure, Request, Response};
use crate::security::{self, wide};

/// The largest answer a client reads.
pub const MAX_RESPONSE: usize = 16 * 1024 * 1024;

/// Why a call failed.
#[derive(Debug)]
pub enum CallError {
    /// Nothing serves the pipe: Kestrel is not running (or its query service is off).
    NotRunning,
    /// Every connection was taken for the whole time allowed.
    Busy,
    /// The pipe is served by a program of another account, which is not to be trusted.
    WrongOwner,
    /// No answer in the time allowed.
    TimedOut,
    /// The app answered that it could not do it.
    Failed(Failure),
    /// The answer was not one the protocol allows.
    Protocol(String),
    Io(io::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NotRunning => f.write_str("Kestrel isn't running, or its query service is turned off"),
            CallError::Busy => f.write_str("Kestrel is busy answering other programs; try again shortly"),
            CallError::WrongOwner => f.write_str("The pipe is served by a program of another account, so it was not used"),
            CallError::TimedOut => f.write_str("Kestrel did not answer in time"),
            CallError::Failed(failure) => f.write_str(&failure.message),
            CallError::Protocol(message) => write!(f, "Kestrel's answer could not be read: {message}"),
            CallError::Io(error) => write!(f, "Couldn't talk to Kestrel: {error}"),
        }
    }
}

impl std::error::Error for CallError {}

impl From<io::Error> for CallError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::TimedOut { CallError::TimedOut } else { CallError::Io(error) }
    }
}

/// A connection to the app.
pub struct Client {
    pub(crate) pipe: Pipe,
    pub(crate) pending: Vec<u8>,
    server_pid: u32,
}

impl Client {
    /// Connects to the pipe `name`, waiting up to `timeout` for a free connection, and checks
    /// that the program serving it runs as this account.
    pub fn connect(name: &str, timeout: Duration) -> Result<Client, CallError> {
        if !crate::name::is_local_pipe(name) {
            return Err(CallError::Io(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not a local pipe"))));
        }
        let text = wide(name);
        let deadline = Instant::now() + timeout;
        let handle = loop {
            // SAFETY: `text` is NUL-terminated. SECURITY_IDENTIFICATION: the server may learn who
            // connected, but never act as this account.
            let handle = unsafe {
                CreateFileW(
                    text.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                break handle;
            }
            // SAFETY: plain FFI, right after the call whose error it reads.
            match unsafe { GetLastError() } {
                ERROR_FILE_NOT_FOUND => return Err(CallError::NotRunning),
                // Only another account's pipe refuses this account.
                ERROR_ACCESS_DENIED => return Err(CallError::WrongOwner),
                ERROR_PIPE_BUSY => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(CallError::Busy);
                    }
                    // SAFETY: `text` is NUL-terminated. Returns at once if the pipe is gone.
                    unsafe { WaitNamedPipeW(text.as_ptr(), millis(left).max(1)) };
                }
                code => return Err(CallError::Io(io::Error::from_raw_os_error(code as i32))),
            }
        };
        let pipe = Pipe::new(handle)?;
        let mut server_pid = 0u32;
        // SAFETY: `pipe.handle` is a connected client end.
        if unsafe { GetNamedPipeServerProcessId(pipe.handle, &mut server_pid) } == 0 {
            return Err(CallError::Io(io::Error::last_os_error()));
        }
        let mine = security::current_user_sid()?;
        match security::process_user_sid(server_pid) {
            Ok(theirs) if theirs == mine => {}
            _ => return Err(CallError::WrongOwner),
        }
        Ok(Client { pipe, pending: Vec::new(), server_pid })
    }

    /// The process id of the app answering.
    pub fn server_pid(&self) -> u32 {
        self.server_pid
    }

    /// Sends `request` and waits up to `timeout` for the answer.
    pub fn call(&mut self, request: &Request, timeout: Duration) -> Result<Value, CallError> {
        let started = Instant::now();
        let mut line = serde_json::to_vec(request).map_err(|error| CallError::Protocol(error.to_string()))?;
        line.push(b'\n');
        self.pipe.write_all(&line, timeout)?;
        let left = timeout.saturating_sub(started.elapsed());
        let answer = match read_line(&self.pipe, &mut self.pending, MAX_RESPONSE, left) {
            Ok(Some(answer)) => answer,
            Ok(None) => return Err(CallError::Protocol("Kestrel closed the connection without answering".into())),
            Err(error) if is_too_long(&error) => return Err(CallError::Protocol(format!("the answer is longer than {MAX_RESPONSE} bytes"))),
            Err(error) => return Err(error.into()),
        };
        let response: Response = serde_json::from_slice(&answer).map_err(|error| CallError::Protocol(error.to_string()))?;
        response.into_result().map_err(CallError::Failed)
    }

    /// [`Client::call`], read as `T`.
    pub fn call_as<T: DeserializeOwned>(&mut self, request: &Request, timeout: Duration) -> Result<T, CallError> {
        let value = self.call(request, timeout)?;
        serde_json::from_value(value).map_err(|error| CallError::Protocol(error.to_string()))
    }
}

/// Connects, asks once and hangs up, within `timeout` overall.
pub fn call(name: &str, request: &Request, timeout: Duration) -> Result<Value, CallError> {
    let started = Instant::now();
    let mut client = Client::connect(name, timeout)?;
    client.call(request, timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(1)))
}

/// [`call`], read as `T`.
pub fn call_as<T: DeserializeOwned>(name: &str, request: &Request, timeout: Duration) -> Result<T, CallError> {
    let value = call(name, request, timeout)?;
    serde_json::from_value(value).map_err(|error| CallError::Protocol(error.to_string()))
}
