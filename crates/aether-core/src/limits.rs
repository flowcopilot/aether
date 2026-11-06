//! Limits that more than one crate checks.

use crate::named;

/// The most days a direct grant lasts.
pub const MAX_DIRECT_ACCESS_DAYS: u32 = 365;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum QueueLimitId {
    Global,
    PerPeer,
}

named!(QueueLimitId {
    Global => "queue_global",
    PerPeer => "queue_per_peer",
});
