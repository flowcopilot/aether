use aether_core::{
    AccessMode, AgentId, CapabilityId, EndpointId, Request, RequestId, ScopeId, TaskId,
    WorkspaceId,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{NodeStore, StoreError, now, sql_count, sql_named, sql_names};

pub const CONTENT_RETENTION_SECONDS: i64 = 24 * 60 * 60;
/// Maximum rows `clear_expired_content` clears in one call.
pub const CONTENT_CLEANUP_BATCH: i64 = 500;

/// The strings are the `tasks.state` column. Changing one breaks every
/// stored row that holds it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

aether_core::named!(TaskState {
    PendingApproval => "pending_approval",
    Queued => "queued",
    Leased => "leased",
    Running => "running",
    UnknownResult => "unknown_result",
    OperatorReviewed => "operator_reviewed",
    Succeeded => "succeeded",
    Failed => "failed",
    Rejected => "rejected",
    Cancelled => "cancelled",
});

impl TaskState {
    /// The states that hold an execution lease.
    const ACTIVE: [Self; 2] = [Self::Leased, Self::Running];

    /// The states whose input and result expire after
    /// [`CONTENT_RETENTION_SECONDS`].
    const CONTENT_EXPIRES: [Self; 5] = [
        Self::Succeeded,
        Self::Failed,
        Self::Rejected,
        Self::Cancelled,
        Self::UnknownResult,
    ];

    fn sql_list(states: &[Self]) -> String {
        sql_names(&states.iter().map(Self::as_str).collect::<Vec<_>>())
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

/// What happened to a task. The names are the `task_events.event_type`
/// column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventType {
    /// The task entered this state.
    Entered(TaskState),
    /// A lease ended before the task ran, and the task waits again.
    Requeued,
    /// A lease expired before the task ran, and the task waits again.
    LeaseExpired,
    /// The input and result of an ended task were cleared.
    ContentExpired,
    /// The agent reported progress.
    Progress,
    /// The agent asked for a person's approval.
    ApprovalRequested,
    /// The agent gave its answer.
    Result,
}

impl EventType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Entered(state) => state.as_str(),
            Self::Requeued => "requeued",
            Self::LeaseExpired => "lease_expired",
            Self::ContentExpired => "content_expired",
            Self::Progress => "progress",
            Self::ApprovalRequested => "approval_requested",
            Self::Result => "result",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        TaskState::parse(value).map(Self::Entered).or(match value {
            "requeued" => Some(Self::Requeued),
            "lease_expired" => Some(Self::LeaseExpired),
            "content_expired" => Some(Self::ContentExpired),
            "progress" => Some(Self::Progress),
            "approval_requested" => Some(Self::ApprovalRequested),
            "result" => Some(Self::Result),
            _ => None,
        })
    }

    /// The state of the task right after this event. `None` for
    /// [`Self::ContentExpired`], which happens to a task in any ended state
    /// and leaves that state as it is.
    #[must_use]
    pub const fn task_state(self) -> Option<TaskState> {
        match self {
            Self::Entered(state) => Some(state),
            Self::Requeued | Self::LeaseExpired => Some(TaskState::Queued),
            Self::Progress | Self::ApprovalRequested | Self::Result => {
                Some(TaskState::Running)
            }
            Self::ContentExpired => None,
        }
    }
}

sql_named!(TaskState, EventType);

/// A task to queue: the admitted request and what the caller sent with it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskInput {
    pub request: Request,
    pub idempotency_key: String,
    pub request_fingerprint: String,
    pub skill: String,
    pub remote_text: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct QueueLimits {
    pub global: usize,
    pub per_peer: usize,
}

/// How the queue took a task: as a new one, or as the task that the same
/// request already queued.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TaskAdmission {
    Accepted(String),
    Existing(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskEvent {
    pub sequence: i64,
    pub event_type: EventType,
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
    pub event_type: EventType,
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
    pub request: Request,
    pub skill: String,
    pub remote_text: String,
}

/// The columns [`request_from_row`] reads, in its order.
const REQUEST_COLUMNS: &str = "id,request_id,caller_agent_id,caller_endpoint_id,scope_id,capability_id,workspace_id,access_mode";

/// Reads the [`REQUEST_COLUMNS`] that start at column `first`.
fn request_from_row(
    row: &rusqlite::Row<'_>,
    first: usize,
) -> rusqlite::Result<Request> {
    let column = |offset: usize| first.saturating_add(offset);
    let access_mode: String = row.get(column(7))?;
    Ok(Request {
        task_id: TaskId::new(row.get::<_, String>(column(0))?),
        id: RequestId::new(row.get::<_, String>(column(1))?),
        caller_agent_id: AgentId::new(row.get::<_, String>(column(2))?),
        caller_endpoint_id: EndpointId::new(row.get::<_, String>(column(3))?),
        scope_id: ScopeId::new(row.get::<_, String>(column(4))?),
        capability_id: CapabilityId::new(row.get::<_, String>(column(5))?),
        workspace_id: WorkspaceId::new(row.get::<_, String>(column(6))?),
        access_mode: AccessMode::parse(&access_mode).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                column(7),
                rusqlite::types::Type::Text,
                "unknown access mode".into(),
            )
        })?,
    })
}

/// How many tasks the queue holds.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TaskCounts {
    pub all: usize,
    pub queued: usize,
    /// The callers with a leased or running task.
    pub active_callers: usize,
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

impl NodeStore {
    pub fn task_counts(&self) -> Result<TaskCounts, StoreError> {
        let count =
            |sql: &str, state: Option<TaskState>| -> Result<usize, StoreError> {
                let value: i64 = match state {
                    Some(state) => {
                        self.connection.query_row(sql, [state], |row| row.get(0))?
                    }
                    None => self.connection.query_row(sql, [], |row| row.get(0))?,
                };
                usize::try_from(value).map_err(|_| StoreError::InvalidState)
            };
        Ok(TaskCounts {
            all: count("SELECT COUNT(*) FROM tasks", None)?,
            queued: count(
                "SELECT COUNT(*) FROM tasks WHERE state=?1",
                Some(TaskState::Queued),
            )?,
            active_callers: count(
                &format!(
                    "SELECT COUNT(DISTINCT caller_endpoint_id) FROM tasks WHERE state IN ({})",
                    TaskState::sql_list(&TaskState::ACTIVE)
                ),
                None,
            )?,
        })
    }

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
            Ok(LastTask {
                caller_endpoint_id: row.get(0)?,
                state: row.get(1)?,
                created_at: row.get(2)?,
                remote_text: row.get(3)?,
                result: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn admit(
        &mut self,
        input: &TaskInput,
        limits: QueueLimits,
    ) -> Result<TaskAdmission, StoreError> {
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
    ) -> Result<TaskAdmission, StoreError> {
        if replay_key.is_empty() || replay_limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        accept_replay_key(
            &tx,
            input.request.caller_endpoint_id.as_str(),
            input.request.scope_id.as_str(),
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
) -> Result<TaskAdmission, StoreError> {
    let request = &input.request;
    let existing = tx
        .query_row(
            &format!(
                "SELECT {REQUEST_COLUMNS},request_fingerprint FROM tasks WHERE caller_endpoint_id=?1 AND idempotency_key=?2"
            ),
            params![request.caller_endpoint_id.as_str(), input.idempotency_key],
            |row| Ok((request_from_row(row, 0)?, row.get::<_, String>(8)?)),
        )
        .optional()?;
    if let Some((stored, fingerprint)) = existing {
        return if stored == *request && fingerprint == input.request_fingerprint {
            Ok(TaskAdmission::Existing(stored.task_id.as_str().to_owned()))
        } else {
            Err(StoreError::DuplicateConflict)
        };
    }
    if count(tx, None)? >= limits.global
        || count(tx, Some(request.caller_agent_id.as_str()))? >= limits.per_peer
    {
        return Err(StoreError::Overloaded);
    }
    tx.execute(
        &format!(
            "INSERT INTO tasks({REQUEST_COLUMNS},idempotency_key,request_fingerprint,skill,remote_text,state,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"
        ),
        params![
            request.task_id.as_str(),
            request.id.as_str(),
            request.caller_agent_id.as_str(),
            request.caller_endpoint_id.as_str(),
            request.scope_id.as_str(),
            request.capability_id.as_str(),
            request.workspace_id.as_str(),
            request.access_mode.as_str(),
            input.idempotency_key,
            input.request_fingerprint,
            input.skill,
            input.remote_text,
            TaskState::Queued,
            now(),
        ],
    )?;
    let task_id = request.task_id.as_str();
    event(tx, task_id, EventType::Entered(TaskState::Queued))?;
    Ok(TaskAdmission::Accepted(task_id.to_owned()))
}

impl NodeStore {
    pub fn state(&self, task_id: &str) -> Result<TaskState, StoreError> {
        self.connection
            .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn events(&self, task_id: &str) -> Result<Vec<TaskEvent>, StoreError> {
        self.events_after(task_id, 0)
    }

    pub fn stored_task(&self, task_id: &str) -> Result<StoredTask, StoreError> {
        let (id, state, skill, remote_text, result): (
            String,
            TaskState,
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
            state,
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
        let fetch = sql_count(limit.saturating_add(1));
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
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
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
        event_type: EventType,
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
        let task = tx
            .query_row(
                &format!(
                    "SELECT queued.id FROM tasks queued WHERE queued.state=?1
                     ORDER BY (SELECT COUNT(*) FROM tasks active WHERE active.caller_agent_id=queued.caller_agent_id AND active.state IN ({})), queued.created_at, queued.id LIMIT 1",
                    TaskState::sql_list(&TaskState::ACTIVE),
                ),
                [TaskState::Queued],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(task) = task else { return Ok(None) };
        enter(&tx, &task, TaskState::Leased)?;
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
        enter(&tx, task_id, TaskState::Running)?;
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
                event(&tx, task_id, EventType::Result)?;
            }
            tx.commit()?;
            return Ok(());
        }
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        enter(&tx, task_id, TaskState::Succeeded)?;
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
        let tx = self.connection.transaction()?;
        let ids = {
            let mut statement = tx.prepare(&format!(
                "SELECT id FROM tasks
                 WHERE content_expired_at IS NULL
                   AND state IN ({})
                   AND ((input_expires_at IS NOT NULL AND input_expires_at<=?1)
                        OR (result_expires_at IS NOT NULL AND result_expires_at<=?1))
                 LIMIT ?2",
                TaskState::sql_list(&TaskState::CONTENT_EXPIRES),
            ))?;
            statement
                .query_map(params![timestamp, CONTENT_CLEANUP_BATCH], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for task_id in &ids {
            tx.execute(
                "UPDATE tasks SET remote_text='',final_result=NULL,result_expires_at=NULL,content_expired_at=?1 WHERE id=?2",
                params![timestamp, task_id],
            )?;
            event(&tx, task_id, EventType::ContentExpired)?;
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
        enter(&tx, task_id, TaskState::Failed)?;
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
        enter(&tx, task_id, TaskState::UnknownResult)?;
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
        if state(&tx, task_id)? == TaskState::Cancelled {
            tx.commit()?;
            return Ok(());
        }
        if !lease_matches(&tx, task_id, execution_id)? {
            return Err(StoreError::NotFound);
        }
        enter(&tx, task_id, TaskState::Rejected)?;
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
        enter(&tx, task_id, TaskState::Cancelled)?;
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
        enter(&tx, task_id, TaskState::Cancelled)?;
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
                &format!(
                    "SELECT {REQUEST_COLUMNS},skill,remote_text FROM tasks WHERE id=?1"
                ),
                [task_id],
                |row| {
                    Ok(TaskExecutionData {
                        request: request_from_row(row, 0)?,
                        skill: row.get(8)?,
                        remote_text: row.get(9)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn requeue_leased(&mut self, task_id: &str) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        if state(&tx, task_id)? == TaskState::Leased {
            change(&tx, task_id, TaskState::Queued, EventType::Requeued)?;
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
                    change(&tx, &id, TaskState::Queued, EventType::LeaseExpired)?;
                }
                TaskState::Running => {
                    enter(&tx, &id, TaskState::UnknownResult)?;
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
    let limit = sql_count(limit);
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

fn count(tx: &Transaction<'_>, peer: Option<&str>) -> Result<usize, StoreError> {
    let count: i64 = match peer {
        Some(peer) => tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state=?1 AND caller_agent_id=?2",
            params![TaskState::Queued, peer],
            |row| row.get(0),
        )?,
        None => tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state=?1",
            [TaskState::Queued],
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
    .map_err(StoreError::from)
}

fn state(tx: &Transaction<'_>, task_id: &str) -> Result<TaskState, StoreError> {
    tx.query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| row.get(0))
        .optional()?
        .ok_or(StoreError::NotFound)
}

fn set_input_expiry(tx: &Transaction<'_>, task_id: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE tasks SET input_expires_at=?1 WHERE id=?2 AND input_expires_at IS NULL",
        params![now().saturating_add(CONTENT_RETENTION_SECONDS), task_id],
    )?;
    Ok(())
}

/// Moves the task to `next` and records that it entered it.
fn enter(
    tx: &Transaction<'_>,
    task_id: &str,
    next: TaskState,
) -> Result<(), StoreError> {
    change(tx, task_id, next, EventType::Entered(next))
}

fn change(
    tx: &Transaction<'_>,
    task_id: &str,
    next: TaskState,
    event_type: EventType,
) -> Result<(), StoreError> {
    if !state(tx, task_id)?.allows(next) {
        return Err(StoreError::InvalidState);
    }
    tx.execute("UPDATE tasks SET state=?1 WHERE id=?2", params![next, task_id])?;
    event(tx, task_id, event_type)
}

fn event(
    tx: &Transaction<'_>,
    task_id: &str,
    event_type: EventType,
) -> Result<(), StoreError> {
    let sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM task_events WHERE task_id=?1",
        [task_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES(?1,?2,?3,'{}',?4)",
        params![task_id, sequence, event_type, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;

    use super::*;
    use crate::{migrate::migrate, test_support::*, *};

    /// Admits a new task of `peer` (endpoint `endpoint-<peer>`) under
    /// `replay_key`.
    fn accept_replay_key(
        store: &mut NodeStore,
        peer: &str,
        replay_key: &str,
        expires_at: i64,
        limit: usize,
    ) -> Result<(), StoreError> {
        let input = input(&format!("{peer}-{replay_key}"), peer);
        let limits = QueueLimits { global: 100, per_peer: 100 };
        store
            .admit_with_replay(&input, limits, replay_key, expires_at, limit)
            .map(|_| ())
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
    fn concurrent_admission_returns_the_existing_task() {
        use std::sync::{Arc, Barrier};

        let database = path("concurrent-admission");
        let store = NodeStore::open(&database).expect("store must open");
        store.put_scope("scope", ScopeKind::Direct).expect("scope must store");
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
                .filter(|result| matches!(result, TaskAdmission::Accepted(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, TaskAdmission::Existing(_)))
                .count(),
            1
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn replay_keys_are_scoped_expire_and_survive_restart() {
        let (mut store, database) = store();
        accept_replay_key(&mut store, "a", "exchange", i64::MAX, 2)
            .expect("replay key must store");
        assert_eq!(
            accept_replay_key(&mut store, "a", "exchange", i64::MAX, 2),
            Err(StoreError::DuplicateConflict)
        );
        accept_replay_key(&mut store, "b", "exchange", i64::MAX, 2)
            .expect("other peer may use the key");
        drop(store);
        let mut store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            accept_replay_key(&mut store, "a", "exchange", i64::MAX, 2),
            Err(StoreError::DuplicateConflict)
        );
        accept_replay_key(&mut store, "a", "expired", 0, 2)
            .expect("expired key must store");
        accept_replay_key(&mut store, "a", "replacement", i64::MAX, 2)
            .expect("expired key must clear");
        accept_replay_key(&mut store, "a", "limited", i64::MAX, 2)
            .expect("limited key must store");
        accept_replay_key(&mut store, "a", "newer", i64::MAX, 2)
            .expect("oldest key must expire at the limit");
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn replay_admission_is_atomic_and_capacity_is_per_caller_scope() {
        let (mut store, database) = store();
        let mut missing_scope = input("missing", "a");
        missing_scope.request.scope_id = ScopeId::new("missing");
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
            TaskAdmission::Accepted("one".to_owned())
        );
        accept_replay_key(&mut store, "b", "peer-replay", i64::MAX, 1)
            .expect("other caller replay must store");
        accept_replay_key(&mut store, "b", "peer-replay-new", i64::MAX, 1)
            .expect("other caller capacity may replace only its own key");
        assert_eq!(
            accept_replay_key(&mut store, "a", "replay-one", i64::MAX, 1),
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
            store.append_event("task", EventType::Progress).expect("event must append");
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
                    TaskEvent {
                        sequence: 1,
                        event_type: EventType::parse("queued").expect("event names")
                    },
                    TaskEvent {
                        sequence: 2,
                        event_type: EventType::parse("leased").expect("event names")
                    },
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
                    TaskEvent {
                        sequence: 3,
                        event_type: EventType::parse("running").expect("event names")
                    },
                    TaskEvent {
                        sequence: 4,
                        event_type: EventType::parse("succeeded").expect("event names")
                    },
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
        let tasks = CONTENT_CLEANUP_BATCH.saturating_add(1);
        let limits = QueueLimits { global: 1_000, per_peer: 1_000 };
        for index in 0..tasks {
            let id = format!("task-{index}");
            store.admit(&input(&id, "caller"), limits).expect("task must admit");
            store.cancel(&id).expect("task must cancel");
        }
        let cutoff = now() + CONTENT_RETENTION_SECONDS;
        assert_eq!(
            store
                .clear_expired_content(cutoff)
                .ok()
                .and_then(|n| i64::try_from(n).ok()),
            Some(CONTENT_CLEANUP_BATCH),
            "one call must clear at most one batch"
        );
        assert_eq!(
            store.clear_expired_content(cutoff).ok(),
            Some(1),
            "a later call must clear whatever the batch left behind"
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
                    TaskEvent {
                        sequence: 1,
                        event_type: EventType::parse("queued").expect("event names")
                    },
                    TaskEvent {
                        sequence: 2,
                        event_type: EventType::parse("leased").expect("event names")
                    },
                    TaskEvent {
                        sequence: 3,
                        event_type: EventType::parse("running").expect("event names")
                    },
                    TaskEvent {
                        sequence: 4,
                        event_type: EventType::parse("succeeded").expect("event names")
                    },
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
            Ok(TaskAdmission::Existing("one".to_owned()))
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
                .map(|event| event.event_type.as_str())
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
                .map(|event| event.event_type.as_str())
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
    fn rejecting_a_task_cancelled_while_leased_is_a_no_op() {
        let (mut store, database) = store();
        store
            .admit(&input("task", "caller"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.cancel("task").expect("task must cancel");
        store
            .mark_rejected("task", "execution")
            .expect("a dispatch denial after cancellation must be a no-op");
        assert_eq!(store.state("task"), Ok(TaskState::Cancelled));
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
            assert_eq!(TaskState::parse(name), Some(state));
            assert_eq!(EventType::parse(name), Some(EventType::Entered(state)));
        }
        assert_eq!(TaskState::parse("Queued"), None);
        for (event, name) in [
            (EventType::Requeued, "requeued"),
            (EventType::LeaseExpired, "lease_expired"),
            (EventType::ContentExpired, "content_expired"),
            (EventType::Progress, "progress"),
            (EventType::ApprovalRequested, "approval_requested"),
            (EventType::Result, "result"),
        ] {
            assert_eq!(event.as_str(), name);
            assert_eq!(EventType::parse(name), Some(event));
        }
    }
}
