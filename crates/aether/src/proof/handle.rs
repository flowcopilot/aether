use std::path::{Path, PathBuf};

use aether_core::{Grant, GrantId, ScopeKind, TrustProvider, limits::QueueLimitId};
use aether_store::{QueueLimits, TaskAdmission as StoreAdmission, TaskState};
use tokio::sync::broadcast;

use super::{Identity, ProofError, ProofNode, SubmissionInput};
use crate::execution::Agent;

pub struct ProofHandle<'a, T, P> {
    node: &'a ProofNode<T, P>,
}

impl<'a, T, P> ProofHandle<'a, T, P> {
    #[must_use]
    pub const fn new(node: &'a ProofNode<T, P>) -> Self {
        Self { node }
    }
}

impl<T, P> ProofHandle<'_, T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    #[must_use]
    pub fn scope_id(&self) -> &str {
        self.node.scope_id()
    }

    #[must_use]
    pub fn scope_kind_for(&self, scope_id: &str) -> Option<ScopeKind> {
        self.node.scope_kind_for(scope_id)
    }

    #[must_use]
    pub fn has_scope_kind(&self, kind: ScopeKind) -> bool {
        self.node.has_scope_kind(kind)
    }

    #[must_use]
    pub fn scope_id_for_kind(&self, kind: ScopeKind) -> Option<&str> {
        self.node.scope_id_for_kind(kind)
    }

    pub fn admit(
        &self,
        input: SubmissionInput<'_>,
        identity: Identity<'_>,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        self.node.admit(input, identity, replay_key)
    }

    pub fn task_snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), ProofError> {
        self.node.task_snapshot(task_id)
    }

    pub fn task_progress(
        &self,
        task_id: &str,
        sequence: i64,
        limit: usize,
    ) -> Result<
        (TaskState, Option<String>, Vec<aether_store::TaskEvent>, bool),
        ProofError,
    > {
        self.node.task_progress(task_id, sequence, limit)
    }

    pub fn content_is_expired(&self, task_id: &str) -> Result<bool, ProofError> {
        self.node.content_is_expired(task_id)
    }

    pub fn cancel(&self, task_id: &str) -> Result<(), ProofError> {
        self.node.cancel(task_id)
    }

    pub fn cancel_from(
        &self,
        task_id: &str,
        caller_endpoint: &aether_core::EndpointId,
        scope_id: &str,
    ) -> Result<(), ProofError> {
        self.node.cancel_from(task_id, caller_endpoint, scope_id)
    }

    pub fn events_after(
        &self,
        task_id: &str,
        sequence: i64,
    ) -> Result<Vec<aether_store::TaskEvent>, ProofError> {
        self.node.events_after(task_id, sequence)
    }

    pub fn current_queue_limit(&self, id: QueueLimitId) -> Result<u32, ProofError> {
        self.node.current_queue_limit(id)
    }

    pub fn set_queue_limit(
        &self,
        id: QueueLimitId,
        limit: u32,
    ) -> Result<(), ProofError> {
        self.node.set_queue_limit(id, limit)
    }

    pub fn revoke_grant(&self, id: &GrantId) -> Result<bool, ProofError> {
        self.node.revoke_grant(id)
    }

    pub fn grants(&self) -> Result<Vec<Grant>, ProofError> {
        self.node.grants()
    }

    pub fn has_grant(&self, id: &GrantId) -> Result<bool, ProofError> {
        self.node.has_grant(id)
    }

    pub fn queue_limits(&self) -> Result<QueueLimits, ProofError> {
        self.node.queue_limits()
    }

    pub fn workspace(&self) -> Result<PathBuf, ProofError> {
        self.node.workspace()
    }

    pub fn apply_workspace(&self, path: &Path) -> Result<(), ProofError> {
        self.node.apply_workspace(path)
    }

    pub fn apply_stored_grant(&self, grant: Grant) -> Result<(), ProofError> {
        self.node.apply_stored_grant(grant)
    }

    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.node.subscribe_events()
    }

    pub fn shutdown(&self) {
        self.node.shutdown();
    }
}
