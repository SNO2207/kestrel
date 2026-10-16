//! When the media pass waits by itself: while another app or a game runs full screen, or while
//! battery saver is on. Both are cheap to ask, so the workers ask between items.

use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, QUERY_USER_NOTIFICATION_STATE,
    SHQueryUserNotificationState,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

pub const FULL_SCREEN: &str = "An app is running full screen";
pub const GAME: &str = "A game is running";
pub const PRESENTING: &str = "Presentation mode is on";
pub const BATTERY_SAVER: &str = "Battery saver is on";

/// Why the media pass should wait now, if it should.
pub fn reason_to_wait() -> Option<String> {
    full_screen().or_else(battery_saver).map(str::to_owned)
}

/// A full-screen app, game or presentation in front, unless it is Kestrel itself (F11).
fn full_screen() -> Option<&'static str> {
    let mut state: QUERY_USER_NOTIFICATION_STATE = 0;
    // SAFETY: the call writes one value to `state`.
    if unsafe { SHQueryUserNotificationState(&mut state) } != 0 {
        return None;
    }
    let reason = match state {
        QUNS_RUNNING_D3D_FULL_SCREEN => GAME,
        QUNS_BUSY => FULL_SCREEN,
        QUNS_PRESENTATION_MODE => PRESENTING,
        _ => return None,
    };
    (!foreground_is_ours()).then_some(reason)
}

fn foreground_is_ours() -> bool {
    // SAFETY: plain queries about the foreground window; a null window has no process.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return false;
        }
        let mut process = 0u32;
        GetWindowThreadProcessId(window, &mut process);
        process == std::process::id()
    }
}

fn battery_saver() -> Option<&'static str> {
    // SAFETY: SYSTEM_POWER_STATUS is plain data the call fills.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let known = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    // `SystemStatusFlag` is 1 while battery saver is on.
    (known && status.SystemStatusFlag == 1).then_some(BATTERY_SAVER)
}

#[cfg(test)]
mod tests {
    #[test]
    fn asking_never_fails() {
        // Whatever this machine is doing, the answer is one of the known reasons or none.
        let known = [super::FULL_SCREEN, super::GAME, super::PRESENTING, super::BATTERY_SAVER];
        assert!(super::reason_to_wait().is_none_or(|reason| known.contains(&reason.as_str())));
    }
}
