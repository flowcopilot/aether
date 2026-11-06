//! Steps for the node's task queue: duplicates, limits, cancel and leases.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use aether_store::{
    NodeStore, QueueLimits, StoreError, TaskAdmission as Admission, TaskInput,
    TaskState,
};
use cucumber::{given, then, when};
use tempfile::TempDir;

use super::fixtures::{blocking, names, task_input};
use crate::steps::World;

const LIMITS: QueueLimits = QueueLimits { global: 1, per_peer: 1 };

#[derive(Default)]
pub struct State {
    store: Option<(TempDir, Arc<Mutex<NodeStore>>)>,
    answer: Option<Result<Admission, StoreError>>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

impl State {
    async fn with_store<R, F>(&self, work: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&mut NodeStore) -> R + Send + 'static,
    {
        let (_, store) = self.store.as_ref().expect("a task store must be open");
        let store = Arc::clone(store);
        blocking(move || work(&mut store.lock().expect("store lock"))).await
    }
}

/// The caller's task `id`, with its own request and key.
fn input(id: &str) -> TaskInput {
    task_input(id, &format!("request-{id}"), &format!("key-{id}"))
}

#[given("a task store whose queue holds one task")]
async fn open_store(world: &mut World) {
    let opened = blocking(|| {
        let root = tempfile::tempdir().expect("temporary directory must create");
        let store = NodeStore::open(root.path().join(aether_store::NODE_DATABASE))
            .expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        (root, Arc::new(Mutex::new(store)))
    })
    .await;
    world.proof.queue.store = Some(opened);
}

#[given(expr = "the caller has queued task {string}")]
async fn has_queued(world: &mut World, id: String) {
    let queued = input(&id);
    let answer =
        world.proof.queue.with_store(move |store| store.admit(&queued, LIMITS)).await;
    assert_eq!(answer, Ok(Admission::Accepted(id)));
}

#[when(expr = "the caller queues task {string}")]
#[when(expr = "the caller sends task {string} again")]
async fn queues(world: &mut World, id: String) {
    let answer = world
        .proof
        .queue
        .with_store(move |store| store.admit(&input(&id), LIMITS))
        .await;
    world.proof.queue.answer = Some(answer);
}

#[when(expr = "the caller sends another request with the key of task {string}")]
async fn conflicting_request(world: &mut World, id: String) {
    let answer = world
        .proof
        .queue
        .with_store(move |store| {
            let mut conflicting = input(&id);
            conflicting.request.id = aether_core::RequestId::new("different-request");
            store.admit(&conflicting, LIMITS)
        })
        .await;
    world.proof.queue.answer = Some(answer);
}

#[then(expr = "the store accepts task {string}")]
fn accepts(world: &mut World, id: String) {
    assert_eq!(world.proof.queue.answer, Some(Ok(Admission::Accepted(id))));
}

#[then(expr = "the store returns the existing task {string}")]
fn returns_existing(world: &mut World, id: String) {
    assert_eq!(world.proof.queue.answer, Some(Ok(Admission::Existing(id))));
}

#[then(expr = "the store refuses it because {string}")]
fn refuses(world: &mut World, reason: String) {
    let error = match reason.as_str() {
        "the request does not match the key" => StoreError::DuplicateConflict,
        "the queue is full" => StoreError::Overloaded,
        other => unreachable!("unknown reason {other}"),
    };
    assert_eq!(world.proof.queue.answer, Some(Err(error)));
}

#[given(expr = "the caller cancels task {string}")]
#[when(expr = "the caller cancels task {string}")]
async fn cancels(world: &mut World, id: String) {
    world
        .proof
        .queue
        .with_store(move |store| store.cancel(&id))
        .await
        .expect("task must cancel");
}

#[then(expr = "task {string} is {word}")]
async fn task_is(world: &mut World, id: String, state: String) {
    let expected = TaskState::parse(&state).expect("a known task state");
    let actual = world.proof.queue.with_store(move |store| store.state(&id)).await;
    assert_eq!(actual, Ok(expected));
}

#[then(expr = "the result of task {string} is unknown")]
async fn result_unknown(world: &mut World, id: String) {
    let actual = world.proof.queue.with_store(move |store| store.state(&id)).await;
    assert_eq!(actual, Ok(TaskState::UnknownResult));
}

#[given(expr = "a worker leases task {string} in turn {string}")]
#[when(expr = "a worker leases task {string} in turn {string}")]
#[then(expr = "a worker can lease task {string} again in turn {string}")]
async fn leases(world: &mut World, id: String, turn: String) {
    let leased = world
        .proof
        .queue
        .with_store(move |store| store.lease_next("worker", &turn, 1))
        .await;
    assert_eq!(leased, Ok(Some(id)));
}

#[given(expr = "task {string} starts running in turn {string}")]
async fn starts_running(world: &mut World, id: String, turn: String) {
    world
        .proof
        .queue
        .with_store(move |store| store.mark_running(&id, &turn))
        .await
        .expect("task must run");
}

#[given("the lease expires")]
#[when("the lease expires")]
async fn lease_expires(world: &mut World) {
    world
        .proof
        .queue
        .with_store(|store| store.recover_expired_leases(1))
        .await
        .expect("lease must recover");
}

#[then("no worker can lease a task")]
async fn nothing_to_lease(world: &mut World) {
    let leased = world
        .proof
        .queue
        .with_store(|store| store.lease_next("worker", "turn-three", 2))
        .await;
    assert_eq!(leased, Ok(None));
}

#[then(expr = "the events of task {string} after sequence {int} are {string}")]
async fn events_after(world: &mut World, id: String, sequence: i64, expected: String) {
    let events = world
        .proof
        .queue
        .with_store(move |store| store.events_after(&id, sequence))
        .await
        .expect("events must reconnect");
    let types = events
        .into_iter()
        .map(|event| event.event_type.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(types, names(&expected));
}
