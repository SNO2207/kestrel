//! Running a cleanup tool (`cargo clean`, `pnpm store prune`…) with its output captured, and
//! opening Windows' own cleanup.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::os::windows::io::AsRawHandle;

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// A job holding the tool and whatever it starts, so a tool stopped for taking too long takes
/// its own helpers with it.
struct Job(HANDLE);

impl Job {
    fn holding(child: &std::process::Child) -> Option<Job> {
        // SAFETY: a job with default security and no name; the child's handle is valid while
        // `child` lives, and the job is closed when dropped.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            if AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Job(job))
        }
    }

    fn terminate(&self) {
        // SAFETY: the job handle is open until drop.
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: opened by `holding` and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// `CREATE_NO_WINDOW`: the tool's console never flashes up.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Output kept from the end of a long run.
const MAX_OUTPUT: usize = 64 * 1024;

pub struct Ran {
    pub status: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

/// Runs `program` (a full path) with `args` in `dir`, waits for it at most `timeout`, and
/// returns its exit code and what it printed, both streams together in the order they came.
pub fn run(program: &Path, args: &[String], dir: &Path, timeout: Duration) -> Result<Ran, String> {
    let name = program.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Couldn't start {name}: {e}"))?;
    let job = Job::holding(&child);
    let output = Arc::new(Mutex::new(Vec::new()));
    let (finished, readers_done) = std::sync::mpsc::channel::<()>();
    let streams: Vec<Box<dyn Read + Send>> = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let readers = streams.len();
    for mut stream in streams {
        let output = output.clone();
        let finished = finished.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(n) = stream.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut out = output.lock();
                out.extend_from_slice(&buffer[..n]);
                if out.len() > 4 * MAX_OUTPUT {
                    let excess = out.len() - MAX_OUTPUT;
                    out.drain(..excess);
                }
            }
            let _ = finished.send(());
        });
    }
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                match &job {
                    Some(job) => job.terminate(),
                    None => {
                        let _ = child.kill();
                    }
                }
                break child.wait().ok().and_then(|s| s.code());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("{name} failed: {e}")),
        }
    };
    // A program the tool started in the background (a build server, say) may still hold the
    // output open; what came so far is enough.
    for _ in 0..readers {
        if readers_done.recv_timeout(Duration::from_secs(2)).is_err() {
            break;
        }
    }
    let bytes = std::mem::take(&mut *output.lock());
    let start = bytes.len().saturating_sub(MAX_OUTPUT);
    Ok(Ran { status, timed_out, output: String::from_utf8_lossy(&bytes[start..]).trim_end().to_owned() })
}

/// Opens a file or a settings page as a double-click would.
pub fn open(file: &str, params: &str) -> Result<(), String> {
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let (verb, target, parameters) = (wide("open"), wide(file), wide(params));
    // SAFETY: every string is NUL-terminated and outlives the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            if params.is_empty() { std::ptr::null() } else { parameters.as_ptr() },
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success as a value greater than 32.
    if result as usize > 32 { Ok(()) } else { Err(format!("Windows couldn't open {file}")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_runs_in_its_folder_and_its_output_comes_back() {
        let dir = std::env::temp_dir().join(format!("kestrel-cleanup-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        let args: Vec<String> = ["/d", "/c", "echo out& echo err 1>&2& cd& exit 3"].iter().map(|a| a.to_string()).collect();
        let ran = run(Path::new(&cmd), &args, &dir, Duration::from_secs(20)).unwrap();
        assert_eq!(ran.status, Some(3));
        assert!(!ran.timed_out);
        assert!(ran.output.contains("out") && ran.output.contains("err"), "{}", ran.output);
        assert!(ran.output.to_lowercase().contains(&dir.to_string_lossy().to_lowercase()), "{}", ran.output);

        let slow: Vec<String> = ["/d", "/c", "ping -n 30 127.0.0.1 >nul"].iter().map(|a| a.to_string()).collect();
        let ran = run(Path::new(&cmd), &slow, &dir, Duration::from_millis(300)).unwrap();
        assert!(ran.timed_out);
        // What the tool started went with it: nothing holds its folder any more.
        let removed = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            std::fs::remove_dir_all(&dir).is_ok()
        });
        assert!(removed, "the timed-out tool's own child still runs in {}", dir.display());
    }
}
