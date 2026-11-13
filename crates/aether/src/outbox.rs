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
    settings::KeptStore,
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
/// The longest pause between rounds while rounds fail.
const ERROR_PAUSE_CAP: Duration = Duration::from_secs(30);

/// The pause before the next attempt after the rate limit of the agent shed
/// the requests of an attempt. It is short and does not grow: the limit
/// refills within a second, and the task is not stuck, as one behind a full
/// queue is.
const RATE_LIMITED_PAUSE: Duration = Duration::from_secs(2);

const UNREACHABLE: &str = "the agent cannot be reached; it may be offline";
const RATE_LIMITED: &str = "the agent is busy with other requests from this node";
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
    /// The per-peer rate limit of the agent shed the requests of this node,
    /// also after `follow` sent them again for a while. The agent has the
    /// task, or room for it: the limit refills within a second.
    RateLimited,
    /// The agent cannot be reached.
    Unreachable,
}

/// The time before the next attempt after `attempts` attempts, for `delay`.
#[must_use]
pub fn next_delay(delay: Delay, attempts: u64) -> Duration {
    let (first, cap) = match delay {
        Delay::Working => return Duration::from_secs(5),
        Delay::RateLimited => return RATE_LIMITED_PAUSE,
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
    // The rate limit sends the same message as a full queue, with
    // `retryAfterMs` in its `data`. It admitted nothing, so the task is
    // neither refused nor stuck behind a queue: ask again soon.
    if crate::transport::is_rate_limited(&frames) {
        return Outcome::waiting(RATE_LIMITED, Delay::RateLimited);
    }
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
    let store = open_shared(data_dir);
    let endpoint_id = task.endpoint_id.clone();
    let bundle = with_store(&store, move |store| {
        Ok(crate::direct::contact_by_endpoint(store, &endpoint_id).ok())
    })
    .await?;
    attempt_in(&store, courier, task, bundle, window).await
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
    let mut shed_since: Option<std::time::Instant> = None;
    let mut shed_retries: u32 = 0;
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
        // The per-peer rate limit of the agent shed the frame before the
        // agent saw it, also in the middle of a poll. Send it again a little
        // later with a new frame, as `request_with_polling_on` does, but
        // never past the attempt window. A full queue is not retried: it is
        // the `Busy` delay of `outcome`.
        if result.as_ref().is_ok_and(|frames| crate::transport::is_rate_limited(frames))
        {
            let since = *shed_since.get_or_insert_with(std::time::Instant::now);
            if let Some(wait) = crate::transport::overload_retry_wait(
                shed_retries,
                since.elapsed(),
                window.saturating_sub(started.elapsed()),
            ) {
                tokio::time::sleep(wait).await;
                shed_retries = shed_retries.saturating_add(1);
                continue;
            }
        } else {
            shed_since = None;
            shed_retries = 0;
        }
        // An ended status is not enough to stop: the host cut the page
        // before the last event when it says that more events remain.
        let more_events = result
            .as_ref()
            .is_ok_and(|frames| crate::transport::response_more_events_remain(frames));
        let outcome = outcome(&task.task_id, result);
        after = after.max(outcome.last_event_sequence);
        let working = outcome.state == SentState::Working;
        if !working && !more_events {
            return Ok(Outcome { last_event_sequence: after, ..outcome });
        }
        if started.elapsed() >= window {
            return Ok(still_working(outcome, after));
        }
        if working {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The outcome of an attempt whose window closed before the last page of
/// events: the task stays active, and the next attempt reads on from `after`.
fn still_working(outcome: Outcome, after: u64) -> Outcome {
    Outcome {
        state: SentState::Working,
        reason: None,
        host_state: outcome.host_state,
        answer: None,
        last_event_sequence: after,
        delay: Some(Delay::Working),
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

/// The node store of a round and its attempts. It stays open between rounds
/// and opens again when the database file is replaced or migrated by another
/// binary (see [`KeptStore`]), so `enqueue` and the loop use the same file.
type SharedStore = Arc<Mutex<KeptStore>>;

async fn off_runtime<R: Send + 'static>(
    work: impl FnOnce() -> Result<R, OutboxError> + Send + 'static,
) -> Result<R, OutboxError> {
    tokio::task::spawn_blocking(work).await.map_err(|_| OutboxError::Unavailable)?
}

/// Runs `work` on the current store, off the runtime. The work runs once:
/// a store that fails to open is [`OutboxError::Unavailable`].
async fn with_store<R: Send + 'static>(
    store: &SharedStore,
    work: impl FnOnce(&mut NodeStore) -> Result<R, OutboxError> + Send + 'static,
) -> Result<R, OutboxError> {
    let store = Arc::clone(store);
    off_runtime(move || {
        let mut kept = store.lock().unwrap_or_else(PoisonError::into_inner);
        work(kept.current().map_err(|_| OutboxError::Unavailable)?)
    })
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
    let store = open_shared(data_dir);
    run_round(&store, courier, presence, now).await
}

/// The node store of `data_dir`, for rounds to share. It opens on first use.
fn open_shared(data_dir: &Path) -> SharedStore {
    Arc::new(Mutex::new(KeptStore::new(data_dir.to_path_buf(), false)))
}

/// One round on an open `store`; [`run`] keeps one store for all its rounds.
async fn run_round(
    store: &SharedStore,
    courier: Arc<dyn Courier>,
    presence: Option<&BTreeMap<String, bool>>,
    now: i64,
) -> Result<RoundReport, OutboxError> {
    let presence = presence.cloned();
    let Round { contacts, claimed } =
        with_store(store, move |store| start_round(store, presence.as_ref(), now))
            .await?;
    let mut attempts = tokio::task::JoinSet::new();
    for task in claimed {
        let courier = Arc::clone(&courier);
        let store = Arc::clone(store);
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
    report.active = with_store(store, move |store| {
        purge_hourly(store, now);
        store.has_active_sent_tasks().map_err(|_| OutboxError::Unavailable)
    })
    .await?;
    Ok(report)
}

/// A round after its store part, before its attempts.
struct Round {
    /// The bundle of each contact, by endpoint.
    contacts: BTreeMap<String, InvitationBundle>,
    claimed: Vec<SentTask>,
}

/// The store part of a round before its attempts: it ends, wakes and defers
/// tasks, and claims the due ones.
fn start_round(
    store: &mut NodeStore,
    presence: Option<&BTreeMap<String, bool>>,
    now: i64,
) -> Result<Round, OutboxError> {
    let busy = |_: StoreError| OutboxError::Unavailable;
    store.expire_sent_tasks(now, EXPIRED).map_err(busy)?;
    let active = store.active_sent_tasks().map_err(busy)?;
    // A contact that cannot be read counts as gone, as each lookup did.
    let contacts = crate::direct::contacts_by_endpoint(store).unwrap_or_default();
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
    Ok(Round { contacts, claimed })
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

/// The pause after `failures` rounds in a row that failed: [`POLL_INTERVAL`]
/// doubled for each failure after the first, up to [`ERROR_PAUSE_CAP`].
fn error_pause(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    let factor = 1_u32.checked_shl(doublings).unwrap_or(1);
    POLL_INTERVAL.saturating_mul(factor).min(ERROR_PAUSE_CAP)
}

/// One round of [`run`] on the `store` that all its rounds share.
async fn next_round(
    store: &SharedStore,
    courier: &Arc<dyn Courier>,
    presence: Option<&BTreeMap<String, bool>>,
) -> Result<RoundReport, OutboxError> {
    run_round(store, Arc::clone(courier), presence, now()).await
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
    // The store stays open between rounds. It opens the file again when the
    // path names another file or another binary migrated it, and a failed
    // round makes it open again too.
    let store = open_shared(&data_dir);
    let mut failures: u32 = 0;
    loop {
        if active
            && presence_read.is_none_or(|read| read.elapsed() >= PRESENCE_INTERVAL)
        {
            presence = match hubs.client() {
                Ok(Some(hub)) => match hub.presence(Duration::from_secs(4)).await {
                    Ok(map) => Some(map),
                    Err(error) => {
                        tracing::warn!(?error, "hub presence refresh failed");
                        None
                    }
                },
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(?error, "hub client unavailable for presence");
                    None
                }
            };
            presence_read = Some(std::time::Instant::now());
        }
        let round = next_round(&store, &courier, presence.as_ref()).await;
        if let Ok(report) = round {
            active = report.active;
            failures = 0;
        } else {
            active = true;
            failures = failures.saturating_add(1);
            store.lock().unwrap_or_else(PoisonError::into_inner).close();
        }
        let pause = if failures > 0 {
            error_pause(failures)
        } else if active {
            POLL_INTERVAL
        } else {
            Duration::from_secs(30)
        };
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

    #[test]
    fn failed_rounds_back_off_to_a_cap_and_a_good_round_resets_the_pause() {
        let pauses: Vec<u64> =
            (1..=8).map(|failures| super::error_pause(failures).as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(super::error_pause(u32::MAX), super::ERROR_PAUSE_CAP);
        assert_eq!(super::error_pause(0), super::POLL_INTERVAL);
    }

    #[test]
    fn response_linger_outlives_the_poll_interval() {
        assert!(
            crate::transport::RESPONSE_LINGER > super::POLL_INTERVAL,
            "polling clients must keep connections between outbox polls"
        );
    }

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

    /// The "node overloaded" answer of the host's per-peer rate limit.
    #[expect(clippy::unnecessary_wraps, reason = "a scripted answer is a Result")]
    fn rate_limited() -> Result<Vec<Frame>, TransportError> {
        let request = Frame {
            version: 0,
            kind: FrameKind::Request,
            exchange_id: "e".to_owned(),
            scope_id: "direct".to_owned(),
            task_id: None,
            payload: serde_json::json!({}),
        };
        Ok(crate::transport::rate_limited_response(&[request]))
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
        // The rate limit has the same message, and its own reason and delay.
        let limited = outcome(task, rate_limited());
        assert_eq!(
            (limited.state, limited.delay),
            (SentState::Waiting, Some(Delay::RateLimited))
        );
        assert_eq!(limited.reason.as_deref(), Some(super::RATE_LIMITED));
        assert_eq!(busy.reason.as_deref(), Some("the queue of the agent is full"));
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
        // A rate-limited task waits briefly and the wait does not grow.
        assert_eq!(next_delay(Delay::RateLimited, 0), super::RATE_LIMITED_PAUSE);
        assert_eq!(next_delay(Delay::RateLimited, 99), super::RATE_LIMITED_PAUSE);
        assert!(super::RATE_LIMITED_PAUSE < next_delay(Delay::Busy, 0));
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

    /// One page of an ended task: the cursor, and whether events remain.
    #[expect(clippy::unnecessary_wraps, reason = "a scripted answer is a Result")]
    fn page(cursor: i64, more_events: bool) -> Result<Vec<Frame>, TransportError> {
        let task = "00000000-0000-4000-8000-000000000001";
        Ok(vec![Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: "e".to_owned(),
            scope_id: "direct".to_owned(),
            task_id: Some(task.to_owned()),
            payload: crate::transport::text_result_payload_with_progress(
                task,
                A2aTaskStatus::Completed,
                "done",
                cursor,
                more_events,
            ),
        }])
    }

    #[tokio::test]
    async fn an_ended_task_is_read_to_its_last_event() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![page(2, true), page(4, false)]);
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(5))
                .await
                .expect("attempt");
        assert_eq!(state, SentState::Answered);
        let cursors: Vec<Option<u64>> = courier
            .requests()
            .iter()
            .map(|frame| {
                serde_json::from_value::<A2aRequest>(frame.payload.clone())
                    .expect("payload")
                    .last_event_sequence
            })
            .collect();
        assert_eq!(cursors, [Some(0), Some(2)]);
        let kept = NodeStore::in_dir(caller.path())
            .expect("store")
            .sent_task(&task.task_id)
            .expect("read")
            .expect("the task exists");
        assert_eq!(kept.last_event_sequence, 4);
        assert_eq!(kept.answer.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn a_closed_window_keeps_a_task_with_more_events_active() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![page(2, true)]);
        let state = attempt(caller.path(), courier.as_ref(), &task, Duration::ZERO)
            .await
            .expect("attempt");
        assert_eq!(state, SentState::Working);
        let kept = NodeStore::in_dir(caller.path())
            .expect("store")
            .sent_task(&task.task_id)
            .expect("read")
            .expect("the task exists");
        assert_eq!(kept.last_event_sequence, 2);
        assert_eq!(kept.answer, None);
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
        // A full queue is not sent again within the attempt.
        assert_eq!(courier.requests().len(), 1);
        let store = NodeStore::in_dir(caller.path()).expect("store");
        let waiting = store.sent_task(&task.task_id).expect("read").expect("exists");
        assert!(waiting.next_attempt_at >= now().saturating_add(4));
        assert_eq!(waiting.reason.as_deref(), Some("the queue of the agent is full"));
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
    async fn a_poll_that_the_rate_limit_shed_is_sent_again_in_the_attempt() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with(vec![
            result(A2aTaskStatus::Working, ""),
            rate_limited(),
            rate_limited(),
            result(A2aTaskStatus::Completed, "done"),
        ]);
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(10))
                .await
                .expect("attempt");
        assert_eq!(state, SentState::Answered);
        let payloads: Vec<A2aRequest> = courier
            .requests()
            .iter()
            .map(|frame| {
                serde_json::from_value(frame.payload.clone()).expect("payload")
            })
            .collect();
        assert_eq!(payloads.len(), 4);
        assert!(
            payloads
                .iter()
                .all(|payload| payload.idempotency_key == task.idempotency_key)
        );
        let nonces: std::collections::BTreeSet<&str> =
            payloads.iter().map(|payload| payload.request_nonce.as_str()).collect();
        assert_eq!(nonces.len(), 4, "every send is a new frame");
    }

    #[tokio::test]
    async fn an_agent_that_keeps_shedding_makes_the_task_wait_briefly_with_its_own_reason()
     {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with((0..40).map(|_| rate_limited()).collect());
        let started = std::time::Instant::now();
        let state =
            attempt(caller.path(), courier.as_ref(), &task, Duration::from_secs(20))
                .await
                .expect("attempt");
        let elapsed = started.elapsed();
        assert_eq!(state, SentState::Waiting);
        // The sends stop at the end of the overload window, not the attempt's.
        assert!(elapsed >= crate::transport::OVERLOAD_RETRY_WINDOW, "{elapsed:?}");
        assert!(
            elapsed
                < crate::transport::OVERLOAD_RETRY_WINDOW + Duration::from_millis(600),
            "{elapsed:?}"
        );
        assert!(courier.requests().len() > 2);
        let kept = NodeStore::in_dir(caller.path())
            .expect("store")
            .sent_task(&task.task_id)
            .expect("read")
            .expect("the task exists");
        assert_eq!(kept.reason.as_deref(), Some(super::RATE_LIMITED));
        let pause = kept.next_attempt_at.saturating_sub(now());
        assert!(
            (0..=super::seconds(super::RATE_LIMITED_PAUSE)).contains(&pause),
            "{pause} s"
        );
    }

    #[tokio::test]
    async fn the_rate_limit_retry_stops_at_the_attempt_window() {
        let (_host, caller) = caller();
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let courier = Scripted::with((0..40).map(|_| rate_limited()).collect());
        let started = std::time::Instant::now();
        let state = attempt(caller.path(), courier.as_ref(), &task, Duration::ZERO)
            .await
            .expect("attempt");
        assert_eq!(state, SentState::Waiting);
        assert_eq!(courier.requests().len(), 1);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Changes `user_version` of the database in `directory` by `change`.
    fn bump_schema_version(directory: &std::path::Path, change: i64) {
        let connection =
            rusqlite::Connection::open(directory.join(aether_store::NODE_DATABASE))
                .expect("database must open");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version must read");
        connection
            .pragma_update(None, "user_version", version.saturating_add(change))
            .expect("version must write");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rounds_that_share_a_store_follow_a_database_file_that_replaced_the_old_one()
     {
        let (host, caller) = caller();
        let store = super::open_shared(caller.path());
        let courier = Scripted::with(vec![result(A2aTaskStatus::Completed, "done")]);
        let idle = super::run_round(&store, courier.clone(), None, now())
            .await
            .expect("round");
        assert!(idle.attempted.is_empty());
        // An operator restores a backup over the database while the node runs.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(
                caller.path().join(format!("{}{suffix}", aether_store::NODE_DATABASE)),
            );
        }
        let endpoint = crate::direct::endpoint_id(caller.path()).expect("endpoint");
        crate::direct::import_bundle(
            caller.path(),
            &crate::direct::test_bundle(host.path(), &endpoint, false),
        )
        .expect("the contact must import");
        let task = enqueue(caller.path(), "tudor", "work", now()).expect("enqueue");
        let round =
            super::run_round(&store, courier, None, now()).await.expect("round");
        assert_eq!(round.attempted, [(task.task_id, SentState::Answered)]);
    }

    #[tokio::test]
    async fn rounds_that_share_a_store_fail_closed_for_a_newer_schema() {
        let (_host, caller) = caller();
        let store = super::open_shared(caller.path());
        let courier = Scripted::with(Vec::new());
        super::run_round(&store, courier.clone(), None, now()).await.expect("round");
        // Another binary migrated the file while the loop kept its store.
        bump_schema_version(caller.path(), 1);
        assert_eq!(
            super::run_round(&store, courier.clone(), None, now()).await.err(),
            Some(OutboxError::Unavailable)
        );
        bump_schema_version(caller.path(), -1);
        super::run_round(&store, courier, None, now()).await.expect("round");
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
