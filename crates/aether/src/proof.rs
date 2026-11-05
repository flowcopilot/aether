use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use aether_core::{
    Admission, Claim, Denial, EndpointId, PolicyEngine, Request, Scope, ScopeId,
    ScopeKind, TrustProvider,
};
use aether_store::TaskExecutionData;
use aether_store::{
    NodeStore, QueueLimits, StoreError, TaskAdmission as StoreAdmission, TaskInput,
    TaskState,
};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::execution::{
    Agent, ExecutionError, ExecutionEvent, Executor, Task, Workspace,
};

#[derive(Debug)]
pub enum ProofError {
    Denied(Denial),
    /// No access group of a direct scope holds the caller.
    GroupDenied,
    Store(StoreError),
    Execution(ExecutionError),
    TaskMismatch,
    Queue,
    Shutdown,
}

impl From<StoreError> for ProofError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<ExecutionError> for ProofError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

/// How the caller of a task proved who it is.
#[derive(Clone, Copy)]
pub enum Identity<'a> {
    /// An Iroh caller's signed claim, checked by `PolicyEngine::admit`.
    Claim(&'a Claim),
    /// A PACT caller, whose JWT the PACT gateway verified. Only
    /// `PolicyEngine::admit_verified` checks it.
    Verified,
}

#[derive(Clone, Copy)]
pub struct SubmissionInput<'a> {
    pub request: &'a Request,
    pub authenticated_endpoint: &'a EndpointId,
    pub idempotency_key: &'a str,
    pub task: &'a Task,
}

type StoreCommand = Box<dyn FnOnce(&mut NodeStore) + Send>;

enum StoreMessage {
    Operation(StoreCommand),
    Stop,
}

struct StoreWorker {
    sender: SyncSender<StoreMessage>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Clone for StoreWorker {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), thread: Mutex::new(None) }
    }
}

impl StoreWorker {
    fn new(store: NodeStore) -> Result<Self, ProofError> {
        let (sender, receiver) = mpsc::sync_channel::<StoreMessage>(64);
        let thread = thread::Builder::new()
            .name("aether-store".to_owned())
            .spawn(move || {
                let mut store = store;
                while let Ok(StoreMessage::Operation(command)) = receiver.recv() {
                    command(&mut store);
                }
            })
            .map_err(|_| ProofError::Queue)?;
        Ok(Self { sender, thread: Mutex::new(Some(thread)) })
    }

    fn call<R: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut NodeStore) -> Result<R, StoreError> + Send + 'static,
    ) -> Result<R, ProofError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(StoreMessage::Operation(Box::new(move |store| {
                let _ = sender.send(operation(store));
            })))
            .map_err(|_| ProofError::Queue)?;
        receiver.recv().map_err(|_| ProofError::Queue)?.map_err(ProofError::Store)
    }

    fn admit_only(
        &self,
        input: TaskInput,
        limits: QueueLimits,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        self.call(move |store| match replay_key {
            Some(replay_key) => store.admit_with_replay(
                &input,
                limits,
                &replay_key,
                replay_expiry(),
                REPLAY_LIMIT,
            ),
            None => store.admit(&input, limits),
        })
    }

    fn stop(&self) {
        let _ = self.sender.send(StoreMessage::Stop);
    }

    fn join(&self) {
        self.stop();
        if let Ok(mut guard) = self.thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

struct ExecutionBundle {
    task_id: String,
    execution_id: String,
    task: Task,
}

fn permits_dispatch<T>(
    policy: &PolicyEngine<T>,
    scopes: &BTreeMap<ScopeId, Scope>,
    data: &TaskExecutionData,
) -> Result<(), Denial>
where
    T: TrustProvider,
{
    let scope = scopes.get(&data.request.scope_id).ok_or(Denial::ScopeMismatch)?;
    policy.permits_dispatch(&data.request, scope)
}

fn group_allows_dispatch(
    store: &StoreWorker,
    scopes: &BTreeMap<ScopeId, Scope>,
    data: &TaskExecutionData,
) -> Result<bool, ProofError> {
    if scopes
        .get(&data.request.scope_id)
        .is_none_or(|scope| scope.kind != ScopeKind::Direct)
    {
        return Ok(true);
    }
    let agent = data.request.caller_agent_id.as_str().to_owned();
    let endpoint = data.request.caller_endpoint_id.as_str().to_owned();
    store.call(move |store| store.access_group_allows(&agent, &endpoint))
}

struct SchedulerContext<T, P> {
    store: StoreWorker,
    executor: Arc<Mutex<Executor<P>>>,
    canceller: P,
    event_sender: broadcast::Sender<String>,
    shutdown: Arc<AtomicBool>,
    executing: Arc<AtomicBool>,
    active_task: Arc<Mutex<Option<String>>>,
    execution_thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    workspace: Workspace,
    dispatch_serial: Arc<Mutex<()>>,
    scheduler_wake: SyncSender<()>,
    policy: Arc<Mutex<PolicyEngine<T>>>,
    scopes: BTreeMap<ScopeId, Scope>,
}

impl<T, P> SchedulerContext<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    fn prepare_execution(&self) -> Result<Option<ExecutionBundle>, ProofError> {
        let _dispatch_guard =
            self.dispatch_serial.lock().map_err(|_| ProofError::Queue)?;
        let execution_id = Uuid::new_v4().to_string();
        let leased = self.store.call({
            let execution_id = execution_id.clone();
            move |store| {
                store.lease_next("scheduler", &execution_id, timestamp_after(LEASE_TTL))
            }
        })?;
        let Some(task_id) = leased else {
            return Ok(None);
        };
        let prepared = self.prepare_leased(task_id, execution_id);
        if !matches!(prepared, Ok(Some(_))) {
            // The leased task does not run, so the next queued task does
            // not wait for the fallback poll.
            let _ = self.scheduler_wake.try_send(());
        }
        prepared
    }

    fn prepare_leased(
        &self,
        task_id: String,
        execution_id: String,
    ) -> Result<Option<ExecutionBundle>, ProofError> {
        let state = self.store.call({
            let task_id = task_id.clone();
            move |store| store.state(&task_id)
        })?;
        if state == TaskState::Cancelled {
            let _ = self.store.call(move |store| store.requeue_leased(&task_id));
            return Ok(None);
        }
        if state != TaskState::Leased {
            return Ok(None);
        }
        let execution_data = self.store.call({
            let task_id = task_id.clone();
            move |store| store.task_execution_data(&task_id)
        })?;
        let workspace_id = execution_data.request.workspace_id.as_str();
        if workspace_id.contains("..") || workspace_id.starts_with('/') {
            return Err(ProofError::Execution(ExecutionError::Path));
        }
        let policy = self.policy.lock().map_err(|_| ProofError::Queue)?;
        let dispatch = permits_dispatch(&policy, &self.scopes, &execution_data);
        drop(policy);
        let allowed =
            group_allows_dispatch(&self.store, &self.scopes, &execution_data)?;
        let refusal = match dispatch {
            Err(denial) => Some(ProofError::Denied(denial)),
            Ok(()) if !allowed => Some(ProofError::GroupDenied),
            Ok(()) => None,
        };
        if let Some(refusal) = refusal {
            let reject_id = task_id.clone();
            let reject_execution = execution_id;
            let _ = self
                .store
                .call(move |store| store.mark_rejected(&reject_id, &reject_execution));
            let _ = self.event_sender.send(task_id);
            return Err(refusal);
        }
        let task = Task {
            id: task_id.clone(),
            skill: execution_data.skill,
            workspace_id: execution_data.request.workspace_id.as_str().to_owned(),
            remote_text: execution_data.remote_text,
            context_tokens: 0,
        };
        Ok(Some(ExecutionBundle { task_id, execution_id, task }))
    }

    fn run_execution(&self, bundle: ExecutionBundle) {
        let ExecutionBundle { task_id, execution_id, task } = bundle;
        if self.shutdown.load(Ordering::Acquire) {
            // Shutdown landed before this worker could register as the
            // active task or touch the executor. The lease was never
            // confirmed as started, so the task can go straight back to the
            // queue for a future node run instead of being marked failed.
            let _ = self.store.call(move |store| store.requeue_leased(&task_id));
            self.finish_execution();
            return;
        }
        if let Ok(mut active) = self.active_task.lock() {
            *active = Some(task_id.clone());
        }
        let renewal_failed = Arc::new(AtomicBool::new(false));
        let renewal = {
            let store = self.store.clone();
            let task_id = task_id.clone();
            let execution_id = execution_id.clone();
            let renewal_failed = Arc::clone(&renewal_failed);
            let shutdown = Arc::clone(&self.shutdown);
            let canceller = self.canceller.clone();
            let (stop, stop_rx) = mpsc::channel();
            let spawned = thread::Builder::new()
                .name("aether-lease-renewal".to_owned())
                .spawn(move || {
                    loop {
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        match stop_rx.recv_timeout(LEASE_RENEWAL_INTERVAL) {
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => {
                                if renewal_failed.load(Ordering::Acquire) {
                                    // Say it again until the turn ends: an
                                    // adapter drops a cancel for a turn that
                                    // it has not registered yet.
                                    canceller.cancel(&task_id);
                                    continue;
                                }
                                if store
                                    .call({
                                        let task_id = task_id.clone();
                                        let execution_id = execution_id.clone();
                                        move |store| {
                                            store.renew_lease(
                                                &task_id,
                                                &execution_id,
                                                timestamp_after(LEASE_TTL),
                                                timestamp_after(Duration::ZERO),
                                            )
                                        }
                                    })
                                    .is_err()
                                {
                                    renewal_failed.store(true, Ordering::Release);
                                    // Stop the process promptly instead of
                                    // waiting for it to finish on its own; its
                                    // lease may already belong to a recovery
                                    // pass. A cancel also deletes the lease,
                                    // so this is the cancel's second signal.
                                    canceller.cancel(&task_id);
                                }
                            }
                        }
                    }
                });
            (stop, spawned)
        };
        let renewal = match renewal {
            (stop, Ok(handle)) => (stop, handle),
            (_, Err(_)) => {
                // Without a renewal thread the lease would lapse mid-run:
                // give the task back to the queue instead of starting it.
                let _ = self.store.call(move |store| store.requeue_leased(&task_id));
                self.finish_execution();
                return;
            }
        };
        let executor = Arc::clone(&self.executor);
        let store = self.store.clone();
        let event_sender = self.event_sender.clone();
        let Ok(workspace) = self.workspace.path() else {
            self.finish_execution();
            return;
        };
        let dispatch_task_id = task_id.clone();
        let start_store = self.store.clone();
        let dispatch_serial = Arc::clone(&self.dispatch_serial);
        let shutdown = Arc::clone(&self.shutdown);
        let policy = Arc::clone(&self.policy);
        let scopes = self.scopes.clone();
        let start_execution_id = execution_id.clone();
        let start_confirmed = Arc::new(AtomicBool::new(false));
        let confirmed_flag = Arc::clone(&start_confirmed);
        let mut event_error: Option<ProofError> = None;
        let result = executor.lock().map_err(|_| ExecutionError::Unavailable).and_then(
            |mut executor| {
                executor.execute_with_confirmed_start(
                    &task,
                    &workspace,
                    &mut || {
                        // Serialize against `cancel`/`cancel_from` so the
                        // decision to actually start cannot interleave with a
                        // cancellation racing in on the same task.
                        let _dispatch_guard = dispatch_serial
                            .lock()
                            .map_err(|_| ExecutionError::Unavailable)?;
                        if shutdown.load(Ordering::Acquire) {
                            return Err(ExecutionError::Cancelled);
                        }
                        // Recheck policy against the live grants: one revoked
                        // after this task was leased must not authorize a
                        // stale start.
                        let execution_data = start_store
                            .call({
                                let task_id = dispatch_task_id.clone();
                                move |store| store.task_execution_data(&task_id)
                            })
                            .map_err(|_| ExecutionError::Unavailable)?;
                        {
                            let policy_guard = policy
                                .lock()
                                .map_err(|_| ExecutionError::Unavailable)?;
                            if permits_dispatch(&policy_guard, &scopes, &execution_data)
                                .is_err()
                                || !group_allows_dispatch(
                                    &start_store,
                                    &scopes,
                                    &execution_data,
                                )
                                .unwrap_or(false)
                            {
                                let reject_id = dispatch_task_id.clone();
                                let reject_execution = start_execution_id.clone();
                                let _ = start_store.call(move |store| {
                                    store.mark_rejected(&reject_id, &reject_execution)
                                });
                                return Err(ExecutionError::Unavailable);
                            }
                        }
                        start_store
                            .call({
                                let task_id = dispatch_task_id.clone();
                                let execution_id = start_execution_id.clone();
                                move |store| store.mark_running(&task_id, &execution_id)
                            })
                            .map_err(|_| ExecutionError::Unavailable)?;
                        let outcome = match start_store.call({
                            let task_id = dispatch_task_id.clone();
                            move |store| store.state(&task_id)
                        }) {
                            Ok(TaskState::Cancelled) => Err(ExecutionError::Cancelled),
                            Ok(TaskState::Leased | TaskState::Running) => Ok(()),
                            Ok(_) | Err(_) => Err(ExecutionError::Unavailable),
                        };
                        if outcome.is_ok() {
                            confirmed_flag.store(true, Ordering::Release);
                        }
                        outcome
                    },
                    &mut |event| {
                        if event_error.is_none() {
                            if event == ExecutionEvent::Completed {
                                let event_task_id = dispatch_task_id.clone();
                                if matches!(
                                    store
                                        .call(move |store| store.state(&event_task_id)),
                                    Ok(TaskState::Cancelled)
                                ) {
                                    return;
                                }
                            }
                            let event_task_id = dispatch_task_id.clone();
                            let event_type = event.event_type();
                            event_error = store
                                .call(move |store| {
                                    store.append_event(&event_task_id, event_type)
                                })
                                .err();
                            if event_error.is_none() {
                                let _ = event_sender.send(dispatch_task_id.clone());
                            }
                        }
                    },
                )
            },
        );
        let _ = renewal.0.send(());
        let _ = renewal.1.join();
        if let Ok(mut active) = self.active_task.lock()
            && active.as_deref() == Some(task_id.as_str())
        {
            *active = None;
        }
        let start_confirmed = start_confirmed.load(Ordering::Acquire);
        if renewal_failed.load(Ordering::Acquire) {
            if let Ok(late_result) = &result {
                let task_id = task_id.clone();
                let late_result = late_result.clone();
                let late_execution = execution_id.clone();
                let _ = self.store.call(move |store| {
                    store.mark_succeeded(
                        &task_id,
                        &late_execution,
                        &late_result,
                        result_expiry(),
                    )
                });
            }
            if matches!(
                self.store.call({
                    let task_id = task_id.clone();
                    move |store| store.state(&task_id)
                }),
                Ok(TaskState::Cancelled)
            ) {
                self.finish_execution();
                return;
            }
            let outcome_task_id = task_id.clone();
            let outcome_execution = execution_id;
            let _ = self.store.call(move |store| {
                if start_confirmed {
                    store.mark_unknown_result(&outcome_task_id, &outcome_execution)
                } else {
                    store.mark_failed(&outcome_task_id, &outcome_execution)
                }
            });
            let _ = self.event_sender.send(task_id);
            self.finish_execution();
            return;
        }
        if event_error.is_some() {
            let outcome_task_id = task_id.clone();
            let outcome_execution = execution_id;
            let _ = self.store.call(move |store| {
                if start_confirmed {
                    store.mark_unknown_result(&outcome_task_id, &outcome_execution)
                } else {
                    store.mark_failed(&outcome_task_id, &outcome_execution)
                }
            });
            let _ = self.event_sender.send(task_id);
            self.finish_execution();
            return;
        }
        match result {
            Ok(result) => {
                let succeeded_task_id = task_id.clone();
                let succeeded_execution = execution_id;
                let _ = self.store.call(move |store| {
                    store.mark_succeeded(
                        &succeeded_task_id,
                        &succeeded_execution,
                        &result,
                        result_expiry(),
                    )
                });
            }
            Err(ExecutionError::Cancelled) => {}
            Err(_) => {
                // Once the process was told to start, a failure here means
                // its outcome is genuinely unknown: it may have completed on
                // the remote side even though this node lost track of it.
                // Preserve that ambiguity for operator review rather than
                // silently discarding the task as failed.
                let outcome_task_id = task_id.clone();
                let outcome_execution = execution_id;
                let _ = self.store.call(move |store| {
                    if start_confirmed {
                        store.mark_unknown_result(&outcome_task_id, &outcome_execution)
                    } else {
                        store.mark_failed(&outcome_task_id, &outcome_execution)
                    }
                });
            }
        }
        let _ = self.event_sender.send(task_id);
        self.finish_execution();
    }

    fn finish_execution(&self) {
        self.executing.store(false, Ordering::Release);
        let _ = self.scheduler_wake.try_send(());
    }

    fn dispatch_one(&self) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        if self.executing.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(Some(bundle)) = self.prepare_execution() else {
            // Nothing queued, or the task failed before it started.
            self.executing.store(false, Ordering::Release);
            return;
        };
        // Join the previous execution thread's handle before replacing it.
        // The `executing` gate above guarantees it has already returned; this
        // just reclaims the `JoinHandle` so shutdown never has to guess
        // whether a stale handle is still live.
        if let Ok(mut previous) = self.execution_thread.lock()
            && let Some(handle) = previous.take()
        {
            let _ = handle.join();
        }
        let store = self.store.clone();
        let executor = Arc::clone(&self.executor);
        let canceller = self.canceller.clone();
        let event_sender = self.event_sender.clone();
        let shutdown = Arc::clone(&self.shutdown);
        let executing = Arc::clone(&self.executing);
        let active_task = Arc::clone(&self.active_task);
        let execution_thread = Arc::clone(&self.execution_thread);
        let workspace = self.workspace.clone();
        let dispatch_serial = Arc::clone(&self.dispatch_serial);
        let scheduler_wake = self.scheduler_wake.clone();
        let ctx = Self {
            store,
            executor,
            canceller,
            event_sender,
            shutdown,
            executing,
            active_task,
            execution_thread: Arc::clone(&execution_thread),
            workspace,
            dispatch_serial,
            scheduler_wake,
            policy: Arc::clone(&self.policy),
            scopes: self.scopes.clone(),
        };
        let task_id = bundle.task_id.clone();
        if let Ok(handle) = thread::Builder::new()
            .name("aether-execution".to_owned())
            .spawn(move || ctx.run_execution(bundle))
        {
            if let Ok(mut slot) = self.execution_thread.lock() {
                *slot = Some(handle);
            }
        } else {
            // The task never started: give the execution slot back and
            // return the lease to the queue so the scheduler retries it
            // instead of leaving it stranded in `Leased` forever.
            self.executing.store(false, Ordering::Release);
            let _ = self.store.call(move |store| store.requeue_leased(&task_id));
            let _ = self.scheduler_wake.try_send(());
        }
    }

    fn run(&self, wake: &mpsc::Receiver<()>) {
        let mut last_maintenance = Instant::now();
        while !self.shutdown.load(Ordering::Acquire) {
            let _ = wake.recv_timeout(SCHEDULER_POLL);
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
                self.maintain();
                last_maintenance = Instant::now();
            }
            self.dispatch_one();
        }
    }

    fn maintain(&self) {
        let now = timestamp_after(Duration::ZERO);
        let recovered =
            self.store.call(move |store| store.recover_expired_leases(now)).ok();
        if let Some(recovered) = recovered
            && !recovered.is_empty()
        {
            let active = self.active_task.lock().ok().and_then(|active| active.clone());
            if let Some(active_id) = active
                && recovered.contains(&active_id)
            {
                // The active worker's lease was just reclaimed as
                // `UnknownResult`; stop it promptly instead of letting it
                // keep running unsupervised past its recovered deadline.
                self.canceller.cancel(&active_id);
            }
            for task_id in recovered {
                let _ = self.event_sender.send(task_id);
            }
        }
        let _ = self.store.call(move |store| store.clear_expired_content(now));
    }
}

pub struct ProofNode<T, P> {
    policy: Arc<Mutex<PolicyEngine<T>>>,
    store: StoreWorker,
    canceller: P,
    limits: Arc<Mutex<QueueLimits>>,
    scope: Scope,
    scopes: BTreeMap<aether_core::ScopeId, Scope>,
    event_sender: broadcast::Sender<String>,
    shutdown: Arc<AtomicBool>,
    scheduler_wake: SyncSender<()>,
    scheduler_thread: Mutex<Option<JoinHandle<()>>>,
    active_task: Arc<Mutex<Option<String>>>,
    execution_thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    dispatch_serial: Arc<Mutex<()>>,
    executor: Arc<Mutex<Executor<P>>>,
    workspace: Workspace,
}

impl<T, P> ProofNode<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    pub fn new(
        policy: PolicyEngine<T>,
        store: NodeStore,
        executor: Executor<P>,
        limits: QueueLimits,
        scope: Scope,
    ) -> Result<Self, ProofError> {
        Self::with_scopes(policy, store, executor, limits, [scope])
    }

    pub fn with_scopes(
        policy: PolicyEngine<T>,
        mut store: NodeStore,
        executor: Executor<P>,
        limits: QueueLimits,
        scopes: impl IntoIterator<Item = Scope>,
    ) -> Result<Self, ProofError> {
        let scopes = scopes
            .into_iter()
            .map(|scope| (scope.id.clone(), scope))
            .collect::<BTreeMap<_, _>>();
        let scope = scopes.values().next().cloned().ok_or(ProofError::Queue)?;
        let identities = policy
            .grants()
            .iter()
            .filter(|grant| {
                scopes
                    .get(&grant.scope_id)
                    .is_some_and(|scope| scope.kind == ScopeKind::Direct)
            })
            .map(|grant| {
                (
                    grant.subject_agent_id.as_str().to_owned(),
                    grant.subject_endpoint_id.as_str().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        store.initialize_access_groups(&identities)?;
        let workspace = executor.workspace();
        let store = StoreWorker::new(store)?;
        let stored_scopes = scopes.values().cloned().collect::<Vec<_>>();
        store.call(move |store| {
            store.recover_expired_leases(timestamp_after(Duration::ZERO))?;
            store.clear_expired_content(timestamp_after(Duration::ZERO))?;
            for scope in stored_scopes {
                store.put_scope(scope.id.as_str(), scope.kind)?;
            }
            Ok(())
        })?;
        let canceller = executor.cancellation_handle();
        let (event_sender, _) = broadcast::channel(64);
        let shutdown = Arc::new(AtomicBool::new(false));
        let executing = Arc::new(AtomicBool::new(false));
        let active_task = Arc::new(Mutex::new(None));
        let execution_thread = Arc::new(Mutex::new(None));
        let dispatch_serial = Arc::new(Mutex::new(()));
        let (scheduler_wake, scheduler_wake_rx) = mpsc::sync_channel(64);
        let executor = Arc::new(Mutex::new(executor));
        let policy = Arc::new(Mutex::new(policy));
        let scheduler_ctx = SchedulerContext {
            store: store.clone(),
            executor: Arc::clone(&executor),
            canceller: canceller.clone(),
            event_sender: event_sender.clone(),
            shutdown: Arc::clone(&shutdown),
            executing: Arc::clone(&executing),
            active_task: Arc::clone(&active_task),
            execution_thread: Arc::clone(&execution_thread),
            workspace: workspace.clone(),
            dispatch_serial: Arc::clone(&dispatch_serial),
            scheduler_wake: scheduler_wake.clone(),
            policy: Arc::clone(&policy),
            scopes: scopes.clone(),
        };
        let scheduler_thread = thread::Builder::new()
            .name("aether-scheduler".to_owned())
            .spawn(move || scheduler_ctx.run(&scheduler_wake_rx))
            .map_err(|_| ProofError::Queue)?;
        Ok(Self {
            policy,
            store,
            canceller,
            limits: Arc::new(Mutex::new(limits)),
            scope,
            scopes,
            event_sender,
            shutdown,
            scheduler_wake,
            scheduler_thread: Mutex::new(Some(scheduler_thread)),
            active_task,
            execution_thread,
            dispatch_serial,
            executor,
            workspace,
        })
    }

    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let started = Instant::now();
        let _ = self.scheduler_wake.try_send(());
        if let Ok(active) = self.active_task.lock()
            && let Some(task_id) = active.as_deref()
        {
            let task_id = task_id.to_owned();
            drop(active);
            let _ = self.cancel(&task_id);
        }
        // Stop the scheduler loop first so it leases no further work, then
        // re-assert cancellation for as long as a task stays active: a
        // process that ignores the first interrupt still gets the signal
        // again while we wait for it to stop.
        if let Ok(mut guard) = self.scheduler_thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        // With the scheduler stopped, no new execution can start. Wait for
        // the last execution (and, transitively, lease-renewal) thread within
        // the deadline so storage never closes while it is still writing the
        // task's final state. Past the deadline the thread is left to finish
        // on its own rather than block shutdown forever.
        let execution =
            self.execution_thread.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = execution {
            while !handle.is_finished() && started.elapsed() < SHUTDOWN_DEADLINE {
                if let Ok(active) = self.active_task.lock()
                    && let Some(task_id) = active.as_deref()
                {
                    self.canceller.cancel(task_id);
                }
                thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        self.store.join();
    }

    #[must_use]
    pub fn scope_id(&self) -> &str {
        self.scope.id.as_str()
    }

    #[must_use]
    pub const fn scope_kind(&self) -> ScopeKind {
        self.scope.kind
    }

    #[must_use]
    pub fn scope_kind_for(&self, scope_id: &str) -> Option<ScopeKind> {
        self.scopes.get(&aether_core::ScopeId::new(scope_id)).map(|scope| scope.kind)
    }

    #[must_use]
    pub fn has_scope_kind(&self, kind: ScopeKind) -> bool {
        self.scopes.values().any(|scope| scope.kind == kind)
    }

    #[must_use]
    pub fn scope_id_for_kind(&self, kind: ScopeKind) -> Option<&str> {
        let mut scopes = self.scopes.values().filter(|scope| scope.kind == kind);
        let scope = scopes.next()?;
        if scopes.next().is_some() {
            return None;
        }
        Some(scope.id.as_str())
    }

    pub fn set_queue_limit(&self, id: &str, limit: u32) -> Result<(), ProofError> {
        let limit = usize::try_from(limit).map_err(|_| ProofError::Queue)?;
        let mut limits = self.limits.lock().map_err(|_| ProofError::Queue)?;
        match id {
            "queue_global" => limits.global = limit,
            "queue_per_peer" => limits.per_peer = limit,
            _ => return Err(ProofError::Queue),
        }
        drop(limits);
        Ok(())
    }

    pub fn current_queue_limit(&self, id: &str) -> Result<u32, ProofError> {
        let limits = self.queue_limits()?;
        let limit = match id {
            "queue_global" => limits.global,
            "queue_per_peer" => limits.per_peer,
            _ => return Err(ProofError::Queue),
        };
        u32::try_from(limit).map_err(|_| ProofError::Queue)
    }

    /// Moves the executor and its agent to `path`. The executor
    /// canonicalizes the path; `workspace` then reads the new value.
    pub fn apply_workspace(&self, path: &Path) -> Result<(), ProofError> {
        self.executor.lock().map_err(|_| ProofError::Queue)?.set_workspace(path)?;
        Ok(())
    }

    /// The executor's live workspace path.
    pub fn workspace(&self) -> Result<PathBuf, ProofError> {
        self.workspace.path().map_err(|_| ProofError::Queue)
    }

    pub fn apply_grant(&self, grant: aether_core::Grant) -> Result<(), ProofError> {
        self.apply_grant_inner(grant, true)
    }

    pub fn apply_stored_grant(
        &self,
        grant: aether_core::Grant,
    ) -> Result<(), ProofError> {
        self.apply_grant_inner(grant, false)
    }

    fn apply_grant_inner(
        &self,
        grant: aether_core::Grant,
        add_to_friends: bool,
    ) -> Result<(), ProofError> {
        let scope = self
            .scopes
            .get(&grant.scope_id)
            .ok_or(ProofError::Denied(Denial::ScopeMismatch))?;
        let mut policy = self.policy.lock().map_err(|_| ProofError::Queue)?;
        policy.verify_grant(&grant, scope).map_err(ProofError::Denied)?;
        if add_to_friends
            && scope.kind == ScopeKind::Direct
            && !policy.grants().iter().any(|existing| {
                existing.subject_agent_id == grant.subject_agent_id
                    && existing.subject_endpoint_id == grant.subject_endpoint_id
            })
        {
            // A new person joins friends, unless the page put them in
            // another group first, as for an approved access request.
            let agent = grant.subject_agent_id.as_str().to_owned();
            let endpoint = grant.subject_endpoint_id.as_str().to_owned();
            self.store.call(move |store| {
                if store.access_group_allows(&agent, &endpoint)? {
                    return Ok(());
                }
                store.add_access_group_member(
                    aether_store::FRIENDS_GROUP,
                    &agent,
                    &endpoint,
                )
            })?;
        }
        policy.replace_grant(grant);
        drop(policy);
        Ok(())
    }

    pub fn has_grant(&self, id: &aether_core::GrantId) -> Result<bool, ProofError> {
        Ok(self.policy.lock().map_err(|_| ProofError::Queue)?.has_grant(id))
    }

    /// Removes the grant `id` from the live policy. A queued task under it
    /// is then rejected by the dispatch-time policy recheck. Returns whether
    /// the grant was present.
    ///
    /// It holds `dispatch_serial`, as the recheck and `mark_running` do: a
    /// task that passed the recheck is running when this returns, and no
    /// task under the grant starts after it.
    pub fn revoke_grant(&self, id: &aether_core::GrantId) -> Result<bool, ProofError> {
        let _dispatch_guard =
            self.dispatch_serial.lock().map_err(|_| ProofError::Queue)?;
        Ok(self.policy.lock().map_err(|_| ProofError::Queue)?.remove_grant(id))
    }

    /// The grants the node admits requests under.
    pub fn grants(&self) -> Result<Vec<aether_core::Grant>, ProofError> {
        Ok(self.policy.lock().map_err(|_| ProofError::Queue)?.grants().to_vec())
    }

    pub fn content_is_expired(&self, task_id: &str) -> Result<bool, ProofError> {
        self.store.call({
            let task_id = task_id.to_owned();
            move |store| store.content_is_expired(&task_id)
        })
    }

    pub fn queue_limits(&self) -> Result<QueueLimits, ProofError> {
        self.limits.lock().map(|limits| *limits).map_err(|_| ProofError::Queue)
    }

    /// Admits a task and queues it for the scheduler. Only the identity
    /// step differs between an Iroh task and a PACT task: both enter the
    /// same queue and limits, and dispatch rechecks the same grant. A
    /// transport request gives a `replay_key`, so that a replayed frame
    /// cannot queue the task again.
    pub fn admit(
        &self,
        input: SubmissionInput<'_>,
        identity: Identity<'_>,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(ProofError::Shutdown);
        }
        let SubmissionInput { request, authenticated_endpoint, idempotency_key, task } =
            input;
        if task.id != request.task_id.as_str()
            || task.workspace_id != request.workspace_id.as_str()
        {
            return Err(ProofError::TaskMismatch);
        }
        let scope = self
            .scopes
            .get(&request.scope_id)
            .ok_or(ProofError::Denied(Denial::ScopeMismatch))?;
        let policy = self.policy.lock().map_err(|_| ProofError::Queue)?;
        let admission = match identity {
            Identity::Claim(claim) => {
                policy.admit(request, claim, scope, authenticated_endpoint)
            }
            Identity::Verified => {
                policy.admit_verified(request, scope, authenticated_endpoint)
            }
        };
        drop(policy);
        if let Admission::Denied(denial) = admission {
            return Err(ProofError::Denied(denial));
        }
        self.check_group(scope, request, authenticated_endpoint)?;
        self.enqueue(request, authenticated_endpoint, idempotency_key, task, replay_key)
    }

    fn check_group(
        &self,
        scope: &Scope,
        request: &Request,
        endpoint: &EndpointId,
    ) -> Result<(), ProofError> {
        if scope.kind == ScopeKind::Direct {
            let agent = request.caller_agent_id.as_str().to_owned();
            let endpoint = endpoint.as_str().to_owned();
            if !self
                .store
                .call(move |store| store.access_group_allows(&agent, &endpoint))?
            {
                return Err(ProofError::GroupDenied);
            }
        }
        Ok(())
    }

    /// Stores an admitted task in the queue and wakes the scheduler.
    fn enqueue(
        &self,
        request: &Request,
        authenticated_endpoint: &EndpointId,
        idempotency_key: &str,
        task: &Task,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        let input = TaskInput {
            request: Request {
                caller_endpoint_id: authenticated_endpoint.clone(),
                ..request.clone()
            },
            idempotency_key: idempotency_key.to_owned(),
            request_fingerprint: request_fingerprint(request, task),
            skill: task.skill.clone(),
            remote_text: task.remote_text.clone(),
        };
        let limits = self.queue_limits()?;
        let admission = {
            let _dispatch_guard =
                self.dispatch_serial.lock().map_err(|_| ProofError::Queue)?;
            self.store.admit_only(input, limits, replay_key)?
        };
        if let StoreAdmission::Accepted(task_id) = &admission {
            let _ = self.scheduler_wake.try_send(());
            let _ = self.event_sender.send(task_id.clone());
        }
        Ok(admission)
    }

    pub fn state(&self, task_id: &str) -> Result<TaskState, ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.state(&task_id))
    }

    pub fn events(
        &self,
        task_id: &str,
    ) -> Result<Vec<aether_store::TaskEvent>, ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.events(&task_id))
    }

    pub fn events_after(
        &self,
        task_id: &str,
        sequence: i64,
    ) -> Result<Vec<aether_store::TaskEvent>, ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.events_after(&task_id, sequence))
    }

    /// Reads at most `limit` events after `sequence` in one bounded store
    /// operation. The returned `bool` is `true` when more events remain.
    pub fn events_page(
        &self,
        task_id: &str,
        sequence: i64,
        limit: usize,
    ) -> Result<(Vec<aether_store::TaskEvent>, bool), ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.events_page(&task_id, sequence, limit))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.event_sender.subscribe()
    }

    pub fn stored_task(
        &self,
        task_id: &str,
    ) -> Result<aether_store::StoredTask, ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.stored_task(&task_id))
    }

    /// Reads a task's current state and final result without loading its
    /// complete event journal.
    pub fn task_snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), ProofError> {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.task_snapshot(&task_id))
    }

    /// Reads a task's snapshot and one page of its events in one store
    /// operation, so a polling caller cannot observe them as of two
    /// different instants.
    pub fn task_progress(
        &self,
        task_id: &str,
        sequence: i64,
        limit: usize,
    ) -> Result<
        (TaskState, Option<String>, Vec<aether_store::TaskEvent>, bool),
        ProofError,
    > {
        let task_id = task_id.to_owned();
        self.store.call(move |store| store.task_progress(&task_id, sequence, limit))
    }

    pub fn cancel(&self, task_id: &str) -> Result<(), ProofError> {
        let task_id = task_id.to_owned();
        {
            let _guard = self.dispatch_serial.lock().map_err(|_| ProofError::Queue)?;
            let stored_task_id = task_id.clone();
            self.store.call(move |store| store.cancel(&stored_task_id))?;
        }
        self.canceller.cancel(&task_id);
        let _ = self.scheduler_wake.try_send(());
        let _ = self.event_sender.send(task_id);
        Ok(())
    }

    pub fn cancel_from(
        &self,
        task_id: &str,
        caller_endpoint: &EndpointId,
        scope_id: &str,
    ) -> Result<(), ProofError> {
        let task_id = task_id.to_owned();
        {
            let _guard = self.dispatch_serial.lock().map_err(|_| ProofError::Queue)?;
            let caller_endpoint = caller_endpoint.as_str().to_owned();
            let scope_id = scope_id.to_owned();
            let stored_task_id = task_id.clone();
            self.store.call(move |store| {
                store.cancel_if_owned(&stored_task_id, &caller_endpoint, &scope_id)
            })?;
        }
        self.canceller.cancel(&task_id);
        let _ = self.scheduler_wake.try_send(());
        let _ = self.event_sender.send(task_id);
        Ok(())
    }
}

const REPLAY_LIMIT: usize = 256;
const REPLAY_TTL: Duration = Duration::from_secs(300);
const RESULT_TTL: Duration = Duration::from_hours(24);
const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEWAL_INTERVAL: Duration = Duration::from_secs(10);
/// The scheduler wakes on each admission, finish and cancellation; this
/// poll is the fallback, and it starts the tasks that a previous run left
/// queued.
const SCHEDULER_POLL: Duration = Duration::from_secs(1);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(30);

fn replay_expiry() -> i64 {
    timestamp_after(REPLAY_TTL)
}

fn result_expiry() -> i64 {
    timestamp_after(RESULT_TTL)
}

fn timestamp_after(duration: Duration) -> i64 {
    SystemTime::now()
        .checked_add(duration)
        .and_then(aether_core::time::unix_timestamp)
        .unwrap_or(i64::MAX)
}

pub(crate) fn request_fingerprint(request: &Request, task: &Task) -> String {
    let fields = [
        request.id.as_str(),
        request.task_id.as_str(),
        request.caller_agent_id.as_str(),
        request.caller_endpoint_id.as_str(),
        request.scope_id.as_str(),
        request.capability_id.as_str(),
        request.workspace_id.as_str(),
        task.id.as_str(),
        task.skill.as_str(),
        task.workspace_id.as_str(),
        task.remote_text.as_str(),
    ];
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"aether.request-fingerprint.v1\0");
    for field in fields {
        // A `u64` holds every `usize` on the platforms Rust supports.
        let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
        fingerprint.update(&length.to_be_bytes());
        fingerprint.update(field.as_bytes());
    }
    fingerprint.update(&task.context_tokens.to_be_bytes());
    fingerprint.finalize().to_hex().to_string()
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use aether_core::{
        AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId,
        Grant, GrantId, ProviderError, Request, RequestId, ScopeId, TaskId,
        WorkspaceId,
    };

    use super::*;

    struct Trusted;

    impl TrustProvider for Trusted {
        fn verify(
            &self,
            _claim: &Claim,
            _authenticated_endpoint: &EndpointId,
            _scope: &Scope,
        ) -> Result<(), ProviderError> {
            Ok(())
        }

        fn verify_grant(
            &self,
            _grant: &Grant,
            _scope: &Scope,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct Cancellable {
        cancelled: Arc<AtomicBool>,
    }

    impl Agent for Cancellable {
        fn start_turn(
            &mut self,
            _task_id: &str,
            _instruction: &str,
            _remote_text: &str,
        ) -> Result<String, ExecutionError> {
            Ok("done".to_owned())
        }

        fn cancel(&self, _task_id: &str) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    #[derive(Clone)]
    struct Blocking {
        active: Arc<AtomicUsize>,
        gate: Arc<AtomicBool>,
        order: Arc<Mutex<Vec<String>>>,
    }

    impl Agent for Blocking {
        fn start_turn(
            &mut self,
            task_id: &str,
            _instruction: &str,
            _remote_text: &str,
        ) -> Result<String, ExecutionError> {
            self.active.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut order) = self.order.lock() {
                order.push(task_id.to_owned());
            }
            while !self.gate.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
            Ok(format!("done:{task_id}"))
        }

        fn cancel(&self, _task_id: &str) {}
    }

    fn test_grant(peer: &str, endpoint: &EndpointId) -> Grant {
        let now = SystemTime::now();
        Grant {
            id: GrantId::new(format!("grant-{peer}")),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new(peer),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            capabilities: BTreeSet::from([CapabilityId::new("review")]),
            workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at: now,
            expires_at: now + Duration::from_secs(3600),
            signing_key: [0; 32],
            nonce: [0; 32],
            signature: Vec::new(),
        }
    }

    fn test_node(
        process: Cancellable,
        limits: QueueLimits,
        workspace: &Path,
    ) -> ProofNode<Trusted, Cancellable> {
        let endpoint = EndpointId::new("endpoint");
        ProofNode::new(
            PolicyEngine::new(Trusted, [test_grant("caller", &endpoint)]),
            NodeStore::in_dir(workspace).expect("store must open"),
            Executor::new(
                process,
                crate::execution::Limits {
                    workspace: workspace.to_path_buf(),
                    max_context_tokens: 32,
                    max_seconds: 120,
                },
            ),
            limits,
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create")
    }

    #[test]
    fn group_membership_controls_direct_admission() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let (request, claim, endpoint, task) = test_request("group-task", "bob");
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [test_grant("bob", &endpoint)]),
            NodeStore::in_dir(&workspace).expect("store must open"),
            Executor::new(
                Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
                crate::execution::Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 32,
                    max_seconds: 5,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        let store = NodeStore::in_dir(&workspace).expect("store must reopen");
        store
            .remove_access_group_member("friends", "bob", endpoint.as_str())
            .expect("access must remove");
        assert!(matches!(
            submit_and_wait(&node, &request, &claim, &endpoint, "key", &task),
            Err(ProofError::GroupDenied)
        ));
        store.create_access_group("work").expect("group must create");
        store
            .add_access_group_member("work", "bob", endpoint.as_str())
            .expect("access must add");
        assert!(matches!(
            submit_and_wait(&node, &request, &claim, &endpoint, "key", &task),
            Ok((TaskState::Succeeded, _))
        ));
        node.shutdown();
    }

    #[test]
    fn a_grant_for_a_member_of_another_group_does_not_join_friends() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let node = test_node(
            Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
            QueueLimits { global: 1, per_peer: 1 },
            &workspace,
        );
        let store = NodeStore::in_dir(&workspace).expect("store must reopen");
        let bob = EndpointId::new("endpoint-bob");
        let carol = EndpointId::new("endpoint-carol");
        store
            .add_access_group_member("requests", "bob", bob.as_str())
            .expect("access must add");
        node.apply_grant(test_grant("bob", &bob)).expect("grant must apply");
        node.apply_grant(test_grant("carol", &carol)).expect("grant must apply");
        let members = |group: &str| {
            store
                .access_groups()
                .expect("groups must read")
                .into_iter()
                .find(|entry| entry.name == group)
                .map(|entry| {
                    entry
                        .members
                        .into_iter()
                        .map(|member| member.agent_id)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(members("requests"), ["bob"]);
        assert_eq!(members("friends"), ["caller", "carol"]);
        node.shutdown();
    }

    /// Admits a task as an Iroh caller and returns its id.
    fn admit_task<T, P>(
        node: &ProofNode<T, P>,
        request: &Request,
        claim: &Claim,
        endpoint: &EndpointId,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<String, ProofError>
    where
        T: TrustProvider + 'static,
        P: Agent + Clone + Send + 'static,
    {
        let input = SubmissionInput {
            request,
            authenticated_endpoint: endpoint,
            idempotency_key,
            task,
        };
        let (StoreAdmission::Accepted(task_id) | StoreAdmission::Existing(task_id)) =
            node.admit(input, Identity::Claim(claim), None)?;
        Ok(task_id)
    }

    /// Admits a task as an Iroh caller and polls its state until the task
    /// ends. Returns the final state and, for a success, the result.
    fn submit_and_wait<T, P>(
        node: &ProofNode<T, P>,
        request: &Request,
        claim: &Claim,
        endpoint: &EndpointId,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<(TaskState, Option<String>), ProofError>
    where
        T: TrustProvider + 'static,
        P: Agent + Clone + Send + 'static,
    {
        let task_id =
            admit_task(node, request, claim, endpoint, idempotency_key, task)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match node.state(&task_id)? {
                TaskState::PendingApproval
                | TaskState::Queued
                | TaskState::Leased
                | TaskState::Running => {
                    // A task that never ends fails the test with this error.
                    if Instant::now() >= deadline {
                        return Err(ProofError::Queue);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                _ => return node.task_snapshot(&task_id),
            }
        }
    }

    /// Polls instead of sleeping a fixed time: a loaded CI runner can take
    /// far longer than any fixed sleep to reach the state under test.
    fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn test_request(id: &str, peer: &str) -> (Request, Claim, EndpointId, Task) {
        let endpoint = EndpointId::new(format!("endpoint-{peer}"));
        let request = Request {
            id: RequestId::new(format!("request-{id}")),
            task_id: TaskId::new(id),
            caller_agent_id: AgentId::new(peer),
            caller_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            capability_id: CapabilityId::new("review"),
            workspace_id: WorkspaceId::new("workspace"),
            access_mode: AccessMode::ReadOnly,
        };
        let claim = Claim {
            id: ClaimId::new("claim"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new(peer),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            issued_at: SystemTime::now(),
            expires_at: SystemTime::now() + Duration::from_secs(60),
            signing_key: [0; 32],
            nonce: [0; 32],
            signature: Vec::new(),
        };
        let task = Task {
            id: id.to_owned(),
            skill: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            remote_text: format!("input-{id}"),
            context_tokens: 1,
        };
        (request, claim, endpoint, task)
    }

    #[derive(Clone)]
    struct BlockingCancellable {
        gate: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
        active: Arc<AtomicUsize>,
    }

    impl Agent for BlockingCancellable {
        fn start_turn(
            &mut self,
            _task_id: &str,
            _instruction: &str,
            _remote_text: &str,
        ) -> Result<String, ExecutionError> {
            self.active.fetch_add(1, Ordering::AcqRel);
            while !self.gate.load(Ordering::Acquire) {
                if self.cancelled.load(Ordering::Acquire) {
                    self.active.fetch_sub(1, Ordering::AcqRel);
                    return Err(ExecutionError::Cancelled);
                }
                thread::sleep(Duration::from_millis(5));
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
            Ok("done".to_owned())
        }

        fn cancel(&self, _task_id: &str) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    #[test]
    fn cancellation_records_state_and_signals_the_adapter() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let cancelled = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let (request_a, claim_a, endpoint_a, task_a) = test_request("task-a", "peer-a");
        let node = Arc::new(
            ProofNode::new(
                PolicyEngine::new(Trusted, [test_grant("peer-a", &endpoint_a)]),
                NodeStore::in_dir(&workspace).expect("store must open"),
                Executor::new(
                    BlockingCancellable {
                        gate: Arc::clone(&gate),
                        cancelled: Arc::clone(&cancelled),
                        active: Arc::clone(&active),
                    },
                    crate::execution::Limits {
                        workspace: workspace.clone(),
                        max_context_tokens: 32,
                        max_seconds: 120,
                    },
                ),
                QueueLimits { global: 2, per_peer: 2 },
                Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .expect("node must create"),
        );
        let handle_a = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_a,
                    &claim_a,
                    &endpoint_a,
                    "key-a",
                    &task_a,
                )
            })
        };
        wait_until("task a to start", || active.load(Ordering::Acquire) == 1);
        let mut event_wake = node.subscribe_events();
        node.store
            .call(move |store| {
                store.admit(
                    &TaskInput {
                        request: aether_core::Request {
                            task_id: aether_core::TaskId::new("task"),
                            id: aether_core::RequestId::new("request"),
                            caller_agent_id: aether_core::AgentId::new("caller"),
                            caller_endpoint_id: aether_core::EndpointId::new(
                                "endpoint",
                            ),
                            scope_id: aether_core::ScopeId::new("direct"),
                            capability_id: aether_core::CapabilityId::new("review"),
                            workspace_id: aether_core::WorkspaceId::new("workspace"),
                            access_mode: aether_core::AccessMode::ReadOnly,
                        },
                        idempotency_key: "key".to_owned(),
                        request_fingerprint: "fingerprint".to_owned(),
                        skill: "review".to_owned(),
                        remote_text: "task input".to_owned(),
                    },
                    QueueLimits { global: 2, per_peer: 2 },
                )
            })
            .expect("task must queue");
        node.cancel("task").expect("task must cancel");
        assert_eq!(event_wake.try_recv().expect("event wake must arrive"), "task");
        assert_eq!(node.state("task").expect("state must load"), TaskState::Cancelled);
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            node.events("task")
                .expect("events must load")
                .into_iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["queued", "cancelled"]
        );
        node.cancel("task").expect("repeat cancel must succeed");
        gate.store(true, Ordering::Release);
        let _ = handle_a.join();
    }

    #[test]
    fn queue_limits_update_in_a_running_node() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let node = test_node(
            Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
            QueueLimits { global: 1, per_peer: 1 },
            &workspace,
        );
        node.set_queue_limit("queue_global", 2).expect("queue limit must update");
        assert_eq!(
            node.queue_limits().expect("queue limits must load"),
            QueueLimits { global: 2, per_peer: 1 }
        );
        node.shutdown();
    }

    #[derive(Clone)]
    struct RecordsWorkspace {
        workspace: Arc<Mutex<Option<PathBuf>>>,
    }

    impl Agent for RecordsWorkspace {
        fn start_turn(
            &mut self,
            _task_id: &str,
            _instruction: &str,
            _remote_text: &str,
        ) -> Result<String, ExecutionError> {
            Ok("done".to_owned())
        }

        fn cancel(&self, _task_id: &str) {}

        fn set_workspace(&self, workspace: &Path) {
            if let Ok(mut current) = self.workspace.lock() {
                *current = Some(workspace.to_path_buf());
            }
        }
    }

    #[test]
    fn apply_workspace_reaches_the_underlying_process() {
        let directory = tempfile::tempdir().expect("directory must create");
        let old_workspace = directory.path().join("old");
        let new_workspace = directory.path().join("new");
        std::fs::create_dir_all(&old_workspace).expect("old workspace must create");
        std::fs::create_dir_all(&new_workspace).expect("new workspace must create");
        let recorded = Arc::new(Mutex::new(None));
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, Vec::<Grant>::new()),
            NodeStore::in_dir(directory.path()).expect("store must open"),
            Executor::new(
                RecordsWorkspace { workspace: Arc::clone(&recorded) },
                crate::execution::Limits {
                    workspace: old_workspace,
                    max_context_tokens: 32,
                    max_seconds: 1,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        // Only Executor::set_workspace ran so far, at construction: nothing
        // has told the process about a workspace change yet.
        assert_eq!(recorded.lock().expect("recorded value must read").as_deref(), None);
        node.apply_workspace(&new_workspace).expect("workspace must apply");
        assert_eq!(
            recorded.lock().expect("recorded workspace must read").as_deref(),
            Some(
                new_workspace
                    .canonicalize()
                    .expect("new workspace must canonicalize")
                    .as_path()
            )
        );
        node.shutdown();
    }

    #[test]
    fn startup_recovers_an_expired_running_lease() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join(aether_store::NODE_DATABASE);
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let mut store = NodeStore::open(&path).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("task"),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("caller"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("direct"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: "fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "task input".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must queue");
        store.lease_next("worker", "execution", 0).expect("task must lease");
        store.mark_running("task", "execution").expect("task must run");
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, Vec::<Grant>::new()),
            store,
            Executor::new(
                Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
                crate::execution::Limits {
                    workspace: directory.path().to_path_buf(),
                    max_context_tokens: 1,
                    max_seconds: 1,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        assert_eq!(
            node.state("task").expect("task state must load"),
            TaskState::UnknownResult
        );
        node.shutdown();
    }

    #[test]
    fn restart_preserves_every_terminal_task_state() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join(aether_store::NODE_DATABASE);
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let mut store = NodeStore::open(&path).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");

        let mut expected = BTreeMap::from([
            ("succeeded-task".to_owned(), TaskState::Succeeded),
            ("failed-task".to_owned(), TaskState::Failed),
            ("rejected-task".to_owned(), TaskState::Rejected),
            ("cancelled-task".to_owned(), TaskState::Cancelled),
            ("unknown-task".to_owned(), TaskState::UnknownResult),
        ]);
        for id in expected.keys() {
            store
                .admit(
                    &TaskInput {
                        request: aether_core::Request {
                            task_id: aether_core::TaskId::new(id.clone()),
                            id: aether_core::RequestId::new(format!("request-{id}")),
                            caller_agent_id: aether_core::AgentId::new("caller"),
                            caller_endpoint_id: aether_core::EndpointId::new(
                                "endpoint",
                            ),
                            scope_id: aether_core::ScopeId::new("direct"),
                            capability_id: aether_core::CapabilityId::new("review"),
                            workspace_id: aether_core::WorkspaceId::new("workspace"),
                            access_mode: aether_core::AccessMode::ReadOnly,
                        },
                        idempotency_key: format!("key-{id}"),
                        request_fingerprint: format!("fingerprint-{id}"),
                        skill: "review".to_owned(),
                        remote_text: "task input".to_owned(),
                    },
                    QueueLimits { global: 10, per_peer: 10 },
                )
                .expect("task must admit");
        }
        // Drive each admitted task to its target terminal state before the
        // "restart" below, using whichever task `lease_next` actually
        // returns rather than assuming admission order, since fairness
        // selection is not this test's concern.
        for index in 0..expected.len() {
            let execution_id = format!("execution-{index}");
            let task_id = store
                .lease_next("worker", &execution_id, 2_000_000_000)
                .expect("lease must succeed")
                .expect("an admitted task must still be queued");
            let target = expected
                .remove(&task_id)
                .expect("the leased task must be one this test admitted");
            match target {
                TaskState::Succeeded => {
                    store.mark_running(&task_id, &execution_id).expect("task must run");
                    store
                        .mark_succeeded(&task_id, &execution_id, "done", i64::MAX)
                        .expect("task must succeed");
                }
                TaskState::Failed => {
                    store.mark_running(&task_id, &execution_id).expect("task must run");
                    store.mark_failed(&task_id, &execution_id).expect("task must fail");
                }
                TaskState::Rejected => {
                    store
                        .mark_rejected(&task_id, &execution_id)
                        .expect("task must reject");
                }
                TaskState::Cancelled => {
                    store.cancel(&task_id).expect("task must cancel");
                }
                TaskState::UnknownResult => {
                    store.mark_running(&task_id, &execution_id).expect("task must run");
                    store
                        .mark_unknown_result(&task_id, &execution_id)
                        .expect("task must become unknown");
                }
                other => panic!("test does not expect target state {other:?}"),
            }
            expected.insert(task_id, target);
        }
        drop(store);

        // The "restart": a fresh store handle and a fresh `ProofNode`, not
        // the same live objects the setup above used.
        let store = NodeStore::open(&path).expect("store must reopen after restart");
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, Vec::<Grant>::new()),
            store,
            Executor::new(
                Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
                crate::execution::Limits {
                    workspace: directory.path().to_path_buf(),
                    max_context_tokens: 32,
                    max_seconds: 1,
                },
            ),
            QueueLimits { global: 10, per_peer: 10 },
            scope,
        )
        .expect("node must create after restart");
        for (id, state) in &expected {
            assert_eq!(
                node.state(id).expect("restarted task state must load"),
                *state,
                "task {id} did not keep its terminal state across restart"
            );
        }
        node.shutdown();
    }

    #[test]
    fn scheduler_executes_two_tasks_in_order_with_one_slot() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let gate = Arc::new(AtomicBool::new(false));
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let blocking = Blocking {
            active: Arc::clone(&active),
            gate: Arc::clone(&gate),
            order: Arc::clone(&order),
        };
        let (request_a, claim_a, endpoint_a, task_a) = test_request("task-a", "peer-a");
        let (request_b, claim_b, endpoint_b, task_b) = test_request("task-b", "peer-b");
        let node = Arc::new(
            ProofNode::new(
                PolicyEngine::new(
                    Trusted,
                    [
                        test_grant("peer-a", &endpoint_a),
                        test_grant("peer-b", &endpoint_b),
                    ],
                ),
                NodeStore::in_dir(&workspace).expect("store must open"),
                Executor::new(
                    blocking,
                    crate::execution::Limits {
                        workspace: workspace.clone(),
                        max_context_tokens: 32,
                        max_seconds: 120,
                    },
                ),
                QueueLimits { global: 4, per_peer: 2 },
                Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .expect("node must create"),
        );
        let handle_a = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_a,
                    &claim_a,
                    &endpoint_a,
                    "key-a",
                    &task_a,
                )
            })
        };
        wait_until("task a to start", || active.load(Ordering::Acquire) == 1);
        let handle_b = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_b,
                    &claim_b,
                    &endpoint_b,
                    "key-b",
                    &task_b,
                )
            })
        };
        wait_until("task b to queue", || {
            node.state("task-b").is_ok_and(|state| state == TaskState::Queued)
        });
        assert_eq!(active.load(Ordering::Acquire), 1);
        gate.store(true, Ordering::Release);
        let result_a =
            handle_a.join().expect("thread a must join").expect("task a must complete");
        let result_b =
            handle_b.join().expect("thread b must join").expect("task b must complete");
        assert_eq!(result_a, (TaskState::Succeeded, Some("done:task-a".to_owned())));
        assert_eq!(result_b, (TaskState::Succeeded, Some("done:task-b".to_owned())));
        let execution_order = order.lock().expect("order must lock").clone();
        assert_eq!(execution_order, vec!["task-a".to_owned(), "task-b".to_owned()]);
        node.shutdown();
    }

    #[test]
    fn cancellation_before_start_prevents_execution() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let gate = Arc::new(AtomicBool::new(false));
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let (request_a, claim_a, endpoint_a, task_a) = test_request("task-a", "peer-a");
        let (request_b, claim_b, endpoint_b, task_b) = test_request("task-b", "peer-b");
        let node = Arc::new(
            ProofNode::new(
                PolicyEngine::new(
                    Trusted,
                    [
                        test_grant("peer-a", &endpoint_a),
                        test_grant("peer-b", &endpoint_b),
                    ],
                ),
                NodeStore::in_dir(&workspace).expect("store must open"),
                Executor::new(
                    Blocking {
                        active: Arc::clone(&active),
                        gate: Arc::clone(&gate),
                        order: Arc::clone(&order),
                    },
                    crate::execution::Limits {
                        workspace: workspace.clone(),
                        max_context_tokens: 32,
                        max_seconds: 120,
                    },
                ),
                QueueLimits { global: 2, per_peer: 2 },
                Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .expect("node must create"),
        );
        let handle_a = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_a,
                    &claim_a,
                    &endpoint_a,
                    "key-a",
                    &task_a,
                )
            })
        };
        wait_until("task a to start", || active.load(Ordering::Acquire) == 1);
        let handle_b = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_b,
                    &claim_b,
                    &endpoint_b,
                    "key-b",
                    &task_b,
                )
            })
        };
        wait_until("task b to queue", || {
            node.state("task-b").is_ok_and(|state| state == TaskState::Queued)
        });
        node.cancel("task-b").expect("task b must cancel");
        gate.store(true, Ordering::Release);
        handle_a.join().expect("thread a must join").expect("task a must complete");
        let result_b = handle_b.join().expect("thread b must join");
        assert!(matches!(result_b, Ok((TaskState::Cancelled, None))), "{result_b:?}");
        let execution_order = order.lock().expect("order must lock").clone();
        assert_eq!(execution_order, vec!["task-a".to_owned()]);
        node.cancel("task-b").expect("repeat cancel must succeed");
        assert_eq!(
            node.state("task-b").expect("state must load"),
            TaskState::Cancelled
        );
    }

    #[test]
    fn shutdown_stops_active_execution_and_preserves_waiting_tasks() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let db_path = workspace.join(aether_store::NODE_DATABASE);
        let gate = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let (request_a, claim_a, endpoint_a, task_a) = test_request("task-a", "peer-a");
        let (request_b, claim_b, endpoint_b, task_b) = test_request("task-b", "peer-b");
        let node = Arc::new(
            ProofNode::new(
                PolicyEngine::new(
                    Trusted,
                    [
                        test_grant("peer-a", &endpoint_a),
                        test_grant("peer-b", &endpoint_b),
                    ],
                ),
                NodeStore::open(&db_path).expect("store must open"),
                Executor::new(
                    BlockingCancellable {
                        gate: Arc::clone(&gate),
                        cancelled: Arc::clone(&cancelled),
                        active: Arc::clone(&active),
                    },
                    crate::execution::Limits {
                        workspace,
                        max_context_tokens: 32,
                        max_seconds: 120,
                    },
                ),
                QueueLimits { global: 4, per_peer: 2 },
                Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .expect("node must create"),
        );
        admit_task(&node, &request_a, &claim_a, &endpoint_a, "key-a", &task_a)
            .expect("task a must admit");
        wait_until("task a to start", || active.load(Ordering::Acquire) == 1);
        admit_task(&node, &request_b, &claim_b, &endpoint_b, "key-b", &task_b)
            .expect("task b must admit");
        node.shutdown();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(matches!(
            admit_task(&node, &request_b, &claim_b, &endpoint_b, "key-b", &task_b),
            Err(ProofError::Shutdown)
        ));
        let store = NodeStore::open(&db_path).expect("store must reopen");
        assert_eq!(
            store.state("task-a").expect("task a must stay durable"),
            TaskState::Cancelled
        );
        assert_eq!(
            store.state("task-b").expect("task b must stay durable"),
            TaskState::Queued
        );
    }

    #[test]
    fn configured_node_admits_a_task_in_a_non_primary_scope() {
        let temporary = tempfile::tempdir().expect("directory must create");
        let path = temporary.path().join(aether_store::NODE_DATABASE);
        let now = SystemTime::now();
        let direct = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let directory =
            Scope { id: ScopeId::new("directory"), kind: ScopeKind::Directory };
        let endpoint = EndpointId::new("endpoint");
        let grant = Grant {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new("caller"),
            subject_endpoint_id: endpoint.clone(),
            scope_id: directory.id.clone(),
            capabilities: BTreeSet::from([CapabilityId::new("review")]),
            workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at: now,
            expires_at: now + Duration::from_secs(60),
            signing_key: [0; 32],
            nonce: [0; 32],
            signature: Vec::new(),
        };
        let claim = Claim {
            id: ClaimId::new("claim"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new("caller"),
            subject_endpoint_id: endpoint.clone(),
            scope_id: directory.id.clone(),
            issued_at: now,
            expires_at: now + Duration::from_secs(60),
            signing_key: [0; 32],
            nonce: [0; 32],
            signature: Vec::new(),
        };
        let executor = Executor::new(
            Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
            crate::execution::Limits {
                workspace: temporary.path().to_path_buf(),
                max_context_tokens: 32,
                max_seconds: 1,
            },
        );
        let node = ProofNode::with_scopes(
            PolicyEngine::new(Trusted, [grant]),
            NodeStore::open(&path).expect("store must open"),
            executor,
            QueueLimits { global: 1, per_peer: 1 },
            [direct, directory],
        )
        .expect("node must create");
        let request = Request {
            id: RequestId::new("request"),
            task_id: TaskId::new("task"),
            caller_agent_id: AgentId::new("caller"),
            caller_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("directory"),
            capability_id: CapabilityId::new("review"),
            workspace_id: WorkspaceId::new("workspace"),
            access_mode: AccessMode::ReadOnly,
        };
        let task = Task {
            id: "task".to_owned(),
            skill: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            remote_text: "task text".to_owned(),
            context_tokens: 1,
        };
        let result = submit_and_wait(&node, &request, &claim, &endpoint, "key", &task);
        assert!(
            matches!(
                result,
                Ok((TaskState::Succeeded, Some(ref result))) if result == "done"
            ),
            "{result:?}"
        );
        node.shutdown();
    }

    #[derive(Clone)]
    struct FailsAfterStart;

    impl Agent for FailsAfterStart {
        fn start_turn(
            &mut self,
            _task_id: &str,
            _instruction: &str,
            _remote_text: &str,
        ) -> Result<String, ExecutionError> {
            Err(ExecutionError::Protocol)
        }

        fn cancel(&self, _task_id: &str) {}
    }

    #[test]
    fn a_failure_after_confirmed_start_becomes_unknown_result() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let (request, claim, endpoint, task) = test_request("task", "peer");
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [test_grant("peer", &endpoint)]),
            NodeStore::in_dir(&workspace).expect("store must open"),
            Executor::new(
                FailsAfterStart,
                crate::execution::Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 32,
                    max_seconds: 5,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        let result = submit_and_wait(&node, &request, &claim, &endpoint, "key", &task);
        // The process was confirmed started before it failed, so the outcome
        // is genuinely unknown and must wait for operator review instead of
        // being silently discarded as a plain failure.
        assert!(matches!(result, Ok((TaskState::UnknownResult, None))), "{result:?}");
        node.shutdown();
    }

    #[test]
    fn a_grant_revoked_while_queued_rejects_the_task_at_dispatch() {
        grant_withdrawn_while_queued_rejects_the_task(|node, grant_b, _| {
            node.apply_grant(Grant { capabilities: BTreeSet::new(), ..grant_b })
                .expect("grant replacement must apply");
        });
    }

    #[test]
    fn a_grant_removed_while_queued_rejects_the_task_at_dispatch() {
        grant_withdrawn_while_queued_rejects_the_task(|node, grant_b, _| {
            assert!(node.revoke_grant(&grant_b.id).expect("grant must revoke"));
        });
    }

    #[test]
    fn removing_a_group_member_rejects_queued_work() {
        grant_withdrawn_while_queued_rejects_the_task(|_, grant, workspace| {
            NodeStore::in_dir(workspace)
                .expect("store must open")
                .remove_access_group_member(
                    "friends",
                    grant.subject_agent_id.as_str(),
                    grant.subject_endpoint_id.as_str(),
                )
                .expect("group member must remove");
        });
    }

    /// Queues task-b behind a running task-a, withdraws task-b's grant with
    /// `withdraw`, and checks that dispatch rejects task-b.
    fn grant_withdrawn_while_queued_rejects_the_task(
        withdraw: impl FnOnce(&ProofNode<Trusted, Blocking>, Grant, &Path),
    ) {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let gate = Arc::new(AtomicBool::new(false));
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let (request_a, claim_a, endpoint_a, task_a) = test_request("task-a", "peer-a");
        let (request_b, claim_b, endpoint_b, task_b) = test_request("task-b", "peer-b");
        let grant_b = test_grant("peer-b", &endpoint_b);
        let node = Arc::new(
            ProofNode::new(
                PolicyEngine::new(
                    Trusted,
                    [test_grant("peer-a", &endpoint_a), grant_b.clone()],
                ),
                NodeStore::in_dir(&workspace).expect("store must open"),
                Executor::new(
                    Blocking {
                        active: Arc::clone(&active),
                        gate: Arc::clone(&gate),
                        order: Arc::clone(&order),
                    },
                    crate::execution::Limits {
                        workspace: workspace.clone(),
                        max_context_tokens: 32,
                        max_seconds: 120,
                    },
                ),
                QueueLimits { global: 2, per_peer: 2 },
                Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .expect("node must create"),
        );
        let handle_a = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_a,
                    &claim_a,
                    &endpoint_a,
                    "key-a",
                    &task_a,
                )
            })
        };
        wait_until("task a to start", || active.load(Ordering::Acquire) == 1);
        let handle_b = {
            let node = Arc::clone(&node);
            thread::spawn(move || {
                submit_and_wait(
                    &node,
                    &request_b,
                    &claim_b,
                    &endpoint_b,
                    "key-b",
                    &task_b,
                )
            })
        };
        wait_until("task b to queue", || {
            node.state("task-b").is_ok_and(|state| state == TaskState::Queued)
        });
        // Withdraw task-b's grant while it still sits queued behind task-a;
        // dispatch must resolve it to `rejected`, not leave it stuck.
        withdraw(&node, grant_b, &workspace);
        gate.store(true, Ordering::Release);
        handle_a.join().expect("thread a must join").expect("task a must complete");
        let result_b = handle_b.join().expect("thread b must join");
        assert!(matches!(result_b, Ok((TaskState::Rejected, None))), "{result_b:?}");
        let execution_order = order.lock().expect("order must lock").clone();
        assert_eq!(execution_order, vec!["task-a".to_owned()]);
        node.shutdown();
    }
}
