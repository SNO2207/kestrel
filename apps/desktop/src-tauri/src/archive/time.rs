//! Times in archives: zip keeps the local time of day, tar the seconds since 1970 (UTC), 7z a
//! FILETIME. Kestrel works in FILETIME ticks (100 ns since 1601, UTC), as the index does.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime,
};

/// FILETIME ticks at the Unix epoch.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: i64 = 10_000_000;

pub fn from_unix(seconds: i64) -> i64 {
    seconds.saturating_mul(TICKS_PER_SECOND).saturating_add(UNIX_EPOCH_TICKS)
}

pub fn to_unix(ticks: i64) -> i64 {
    (ticks - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_SECOND)
}

/// Milliseconds since 1970, as the window's rows carry times; 0 stays "unknown".
pub fn to_unix_ms(ticks: i64) -> i64 {
    if ticks == 0 { 0 } else { (ticks - UNIX_EPOCH_TICKS) / 10_000 }
}

fn ticks_of(filetime: FILETIME) -> i64 {
    (filetime.dwHighDateTime as i64) << 32 | filetime.dwLowDateTime as i64
}

/// A local date and time, as a zip stores it, in FILETIME ticks; 0 if it isn't a real time.
pub fn from_local(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> i64 {
    let local = SYSTEMTIME {
        wYear: year,
        wMonth: month.into(),
        wDayOfWeek: 0,
        wDay: day.into(),
        wHour: hour.into(),
        wMinute: minute.into(),
        wSecond: second.into(),
        wMilliseconds: 0,
    };
    // SAFETY: all-zero is a valid value for these plain-data structs.
    let (mut utc, mut filetime): (SYSTEMTIME, FILETIME) = unsafe { std::mem::zeroed() };
    // SAFETY: each pointer is to a live struct of the type the call expects; a null time zone
    // means the system's own.
    let ok = unsafe {
        TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) != 0
            && SystemTimeToFileTime(&utc, &mut filetime) != 0
    };
    if ok { ticks_of(filetime) } else { 0 }
}

/// FILETIME ticks as a local date and time: year, month, day, hour, minute, second.
pub fn to_local(ticks: i64) -> Option<(u16, u8, u8, u8, u8, u8)> {
    if ticks <= 0 {
        return None;
    }
    let filetime = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    // SAFETY: all-zero is a valid value for these plain-data structs.
    let (mut utc, mut local): (SYSTEMTIME, SYSTEMTIME) = unsafe { std::mem::zeroed() };
    // SAFETY: each pointer is to a live struct of the type the call expects.
    let ok = unsafe {
        FileTimeToSystemTime(&filetime, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
    };
    ok.then_some((local.wYear, local.wMonth as u8, local.wDay as u8, local.wHour as u8, local.wMinute as u8, local.wSecond as u8))
}

/// FILETIME ticks as a `SystemTime`, for setting a file's time.
pub fn system_time(ticks: i64) -> Option<SystemTime> {
    if ticks <= 0 {
        return None;
    }
    let since = ticks - UNIX_EPOCH_TICKS;
    let duration = Duration::from_nanos(since.unsigned_abs().saturating_mul(100));
    if since >= 0 { UNIX_EPOCH.checked_add(duration) } else { UNIX_EPOCH.checked_sub(duration) }
}

/// A `SystemTime` as FILETIME ticks.
pub fn from_system(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => UNIX_EPOCH_TICKS + (after.as_nanos() / 100) as i64,
        Err(before) => UNIX_EPOCH_TICKS - (before.duration().as_nanos() / 100) as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_convert_both_ways() {
        assert_eq!(to_unix(from_unix(1_700_000_000)), 1_700_000_000);
        assert_eq!(from_system(UNIX_EPOCH), UNIX_EPOCH_TICKS);
        assert_eq!(system_time(from_unix(86_400)), UNIX_EPOCH.checked_add(Duration::from_secs(86_400)));
        let ticks = from_local(2024, 6, 1, 12, 30, 10);
        assert_ne!(ticks, 0);
        assert_eq!(to_local(ticks), Some((2024, 6, 1, 12, 30, 10)));
        assert_eq!(from_local(2024, 13, 1, 0, 0, 0), 0, "not a date");
        assert_eq!(to_unix_ms(0), 0);
    }
}
