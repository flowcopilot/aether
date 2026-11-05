//! The counts on the node dashboard, read from the modules that own them.

use crate::{NodeStore, StoreError, now};

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct NodeSummary {
    pub agents: usize,
    pub active_connections: usize,
    pub tasks: usize,
    pub queued_tasks: usize,
}

impl NodeStore {
    pub fn summary(&self) -> Result<NodeSummary, StoreError> {
        let now = now();
        let tasks = self.task_counts()?;
        Ok(NodeSummary {
            agents: self
                .live_record_count(now)?
                .saturating_add(self.active_member_count(now)?),
            active_connections: tasks.active_callers,
            tasks: tasks.all,
            queued_tasks: tasks.queued,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{test_support::*, *};

    #[test]
    fn summary_counts_agents_connections_and_tasks() {
        let (mut store, database) = store();
        let record = |record: &[u8], expires_at| SignedRecord {
            scope_id: "scope".to_owned(),
            agent_id: "agent".to_owned(),
            record: record.to_vec(),
            expires_at,
        };
        store.put_contact_record(&record(b"record", 2_000_000_000)).expect("record");
        store.put_contact_record(&record(b"renewed", 2_000_000_001)).expect("renewal");
        assert_eq!(store.contact_record("agent").expect("record").record, b"renewed");
        store
            .admit(&input("task", "peer"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        let summary = store.summary().expect("summary must load");
        assert_eq!(summary.agents, 1);
        assert_eq!(summary.tasks, 1);
        assert_eq!(summary.queued_tasks, 1);
        assert_eq!(summary.active_connections, 0);
        store
            .lease_next("worker", "execution", 2_000_000_000)
            .expect("task must lease");
        assert_eq!(store.summary().expect("summary must load").active_connections, 1);
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }
}
