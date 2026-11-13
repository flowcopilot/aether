//! Tests of a node that runs more than one task at a time: the cap, a
//! lowered cap, a cancel, a lost lease and a shutdown with several tasks
//! running. The agent is a fake behind the `Agent` seam; each of its turns
//! waits until the test releases or cancels its task.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use aether_core::{
    Claim, EndpointId, PolicyEngine, Request, Scope, ScopeId, ScopeKind,
    limits::{MAX_RUNNING_TASKS, QueueLimitId},
};
use aether_store::{NodeStore, QueueLimits, TaskState};

use super::{
    ProofNode,
    scheduler::SchedulerContext,
    tests::{admit_task, test_grant, test_request, wait_until},
};
use crate::{
    execution::{
        Agent, ExecutionError, ExecutionEvent, Executor, Limits, Task, TurnContext,
    },
    test_support::Trusted,
};

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("lock must not be poisoned")
}

#[derive(Default)]
struct TurnLog {
    /// The tasks whose turn is in progress.
    inside: Mutex<BTreeSet<String>>,
    /// Every turn start, in order.
    started: Mutex<Vec<String>>,
    /// The most turns that were in progress at one time.
    peak: AtomicUsize,
    released: Mutex<BTreeSet<String>>,
    cancelled: Mutex<BTreeSet<String>>,
    /// What the agent says it can run at once; 0 for no limit of its own.
    capacity: AtomicUsize,
}

impl TurnLog {
    fn inside(&self) -> Vec<String> {
        locked(&self.inside).iter().cloned().collect()
    }

    fn started(&self) -> Vec<String> {
        locked(&self.started).clone()
    }

    fn release(&self, task_id: &str) {
        locked(&self.released).insert(task_id.to_owned());
    }

    fn was_cancelled(&self, task_id: &str) -> bool {
        locked(&self.cancelled).contains(task_id)
    }
}

#[derive(Clone)]
struct Parallel(Arc<TurnLog>);

impl Agent for Parallel {
    fn start_turn_with_events(
        &mut self,
        ctx: &TurnContext,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        let log = &self.0;
        {
            let mut inside = locked(&log.inside);
            inside.insert(ctx.task_id.clone());
            log.peak.fetch_max(inside.len(), Ordering::AcqRel);
        }
        locked(&log.started).push(ctx.task_id.clone());
        let result = loop {
            if locked(&log.cancelled).contains(&ctx.task_id) {
                break Err(ExecutionError::Cancelled);
            }
            if locked(&log.released).contains(&ctx.task_id) {
                break Ok(format!("done:{}", ctx.task_id));
            }
            thread::sleep(Duration::from_millis(5));
        };
        locked(&log.inside).remove(&ctx.task_id);
        if result.is_ok() {
            events(ExecutionEvent::Completed);
        }
        result
    }

    fn cancel(&self, task_id: &str) {
        locked(&self.0.cancelled).insert(task_id.to_owned());
    }

    fn capacity(&self) -> usize {
        match self.0.capacity.load(Ordering::Acquire) {
            0 => usize::MAX,
            capacity => capacity,
        }
    }
}

type Submission = (Request, Claim, EndpointId, Task);

struct Rig {
    node: Arc<ProofNode<Trusted, Parallel>>,
    log: Arc<TurnLog>,
    submissions: Vec<Submission>,
    database: std::path::PathBuf,
    _directory: tempfile::TempDir,
}

impl Rig {
    /// A node with `running` slots and `tasks` callers that each have one
    /// task to send.
    fn new(running: usize, tasks: usize) -> Self {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let database = workspace.join(aether_store::NODE_DATABASE);
        let submissions = (0..tasks)
            .map(|index| {
                test_request(&format!("task-{index}"), &format!("peer-{index}"))
            })
            .collect::<Vec<_>>();
        let grants = submissions
            .iter()
            .enumerate()
            .map(|(index, (_, _, endpoint, _))| {
                test_grant(&format!("peer-{index}"), endpoint)
            })
            .collect::<Vec<_>>();
        let log = Arc::new(TurnLog::default());
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, grants),
            NodeStore::open(&database).expect("store must open"),
            Executor::new(
                Parallel(Arc::clone(&log)),
                Limits { workspace, max_context_tokens: 32, max_seconds: 120 },
            ),
            QueueLimits { global: 16, per_peer: 4 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        node.set_running_limit(running).expect("running limit must set");
        Self { node: Arc::new(node), log, submissions, database, _directory: directory }
    }

    /// Admits the task at `index`; returns its ID.
    fn admit(&self, index: usize) -> String {
        let (request, claim, endpoint, task) =
            self.submissions.get(index).expect("the submission must exist");
        admit_task(&self.node, request, claim, endpoint, &format!("key-{index}"), task)
            .expect("task must admit")
    }

    fn state(&self, task_id: &str) -> TaskState {
        self.node.state(task_id).expect("state must load")
    }

    fn wait_for_state(&self, task_id: &str, expected: TaskState) {
        wait_until(&format!("{task_id} to be {expected:?}"), || {
            self.node.state(task_id).is_ok_and(|state| state == expected)
        });
    }

    fn wait_for_turns(&self, count: usize) {
        wait_until(&format!("{count} turns in progress"), || {
            self.log.inside().len() == count
        });
    }

    /// Gives the scheduler's own context, so a test can run what its loop
    /// runs.
    fn scheduler(&self) -> SchedulerContext<Trusted, Parallel> {
        SchedulerContext {
            store: self.node.store.clone(),
            executor: Arc::clone(&self.node.executor),
            canceller: self.node.canceller.clone(),
            event_sender: self.node.event_sender.clone(),
            shutdown: Arc::clone(&self.node.shutdown),
            running: Arc::clone(&self.node.running),
            running_limit: Arc::clone(&self.node.running_limit),
            execution_threads: Arc::clone(&self.node.execution_threads),
            workspace: self.node.workspace.clone(),
            dispatch_serial: Arc::clone(&self.node.dispatch_serial),
            scheduler_wake: self.node.scheduler_wake.clone(),
            policy: Arc::clone(&self.node.policy),
            scopes: self.node.scopes.clone(),
        }
    }

    /// Lets every turn that is still in progress finish, then stops the
    /// node.
    fn finish(self) {
        for task_id in self.log.inside() {
            self.log.release(&task_id);
        }
        self.node.shutdown();
    }
}

#[test]
fn a_new_node_runs_one_task_at_a_time() {
    let directory = tempfile::tempdir().expect("directory must create");
    let workspace = directory.path().to_path_buf();
    let log = Arc::new(TurnLog::default());
    let node = ProofNode::new(
        PolicyEngine::new(Trusted, Vec::new()),
        NodeStore::in_dir(&workspace).expect("store must open"),
        Executor::new(
            Parallel(log),
            Limits { workspace, max_context_tokens: 32, max_seconds: 120 },
        ),
        QueueLimits { global: 1, per_peer: 1 },
        Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
    )
    .expect("node must create");
    assert_eq!(
        node.current_queue_limit(QueueLimitId::Running).expect("limit reads"),
        1
    );
    assert_eq!(node.effective_running_limit(), 1);
    node.shutdown();
}

#[test]
fn the_running_limit_has_a_floor_and_a_ceiling() {
    let rig = Rig::new(1, 0);
    assert!(rig.node.set_running_limit(0).is_err());
    let too_many =
        usize::try_from(MAX_RUNNING_TASKS).expect("the cap fits").saturating_add(1);
    assert!(rig.node.set_running_limit(too_many).is_err());
    assert!(rig.node.set_queue_limit(QueueLimitId::Running, 0).is_err());
    assert_eq!(rig.node.effective_running_limit(), 1);
    rig.node
        .set_queue_limit(QueueLimitId::Running, MAX_RUNNING_TASKS)
        .expect("max sets");
    assert_eq!(
        rig.node.current_queue_limit(QueueLimitId::Running).expect("limit reads"),
        MAX_RUNNING_TASKS
    );
    rig.finish();
}

#[test]
fn a_limit_of_two_runs_two_tasks_at_once_and_never_more() {
    let rig = Rig::new(2, 4);
    let ids = (0..4).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(2);
    // The first two run; the others wait for a slot.
    let mut first = rig.log.started();
    first.sort();
    assert_eq!(first, ["task-0", "task-1"]);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.log.inside().len(), 2);
    assert_eq!(rig.state(&ids[2]), TaskState::Queued);
    assert_eq!(rig.state(&ids[3]), TaskState::Queued);
    // A task that ends hands its slot to the next one, in queue order.
    rig.log.release(&ids[0]);
    rig.wait_for_state(&ids[0], TaskState::Succeeded);
    wait_until("task 2 to start", || rig.log.started().contains(&ids[2]));
    assert_eq!(rig.state(&ids[3]), TaskState::Queued);
    rig.log.release(&ids[1]);
    wait_until("task 3 to start", || rig.log.started().contains(&ids[3]));
    for id in &ids {
        rig.log.release(id);
    }
    for id in &ids {
        rig.wait_for_state(id, TaskState::Succeeded);
    }
    assert_eq!(rig.log.peak.load(Ordering::Acquire), 2);
    // Each task's start is confirmed once: one turn for each.
    let mut started = rig.log.started();
    started.sort();
    assert_eq!(started, ["task-0", "task-1", "task-2", "task-3"]);
    wait_until("every slot to be free", || rig.node.running.reserved() == 0);
    rig.finish();
}

#[test]
fn raising_the_limit_starts_waiting_tasks_without_a_restart() {
    let rig = Rig::new(1, 3);
    let ids = (0..3).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(1);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.log.inside().len(), 1);
    rig.node.set_queue_limit(QueueLimitId::Running, 3).expect("limit must raise");
    rig.wait_for_turns(3);
    for id in &ids {
        assert_eq!(rig.state(id), TaskState::Running);
    }
    rig.finish();
}

#[test]
fn lowering_the_limit_never_stops_a_running_task() {
    let rig = Rig::new(3, 4);
    let ids = (0..4).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(3);
    assert_eq!(rig.state(&ids[3]), TaskState::Queued);
    rig.node.set_queue_limit(QueueLimitId::Running, 1).expect("limit must lower");
    thread::sleep(Duration::from_millis(100));
    // All three run on, and nothing was cancelled.
    assert_eq!(rig.log.inside().len(), 3);
    for id in &ids[..3] {
        assert_eq!(rig.state(id), TaskState::Running);
        assert!(!rig.log.was_cancelled(id));
    }
    // Two running tasks end: one still runs, which is the new limit, so the
    // waiting task does not start yet.
    rig.log.release(&ids[0]);
    rig.log.release(&ids[1]);
    rig.wait_for_state(&ids[0], TaskState::Succeeded);
    rig.wait_for_state(&ids[1], TaskState::Succeeded);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(rig.state(&ids[3]), TaskState::Queued);
    // The last running task ends: the waiting one starts.
    rig.log.release(&ids[2]);
    wait_until("the waiting task to start", || rig.log.started().contains(&ids[3]));
    assert_eq!(rig.log.peak.load(Ordering::Acquire), 3);
    rig.finish();
}

#[test]
fn an_agent_that_runs_fewer_tasks_lowers_the_limit() {
    let rig = Rig::new(3, 3);
    rig.log.capacity.store(1, Ordering::Release);
    assert_eq!(rig.node.effective_running_limit(), 1);
    let ids = (0..3).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(1);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.log.inside().len(), 1);
    // The agent now takes more: the waiting tasks start.
    rig.log.capacity.store(0, Ordering::Release);
    rig.node.set_running_limit(3).expect("limit must set");
    rig.wait_for_turns(3);
    assert_eq!(rig.node.effective_running_limit(), 3);
    for id in &ids {
        assert_eq!(rig.state(id), TaskState::Running);
    }
    rig.finish();
}

#[test]
fn a_cancel_stops_only_its_task() {
    let rig = Rig::new(2, 3);
    let ids = (0..3).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(2);
    rig.node.cancel(&ids[0]).expect("task must cancel");
    rig.wait_for_state(&ids[0], TaskState::Cancelled);
    assert!(rig.log.was_cancelled(&ids[0]));
    assert!(!rig.log.was_cancelled(&ids[1]));
    // The other task runs on, and the freed slot starts the waiting one.
    wait_until("task 2 to start", || rig.log.started().contains(&ids[2]));
    assert_eq!(rig.state(&ids[1]), TaskState::Running);
    rig.log.release(&ids[1]);
    rig.wait_for_state(&ids[1], TaskState::Succeeded);
    rig.log.release(&ids[2]);
    rig.wait_for_state(&ids[2], TaskState::Succeeded);
    assert_eq!(rig.state(&ids[0]), TaskState::Cancelled);
    rig.finish();
}

#[test]
fn a_lost_lease_stops_only_its_task() {
    let rig = Rig::new(2, 2);
    let ids = (0..2).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(2);
    // The lease of the first task runs out; the second keeps its own.
    let connection = rusqlite::Connection::open(&rig.database).expect("database opens");
    connection.busy_timeout(Duration::from_secs(5)).expect("timeout sets");
    let changed = connection
        .execute("UPDATE execution_leases SET expires_at=1 WHERE task_id=?1", [&ids[0]])
        .expect("lease must expire");
    assert_eq!(changed, 1);
    drop(connection);
    rig.scheduler().maintain();
    rig.wait_for_state(&ids[0], TaskState::UnknownResult);
    wait_until("the turn of the lost task to stop", || {
        rig.log.was_cancelled(&ids[0]) && rig.log.inside() == [ids[1].clone()]
    });
    assert!(!rig.log.was_cancelled(&ids[1]));
    assert_eq!(rig.state(&ids[1]), TaskState::Running);
    rig.log.release(&ids[1]);
    rig.wait_for_state(&ids[1], TaskState::Succeeded);
    // The lost task stays as recovery left it.
    assert_eq!(rig.state(&ids[0]), TaskState::UnknownResult);
    rig.finish();
}

#[test]
fn shutdown_cancels_and_joins_every_running_task() {
    let rig = Rig::new(3, 4);
    let ids = (0..4).map(|index| rig.admit(index)).collect::<Vec<_>>();
    rig.wait_for_turns(3);
    let (node, log) = (Arc::clone(&rig.node), Arc::clone(&rig.log));
    node.shutdown();
    for id in &ids[..3] {
        assert!(log.was_cancelled(id), "{id} must get the cancel");
    }
    assert!(log.inside().is_empty(), "every turn must have ended");
    assert_eq!(node.running.reserved(), 0);
    assert!(locked(&node.execution_threads).is_empty());
    let store = NodeStore::open(&rig.database).expect("store must reopen");
    for id in &ids[..3] {
        assert_eq!(store.state(id).expect("task is durable"), TaskState::Cancelled);
    }
    // The task that did not run waits for the next start of the node.
    assert_eq!(store.state(&ids[3]).expect("task is durable"), TaskState::Queued);
}

#[test]
fn the_default_limit_runs_tasks_one_at_a_time_as_before() {
    let rig = Rig::new(1, 3);
    let ids = (0..3).map(|index| rig.admit(index)).collect::<Vec<_>>();
    for (position, id) in ids.iter().enumerate() {
        wait_until("the next task to start", || rig.log.started().contains(id));
        assert_eq!(
            rig.log.inside(),
            std::slice::from_ref(id),
            "task {position} runs alone"
        );
        thread::sleep(Duration::from_millis(50));
        assert_eq!(rig.log.inside().len(), 1);
        rig.log.release(id);
        rig.wait_for_state(id, TaskState::Succeeded);
    }
    assert_eq!(rig.log.peak.load(Ordering::Acquire), 1);
    assert_eq!(rig.log.started(), ["task-0", "task-1", "task-2"]);
    rig.finish();
}
