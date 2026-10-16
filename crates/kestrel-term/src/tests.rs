//! Real shells in real pseudoconsoles, in scratch folders under %TEMP%.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::pty::{Arg, Command, Event, Session, Size, is_alive};
use crate::shells;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kestrel-term-test-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cmd(cwd: &Path) -> Command {
    let shells = shells::detect();
    let cmd = shells.iter().find(|s| s.id == "cmd").unwrap();
    shells::launch(cmd, cwd)
}

/// A session whose output is gathered as text, and whose exit is sent on a channel.
struct Watched {
    session: Session,
    text: Arc<Mutex<String>>,
    exit: Receiver<Option<u32>>,
}

fn watch(command: &Command, size: Size) -> Watched {
    let text: Arc<Mutex<String>> = Arc::default();
    let (send, exit) = channel();
    let gathered = text.clone();
    let session = Session::spawn(command, size, move |event| match event {
        Event::Output(bytes) => gathered.lock().push_str(&String::from_utf8_lossy(bytes)),
        Event::Exit(code) => {
            let _ = send.send(code);
        }
    })
    .unwrap();
    Watched { session, text, exit }
}

impl Watched {
    /// Closes the session, waits for everything it ran to end (the shell holds its folder
    /// open), and removes the scratch folder.
    fn finish(self, dir: &Path) {
        let pids = self.session.processes();
        drop(self);
        for pid in pids {
            wait_until(&format!("process {pid} ends"), || !is_alive(pid));
        }
        remove(dir);
    }

    /// Waits for the output to contain `needle` `times` times.
    fn wait_for(&self, needle: &str, times: usize) {
        let started = Instant::now();
        while self.text.lock().matches(needle).count() < times {
            assert!(started.elapsed() < Duration::from_secs(20), "no {needle:?} x{times} in {:?}", self.text.lock());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Removes a scratch folder, trying again for a moment while a process that just ended still
/// has it open.
fn remove(dir: &Path) {
    let started = Instant::now();
    while let Err(error) = std::fs::remove_dir_all(dir) {
        assert!(started.elapsed() < Duration::from_secs(10), "{}: {error}", dir.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

const SIZE: Size = Size { cols: 100, rows: 30 };

#[test]
fn a_shell_runs_in_its_folder_and_answers() {
    let dir = scratch("echo");
    std::fs::write(dir.join("marker-file.txt"), b"x").unwrap();
    let watched = watch(&cmd(&dir), SIZE);
    assert!(watched.session.is_running());
    assert!(watched.session.processes().contains(&watched.session.pid()), "the shell is in its job");
    watched.session.write(b"echo kestrel-says-hello\r").unwrap();
    // Once as typed, once as printed.
    watched.wait_for("kestrel-says-hello", 2);
    watched.session.write(b"dir /b\r").unwrap();
    watched.wait_for("marker-file.txt", 1);
    watched.finish(&dir);
}

#[test]
fn closing_ends_the_shell_and_the_console_programs_it_started() {
    let dir = scratch("close");
    let watched = watch(&cmd(&dir), SIZE);
    // A console program that would run for a minute, started from the shell.
    watched.session.write(b"ping -n 60 127.0.0.1\r").unwrap();
    watched.wait_for("127.0.0.1", 2);
    wait_until("ping joins the shell's job", || watched.session.processes().len() >= 2);
    let pids = watched.session.processes();
    assert!(pids.iter().all(|&pid| is_alive(pid)));
    let started = Instant::now();
    watched.session.close();
    for &pid in &pids {
        wait_until(&format!("process {pid} ends"), || !is_alive(pid));
    }
    // The exit is reported once the last output has come through.
    let code = watched.exit.recv_timeout(Duration::from_secs(10)).expect("the exit is reported");
    assert_eq!(code, Some(1), "ended, not exited");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!watched.session.is_running());
    assert!(watched.session.write(b"echo too late\r").is_err() || watched.session.processes().is_empty());
    watched.finish(&dir);
}

#[test]
fn dropping_a_session_ends_everything_it_started() {
    let dir = scratch("drop");
    let watched = watch(&cmd(&dir), SIZE);
    watched.session.write(b"ping -n 60 127.0.0.1\r").unwrap();
    wait_until("ping starts", || watched.session.processes().len() >= 2);
    let pids = watched.session.processes();
    drop(watched.session);
    for &pid in &pids {
        wait_until(&format!("process {pid} ends"), || !is_alive(pid));
    }
    remove(&dir);
}

#[test]
fn a_shell_that_exits_reports_its_code_after_its_last_output() {
    let dir = scratch("exit");
    let mut command = cmd(&dir);
    command.args = vec![Arg::Quoted("/c".into()), Arg::Raw("echo last words& exit 7".into())];
    let watched = watch(&command, SIZE);
    let code = watched.exit.recv_timeout(Duration::from_secs(20)).expect("the exit is reported");
    assert_eq!(code, Some(7));
    assert!(watched.text.lock().contains("last words"), "{:?}", watched.text.lock());
    watched.finish(&dir);
}

#[test]
fn resizing_reaches_the_shell() {
    let dir = scratch("resize");
    let watched = watch(&cmd(&dir), Size { cols: 80, rows: 25 });
    watched.session.resize(Size { cols: 132, rows: 40 }).unwrap();
    watched.session.write(b"mode con\r").unwrap();
    watched.wait_for("132", 1);
    watched.finish(&dir);
}

#[test]
fn the_environment_can_be_changed_for_the_shell() {
    let dir = scratch("env");
    let mut command = cmd(&dir);
    command.env = vec![("KESTREL_TERM_TEST".into(), Some("set-for-the-shell".into())), ("PATHEXT_NOT_THERE".into(), None)];
    let watched = watch(&command, SIZE);
    watched.session.write(b"echo %KESTREL_TERM_TEST%\r").unwrap();
    watched.wait_for("set-for-the-shell", 1);
    watched.finish(&dir);
}

#[test]
fn scripts_run_in_command_prompt_without_looking_in_the_folder() {
    let dir = scratch("script");
    // A program of the folder's own, named like the command, must not be the one that runs.
    std::fs::write(dir.join("whoami.cmd"), "@echo the folder's copy ran\r\n").unwrap();
    let watched = watch(&shells::run_in_cmd("whoami", &dir), SIZE);
    let user = std::env::var("USERNAME").unwrap().to_lowercase();
    let started = Instant::now();
    while !watched.text.lock().to_lowercase().contains(&user) {
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", watched.text.lock());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!watched.text.lock().contains("the folder's copy ran"));
    // The prompt stays open afterwards, as `cmd /k` keeps it, with the setting cleared again.
    watched.session.write(b"echo [%NoDefaultCurrentDirectoryInExePath%]\r").unwrap();
    watched.wait_for("[%NoDefaultCurrentDirectoryInExePath%]", 2);
    watched.finish(&dir);
}

#[test]
fn a_missing_folder_is_refused() {
    let missing = std::env::temp_dir().join("kestrel-term-test-no-such-folder");
    let error = Session::spawn(&cmd(&missing), SIZE, |_| {}).err().expect("refused");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
