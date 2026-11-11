//! Limits that more than one crate checks.

use crate::named;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum QueueLimitId {
    Global,
    PerPeer,
}

named!(QueueLimitId {
    Global => "queue_global",
    PerPeer => "queue_per_peer",
});
