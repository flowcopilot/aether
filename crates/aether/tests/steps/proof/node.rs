//! Steps for a proof node in this process: admission, one agent turn,
//! cancellation and the task event stream.

use std::{
    fmt, fs,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aether::{
    execution::{Agent, ExecutionError, Executor, Limits, Task, TurnContext},
    identity::SignedClaims,
    proof::{Identity, ProofError, ProofNode, SubmissionInput},
};
use aether_core::{
    Claim, Denial, EndpointId, PolicyEngine, Request, RequestId, TaskId,
};
use aether_store::{
    NodeStore, QueueLimits, StoreError, TaskAdmission as StoreAdmission, TaskState,
};
use cucumber::{given, then, when};
use tempfile::TempDir;
use tokio::{sync::broadcast, task::JoinHandle};

use super::fixtures::{
    CALLER_ENDPOINT, claim, expired_claim, grant, names, request, scope, task,
    task_input,
};
use crate::{common::blocking, steps::World};

type Node = ProofNode<SignedClaims, TestAgent>;
type Outcome = Result<(TaskState, Option<String>), ProofError>;

/// What the fake agent does in a turn.
#[derive(Clone, Copy, Debug)]
enum Behaviour {
    /// Answers at once with `<skill>:<remote text>`.
    Answers,
    /// Waits until it is cancelled. Then it stops, or it still returns a
    /// result after the cancel.
    WaitsForCancel { late_result: bool },
}

/// A fake agent that counts its turns and records a cancel.
#[derive(Clone, Debug)]
struct TestAgent {
    behaviour: Behaviour,
    turns: Arc<AtomicUsize>,
    started: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl TestAgent {
    fn new(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            turns: Arc::default(),
            started: Arc::default(),
            cancelled: Arc::default(),
        }
    }
}

impl Agent for TestAgent {
    fn start_turn(&mut self, ctx: &TurnContext) -> Result<String, ExecutionError> {
        self.turns.fetch_add(1, Ordering::Relaxed);
        self.started.store(true, Ordering::Release);
        let Behaviour::WaitsForCancel { late_result } = self.behaviour else {
            return Ok(format!("{}:{}", ctx.skill, ctx.text));
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.cancelled.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(ExecutionError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if late_result {
            return Ok("late result".to_owned());
        }
        Err(ExecutionError::Cancelled)
    }

    fn cancel(&self, _task_id: &str) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// What the node's store holds before the node starts.
#[derive(Clone, Copy)]
enum Seed {
    Nothing,
    /// Task `task` was running when its lease expired, so its result is
    /// unknown.
    LostResult,
    /// The queue of one task already holds a task from another caller.
    FullQueue,
}

impl Seed {
    fn apply(self, store: &mut NodeStore) {
        let limits = QueueLimits { global: 1, per_peer: 1 };
        match self {
            Self::Nothing => {}
            Self::LostResult => {
                store
                    .put_scope("direct", aether_core::ScopeKind::Direct)
                    .expect("scope must store");
                store
                    .admit(&task_input("task", "request", "key"), limits)
                    .expect("task must queue");
                store.lease_next("proof", "turn", 1).expect("task must lease");
                store.mark_running("task", "turn").expect("task must run");
                store.recover_expired_leases(1).expect("task must recover");
            }
            Self::FullQueue => {
                store
                    .put_scope("direct", aether_core::ScopeKind::Direct)
                    .expect("scope must store");
                let mut other = task_input("other-task", "other-request", "other-key");
                other.request.caller_agent_id =
                    aether_core::AgentId::new("other-caller");
                other.request.caller_endpoint_id =
                    aether_core::EndpointId::new("other-endpoint");
                "other-fingerprint".clone_into(&mut other.request_fingerprint);
                store.admit(&other, limits).expect("task must queue");
            }
        }
    }
}

#[derive(Default)]
pub struct State {
    root: Option<TempDir>,
    node: Option<Arc<Node>>,
    agent: Option<TestAgent>,
    /// The end of the last task the caller waited for.
    outcome: Option<Outcome>,
    /// The answer to a submission the caller did not wait for.
    admission: Option<Result<StoreAdmission, ProofError>>,
    /// A submission that runs while the scenario goes on.
    running: Option<JoinHandle<Outcome>>,
    subscriber: Option<broadcast::Receiver<String>>,
    last_task: Option<String>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("agent", &self.agent)
            .field("outcome", &self.outcome)
            .field("admission", &self.admission)
            .field("last_task", &self.last_task)
            .finish_non_exhaustive()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // Shutting a node down joins its threads. Do it away from the task
        // that polls the scenarios, and remove the files after it.
        let node = self.node.take();
        let root = self.root.take();
        if node.is_some() || root.is_some() {
            std::thread::spawn(move || {
                if let Some(node) = node {
                    node.shutdown();
                }
                drop(root);
            });
        }
    }
}

impl State {
    fn node(&self) -> Arc<Node> {
        Arc::clone(self.node.as_ref().expect("a proof node must run"))
    }

    const fn agent(&self) -> &TestAgent {
        self.agent.as_ref().expect("a proof node must run")
    }

    async fn with_node<R, F>(&self, work: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&Node) -> R + Send + 'static,
    {
        let node = self.node();
        blocking(move || work(&node)).await
    }

    async fn start(&mut self, grants: bool, behaviour: Behaviour, seed: Seed) {
        let agent = TestAgent::new(behaviour);
        let executor_agent = agent.clone();
        let (root, node) = blocking(move || {
            let root = tempfile::tempdir().expect("temporary directory must create");
            let workspace = root.path().join("workspace");
            fs::create_dir_all(&workspace).expect("workspace must exist");
            let mut store =
                NodeStore::open(root.path().join(aether_store::NODE_DATABASE))
                    .expect("store must open");
            seed.apply(&mut store);
            let scope = scope("direct");
            let grants = if grants { vec![grant(&scope)] } else { Vec::new() };
            let node = ProofNode::new(
                PolicyEngine::new(super::fixtures::trust_provider(), grants),
                store,
                Executor::new(
                    executor_agent,
                    Limits { workspace, max_context_tokens: 32, max_seconds: 10 },
                ),
                QueueLimits { global: 1, per_peer: 1 },
                scope,
            )
            .expect("proof node must create");
            (root, node)
        })
        .await;
        self.root = Some(root);
        self.node = Some(Arc::new(node));
        self.agent = Some(agent);
    }
}

/// Admits a task as an Iroh caller.
fn admit_task(
    node: &Node,
    request: &Request,
    claim: &Claim,
    idempotency_key: &str,
    task: &Task,
) -> Result<StoreAdmission, ProofError> {
    let input = SubmissionInput {
        request,
        authenticated_endpoint: &EndpointId::new(CALLER_ENDPOINT),
        idempotency_key,
        task,
    };
    node.admit(input, Identity::Claim(claim), None)
}

/// Admits a task as an Iroh caller and polls its state until the task ends.
fn submit_and_wait(
    node: &Node,
    request: &Request,
    claim: &Claim,
    idempotency_key: &str,
    task: &Task,
) -> Outcome {
    let (StoreAdmission::Accepted(task_id) | StoreAdmission::Existing(task_id)) =
        admit_task(node, request, claim, idempotency_key, task)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match node.state(&task_id)? {
            TaskState::PendingApproval
            | TaskState::Queued
            | TaskState::Leased
            | TaskState::Running => {
                // A task that never ends fails the scenario with this error.
                if Instant::now() >= deadline {
                    return Err(ProofError::Internal);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => return node.task_snapshot(&task_id),
        }
    }
}

fn submit_review(node: &Node, claim: &Claim) -> Outcome {
    submit_and_wait(node, &request(&scope("direct")), claim, "key", &task("task"))
}

fn event_types(events: Vec<aether_store::TaskEvent>) -> Vec<String> {
    events.into_iter().map(|event| event.event_type.as_str().to_owned()).collect()
}

#[given("a proof node that grants review to the caller")]
async fn node_with_grant(world: &mut World) {
    world.proof.node.start(true, Behaviour::Answers, Seed::Nothing).await;
}

#[given("a proof node with no grants")]
async fn node_without_grants(world: &mut World) {
    world.proof.node.start(false, Behaviour::Answers, Seed::Nothing).await;
}

#[given("a proof node whose agent waits until it is cancelled")]
async fn node_with_waiting_agent(world: &mut World) {
    let behaviour = Behaviour::WaitsForCancel { late_result: false };
    world.proof.node.start(true, behaviour, Seed::Nothing).await;
}

#[given("a proof node whose agent returns a late result after it is cancelled")]
async fn node_with_late_agent(world: &mut World) {
    let behaviour = Behaviour::WaitsForCancel { late_result: true };
    world.proof.node.start(true, behaviour, Seed::Nothing).await;
}

#[given("a proof node that lost the result of the caller's task")]
async fn node_with_lost_result(world: &mut World) {
    world.proof.node.start(true, Behaviour::Answers, Seed::LostResult).await;
}

#[given("a proof node whose queue is full with another caller's task")]
async fn node_with_full_queue(world: &mut World) {
    world.proof.node.start(true, Behaviour::Answers, Seed::FullQueue).await;
}

#[given("an event subscriber that reads no events")]
fn idle_subscriber(world: &mut World) {
    world.proof.node.subscriber = Some(world.proof.node.node().subscribe_events());
}

#[when("the caller sends a review task and waits for it to end")]
async fn submit_and_wait_for_task(world: &mut World) {
    let outcome = world
        .proof
        .node
        .with_node(|node| submit_review(node, &claim(&scope("direct"))))
        .await;
    world.proof.node.outcome = Some(outcome);
}

#[when("the caller sends a review task with an expired claim")]
async fn submit_with_expired_claim(world: &mut World) {
    let outcome = world
        .proof
        .node
        .with_node(|node| submit_review(node, &expired_claim(&scope("direct"))))
        .await;
    world.proof.node.outcome = Some(outcome);
}

#[when("the caller sends the same task again")]
async fn submit_again(world: &mut World) {
    let admission = world
        .proof
        .node
        .with_node(|node| {
            let scope = scope("direct");
            admit_task(node, &request(&scope), &claim(&scope), "key", &task("task"))
        })
        .await;
    world.proof.node.admission = Some(admission);
}

#[when("the caller sends a review task")]
fn submit_in_background(world: &mut World) {
    let node = world.proof.node.node();
    world.proof.node.running = Some(tokio::task::spawn_blocking(move || {
        submit_review(&node, &claim(&scope("direct")))
    }));
}

#[when("the agent turn starts")]
async fn agent_turn_starts(world: &mut World) {
    let started = Arc::clone(&world.proof.node.agent().started);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "the agent turn must start");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[when("the caller cancels the task")]
async fn caller_cancels(world: &mut World) {
    world
        .proof
        .node
        .with_node(|node| {
            let request = request(&scope("direct"));
            node.cancel_from(
                "task",
                &request.caller_endpoint_id,
                request.scope_id.as_str(),
            )
            .expect("caller must cancel the active task");
        })
        .await;
}

#[when(expr = "the caller sends {int} review tasks one after another")]
async fn submit_many(world: &mut World, count: usize) {
    let last_task = world
        .proof
        .node
        .with_node(move |node| {
            let scope = scope("direct");
            let claim = claim(&scope);
            let mut last_task_id = String::new();
            for index in 0..count {
                let task_id = format!("lag-task-{index}");
                let request = Request {
                    id: RequestId::new(format!("lag-request-{index}")),
                    task_id: TaskId::new(&task_id),
                    ..request(&scope)
                };
                assert_eq!(
                    submit_and_wait(
                        node,
                        &request,
                        &claim,
                        &format!("lag-key-{index}"),
                        &task(&task_id)
                    )
                    .expect("task must complete"),
                    (
                        TaskState::Succeeded,
                        Some("review:untrusted task text".to_owned())
                    )
                );
                last_task_id = task_id;
            }
            last_task_id
        })
        .await;
    world.proof.node.last_task = Some(last_task);
}

#[then(expr = "the task succeeds with the result {string}")]
fn task_succeeds(world: &mut World, result: String) {
    let outcome = world.proof.node.outcome.as_ref().expect("a task must end");
    assert!(
        matches!(outcome, Ok((TaskState::Succeeded, Some(text))) if *text == result),
        "{outcome:?}"
    );
}

#[then("the node returns the existing task")]
fn returns_existing(world: &mut World) {
    let admission = world.proof.node.admission.as_ref().expect("the caller must send");
    assert!(
        matches!(admission, Ok(StoreAdmission::Existing(id)) if id == "task"),
        "{admission:?}"
    );
}

#[then(expr = "the agent ran {int} turn(s)")]
fn agent_turns(world: &mut World, turns: usize) {
    assert_eq!(world.proof.node.agent().turns.load(Ordering::Relaxed), turns);
}

#[then(expr = "the task is {word}")]
async fn task_is(world: &mut World, state: String) {
    let expected = TaskState::parse(&state).expect("a known task state");
    let actual = world.proof.node.with_node(|node| node.state("task")).await;
    assert_eq!(actual.expect("task state must load"), expected);
}

#[then(expr = "the task events are {string}")]
async fn task_events(world: &mut World, expected: String) {
    let events = world.proof.node.with_node(|node| node.events("task")).await;
    assert_eq!(event_types(events.expect("task events must load")), names(&expected));
}

#[then(expr = "the task events become {string}")]
async fn task_events_become(world: &mut World, expected: String) {
    // The late result is written after the cancel, so wait for it.
    let deadline = Instant::now() + Duration::from_secs(2);
    let events = loop {
        let events = world.proof.node.with_node(|node| node.events("task")).await;
        let events = event_types(events.expect("task events must load"));
        if events.iter().any(|event| event == "result") || Instant::now() >= deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(events, names(&expected));
}

#[then("the task ends as cancelled with no result")]
async fn ends_cancelled(world: &mut World) {
    let running = world.proof.node.running.take().expect("the caller must send a task");
    let outcome = running.await.expect("submission must finish");
    assert!(matches!(outcome, Ok((TaskState::Cancelled, None))), "{outcome:?}");
}

#[then("the agent was asked to cancel")]
fn agent_cancelled(world: &mut World) {
    assert!(world.proof.node.agent().cancelled.load(Ordering::Acquire));
}

#[then(expr = "the node refuses the task because {string}")]
fn refuses(world: &mut World, reason: String) {
    let outcome = world.proof.node.outcome.as_ref().expect("a task must end");
    let refused = match reason.as_str() {
        "the claim expired" => {
            matches!(outcome, Err(ProofError::Denied(Denial::ClaimExpired)))
        }
        "no local grant allows it" => {
            matches!(outcome, Err(ProofError::Denied(Denial::LocalGrantMissing)))
        }
        "its earlier result is unknown" => {
            matches!(outcome, Err(ProofError::Store(StoreError::DuplicateConflict)))
        }
        "the queue is full" => {
            matches!(outcome, Err(ProofError::Store(StoreError::Overloaded)))
        }
        other => unreachable!("unknown reason {other}"),
    };
    assert!(refused, "expected a refusal because {reason}, got {outcome:?}");
}

#[then("the subscriber has fallen behind")]
fn subscriber_lags(world: &mut World) {
    let subscriber =
        world.proof.node.subscriber.as_mut().expect("a subscriber must exist");
    assert!(matches!(
        subscriber.try_recv(),
        Err(broadcast::error::TryRecvError::Lagged(_))
    ));
}

#[then("the last task succeeded")]
async fn last_task_succeeded(world: &mut World) {
    let last = world.proof.node.last_task.clone().expect("tasks must run");
    let state = world.proof.node.with_node(move |node| node.state(&last)).await;
    assert_eq!(state.expect("task state must load"), TaskState::Succeeded);
}

#[then(expr = "the events of the last task are {string}")]
async fn last_task_events(world: &mut World, expected: String) {
    let last = world.proof.node.last_task.clone().expect("tasks must run");
    let events =
        world.proof.node.with_node(move |node| node.events_after(&last, 0)).await;
    assert_eq!(event_types(events.expect("events must resume")), names(&expected));
}

#[then(expr = "the events of the last task after its second event are {string}")]
async fn last_task_events_after_second(world: &mut World, expected: String) {
    let last = world.proof.node.last_task.clone().expect("tasks must run");
    let resumed = world
        .proof
        .node
        .with_node(move |node| {
            let events = node.events_after(&last, 0).expect("events must resume");
            let second = events.get(1).expect("the task must have two events").sequence;
            node.events_after(&last, second)
        })
        .await;
    assert_eq!(
        event_types(resumed.expect("events must resume from the stored sequence")),
        names(&expected)
    );
}
