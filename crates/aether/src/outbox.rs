//! The outbox: the tasks that this node sends to other agents.
//!
//! A task waits here until its agent answers, refuses it, or cannot be
//! reached before its deadline. Each attempt sends the same task ID and
//! idempotency key, so the agent keeps one task however often the node
//! sends it. The answer stays here for the inbox. The hub carries none of it.

use aether_core::time::now;
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use aether_store::{
    NodeStore, SentAttempt, SentState, SentTask, SentTaskInput, StoreError,
};

use crate::{
    direct::{InvitationBundle, RequestIds},
    transport::{
        A2aAuthorization, A2aTaskStatus, Frame, FrameKind, Link, TransportError,
    },
};

pub const MAX_WAIT: Duration = Duration::from_hours(7 * 24);
pub const PRESENCE_INTERVAL: Duration = Duration::from_secs(60);
pub const ATTEMPT_WINDOW: Duration = Duration::from_secs(25);
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const LEASE: Duration = Duration::from_secs(90);
pub const MAX_PARALLEL: usize = 4;
pub const RETENTION: Duration = Duration::from_hours(7 * 24);
pub const OFFLINE_PROBE: Duration = Duration::from_mins(30);

const UNREACHABLE: &str = "the agent cannot be reached; it may be offline";
const ACCESS_ENDED: &str = "the access to this agent ended";
const EXPIRED: &str = "the agent did not answer before the deadline";
const PURGE_KEY: &str = "outbox.last_purge";

/// Sends one request frame to the agent of `bundle` and returns its answer.
/// `link` is the connection of one attempt, kept between its polls.
pub trait Courier: Send + Sync {
    fn exchange<'a>(
        &'a self,
        bundle: &'a InvitationBundle,
        link: &'a mut Link,
        request: Frame,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Frame>, TransportError>> + Send + 'a>>;
}

/// A courier that reaches each agent through the relay of its invite.
pub struct RelayCourier {
    endpoint: iroh::Endpoint,
}

impl RelayCourier {
    /// A courier on the node's own endpoint.
    #[must_use]
    pub const fn shared(endpoint: iroh::Endpoint) -> Self {
        Self { endpoint }
    }

    /// A courier on a new endpoint with the key of `data_dir`, for a process
    /// without a running node. Close it after the attempt.
    pub async fn bind(data_dir: &Path, relay: &str) -> Result<Self, TransportError> {
        let endpoint = crate::transport::bind_node(data_dir, 0, Some(relay)).await?;
        Ok(Self { endpoint })
    }

    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

impl Courier for RelayCourier {
    fn exchange<'a>(
        &'a self,
        bundle: &'a InvitationBundle,
        link: &'a mut Link,
        request: Frame,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Frame>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let relay = bundle.relay_url.as_deref().ok_or(TransportError::Address)?;
            let remote = crate::transport::relay_endpoint_addr(
                &bundle.invitation.endpoint_id,
                relay,
            )?;
            link.exchange(&self.endpoint, remote, request).await
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum OutboxError {
    #[error("no agent with this name; list the agents you can use")]
    UnknownContact,
    #[error("the access to this agent has expired; ask for a new invite")]
    Expired,
    #[error(
        "this agent has no relay; send the task with `aether node request --address`"
    )]
    NoRelay,
    #[error("the outbox is unavailable")]
    Unavailable,
}

fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// Keeps the task `text` for the agent named `agent` at Unix time `now`.
/// It ends at the earlier of [`MAX_WAIT`] and the end of the access.
pub fn enqueue(
    data_dir: &Path,
    agent: &str,
    text: &str,
    now: i64,
) -> Result<SentTask, OutboxError> {
    let store =
        crate::direct::open_store(data_dir).map_err(|_| OutboxError::Unavailable)?;
    let bundle = match crate::direct::contact_by_name(&store, agent) {
        Ok(bundle) => bundle,
        Err(_) if crate::direct::contact_bundle(&store, agent).is_ok() => {
            return Err(OutboxError::Expired);
        }
        Err(_) => return Err(OutboxError::UnknownContact),
    };
    if bundle.relay_url.is_none() {
        return Err(OutboxError::NoRelay);
    }
    let access_ends =
        i64::try_from(crate::direct::access_ends_at(&bundle)).unwrap_or(i64::MAX);
    if access_ends <= now {
        return Err(OutboxError::Expired);
    }
    let ids = RequestIds::fresh();
    let input = SentTaskInput {
        task_id: ids.task_id.clone(),
        idempotency_key: ids.idempotency_key,
        agent_id: bundle.invitation.agent_id.clone(),
        endpoint_id: bundle.invitation.endpoint_id.clone(),
        authorization_json: serde_json::to_string(&bundle.authorization)
            .map_err(|_| OutboxError::Unavailable)?,
        text: text.to_owned(),
        created_at: now,
        expires_at: now.saturating_add(seconds(MAX_WAIT)).min(access_ends),
    };
    let store = NodeStore::in_dir(data_dir).map_err(|_| OutboxError::Unavailable)?;
    store.insert_sent_task(&input).map_err(|_| OutboxError::Unavailable)?;
    store.sent_task(&ids.task_id).ok().flatten().ok_or(OutboxError::Unavailable)
}

/// Why a task is tried again later.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Delay {
    /// The agent works on the task: ask again soon.
    Working,
    /// The queue of the agent is full.
    Busy,
    /// The agent cannot be reached.
    Unreachable,
}

/// The time before the next attempt after `attempts` attempts, for `delay`.
#[must_use]
pub fn next_delay(delay: Delay, attempts: u64) -> Duration {
    let (first, cap) = match delay {
        Delay::Working => return Duration::from_secs(5),
        Delay::Busy => (Duration::from_secs(5), Duration::from_mins(5)),
        Delay::Unreachable => (Duration::from_secs(30), OFFLINE_PROBE),
    };
    let factor =
        1_u32.checked_shl(u32::try_from(attempts.min(16)).unwrap_or(16)).unwrap_or(1);
    first.saturating_mul(factor).min(cap)
}

/// What one answer of an agent means for its task.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Outcome {
    pub state: SentState,
    pub reason: Option<String>,
    pub host_state: Option<String>,
    pub answer: Option<String>,
    pub last_event_sequence: u64,
    /// When the task is still active, why and how it waits.
    pub delay: Option<Delay>,
}

impl Outcome {
    fn ended(state: SentState, reason: impl Into<String>) -> Self {
        Self {
            state,
            reason: Some(reason.into()),
            host_state: None,
            answer: None,
            last_event_sequence: 0,
            delay: None,
        }
    }

    fn waiting(reason: &str, delay: Delay) -> Self {
        Self {
            state: SentState::Waiting,
            delay: Some(delay),
            ..Self::ended(SentState::Waiting, reason)
        }
    }
}

/// Maps the `result` of one exchange for the task `task_id`. The error
/// messages are the strings of `transport::a2a_error_message`.
#[must_use]
pub fn outcome(task_id: &str, result: Result<Vec<Frame>, TransportError>) -> Outcome {
    let Ok(frames) = result else {
        return Outcome::waiting(UNREACHABLE, Delay::Unreachable);
    };
    let last_event_sequence = crate::transport::response_last_event_sequence(&frames);
    if frames.last().is_some_and(|frame| frame.kind == FrameKind::Error) {
        let message = frames
            .last()
            .and_then(|frame| frame.payload.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the task failed");
        let mut outcome = match message {
            "node overloaded" => {
                Outcome::waiting("the queue of the agent is full", Delay::Busy)
            }
            "permission denied" => Outcome::ended(
                SentState::Failed,
                "the agent refused the task: permission denied",
            ),
            "task result has expired" => Outcome::ended(
                SentState::Failed,
                "the answer expired on the agent before this node read it",
            ),
            "operator review required" => Outcome::ended(
                SentState::Failed,
                "the owner of the agent must review the task",
            ),
            "replay rejected" => Outcome::ended(
                SentState::Failed,
                "the agent refused the task as a replay",
            ),
            other => Outcome::ended(SentState::Failed, other.to_owned()),
        };
        outcome.last_event_sequence = last_event_sequence;
        return outcome;
    }
    let Ok(status) = crate::transport::response_task_status(&frames) else {
        return Outcome::waiting(UNREACHABLE, Delay::Unreachable);
    };
    let answer = crate::direct::answer(task_id.to_owned(), &frames);
    let host_state = answer.as_ref().map(|answer| answer.state.clone());
    let mut outcome = match status {
        A2aTaskStatus::Completed => Outcome {
            state: SentState::Answered,
            reason: None,
            host_state: None,
            answer: Some(answer.map(|answer| answer.text).unwrap_or_default()),
            last_event_sequence,
            delay: None,
        },
        A2aTaskStatus::Submitted | A2aTaskStatus::Working => Outcome {
            state: SentState::Working,
            reason: None,
            host_state: None,
            answer: None,
            last_event_sequence,
            delay: Some(Delay::Working),
        },
        A2aTaskStatus::Failed => {
            Outcome::ended(SentState::Failed, "the agent failed the task")
        }
        A2aTaskStatus::Rejected => {
            Outcome::ended(SentState::Failed, "the agent rejected the task")
        }
        A2aTaskStatus::Canceled => Outcome::ended(
            SentState::Failed,
            "the owner of the agent cancelled the task",
        ),
        A2aTaskStatus::InputRequired
        | A2aTaskStatus::AuthRequired
        | A2aTaskStatus::Unspecified => Outcome::ended(
            SentState::Failed,
            "the agent asked for something that Aether cannot give",
        ),
    };
    outcome.host_state = host_state;
    outcome.last_event_sequence = last_event_sequence;
    outcome
}

/// One attempt for `task`: send it, then follow it while the agent works on
/// it and `window` is open. Records the result and returns the new state.
pub async fn attempt(
    data_dir: &Path,
    courier: &dyn Courier,
    task: &SentTask,
    window: Duration,
) -> Result<SentState, OutboxError> {
    let data_dir = data_dir.to_path_buf();
    let endpoint_id = task.endpoint_id.clone();
    let (store, bundle) = off_runtime(move || {
        let store =
            NodeStore::in_dir(&data_dir).map_err(|_| OutboxError::Unavailable)?;
        let bundle = crate::direct::contact_by_endpoint(&store, &endpoint_id).ok();
        Ok((store, bundle))
    })
    .await?;
    attempt_in(&Arc::new(Mutex::new(store)), courier, task, bundle, window).await
}

/// [`attempt`] with an open store and the bundle of the task's contact.
async fn attempt_in(
    store: &SharedStore,
    courier: &dyn Courier,
    task: &SentTask,
    bundle: Option<InvitationBundle>,
    window: Duration,
) -> Result<SentState, OutboxError> {
    let Some(bundle) = bundle else {
        let endpoint_id = task.endpoint_id.clone();
        return with_store(store, move |store| {
            store
                .end_sent_tasks_for_endpoint(&endpoint_id, ACCESS_ENDED, now())
                .map_err(|_| OutboxError::Unavailable)?;
            Ok(SentState::NotDelivered)
        })
        .await;
    };
    let outcome = follow(courier, &bundle, task, window).await?;
    let task_id = task.task_id.clone();
    let lease_until = task.lease_until;
    let attempts = task.attempts;
    with_store(store, move |store| {
        let now = now();
        let delay =
            outcome.delay.map_or(Duration::ZERO, |delay| next_delay(delay, attempts));
        store
            .record_sent_attempt(
                &task_id,
                lease_until,
                &SentAttempt {
                    state: outcome.state,
                    reason: outcome.reason,
                    host_state: outcome.host_state,
                    answer: outcome.answer,
                    last_event_sequence: outcome.last_event_sequence,
                    next_attempt_at: now.saturating_add(seconds(delay)),
                },
                now,
            )
            .map_err(|_| OutboxError::Unavailable)?;
        Ok(outcome.state)
    })
    .await
}

/// Sends `task` to the agent of `bundle`, then follows it while the agent
/// works on it and `window` is open.
async fn follow(
    courier: &dyn Courier,
    bundle: &InvitationBundle,
    task: &SentTask,
    window: Duration,
) -> Result<Outcome, OutboxError> {
    let authorization: A2aAuthorization =
        serde_json::from_str(&task.authorization_json)
            .map_err(|_| OutboxError::Unavailable)?;
    let ids = RequestIds {
        task_id: task.task_id.clone(),
        idempotency_key: task.idempotency_key.clone(),
    };
    let started = std::time::Instant::now();
    let mut after = task.last_event_sequence;
    let mut link = Link::default();
    let mut replayed = false;
    loop {
        let frame = crate::direct::resend_frame(
            authorization.clone(),
            &task.text,
            ids.clone(),
            after,
        )
        .map_err(|_| OutboxError::Unavailable)?;
        let result = courier.exchange(bundle, &mut link, frame).await;
        // The link sends a frame again after a kept connection breaks. When
        // the host admitted the first copy, it refuses the second as a
        // replay: ask once more with a new frame. A second replay is real.
        if !replayed && is_replay(&result) {
            replayed = true;
            continue;
        }
        let outcome = outcome(&task.task_id, result);
        after = after.max(outcome.last_event_sequence);
        if outcome.state != SentState::Working || started.elapsed() >= window {
            return Ok(Outcome { last_event_sequence: after, ..outcome });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether the host refused the frame of `result` as a replay.
fn is_replay(result: &Result<Vec<Frame>, TransportError>) -> bool {
    result.as_ref().is_ok_and(|frames| {
        frames.last().is_some_and(|frame| {
            frame.kind == FrameKind::Error
                && frame.payload.get("message").and_then(serde_json::Value::as_str)
                    == Some("replay rejected")
        })
    })
}

/// The node store of one round, shared by its attempts.
type SharedStore = Arc<Mutex<NodeStore>>;

async fn off_runtime<R: Send + 'static>(
    work: impl FnOnce() -> Result<R, OutboxError> + Send + 'static,
) -> Result<R, OutboxError> {
    tokio::task::spawn_blocking(work).await.map_err(|_| OutboxError::Unavailable)?
}

async fn with_store<R: Send + 'static>(
    store: &SharedStore,
    work: impl FnOnce(&mut NodeStore) -> Result<R, OutboxError> + Send + 'static,
) -> Result<R, OutboxError> {
    let store = Arc::clone(store);
    off_runtime(move || work(&mut store.lock().unwrap_or_else(PoisonError::into_inner)))
        .await
}

/// What one round of the outbox did.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct RoundReport {
    /// The tasks that this round tried, with their new state.
    pub attempted: Vec<(String, SentState)>,
    /// Whether tasks still wait or work after the round.
    pub active: bool,
}

/// One round of the outbox at Unix time `now`.
///
/// It ends the tasks past their deadline or without access, wakes the agents
/// that `presence` shows online, waits for the ones that it shows offline,
/// and tries the due tasks. A busy database waits for the next round.
pub async fn run_once(
    data_dir: &Path,
    courier: Arc<dyn Courier>,
    presence: Option<&BTreeMap<String, bool>>,
    now: i64,
) -> Result<RoundReport, OutboxError> {
    let data_dir = data_dir.to_path_buf();
    let presence = presence.cloned();
    let Round { store, contacts, claimed } =
        off_runtime(move || start_round(&data_dir, presence.as_ref(), now)).await?;
    let store: SharedStore = Arc::new(Mutex::new(store));
    let mut attempts = tokio::task::JoinSet::new();
    for task in claimed {
        let courier = Arc::clone(&courier);
        let store = Arc::clone(&store);
        let bundle = contacts.get(&task.endpoint_id).cloned();
        attempts.spawn(async move {
            let state =
                attempt_in(&store, courier.as_ref(), &task, bundle, ATTEMPT_WINDOW)
                    .await;
            (task.task_id, state)
        });
    }
    let mut report = RoundReport::default();
    while let Some(joined) = attempts.join_next().await {
        if let Ok((task_id, Ok(state))) = joined {
            report.attempted.push((task_id, state));
        }
    }
    report.active = with_store(&store, move |store| {
        purge_hourly(store, now);
        store.has_active_sent_tasks().map_err(|_| OutboxError::Unavailable)
    })
    .await?;
    Ok(report)
}

/// A round after its store part, before its attempts.
struct Round {
    store: NodeStore,
    /// The bundle of each contact, by endpoint.
    contacts: BTreeMap<String, InvitationBundle>,
    claimed: Vec<SentTask>,
}

/// The store part of a round before its attempts: it ends, wakes and defers
/// tasks, and claims the due ones.
fn start_round(
    data_dir: &Path,
    presence: Option<&BTreeMap<String, bool>>,
    now: i64,
) -> Result<Round, OutboxError> {
    let mut store =
        NodeStore::in_dir(data_dir).map_err(|_| OutboxError::Unavailable)?;
    let busy = |_: StoreError| OutboxError::Unavailable;
    store.expire_sent_tasks(now, EXPIRED).map_err(busy)?;
    let active = store.active_sent_tasks().map_err(busy)?;
    // A contact that cannot be read counts as gone, as each lookup did.
    let contacts = crate::direct::contacts_by_endpoint(&store).unwrap_or_default();
    let mut endpoints: BTreeMap<String, Vec<&SentTask>> = BTreeMap::new();
    for task in &active {
        endpoints.entry(task.endpoint_id.clone()).or_default().push(task);
    }
    for (endpoint, tasks) in &endpoints {
        if !contacts.contains_key(endpoint) {
            store
                .end_sent_tasks_for_endpoint(endpoint, ACCESS_ENDED, now)
                .map_err(busy)?;
            continue;
        }
        match presence.and_then(|presence| presence.get(endpoint)) {
            Some(true) => {
                store.wake_sent_tasks(endpoint, now).map_err(busy)?;
            }
            // An offline agent is tried once, then every OFFLINE_PROBE.
            Some(false) if tasks.iter().all(|task| task.attempts > 0) => {
                let last =
                    tasks.iter().map(|task| task.updated_at).max().unwrap_or(now);
                let probe = last.saturating_add(seconds(OFFLINE_PROBE));
                if now < probe {
                    store.defer_sent_tasks(endpoint, probe).map_err(busy)?;
                }
            }
            _ => {}
        }
    }
    let claimed = store
        .claim_due_sent_tasks(now, now.saturating_add(seconds(LEASE)), MAX_PARALLEL)
        .map_err(busy)?;
    Ok(Round { store, contacts, claimed })
}

/// Deletes the tasks that ended more than [`RETENTION`] ago, at most once an
/// hour.
fn purge_hourly(store: &NodeStore, now: i64) {
    let last = store
        .configuration(PURGE_KEY)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < 3_600 {
        return;
    }
    if store.purge_sent_tasks(now.saturating_sub(seconds(RETENTION))).is_ok() {
        let _ = store.put_configuration(PURGE_KEY, &now.to_string());
    }
}

/// Runs the outbox until the task is aborted.
///
/// `wake` starts a round at once; else a round runs each second while tasks
/// wait, and each 30 s otherwise.
/// While tasks wait, the node reads the presence of its contacts from its
/// hub every [`PRESENCE_INTERVAL`]: the flag `hub_flag`, else the hub set on
/// the page.
pub async fn run(
    data_dir: PathBuf,
    courier: Arc<dyn Courier>,
    hub_flag: Option<String>,
    wake: Arc<tokio::sync::Notify>,
) {
    let hubs = crate::hub_client::NodeHub::new(data_dir.clone(), hub_flag.clone());
    let mut presence: Option<BTreeMap<String, bool>> = None;
    let mut presence_read: Option<std::time::Instant> = None;
    let mut active = true;
    loop {
        if active
            && presence_read.is_none_or(|read| read.elapsed() >= PRESENCE_INTERVAL)
        {
            if let Ok(Some(hub)) = hubs.client() {
                presence = Some(hub.presence(Duration::from_secs(4)).await);
            }
            presence_read = Some(std::time::Instant::now());
        }
        match run_once(&data_dir, Arc::clone(&courier), presence.as_ref(), now()).await
        {
            Ok(report) => active = report.active,
            Err(_) => active = true,
        }
        let pause = if active { POLL_INTERVAL } else { Duration::from_secs(30) };
        tokio::select! {
            () = wake.notified() => {}
            () = tokio::time::sleep(pause) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use aether_store::{NodeStore, SentState};

    use super::{
        Courier, Delay, OutboxError, attempt, enqueue, next_delay, now, outcome,
        run_once,
    };
    use crate::{
        direct::InvitationBundle,
        transport::{
            A2aRequest, A2aTaskStatus, Frame, FrameKind, Link, TransportError,
        },
    };

    /// A courier that gives its answers in order, and keeps each request.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<VecDeque<Result<Vec<Frame>, TransportError>>>,
        requests: Mutex<Vec<Frame>>,
    }

    impl Scripted {
        fn with(answers: Vec<Result<Vec<Frame>, TransportError>>) -> Arc<Self> {
            Arc::new(Self { answers: Mutex::new(answers.into()), ..Self::default() })
        }

        fn requests(&self) -> Vec<Frame> {
            self.requests.lock().expect("requests must lock").clone()
        }
    }

    impl Courier for Scripted {
        fn exchange<'a>(
            &'a self,
            _bundle: &'a InvitationBundle,
            _link: &'a mut Link,
            request: Frame,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<Frame>, TransportError>> + Send + 'a>>
        {
            self.requests.lock().expect("requests must lock").push(request);
            let answer = self
                .answers
                .lock()
                .expect("answers must lock")
                .pop_front()
                .unwrap_or(Err(TransportError::Timeout));
            Box::pin(async move { answer })
        }
    }

    #[expect(clippy::unnecessary_wraps, reason = "a scripted answer is a Result")]
    fn error(message: &str) -> Result<Vec<Frame>, TransportError> {
        Ok(vec![Frame {
            version: 0,
            kind: FrameKind::Error,
            exchange_id: "e".to_owned(),
            scope_id: "direct".to_owned(),
            task_id: None,
            payload: serde_json::json!({ "code": -32600, "message": message }),
        }])
    }

    #[expect(clippy::unnecessary_wraps, reason = "a scripted answer is a Result")]
    fn result(status: A2aTaskStatus, text: &str) -> Result<Vec<Frame>, TransportError> {
        Ok(vec![Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: "e".to_owned(),
            scope_id: "direct".to_owned(),
            task_id: Some("00000000-0000-4000-8000-000000000001".to_owned()),
            payload: crate::transport::text_result_payload(
                "00000000-0000-4000-8000-000000000001",
                status,
                text,
            ),
        }])
    }

    /// A caller data directory with the contact `tudor`.
    fn caller() -> (tempfile::TempDir, tempfile::TempDir) {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = crate::direct::endpoint_id(caller.path()).expect("endpoint");
        crate::direct::import_bundle(
            caller.path(),
            &crate::direct::test_bundle(host.path(), &endpoint, false),
        )
        .expect("the contact must import");
        (host, caller)
    }

    #[test]
    fn outcome_maps_each_host_answer() {
        let task = "00000000-0000-4000-8000-000000000001";
        let busy = outcome(task, error("node overloaded"));
        assert_eq!((busy.state, busy.delay), (SentState::Waiting, Some(Delay::Busy)));
        for message in [
            "permission denied",
            "task result has expired",
            "operator review required",
            "replay rejected",
            "something else",
        ] {
            assert_eq!(
                outcome(task, error(message)).state,
                SentState::Failed,
                "{message}"
            );
        }
        let unreachable = outcome(task, Err(TransportError::Timeout));
        assert_eq!(
            (unreachable.state, unreachable.delay),
            (SentState::Waiting, Some(Delay::Unreachable))
        );
        let answered = outcome(task, result(A2aTaskStatus::Completed, "Three names"));
        assert_eq!(answered.state, SentState::Answered);
        assert_eq!(answered.answer.as_deref(), Some("Three names"));
        let working = outcome(task, result(A2aTaskStatus::Working, ""));
        assert_eq!(
            (working.state, working.delay),
            (SentState::Working, Some(Delay::Working))
        );
        assert_eq!(
            outcome(task, result(A2aTaskStatus::Failed, "")).state,
            SentState::Failed
        );
        assert_eq!(
            outcome(task, result(A2aTaskStatus::Rejected, "")).state,
            SentState::Failed
        );
    }

    #[test]
    fn next_delay_grows_and_stops_at_its_cap() {
        assert_eq!(next_delay(Delay::Busy, 0), Duration::from_secs(5));
        assert_eq!(next_delay(Delay::Busy, 1), Duration::from_secs(10));
        assert_eq!(next_delay(Delay::Busy, 30), Duration::from_secs(300));
        assert_eq!(next_delay(Delay::Unreachable, 0), Duration::from_secs(30));
        assert_eq!(next_delay(Delay::Unreachable, 2), Duration::from_secs(120));
        assert_eq!(next_delay(Delay::Unreachable, 99), super::OFFLINE_PROBE);
        assert_eq!(next_delay(Delay::Working, 99), Duration::from_secs(5));
    }

    #[test]
    fn enqueue_refuses_an_unknown_expired_or_relayless_contact() {
        let (host, caller) = caller();
        let now = now();
        assert_eq!(
            enqueue(caller.path(), "nobody", "x", now).err(),
            Some(OutboxError::UnknownContact)
        );
        assert_eq!(
            enqueue(caller.path(), "tudor", "x", now.saturating_add(3_600)).err(),
            Some(OutboxError::Expired)
        );
        let task = enqueue(caller.path(), "tudor", "Give me three names", now)
            .expect("enqueue");
        assert_eq!(task.state, SentState::Waiting);
        assert!(task.expires_at <= now.saturating_add(60));

        let relayless = tempfile::tempdir().expect("directory must create");
        let endpoint = crate::direct::endpoint_id(relayless.path()).expect("endpoint");
        let mut bundle = crate::direct::test_bundle(host.path(), &endpoint, false);
        bundle.relay_url = None;
        crate::direct::import_bundle(relayless.path(), &bundle).expect("import");
        assert_eq!(
            enqueue(relayless.path(), "tudor", "x", now).err(),
            Some(OutboxError::NoRelay)
        );
    }

    #[tokio::test]
    async fn every_attempt_reuses_the_task_id_and_idempotency_key() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![
            result(A2aTaskStatus::Working, ""),
            result(A2aTaskStatus::Completed, "done"),
        ]);
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(5))
                .await
                .expect("attempt");
        assert_eq!(state, SentState::Answered);
        let requests = courier.requests();
        assert_eq!(requests.len(), 2);
        let payloads: Vec<A2aRequest> = requests
            .iter()
            .map(|frame| {
                serde_json::from_value(frame.payload.clone()).expect("payload")
            })
            .collect();
        assert!(
            requests
                .iter()
                .all(|frame| frame.task_id.as_deref() == Some(task.task_id.as_str()))
        );
        assert!(
            payloads
                .iter()
                .all(|payload| payload.idempotency_key == task.idempotency_key)
        );
        assert_ne!(payloads[0].request_nonce, payloads[1].request_nonce);
        let kept = NodeStore::in_dir(caller.path())
            .expect("store")
            .sent_task(&task.task_id)
            .expect("read")
            .expect("the task exists");
        assert_eq!(kept.answer.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn a_replay_of_an_admitted_frame_asks_again_with_a_new_frame() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![
            error("replay rejected"),
            result(A2aTaskStatus::Completed, "done"),
        ]);
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(5))
                .await
                .expect("attempt");
        assert_eq!(state, SentState::Answered);
        let nonces: Vec<String> = courier
            .requests()
            .iter()
            .map(|frame| {
                serde_json::from_value::<A2aRequest>(frame.payload.clone())
                    .expect("payload")
                    .request_nonce
            })
            .collect();
        assert_eq!(nonces.len(), 2);
        assert_ne!(nonces[0], nonces[1]);
    }

    #[tokio::test]
    async fn a_second_replay_fails_the_task() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier =
            Scripted::with(vec![error("replay rejected"), error("replay rejected")]);
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(5))
                .await
                .expect("attempt");
        assert_eq!(state, SentState::Failed);
        assert_eq!(courier.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_full_host_queue_is_tried_again_after_the_backoff() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![
            error("node overloaded"),
            result(A2aTaskStatus::Completed, "done"),
        ]);
        let first =
            run_once(caller.path(), courier.clone(), None, now()).await.expect("round");
        assert_eq!(first.attempted, [(task.task_id.clone(), SentState::Waiting)]);
        let store = NodeStore::in_dir(caller.path()).expect("store");
        let waiting = store.sent_task(&task.task_id).expect("read").expect("exists");
        assert!(waiting.next_attempt_at >= now().saturating_add(4));
        let early =
            run_once(caller.path(), courier.clone(), None, now()).await.expect("round");
        assert!(early.attempted.is_empty());
        let later =
            run_once(caller.path(), courier.clone(), None, waiting.next_attempt_at)
                .await
                .expect("round");
        assert_eq!(later.attempted, [(task.task_id, SentState::Answered)]);
        assert!(!later.active);
    }

    #[tokio::test]
    async fn an_offline_contact_waits_until_presence_says_online() {
        let (host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![
            Err(TransportError::Timeout),
            result(A2aTaskStatus::Completed, "done"),
        ]);
        let host_endpoint = crate::direct::endpoint_id(host.path()).expect("endpoint");
        let offline = BTreeMap::from([(host_endpoint.clone(), false)]);
        let online = BTreeMap::from([(host_endpoint, true)]);
        // A new task is tried once, also to an offline agent.
        let first = run_once(caller.path(), courier.clone(), Some(&offline), now())
            .await
            .expect("round");
        assert_eq!(first.attempted, [(task.task_id.clone(), SentState::Waiting)]);
        let store = NodeStore::in_dir(caller.path()).expect("store");
        let due = store
            .sent_task(&task.task_id)
            .expect("read")
            .expect("exists")
            .next_attempt_at;
        let still_offline =
            run_once(caller.path(), courier.clone(), Some(&offline), due)
                .await
                .expect("round");
        assert!(still_offline.attempted.is_empty());
        let woken = run_once(caller.path(), courier.clone(), Some(&online), due)
            .await
            .expect("round");
        assert_eq!(woken.attempted, [(task.task_id, SentState::Answered)]);
    }

    #[tokio::test]
    async fn a_task_ends_as_not_delivered_after_seven_days_or_when_access_ends() {
        let (_host, caller) = caller();
        let now = now();
        let expired = enqueue(caller.path(), "tudor", "late", now).expect("enqueue");
        let courier = Scripted::with(Vec::new());
        run_once(caller.path(), courier.clone(), None, expired.expires_at)
            .await
            .expect("round");
        let store = NodeStore::in_dir(caller.path()).expect("store");
        let ended = store.sent_task(&expired.task_id).expect("read").expect("exists");
        assert_eq!(ended.state, SentState::NotDelivered);

        let gone = enqueue(caller.path(), "tudor", "no access", now).expect("enqueue");
        crate::direct::remove_contact(caller.path(), "tudor").expect("remove");
        run_once(caller.path(), courier, None, now).await.expect("round");
        let ended = store.sent_task(&gone.task_id).expect("read").expect("exists");
        assert_eq!(ended.state, SentState::NotDelivered);
        assert_eq!(ended.reason.as_deref(), Some(super::ACCESS_ENDED));
    }
}
