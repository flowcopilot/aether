use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use aether_core::{
    Denial, PolicyEngine, Scope, ScopeId, ScopeKind, TrustProvider, WorkspaceId,
};
use aether_store::{NodeStore, StoreError, TaskExecutionData, TaskState};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::execution::{
    Agent, ExecutionError, ExecutionEvent, Executor, Task, Workspace,
};

use super::{
    LEASE_RENEWAL_INTERVAL, LEASE_TTL, MAINTENANCE_INTERVAL, ProofError,
    SCHEDULER_POLL, result_expiry, timestamp_after, worker::StoreWorker,
};

struct ExecutionBundle {
    task_id: String,
    execution_id: String,
    task: Task,
}

enum DispatchBlock {
    Policy(Denial),
    Group,
}

#[derive(Debug, Eq, PartialEq)]
enum TaskOutcome {
    Succeeded(String),
    Failed,
}

/// What one run observed, in the terms that decide how it settles.
struct RunReport {
    result: Result<String, ExecutionError>,
    /// The lease renewal failed, so the lease may belong to recovery.
    renewal_failed: bool,
    /// A progress event could not be written to the journal.
    event_error: bool,
    /// The start closure already wrote the task's next state (a rejection
    /// or a requeue), so nothing is left to settle.
    settled_at_start: bool,
}

/// The single store write that ends a run, and the label its error is
/// logged under.
#[derive(Debug, Eq, PartialEq)]
struct Settlement {
    label: &'static str,
    outcome: TaskOutcome,
}

/// Decides how a run settles. `cancelled` reads whether the task was
/// cancelled; it is consulted only after a renewal failure, where the
/// process was stopped by the renewal thread or by a cancel.
fn settlement(run: RunReport, cancelled: impl FnOnce() -> bool) -> Option<Settlement> {
    let settle = |label, outcome| Some(Settlement { label, outcome });
    if run.settled_at_start {
        return None;
    }
    if run.renewal_failed {
        return match run.result {
            Ok(text) => {
                settle("late success after renewal loss", TaskOutcome::Succeeded(text))
            }
            Err(_) if cancelled() => None,
            Err(_) => settle("renewal failed", TaskOutcome::Failed),
        };
    }
    if run.event_error {
        return settle("event journal write", TaskOutcome::Failed);
    }
    match run.result {
        Ok(text) => settle("succeeded", TaskOutcome::Succeeded(text)),
        Err(ExecutionError::Cancelled) => None,
        Err(_) => settle("execution failed", TaskOutcome::Failed),
    }
}

/// Holds the scheduler's single execution slot for one task. Dropping it
/// clears the active task if it still names this one, frees the slot and
/// wakes the scheduler, on every exit of the run.
struct ExecutionSlot {
    executing: Arc<AtomicBool>,
    active_task: Arc<Mutex<Option<String>>>,
    scheduler_wake: SyncSender<()>,
    task_id: String,
}

impl ExecutionSlot {
    fn activate(&self) {
        if let Ok(mut active) = self.active_task.lock() {
            *active = Some(self.task_id.clone());
        }
    }

    fn deactivate(&self) {
        if let Ok(mut active) = self.active_task.lock()
            && active.as_deref() == Some(self.task_id.as_str())
        {
            *active = None;
        }
    }
}

impl Drop for ExecutionSlot {
    fn drop(&mut self) {
        self.deactivate();
        self.executing.store(false, Ordering::Release);
        let _ = self.scheduler_wake.try_send(());
    }
}

pub(in crate::proof) struct SchedulerContext<T, P> {
    pub(in crate::proof) store: StoreWorker,
    pub(in crate::proof) executor: Arc<Mutex<Executor<P>>>,
    pub(in crate::proof) canceller: P,
    pub(in crate::proof) event_sender: broadcast::Sender<String>,
    pub(in crate::proof) shutdown: Arc<AtomicBool>,
    pub(in crate::proof) executing: Arc<AtomicBool>,
    pub(in crate::proof) active_task: Arc<Mutex<Option<String>>>,
    pub(in crate::proof) execution_thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub(in crate::proof) workspace: Workspace,
    pub(in crate::proof) dispatch_serial: Arc<Mutex<()>>,
    pub(in crate::proof) scheduler_wake: SyncSender<()>,
    pub(in crate::proof) policy: Arc<Mutex<PolicyEngine<T>>>,
    pub(in crate::proof) scopes: BTreeMap<ScopeId, Scope>,
}

impl<T, P: Clone> Clone for SchedulerContext<T, P> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            executor: Arc::clone(&self.executor),
            canceller: self.canceller.clone(),
            event_sender: self.event_sender.clone(),
            shutdown: Arc::clone(&self.shutdown),
            executing: Arc::clone(&self.executing),
            active_task: Arc::clone(&self.active_task),
            execution_thread: Arc::clone(&self.execution_thread),
            workspace: self.workspace.clone(),
            dispatch_serial: Arc::clone(&self.dispatch_serial),
            scheduler_wake: self.scheduler_wake.clone(),
            policy: Arc::clone(&self.policy),
            scopes: self.scopes.clone(),
        }
    }
}

impl<T, P> SchedulerContext<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    fn dispatch_allowed(&self, data: &TaskExecutionData) -> Result<(), DispatchBlock> {
        let policy = self.policy.lock().map_err(|_| DispatchBlock::Group)?;
        let scope = self
            .scopes
            .get(&data.request.scope_id)
            .ok_or(DispatchBlock::Policy(Denial::ScopeMismatch))?;
        policy.permits_dispatch(&data.request, scope).map_err(DispatchBlock::Policy)?;
        drop(policy);
        if scope.kind == ScopeKind::Direct {
            let agent = data.request.caller_agent_id.as_str().to_owned();
            let endpoint = data.request.caller_endpoint_id.as_str().to_owned();
            let allowed = self
                .store
                .call(move |store| store.access_group_allows(&agent, &endpoint))
                .map_err(|_| DispatchBlock::Group)?;
            if !allowed {
                return Err(DispatchBlock::Group);
            }
        }
        Ok(())
    }

    fn log_store(label: &'static str, result: Result<(), ProofError>) {
        if let Err(error) = result {
            eprintln!("aether: scheduler {label}: {error:?}");
        }
    }

    fn notify_task(&self, task_id: &str) {
        // `send` fails only when no receiver is subscribed. That is normal:
        // a poll that nobody waits on needs no wake-up.
        let _ = self.event_sender.send(task_id.to_owned());
    }

    /// Ends a leased task that will not run with `end`
    /// (`NodeStore::mark_rejected` or `NodeStore::mark_failed`).
    fn end_leased(
        &self,
        task_id: &str,
        execution_id: &str,
        end: fn(&mut NodeStore, &str, &str) -> Result<(), StoreError>,
    ) -> Result<(), ProofError> {
        let task_id = task_id.to_owned();
        let execution_id = execution_id.to_owned();
        let notify = task_id.clone();
        self.store.call(move |store| end(store, &task_id, &execution_id))?;
        self.notify_task(&notify);
        Ok(())
    }

    fn settle(
        &self,
        task_id: &str,
        execution_id: &str,
        start_confirmed: bool,
        outcome: TaskOutcome,
    ) -> Result<(), ProofError> {
        let task_id = task_id.to_owned();
        let execution_id = execution_id.to_owned();
        self.store.call(move |store| match outcome {
            TaskOutcome::Succeeded(text) => {
                store.mark_succeeded(&task_id, &execution_id, &text, result_expiry())
            }
            TaskOutcome::Failed => {
                if start_confirmed {
                    store.mark_unknown_result(&task_id, &execution_id)
                } else {
                    store.mark_failed(&task_id, &execution_id)
                }
            }
        })
    }

    fn prepare_execution(&self) -> Result<Option<ExecutionBundle>, ProofError> {
        let _dispatch_guard =
            self.dispatch_serial.lock().map_err(|_| ProofError::Internal)?;
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
        // A task cancelled after it was leased has no lease left to return.
        if state != TaskState::Leased {
            return Ok(None);
        }
        let execution_data = self.store.call({
            let task_id = task_id.clone();
            move |store| store.task_execution_data(&task_id)
        })?;
        let workspace_id = execution_data.request.workspace_id.as_str();
        if !WorkspaceId::allows_path_token(workspace_id) {
            Self::log_store(
                "invalid workspace id",
                self.end_leased(&task_id, &execution_id, NodeStore::mark_failed),
            );
            return Ok(None);
        }
        match self.dispatch_allowed(&execution_data) {
            Ok(()) => {}
            Err(DispatchBlock::Policy(denial)) => {
                Self::log_store(
                    "reject leased",
                    self.end_leased(&task_id, &execution_id, NodeStore::mark_rejected),
                );
                return Err(ProofError::Denied(denial));
            }
            Err(DispatchBlock::Group) => {
                Self::log_store(
                    "reject leased",
                    self.end_leased(&task_id, &execution_id, NodeStore::mark_rejected),
                );
                return Err(ProofError::GroupDenied);
            }
        }
        let task = Task {
            id: task_id.clone(),
            skill: execution_data.skill,
            workspace_id: execution_data.request.workspace_id.as_str().to_owned(),
            remote_text: execution_data.remote_text,
        };
        Ok(Some(ExecutionBundle { task_id, execution_id, task }))
    }

    fn run_execution(&self, bundle: ExecutionBundle) {
        let ExecutionBundle { task_id, execution_id, task } = bundle;
        let slot = ExecutionSlot {
            executing: Arc::clone(&self.executing),
            active_task: Arc::clone(&self.active_task),
            scheduler_wake: self.scheduler_wake.clone(),
            task_id: task_id.clone(),
        };
        if self.shutdown.load(Ordering::Acquire) {
            // Shutdown landed before this worker could register as the
            // active task or touch the executor. The lease was never
            // confirmed as started, so the task can go straight back to the
            // queue for a future node run instead of being marked failed.
            let _ = self.store.call(move |store| store.requeue_leased(&task_id));
            return;
        }
        slot.activate();
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
                                    break;
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
                return;
            }
        };
        let executor = Arc::clone(&self.executor);
        let store = self.store.clone();
        let event_sender = self.event_sender.clone();
        if self.workspace.path().is_err() {
            Self::log_store(
                "workspace path",
                self.end_leased(&task_id, &execution_id, NodeStore::mark_failed),
            );
            return;
        }
        let dispatch_task_id = task_id.clone();
        let dispatch_ctx = self.clone();
        let start_store = self.store.clone();
        let dispatch_serial = Arc::clone(&self.dispatch_serial);
        let shutdown = Arc::clone(&self.shutdown);
        let start_execution_id = execution_id.clone();
        let start_confirmed = Arc::new(AtomicBool::new(false));
        let confirmed_flag = Arc::clone(&start_confirmed);
        let mut event_error = false;
        let mut settled_at_start = false;
        let result = executor.lock().map_err(|_| ExecutionError::Unavailable).and_then(
            |mut executor| {
                executor.execute_with_confirmed_start(
                    &task,
                    &mut || {
                        let _dispatch_guard = dispatch_serial
                            .lock()
                            .map_err(|_| ExecutionError::Unavailable)?;
                        if shutdown.load(Ordering::Acquire) {
                            // The process never started, so the task goes
                            // back to the queue, as it does when shutdown
                            // lands before this worker registered.
                            let task_id = dispatch_task_id.clone();
                            settled_at_start = start_store
                                .call(move |store| store.requeue_leased(&task_id))
                                .is_ok();
                            return Err(ExecutionError::Cancelled);
                        }
                        let execution_data = start_store
                            .call({
                                let task_id = dispatch_task_id.clone();
                                move |store| store.task_execution_data(&task_id)
                            })
                            .map_err(|_| ExecutionError::Unavailable)?;
                        if dispatch_ctx.dispatch_allowed(&execution_data).is_err() {
                            let rejected = dispatch_ctx.end_leased(
                                &dispatch_task_id,
                                &start_execution_id,
                                NodeStore::mark_rejected,
                            );
                            settled_at_start = rejected.is_ok();
                            Self::log_store("reject at start", rejected);
                            return Err(ExecutionError::Unavailable);
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
                        if event_error {
                            return;
                        }
                        if event == ExecutionEvent::Completed {
                            let event_task_id = dispatch_task_id.clone();
                            if matches!(
                                store.call(move |store| store.state(&event_task_id)),
                                Ok(TaskState::Cancelled)
                            ) {
                                return;
                            }
                        }
                        let event_task_id = dispatch_task_id.clone();
                        let event_type = event.event_type();
                        if store
                            .call(move |store| {
                                store.append_event(&event_task_id, event_type)
                            })
                            .is_err()
                        {
                            event_error = true;
                            return;
                        }
                        // No receiver is normal; see `notify_task`.
                        let _ = event_sender.send(dispatch_task_id.clone());
                    },
                )
            },
        );
        let _ = renewal.0.send(());
        let _ = renewal.1.join();
        slot.deactivate();
        // A renewal that failed before the turn started left a cancel that
        // no turn took. The task may be queued again; its next run must not
        // be stopped by it.
        self.canceller.forget(&task_id);
        let run = RunReport {
            result,
            renewal_failed: renewal_failed.load(Ordering::Acquire),
            event_error,
            settled_at_start,
        };
        let cancelled = || {
            let task_id = task_id.clone();
            matches!(
                self.store.call(move |store| store.state(&task_id)),
                Ok(TaskState::Cancelled)
            )
        };
        if let Some(Settlement { label, outcome }) = settlement(run, cancelled) {
            Self::log_store(
                label,
                self.settle(
                    &task_id,
                    &execution_id,
                    start_confirmed.load(Ordering::Acquire),
                    outcome,
                ),
            );
        }
        self.notify_task(&task_id);
    }

    fn dispatch_one(&self) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        if self.executing.swap(true, Ordering::AcqRel) {
            return;
        }
        let bundle = match self.prepare_execution() {
            Ok(Some(bundle)) => bundle,
            Ok(None) => {
                self.executing.store(false, Ordering::Release);
                return;
            }
            Err(error) => {
                eprintln!("aether: scheduler prepare execution: {error:?}");
                self.executing.store(false, Ordering::Release);
                let _ = self.scheduler_wake.try_send(());
                return;
            }
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
        let ctx = self.clone();
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

    pub(in crate::proof) fn run(&self, wake: &mpsc::Receiver<()>) {
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
        match self.store.call(move |store| store.recover_expired_leases(now)) {
            Ok(recovered) => self.stop_recovered(&recovered),
            Err(error) => {
                eprintln!("aether: scheduler recover expired leases: {error:?}");
            }
        }
        if let Err(error) =
            self.store.call(move |store| store.clear_expired_content(now))
        {
            eprintln!("aether: scheduler clear expired content: {error:?}");
        }
    }

    fn stop_recovered(&self, recovered: &[String]) {
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
            self.notify_task(task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(result: Result<&str, ExecutionError>) -> RunReport {
        RunReport {
            result: result.map(str::to_owned),
            renewal_failed: false,
            event_error: false,
            settled_at_start: false,
        }
    }

    fn outcome(run: RunReport, cancelled: bool) -> Option<TaskOutcome> {
        settlement(run, || cancelled).map(|settlement| settlement.outcome)
    }

    #[test]
    fn a_finished_run_settles_from_its_result() {
        assert_eq!(
            outcome(run(Ok("done")), false),
            Some(TaskOutcome::Succeeded("done".to_owned()))
        );
        assert_eq!(outcome(run(Err(ExecutionError::Cancelled)), false), None);
        assert_eq!(
            outcome(run(Err(ExecutionError::Timeout)), false),
            Some(TaskOutcome::Failed)
        );
    }

    #[test]
    fn a_journal_error_fails_the_run_whatever_its_result() {
        for result in [Ok("done"), Err(ExecutionError::Cancelled)] {
            let report = RunReport { event_error: true, ..run(result) };
            assert_eq!(outcome(report, false), Some(TaskOutcome::Failed));
        }
    }

    #[test]
    fn a_late_success_after_renewal_loss_settles_only_as_success() {
        let report = RunReport { renewal_failed: true, ..run(Ok("late")) };
        assert_eq!(
            settlement(report, || true),
            Some(Settlement {
                label: "late success after renewal loss",
                outcome: TaskOutcome::Succeeded("late".to_owned()),
            })
        );
    }

    #[test]
    fn a_renewal_loss_fails_the_run_unless_it_was_cancelled() {
        let lost = || RunReport {
            renewal_failed: true,
            event_error: true,
            ..run(Err(ExecutionError::Cancelled))
        };
        assert_eq!(outcome(lost(), true), None);
        assert_eq!(outcome(lost(), false), Some(TaskOutcome::Failed));
    }

    #[test]
    fn a_run_settled_at_start_is_not_settled_again() {
        for renewal_failed in [false, true] {
            let report = RunReport {
                settled_at_start: true,
                renewal_failed,
                ..run(Err(ExecutionError::Unavailable))
            };
            assert_eq!(outcome(report, false), None);
        }
    }
}
