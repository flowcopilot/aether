//! Limits that more than one crate checks.

/// The most days of access a hub invite or access request gives.
pub const MAX_HUB_ACCESS_DAYS: u32 = 30;

/// The most days a direct grant lasts.
pub const MAX_DIRECT_ACCESS_DAYS: u32 = 365;
