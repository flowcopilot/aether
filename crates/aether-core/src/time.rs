//! Whole seconds since the Unix epoch, the form every signed field, wire
//! field and database column uses for a time.
//!
//! A conversion returns `None` where it has no answer, and the caller picks
//! what that means. The clock functions pick once for every caller: a clock
//! set before 1970 reads as the latest time, so every expiry check fails
//! closed.

use std::time::{Duration, SystemTime};

/// The current time in seconds since the epoch, as the database stores it.
#[must_use]
pub fn now() -> i64 {
    unix_timestamp(SystemTime::now()).unwrap_or(i64::MAX)
}

/// The current time in seconds since the epoch.
#[must_use]
pub fn now_seconds() -> u64 {
    unix_seconds(SystemTime::now()).unwrap_or(u64::MAX)
}

/// Seconds since the epoch, or `None` for a time before it.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH).ok().map(|since| since.as_secs())
}

/// Seconds since the epoch as the database stores them, or `None` for a
/// time before the epoch or past `i64::MAX` seconds.
#[must_use]
pub fn unix_timestamp(time: SystemTime) -> Option<i64> {
    unix_seconds(time).and_then(|seconds| i64::try_from(seconds).ok())
}

/// The time `seconds` after the epoch, or `None` when `SystemTime` cannot
/// hold it.
#[must_use]
pub fn from_unix_seconds(seconds: u64) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{unix_seconds, unix_timestamp};

    #[test]
    fn a_time_before_the_epoch_has_no_seconds() {
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_seconds(before), None);
        assert_eq!(unix_timestamp(before), None);
    }
}
