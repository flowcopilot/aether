use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};

use rusqlite::{
    Connection, MAIN_DB, OptionalExtension, Transaction, TransactionBehavior, params,
};
use rusqlite_migration::{M, Migrations};

pub mod pact;

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
pub const CONTENT_RETENTION_SECONDS: i64 = 24 * 60 * 60;
/// Maximum rows `clear_expired_content` clears in one call.
pub const CONTENT_CLEANUP_BATCH: i64 = 500;
pub const CONTROL_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/control/001_initial.sql"),
    include_str!("../migrations/control/002_directory_events.sql"),
    include_str!("../migrations/control/003_managed_members.sql"),
    include_str!("../migrations/control/004_managed_events.sql"),
];
pub const BACKUP_RETENTION: usize = 3;
/// The file name of the node store in a data directory.
pub const NODE_DATABASE: &str = "node.sqlite";

#[must_use]
pub const fn protocol_family() -> &'static str {
    aether_core::PROTOCOL_FAMILY
}

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

/// The state of a task that this node sent. The strings are the
/// `sent_tasks.state` column.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr, strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum SentState {
    /// Not on the host yet, or the host did not accept it yet.
    Waiting,
    /// The host has the task and works on it.
    Working,
    /// The host answered.
    Answered,
    /// The host refused or failed the task.
    Failed,
    /// The node gave up: the agent was not reachable, or access ended.
    NotDelivered,
}

impl SentState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// Whether the outbox still sends or polls the task.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Waiting | Self::Working)
    }
}

/// A new task for the outbox.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SentTaskInput {
    pub task_id: String,
    pub idempotency_key: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub authorization_json: String,
    pub text: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A task that this node sent, with what it knows of it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SentTask {
    pub task_id: String,
    pub idempotency_key: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub authorization_json: String,
    pub text: String,
    pub state: SentState,
    pub reason: Option<String>,
    pub host_state: Option<String>,
    pub answer: Option<String>,
    pub last_event_sequence: u64,
    pub attempts: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub next_attempt_at: i64,
    pub expires_at: i64,
    pub finished_at: Option<i64>,
    pub read_at: Option<i64>,
    pub lease_until: Option<i64>,
}

/// The result of one attempt to send or poll a task.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SentAttempt {
    pub state: SentState,
    pub reason: Option<String>,
    pub host_state: Option<String>,
    pub answer: Option<String>,
    pub last_event_sequence: u64,
    pub next_attempt_at: i64,
}

const SENT_COLUMNS: &str = "task_id,idempotency_key,agent_id,endpoint_id,authorization_json,text,state,reason,host_state,answer,last_event_sequence,attempts,created_at,updated_at,next_attempt_at,expires_at,finished_at,read_at,lease_until";

fn sent_task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(SentTask, String)> {
    let state: String = row.get(6)?;
    Ok((
        SentTask {
            task_id: row.get(0)?,
            idempotency_key: row.get(1)?,
            agent_id: row.get(2)?,
            endpoint_id: row.get(3)?,
            authorization_json: row.get(4)?,
            text: row.get(5)?,
            state: SentState::Waiting,
            reason: row.get(7)?,
            host_state: row.get(8)?,
            answer: row.get(9)?,
            last_event_sequence: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
            attempts: u64::try_from(row.get::<_, i64>(11)?).unwrap_or(0),
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
            next_attempt_at: row.get(14)?,
            expires_at: row.get(15)?,
            finished_at: row.get(16)?,
            read_at: row.get(17)?,
            lease_until: row.get(18)?,
        },
        state,
    ))
}

/// Gives the parsed `state` to a row that [`sent_task_from_row`] read.
fn with_state((mut task, state): (SentTask, String)) -> Result<SentTask, StoreError> {
    task.state = state.parse().map_err(|_| StoreError::InvalidState)?;
    Ok(task)
}

/// The side of a hub invite that this node's page acted on. The strings are
/// the `hub_approvals.side` column.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr, strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum ApprovalSide {
    /// This node offers its agent: its page sent the invite or approved a
    /// request.
    Sender,
    /// This node gets access: its page accepted an invite or sent a request.
    Receiver,
}

/// How a hub invite started. The strings are the `hub_approvals.kind`
/// column.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr, strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum ApprovalKind {
    /// The host invited someone by name.
    Invite,
    /// Someone asked a listed service agent for access.
    Request,
}

/// A hub invite that this node's own page sent or approved. The node signs
/// a grant for a hub row only when such a record names that row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HubApproval {
    pub invite_id: String,
    pub side: ApprovalSide,
    pub kind: ApprovalKind,
    /// The name of the other side on the hub.
    pub peer_name: String,
    /// The endpoint of the other side, once the node signed for it. A record
    /// never moves to another endpoint.
    pub peer_endpoint: Option<String>,
    /// The most days of access that the owner approved.
    pub days: u32,
    pub access_group: String,
    /// The signed line, kept so that a second delivery signs nothing new.
    pub bundle: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    /// For a request that this node sent: the listing it was sent for.
    pub listing: Option<KeptListing>,
}

/// A public listing as the hub returned it: the base64url text, the
/// signature of the endpoint key, and the expiry in Unix seconds.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeptListing {
    pub text: String,
    pub signature: String,
    pub expires_at: i64,
}

/// An access request that an agent prepared and the person has not
/// confirmed yet.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessRequestDraft {
    pub id: String,
    /// The full name of the listed service agent.
    pub target_name: String,
    pub listing: KeptListing,
    pub days: u32,
    pub created_at: i64,
    pub expires_at: i64,
}

/// The most drafts that wait for the person at one time.
pub const MAX_ACCESS_REQUEST_DRAFTS: i64 = 5;

/// The strings are the `tasks.state` column. Changing one breaks every
/// stored row that holds it.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr, strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum TaskState {
    PendingApproval,
    Queued,
    Leased,
    Running,
    UnknownResult,
    OperatorReviewed,
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
}

impl TaskState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        value.parse().map_err(|_| StoreError::InvalidState)
    }

    fn allows(self, next: Self) -> bool {
        match self {
            Self::PendingApproval => {
                matches!(next, Self::Queued | Self::Rejected | Self::Cancelled)
            }
            Self::Queued => {
                matches!(next, Self::Leased | Self::Rejected | Self::Cancelled)
            }
            Self::Leased => matches!(
                next,
                Self::Queued
                    | Self::Running
                    | Self::UnknownResult
                    | Self::Rejected
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::Running => matches!(
                next,
                Self::Succeeded | Self::Failed | Self::UnknownResult | Self::Cancelled
            ),
            Self::UnknownResult => next == Self::OperatorReviewed,
            Self::OperatorReviewed => {
                matches!(next, Self::Succeeded | Self::Failed | Self::Cancelled)
            }
            Self::Succeeded | Self::Failed | Self::Rejected | Self::Cancelled => false,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskInput {
    pub id: String,
    pub request_id: String,
    pub idempotency_key: String,
    pub caller_agent_id: String,
    pub caller_endpoint_id: String,
    pub request_fingerprint: String,
    pub scope_id: String,
    pub capability_id: String,
    pub workspace_id: String,
    pub access_mode: String,
    pub skill: String,
    pub remote_text: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct QueueLimits {
    pub global: usize,
    pub per_peer: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Admission {
    Accepted(String),
    Existing(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskEvent {
    pub sequence: i64,
    pub event_type: String,
}

impl TaskEvent {
    /// Reads `sequence,event_type`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { sequence: row.get(0)?, event_type: row.get(1)? })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskActivity {
    pub cursor: i64,
    pub task_id: String,
    pub event_type: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StoredTask {
    pub id: String,
    pub state: TaskState,
    pub skill: String,
    pub remote_text: String,
    pub events: Vec<TaskEvent>,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskExecutionData {
    pub caller_agent_id: String,
    pub caller_endpoint_id: String,
    pub scope_id: String,
    pub capability_id: String,
    pub workspace_id: String,
    pub access_mode: String,
    pub skill: String,
    pub remote_text: String,
}

/// The newest task one caller endpoint started in a scope.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LastTask {
    pub caller_endpoint_id: String,
    pub state: TaskState,
    pub created_at: i64,
    pub remote_text: String,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct NodeSummary {
    pub agents: usize,
    pub grants: usize,
    pub active_connections: usize,
    pub tasks: usize,
    pub queued_tasks: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingChange {
    pub id: String,
    pub change_type: String,
    pub value: String,
}

impl PendingChange {
    /// Reads `id,change_type,value`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { id: row.get(0)?, change_type: row.get(1)?, value: row.get(2)? })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryEvent {
    pub sequence: i64,
    pub event_type: String,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Option<Vec<u8>>,
}

/// A member of a managed scope. Its JSON form is the control service wire
/// form.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedMember {
    pub scope_id: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub role: String,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

impl ManagedMember {
    /// Reads `scope_id,agent_id,endpoint_id,role,expires_at,revoked_at`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            scope_id: row.get(0)?,
            agent_id: row.get(1)?,
            endpoint_id: row.get(2)?,
            role: row.get(3)?,
            expires_at: row.get(4)?,
            revoked_at: row.get(5)?,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ManagedEvent {
    pub sequence: i64,
    pub event_type: String,
    pub scope_id: String,
    pub agent_id: String,
    pub member: Option<ManagedMember>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AuditRecord {
    pub actor_type: String,
    pub action: String,
    pub result: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditActor {
    Mcp,
    Ui,
}

impl AuditActor {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditAction {
    Change,
}

impl AuditAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditResult {
    Pending,
    Applied,
    Confirmed,
    Failed,
}

impl AuditResult {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

pub struct NodeStore {
    connection: Connection,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessGroup {
    pub name: String,
    pub members: Vec<AccessGroupMember>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessGroupMember {
    pub agent_id: String,
    pub endpoint_id: String,
}

pub struct ControlStore {
    connection: Connection,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignedRecord {
    pub id: String,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Vec<u8>,
    pub expires_at: i64,
}

impl SignedRecord {
    /// Reads `id,scope_id,agent_id,record,expires_at`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            scope_id: row.get(1)?,
            agent_id: row.get(2)?,
            record: row.get(3)?,
            expires_at: row.get(4)?,
        })
    }
}

impl NodeStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), NODE_MIGRATIONS)? })
    }

    /// Opens `node.sqlite` in the data directory `dir`.
    pub fn in_dir(dir: &Path) -> Result<Self, StoreError> {
        Self::open(dir.join(NODE_DATABASE))
    }

    pub fn create_access_group(&self, name: &str) -> Result<(), StoreError> {
        self.connection
            .execute("INSERT INTO access_groups(name) VALUES (?1)", [name])?;
        Ok(())
    }

    /// Moves existing direct grants into the default list once. Later
    /// restarts do not restore members that the owner removed.
    pub fn initialize_access_groups(
        &mut self,
        identities: &[(String, String)],
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        let initialized: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM configuration WHERE key='access_groups.initialized')",
            [],
            |row| row.get(0),
        )?;
        if !initialized {
            for (agent, endpoint) in identities {
                transaction.execute(
                    "INSERT OR IGNORE INTO access_group_members(group_name,agent_id,endpoint_id) VALUES ('friends',?1,?2)",
                    params![agent, endpoint],
                )?;
            }
            transaction.execute(
                "INSERT INTO configuration(key,value) VALUES ('access_groups.initialized','1')",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_access_group(&self, name: &str) -> Result<(), StoreError> {
        if self.connection.execute("DELETE FROM access_groups WHERE name=?1", [name])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn add_access_group_member(
        &self,
        group: &str,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<(), StoreError> {
        if !self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_groups WHERE name=?1)",
            [group],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(StoreError::NotFound);
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO access_group_members(group_name,agent_id,endpoint_id) VALUES (?1,?2,?3)",
            params![group, agent_id, endpoint_id],
        )?;
        Ok(())
    }

    pub fn remove_access_group_member(
        &self,
        group: &str,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<(), StoreError> {
        if self.connection.execute(
            "DELETE FROM access_group_members WHERE group_name=?1 AND agent_id=?2 AND endpoint_id=?3",
            params![group, agent_id, endpoint_id],
        )? == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn access_group_allows(
        &self,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<bool, StoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_group_members WHERE agent_id=?1 AND endpoint_id=?2)",
            params![agent_id, endpoint_id],
            |row| row.get(0),
        ).map_err(StoreError::from)
    }

    pub fn access_groups(&self) -> Result<Vec<AccessGroup>, StoreError> {
        let mut groups =
            self.connection.prepare("SELECT name FROM access_groups ORDER BY name")?;
        let mut members = self.connection.prepare(
            "SELECT agent_id,endpoint_id FROM access_group_members WHERE group_name=?1 ORDER BY agent_id,endpoint_id",
        )?;
        groups
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|name| {
                let name = name?;
                let entries = members
                    .query_map([&name], |row| {
                        Ok(AccessGroupMember {
                            agent_id: row.get(0)?,
                            endpoint_id: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(AccessGroup { name, members: entries })
            })
            .collect()
    }

    pub fn put_scope(&self, id: &str, kind: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO scopes(id, kind) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            params![id, kind],
        )?;
        Ok(())
    }

    pub fn put_configuration(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO configuration(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn configuration(&self, key: &str) -> Result<String, StoreError> {
        self.connection
            .query_row("SELECT value FROM configuration WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Removes `key`. Removing a key that is not there is not an error.
    pub fn delete_configuration(&self, key: &str) -> Result<(), StoreError> {
        self.connection.execute("DELETE FROM configuration WHERE key=?1", [key])?;
        Ok(())
    }

    pub fn configuration_values(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, StoreError> {
        let pattern = format!("{prefix}%");
        let mut statement = self.connection.prepare(
            "SELECT value FROM configuration WHERE key LIKE ?1 ORDER BY key",
        )?;
        statement
            .query_map([pattern], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn configuration_key_counts(
        &self,
        prefix: &str,
    ) -> Result<BTreeMap<String, usize>, StoreError> {
        let pattern = format!("{prefix}%");
        let mut statement = self
            .connection
            .prepare("SELECT key FROM configuration WHERE key LIKE ?1")?;
        let mut counts = BTreeMap::new();
        for key in statement.query_map([pattern], |row| row.get::<_, String>(0))? {
            let key = key?;
            if let Some(rest) = key.strip_prefix(prefix)
                && let Some((category, _)) = rest.split_once('.')
            {
                let count: &mut usize = counts.entry(category.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
        Ok(counts)
    }

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

    /// Each caller endpoint's newest task in `scope_id`.
    pub fn last_tasks(&self, scope_id: &str) -> Result<Vec<LastTask>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT caller_endpoint_id,state,created_at,remote_text,final_result FROM (
                 SELECT caller_endpoint_id,state,created_at,remote_text,final_result,
                        ROW_NUMBER() OVER (
                            PARTITION BY caller_endpoint_id ORDER BY created_at DESC,rowid DESC
                        ) AS rank
                 FROM tasks WHERE scope_id=?1
             ) WHERE rank=1",
        )?;
        let rows = statement.query_map([scope_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.map(|row| {
            let (caller_endpoint_id, state, created_at, remote_text, result) = row?;
            Ok(LastTask {
                caller_endpoint_id,
                state: TaskState::parse(&state)?,
                created_at,
                remote_text,
                result,
            })
        })
        .collect()
    }

    pub fn put_signed_record(
        &self,
        id: &str,
        scope_id: &str,
        agent_id: &str,
        record: &[u8],
        expires_at: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![id, scope_id, agent_id, record, expires_at],
        )?;
        Ok(())
    }

    pub fn signed_record(&self, id: &str) -> Result<SignedRecord, StoreError> {
        self.connection
            .query_row(
                "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE id=?1",
                [id],
                SignedRecord::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn mark_signed_record_stale(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE signed_records SET expires_at=0 WHERE id=?1 AND expires_at<>0",
            [id],
        )? != 0)
    }

    /// Deletes the signed record `id`; [`StoreError::NotFound`] when there is
    /// none.
    pub fn remove_signed_record(&self, id: &str) -> Result<(), StoreError> {
        match self.connection.execute("DELETE FROM signed_records WHERE id=?1", [id])? {
            0 => Err(StoreError::NotFound),
            _ => Ok(()),
        }
    }

    pub fn apply_managed_member(
        &mut self,
        cursor_key: &str,
        cursor: i64,
        member: &ManagedMember,
    ) -> Result<(), StoreError> {
        if cursor_key.is_empty()
            || cursor <= 0
            || member.scope_id.is_empty()
            || member.agent_id.is_empty()
            || member.endpoint_id.is_empty()
            || member.role.is_empty()
            || member.expires_at <= 0
            || member.revoked_at.is_some()
        {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        advance_managed_cursor(&transaction, cursor_key, cursor)?;
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'managed') ON CONFLICT(id) DO NOTHING",
            [&member.scope_id],
        )?;
        let kind: String = transaction.query_row(
            "SELECT kind FROM scopes WHERE id=?1",
            [&member.scope_id],
            |row| row.get(0),
        )?;
        if kind != "managed" {
            return Err(StoreError::InvalidState);
        }
        transaction.execute(
            "INSERT INTO managed_members(scope_id,agent_id,endpoint_id,role,expires_at,revoked_at) VALUES(?1,?2,?3,?4,?5,NULL)
             ON CONFLICT(scope_id,agent_id) DO UPDATE SET endpoint_id=excluded.endpoint_id,role=excluded.role,expires_at=excluded.expires_at,revoked_at=NULL",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn revoke_managed_member(
        &mut self,
        cursor_key: &str,
        cursor: i64,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        if cursor_key.is_empty()
            || cursor <= 0
            || scope_id.is_empty()
            || agent_id.is_empty()
        {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        advance_managed_cursor(&transaction, cursor_key, cursor)?;
        if transaction.execute(
            "UPDATE managed_members SET revoked_at=?1 WHERE scope_id=?2 AND agent_id=?3 AND revoked_at IS NULL",
            params![now(), scope_id, agent_id],
        )? != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn managed_members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<ManagedMember>, StoreError> {
        if scope_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let mut statement = self.connection.prepare(
            "SELECT scope_id,agent_id,endpoint_id,role,expires_at,revoked_at FROM managed_members WHERE scope_id=?1 ORDER BY agent_id",
        )?;
        Ok(statement
            .query_map([scope_id], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn all_managed_members(&self) -> Result<Vec<ManagedMember>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT scope_id,agent_id,endpoint_id,role,expires_at,revoked_at FROM managed_members ORDER BY scope_id,agent_id",
        )?;
        Ok(statement
            .query_map([], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn has_grant(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .connection
            .query_row("SELECT 1 FROM grants WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn queue_change(
        &mut self,
        id: &str,
        change_type: &str,
        value: &str,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO pending_changes(id,change_type,value,expires_at) VALUES(?1,?2,?3,?4)",
            params![id, change_type, value, expires_at],
        )?;
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','pending',?1)",
            [now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_change(
        &self,
        actor: AuditActor,
        result: AuditResult,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,'change',?2,?3)",
            params![actor.as_str(), result.as_str(), now()],
        )?;
        Ok(())
    }

    /// Restores the value that an applied change replaced and records the
    /// failure. `previous` is `None` when the change created the key.
    pub fn revert_change(
        &mut self,
        id: &str,
        change_type: &str,
        previous: Option<&str>,
        actor: AuditActor,
    ) -> Result<(), StoreError> {
        let key = format!("admin.{change_type}.{id}");
        let tx = self.connection.transaction()?;
        match previous {
            Some(value) => tx.execute(
                "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?,
            None => tx.execute("DELETE FROM configuration WHERE key=?1", [key])?,
        };
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,'change','failed',?2)",
            params![actor.as_str(), now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn confirm_pending_change(&mut self, id: &str) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        let (change_type, value, expires_at): (String, String, i64) = tx
            .query_row(
                "SELECT change_type,value,expires_at FROM pending_changes WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if expires_at <= now() {
            return Err(StoreError::InvalidState);
        }
        tx.execute(
            "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("admin.{change_type}.{id}"), value],
        )?;
        tx.execute("DELETE FROM pending_changes WHERE id=?1", [id])?;
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('ui','change','confirmed',?1)",
            [now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_changes(&self) -> Result<Vec<PendingChange>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT id,change_type,value FROM pending_changes WHERE expires_at>?1 ORDER BY id")?;
        Ok(statement
            .query_map([now()], PendingChange::from_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Reads a pending change's type and value regardless of expiry, so a
    /// caller can classify "never existed" separately from "expired" (which
    /// `confirm_pending_change` already distinguishes on the write path).
    pub fn pending_change_ignoring_expiry(
        &self,
        id: &str,
    ) -> Result<PendingChange, StoreError> {
        self.connection
            .query_row(
                "SELECT id,change_type,value FROM pending_changes WHERE id=?1",
                [id],
                PendingChange::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>, StoreError> {
        self.audit_records_page(0, usize::MAX)
    }

    /// At most `limit` audit records in order, after the first `skip`.
    pub fn audit_records_page(
        &self,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT actor_type,action,result FROM audit_records ORDER BY id LIMIT ?1 OFFSET ?2",
        )?;
        let records = statement
            .query_map([sql_count(limit), sql_count(skip)], |row| {
                Ok(AuditRecord {
                    actor_type: row.get(0)?,
                    action: row.get(1)?,
                    result: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if records.iter().all(audit_record_is_safe) {
            Ok(records)
        } else {
            Err(StoreError::InvalidState)
        }
    }

    pub fn admit(
        &mut self,
        input: &TaskInput,
        limits: QueueLimits,
    ) -> Result<Admission, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let admission = admit(&tx, input, limits)?;
        tx.commit()?;
        Ok(admission)
    }

    pub fn admit_with_replay(
        &mut self,
        input: &TaskInput,
        limits: QueueLimits,
        replay_key: &str,
        replay_expires_at: i64,
        replay_limit: usize,
    ) -> Result<Admission, StoreError> {
        if replay_key.is_empty() || replay_limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        accept_replay_key(
            &tx,
            &input.caller_endpoint_id,
            &input.scope_id,
            replay_key,
            replay_expires_at,
            replay_limit,
        )?;
        let admission = admit(&tx, input, limits)?;
        tx.commit()?;
        Ok(admission)
    }
}

fn admit(
    tx: &Transaction<'_>,
    input: &TaskInput,
    limits: QueueLimits,
) -> Result<Admission, StoreError> {
    if let Some((id, request_id, fingerprint, caller, scope, capability, workspace, access)) = tx
            .query_row(
                "SELECT id,request_id,request_fingerprint,caller_agent_id,scope_id,capability_id,workspace_id,access_mode FROM tasks WHERE caller_endpoint_id=?1 AND idempotency_key=?2",
                params![input.caller_endpoint_id, input.idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?)),
            )
            .optional()?
        {
            return if id == input.id
                && request_id == input.request_id
                && fingerprint == input.request_fingerprint
                && caller == input.caller_agent_id
                && scope == input.scope_id
                && capability == input.capability_id
                && workspace == input.workspace_id
                && access == input.access_mode
            {
                Ok(Admission::Existing(id))
            } else {
                Err(StoreError::DuplicateConflict)
            };
        }
    if count(tx, None)? >= limits.global
        || count(tx, Some(&input.caller_agent_id))? >= limits.per_peer
    {
        return Err(StoreError::Overloaded);
    }
    tx.execute(
            "INSERT INTO tasks(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,skill,remote_text,state,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'queued',?13)",
            params![input.id,input.request_id,input.idempotency_key,input.caller_agent_id,input.caller_endpoint_id,input.request_fingerprint,input.scope_id,input.capability_id,input.workspace_id,input.access_mode,input.skill,input.remote_text,now()],
        )?;
    event(tx, &input.id, "queued")?;
    Ok(Admission::Accepted(input.id.clone()))
}

impl NodeStore {
    pub fn state(&self, task_id: &str) -> Result<TaskState, StoreError> {
        self.connection
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
            .and_then(|value| TaskState::parse(&value))
    }

    pub fn events(&self, task_id: &str) -> Result<Vec<TaskEvent>, StoreError> {
        self.events_after(task_id, 0)
    }

    pub fn stored_task(&self, task_id: &str) -> Result<StoredTask, StoreError> {
        let (id, state, skill, remote_text, result): (
            String,
            String,
            String,
            String,
            Option<String>,
        ) = self
            .connection
            .query_row(
                "SELECT id,state,skill,remote_text,final_result FROM tasks WHERE id=?1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(StoredTask {
            id,
            state: TaskState::parse(&state)?,
            skill,
            remote_text,
            events: self.events(task_id)?,
            result,
        })
    }

    pub fn events_after(
        &self,
        task_id: &str,
        last_sequence: i64,
    ) -> Result<Vec<TaskEvent>, StoreError> {
        let mut statement = self.connection.prepare("SELECT sequence,event_type FROM task_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence")?;
        Ok(statement
            .query_map(params![task_id, last_sequence], TaskEvent::from_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Reads at most `limit` events after `last_sequence`, bounded in SQL
    /// rather than loaded in full and truncated in memory. The returned
    /// `bool` is `true` when the task has more events beyond this page.
    pub fn events_page(
        &self,
        task_id: &str,
        last_sequence: i64,
        limit: usize,
    ) -> Result<(Vec<TaskEvent>, bool), StoreError> {
        let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type FROM task_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let mut events = statement
            .query_map(params![task_id, last_sequence, fetch], TaskEvent::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = events.len() > limit;
        events.truncate(limit);
        Ok((events, has_more))
    }

    /// Reads a task's current state and final result without loading its
    /// event journal, for callers that only need the durable outcome.
    pub fn task_snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), StoreError> {
        self.connection
            .query_row(
                "SELECT state,final_result FROM tasks WHERE id=?1",
                [task_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
            .and_then(|(state, result)| Ok((TaskState::parse(&state)?, result)))
    }

    /// Reads a task's snapshot and one page of its events together, so a
    /// polling caller sees both as of the same instant. Reading them through
    /// two separate `NodeStore` calls would let another store operation
    /// (an append from the executing task, a state change from the
    /// scheduler) land in between the two reads on the single store worker
    /// thread, and the caller would see events and a state snapshot that
    /// never coexisted.
    pub fn task_progress(
        &self,
        task_id: &str,
        last_sequence: i64,
        limit: usize,
    ) -> Result<(TaskState, Option<String>, Vec<TaskEvent>, bool), StoreError> {
        let (state, result) = self.task_snapshot(task_id)?;
        let (events, has_more) = self.events_page(task_id, last_sequence, limit)?;
        Ok((state, result, events, has_more))
    }

    /// At most `limit` task events after the event at `cursor`, in order.
    pub fn task_activity_after(
        &self,
        cursor: i64,
        limit: usize,
    ) -> Result<Vec<TaskActivity>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT rowid,task_id,event_type FROM task_events WHERE rowid>?1 ORDER BY rowid LIMIT ?2",
        )?;
        Ok(statement
            .query_map([cursor, sql_count(limit)], |row| {
                Ok(TaskActivity {
                    cursor: row.get(0)?,
                    task_id: row.get(1)?,
                    event_type: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn append_event(
        &mut self,
        task_id: &str,
        event_type: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        state(&tx, task_id)?;
        event(&tx, task_id, event_type)?;
        tx.commit()?;
        Ok(())
    }

    pub fn lease_next(
        &mut self,
        owner: &str,
        execution_id: &str,
        expires_at: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.connection.transaction()?;
        let task = tx.query_row(
            "SELECT queued.id FROM tasks queued WHERE queued.state='queued'
             ORDER BY (SELECT COUNT(*) FROM tasks active WHERE active.caller_agent_id=queued.caller_agent_id AND active.state IN ('leased','running')), queued.created_at, queued.id LIMIT 1",
            [], |row| row.get::<_, String>(0),
        ).optional()?;
        let Some(task) = task else { return Ok(None) };
        change(&tx, &task, TaskState::Leased, "leased")?;
        tx.execute("INSERT INTO execution_leases(task_id,owner,execution_id,expires_at) VALUES(?1,?2,?3,?4)", params![task,owner,execution_id,expires_at])?;
        tx.commit()?;
        Ok(Some(task))
    }

    pub fn mark_running(
        &mut self,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        if state(&tx, task_id)? != TaskState::Leased {
            return Err(StoreError::InvalidState);
        }
        change(&tx, task_id, TaskState::Running, "running")?;
        tx.commit()?;
        Ok(())
    }

    /// Extends a lease's expiry. `now` must reject a renewal once the
    /// lease's current expiry has already passed, so a renewal cannot
    /// resurrect a lease the recovery job simply has not reclaimed yet.
    pub fn renew_lease(
        &mut self,
        task_id: &str,
        execution_id: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        if self.connection.execute(
            "UPDATE execution_leases SET expires_at=?1 WHERE task_id=?2 AND execution_id=?3 AND expires_at>?4",
            params![expires_at, task_id, execution_id, now],
        )? == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn mark_succeeded(
        &mut self,
        task_id: &str,
        execution_id: &str,
        result: &str,
        result_expires_at: i64,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Cancelled {
            if !result.is_empty() {
                event(&tx, task_id, "result")?;
            }
            tx.commit()?;
            return Ok(());
        }
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        change(&tx, task_id, TaskState::Succeeded, "succeeded")?;
        tx.execute(
            "UPDATE tasks SET final_result=?1,result_expires_at=?2 WHERE id=?3",
            params![result, result_expires_at, task_id],
        )?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Clears input and output for every terminal task whose retention
    /// deadline has passed. Bounded to `CONTENT_CLEANUP_BATCH` rows per call
    /// so a large backlog cannot hold the store worker's single connection
    /// for an unbounded time; a caller that runs this periodically clears
    /// the rest over later calls, since already-cleared rows are excluded by
    /// `content_expired_at IS NULL`.
    pub fn clear_expired_content(
        &mut self,
        timestamp: i64,
    ) -> Result<usize, StoreError> {
        self.clear_expired_content_batch(timestamp, CONTENT_CLEANUP_BATCH)
    }

    fn clear_expired_content_batch(
        &mut self,
        timestamp: i64,
        batch: i64,
    ) -> Result<usize, StoreError> {
        let tx = self.connection.transaction()?;
        let ids = {
            let mut statement = tx.prepare(
                "SELECT id FROM tasks
                 WHERE content_expired_at IS NULL
                   AND state IN ('succeeded','failed','rejected','cancelled','unknown_result')
                   AND ((input_expires_at IS NOT NULL AND input_expires_at<=?1)
                        OR (result_expires_at IS NOT NULL AND result_expires_at<=?1))
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![timestamp, batch], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for task_id in &ids {
            tx.execute(
                "UPDATE tasks SET remote_text='',final_result=NULL,result_expires_at=NULL,content_expired_at=?1 WHERE id=?2",
                params![timestamp, task_id],
            )?;
            event(&tx, task_id, "content_expired")?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    pub fn content_is_expired(&self, task_id: &str) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT content_expired_at IS NOT NULL FROM tasks WHERE id=?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn mark_failed(
        &mut self,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Cancelled {
            tx.commit()?;
            return Ok(());
        }
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        change(&tx, task_id, TaskState::Failed, "failed")?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Records that an execution's outcome could not be confirmed after the
    /// process was told to start. Unlike `mark_failed`, this never discards
    /// the task: `UnknownResult` requires operator review before the task
    /// can be retried, because the remote process may have actually run.
    pub fn mark_unknown_result(
        &mut self,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Cancelled {
            tx.commit()?;
            return Ok(());
        }
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        change(&tx, task_id, TaskState::UnknownResult, "unknown_result")?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_rejected(
        &mut self,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        change(&tx, task_id, TaskState::Rejected, "rejected")?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn cancel(&mut self, task_id: &str) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Cancelled {
            tx.commit()?;
            return Ok(());
        }
        change(&tx, task_id, TaskState::Cancelled, "cancelled")?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn cancel_if_owned(
        &mut self,
        task_id: &str,
        caller_endpoint_id: &str,
        scope_id: &str,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        let (owner, scope): (String, String) = tx
            .query_row(
                "SELECT caller_endpoint_id,scope_id FROM tasks WHERE id=?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if owner != caller_endpoint_id || scope != scope_id {
            return Err(StoreError::Unauthorized);
        }
        if state(&tx, task_id)? == TaskState::Cancelled {
            tx.commit()?;
            return Ok(());
        }
        change(&tx, task_id, TaskState::Cancelled, "cancelled")?;
        set_input_expiry(&tx, task_id)?;
        tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn task_execution_data(
        &self,
        task_id: &str,
    ) -> Result<TaskExecutionData, StoreError> {
        self.connection
            .query_row(
                "SELECT caller_agent_id,caller_endpoint_id,scope_id,capability_id,workspace_id,access_mode,skill,remote_text FROM tasks WHERE id=?1",
                [task_id],
                |row| {
                    Ok(TaskExecutionData {
                        caller_agent_id: row.get(0)?,
                        caller_endpoint_id: row.get(1)?,
                        scope_id: row.get(2)?,
                        capability_id: row.get(3)?,
                        workspace_id: row.get(4)?,
                        access_mode: row.get(5)?,
                        skill: row.get(6)?,
                        remote_text: row.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn requeue_leased(&mut self, task_id: &str) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Leased {
            change(&tx, task_id, TaskState::Queued, "requeued")?;
            tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [task_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Reclaims every lease that expired at or before `timestamp`.
    ///
    /// Returns the IDs of tasks that were actually `Running` and moved to
    /// `UnknownResult`. Only those tasks may have a live worker attached
    /// (a merely `Leased` task never started a process), so the caller can
    /// use this list to stop the worker for a task it just reclaimed
    /// instead of letting it keep running unsupervised.
    pub fn recover_expired_leases(
        &mut self,
        timestamp: i64,
    ) -> Result<Vec<String>, StoreError> {
        let tx = self.connection.transaction()?;
        let ids = {
            let mut statement = tx
                .prepare("SELECT task_id FROM execution_leases WHERE expires_at<=?1")?;
            statement
                .query_map([timestamp], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut recovered_running = Vec::new();
        for id in ids {
            match state(&tx, &id)? {
                TaskState::Leased => {
                    change(&tx, &id, TaskState::Queued, "lease_expired")?;
                }
                TaskState::Running => {
                    change(&tx, &id, TaskState::UnknownResult, "unknown_result")?;
                    // `UnknownResult` still gets a definite retention
                    // deadline: it must not keep its input forever just
                    // because it also requires operator review.
                    set_input_expiry(&tx, &id)?;
                    recovered_running.push(id.clone());
                }
                _ => {}
            }
            tx.execute("DELETE FROM execution_leases WHERE task_id=?1", [&id])?;
        }
        tx.commit()?;
        Ok(recovered_running)
    }
}

fn accept_replay_key(
    tx: &Transaction<'_>,
    caller_endpoint_id: &str,
    scope_id: &str,
    replay_key: &str,
    expires_at: i64,
    limit: usize,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM replay_entries WHERE caller_endpoint_id=?1 AND scope_id=?2 AND expires_at<=?3",
        params![caller_endpoint_id, scope_id, now()],
    )?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM replay_entries WHERE caller_endpoint_id=?1 AND scope_id=?2 AND replay_key=?3)",
        params![caller_endpoint_id, scope_id, replay_key],
        |row| row.get(0),
    )?;
    if exists {
        return Err(StoreError::DuplicateConflict);
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM replay_entries WHERE caller_endpoint_id=?1 AND scope_id=?2",
        params![caller_endpoint_id, scope_id],
        |row| row.get(0),
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    if count >= limit {
        // Make room for the new entry: remove the oldest ones over the limit.
        let excess = count.saturating_sub(limit).saturating_add(1);
        tx.execute(
            "DELETE FROM replay_entries WHERE rowid IN (SELECT rowid FROM replay_entries WHERE caller_endpoint_id=?1 AND scope_id=?2 ORDER BY expires_at, rowid LIMIT ?3)",
            params![caller_endpoint_id, scope_id, excess],
        )?;
    }
    tx.execute(
        "INSERT INTO replay_entries(caller_endpoint_id,scope_id,replay_key,expires_at) VALUES(?1,?2,?3,?4)",
        params![caller_endpoint_id, scope_id, replay_key, expires_at],
    )?;
    Ok(())
}

fn advance_managed_cursor(
    transaction: &Transaction<'_>,
    cursor_key: &str,
    cursor: i64,
) -> Result<(), StoreError> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT value FROM configuration WHERE key=?1",
            [cursor_key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        let existing = existing.parse::<i64>().map_err(|_| StoreError::InvalidState)?;
        if cursor <= existing {
            return Err(StoreError::InvalidState);
        }
    }
    transaction.execute(
        "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![cursor_key, cursor.to_string()],
    )?;
    Ok(())
}

/// A row count for `LIMIT` or `OFFSET`; a count past `i64::MAX` is no limit.
fn sql_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn audit_record_is_safe(record: &AuditRecord) -> bool {
    matches!(record.actor_type.as_str(), "mcp" | "ui")
        && record.action == "change"
        && matches!(
            record.result.as_str(),
            "pending" | "applied" | "confirmed" | "failed"
        )
}

impl ControlStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), CONTROL_MIGRATIONS)? })
    }

    pub fn put_scope(&mut self, id: &str, kind: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            params![id, kind],
        )?;
        Ok(())
    }

    pub fn put_signed_record(
        &mut self,
        record: &SignedRecord,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![record.id, record.scope_id, record.agent_id, record.record, record.expires_at],
        )?;
        Ok(())
    }

    pub fn publish_signed_record(
        &mut self,
        record: &SignedRecord,
        event_type: &str,
    ) -> Result<(), StoreError> {
        if !matches!(event_type, "published" | "renewed") {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'directory') ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            [&record.scope_id],
        )?;
        transaction.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![record.id, record.scope_id, record.agent_id, record.record, record.expires_at],
        )?;
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![event_type, record.scope_id, record.agent_id, record.record, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn signed_record(&self, agent_id: &str) -> Result<SignedRecord, StoreError> {
        self.connection.query_row(
            "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE agent_id=?1 ORDER BY expires_at DESC LIMIT 1",
            [agent_id],
            SignedRecord::from_row,
        ).optional()?.ok_or(StoreError::NotFound)
    }

    pub fn remove_signed_record(&mut self, agent_id: &str) -> Result<(), StoreError> {
        if self
            .connection
            .execute("DELETE FROM signed_records WHERE agent_id=?1", [agent_id])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn revoke_signed_record(&mut self, agent_id: &str) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        let scope_id: String = transaction
            .query_row(
                "SELECT scope_id FROM signed_records WHERE agent_id=?1 ORDER BY expires_at DESC LIMIT 1",
                [agent_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if transaction
            .execute("DELETE FROM signed_records WHERE agent_id=?1", [agent_id])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES('revoked',?1,?2,NULL,?3)",
            params![scope_id, agent_id, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_signed_records(
        &self,
        timestamp: i64,
    ) -> Result<Vec<SignedRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE expires_at>?1 ORDER BY agent_id",
        )?;
        Ok(statement
            .query_map([timestamp], SignedRecord::from_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn directory_events_after(
        &self,
        sequence: i64,
        limit: usize,
    ) -> Result<Vec<DirectoryEvent>, StoreError> {
        if sequence < 0 || limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let limit = i64::try_from(limit).map_err(|_| StoreError::InvalidState)?;
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,record FROM directory_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![sequence, limit], |row| {
                Ok(DirectoryEvent {
                    sequence: row.get(0)?,
                    event_type: row.get(1)?,
                    scope_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    record: row.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn create_managed_invitation(
        &mut self,
        id: &str,
        scope_id: &str,
        token_hash: &[u8; 32],
        expires_at: i64,
    ) -> Result<(), StoreError> {
        if id.is_empty() || scope_id.is_empty() || expires_at <= now() {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'managed') ON CONFLICT(id) DO NOTHING",
            [scope_id],
        )?;
        let kind: String = transaction.query_row(
            "SELECT kind FROM scopes WHERE id=?1",
            [scope_id],
            |row| row.get(0),
        )?;
        if kind != "managed" {
            return Err(StoreError::InvalidState);
        }
        transaction.execute(
            "INSERT INTO managed_invitations(id,scope_id,token_hash,expires_at) VALUES(?1,?2,?3,?4)",
            params![id, scope_id, token_hash, expires_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn redeem_managed_invitation(
        &mut self,
        token_hash: &[u8; 32],
        member: &ManagedMember,
    ) -> Result<(), StoreError> {
        if member.scope_id.is_empty()
            || member.agent_id.is_empty()
            || member.endpoint_id.is_empty()
            || member.role.is_empty()
            || member.expires_at <= now()
            || member.revoked_at.is_some()
        {
            return Err(StoreError::InvalidState);
        }
        let timestamp = now();
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE managed_invitations SET redeemed_at=?1,redeemed_agent_id=?2 WHERE token_hash=?3 AND redeemed_at IS NULL AND expires_at>?1 AND scope_id=?4",
            params![timestamp, member.agent_id, token_hash, member.scope_id],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO managed_members(scope_id,agent_id,endpoint_id,role,expires_at,revoked_at) VALUES(?1,?2,?3,?4,?5,NULL)",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at],
        )?;
        transaction.execute(
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES('joined',?1,?2,?3,?4,?5,?6)",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn revoke_managed_member(
        &mut self,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        if scope_id.is_empty() || agent_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let timestamp = now();
        let transaction = self.connection.transaction()?;
        if transaction.execute(
            "UPDATE managed_members SET revoked_at=?1 WHERE scope_id=?2 AND agent_id=?3 AND revoked_at IS NULL",
            params![timestamp, scope_id, agent_id],
        )? != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES('revoked',?1,?2,NULL,NULL,NULL,?3)",
            params![scope_id, agent_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn managed_members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<ManagedMember>, StoreError> {
        if scope_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let mut statement = self.connection.prepare(
            "SELECT scope_id,agent_id,endpoint_id,role,expires_at,revoked_at FROM managed_members WHERE scope_id=?1 ORDER BY agent_id",
        )?;
        Ok(statement
            .query_map([scope_id], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn managed_events_after(
        &self,
        scope_id: &str,
        sequence: i64,
        limit: usize,
    ) -> Result<Vec<ManagedEvent>, StoreError> {
        if scope_id.is_empty() || sequence < 0 || limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let limit = i64::try_from(limit).map_err(|_| StoreError::InvalidState)?;
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,endpoint_id,role,expires_at FROM managed_events WHERE scope_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        Ok(statement
            .query_map(params![scope_id, sequence, limit], |row| {
                let event_type: String = row.get(1)?;
                let member = match event_type.as_str() {
                    "joined" => Some(ManagedMember {
                        scope_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        endpoint_id: row.get(4)?,
                        role: row.get(5)?,
                        expires_at: row.get(6)?,
                        revoked_at: None,
                    }),
                    "revoked" => None,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(ManagedEvent {
                    sequence: row.get(0)?,
                    event_type,
                    scope_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    member,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
}

/// The database files that this process has migrated and checked.
static MIGRATED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// Opens `path` in WAL mode. The first open of a path in this process makes
/// a backup if a migration is pending, migrates, and checks the foreign
/// keys. A later open only checks that `user_version` is the latest. If it
/// is not (another binary or a swapped file), the open does the full check
/// again, which fails for a newer schema.
fn open_connection(path: &Path, migrations: &[&str]) -> Result<Connection, StoreError> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(1))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| {
        row.get::<_, String>(0)
    })?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let checked = migrated().contains(&key);
    if checked && schema_is_latest(&connection, migrations)? {
        return Ok(connection);
    }
    // Hold the lock, so that one thread migrates a path at a time.
    let mut migrated = migrated();
    if migration_pending(&connection, migrations)? {
        backup_connection(&connection, path)?;
    }
    migrate(&mut connection, migrations)?;
    migrated.insert(key);
    drop(migrated);
    Ok(connection)
}

fn migrated() -> MutexGuard<'static, HashSet<PathBuf>> {
    MIGRATED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the database at `connection` has every one of `migrations`.
fn schema_is_latest(
    connection: &Connection,
    migrations: &[&str],
) -> Result<bool, StoreError> {
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(usize::try_from(user_version).is_ok_and(|version| version == migrations.len()))
}

fn backup_connection(
    connection: &Connection,
    path: &Path,
) -> Result<PathBuf, StoreError> {
    let backup = next_backup_path(path);
    connection.backup(MAIN_DB, &backup, None)?;
    retain_backups(path)?;
    Ok(backup)
}

fn next_backup_path(path: &Path) -> PathBuf {
    let prefix = format!("{}.backup.{}", path.display(), now());
    let mut index: u32 = 0;
    loop {
        let candidate = if index == 0 {
            PathBuf::from(&prefix)
        } else {
            PathBuf::from(format!("{prefix}.{index}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

fn retain_backups(path: &Path) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or(StoreError::InvalidState)?;
    let prefix = format!("{}{}", name.to_string_lossy(), ".backup.");
    let mut backups = fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    backups.sort();
    while backups.len() > BACKUP_RETENTION {
        let backup = backups.remove(0);
        fs::remove_file(backup)?;
    }
    Ok(())
}

/// Writes `backup` over the database at `path` with the SQLite backup API,
/// so that a WAL file of `path` cannot outlive the restore.
pub fn restore(
    backup: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<(), StoreError> {
    let mut connection = Connection::open(path)?;
    connection.restore(MAIN_DB, backup, None::<fn(rusqlite::backup::Progress)>)?;
    Ok(())
}

fn migrate(connection: &mut Connection, migrations: &[&str]) -> Result<(), StoreError> {
    let applied = legacy_migration_version(connection)?.unwrap_or_default();
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version == 0 && applied != 0 {
        connection.pragma_update(None, "user_version", applied)?;
    }
    let migrations =
        Migrations::new(migrations.iter().map(|migration| M::up(migration)).collect());
    migrations.to_latest(connection).map_err(|_| StoreError::InvalidState)?;
    let foreign_key_error: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |row| row.get(0),
    )?;
    if foreign_key_error {
        return Err(StoreError::InvalidState);
    }
    Ok(())
}

fn migration_pending(
    connection: &Connection,
    migrations: &[&str],
) -> Result<bool, StoreError> {
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != 0 {
        return Ok(version_is_behind(user_version, migrations));
    }
    let Some(applied) = legacy_migration_version(connection)? else {
        return Ok(false);
    };
    Ok(version_is_behind(applied, migrations))
}

/// Whether a database at schema `version` still has `migrations` to apply.
/// A negative version is behind; one past every migration is not.
fn version_is_behind(version: i64, migrations: &[&str]) -> bool {
    usize::try_from(version).map_or(true, |version| version < migrations.len())
}

fn legacy_migration_version(
    connection: &Connection,
) -> Result<Option<i64>, StoreError> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .map_err(Into::into)
}

fn count(tx: &Transaction<'_>, peer: Option<&str>) -> Result<usize, StoreError> {
    let count: i64 = match peer {
        Some(peer) => tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state='queued' AND caller_agent_id=?1",
            [peer],
            |row| row.get(0),
        )?,
        None => tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state='queued'",
            [],
            |row| row.get(0),
        )?,
    };
    usize::try_from(count).map_err(|_| StoreError::InvalidState)
}

fn lease_matches(
    tx: &Transaction<'_>,
    task_id: &str,
    execution_id: &str,
) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_leases WHERE task_id=?1 AND execution_id=?2)",
        params![task_id, execution_id],
        |row| row.get(0),
    )
    .map_err(|_| StoreError::NotFound)
}

fn state(tx: &Transaction<'_>, task_id: &str) -> Result<TaskState, StoreError> {
    tx.query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
        row.get::<_, String>(0)
    })
    .optional()?
    .ok_or(StoreError::NotFound)
    .and_then(|value| TaskState::parse(&value))
}

fn set_input_expiry(tx: &Transaction<'_>, task_id: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE tasks SET input_expires_at=?1 WHERE id=?2 AND input_expires_at IS NULL",
        params![now().saturating_add(CONTENT_RETENTION_SECONDS), task_id],
    )?;
    Ok(())
}

fn change(
    tx: &Transaction<'_>,
    task_id: &str,
    next: TaskState,
    event_type: &str,
) -> Result<(), StoreError> {
    if !state(tx, task_id)?.allows(next) {
        return Err(StoreError::InvalidState);
    }
    tx.execute(
        "UPDATE tasks SET state=?1 WHERE id=?2",
        params![next.as_str(), task_id],
    )?;
    event(tx, task_id, event_type)
}

fn event(
    tx: &Transaction<'_>,
    task_id: &str,
    event_type: &str,
) -> Result<(), StoreError> {
    let sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM task_events WHERE task_id=?1",
        [task_id],
        |row| row.get(0),
    )?;
    tx.execute("INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES(?1,?2,?3,'{}',?4)", params![task_id,sequence,event_type,now()])?;
    Ok(())
}

/// Seconds since the Unix epoch. A clock set before 1970 counts as 1970
/// rather than stopping the store.
impl NodeStore {
    /// Keeps `approval`, in place of a record with the same invite id.
    pub fn put_hub_approval(&self, approval: &HubApproval) -> Result<(), StoreError> {
        let side: &str = approval.side.into();
        let kind: &str = approval.kind.into();
        self.connection.execute(
            "INSERT OR REPLACE INTO hub_approvals(invite_id,side,kind,peer_name,peer_endpoint,days,access_group,bundle,created_at,expires_at,listing,listing_signature,listing_expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                approval.invite_id,
                side,
                kind,
                approval.peer_name,
                approval.peer_endpoint,
                approval.days,
                approval.access_group,
                approval.bundle,
                approval.created_at,
                approval.expires_at,
                approval.listing.as_ref().map(|listing| &listing.text),
                approval.listing.as_ref().map(|listing| &listing.signature),
                approval.listing.as_ref().map(|listing| listing.expires_at),
            ],
        )?;
        Ok(())
    }

    /// The approval of the hub invite `invite_id`, if this node made one.
    pub fn hub_approval(
        &self,
        invite_id: &str,
    ) -> Result<Option<HubApproval>, StoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT invite_id,side,kind,peer_name,peer_endpoint,days,access_group,bundle,created_at,expires_at,listing,listing_signature,listing_expires_at FROM hub_approvals WHERE invite_id=?1",
                [invite_id],
                |row| {
                    let listing = match (
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                    ) {
                        (Some(text), Some(signature), Some(expires_at)) => {
                            Some(KeptListing { text, signature, expires_at })
                        }
                        _ => None,
                    };
                    Ok((
                        (
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ),
                        (
                            row.get::<_, u32>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, i64>(9)?,
                        ),
                        listing,
                    ))
                },
            )
            .optional()?;
        let Some((
            (invite_id, side, kind, peer_name, peer_endpoint),
            (days, access_group, bundle, created_at, expires_at),
            listing,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(HubApproval {
            invite_id,
            side: side.parse().map_err(|_| StoreError::InvalidState)?,
            kind: kind.parse().map_err(|_| StoreError::InvalidState)?,
            peer_name,
            peer_endpoint,
            days,
            access_group,
            bundle,
            created_at,
            expires_at,
            listing,
        }))
    }

    /// Binds `approval`, the record that the caller read, to `endpoint` and
    /// keeps the signed `bundle`. A record that already names another
    /// endpoint does not move, and a record that another approval with the
    /// same invite ID replaced (another name or another time) is not pinned:
    /// [`StoreError::Unauthorized`].
    pub fn pin_hub_approval(
        &self,
        approval: &HubApproval,
        endpoint: &str,
        bundle: &str,
    ) -> Result<(), StoreError> {
        let invite_id = approval.invite_id.as_str();
        let changed = self.connection.execute(
            "UPDATE hub_approvals SET peer_endpoint=?2, bundle=?3 WHERE invite_id=?1 AND peer_name=?4 AND created_at=?5 AND (peer_endpoint IS NULL OR peer_endpoint=?2)",
            params![invite_id, endpoint, bundle, approval.peer_name, approval.created_at],
        )?;
        if changed == 1 {
            return Ok(());
        }
        match self.hub_approval(invite_id)? {
            Some(_) => Err(StoreError::Unauthorized),
            None => Err(StoreError::NotFound),
        }
    }

    /// Removes the approval of `invite_id`. Removing one that is not there is
    /// not an error.
    pub fn remove_hub_approval(&self, invite_id: &str) -> Result<(), StoreError> {
        self.connection
            .execute("DELETE FROM hub_approvals WHERE invite_id=?1", [invite_id])?;
        Ok(())
    }

    /// Removes every approval that expired at Unix time `now`, and returns
    /// how many it removed.
    pub fn clear_expired_hub_approvals(&self, now: i64) -> Result<usize, StoreError> {
        Ok(self
            .connection
            .execute("DELETE FROM hub_approvals WHERE expires_at <= ?1", [now])?)
    }

    /// Keeps a draft that waits for the person. [`StoreError::Overloaded`]
    /// when 5 drafts wait, and [`StoreError::DuplicateConflict`] when a
    /// draft names the same agent.
    pub fn add_access_request_draft(
        &mut self,
        draft: &AccessRequestDraft,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let open: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM access_request_drafts WHERE expires_at > ?1",
            [draft.created_at],
            |row| row.get(0),
        )?;
        if open >= MAX_ACCESS_REQUEST_DRAFTS {
            return Err(StoreError::Overloaded);
        }
        transaction.execute(
            "DELETE FROM access_request_drafts WHERE expires_at <= ?1",
            [draft.created_at],
        )?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO access_request_drafts(id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                draft.id,
                draft.target_name,
                draft.listing.text,
                draft.listing.signature,
                draft.listing.expires_at,
                draft.days,
                draft.created_at,
                draft.expires_at,
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::DuplicateConflict);
        }
        transaction.commit()?;
        Ok(())
    }

    /// The drafts that wait at Unix time `now`, oldest first.
    pub fn access_request_drafts(
        &self,
        now: i64,
    ) -> Result<Vec<AccessRequestDraft>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at FROM access_request_drafts WHERE expires_at > ?1 ORDER BY created_at, id",
        )?;
        let drafts = statement
            .query_map([now], draft_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(drafts)
    }

    /// The draft `id`, if it is there.
    pub fn access_request_draft(
        &self,
        id: &str,
    ) -> Result<Option<AccessRequestDraft>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at FROM access_request_drafts WHERE id=?1",
                [id],
                draft_from_row,
            )
            .optional()?)
    }

    /// Removes the draft `id`. Removing one that is not there is not an
    /// error.
    pub fn remove_access_request_draft(&self, id: &str) -> Result<(), StoreError> {
        self.connection
            .execute("DELETE FROM access_request_drafts WHERE id=?1", [id])?;
        Ok(())
    }

    /// Whether this node asked the agent `name` for access, or prepared a
    /// request to it, and the request is open at Unix time `now`.
    pub fn access_requested(&self, name: &str, now: i64) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_request_drafts WHERE target_name=?1 AND expires_at > ?2) OR EXISTS(SELECT 1 FROM hub_approvals WHERE peer_name=?1 AND side='receiver' AND kind='request' AND expires_at > ?2)",
            params![name, now],
            |row| row.get(0),
        )?)
    }

    /// Keeps the block of `endpoint`, with the name it had, at Unix time
    /// `now`. A second block of one endpoint keeps the first row.
    pub fn put_hub_block(
        &self,
        endpoint: &str,
        name: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO hub_blocks(endpoint,name,created_at) VALUES (?1,?2,?3)",
            params![endpoint, name, now],
        )?;
        Ok(())
    }

    /// Whether the owner blocked `endpoint`.
    pub fn hub_blocked(&self, endpoint: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM hub_blocks WHERE endpoint=?1)",
            [endpoint],
            |row| row.get(0),
        )?)
    }

    /// Removes the block of `endpoint`. Removing one that is not there is not
    /// an error.
    pub fn remove_hub_block(&self, endpoint: &str) -> Result<(), StoreError> {
        self.connection
            .execute("DELETE FROM hub_blocks WHERE endpoint=?1", [endpoint])?;
        Ok(())
    }
}

fn draft_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccessRequestDraft> {
    Ok(AccessRequestDraft {
        id: row.get(0)?,
        target_name: row.get(1)?,
        listing: KeptListing {
            text: row.get(2)?,
            signature: row.get(3)?,
            expires_at: row.get(4)?,
        },
        days: row.get(5)?,
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

impl NodeStore {
    /// Keeps a new task for the outbox, waiting and due at once.
    pub fn insert_sent_task(&self, input: &SentTaskInput) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO sent_tasks(task_id,idempotency_key,agent_id,endpoint_id,authorization_json,text,state,created_at,updated_at,next_attempt_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,'waiting',?7,?7,?7,?8)",
            params![
                input.task_id,
                input.idempotency_key,
                input.agent_id,
                input.endpoint_id,
                input.authorization_json,
                input.text,
                input.created_at,
                input.expires_at,
            ],
        )?;
        Ok(())
    }

    /// The sent task `task_id`, if there is one.
    pub fn sent_task(&self, task_id: &str) -> Result<Option<SentTask>, StoreError> {
        self.connection
            .query_row(
                &format!("SELECT {SENT_COLUMNS} FROM sent_tasks WHERE task_id=?1"),
                [task_id],
                sent_task_from_row,
            )
            .optional()?
            .map(with_state)
            .transpose()
    }

    fn sent_tasks_where(
        &self,
        clause: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<SentTask>, StoreError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {SENT_COLUMNS} FROM sent_tasks WHERE {clause}"
        ))?;
        let rows = statement.query_map(values, sent_task_from_row)?;
        rows.map(|row| with_state(row?)).collect()
    }

    /// Leases at most `limit` active tasks that are due at `now` until
    /// `lease_until`, at most one for each endpoint, and none for an endpoint
    /// that already has a leased task. One `IMMEDIATE` transaction, so two
    /// processes never lease the same task.
    pub fn claim_due_sent_tasks(
        &mut self,
        now: i64,
        lease_until: i64,
        limit: usize,
    ) -> Result<Vec<SentTask>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {SENT_COLUMNS} FROM sent_tasks s WHERE state IN ('waiting','working') AND next_attempt_at<=?1 AND (lease_until IS NULL OR lease_until<=?1) AND NOT EXISTS (SELECT 1 FROM sent_tasks o WHERE o.endpoint_id=s.endpoint_id AND o.task_id<>s.task_id AND o.state IN ('waiting','working') AND o.lease_until>?1) ORDER BY next_attempt_at, created_at, task_id"
            ))?;
            let rows = statement.query_map([now], sent_task_from_row)?;
            rows.map(|row| with_state(row?)).collect::<Result<Vec<_>, _>>()?
        };
        let mut endpoints = std::collections::BTreeSet::new();
        let mut claimed = Vec::new();
        for mut task in candidates {
            if claimed.len() >= limit {
                break;
            }
            if !endpoints.insert(task.endpoint_id.clone()) {
                continue;
            }
            transaction.execute(
                "UPDATE sent_tasks SET lease_until=?2 WHERE task_id=?1",
                params![task.task_id, lease_until],
            )?;
            task.lease_until = Some(lease_until);
            claimed.push(task);
        }
        transaction.commit()?;
        Ok(claimed)
    }

    /// Leases the active task `task_id` until `lease_until`, when no other
    /// process holds it.
    pub fn claim_sent_task(
        &mut self,
        task_id: &str,
        now: i64,
        lease_until: i64,
    ) -> Result<Option<SentTask>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE sent_tasks SET lease_until=?3 WHERE task_id=?1 AND state IN ('waiting','working') AND (lease_until IS NULL OR lease_until<=?2)",
            params![task_id, now, lease_until],
        )?;
        transaction.commit()?;
        if changed == 0 {
            return Ok(None);
        }
        self.sent_task(task_id)
    }

    /// Records one attempt of the task `task_id` and ends its lease.
    /// `lease_until` is the lease that the attempt claimed (`None` for a task
    /// that it did not claim). Only an active task that still has that lease
    /// changes; `false` when the task had ended, or when its lease expired
    /// and another process claimed it.
    pub fn record_sent_attempt(
        &self,
        task_id: &str,
        lease_until: Option<i64>,
        attempt: &SentAttempt,
        now: i64,
    ) -> Result<bool, StoreError> {
        let finished = (!attempt.state.is_active()).then_some(now);
        let changed = self.connection.execute(
            "UPDATE sent_tasks SET state=?2, reason=?3, host_state=?4, answer=?5, last_event_sequence=?6, next_attempt_at=?7, attempts=attempts+1, updated_at=?8, finished_at=?9, lease_until=NULL WHERE task_id=?1 AND state IN ('waiting','working') AND lease_until IS ?10",
            params![
                task_id,
                attempt.state.as_str(),
                attempt.reason,
                attempt.host_state,
                attempt.answer,
                i64::try_from(attempt.last_event_sequence).unwrap_or(i64::MAX),
                attempt.next_attempt_at,
                now,
                finished,
                lease_until,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Makes the active tasks of `endpoint_id` due at `now`: the agent came
    /// online. Returns how many changed.
    pub fn wake_sent_tasks(
        &self,
        endpoint_id: &str,
        now: i64,
    ) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "UPDATE sent_tasks SET next_attempt_at=?2 WHERE endpoint_id=?1 AND state IN ('waiting','working') AND next_attempt_at>?2",
            params![endpoint_id, now],
        )?)
    }

    /// Moves the active, unleased tasks of `endpoint_id` to `until` at the
    /// earliest: the agent is offline.
    pub fn defer_sent_tasks(
        &self,
        endpoint_id: &str,
        until: i64,
    ) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "UPDATE sent_tasks SET next_attempt_at=MAX(next_attempt_at, ?2) WHERE endpoint_id=?1 AND state IN ('waiting','working') AND lease_until IS NULL",
            params![endpoint_id, until],
        )?)
    }

    /// Every task that waits or works, oldest first.
    pub fn active_sent_tasks(&self) -> Result<Vec<SentTask>, StoreError> {
        self.sent_tasks_where(
            "state IN ('waiting','working') ORDER BY created_at, task_id",
            &[],
        )
    }

    /// Whether any task waits or works.
    pub fn has_active_sent_tasks(&self) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM sent_tasks WHERE state IN ('waiting','working'))",
            [],
            |row| row.get(0),
        )?)
    }

    /// Ends the active tasks of `endpoint_id` as not delivered, for `reason`.
    pub fn end_sent_tasks_for_endpoint(
        &self,
        endpoint_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "UPDATE sent_tasks SET state='not_delivered', reason=?2, updated_at=?3, finished_at=?3, lease_until=NULL WHERE endpoint_id=?1 AND state IN ('waiting','working')",
            params![endpoint_id, reason, now],
        )?)
    }

    /// Ends each active task whose deadline is past at `now` as not
    /// delivered, with `reason`.
    pub fn expire_sent_tasks(
        &self,
        now: i64,
        reason: &str,
    ) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "UPDATE sent_tasks SET state='not_delivered', reason=?2, updated_at=?1, finished_at=?1, lease_until=NULL WHERE state IN ('waiting','working') AND expires_at<=?1",
            params![now, reason],
        )?)
    }

    /// At most `limit` sent tasks, newest first, that come after the task
    /// `after` (`created_at`, `task_id`) when it is given. `state` and
    /// `unread_only` filter the rows.
    pub fn sent_tasks_page(
        &self,
        after: Option<(i64, &str)>,
        limit: usize,
        state: Option<SentState>,
        unread_only: bool,
    ) -> Result<Vec<SentTask>, StoreError> {
        let (after_time, after_id) = after.unwrap_or((i64::MAX, ""));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let state = state.map(SentState::as_str);
        self.sent_tasks_where(
            "(created_at<?1 OR (created_at=?1 AND task_id<?2)) AND (?3 IS NULL OR state=?3) AND (?4=0 OR read_at IS NULL) ORDER BY created_at DESC, task_id DESC LIMIT ?5",
            &[&after_time, &after_id, &state, &i64::from(unread_only), &limit],
        )
    }

    /// Marks an ended task as read at `now`; `false` when it does not exist
    /// or is still active.
    pub fn mark_sent_task_read(
        &self,
        task_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE sent_tasks SET read_at=COALESCE(read_at, ?2) WHERE task_id=?1 AND state NOT IN ('waiting','working')",
            params![task_id, now],
        )? == 1)
    }

    /// Deletes the ended tasks that ended at `before` or earlier.
    pub fn purge_sent_tasks(&self, before: i64) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "DELETE FROM sent_tasks WHERE state NOT IN ('waiting','working') AND finished_at<=?1",
            [before],
        )?)
    }
}

fn now() -> i64 {
    aether_core::time::unix_timestamp(SystemTime::now()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        AccessRequestDraft, Admission, ApprovalKind, ApprovalSide, AuditAction,
        AuditActor, AuditResult, BACKUP_RETENTION, CONTENT_RETENTION_SECONDS,
        ControlStore, HubApproval, KeptListing, ManagedMember, NODE_MIGRATIONS,
        NodeStore, QueueLimits, SentAttempt, SentState, SentTask, SentTaskInput,
        SignedRecord, StoreError, StoredTask, TaskEvent, TaskInput, TaskState,
        backup_connection, migrate, now, restore,
    };
    use rusqlite::{Connection, TransactionBehavior, params};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn backup(path: &Path) -> Result<PathBuf, StoreError> {
        backup_connection(&Connection::open(path)?, path)
    }

    fn put_grant(store: &NodeStore, id: &str, expires_at: i64) {
        store
            .connection
            .execute(
                "INSERT INTO grants(id,scope_id,subject_agent_id,subject_endpoint_id,expires_at) VALUES(?1,'scope','agent','endpoint',?2)",
                params![id, expires_at],
            )
            .expect("grant must store");
    }

    fn accept_replay_key(
        store: &mut NodeStore,
        caller_endpoint_id: &str,
        replay_key: &str,
        expires_at: i64,
        limit: usize,
    ) -> Result<(), StoreError> {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::accept_replay_key(
            &tx,
            caller_endpoint_id,
            "scope",
            replay_key,
            expires_at,
            limit,
        )?;
        tx.commit()?;
        Ok(())
    }

    fn add_audit(
        store: &NodeStore,
        actor: AuditActor,
        action: AuditAction,
        result: AuditResult,
    ) {
        store
            .connection
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,?2,?3,?4)",
                params![actor.as_str(), action.as_str(), result.as_str(), now()],
            )
            .expect("audit must store");
    }

    fn approval(invite_id: &str, expires_at: i64) -> HubApproval {
        HubApproval {
            invite_id: invite_id.to_owned(),
            side: ApprovalSide::Sender,
            kind: ApprovalKind::Invite,
            peer_name: "ana".to_owned(),
            peer_endpoint: None,
            days: 7,
            access_group: "friends".to_owned(),
            bundle: None,
            created_at: 100,
            expires_at,
            listing: None,
        }
    }

    fn draft(id: &str, target: &str, created_at: i64) -> AccessRequestDraft {
        AccessRequestDraft {
            id: id.to_owned(),
            target_name: target.to_owned(),
            listing: KeptListing {
                text: "listing".to_owned(),
                signature: "signature".to_owned(),
                expires_at: created_at.saturating_add(1_000),
            },
            days: 7,
            created_at,
            expires_at: created_at.saturating_add(24 * 60 * 60),
        }
    }

    #[test]
    fn access_request_drafts_stop_at_five() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store =
            NodeStore::open(directory.path().join("node.sqlite")).expect("store");
        for index in 0_i64..5 {
            store
                .add_access_request_draft(&draft(
                    &format!("d{index}"),
                    &format!("law{index}#0001"),
                    index.saturating_add(100),
                ))
                .expect("a draft must keep");
        }
        assert_eq!(
            store.add_access_request_draft(&draft("d5", "law5#0001", 200)),
            Err(StoreError::Overloaded)
        );
        store.remove_access_request_draft("d0").expect("a draft must go");
        assert_eq!(
            store.add_access_request_draft(&draft("d6", "law1#0001", 200)),
            Err(StoreError::DuplicateConflict)
        );
        store
            .add_access_request_draft(&draft("d5", "law5#0001", 200))
            .expect("a fifth draft must keep again");
        let drafts = store.access_request_drafts(200).expect("drafts must list");
        assert_eq!(drafts.len(), 5);
        assert_eq!(drafts[0], draft("d1", "law1#0001", 101));
        assert_eq!(
            store.access_request_draft("d5").expect("draft"),
            Some(draft("d5", "law5#0001", 200))
        );

        // After 24 hours a draft is gone, and it no longer counts.
        let later = 101_i64.saturating_add(24 * 60 * 60);
        assert_eq!(store.access_request_drafts(later).expect("drafts").len(), 4);
    }

    #[test]
    fn the_requests_group_exists_after_migration_and_blocks_are_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("node.sqlite");
        let store = NodeStore::open(&path).expect("store must open");
        let groups: Vec<String> = store
            .access_groups()
            .expect("groups must read")
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(groups, ["friends", "requests"]);

        store.put_hub_block("endpoint-a", "ana#0001", 10).expect("block must keep");
        store.put_hub_block("endpoint-a", "ana#0002", 11).expect("block must keep");
        drop(store);
        let store = NodeStore::open(&path).expect("store must open again");
        assert!(store.hub_blocked("endpoint-a").expect("block must read"));
        assert!(!store.hub_blocked("endpoint-b").expect("block must read"));
        store.remove_hub_block("endpoint-a").expect("block must go");
        store.remove_hub_block("endpoint-a").expect("a second removal is no error");
        assert!(!store.hub_blocked("endpoint-a").expect("block must read"));
    }

    #[test]
    fn a_replaced_hub_approval_does_not_take_the_line_of_the_old_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        let read = approval("invite", 900);
        store.put_hub_approval(&read).expect("approval must store");
        // The hub gives back the same invite ID for another name before the
        // worker pins the line that it signed for the first one.
        let replaced = HubApproval { peer_name: "eve#0001".to_owned(), ..read.clone() };
        store.put_hub_approval(&replaced).expect("approval must store");
        assert_eq!(
            store.pin_hub_approval(&read, "endpoint-a", "aether-invite:one"),
            Err(StoreError::Unauthorized)
        );
        let kept = store.hub_approval("invite").expect("read").expect("exists");
        assert_eq!((kept.peer_endpoint, kept.bundle), (None, None));
    }

    #[test]
    fn a_hub_approval_pins_one_endpoint_and_keeps_its_bundle() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_hub_approval(&approval("invite", 900)).expect("approval must store");
        assert_eq!(
            store.hub_approval("invite").expect("approval must read"),
            Some(approval("invite", 900))
        );
        let read = approval("invite", 900);
        store
            .pin_hub_approval(&read, "endpoint-a", "aether-invite:one")
            .expect("the first endpoint must pin");
        store
            .pin_hub_approval(&read, "endpoint-a", "aether-invite:one")
            .expect("the same endpoint may pin again");
        assert_eq!(
            store.pin_hub_approval(&read, "endpoint-b", "aether-invite:two"),
            Err(StoreError::Unauthorized)
        );
        let pinned = store
            .hub_approval("invite")
            .expect("approval must read")
            .expect("approval exists");
        assert_eq!(pinned.peer_endpoint.as_deref(), Some("endpoint-a"));
        assert_eq!(pinned.bundle.as_deref(), Some("aether-invite:one"));
        assert_eq!(
            store.pin_hub_approval(
                &approval("missing", 900),
                "endpoint-a",
                "aether-invite:one"
            ),
            Err(StoreError::NotFound)
        );
        store.remove_hub_approval("invite").expect("approval must remove");
        assert_eq!(store.hub_approval("invite").expect("approval must read"), None);
    }

    #[test]
    fn remove_signed_record_deletes_one_record_and_reports_a_missing_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_scope("direct", "direct").expect("scope must store");
        store
            .put_signed_record("direct:ana", "direct", "ana", b"{}", i64::MAX)
            .expect("record must store");
        store
            .put_signed_record("direct:bob", "direct", "bob", b"{}", i64::MAX)
            .expect("record must store");
        store.remove_signed_record("direct:ana").expect("record must remove");
        assert_eq!(store.signed_record("direct:ana"), Err(StoreError::NotFound));
        assert!(store.signed_record("direct:bob").is_ok());
        assert_eq!(store.remove_signed_record("direct:ana"), Err(StoreError::NotFound));
    }

    #[test]
    fn expired_hub_approvals_are_cleared() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_hub_approval(&approval("old", 500)).expect("approval must store");
        store.put_hub_approval(&approval("new", 1_500)).expect("approval must store");
        assert_eq!(store.clear_expired_hub_approvals(1_000), Ok(1));
        assert_eq!(store.hub_approval("old").expect("approval must read"), None);
        assert!(store.hub_approval("new").expect("approval must read").is_some());
    }

    #[test]
    fn access_groups_persist_membership_and_do_not_restore_removed_members() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("node.sqlite");
        let mut store = NodeStore::open(&path).expect("store must open");
        store
            .initialize_access_groups(&[("bob".to_owned(), "endpoint-a".to_owned())])
            .expect("current access must move into friends");
        assert!(
            store.access_group_allows("bob", "endpoint-a").expect("access must read")
        );
        assert!(
            !store.access_group_allows("bob", "endpoint-b").expect("access must read")
        );
        store.create_access_group("work").expect("group must create");
        store
            .add_access_group_member("work", "bob", "endpoint-a")
            .expect("member must add");
        store
            .remove_access_group_member("friends", "bob", "endpoint-a")
            .expect("member must remove");
        drop(store);
        let mut store = NodeStore::open(&path).expect("store must reopen");
        store
            .initialize_access_groups(&[("bob".to_owned(), "endpoint-a".to_owned())])
            .expect("migration must not restore an old member");
        assert_eq!(
            store
                .access_groups()
                .expect("groups must list")
                .iter()
                .find(|group| group.name == "friends")
                .expect("friends must exist")
                .members
                .len(),
            0
        );
        assert!(
            store
                .access_group_allows("bob", "endpoint-a")
                .expect("work access must read")
        );
        store.delete_access_group("work").expect("group must delete");
        assert!(
            !store.access_group_allows("bob", "endpoint-a").expect("access must read")
        );
    }

    #[test]
    fn stored_names_stay_the_same() {
        for (state, name) in [
            (TaskState::PendingApproval, "pending_approval"),
            (TaskState::Queued, "queued"),
            (TaskState::Leased, "leased"),
            (TaskState::Running, "running"),
            (TaskState::UnknownResult, "unknown_result"),
            (TaskState::OperatorReviewed, "operator_reviewed"),
            (TaskState::Succeeded, "succeeded"),
            (TaskState::Failed, "failed"),
            (TaskState::Rejected, "rejected"),
            (TaskState::Cancelled, "cancelled"),
        ] {
            assert_eq!(state.as_str(), name);
            assert_eq!(TaskState::parse(name), Ok(state));
        }
        assert_eq!(TaskState::parse("Queued"), Err(StoreError::InvalidState));
        assert_eq!(AuditActor::Mcp.as_str(), "mcp");
        assert_eq!(AuditActor::Ui.as_str(), "ui");
        assert_eq!(AuditAction::Change.as_str(), "change");
        assert_eq!(AuditResult::Pending.as_str(), "pending");
        assert_eq!(AuditResult::Applied.as_str(), "applied");
        assert_eq!(AuditResult::Confirmed.as_str(), "confirmed");
        assert_eq!(AuditResult::Failed.as_str(), "failed");
        for (state, name) in [
            (SentState::Waiting, "waiting"),
            (SentState::Working, "working"),
            (SentState::Answered, "answered"),
            (SentState::Failed, "failed"),
            (SentState::NotDelivered, "not_delivered"),
        ] {
            assert_eq!(state.as_str(), name);
            assert_eq!(name.parse::<SentState>(), Ok(state));
        }
    }

    fn sent(task_id: &str, endpoint: &str, created_at: i64) -> SentTaskInput {
        SentTaskInput {
            task_id: task_id.to_owned(),
            idempotency_key: format!("key-{task_id}"),
            agent_id: "ana#0001".to_owned(),
            endpoint_id: endpoint.to_owned(),
            authorization_json: "{}".to_owned(),
            text: "Give me three names".to_owned(),
            created_at,
            expires_at: created_at.saturating_add(1_000),
        }
    }

    fn answered(now: i64) -> SentAttempt {
        SentAttempt {
            state: SentState::Answered,
            reason: None,
            host_state: Some("completed".to_owned()),
            answer: Some("Three names".to_owned()),
            last_event_sequence: 4,
            next_attempt_at: now,
        }
    }

    fn sent_store(name: &str) -> (NodeStore, PathBuf) {
        let database = path(name);
        (NodeStore::open(&database).expect("store must open"), database)
    }

    #[test]
    fn a_0_2_8_database_opens_with_every_migration_and_an_older_binary_refuses_it() {
        // Release 0.2.8 had the node migrations 001 to 011.
        let database = path("release-0-2-8");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..11]).expect("the 0.2.8 schema");
        connection
            .execute(
                "INSERT INTO access_group_members(group_name,agent_id,endpoint_id) VALUES('friends','ana','endpoint-ana')",
                [],
            )
            .expect("member must insert");
        connection
            .execute(
                "INSERT INTO configuration(key,value) VALUES('caller.contact.ana','{}')",
                [],
            )
            .expect("contact must insert");
        drop(connection);

        let store = NodeStore::open(&database).expect("every migration must apply");
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version must read");
        assert_eq!(usize::try_from(version).ok(), Some(NODE_MIGRATIONS.len()));
        assert_eq!(store.configuration("caller.contact.ana").as_deref(), Ok("{}"));
        let groups = store.access_groups().expect("groups must read");
        let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
        assert_eq!(names, ["friends", "requests"]);
        assert_eq!(groups[0].members.len(), 1);
        assert_eq!(groups[0].members[0].agent_id, "ana");
        assert!(groups[1].members.is_empty());
        let prefix = format!("{}{}", database.display(), ".backup.");
        let backups = fs::read_dir(database.parent().expect("parent must exist"))
            .expect("parent must read")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(backups, 1);
        drop(store);

        // The code of 0.2.8 knows 11 migrations and refuses the newer schema.
        let mut connection = Connection::open(&database).expect("database must open");
        assert!(matches!(
            migrate(&mut connection, &NODE_MIGRATIONS[..11]),
            Err(StoreError::InvalidState)
        ));
    }

    #[test]
    fn a_later_open_in_the_process_still_checks_the_schema_version() {
        let database = path("later-open");
        drop(NodeStore::open(&database).expect("first open must migrate"));
        let latest = i64::try_from(NODE_MIGRATIONS.len()).expect("count fits");
        let connection = Connection::open(&database).expect("database must open");
        connection
            .pragma_update(None, "user_version", latest + 1)
            .expect("version must write");
        assert!(matches!(NodeStore::open(&database), Err(StoreError::InvalidState)));
        connection
            .pragma_update(None, "user_version", latest)
            .expect("version must write");
        drop(connection);
        let store = NodeStore::open(&database).expect("current schema must open");
        let mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode must read");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn migration_thirteen_adds_sent_tasks_to_an_existing_database() {
        let database = path("sent-migration");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..12])
            .expect("migrations through 012");
        connection
            .execute("INSERT INTO configuration(key,value) VALUES('kept','yes')", [])
            .expect("configuration must insert");
        drop(connection);
        let store = NodeStore::open(&database).expect("migration 013 must apply");
        assert_eq!(store.configuration("kept").as_deref(), Ok("yes"));
        store
            .insert_sent_task(&sent("task", "endpoint", 10))
            .expect("sent task must insert");
        let prefix = format!("{}{}", database.display(), ".backup.");
        let backups = fs::read_dir(database.parent().expect("parent must exist"))
            .expect("parent must read")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn sent_tasks_claim_once_and_keep_terminal_rows_unchanged() {
        let (mut store, _) = sent_store("sent-claim");
        store.insert_sent_task(&sent("task", "endpoint", 10)).expect("insert");
        let claimed = store.claim_due_sent_tasks(10, 100, 4).expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].lease_until, Some(100));
        assert!(store.claim_due_sent_tasks(20, 120, 4).expect("claim").is_empty());
        assert_eq!(store.claim_sent_task("task", 20, 120).expect("claim"), None);
        assert!(
            store
                .record_sent_attempt("task", Some(100), &answered(30), 30)
                .expect("record")
        );
        let ended = store.sent_task("task").expect("read").expect("the task exists");
        assert_eq!(
            (ended.state, ended.attempts, ended.finished_at),
            (SentState::Answered, 1, Some(30))
        );
        assert_eq!(ended.answer.as_deref(), Some("Three names"));
        assert_eq!(ended.lease_until, None);
        // An ended task does not change again.
        let failed = SentAttempt { state: SentState::Failed, ..answered(40) };
        assert!(!store.record_sent_attempt("task", None, &failed, 40).expect("record"));
        assert_eq!(store.end_sent_tasks_for_endpoint("endpoint", "gone", 50), Ok(0));
        assert_eq!(
            store.sent_task("task").expect("read").map(|task| task.state),
            Some(SentState::Answered)
        );
        assert!(!store.has_active_sent_tasks().expect("active"));
    }

    #[test]
    fn an_attempt_after_its_lease_was_claimed_again_records_nothing() {
        let (mut store, _) = sent_store("sent-stale");
        store.insert_sent_task(&sent("task", "endpoint", 10)).expect("insert");
        assert_eq!(store.claim_due_sent_tasks(10, 100, 4).expect("claim").len(), 1);
        // The first attempt is slow: its lease expires and another process
        // claims the task.
        assert!(store.claim_sent_task("task", 100, 190).expect("claim").is_some());
        assert!(
            !store
                .record_sent_attempt("task", Some(100), &answered(120), 120)
                .expect("record")
        );
        let kept = store.sent_task("task").expect("read").expect("the task exists");
        assert_eq!((kept.state, kept.lease_until), (SentState::Waiting, Some(190)));
        assert!(
            store
                .record_sent_attempt("task", Some(190), &answered(130), 130)
                .expect("record")
        );
    }

    #[test]
    fn one_endpoint_has_at_most_one_claimed_sent_task() {
        let (mut store, _) = sent_store("sent-endpoint");
        store.insert_sent_task(&sent("a", "endpoint-a", 10)).expect("insert");
        store.insert_sent_task(&sent("b", "endpoint-a", 11)).expect("insert");
        store.insert_sent_task(&sent("c", "endpoint-b", 12)).expect("insert");
        let first: Vec<String> = store
            .claim_due_sent_tasks(20, 100, 4)
            .expect("claim")
            .into_iter()
            .map(|task| task.task_id)
            .collect();
        assert_eq!(first, ["a", "c"]);
        assert!(store.claim_due_sent_tasks(30, 130, 4).expect("claim").is_empty());
        let waiting = SentAttempt { state: SentState::Waiting, ..answered(40) };
        store.record_sent_attempt("a", Some(100), &waiting, 40).expect("record");
        let next: Vec<String> = store
            .claim_due_sent_tasks(40, 140, 4)
            .expect("claim")
            .into_iter()
            .map(|task| task.task_id)
            .collect();
        // After its lease ends, the endpoint gets its next task: the one
        // that was due first.
        assert_eq!(next, ["b"]);
        // A leased task is not deferred; a wake moves only the later one.
        assert_eq!(store.defer_sent_tasks("endpoint-a", 500), Ok(1));
        assert_eq!(store.wake_sent_tasks("endpoint-a", 50), Ok(1));
        assert!(store.has_active_sent_tasks().expect("active"));
        assert_eq!(store.active_sent_tasks().expect("active").len(), 3);
    }

    #[test]
    fn sent_tasks_expire_as_not_delivered_after_their_deadline() {
        let (store, _) = sent_store("sent-expire");
        store.insert_sent_task(&sent("old", "endpoint", 10)).expect("insert");
        store.insert_sent_task(&sent("new", "endpoint", 500)).expect("insert");
        assert_eq!(store.expire_sent_tasks(1_010, "expired"), Ok(1));
        let old = store.sent_task("old").expect("read").expect("exists");
        assert_eq!(
            (old.state, old.reason.as_deref()),
            (SentState::NotDelivered, Some("expired"))
        );
        assert_eq!(
            store.sent_task("new").expect("read").map(|task| task.state),
            Some(SentState::Waiting)
        );
        assert_eq!(
            store.end_sent_tasks_for_endpoint("endpoint", "access ended", 1_020),
            Ok(1)
        );
        assert_eq!(
            store.sent_task("new").expect("read").and_then(|task| task.reason),
            Some("access ended".to_owned())
        );
    }

    #[test]
    fn sent_tasks_page_newest_first_and_mark_read() {
        let (store, _) = sent_store("sent-page");
        for (task, time) in [("a", 10), ("b", 20), ("c", 30)] {
            store.insert_sent_task(&sent(task, "endpoint", time)).expect("insert");
        }
        store.record_sent_attempt("b", None, &answered(40), 40).expect("record");
        let ids = |tasks: Vec<SentTask>| {
            tasks.into_iter().map(|task| task.task_id).collect::<Vec<_>>()
        };
        assert_eq!(
            ids(store.sent_tasks_page(None, 2, None, false).expect("page")),
            ["c", "b"]
        );
        assert_eq!(
            ids(store.sent_tasks_page(Some((20, "b")), 2, None, false).expect("page")),
            ["a"]
        );
        assert_eq!(
            ids(store
                .sent_tasks_page(None, 10, Some(SentState::Answered), false)
                .expect("page")),
            ["b"]
        );
        assert!(!store.mark_sent_task_read("a", 50).expect("read"));
        assert!(store.mark_sent_task_read("b", 50).expect("read"));
        assert!(store.mark_sent_task_read("b", 60).expect("read"));
        assert_eq!(
            store.sent_task("b").expect("read").and_then(|task| task.read_at),
            Some(50)
        );
        assert_eq!(
            ids(store.sent_tasks_page(None, 10, None, true).expect("page")),
            ["c", "a"]
        );
    }

    #[test]
    fn purge_sent_tasks_removes_only_old_finished_rows() {
        let (store, _) = sent_store("sent-purge");
        for task in ["old", "recent", "active"] {
            store.insert_sent_task(&sent(task, "endpoint", 10)).expect("insert");
        }
        store.record_sent_attempt("old", None, &answered(100), 100).expect("record");
        store.record_sent_attempt("recent", None, &answered(900), 900).expect("record");
        assert_eq!(store.purge_sent_tasks(500), Ok(1));
        assert_eq!(store.sent_task("old"), Ok(None));
        assert!(store.sent_task("recent").expect("read").is_some());
        assert!(store.sent_task("active").expect("read").is_some());
    }

    fn path(name: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("aether-{name}-"))
            .suffix(".db")
            .tempfile()
            .expect("temporary database must create")
            .keep()
            .expect("temporary database must persist")
            .1
    }

    fn input(id: &str, peer: &str) -> TaskInput {
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

    fn store() -> (NodeStore, PathBuf) {
        let database = path("node");
        let store = NodeStore::open(&database).expect("store must open");
        store.put_scope("scope", "direct").expect("scope must store");
        (store, database)
    }

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

    #[test]
    fn last_tasks_give_each_caller_its_newest_task_state() {
        let (mut store, database) = store();
        let limits = QueueLimits { global: 4, per_peer: 2 };
        store.admit(&input("one", "a"), limits).expect("task must admit");
        store.admit(&input("two", "b"), limits).expect("task must admit");
        store.cancel("two").expect("task must cancel");
        let mut last = store.last_tasks("scope").expect("last tasks must load");
        last.sort_by(|left, right| {
            left.caller_endpoint_id.cmp(&right.caller_endpoint_id)
        });
        assert_eq!(
            last.iter()
                .map(|task| (task.caller_endpoint_id.as_str(), task.state))
                .collect::<Vec<_>>(),
            [("endpoint-a", TaskState::Queued), ("endpoint-b", TaskState::Cancelled)]
        );
        assert!(store.last_tasks("other").expect("last tasks must load").is_empty());
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn migrations_and_backup_restore_work() {
        let (mut store, database) = store();
        ControlStore::open(path("control")).expect("control must open");
        store.put_configuration("owner", "operator").expect("configuration must store");
        put_grant(&store, "grant", 1);
        store
            .admit(&input("one", "a"), QueueLimits { global: 2, per_peer: 2 })
            .expect("task must admit");
        let saved = backup(&database).expect("backup must work");
        assert_eq!(
            store.cancel_if_owned("one", "endpoint-other", "scope"),
            Err(StoreError::Unauthorized)
        );
        assert_eq!(store.state("one"), Ok(TaskState::Queued));
        store.cancel_if_owned("one", "endpoint-a", "scope").expect("task must cancel");
        drop(store);
        restore(&saved, &database).expect("restore must work");
        let restored = NodeStore::open(&database).expect("restored store must open");
        assert_eq!(restored.configuration("owner"), Ok("operator".to_owned()));
        assert_eq!(restored.has_grant("grant"), Ok(true));
        assert_eq!(restored.state("one"), Ok(TaskState::Queued));
        assert_eq!(
            restored
                .events("one")
                .expect("events must load")
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
        fs::remove_file(database).expect("database must remove");
        fs::remove_file(saved).expect("backup must remove");
    }

    #[test]
    fn configuration_values_lists_only_the_matching_prefix_in_key_order() {
        let (store, database) = store();
        store
            .put_configuration("admin.grant.b", "grant-b")
            .expect("configuration must store");
        store
            .put_configuration("admin.grant.a", "grant-a")
            .expect("configuration must store");
        store
            .put_configuration("admin.limit.queue_global", "5")
            .expect("configuration must store");
        assert_eq!(
            store.configuration_values("admin.grant.").expect("values must load"),
            vec!["grant-a".to_owned(), "grant-b".to_owned()]
        );
        assert!(
            store
                .configuration_values("admin.nothing.")
                .expect("values must load")
                .is_empty()
        );
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn managed_roster_events_and_cursors_are_durable() {
        let database = path("managed-roster");
        let mut store = NodeStore::open(&database).expect("node store must open");
        let member = ManagedMember {
            scope_id: "managed".to_owned(),
            agent_id: "member-agent".to_owned(),
            endpoint_id: "member-endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at: i64::MAX,
            revoked_at: None,
        };
        store
            .apply_managed_member("managed.cursor", 1, &member)
            .expect("member event must apply");
        assert_eq!(store.configuration("managed.cursor"), Ok("1".to_owned()));
        assert_eq!(
            store.managed_members("managed").expect("members must load"),
            vec![member.clone()]
        );
        assert_eq!(store.summary().expect("summary must load").agents, 1);
        assert_eq!(
            store.apply_managed_member("managed.cursor", 1, &member),
            Err(StoreError::InvalidState)
        );
        store
            .revoke_managed_member("managed.cursor", 2, "managed", "member-agent")
            .expect("revocation event must apply");
        assert_eq!(store.summary().expect("summary must load").agents, 0);
        drop(store);

        let store = NodeStore::open(&database).expect("node store must reopen");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 1);
        assert!(members[0].revoked_at.is_some());
        assert_eq!(store.configuration("managed.cursor"), Ok("2".to_owned()));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn control_records_are_durable() {
        let database = path("control-record");
        let mut store = ControlStore::open(&database).expect("control must open");
        store.put_scope("scope", "directory").expect("scope must store");
        let record = SignedRecord {
            id: "record".to_owned(),
            scope_id: "scope".to_owned(),
            agent_id: "agent".to_owned(),
            record: vec![1],
            expires_at: 1,
        };
        store.put_signed_record(&record).expect("record must store");
        drop(store);
        let mut store = ControlStore::open(database).expect("control must open");
        assert_eq!(store.signed_record("agent"), Ok(record));
        assert_eq!(store.active_signed_records(0).expect("records must load").len(), 1);
        assert!(store.active_signed_records(1).expect("records must load").is_empty());
        store.remove_signed_record("agent").expect("record must remove");
        assert_eq!(store.signed_record("agent"), Err(StoreError::NotFound));
    }

    #[test]
    fn managed_invitation_redemption_is_durable_and_atomic() {
        let database = path("managed-members");
        let mut store = ControlStore::open(&database).expect("control must open");
        let first_token = [1; 32];
        let second_token = [2; 32];
        let expires_at = i64::MAX;
        store
            .create_managed_invitation("first", "managed", &first_token, expires_at)
            .expect("first invitation must store");
        store
            .create_managed_invitation("second", "managed", &second_token, expires_at)
            .expect("second invitation must store");
        let first_member = ManagedMember {
            scope_id: "managed".to_owned(),
            agent_id: "first-agent".to_owned(),
            endpoint_id: "first-endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at,
            revoked_at: None,
        };
        store
            .redeem_managed_invitation(&first_token, &first_member)
            .expect("first invitation must redeem");
        let conflicting_member = ManagedMember {
            agent_id: "other-agent".to_owned(),
            endpoint_id: first_member.endpoint_id.clone(),
            ..first_member.clone()
        };
        assert!(
            store
                .redeem_managed_invitation(&second_token, &conflicting_member)
                .is_err()
        );
        let second_member = ManagedMember {
            agent_id: "second-agent".to_owned(),
            endpoint_id: "second-endpoint".to_owned(),
            ..first_member.clone()
        };
        store
            .redeem_managed_invitation(&second_token, &second_member)
            .expect("failed redemption must not consume the invitation");
        store
            .revoke_managed_member("managed", "first-agent")
            .expect("member must revoke");
        let events =
            store.managed_events_after("managed", 0, 128).expect("events must load");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "joined");
        assert_eq!(events[1].event_type, "joined");
        assert_eq!(events[2].event_type, "revoked");
        assert!(events[0].member.is_some());
        assert!(events[2].member.is_none());
        assert!(
            store
                .managed_events_after("managed", events[1].sequence, 128)
                .expect("events must resume")
                .iter()
                .all(|event| event.sequence > events[1].sequence)
        );
        drop(store);

        let store = ControlStore::open(&database).expect("control must reopen");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].agent_id, first_member.agent_id);
        assert_eq!(members[0].endpoint_id, first_member.endpoint_id);
        assert_eq!(members[0].role, first_member.role);
        assert_eq!(members[0].expires_at, first_member.expires_at);
        assert!(members[0].revoked_at.is_some());
        assert_eq!(members[1], second_member);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn migration_six_restores_task_foreign_keys_and_constraints() {
        let database = path("migration-six");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..5])
            .expect("old migrations must apply");
        connection
            .execute("INSERT INTO scopes(id,kind) VALUES('scope','direct')", [])
            .expect("scope must insert");
        connection
            .execute(
                "INSERT INTO tasks(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,state,created_at) VALUES('task','request','key','agent','endpoint','fingerprint','scope','capability','workspace','read_only','queued',1)",
                [],
            )
            .expect("task must insert");
        connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('task',1,'queued','{}',1)",
                [],
            )
            .expect("event must insert");
        connection
            .execute(
                "INSERT INTO execution_leases(task_id,owner,execution_id,expires_at) VALUES('task','owner','execution',2)",
                [],
            )
            .expect("lease must insert");
        drop(connection);
        let store = NodeStore::open(&database).expect("repair migration must apply");
        assert_eq!(store.events("task").expect("events must load").len(), 1);
        let foreign_key_error: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get(0),
            )
            .expect("foreign key check must run");
        assert!(!foreign_key_error);
        assert!(store
            .connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('missing',1,'queued','{}',1)",
                [],
            )
            .is_err());
        assert!(store
            .connection
            .execute(
                "INSERT INTO execution_leases(task_id,owner,execution_id,expires_at) VALUES('missing','owner','execution',2)",
                [],
            )
            .is_err());
        assert!(store
            .connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('task',2,'','{}',1)",
                [],
            )
            .is_err());
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn legacy_migration_ledger_converts_once_to_user_version() {
        let database = path("legacy-migration-ledger");
        let connection = Connection::open(&database).expect("database must open");
        for migration in &NODE_MIGRATIONS[..5] {
            connection.execute_batch(migration).expect("legacy migration must apply");
        }
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY NOT NULL); INSERT INTO schema_migrations(version) VALUES(5);",
            )
            .expect("legacy ledger must store");
        drop(connection);
        let store = NodeStore::open(&database).expect("legacy database must convert");
        let user_version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user version must load");
        assert_eq!(usize::try_from(user_version).ok(), Some(NODE_MIGRATIONS.len()));
        drop(store);
        NodeStore::open(&database).expect("converted database must reopen");
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn audit_records_are_durable_and_redacted() {
        let (store, database) = store();
        add_audit(&store, AuditActor::Mcp, AuditAction::Change, AuditResult::Pending);
        add_audit(&store, AuditActor::Ui, AuditAction::Change, AuditResult::Confirmed);
        assert_eq!(
            store.audit_records().expect("audit must load"),
            vec![
                super::AuditRecord {
                    actor_type: "mcp".to_owned(),
                    action: "change".to_owned(),
                    result: "pending".to_owned(),
                },
                super::AuditRecord {
                    actor_type: "ui".to_owned(),
                    action: "change".to_owned(),
                    result: "confirmed".to_owned(),
                },
            ]
        );
        store
            .connection
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("unsafe audit record must insert for the test");
        assert_eq!(store.audit_records(), Err(StoreError::InvalidState));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn opening_a_current_database_does_not_create_a_backup() {
        let database = path("current");
        let store = NodeStore::open(&database).expect("store must open");
        drop(store);
        NodeStore::open(&database).expect("current store must open");
        let prefix = format!("{}{}", database.display(), ".backup.");
        let backups = fs::read_dir(database.parent().expect("parent must exist"))
            .expect("parent must read")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(backups, 0);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn online_backups_preserve_data_and_apply_retention() {
        let (store, database) = store();
        store.put_configuration("owner", "operator").expect("configuration must store");
        let mut backups = Vec::new();
        for _ in 0..=BACKUP_RETENTION {
            backups.push(backup(&database).expect("backup must work"));
        }
        assert_eq!(
            backups.iter().filter(|path| path.exists()).count(),
            BACKUP_RETENTION
        );
        let latest = backups.last().expect("backup must exist");
        let restored = NodeStore::open(latest).expect("backup must open");
        assert_eq!(restored.configuration("owner"), Ok("operator".to_owned()));
        drop(restored);
        drop(store);
        fs::remove_file(database).expect("database must remove");
        for backup in backups.into_iter().filter(|path| path.exists()) {
            fs::remove_file(backup).expect("backup must remove");
        }
    }

    #[test]
    fn concurrent_admission_returns_the_existing_task() {
        use std::sync::{Arc, Barrier};

        let database = path("concurrent-admission");
        let store = NodeStore::open(&database).expect("store must open");
        store.put_scope("scope", "direct").expect("scope must store");
        drop(store);
        let barrier = Arc::new(Barrier::new(2));
        let input = input("one", "a");
        #[expect(
            clippy::needless_collect,
            reason = "both threads must start before the first join, or the barrier never opens"
        )]
        let threads = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                let input = input.clone();
                std::thread::spawn(move || {
                    let mut store = NodeStore::open(database).expect("store must open");
                    barrier.wait();
                    store
                        .admit(&input, QueueLimits { global: 2, per_peer: 2 })
                        .expect("admission must work")
                })
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread must finish"))
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Admission::Accepted(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Admission::Existing(_)))
                .count(),
            1
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn replay_keys_are_scoped_expire_and_survive_restart() {
        let (mut store, database) = store();
        accept_replay_key(&mut store, "endpoint-a", "exchange", i64::MAX, 2)
            .expect("replay key must store");
        assert_eq!(
            accept_replay_key(&mut store, "endpoint-a", "exchange", i64::MAX, 2),
            Err(StoreError::DuplicateConflict)
        );
        accept_replay_key(&mut store, "endpoint-b", "exchange", i64::MAX, 2)
            .expect("other peer may use the key");
        drop(store);
        let mut store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            accept_replay_key(&mut store, "endpoint-a", "exchange", i64::MAX, 2),
            Err(StoreError::DuplicateConflict)
        );
        accept_replay_key(&mut store, "endpoint-a", "expired", 0, 2)
            .expect("expired key must store");
        accept_replay_key(&mut store, "endpoint-a", "replacement", i64::MAX, 2)
            .expect("expired key must clear");
        accept_replay_key(&mut store, "endpoint-a", "limited", i64::MAX, 2)
            .expect("limited key must store");
        accept_replay_key(&mut store, "endpoint-a", "newer", i64::MAX, 2)
            .expect("oldest key must expire at the limit");
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn replay_admission_is_atomic_and_capacity_is_per_caller_scope() {
        let (mut store, database) = store();
        let mut missing_scope = input("missing", "a");
        missing_scope.scope_id = "missing".to_owned();
        assert!(
            store
                .admit_with_replay(
                    &missing_scope,
                    QueueLimits { global: 2, per_peer: 2 },
                    "missing-replay",
                    i64::MAX,
                    1,
                )
                .is_err()
        );
        let replay_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM replay_entries", [], |row| row.get(0))
            .expect("replay count must load");
        assert_eq!(replay_count, 0);
        assert_eq!(
            store
                .admit_with_replay(
                    &input("one", "a"),
                    QueueLimits { global: 2, per_peer: 2 },
                    "replay-one",
                    i64::MAX,
                    1,
                )
                .expect("first replay and task must store"),
            Admission::Accepted("one".to_owned())
        );
        accept_replay_key(&mut store, "endpoint-b", "peer-replay", i64::MAX, 1)
            .expect("other caller replay must store");
        accept_replay_key(&mut store, "endpoint-b", "peer-replay-new", i64::MAX, 1)
            .expect("other caller capacity may replace only its own key");
        assert_eq!(
            accept_replay_key(&mut store, "endpoint-a", "replay-one", i64::MAX, 1),
            Err(StoreError::DuplicateConflict)
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn events_page_bounds_by_limit_and_reports_more_events() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        // One `queued` event already exists from admission; add more so the
        // journal holds more events than a small page can carry.
        for _ in 0..9 {
            store.append_event("task", "progress").expect("event must append");
        }
        let (first_page, first_more) =
            store.events_page("task", 0, 4).expect("first page must load");
        assert_eq!(first_page.len(), 4);
        assert_eq!(
            first_page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(first_more, "more events must remain after a truncated page");
        let cursor = first_page.last().expect("page must be non-empty").sequence;
        let (second_page, second_more) =
            store.events_page("task", cursor, 4).expect("second page must load");
        assert_eq!(
            second_page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            vec![5, 6, 7, 8]
        );
        assert!(second_more);
        let cursor = second_page.last().expect("page must be non-empty").sequence;
        let (last_page, last_more) =
            store.events_page("task", cursor, 4).expect("last page must load");
        assert_eq!(
            last_page.iter().map(|event| event.sequence).collect::<Vec<_>>(),
            vec![9, 10]
        );
        assert!(!last_more, "the final page must not claim more events remain");
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn task_snapshot_reads_state_and_result_without_the_event_journal() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        assert_eq!(
            store.task_snapshot("task").expect("snapshot must load"),
            (TaskState::Queued, None)
        );
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        store
            .mark_succeeded("task", "execution", "final result", 20)
            .expect("task must succeed");
        assert_eq!(
            store.task_snapshot("task").expect("snapshot must load"),
            (TaskState::Succeeded, Some("final result".to_owned()))
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn task_progress_reads_the_snapshot_and_page_together() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        store
            .mark_succeeded("task", "execution", "final result", 20)
            .expect("task must succeed");
        assert_eq!(
            store.task_progress("task", 0, 2).expect("progress must load"),
            (
                TaskState::Succeeded,
                Some("final result".to_owned()),
                vec![
                    TaskEvent { sequence: 1, event_type: "queued".to_owned() },
                    TaskEvent { sequence: 2, event_type: "leased".to_owned() },
                ],
                true
            )
        );
        assert_eq!(
            store.task_progress("task", 2, 2).expect("progress must load"),
            (
                TaskState::Succeeded,
                Some("final result".to_owned()),
                vec![
                    TaskEvent { sequence: 3, event_type: "running".to_owned() },
                    TaskEvent { sequence: 4, event_type: "succeeded".to_owned() },
                ],
                false
            )
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn legacy_terminal_rows_without_expiry_are_backfilled_from_completion_evidence() {
        let database = path("legacy-content-expiry");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..9])
            .expect("migrations through 009 must apply");
        connection
            .execute("INSERT INTO scopes(id,kind) VALUES('scope','direct')", [])
            .expect("scope must insert");
        // A `failed` row from before this code ever set `input_expires_at`:
        // migration 009 skipped it because `result_expires_at` is NULL for a
        // failed task. Its last event records when it actually failed.
        connection
            .execute(
                "INSERT INTO tasks(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,state,created_at,remote_text) VALUES('legacy-failed','request-a','key-a','agent','endpoint-a','fingerprint-a','scope','capability','workspace','read_only','failed',1000,'still here')",
                [],
            )
            .expect("legacy failed task must insert");
        connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('legacy-failed',1,'queued','{}',1000),('legacy-failed',2,'failed','{}',1500)",
                [],
            )
            .expect("legacy failed events must insert");
        // A `cancelled` row with no recorded events at all (defensive case):
        // the fallback must use the task's own admission time, not `now()`.
        connection
            .execute(
                "INSERT INTO tasks(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,state,created_at,remote_text) VALUES('legacy-cancelled','request-b','key-b','agent','endpoint-b','fingerprint-b','scope','capability','workspace','read_only','cancelled',2000,'still here too')",
                [],
            )
            .expect("legacy cancelled task must insert");
        drop(connection);

        let mut store =
            NodeStore::open(&database).expect("legacy migration must apply");
        let expiry: (Option<i64>, Option<i64>) = store
            .connection
            .query_row(
                "SELECT input_expires_at,content_expired_at FROM tasks WHERE id='legacy-failed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row must load");
        assert_eq!(expiry.0, Some(1500 + CONTENT_RETENTION_SECONDS));
        assert_eq!(
            expiry.1, None,
            "backfilling the deadline must not itself clear content"
        );
        let fallback_expiry: Option<i64> = store
            .connection
            .query_row(
                "SELECT input_expires_at FROM tasks WHERE id='legacy-cancelled'",
                [],
                |row| row.get(0),
            )
            .expect("row must load");
        assert_eq!(fallback_expiry, Some(2000 + CONTENT_RETENTION_SECONDS));
        // Neither backfilled deadline should be anywhere near a freshly
        // started 24-hour window from migration time: both are already long
        // past, so the very next cleanup pass reclaims them immediately.
        assert!(store.clear_expired_content(now()).expect("cleanup must run") >= 2);
        assert_eq!(
            store.stored_task("legacy-failed").expect("task must load").remote_text,
            ""
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn clear_expired_content_is_bounded_by_batch_size() {
        let (mut store, database) = store();
        for index in 0..3 {
            let id = format!("task-{index}");
            store
                .admit(&input(&id, "caller"), QueueLimits { global: 10, per_peer: 10 })
                .expect("task must admit");
            let execution_id = format!("exec-{index}");
            store.lease_next("worker", &execution_id, 10).expect("lease");
            store.mark_running(&id, &execution_id).expect("task must run");
            store.mark_failed(&id, &execution_id).expect("task must fail");
        }
        let cutoff = now() + CONTENT_RETENTION_SECONDS;
        assert_eq!(
            store
                .clear_expired_content_batch(cutoff, 2)
                .expect("bounded cleanup must run"),
            2,
            "a batch limit of 2 must clear only two of the three eligible rows"
        );
        assert_eq!(
            store
                .clear_expired_content_batch(cutoff, 2)
                .expect("remaining cleanup must run"),
            1,
            "a later pass must clear whatever the batch limit left behind"
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn unknown_result_from_lease_recovery_gets_a_definite_retention_deadline() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        let recovered = store.recover_expired_leases(10).expect("recovery must run");
        assert_eq!(recovered, vec!["task".to_owned()]);
        assert_eq!(store.state("task"), Ok(TaskState::UnknownResult));
        let input_expiry: Option<i64> = store
            .connection
            .query_row(
                "SELECT input_expires_at FROM tasks WHERE id='task'",
                [],
                |row| row.get(0),
            )
            .expect("row must load");
        assert!(
            input_expiry.is_some(),
            "an unresolved task must not keep its input forever"
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn a_valid_empty_result_is_not_confused_with_expired_content() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        store
            .mark_succeeded("task", "execution", "", 2_000_000_000)
            .expect("task must succeed with an empty result");
        assert!(!store.content_is_expired("task").expect("expiry check must run"));
        assert_eq!(
            store.stored_task("task").expect("task must load").result,
            Some(String::new()),
            "a real empty result must stay `Some(\"\")`, distinct from a cleared `None`"
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn task_input_and_final_result_follow_the_retention_rule() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        store
            .mark_succeeded("task", "execution", "final result", 20)
            .expect("task result must store");
        assert_eq!(
            store.stored_task("task").expect("stored task must load"),
            StoredTask {
                id: "task".to_owned(),
                state: TaskState::Succeeded,
                skill: "capability".to_owned(),
                remote_text: "task input".to_owned(),
                events: vec![
                    TaskEvent { sequence: 1, event_type: "queued".to_owned() },
                    TaskEvent { sequence: 2, event_type: "leased".to_owned() },
                    TaskEvent { sequence: 3, event_type: "running".to_owned() },
                    TaskEvent { sequence: 4, event_type: "succeeded".to_owned() },
                ],
                result: Some("final result".to_owned()),
            }
        );
        assert_eq!(store.clear_expired_content(20).expect("result must expire"), 1);
        assert_eq!(
            store.stored_task("task").expect("stored task must load").result,
            None
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn queue_idempotency_events_and_recovery_work() {
        let (mut store, database) = store();
        for (id, peer) in [("one", "a"), ("two", "a"), ("three", "b")] {
            store
                .admit(&input(id, peer), QueueLimits { global: 3, per_peer: 2 })
                .expect("task must admit");
        }
        assert_eq!(
            store.admit(&input("four", "b"), QueueLimits { global: 3, per_peer: 2 }),
            Err(StoreError::Overloaded)
        );
        assert_eq!(
            store.admit(&input("one", "a"), QueueLimits { global: 3, per_peer: 2 }),
            Ok(Admission::Existing("one".to_owned()))
        );
        assert_eq!(store.lease_next("worker", "one", 1), Ok(Some("one".to_owned())));
        assert_eq!(store.lease_next("worker", "two", 1), Ok(Some("three".to_owned())));
        store.mark_running("three", "two").expect("task must run");
        store.recover_expired_leases(1).expect("recovery must work");
        assert_eq!(store.state("one"), Ok(TaskState::Queued));
        assert_eq!(store.state("three"), Ok(TaskState::UnknownResult));
        assert_eq!(
            store
                .events("one")
                .expect("events must load")
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn stale_execution_cannot_overwrite_task_state() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "current", 10).expect("task must lease");
        assert_eq!(store.mark_running("task", "stale"), Err(StoreError::NotFound));
        store.mark_running("task", "current").expect("task must run");
        assert_eq!(
            store.mark_succeeded("task", "stale", "result", 20),
            Err(StoreError::NotFound)
        );
        assert_eq!(store.state("task"), Ok(TaskState::Running));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn cancel_if_owned_rejects_wrong_scope_distinctly_from_wrong_owner() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        assert_eq!(
            store.cancel_if_owned("task", "endpoint-caller", "other-scope"),
            Err(StoreError::Unauthorized),
            "the correct owner in the wrong scope must still be rejected"
        );
        assert_eq!(
            store.cancel_if_owned("task", "endpoint-other", "scope"),
            Err(StoreError::Unauthorized),
            "the wrong owner in the correct scope must still be rejected"
        );
        // A denied cancellation must not change task state or emit an event.
        assert_eq!(store.state("task"), Ok(TaskState::Queued));
        assert_eq!(
            store
                .events("task")
                .expect("events must load")
                .into_iter()
                .map(|event| event.event_type)
                .collect::<Vec<_>>(),
            ["queued"]
        );
        store
            .cancel_if_owned("task", "endpoint-caller", "scope")
            .expect("the correct owner and scope must cancel");
        assert_eq!(store.state("task"), Ok(TaskState::Cancelled));
        // Repeating a valid cancellation from the correct owner must stay
        // idempotent: no error and no duplicate event.
        store
            .cancel_if_owned("task", "endpoint-caller", "scope")
            .expect("repeated valid cancellation must succeed");
        assert_eq!(
            store
                .events("task")
                .expect("events must load")
                .into_iter()
                .map(|event| event.event_type)
                .collect::<Vec<_>>(),
            ["queued", "cancelled"]
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn a_leased_task_denied_at_dispatch_becomes_rejected_not_stuck() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        // A dispatch-time policy denial must resolve a leased task to a
        // terminal state instead of leaving it stuck in `leased` forever.
        store
            .mark_rejected("task", "execution")
            .expect("a leased task must be rejectable");
        assert_eq!(store.state("task"), Ok(TaskState::Rejected));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn unknown_result_requires_operator_review_and_guards_its_execution_id() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        assert_eq!(
            store.mark_unknown_result("task", "stale"),
            Err(StoreError::NotFound),
            "a stale execution id must not be able to record an outcome"
        );
        store
            .mark_unknown_result("task", "execution")
            .expect("an uncertain outcome must be recorded");
        assert_eq!(store.state("task"), Ok(TaskState::UnknownResult));
        assert_eq!(
            store.mark_succeeded("task", "execution", "late", 20),
            Err(StoreError::NotFound),
            "the lease is gone, so a late result must not resurrect the task"
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn cancelling_an_uncertain_execution_is_a_no_op() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.cancel("task").expect("task must cancel");
        // `mark_unknown_result` must treat an already-cancelled task the same
        // way `mark_failed` and `mark_succeeded` do: cancellation wins.
        store
            .mark_unknown_result("task", "any-execution")
            .expect("cancelling before the outcome is known must be a no-op");
        assert_eq!(store.state("task"), Ok(TaskState::Cancelled));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn a_running_lease_renews_only_for_its_execution() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        assert_eq!(
            store.renew_lease("task", "other", 20, 9),
            Err(StoreError::NotFound)
        );
        store
            .renew_lease("task", "execution", 20, 9)
            .expect("matching execution must renew before its old expiry");
        assert_eq!(
            store.renew_lease("task", "execution", 30, 20),
            Err(StoreError::NotFound),
            "a renewal at or after the lease's current expiry must not resurrect it"
        );
        store.recover_expired_leases(10).expect("early recovery must work");
        assert_eq!(store.state("task"), Ok(TaskState::Running));
        store.recover_expired_leases(20).expect("expired recovery must work");
        assert_eq!(store.state("task"), Ok(TaskState::UnknownResult));
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn constraints_cancellation_and_failed_migrations_keep_the_database_usable() {
        let (mut store, database) = store();
        assert!(
            store
                .put_signed_record("record", "missing", "agent", b"record", 1)
                .is_err()
        );
        store
            .admit(&input("one", "a"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.cancel("one").expect("task must cancel");
        assert_eq!(store.state("one"), Ok(TaskState::Cancelled));
        let migrations =
            NODE_MIGRATIONS.iter().copied().chain(["INVALID SQL"]).collect::<Vec<_>>();
        assert!(migrate(&mut store.connection, &migrations).is_err());
        store
            .put_configuration("after_failure", "usable")
            .expect("database must remain usable");
        fs::remove_file(database).expect("database must remove");
    }
}
