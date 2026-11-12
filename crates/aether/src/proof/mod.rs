use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use aether_core::{
    Claim, Denial, EndpointId, PolicyEngine, Request, Scope, ScopeKind, TrustProvider,
    limits::QueueLimitId,
};
use aether_store::{NodeStore, QueueLimits, StoreError, TaskState};
use tokio::sync::broadcast;

use crate::execution::{Agent, ExecutionError, Executor, Task, Workspace};

#[derive(Debug)]
pub enum ProofError {
    Denied(Denial),
    /// No access group of a direct scope holds the caller.
    GroupDenied,
    Store(StoreError),
    Execution(ExecutionError),
    TaskMismatch,
    Internal,
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

pub(crate) const REPLAY_TTL: Duration = Duration::from_secs(300);
/// The replay entries that the node keeps for one endpoint and scope. An
/// entry stays for `REPLAY_TTL`, and a full table refuses the next frame, so
/// the table has room for every frame that the peer rate limit lets through
/// in that time: its burst and then `PEER_REQUESTS_PER_SECOND` each second.
pub(crate) const REPLAY_LIMIT: usize = 2408;
pub(crate) const RESULT_TTL: Duration = Duration::from_hours(24);
pub(crate) const LEASE_TTL: Duration = Duration::from_secs(30);
pub(crate) const LEASE_RENEWAL_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const SCHEDULER_POLL: Duration = Duration::from_secs(1);
pub(crate) const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(30);

pub(crate) fn replay_expiry() -> i64 {
    timestamp_after(REPLAY_TTL)
}

pub(crate) fn result_expiry() -> i64 {
    timestamp_after(RESULT_TTL)
}

pub(crate) fn timestamp_after(duration: Duration) -> i64 {
    SystemTime::now()
        .checked_add(duration)
        .and_then(aether_core::time::unix_timestamp)
        .unwrap_or(i64::MAX)
}

mod admission;
mod scheduler;
mod worker;

use scheduler::SchedulerContext;
use worker::StoreWorker;

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
        let scope = scopes.values().next().cloned().ok_or(ProofError::Internal)?;
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
            .map_err(|_| ProofError::Internal)?;
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

    pub fn set_queue_limit(
        &self,
        id: QueueLimitId,
        limit: u32,
    ) -> Result<(), ProofError> {
        let limit = usize::try_from(limit).map_err(|_| ProofError::Internal)?;
        let mut limits = self.limits.lock().map_err(|_| ProofError::Internal)?;
        match id {
            QueueLimitId::Global => limits.global = limit,
            QueueLimitId::PerPeer => limits.per_peer = limit,
        }
        drop(limits);
        Ok(())
    }

    pub fn current_queue_limit(&self, id: QueueLimitId) -> Result<u32, ProofError> {
        let limits = self.queue_limits()?;
        let limit = match id {
            QueueLimitId::Global => limits.global,
            QueueLimitId::PerPeer => limits.per_peer,
        };
        u32::try_from(limit).map_err(|_| ProofError::Internal)
    }

    /// Moves the executor and its agent to `path`. The executor
    /// canonicalizes the path; `workspace` then reads the new value.
    pub fn apply_workspace(&self, path: &Path) -> Result<(), ProofError> {
        self.executor.lock().map_err(|_| ProofError::Internal)?.set_workspace(path)?;
        Ok(())
    }

    pub fn workspace(&self) -> Result<PathBuf, ProofError> {
        self.workspace.path().map_err(|_| ProofError::Internal)
    }

    #[cfg(test)]
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
        let mut policy = self.policy.lock().map_err(|_| ProofError::Internal)?;
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
        Ok(self.policy.lock().map_err(|_| ProofError::Internal)?.has_grant(id))
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
            self.dispatch_serial.lock().map_err(|_| ProofError::Internal)?;
        Ok(self.policy.lock().map_err(|_| ProofError::Internal)?.remove_grant(id))
    }

    pub fn grants(&self) -> Result<Vec<aether_core::Grant>, ProofError> {
        Ok(self.policy.lock().map_err(|_| ProofError::Internal)?.grants().to_vec())
    }

    pub fn content_is_expired(&self, task_id: &str) -> Result<bool, ProofError> {
        self.store.call({
            let task_id = task_id.to_owned();
            move |store| store.content_is_expired(&task_id)
        })
    }

    pub fn queue_limits(&self) -> Result<QueueLimits, ProofError> {
        self.limits.lock().map(|limits| *limits).map_err(|_| ProofError::Internal)
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
    pub fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.event_sender.subscribe()
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
            let _guard =
                self.dispatch_serial.lock().map_err(|_| ProofError::Internal)?;
            let stored_task_id = task_id.clone();
            self.store.call(move |store| store.cancel(&stored_task_id))?;
        }
        self.canceller.cancel(&task_id);
        let _ = self.scheduler_wake.try_send(());
        let _ = self.event_sender.send(task_id);
        Ok(())
    }

    /// Cancels the task that `caller_endpoint` sent as `task_id`, and returns
    /// the ID that the node keeps it under.
    pub fn cancel_from(
        &self,
        task_id: &str,
        caller_endpoint: &EndpointId,
        scope_id: &str,
    ) -> Result<String, ProofError> {
        let task_id = {
            let _guard =
                self.dispatch_serial.lock().map_err(|_| ProofError::Internal)?;
            let caller_endpoint = caller_endpoint.as_str().to_owned();
            let scope_id = scope_id.to_owned();
            let sent_task_id = task_id.to_owned();
            self.store.call(move |store| {
                store.cancel_if_owned(&sent_task_id, &caller_endpoint, &scope_id)
            })?
        };
        self.canceller.cancel(&task_id);
        let _ = self.scheduler_wake.try_send(());
        let _ = self.event_sender.send(task_id.clone());
        Ok(task_id)
    }
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

    use aether_store::{TaskAdmission as StoreAdmission, TaskInput};

    use aether_core::{
        AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId,
        Grant, GrantId, Request, RequestId, ScopeId, TaskId, WorkspaceId,
    };

    use super::*;
    use crate::{
        execution::{ExecutionEvent, TurnContext},
        test_support::Trusted,
    };

    #[test]
    fn the_replay_table_holds_every_frame_that_the_rate_limit_lets_through() {
        let frames = u64::from(crate::transport::PEER_REQUESTS_PER_SECOND)
            * (REPLAY_TTL.as_secs() + 1);
        assert!(u64::try_from(REPLAY_LIMIT).expect("limit fits") >= frames);
    }

    #[derive(Clone)]
    struct Cancellable {
        cancelled: Arc<AtomicBool>,
    }

    impl Agent for Cancellable {
        fn start_turn_with_events(
            &mut self,
            _ctx: &TurnContext,
            events: &mut dyn FnMut(ExecutionEvent),
        ) -> Result<String, ExecutionError> {
            events(ExecutionEvent::Completed);
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
        fn start_turn_with_events(
            &mut self,
            ctx: &TurnContext,
            events: &mut dyn FnMut(ExecutionEvent),
        ) -> Result<String, ExecutionError> {
            self.active.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut order) = self.order.lock() {
                order.push(ctx.task_id.clone());
            }
            while !self.gate.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
            events(ExecutionEvent::Completed);
            Ok(format!("done:{}", ctx.task_id))
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
        node.apply_stored_grant(test_grant("bob", &bob)).expect("grant must apply");
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
                        return Err(ProofError::Internal);
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
        fn start_turn_with_events(
            &mut self,
            _ctx: &TurnContext,
            events: &mut dyn FnMut(ExecutionEvent),
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
            events(ExecutionEvent::Completed);
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
        node.set_queue_limit(aether_core::limits::QueueLimitId::Global, 2)
            .expect("queue limit must update");
        assert_eq!(
            node.queue_limits().expect("queue limits must load"),
            QueueLimits { global: 2, per_peer: 1 }
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
                    max_context_tokens: 32,
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
        fn start_turn_with_events(
            &mut self,
            _ctx: &TurnContext,
            _events: &mut dyn FnMut(ExecutionEvent),
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
            node.apply_stored_grant(Grant { capabilities: BTreeSet::new(), ..grant_b })
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

    #[test]
    fn invalid_workspace_id_at_lease_settles_failed() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().to_path_buf();
        let (mut request, claim, endpoint, mut task) = test_request("task", "peer");
        request.workspace_id = WorkspaceId::new("../outside");
        task.workspace_id = "../outside".to_owned();
        let mut grant = test_grant("peer", &endpoint);
        grant.workspaces.insert(WorkspaceId::new("../outside"));
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [grant]),
            NodeStore::in_dir(&workspace).expect("store must open"),
            Executor::new(
                Cancellable { cancelled: Arc::new(AtomicBool::new(false)) },
                crate::execution::Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 32,
                    max_seconds: 120,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        node.admit(
            SubmissionInput {
                request: &request,
                authenticated_endpoint: &endpoint,
                idempotency_key: "once",
                task: &task,
            },
            Identity::Claim(&claim),
            None,
        )
        .expect("task must admit");
        wait_until("task fails", || {
            node.state("task").is_ok_and(|state| state == TaskState::Failed)
        });
        assert_ne!(node.state("task").expect("state must load"), TaskState::Leased);
        node.shutdown();
    }
}
