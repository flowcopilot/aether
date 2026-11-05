use crate::{NodeStore, StoreError, now};

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct NodeSummary {
    pub agents: usize,
    pub grants: usize,
    pub active_connections: usize,
    pub tasks: usize,
    pub queued_tasks: usize,
}

impl NodeStore {
    pub fn summary(&self) -> Result<NodeSummary, StoreError> {
        let agents = self.connection.query_row(
            "SELECT COUNT(*) FROM (
                SELECT scope_id,agent_id FROM signed_records WHERE expires_at>?1
                UNION
                SELECT scope_id,agent_id FROM managed_members WHERE expires_at>?1 AND revoked_at IS NULL
             )",
            [now()],
            |row| row.get::<_, i64>(0),
        )?;
        let grants = self.connection.query_row(
            "SELECT COUNT(*) FROM grants WHERE expires_at>?1",
            [now()],
            |row| row.get::<_, i64>(0),
        )?;
        let active_connections = self.connection.query_row(
            "SELECT COUNT(DISTINCT caller_endpoint_id) FROM tasks WHERE state IN ('leased','running')",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let tasks =
            self.connection.query_row("SELECT COUNT(*) FROM tasks", [], |row| {
                row.get::<_, i64>(0)
            })?;
        let queued_tasks = self.connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state='queued'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(NodeSummary {
            agents: usize::try_from(agents).map_err(|_| StoreError::InvalidState)?,
            grants: usize::try_from(grants).map_err(|_| StoreError::InvalidState)?,
            active_connections: usize::try_from(active_connections)
                .map_err(|_| StoreError::InvalidState)?,
            tasks: usize::try_from(tasks).map_err(|_| StoreError::InvalidState)?,
            queued_tasks: usize::try_from(queued_tasks)
                .map_err(|_| StoreError::InvalidState)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{test_support::*, *};

    #[test]
    fn summary_uses_durable_agent_grant_connection_and_task_data() {
        let (mut store, database) = store();
        store
            .put_signed_record("record", "scope", "agent", b"record", 2_000_000_000)
            .expect("record must store");
        store
            .put_signed_record("record", "scope", "agent", b"renewed", 2_000_000_001)
            .expect("record must renew");
        assert_eq!(
            store.signed_record("record").expect("record must load").record,
            b"renewed"
        );
        put_grant(&store, "grant", 2_000_000_000);
        store
            .admit(&input("task", "peer"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        let summary = store.summary().expect("summary must load");
        assert_eq!(summary.agents, 1);
        assert_eq!(summary.grants, 1);
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
