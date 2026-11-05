//! The node's durable state, one module per feature. Each module owns its
//! tables, the names stored in them and the keys it keeps in
//! `configuration`.

use std::path::Path;

use rusqlite::{Connection, params};

mod access_groups;
mod admin;
mod config;
mod control;
mod directory;
mod hub;
mod managed;
mod migrate;
mod outbox;
pub mod pact;
mod summary;
mod tasks;

pub use access_groups::*;
pub use admin::*;
pub use control::*;
pub use directory::*;
pub use hub::*;
pub use managed::*;
pub use migrate::{BACKUP_RETENTION, restore};
pub use outbox::*;
pub use summary::*;
pub use tasks::*;

use aether_core::time::now;
use migrate::open_connection;

pub const NODE_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/node/001_initial.sql"),
    include_str!("../migrations/node/002_task_indexes.sql"),
    include_str!("../migrations/node/003_task_identity.sql"),
    include_str!("../migrations/node/004_replay_entries.sql"),
    include_str!("../migrations/node/005_pending_change_values.sql"),
    include_str!("../migrations/node/006_task_constraints.sql"),
    include_str!("../migrations/node/007_task_contents.sql"),
    include_str!("../migrations/node/008_managed_roster.sql"),
    include_str!("../migrations/node/009_content_retention.sql"),
    include_str!("../migrations/node/010_legacy_content_expiry.sql"),
    include_str!("../migrations/node/011_access_groups.sql"),
    include_str!("../migrations/node/012_hub_approvals.sql"),
    include_str!("../migrations/node/013_sent_tasks.sql"),
    include_str!("../migrations/node/014_access_requests.sql"),
    include_str!("../migrations/node/015_access_request_drafts.sql"),
];

/// The file name of the node store in a data directory.
pub const NODE_DATABASE: &str = "node.sqlite";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid task state change")]
    InvalidState,
    #[error("task queue is full")]
    Overloaded,
    #[error("idempotency key conflicts")]
    DuplicateConflict,
    #[error("caller is not authorized")]
    Unauthorized,
    #[error("stored item was not found")]
    NotFound,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for StoreError {}

pub struct NodeStore {
    pub(crate) connection: Connection,
}

impl NodeStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), NODE_MIGRATIONS)? })
    }

    pub fn in_dir(dir: &Path) -> Result<Self, StoreError> {
        Self::open(dir.join(NODE_DATABASE))
    }

    pub fn put_scope(&self, id: &str, kind: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO scopes(id, kind) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            params![id, kind],
        )?;
        Ok(())
    }
}

/// A row count for `LIMIT` or `OFFSET`; a count past `i64::MAX` is no limit.
pub(crate) fn sql_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use rusqlite::params;

    use crate::{NodeStore, TaskInput};

    pub fn put_grant(store: &NodeStore, id: &str, expires_at: i64) {
        store
            .connection
            .execute(
                "INSERT INTO grants(id,scope_id,subject_agent_id,subject_endpoint_id,expires_at) VALUES(?1,'scope','agent','endpoint',?2)",
                params![id, expires_at],
            )
            .expect("grant must store");
    }

    pub fn path(name: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("aether-{name}-"))
            .suffix(".db")
            .tempfile()
            .expect("temporary database must create")
            .keep()
            .expect("temporary database must persist")
            .1
    }

    pub fn input(id: &str, peer: &str) -> TaskInput {
        TaskInput {
            id: id.to_owned(),
            request_id: format!("request-{id}"),
            idempotency_key: format!("key-{id}"),
            caller_agent_id: peer.to_owned(),
            caller_endpoint_id: format!("endpoint-{peer}"),
            request_fingerprint: format!("fingerprint-{id}"),
            scope_id: "scope".to_owned(),
            capability_id: "capability".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: "read_only".to_owned(),
            skill: "capability".to_owned(),
            remote_text: "task input".to_owned(),
        }
    }

    pub fn store() -> (NodeStore, PathBuf) {
        let database = path("node");
        let store = NodeStore::open(&database).expect("store must open");
        store.put_scope("scope", "direct").expect("scope must store");
        (store, database)
    }
}
