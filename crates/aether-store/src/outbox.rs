use std::sync::LazyLock;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{NodeStore, StoreError, column_u64, sql_count, sql_named, sql_names};

/// The state of a task that this node sent. The strings are the
/// `sent_tasks.state` column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

aether_core::named!(SentState {
    Waiting => "waiting",
    Working => "working",
    Answered => "answered",
    Failed => "failed",
    NotDelivered => "not_delivered",
});

sql_named!(SentState);

impl SentState {
    /// The states in which the outbox still sends or polls the task.
    const ACTIVE: [Self; 2] = [Self::Waiting, Self::Working];

    #[must_use]
    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }
}

/// `state IN (...)` of the active states, for SQL.
static ACTIVE: LazyLock<String> = LazyLock::new(|| {
    format!("state IN ({})", sql_names(&SentState::ACTIVE.map(|state| state.as_str())))
});

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

fn sent_task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SentTask> {
    Ok(SentTask {
        task_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        agent_id: row.get(2)?,
        endpoint_id: row.get(3)?,
        authorization_json: row.get(4)?,
        text: row.get(5)?,
        state: row.get(6)?,
        reason: row.get(7)?,
        host_state: row.get(8)?,
        answer: row.get(9)?,
        last_event_sequence: column_u64(row, 10)?,
        attempts: column_u64(row, 11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        next_attempt_at: row.get(14)?,
        expires_at: row.get(15)?,
        finished_at: row.get(16)?,
        read_at: row.get(17)?,
        lease_until: row.get(18)?,
    })
}

impl NodeStore {
    /// Keeps a new task for the outbox, waiting and due at once.
    pub fn insert_sent_task(&self, input: &SentTaskInput) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO sent_tasks(task_id,idempotency_key,agent_id,endpoint_id,authorization_json,text,state,created_at,updated_at,next_attempt_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?9,?7,?7,?7,?8)",
            params![
                input.task_id,
                input.idempotency_key,
                input.agent_id,
                input.endpoint_id,
                input.authorization_json,
                input.text,
                input.created_at,
                input.expires_at,
                SentState::Waiting,
            ],
        )?;
        Ok(())
    }

    pub fn sent_task(&self, task_id: &str) -> Result<Option<SentTask>, StoreError> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {SENT_COLUMNS} FROM sent_tasks WHERE task_id=?1"),
                [task_id],
                sent_task_from_row,
            )
            .optional()?)
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
        Ok(rows.collect::<Result<_, _>>()?)
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
                "SELECT {SENT_COLUMNS} FROM sent_tasks s WHERE {ACTIVE} AND next_attempt_at<=?1 AND (lease_until IS NULL OR lease_until<=?1) AND NOT EXISTS (SELECT 1 FROM sent_tasks o WHERE o.endpoint_id=s.endpoint_id AND o.task_id<>s.task_id AND o.{ACTIVE} AND o.lease_until>?1) ORDER BY next_attempt_at, created_at, task_id", ACTIVE = ACTIVE.as_str()
            ))?;
            let rows = statement.query_map([now], sent_task_from_row)?;
            rows.collect::<Result<Vec<_>, _>>()?
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
            &format!("UPDATE sent_tasks SET lease_until=?3 WHERE task_id=?1 AND {ACTIVE} AND (lease_until IS NULL OR lease_until<=?2)", ACTIVE = ACTIVE.as_str()),
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
            &format!("UPDATE sent_tasks SET state=?2, reason=?3, host_state=?4, answer=?5, last_event_sequence=?6, next_attempt_at=?7, attempts=attempts+1, updated_at=?8, finished_at=?9, lease_until=NULL WHERE task_id=?1 AND {ACTIVE} AND lease_until IS ?10", ACTIVE = ACTIVE.as_str()),
            params![
                task_id,
                attempt.state,
                attempt.reason,
                attempt.host_state,
                attempt.answer,
                i64::try_from(attempt.last_event_sequence)
                    .map_err(|_| StoreError::InvalidState)?,
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
            &format!("UPDATE sent_tasks SET next_attempt_at=?2 WHERE endpoint_id=?1 AND {ACTIVE} AND next_attempt_at>?2", ACTIVE = ACTIVE.as_str()),
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
            &format!("UPDATE sent_tasks SET next_attempt_at=MAX(next_attempt_at, ?2) WHERE endpoint_id=?1 AND {ACTIVE} AND lease_until IS NULL", ACTIVE = ACTIVE.as_str()),
            params![endpoint_id, until],
        )?)
    }

    pub fn active_sent_tasks(&self) -> Result<Vec<SentTask>, StoreError> {
        self.sent_tasks_where(
            &format!("{ACTIVE} ORDER BY created_at, task_id", ACTIVE = ACTIVE.as_str()),
            &[],
        )
    }

    pub fn has_active_sent_tasks(&self) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM sent_tasks WHERE {ACTIVE})",
                ACTIVE = ACTIVE.as_str()
            ),
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
            &format!("UPDATE sent_tasks SET state=?4, reason=?2, updated_at=?3, finished_at=?3, lease_until=NULL WHERE endpoint_id=?1 AND {ACTIVE}", ACTIVE = ACTIVE.as_str()),
            params![endpoint_id, reason, now, SentState::NotDelivered],
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
            &format!("UPDATE sent_tasks SET state=?3, reason=?2, updated_at=?1, finished_at=?1, lease_until=NULL WHERE {ACTIVE} AND expires_at<=?1", ACTIVE = ACTIVE.as_str()),
            params![now, reason, SentState::NotDelivered],
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
        let limit = sql_count(limit);
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
            &format!("UPDATE sent_tasks SET read_at=COALESCE(read_at, ?2) WHERE task_id=?1 AND NOT {ACTIVE}", ACTIVE = ACTIVE.as_str()),
            params![task_id, now],
        )? == 1)
    }

    /// Deletes the ended tasks that ended at `before` or earlier.
    pub fn purge_sent_tasks(&self, before: i64) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            &format!(
                "DELETE FROM sent_tasks WHERE NOT {ACTIVE} AND finished_at<=?1",
                ACTIVE = ACTIVE.as_str()
            ),
            [before],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rusqlite::Connection;

    use super::*;
    use crate::{migrate::migrate, test_support::*, *};

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
        assert_eq!(backup_count(&database), 1);
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

    #[test]
    fn sent_state_names_stay_the_same() {
        for (state, name, active) in [
            (SentState::Waiting, "waiting", true),
            (SentState::Working, "working", true),
            (SentState::Answered, "answered", false),
            (SentState::Failed, "failed", false),
            (SentState::NotDelivered, "not_delivered", false),
        ] {
            assert_eq!(state.as_str(), name);
            assert_eq!(SentState::parse(name), Some(state));
            assert_eq!(state.is_active(), active);
        }
    }
}
