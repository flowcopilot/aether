//! Whole seconds since the Unix epoch, the form every signed field, wire
//! field and database column uses for a time.
//!
//! Each function returns `None` where the conversion has no answer, and the
//! caller picks what that means: an error, a clamp, or a panic in a test.

use std::time::{Duration, SystemTime};

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
