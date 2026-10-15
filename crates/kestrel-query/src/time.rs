//! Calendar arithmetic for date filters, without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;
/// Seconds between 1601-01-01 (the FILETIME epoch) and 1970-01-01.
const FILETIME_EPOCH_OFFSET: i64 = 11_644_473_600;

/// The current time and local UTC offset, passed in so that parsing is deterministic in tests.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub now_unix: i64,
    pub utc_offset_secs: i32,
}

impl Clock {
    /// The system clock with the given local UTC offset.
    pub fn system(utc_offset_secs: i32) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        Self { now_unix: now, utc_offset_secs }
    }

    /// The system clock in the machine's current time zone.
    #[cfg(windows)]
    pub fn local() -> Self {
        use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        const DAYLIGHT: u32 = 2;
        let mut zone = TIME_ZONE_INFORMATION::default();
        // SAFETY: `zone` is a valid out-pointer for the duration of the call.
        let state = unsafe { GetTimeZoneInformation(&mut zone) };
        let bias_minutes =
            zone.Bias + if state == DAYLIGHT { zone.DaylightBias } else { zone.StandardBias };
        Self::system(-bias_minutes * 60)
    }

    /// Days since 1970-01-01 of the current local date.
    pub(crate) fn today(self) -> i64 {
        (self.now_unix + self.utc_offset_secs as i64).div_euclid(DAY)
    }

    /// The FILETIME at local midnight starting the given day.
    pub(crate) fn day_start(self, day: i64) -> i64 {
        unix_to_filetime(day * DAY - self.utc_offset_secs as i64)
    }
}

pub(crate) fn unix_to_filetime(unix: i64) -> i64 {
    (unix + FILETIME_EPOCH_OFFSET).saturating_mul(10_000_000)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: `(year, month, day)`.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The first day of the month `offset` months away from the month containing `day`.
pub(crate) fn month_start(day: i64, offset: i64) -> i64 {
    let (year, month, _) = civil_from_days(day);
    let index = year * 12 + month as i64 - 1 + offset;
    days_from_civil(index.div_euclid(12), index.rem_euclid(12) as u32 + 1, 1)
}

/// The Monday on or before `day`. 1970-01-01 was a Thursday.
pub(crate) fn week_start(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 10, 13), 20_739);
        for day in [-800_000, -1, 0, 59, 60, 11_016, 20_739, 50_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn month_and_week_boundaries() {
        let day = days_from_civil(2026, 10, 13); // a Tuesday
        assert_eq!(civil_from_days(week_start(day)), (2026, 10, 12));
        assert_eq!(civil_from_days(month_start(day, 0)), (2026, 10, 1));
        assert_eq!(civil_from_days(month_start(day, -10)), (2025, 12, 1));
        assert_eq!(civil_from_days(month_start(day, 3)), (2027, 1, 1));
    }

    #[test]
    fn filetime_epoch() {
        assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
    }
}
