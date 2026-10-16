//! A shell in a Windows pseudoconsole (ConPTY).
//!
//! The shell is started suspended, put in a job object of its own and only then resumed, so
//! every program it starts, and everything those start, is in the job from its first
//! instruction. Closing a session ends the console programs in the job (the shell, and say a
//! `cargo build` or a dev server it runs) and closes the pseudoconsole; windowed programs the
//! shell opened, such as an editor, are left open, as Windows Terminal leaves them when a tab
//! closes. If Kestrel itself dies, its ends of the pseudoconsole's pipes close, the console
//! host exits and Windows ends the console programs attached to it.
//!
//! Output is read on a thread of its own and handed to a callback as it arrives; once the
//! shell has exited, the pseudoconsole is closed (which flushes its last output) and the
//! callback for the exit is called after the last output.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList,
    QueryInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, OpenProcess, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::pe;

/// How long the end of a session's output waits for its shell to be gone, to report its code.
const EXIT_WAIT_MS: u32 = 5000;

/// Columns and rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    /// Kept within what a console takes: at least 2 by 1, at most 1000 by 500.
    pub fn clamped(self) -> Size {
        Size { cols: self.cols.clamp(2, 1000), rows: self.rows.clamp(1, 500) }
    }

    fn coord(self) -> COORD {
        let size = self.clamped();
        COORD { X: size.cols as i16, Y: size.rows as i16 }
    }
}

/// One argument of a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// Quoted as `CommandLineToArgvW` reads it back, if it needs to be.
    Quoted(String),
    /// Passed as it is: for `cmd /k <command>`, which takes the rest of the line literally.
    Raw(String),
}

/// What to start, where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<Arg>,
    pub cwd: PathBuf,
    /// Variables to set (`Some`) or remove (`None`) in the inherited environment.
    pub env: Vec<(String, Option<String>)>,
}

/// Quotes one argument the way `CommandLineToArgvW` reads it back.
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

impl Command {
    /// The command line CreateProcess gets.
    pub fn command_line(&self) -> String {
        let mut line = quote(&self.program.to_string_lossy());
        for arg in &self.args {
            line.push(' ');
            match arg {
                Arg::Quoted(text) => line.push_str(&quote(text)),
                Arg::Raw(text) => line.push_str(text),
            }
        }
        line
    }

    /// The environment block: this process's, changed as asked, sorted as Windows wants, or
    /// `None` to inherit it unchanged.
    fn environment(&self) -> Option<Vec<u16>> {
        if self.env.is_empty() {
            return None;
        }
        // Names and values as Windows has them (not necessarily valid Unicode).
        let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
        for (name, value) in &self.env {
            vars.retain(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case(name));
            if let Some(value) = value {
                vars.push((name.into(), value.into()));
            }
        }
        vars.sort_by_cached_key(|(name, _)| name.to_string_lossy().to_uppercase());
        let mut block = Vec::new();
        for (name, value) in vars {
            block.extend(name.encode_wide());
            block.push(u16::from(b'='));
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        Some(block)
    }
}

fn wide(path: &OsStr) -> Vec<u16> {
    path.encode_wide().chain([0]).collect()
}

/// A kernel handle, closed when dropped.
struct Handle(HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: owned, closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A pseudoconsole, closed when dropped. Closing it may wait until its last output has been
/// read, so it is dropped only on threads that may wait, never while its output goes unread.
struct Pseudo(HPCON);

// SAFETY: a pseudoconsole handle may be used and closed from any thread.
unsafe impl Send for Pseudo {}

impl Drop for Pseudo {
    fn drop(&mut self) {
        // SAFETY: created by CreatePseudoConsole, closed once.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

fn pipe() -> io::Result<(Handle, Handle)> {
    let (mut read, mut write) = (null_mut(), null_mut());
    // SAFETY: plain FFI; both handles are owned by the returned values.
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Handle(read), Handle(write)))
}

struct Inner {
    pid: u32,
    process: Handle,
    job: Option<Handle>,
    input: Mutex<Option<Handle>>,
    console: Mutex<Option<Pseudo>>,
}

impl Inner {
    /// Closes the pseudoconsole (once): Windows ends the console programs still attached, the
    /// console host flushes what it has left and exits, and the reader sees the end.
    fn close_console(&self) {
        let console = self.console.lock().take();
        drop(self.input.lock().take());
        drop(console);
    }

    /// The processes in the job: the shell and everything started from it still running.
    fn processes(&self) -> Vec<u32> {
        let Some(job) = &self.job else { return vec![self.pid] };
        // Room for 1024 ids after the two counts.
        const IDS: usize = 1024;
        let first = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList) / size_of::<usize>();
        let mut buffer = vec![0usize; first + IDS];
        let bytes = (buffer.len() * size_of::<usize>()) as u32;
        // SAFETY: `buffer` is large and aligned enough for the list header and 1024 ids.
        let ok = unsafe { QueryInformationJobObject(job.0, JobObjectBasicProcessIdList, buffer.as_mut_ptr().cast(), bytes, null_mut()) };
        if ok == 0 {
            return Vec::new();
        }
        // SAFETY: the call filled in a JOBOBJECT_BASIC_PROCESS_ID_LIST at the buffer's start.
        let list = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
        let count = (list.NumberOfProcessIdsInList as usize).min(IDS);
        buffer[first..first + count].iter().map(|&pid| pid as u32).collect()
    }

    /// Ends every console program in the job; windowed programs are left running.
    fn end_console_programs(&self) {
        for pid in self.processes() {
            // SAFETY: plain FFI; the handle is closed by `Handle`.
            let process = Handle(unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
            if process.0.is_null() {
                continue;
            }
            // A program whose kind can't be told is ended: it is the session's own.
            let windowed = image_path(&process).is_some_and(|path| pe::is_windowed(&path));
            if !windowed {
                // SAFETY: `process` is a valid handle with terminate access.
                unsafe { TerminateProcess(process.0, 1) };
            }
        }
    }
}

fn image_path(process: &Handle) -> Option<PathBuf> {
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` characters; the handle is valid.
    let ok = unsafe { QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut length) };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
}

/// A running shell in a pseudoconsole.
pub struct Session {
    inner: Arc<Inner>,
    closed: AtomicBool,
}

/// What a session reports: output as it arrives, then once the exit.
pub enum Event<'a> {
    Output(&'a [u8]),
    /// The shell exited (or was ended), with its exit code if known. Nothing follows.
    Exit(Option<u32>),
}

impl Session {
    /// Starts `command` in a new pseudoconsole of `size`. `on_event` is called from a thread of
    /// the session's own, for each piece of output and finally for the exit.
    pub fn spawn(command: &Command, size: Size, mut on_event: impl FnMut(Event) + Send + 'static) -> io::Result<Session> {
        if !command.cwd.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a folder", command.cwd.display())));
        }
        let (input_read, input_write) = pipe()?;
        let (output_read, output_write) = pipe()?;
        let mut hpc: HPCON = 0;
        // SAFETY: the pipe ends are valid; the pseudoconsole duplicates them.
        let hr = unsafe { CreatePseudoConsole(size.coord(), input_read.0, output_write.0, 0, &mut hpc) };
        if hr < 0 {
            return Err(io::Error::other(format!("couldn't create a pseudoconsole (0x{:08x})", hr as u32)));
        }
        // The pseudoconsole holds its own ends now.
        drop(input_read);
        drop(output_write);
        let console = Pseudo(hpc);

        let started = start_process(command, hpc);
        let (process, thread, pid) = match started {
            Ok(started) => started,
            Err(error) => {
                // Unread output must not hold up closing the pseudoconsole: the read end goes first.
                drop(output_read);
                drop(console);
                return Err(error);
            }
        };
        // SAFETY: plain FFI; the job is owned by `Handle`.
        let job = Handle(unsafe { CreateJobObjectW(null(), null()) });
        // SAFETY: both handles are valid; the process has not run yet.
        let job = (!job.0.is_null() && unsafe { AssignProcessToJobObject(job.0, process.0) } != 0).then_some(job);
        // SAFETY: the thread was created suspended and is resumed once.
        unsafe { ResumeThread(thread.0) };
        drop(thread);

        let inner = Arc::new(Inner { pid, process, job, input: Mutex::new(Some(input_write)), console: Mutex::new(Some(console)) });

        // The shell's end: close the pseudoconsole, so its last output comes through and the
        // reader below reaches the end.
        let waiting = inner.clone();
        std::thread::Builder::new().name(format!("kestrel-term-wait-{pid}")).spawn(move || {
            // SAFETY: the process handle stays open while `waiting` holds it.
            unsafe { WaitForSingleObject(waiting.process.0, INFINITE) };
            waiting.close_console();
        })?;

        let reading = inner.clone();
        std::thread::Builder::new().name(format!("kestrel-term-read-{pid}")).spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                // SAFETY: `buffer` is valid for its length; the handle is owned by `output_read`.
                let ok = unsafe { ReadFile(output_read.0, buffer.as_mut_ptr(), buffer.len() as u32, &mut read, null_mut()) };
                if ok == 0 || read == 0 {
                    break;
                }
                on_event(Event::Output(&buffer[..read as usize]));
            }
            drop(output_read);
            // The output ends once the pseudoconsole has closed, which a session being ended
            // can do a moment before its shell is gone: wait for that, then read its code.
            let mut status = 0u32;
            // SAFETY: the process handle stays open while `reading` holds it.
            let exited = unsafe { WaitForSingleObject(reading.process.0, EXIT_WAIT_MS) } == 0
                && unsafe { GetExitCodeProcess(reading.process.0, &mut status) } != 0;
            drop(reading);
            on_event(Event::Exit(exited.then_some(status)));
        })?;
        Ok(Session { inner, closed: AtomicBool::new(false) })
    }

    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    /// The processes the shell has started and that still run, the shell included.
    pub fn processes(&self) -> Vec<u32> {
        self.inner.processes()
    }

    /// Sends keys (or pasted text) to the shell.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        self.writer().write(data)
    }

    /// A handle that writes to the shell, for writing without holding whatever keeps the
    /// session: a write waits while the shell is not reading.
    pub fn writer(&self) -> Writer {
        Writer(self.inner.clone())
    }

    pub fn resize(&self, size: Size) -> io::Result<()> {
        let console = self.inner.console.lock();
        let Some(console) = console.as_ref() else { return Ok(()) };
        // SAFETY: the pseudoconsole is open while the lock is held.
        let hr = unsafe { ResizePseudoConsole(console.0, size.coord()) };
        if hr < 0 { Err(io::Error::other(format!("couldn't resize the terminal (0x{:08x})", hr as u32))) } else { Ok(()) }
    }

    /// Ends the console programs the shell started (and the shell), and closes the
    /// pseudoconsole, without waiting: the closing finishes on a thread of its own.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.end_console_programs();
        let inner = self.inner.clone();
        let _ = std::thread::Builder::new().name("kestrel-term-close".into()).spawn(move || inner.close_console());
    }

    /// Whether the shell is still running.
    pub fn is_running(&self) -> bool {
        // SAFETY: the process handle is open while the session exists.
        unsafe { WaitForSingleObject(self.inner.process.0, 0) != 0 }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// Writes to a session's shell; see [`Session::writer`].
pub struct Writer(Arc<Inner>);

impl Writer {
    pub fn write(&self, mut data: &[u8]) -> io::Result<()> {
        let input = self.0.input.lock();
        let Some(input) = input.as_ref() else { return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal has closed")) };
        while !data.is_empty() {
            let mut written = 0u32;
            let chunk = data.len().min(1 << 20) as u32;
            // SAFETY: `data` is valid for `chunk` bytes; the handle is open while the lock is held.
            if unsafe { WriteFile(input.0, data.as_ptr(), chunk, &mut written, null_mut()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            data = &data[written as usize..];
        }
        Ok(())
    }
}

/// Starts `command` suspended, attached to the pseudoconsole `hpc`. Returns the process and
/// its first thread, and the process id.
fn start_process(command: &Command, hpc: HPCON) -> io::Result<(Handle, Handle, u32)> {
    let mut size = 0usize;
    // SAFETY: asks only for the size needed (the call fails by design).
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
    let mut list = vec![0u64; size.div_ceil(8)];
    let attributes = list.as_mut_ptr().cast();
    // SAFETY: `list` holds `size` bytes, suitably aligned.
    if unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // The attribute's value is the pseudoconsole handle itself.
    // SAFETY: the list was initialised above; `hpc` stays open until the process has started.
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attributes,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const core::ffi::c_void,
            size_of::<HPCON>(),
            null_mut(),
            null(),
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        // SAFETY: initialised above.
        unsafe { DeleteProcThreadAttributeList(attributes) };
        return Err(error);
    }
    // SAFETY: STARTUPINFOEXW is plain data, valid when zeroed.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes;
    // Standard handles are named, as invalid, so the shell never inherits Kestrel's own
    // (redirected in development) in place of the pseudoconsole's.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

    let application = wide(command.program.as_os_str());
    let mut line: Vec<u16> = OsStr::new(&command.command_line()).encode_wide().chain([0]).collect();
    let cwd = wide(command.cwd.as_os_str());
    let environment = command.environment();
    // SAFETY: plain data, filled in by CreateProcessW.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid for the call; the command line buffer is writable, as
    // CreateProcessW requires.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            environment.as_ref().map_or(null(), |block| block.as_ptr().cast()),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: initialised above, no longer needed once the process exists (or failed to).
    unsafe { DeleteProcThreadAttributeList(attributes) };
    if created == 0 {
        return Err(io::Error::new(error.kind(), format!("couldn't start {}: {error}", command.program.display())));
    }
    Ok((Handle(info.hProcess), Handle(info.hThread), info.dwProcessId))
}

/// Whether a process is still running, by id. For tests and the e2e hooks.
pub fn is_alive(pid: u32) -> bool {
    // SAFETY: plain FFI; the handle is closed by `Handle`.
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE, 0, pid) });
    // SAFETY: valid handle for the call.
    !process.0.is_null() && unsafe { WaitForSingleObject(process.0, 0) } != 0
}
