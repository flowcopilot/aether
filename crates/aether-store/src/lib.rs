//! The node's durable state, one module per feature. Each module owns its
//! tables, the names stored in them and the keys it keeps in
//! `configuration`.

use std::path::Path;

use aether_core::ScopeKind;
use rusqlite::{Connection, OptionalExtension, params};

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

pub use access_groups::{
    AccessGroup, AccessGroupMember, FRIENDS_GROUP, REQUESTS_GROUP,
};
pub use admin::{
    AuditAction, AuditActor, AuditRecord, AuditResult, ChangeKind, PendingChange,
    Replaced,
};
pub use control::{
    CONTROL_MIGRATIONS, ControlStore, DirectoryEvent, DirectoryEventType, ManagedEvent,
    ManagedEventType,
};
pub use directory::SignedRecord;
pub use hub::{
    AccessRequestDraft, ApprovalKind, ApprovalSide, HubApproval, KeptListing,
    MAX_ACCESS_REQUEST_DRAFTS,
};
pub use managed::ManagedMember;
pub use migrate::BACKUP_RETENTION;
pub use outbox::{SentAttempt, SentState, SentTask, SentTaskInput};
pub use summary::NodeSummary;
pub use tasks::{
    CONTENT_CLEANUP_BATCH, CONTENT_RETENTION_SECONDS, EventType, LastTask, QueueLimits,
    StoredTask, TaskActivity, TaskAdmission, TaskCounts, TaskEvent, TaskExecutionData,
    TaskInput, TaskState,
};

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
    include_str!("../migrations/node/016_signed_record_kinds.sql"),
    include_str!("../migrations/node/017_drop_unused_grants.sql"),
    include_str!("../migrations/node/018_task_caller_ids.sql"),
];

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
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for StoreError {}

/// Statements that the connection prepares once and keeps, so a hot path
/// does not parse its SQL on every call. Each statement text is one cache
/// entry, so the text must not carry values; bind them as parameters.
pub(crate) trait CachedSql {
    fn execute_cached<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> rusqlite::Result<usize>;

    fn query_row_cached<T, P, F>(
        &self,
        sql: &str,
        params: P,
        map: F,
    ) -> rusqlite::Result<T>
    where
        P: rusqlite::Params,
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>;
}

impl CachedSql for Connection {
    fn execute_cached<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> rusqlite::Result<usize> {
        self.prepare_cached(sql)?.execute(params)
    }

    fn query_row_cached<T, P, F>(
        &self,
        sql: &str,
        params: P,
        map: F,
    ) -> rusqlite::Result<T>
    where
        P: rusqlite::Params,
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        self.prepare_cached(sql)?.query_row(params, map)
    }
}

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

    pub fn put_scope(&self, id: &str, kind: ScopeKind) -> Result<(), StoreError> {
        put_scope(&self.connection, id, kind)
    }
}

pub(crate) fn put_scope(
    connection: &Connection,
    id: &str,
    kind: ScopeKind,
) -> Result<(), StoreError> {
    connection.execute_cached(
        "INSERT INTO scopes(id,kind) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
        params![id, kind.as_str()],
    )?;
    Ok(())
}

/// Adds the scope `id` of `kind` unless it is there; a scope of another
/// kind is [`StoreError::InvalidState`].
pub(crate) fn ensure_scope(
    connection: &Connection,
    id: &str,
    kind: ScopeKind,
) -> Result<(), StoreError> {
    connection.execute_cached(
        "INSERT INTO scopes(id,kind) VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",
        params![id, kind.as_str()],
    )?;
    let stored: Option<String> = connection
        .query_row_cached("SELECT kind FROM scopes WHERE id=?1", [id], |row| row.get(0))
        .optional()?;
    if stored.as_deref() == Some(kind.as_str()) {
        Ok(())
    } else {
        Err(StoreError::InvalidState)
    }
}

/// Stores an enum that [`aether_core::named!`] names as its name.
macro_rules! sql_named {
    ($($name:ty),+ $(,)?) => {$(
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!("unknown {} {text:?}", stringify!($name)).into(),
                    )
                })
            }
        }
    )+};
}
pub(crate) use sql_named;

/// Sends an enum that [`aether_core::named!`] names as its name on the
/// wire, so the column and the wire share one table of names.
macro_rules! serde_named {
    ($($name:ty),+ $(,)?) => {$(
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&text).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown {} {text:?}", stringify!($name)))
                })
            }
        }
    )+};
}
pub(crate) use serde_named;

/// The column `index` of `row`, which holds a count that is never
/// negative.
pub(crate) fn column_u64(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

/// `names` as a quoted SQL list, for `IN (...)`. Every name comes from a
/// [`aether_core::named!`] table, so none holds a quote.
pub(crate) fn sql_names(names: &[&str]) -> String {
    names.iter().map(|name| format!("'{name}'")).collect::<Vec<_>>().join(",")
}

/// A row count for `LIMIT` or `OFFSET`; a count past `i64::MAX` is no limit.
pub(crate) fn sql_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    use aether_core::{
        AccessMode, AgentId, CapabilityId, EndpointId, Request, RequestId, ScopeId,
        ScopeKind, TaskId, WorkspaceId,
    };

    use crate::{NodeStore, TaskInput};

    /// How many backups of `database` sit next to it.
    pub fn backup_count(database: &Path) -> usize {
        let prefix = format!("{}.backup.", database.display());
        std::fs::read_dir(database.parent().expect("parent must exist"))
            .expect("parent must read")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().to_string_lossy().starts_with(&prefix))
            .count()
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
            request: Request {
                id: RequestId::new(format!("request-{id}")),
                task_id: TaskId::new(id),
                caller_agent_id: AgentId::new(peer),
                caller_endpoint_id: EndpointId::new(format!("endpoint-{peer}")),
                scope_id: ScopeId::new("scope"),
                capability_id: CapabilityId::new("capability"),
                workspace_id: WorkspaceId::new("workspace"),
                access_mode: AccessMode::ReadOnly,
            },
            idempotency_key: format!("key-{id}"),
            request_fingerprint: format!("fingerprint-{id}"),
            skill: "capability".to_owned(),
            remote_text: "task input".to_owned(),
        }
    }

    pub fn store() -> (NodeStore, PathBuf) {
        let database = path("node");
        let store = NodeStore::open(&database).expect("store must open");
        store.put_scope("scope", ScopeKind::Direct).expect("scope must store");
        (store, database)
    }

    pub fn temp_node_store() -> (tempfile::TempDir, NodeStore) {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        (directory, store)
    }

    pub fn temp_pact_store() -> (tempfile::TempDir, crate::pact::PactStore) {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = crate::pact::PactStore::open(
            directory.path().join(crate::pact::PACT_DATABASE),
        )
        .expect("store must open");
        (directory, store)
    }
}
