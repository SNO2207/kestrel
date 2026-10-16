//! The pipe server the app runs.
//!
//! One thread accepts connections; each connection gets a thread of its own, which reads a
//! request line, has the handler answer it on another thread, and writes the response line,
//! again and again until the client hangs up or goes quiet. Everything is bounded:
//!
//! - at most [`Limits::max_clients`] connections at once; further clients wait in
//!   `WaitNamedPipe` (and give up after their own time limit);
//! - a request line may be [`Limits::max_request`] bytes, a response [`Limits::max_response`];
//! - a client has [`Limits::read_timeout`] to send each request once connected (and
//!   [`Limits::idle_timeout`] between requests), and [`Limits::write_timeout`] to take the
//!   answer;
//! - an answer that takes longer than [`Limits::handler_timeout`] is answered with a `timeout`
//!   failure, and the handler is told (through [`Context::cancelled`]) to stop;
//! - at most [`Limits::max_handlers`] handlers run at once, those given up on included;
//!   requests beyond that are answered `busy`.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, ResetEvent, SetEvent,
    WaitForMultipleObjects,
};

use crate::pipe::{Pipe, is_too_long, new_event, read_line};
use crate::protocol::{Failure, FailureKind, Request, Response};
use crate::security::{self, Descriptor, wide};

/// The server's bounds; see the module documentation.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_clients: usize,
    pub max_handlers: usize,
    pub max_request: usize,
    pub max_response: usize,
    pub read_timeout: Duration,
    pub idle_timeout: Duration,
    pub write_timeout: Duration,
    pub handler_timeout: Duration,
    /// Requests answered on one connection before the server closes it.
    pub max_requests_per_connection: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_clients: 8,
            max_handlers: 16,
            max_request: 64 * 1024,
            max_response: 4 * 1024 * 1024,
            read_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(5),
            handler_timeout: Duration::from_secs(10),
            max_requests_per_connection: 256,
        }
    }
}

/// Who asked, and until when an answer is wanted.
#[derive(Clone, Debug)]
pub struct Context {
    /// The client's process id, when Windows says.
    pub client_pid: Option<u32>,
    /// The client's program, such as `kestrel-mcp.exe`.
    pub client_name: String,
    /// When the server stops waiting for the answer.
    pub deadline: Instant,
    /// Set once the server has given up waiting: the handler should stop.
    pub cancelled: Arc<AtomicBool>,
}

impl Context {
    /// The deadline has passed, or the server gave up: work should stop.
    pub fn expired(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
}

/// Answers requests. Runs on a thread of its own per request.
pub type Handler = dyn Fn(Request, &Context) -> Result<Value, Failure> + Send + Sync;

/// Told about every request answered: the request, who asked, whether it worked, how long it took.
pub type Observer = dyn Fn(&Request, &Context, Result<(), &Failure>, Duration) + Send + Sync;

struct Shared {
    name: String,
    limits: Limits,
    handler: Arc<Handler>,
    observer: Option<Arc<Observer>>,
    /// Connections being served.
    clients: Mutex<usize>,
    freed: Condvar,
    /// Handlers running, those given up on included.
    handlers: Arc<AtomicUsize>,
    stopping: AtomicBool,
}

/// A running server. Dropping it stops it.
pub struct Server {
    shared: Arc<Shared>,
    stop: HANDLE,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: `stop` is an event handle, usable from any thread.
unsafe impl Send for Server {}
unsafe impl Sync for Server {}

impl Server {
    /// Creates the pipe `name` (failing if it already exists: another server has it) and starts
    /// answering requests with `handler`.
    pub fn start(name: &str, limits: Limits, handler: Arc<Handler>, observer: Option<Arc<Observer>>) -> io::Result<Server> {
        let sid = security::current_user_sid()?;
        let descriptor = Descriptor::from_sddl(&security::sddl_for(&sid)?)?;
        let limits = Limits { max_clients: limits.max_clients.clamp(2, 64), ..limits };
        let shared = Arc::new(Shared {
            name: name.to_owned(),
            limits,
            handler,
            observer,
            clients: Mutex::new(0),
            freed: Condvar::new(),
            handlers: Arc::new(AtomicUsize::new(0)),
            stopping: AtomicBool::new(false),
        });
        // The first instance is made here, so a name already taken fails `start` itself.
        let first = create_instance(&shared, &descriptor, true)?;
        let stop = new_event()?;
        let accepting = shared.clone();
        let stop_for_thread = stop as usize;
        let thread = std::thread::Builder::new()
            .name("kestrel-ipc-accept".into())
            .spawn(move || accept_loop(accepting, descriptor, first, stop_for_thread as HANDLE))?;
        Ok(Server { shared, stop, thread: Some(thread) })
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Stops accepting connections and waits for the accepting thread to end. Connections
    /// already being served finish their current request.
    pub fn stop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // SAFETY: the event is owned by this server.
        unsafe { SetEvent(self.stop) };
        self.shared.freed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: owned by this server, closed once.
        unsafe { CloseHandle(self.stop) };
    }
}

/// A listening instance of the pipe, closed when dropped unless handed on.
struct Instance(HANDLE);

// SAFETY: a kernel handle, usable from any thread.
unsafe impl Send for Instance {}

impl Drop for Instance {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: owned, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn create_instance(shared: &Shared, descriptor: &Descriptor, first: bool) -> io::Result<Instance> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(&shared.name);
    // The first instance claims the name: if some other program created it first (to pose as
    // Kestrel), this fails rather than joining that program's pipe.
    let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    // SAFETY: `name` and `attributes` (and the descriptor it points at) outlive the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            shared.limits.max_clients as u32,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(Instance(handle)) }
}

/// Waits for a client on `instance`, or for `stop`. True once a client is connected.
fn connect(instance: &Instance, event: HANDLE, stop: HANDLE) -> io::Result<bool> {
    // SAFETY: OVERLAPPED is plain data; it stays in place until the operation has ended.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    // SAFETY: the event is valid.
    unsafe { ResetEvent(event) };
    // SAFETY: `instance` is a listening pipe; `overlapped` outlives the operation (it is waited
    // for, or cancelled and waited for, below).
    let connected = unsafe { ConnectNamedPipe(instance.0, &mut overlapped) } != 0;
    // SAFETY: plain FFI, right after the call whose error it reads.
    let code = unsafe { GetLastError() };
    if connected || code == ERROR_PIPE_CONNECTED {
        return Ok(true);
    }
    if code != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let handles = [event, stop];
    // SAFETY: both handles are valid events.
    let waited = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    let mut done = 0u32;
    if waited == WAIT_OBJECT_0 {
        // SAFETY: the operation has completed.
        let ok = unsafe { GetOverlappedResult(instance.0, &overlapped, &mut done, 1) } != 0;
        return if ok { Ok(true) } else { Err(io::Error::last_os_error()) };
    }
    // Stopping: cancel the wait for a client, and wait for the cancellation to land.
    // SAFETY: `overlapped` is the pending operation's.
    unsafe {
        CancelIoEx(instance.0, &overlapped);
        GetOverlappedResult(instance.0, &overlapped, &mut done, 1);
    }
    Ok(false)
}

fn accept_loop(shared: Arc<Shared>, descriptor: Descriptor, first: Instance, stop: HANDLE) {
    let Ok(event) = new_event() else { return };
    let mut listening = Some(first);
    while !shared.stopping.load(Ordering::SeqCst) {
        let instance = match listening.take() {
            Some(instance) => instance,
            None => {
                // At most `max_clients` connections: wait for one to end before listening again.
                // While all are busy, the instances being served keep the name alive.
                let mut clients = shared.clients.lock().unwrap_or_else(|e| e.into_inner());
                while *clients >= shared.limits.max_clients && !shared.stopping.load(Ordering::SeqCst) {
                    clients = shared.freed.wait_timeout(clients, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
                }
                drop(clients);
                if shared.stopping.load(Ordering::SeqCst) {
                    break;
                }
                match create_instance(&shared, &descriptor, false) {
                    Ok(instance) => instance,
                    Err(error) => {
                        eprintln!("kestrel-ipc: could not listen on {}: {error}", shared.name);
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                }
            }
        };
        match connect(&instance, event, stop) {
            Ok(true) => {
                *shared.clients.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                let serving = shared.clone();
                let spawned = std::thread::Builder::new().name("kestrel-ipc-client".into()).spawn(move || {
                    serve_client(&serving, instance);
                    *serving.clients.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
                    serving.freed.notify_all();
                });
                if spawned.is_err() {
                    *shared.clients.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
                }
            }
            Ok(false) => break,
            Err(error) => {
                eprintln!("kestrel-ipc: a client could not connect: {error}");
            }
        }
    }
    // SAFETY: created above, closed once.
    unsafe { CloseHandle(event) };
}

/// The program a process runs, by file name, or `""`.
fn process_name(pid: u32) -> String {
    // SAFETY: plain FFI; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return String::new();
    }
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` characters; the handle is valid and closed once.
    let ok = unsafe {
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        ok
    };
    if ok == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    path.rsplit('\\').next().unwrap_or(&path).to_owned()
}

fn client_pid(handle: HANDLE) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: `handle` is a connected server end of a pipe.
    (unsafe { GetNamedPipeClientProcessId(handle, &mut pid) } != 0).then_some(pid)
}

fn line_of(response: &Response) -> Vec<u8> {
    let mut line = serde_json::to_vec(response).unwrap_or_else(|_| br#"{"ok":false,"error":{"kind":"internal","message":"unwritable answer"}}"#.to_vec());
    line.push(b'\n');
    line
}

fn serve_client(shared: &Shared, instance: Instance) {
    let handle = instance.0;
    std::mem::forget(instance);
    let Ok(pipe) = Pipe::new(handle) else { return };
    let pid = client_pid(handle);
    let client_name = pid.map(process_name).unwrap_or_default();
    let limits = &shared.limits;
    let mut pending = Vec::new();
    for served in 0..limits.max_requests_per_connection {
        let wait = if served == 0 { limits.read_timeout } else { limits.idle_timeout };
        let line = match read_line(&pipe, &mut pending, limits.max_request, wait) {
            Ok(Some(line)) => line,
            // The client hung up, or went quiet: either way the connection is done.
            Ok(None) => break,
            Err(error) if is_too_long(&error) => {
                let failure = Failure::new(FailureKind::TooLarge, format!("Requests may be at most {} bytes", limits.max_request));
                let _ = pipe.write_all(&line_of(&Response::failure(failure)), limits.write_timeout);
                break;
            }
            Err(_) => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = answer(shared, &line, pid, &client_name);
        let mut bytes = line_of(&response);
        if bytes.len() > limits.max_response {
            let failure = Failure::new(
                FailureKind::TooLarge,
                format!("The answer would be {} bytes, more than {}; ask for fewer results", bytes.len(), limits.max_response),
            );
            bytes = line_of(&Response::failure(failure));
        }
        if pipe.write_all(&bytes, limits.write_timeout).is_err() {
            break;
        }
    }
    // Let the client read the last answer before the connection goes.
    // SAFETY: the handle is still open; `pipe` closes it when dropped.
    unsafe {
        FlushFileBuffers(pipe.handle);
        DisconnectNamedPipe(pipe.handle);
    }
}

/// Decrements the count of running handlers when the handler's thread is done.
struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn answer(shared: &Shared, line: &[u8], pid: Option<u32>, client_name: &str) -> Response {
    let request: Request = match serde_json::from_slice(line) {
        Ok(request) => request,
        Err(error) => return Response::failure(Failure::bad_request(format!("Not a request Kestrel knows: {error}"))),
    };
    let limits = &shared.limits;
    if shared.handlers.fetch_add(1, Ordering::SeqCst) >= limits.max_handlers {
        shared.handlers.fetch_sub(1, Ordering::SeqCst);
        return Response::failure(Failure::new(FailureKind::Busy, "Kestrel is answering too many requests; try again shortly"));
    }
    let running = Running(shared.handlers.clone());
    let started = Instant::now();
    let context = Context {
        client_pid: pid,
        client_name: client_name.to_owned(),
        deadline: started + limits.handler_timeout,
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (send, receive) = mpsc::channel();
    let handler = shared.handler.clone();
    let (for_handler, asked) = (context.clone(), request.clone());
    let spawned = std::thread::Builder::new().name("kestrel-ipc-handler".into()).spawn(move || {
        let _running = running;
        let _ = send.send(handler(asked, &for_handler));
    });
    let outcome = match spawned {
        Err(error) => Err(Failure::new(FailureKind::Internal, format!("Couldn't start answering: {error}"))),
        Ok(_) => match receive.recv_timeout(limits.handler_timeout) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                context.cancelled.store(true, Ordering::Relaxed);
                Err(Failure::new(
                    FailureKind::Timeout,
                    format!("Kestrel took longer than {} s to answer", limits.handler_timeout.as_secs_f64()),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Failure::new(FailureKind::Internal, "The answer failed")),
        },
    };
    if let Some(observer) = &shared.observer {
        observer(&request, &context, outcome.as_ref().map(|_| ()), started.elapsed());
    }
    match outcome {
        Ok(result) => Response::success(result),
        Err(failure) => Response::failure(failure),
    }
}

/// The access list Windows enforces on the pipe `name`, read through a connection of its own
/// (which the server then sees as a client that never asks anything).
#[cfg(test)]
pub(crate) fn listening_dacl(name: &str) -> io::Result<String> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_GENERIC_READ, OPEN_EXISTING, READ_CONTROL};
    let wide_name = wide(name);
    // SAFETY: `wide_name` is NUL-terminated; the handle is closed below.
    let handle = unsafe { CreateFileW(wide_name.as_ptr(), READ_CONTROL | FILE_GENERIC_READ, 0, null(), OPEN_EXISTING, 0, null_mut()) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let dacl = security::dacl_of(handle);
    // SAFETY: opened above.
    unsafe { CloseHandle(handle) };
    dacl
}
