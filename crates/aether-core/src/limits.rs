//! Limits that more than one crate checks.

use crate::named;

/// The limits of the task queue and of the tasks that run from it.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum QueueLimitId {
    /// How many tasks wait in all.
    Global,
    /// How many tasks wait for each caller endpoint.
    PerPeer,
    /// How many tasks this node runs at once, for all callers together.
    Running,
}

named!(QueueLimitId {
    Global => "queue_global",
    PerPeer => "queue_per_peer",
    Running => "running_tasks",
});

impl QueueLimitId {
    /// Every limit, in the order that the node shows them.
    pub const ALL: [Self; 3] = [Self::Global, Self::PerPeer, Self::Running];

    /// The smallest value that the owner can set.
    #[must_use]
    pub const fn minimum(self) -> u32 {
        1
    }

    /// The largest value that the owner can set. `None`: no fixed maximum.
    #[must_use]
    pub const fn maximum(self) -> Option<u32> {
        match self {
            Self::Global | Self::PerPeer => None,
            Self::Running => Some(MAX_RUNNING_TASKS),
        }
    }

    /// Whether the owner can set this limit to `value`.
    #[must_use]
    pub fn accepts(self, value: u32) -> bool {
        value >= self.minimum() && self.maximum().is_none_or(|maximum| value <= maximum)
    }
}

/// How many tasks a node runs at once unless its owner sets more. One keeps
/// the behavior of a node before the limit existed.
pub const DEFAULT_RUNNING_TASKS: u32 = 1;

/// The most tasks that the owner can let a node run at once.
///
/// Each running task is a turn of the agent (for Codex its own
/// `codex app-server` process and task folder) with a lease-renewal thread,
/// and every store write of the running tasks goes through one connection.
/// 32 is far above what one computer's agent runs well, and it keeps a
/// typing mistake from starting hundreds of processes.
pub const MAX_RUNNING_TASKS: u32 = 32;

#[cfg(test)]
mod tests {
    use super::{MAX_RUNNING_TASKS, QueueLimitId};

    #[test]
    fn the_running_limit_has_a_floor_and_a_ceiling() {
        assert!(!QueueLimitId::Running.accepts(0));
        assert!(QueueLimitId::Running.accepts(1));
        assert!(QueueLimitId::Running.accepts(MAX_RUNNING_TASKS));
        assert!(!QueueLimitId::Running.accepts(MAX_RUNNING_TASKS.saturating_add(1)));
    }

    #[test]
    fn a_queue_limit_has_only_a_floor() {
        assert!(!QueueLimitId::Global.accepts(0));
        assert!(QueueLimitId::PerPeer.accepts(u32::MAX));
    }

    #[test]
    fn every_limit_has_its_own_name() {
        for id in QueueLimitId::ALL {
            assert_eq!(QueueLimitId::parse(id.as_str()), Some(id));
        }
    }
}
