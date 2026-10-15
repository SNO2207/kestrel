//! Runs the helper the way Kestrel does and checks what Windows reports.
//!
//! Menu text follows the system language, so the checks use canonical verbs (`open`, `copy`,
//! `properties`), which do not. Every file lives in a scratch folder under the temp
//! directory. One test replaces the clipboard's contents, as Copy in Explorer would.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const HELPER: &str = env!("CARGO_BIN_EXE_kestrel-shellhost");

/// A scratch folder under the temp directory, emptied first; each test has its own.
fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kestrel-shellhost-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn helper(args: &[&str]) -> Output {
    Command::new(HELPER).args(args).stdin(Stdio::null()).output().expect("the helper starts")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The canonical verbs in a `verbs` listing, submenus included.
fn verbs_of(items: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for item in items.as_array().expect("a list of items") {
        if let Some(verb) = item["verb"].as_str() {
            found.push(verb.to_lowercase());
        }
        if let Some(children) = item.get("items") {
            found.extend(verbs_of(children));
        }
    }
    found
}

fn listed_verbs(args: &[&str]) -> Vec<String> {
    let output = helper(args);
    assert!(output.status.success(), "verbs failed: {}", stderr(&output));
    let items: Value = serde_json::from_slice(&output.stdout).expect("verbs prints JSON");
    verbs_of(&items)
}

fn assert_has(verbs: &[String], expected: &[&str]) {
    for verb in expected {
        assert!(verbs.iter().any(|found| found == verb), "{verb} is missing from {verbs:?}");
    }
}

#[test]
fn lists_the_canonical_verbs_of_a_file() {
    let dir = sandbox("file");
    let file = dir.join("notes.txt");
    fs::write(&file, "notes").unwrap();

    let output = helper(&["verbs", "--", text(&file)]);
    assert!(output.status.success(), "{}", stderr(&output));
    let items: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_has(&verbs_of(&items), &["open", "cut", "copy", "delete", "rename", "properties"]);

    // Every item has the fields Kestrel and the tests rely on, and exactly one is the default.
    let list = items.as_array().unwrap();
    for item in list {
        assert!(item["text"].is_string() && item["submenu"].is_boolean() && item["disabled"].is_boolean());
        assert!(item["submenu"].as_bool().unwrap() || item["id"].is_u64(), "{item}");
    }
    let defaults: Vec<_> = list.iter().filter(|item| item["default"] == true).collect();
    assert_eq!(defaults.len(), 1, "{defaults:?}");
    assert_eq!(defaults[0]["verb"], "open");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn lists_verbs_for_several_items_and_for_folders() {
    let dir = sandbox("several");
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("b.md"), "b").unwrap();
    fs::create_dir(dir.join("folder")).unwrap();

    let several = listed_verbs(&["verbs", "--", text(&dir.join("a.txt")), text(&dir.join("b.md"))]);
    assert_has(&several, &["copy", "cut", "delete", "properties"]);
    let folder = listed_verbs(&["verbs", "--", text(&dir.join("folder"))]);
    assert_has(&folder, &["open", "copy", "delete", "properties"]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn lists_the_menu_of_a_folder_background() {
    let dir = sandbox("background");
    let verbs = listed_verbs(&["verbs", "--background", "--", text(&dir)]);
    assert_has(&verbs, &["properties"]);
    // The item commands of the folder itself are not on its background menu.
    assert!(!verbs.iter().any(|verb| verb == "delete" || verb == "rename"), "{verbs:?}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reads_paths_from_standard_input() {
    let dir = sandbox("stdin");
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("b.txt"), "b").unwrap();
    let mut child = Command::new(HELPER)
        .args(["verbs", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let list = format!("{}\r\n{}\r\n", text(&dir.join("a.txt")), text(&dir.join("b.txt")));
    child.stdin.take().unwrap().write_all(list.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_has(&verbs_of(&serde_json::from_slice(&output.stdout).unwrap()), &["copy", "delete"]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn refuses_items_from_different_folders() {
    let dir = sandbox("folders");
    fs::create_dir(dir.join("one")).unwrap();
    fs::create_dir(dir.join("two")).unwrap();
    fs::write(dir.join("one/a.txt"), "a").unwrap();
    fs::write(dir.join("two/b.txt"), "b").unwrap();

    let output = helper(&["verbs", "--", text(&dir.join("one/a.txt")), text(&dir.join("two/b.txt"))]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("different folders"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reports_missing_items_and_bad_command_lines() {
    let dir = sandbox("missing");
    let missing = dir.join("not-there.txt");
    let output = helper(&["verbs", "--", text(&missing)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains(text(&missing)), "{}", stderr(&output));

    for args in [&["verbs"][..], &["fly", "--", "C:\\"], &["menu", "--", text(&dir)], &["openwith"]] {
        let output = helper(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("usage"), "{args:?}: {}", stderr(&output));
    }

    // Open With checks its file before showing anything.
    let output = helper(&["openwith", text(&missing)]);
    assert_eq!(output.status.code(), Some(1));
    let output = helper(&["openwith", text(&dir)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("folder"), "{}", stderr(&output));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unknown_verb_fails_cleanly() {
    let dir = sandbox("unknown-verb");
    let file = dir.join("a.txt");
    fs::write(&file, "a").unwrap();
    let output = helper(&["invoke", "--verb", "kestrel-no-such-verb", "--", text(&file)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("kestrel-no-such-verb"), "{}", stderr(&output));
    assert!(file.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invoking_copy_leaves_the_file_on_the_clipboard() {
    let dir = sandbox("copy");
    let file = dir.join("copied.txt");
    fs::write(&file, "copied").unwrap();

    let output = helper(&["invoke", "--verb", "copy", "--", text(&file)]);
    assert!(output.status.success(), "{}", stderr(&output));
    // The helper has exited by now, so the files must have been rendered onto the clipboard
    // rather than left in a data object that died with it.
    assert_eq!(clipboard::files(), vec![text(&file).to_owned()]);
    fs::remove_dir_all(&dir).unwrap();
}

/// Properties opens its dialog on a thread of its own and returns at once; the helper must
/// keep running until the dialog closes, or the dialog would vanish with it.
#[test]
#[ignore = "shows a Properties dialog for a moment; run with --ignored"]
fn a_properties_dialog_keeps_the_helper_running_until_it_closes() {
    let dir = sandbox("properties");
    let file = dir.join("props.txt");
    fs::write(&file, "props").unwrap();
    let mut child = Command::new(HELPER)
        .args(["invoke", "--verb", "properties", "--", text(&file)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let started = std::time::Instant::now();
    let dialog = loop {
        if let Some(window) = desktop::visible_window_of(child.id()) {
            break window;
        }
        assert!(started.elapsed().as_secs() < 15, "the Properties dialog did not open");
        assert!(child.try_wait().unwrap().is_none(), "the helper exited before showing the dialog");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(child.try_wait().unwrap().is_none(), "the helper exited while its dialog was open");

    desktop::close(dialog);
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed().as_secs() < 15, "the helper kept running after its dialog closed");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(status.success(), "{status:?}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_crash_hook_exits_like_a_crash() {
    let output = Command::new(HELPER)
        .args(["verbs", "--", "C:\\"])
        .env("KESTREL_SHELLHOST_TEST_CRASH", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code().map(|code| code as u32), Some(0xC000_0005));
}

#[test]
fn the_manifest_is_embedded() {
    let binary = fs::read(HELPER).unwrap();
    let contains = |needle: &str| binary.windows(needle.len()).any(|window| window == needle.as_bytes());
    assert!(contains("PerMonitorV2"), "per-monitor DPI awareness");
    assert!(contains("Microsoft.Windows.Common-Controls"), "common controls 6");
    assert!(contains("asInvoker"), "never asks for elevation");
}

mod desktop {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };

    /// A visible top-level window of the process, if it has one.
    pub fn visible_window_of(process: u32) -> Option<HWND> {
        struct Search {
            process: u32,
            found: HWND,
        }
        unsafe extern "system" fn visit(hwnd: HWND, search: LPARAM) -> i32 {
            // SAFETY: `search` is the `Search` passed below, alive during EnumWindows.
            unsafe {
                let search = &mut *(search as *mut Search);
                let mut owner = 0;
                GetWindowThreadProcessId(hwnd, &mut owner);
                if owner == search.process && IsWindowVisible(hwnd) != 0 {
                    search.found = hwnd;
                    return 0;
                }
            }
            1
        }
        let mut search = Search { process, found: std::ptr::null_mut() };
        // SAFETY: the callback only runs during the call.
        unsafe { EnumWindows(Some(visit), (&raw mut search) as LPARAM) };
        (!search.found.is_null()).then_some(search.found)
    }

    /// Asks a window to close, as its close button would.
    pub fn close(window: HWND) {
        // SAFETY: plain FFI call; a window that has gone ignores it.
        unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    }
}

mod clipboard {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    const CF_HDROP: u32 = 15;

    /// The files on the clipboard (CF_HDROP).
    pub fn files() -> Vec<String> {
        // Another program may hold the clipboard for a moment.
        // SAFETY: plain FFI call.
        let opened = (0..50).any(|_| unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        });
        assert!(opened, "the clipboard is in use");
        // SAFETY: the clipboard is open, so the handle stays valid until it is closed below.
        let files = unsafe {
            let drop = GetClipboardData(CF_HDROP);
            if drop.is_null() {
                Vec::new()
            } else {
                let count = DragQueryFileW(drop as _, u32::MAX, std::ptr::null_mut(), 0);
                (0..count)
                    .map(|i| {
                        let length = DragQueryFileW(drop as _, i, std::ptr::null_mut(), 0) as usize;
                        let mut buffer = vec![0u16; length + 1];
                        DragQueryFileW(drop as _, i, buffer.as_mut_ptr(), buffer.len() as u32);
                        String::from_utf16_lossy(&buffer[..length])
                    })
                    .collect()
            }
        };
        // SAFETY: opened above on this thread.
        unsafe { CloseClipboard() };
        files
    }
}
