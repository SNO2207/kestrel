//! Tabs between windows, as Explorer's: a tab dragged out of its strip and let go opens in a new
//! window there, and one let go over another Kestrel window's strip moves into that window.
//!
//! The window the tab came from asks [`tab_drop`] once the button is up. Where the pointer is
//! comes from Windows (`GetCursorPos`, `WindowFromPoint`), not from the page: the pointer may be
//! over another window, or none. Whether a point of another window is on its strip only that
//! window's page can tell (one pane or two, how wide its tabs are), so it is asked
//! (`kestrel:tab-arrive`) and answers with [`tab_arrival`]; one that says no, or doesn't answer
//! in time, leaves the tab to a window of its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, Window};
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetCursorPos, WindowFromPoint};

use crate::parity::appwindows;

/// How long another window has to say whether it takes a tab.
const ANSWER_WAIT: Duration = Duration::from_secs(2);
/// Where a new window goes relative to the pointer, so its tab strip is under it.
const GRIP: (f64, f64) = (120.0, 16.0);

/// A tab as it travels: where it is and where it has been.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TabState {
    pub path: String,
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub cursor: i64,
}

/// Where a tab was let go, as a test gives it: over the page of the window `label`, at a point in
/// CSS pixels, or with no label over no Kestrel window at all.
#[derive(Clone, Debug, Deserialize)]
pub struct DropPoint {
    pub label: Option<String>,
    pub x: f64,
    pub y: f64,
}

/// What another window is asked: would it take this tab, let go at this point of its page?
#[derive(Clone, Debug, Serialize)]
struct Arrival {
    id: u32,
    x: f64,
    y: f64,
    tab: TabState,
}

/// Windows asked to take a tab, waiting for their answers.
#[derive(Default)]
pub struct Handoffs {
    next: AtomicU32,
    waiting: Mutex<HashMap<u32, mpsc::Sender<bool>>>,
}

/// The pointer's place on the screen, in physical pixels.
fn cursor() -> Option<POINT> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid place for the answer.
    (unsafe { GetCursorPos(&mut point) } != 0).then_some(point)
}

/// The app window whose page is under `point` (screen, physical pixels), with the point in its
/// page's CSS pixels; `None` over any other program's window, or none.
fn window_at(app: &AppHandle, point: POINT) -> Option<(Window, f64, f64)> {
    // SAFETY: plain questions about windows; a stale handle at worst finds no match below.
    let root = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) };
    if root.is_null() {
        return None;
    }
    let window = appwindows::app_windows(app).into_iter().find(|window| window.hwnd().is_ok_and(|hwnd| hwnd.0 as HWND == root))?;
    let mut client = point;
    // SAFETY: `root` is a live window (it was just found among the app's), and `client` a valid point.
    if unsafe { ScreenToClient(root, &mut client) } == 0 {
        return None;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    Some((window, f64::from(client.x) / scale, f64::from(client.y) / scale))
}

/// Asks `target` to take the tab, let go at (`x`, `y`) of its page, and waits for its answer.
fn hand_over(handoffs: &Handoffs, target: &Window, x: f64, y: f64, tab: TabState) -> bool {
    let id = handoffs.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (send, answer) = mpsc::channel();
    handoffs.waiting.lock().insert(id, send);
    let asked = target.emit_to(target.label(), "kestrel:tab-arrive", Arrival { id, x, y, tab });
    let taken = asked.is_ok() && answer.recv_timeout(ANSWER_WAIT).unwrap_or(false);
    handoffs.waiting.lock().remove(&id);
    if taken {
        // The tab's new window comes forward, as Explorer's does.
        let _ = target.set_focus();
    }
    taken
}

// ---------- commands ----------

/// A tab of `window`'s was let go outside its strip. Returns `moved` when another window took it,
/// `window` when it went to a new window of its own, or `none` when it stays (the last tab of a
/// window, which has nowhere better to be). `alone` says whether it is the window's only tab.
/// `at` stands in for the pointer in development builds, for the end-to-end tests.
#[tauri::command(async)]
pub fn tab_drop(app: AppHandle, window: Window, handoffs: State<Handoffs>, tab: TabState, alone: bool, at: Option<DropPoint>) -> Result<&'static str, String> {
    let given = at.filter(|_| cfg!(debug_assertions));
    let pointer = cursor();
    let target = match &given {
        Some(DropPoint { label: Some(label), x, y }) => appwindows::app_windows(&app).into_iter().find(|w| w.label() == label).map(|w| (w, *x, *y)),
        Some(DropPoint { label: None, .. }) => None,
        None => pointer.and_then(|point| window_at(&app, point)),
    };
    if let Some((other, x, y)) = &target
        && other.label() != window.label()
        && hand_over(&handoffs, other, *x, *y, tab.clone())
    {
        return Ok("moved");
    }
    if alone {
        return Ok("none");
    }
    // A new window where the tab was let go; a test's drop has no pointer to go by.
    let scale = window.scale_factor().unwrap_or(1.0);
    let place = match (&given, pointer) {
        (None, Some(point)) => Some((f64::from(point.x) / scale - GRIP.0, f64::from(point.y) / scale - GRIP.1)),
        _ => None,
    };
    appwindows::open_window(&app, &window, Some(tab.path), place)?;
    Ok("window")
}

/// Another window's answer to `kestrel:tab-arrive`: whether it took the tab.
#[tauri::command]
pub fn tab_arrival(handoffs: State<Handoffs>, id: u32, taken: bool) {
    handoffs.answer(id, taken);
}

impl Handoffs {
    /// Passes a window's answer to the drop waiting for it; an answer that comes too late, or
    /// twice, finds nobody waiting.
    fn answer(&self, id: u32, taken: bool) {
        if let Some(waiting) = self.waiting.lock().remove(&id) {
            let _ = waiting.send(taken);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_travels_with_its_history() {
        let tab: TabState = serde_json::from_str(r#"{"path":"C:\\Users","history":["C:\\","C:\\Users"],"cursor":1}"#).unwrap();
        assert_eq!((tab.path.as_str(), tab.history.len(), tab.cursor), (r"C:\Users", 2, 1));
        // Older pages send only the path.
        let bare: TabState = serde_json::from_str(r#"{"path":"shell:MyComputerFolder"}"#).unwrap();
        assert!(bare.history.is_empty() && bare.cursor == 0);
    }

    #[test]
    fn an_answer_reaches_only_the_window_waiting_for_it() {
        let handoffs = Handoffs::default();
        let (send, receive) = mpsc::channel();
        handoffs.waiting.lock().insert(7, send);
        handoffs.answer(8, true);
        assert!(receive.try_recv().is_err(), "another tab's answer");
        handoffs.answer(7, true);
        assert_eq!(receive.try_recv(), Ok(true));
        assert!(handoffs.waiting.lock().is_empty());
        handoffs.answer(7, false);
    }

    #[test]
    fn the_pointer_is_found() {
        // Wherever it is: asking never fails on a desktop with a pointer.
        assert!(cursor().is_some());
    }
}
