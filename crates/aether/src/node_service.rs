use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use aether_core::{EndpointId, ScopeKind, TrustProvider};

use crate::policy::AgentAdapter;
use aether_store::StoreError;

use crate::{
    admin::{AdminError, LiveNode},
    directory_publication::{DirectoryPublication, PublicationError},
    execution::{Agent, Task},
    proof::{Identity, ProofError, ProofNode, SubmissionInput},
    transport::{
        A2aCancel, A2aPartContent, A2aRequest, A2aTaskStatus, Frame, FrameKind,
        MAX_EXCHANGE_BYTES, MAX_FRAMES, TransportError, event_payload,
        text_result_payload, text_result_payload_with_progress,
    },
};

/// Reserves room below `MAX_EXCHANGE_BYTES` for the final snapshot frame (and
/// its framing overhead) when bounding an event page by encoded size.
const EVENT_PAGE_BYTE_BUDGET: usize = MAX_EXCHANGE_BYTES - 65_536;

pub struct NodeService<T, P> {
    node: ProofNode<T, P>,
    // Empty unless the policy file names approved workspaces — see
    // `set_approved_workspaces`'s own doc comment. Immutable after
    // construction: the approved set itself is not something a live admin
    // change can extend, only pick from.
    approved_workspaces: BTreeMap<String, PathBuf>,
    task_folder_mode: bool,
    agent_adapter: AgentAdapter,
    /// The `pact` section of the policy file, when the node offers PACT.
    pact: Option<crate::pact::config::PactConfig>,
}

/// The skill that a task runs as. A direct grant from before the general
/// skill names `review`; it runs as the general skill. The grant and the
/// request keep their capability, so the policy check does not change.
fn task_skill(kind: Option<ScopeKind>, capability: &str) -> String {
    if kind == Some(ScopeKind::Direct) && capability == "review" {
        crate::execution::GENERAL_SKILL
    } else {
        capability
    }
    .to_owned()
}

impl<T, P> NodeService<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    #[must_use]
    pub const fn new(node: ProofNode<T, P>) -> Self {
        Self {
            node,
            approved_workspaces: BTreeMap::new(),
            task_folder_mode: false,
            agent_adapter: AgentAdapter::Codex,
            pact: None,
        }
    }

    pub fn set_pact_config(&mut self, pact: Option<crate::pact::config::PactConfig>) {
        self.pact = pact;
    }

    #[must_use]
    pub const fn pact_config(&self) -> Option<&crate::pact::config::PactConfig> {
        self.pact.as_ref()
    }

    /// Admits one PACT turn and returns its task id. The gateway verified
    /// the caller's JWT and delegation token; the grant, middleware and
    /// queue limits still decide here (`ProofNode::admit`).
    pub fn admit_verified(
        &self,
        request: &aether_core::Request,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<String, ProofError> {
        let input = SubmissionInput {
            request,
            authenticated_endpoint: &request.caller_endpoint_id,
            idempotency_key,
            task,
        };
        match self.node.admit(input, Identity::Verified, None)? {
            aether_store::TaskAdmission::Accepted(task_id)
            | aether_store::TaskAdmission::Existing(task_id) => Ok(task_id),
        }
    }

    /// A task's state and, once it succeeded, its result.
    pub fn task_snapshot(
        &self,
        task_id: &str,
    ) -> Result<(aether_store::TaskState, Option<String>), ProofError> {
        self.node.task_snapshot(task_id)
    }

    /// Names the workspace paths a live `Change::Workspace` may switch to
    /// by id. Left empty (the default), any path is still accepted, exactly
    /// as before this existed — this is an opt-in restriction a policy file
    /// enables by naming `approvedWorkspaces`, not a behavior change for a
    /// node that does not configure any.
    pub fn set_approved_workspaces(&mut self, approved: BTreeMap<String, PathBuf>) {
        self.approved_workspaces = approved;
    }

    pub const fn set_task_folder_mode(&mut self, enabled: bool) {
        self.task_folder_mode = enabled;
    }

    #[must_use]
    pub const fn task_folder_mode(&self) -> bool {
        self.task_folder_mode
    }

    pub const fn set_agent_adapter(&mut self, adapter: AgentAdapter) {
        self.agent_adapter = adapter;
    }

    #[must_use]
    pub const fn agent_adapter(&self) -> AgentAdapter {
        self.agent_adapter
    }

    #[must_use]
    pub fn approved_workspace(&self, id: &str) -> Option<&Path> {
        self.approved_workspaces.get(id).map(PathBuf::as_path)
    }

    #[must_use]
    pub fn has_approved_workspaces(&self) -> bool {
        !self.approved_workspaces.is_empty()
    }

    /// Whether a live `Change::Workspace` naming this `id` and `path` may
    /// apply. With no approved set configured, every change is approved,
    /// unchanged from before this existed. Otherwise `path` must
    /// canonicalize to exactly the path approved for `id`: a path that does
    /// not exist, or that resolves elsewhere, is rejected rather than
    /// guessed at.
    #[must_use]
    pub fn workspace_change_is_approved(&self, id: &str, path: &Path) -> bool {
        if self.task_folder_mode {
            return false;
        }
        if !self.has_approved_workspaces() {
            return true;
        }
        let Some(approved) = self.approved_workspace(id) else {
            return false;
        };
        path.canonicalize().is_ok_and(|canonical| canonical == approved)
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.node.subscribe_events()
    }

    #[must_use]
    pub fn scope_id(&self) -> &str {
        self.node.scope_id()
    }

    #[must_use]
    pub const fn scope_kind(&self) -> ScopeKind {
        self.node.scope_kind()
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

    /// The executor's live workspace path.
    pub fn current_workspace(&self) -> Result<PathBuf, ProofError> {
        self.node.workspace()
    }

    pub fn apply_workspace(&self, path: &std::path::Path) -> Result<(), ProofError> {
        if self.task_folder_mode {
            return Err(ProofError::Execution(crate::execution::ExecutionError::Path));
        }
        self.node.apply_workspace(path)
    }

    pub fn apply_grant(&self, grant: aether_core::Grant) -> Result<(), ProofError> {
        self.node.apply_grant(grant)
    }

    pub fn apply_stored_grant(&self, grant_json: &str) -> Result<(), ProofError> {
        let grant = crate::identity::decode_grant(grant_json)
            .map_err(|_| ProofError::Denied(aether_core::Denial::GrantDenied))?;
        self.node.apply_stored_grant(grant)
    }

    pub fn has_grant(&self, id: &aether_core::GrantId) -> Result<bool, ProofError> {
        self.node.has_grant(id)
    }

    pub fn queue_limits(&self) -> Result<aether_store::QueueLimits, ProofError> {
        self.node.queue_limits()
    }

    pub fn shutdown(&self)
    where
        T: TrustProvider + 'static,
        P: Agent + Clone + Send + 'static,
    {
        self.node.shutdown();
    }

    pub fn handle(
        &self,
        authenticated_endpoint: &str,
        frames: Vec<Frame>,
    ) -> Result<Vec<Frame>, TransportError> {
        let Ok([mut frame]) = <[Frame; 1]>::try_from(frames) else {
            return Err(TransportError::InvalidOrder);
        };
        if frame.kind == FrameKind::Cancel {
            return self.cancel(authenticated_endpoint, frame);
        }
        if frame.kind != FrameKind::Request {
            return Err(TransportError::BadFrame);
        }
        let payload: A2aRequest = serde_json::from_value(frame.payload.take())
            .map_err(|_| TransportError::BadFrame)?;
        let (request, claim) = payload.policy_data(&frame.scope_id)?;
        let endpoint = EndpointId::new(authenticated_endpoint);
        if request.caller_endpoint_id != endpoint {
            return Err(TransportError::EndpointMismatch);
        }
        let text = match payload.task.history.as_deref() {
            Some([message]) if message.role == a2a::Role::User => {
                match message.parts.as_slice() {
                    [part] => match &part.content {
                        A2aPartContent::Text(text) => text,
                        _ => return Err(TransportError::BadFrame),
                    },
                    _ => return Err(TransportError::BadFrame),
                }
            }
            _ => return Err(TransportError::BadFrame),
        };
        let task = Task {
            id: payload.task.id,
            skill: task_skill(
                self.scope_kind_for(&frame.scope_id),
                request.capability_id.as_str(),
            ),
            workspace_id: request.workspace_id.as_str().to_owned(),
            remote_text: text.to_owned(),
            context_tokens: 0,
        };
        let admission = self
            .node
            .admit(
                SubmissionInput {
                    request: &request,
                    authenticated_endpoint: &endpoint,
                    idempotency_key: &payload.idempotency_key,
                    task: &task,
                },
                Identity::Claim(&claim),
                Some(format!("{}:{}", frame.exchange_id, payload.request_nonce)),
            )
            .map_err(map_error)?;
        let (aether_store::TaskAdmission::Accepted(task_id)
        | aether_store::TaskAdmission::Existing(task_id)) = admission;
        let last_sequence = i64::try_from(payload.last_event_sequence.unwrap_or(0))
            .map_err(|_| TransportError::BadFrame)?;
        // Bound the page in SQL first (by frame count), then trim further by
        // encoded size, reserving room for the final snapshot frame. Either
        // bound can leave events unread, so both feed `more_events` below.
        // The snapshot and the page are read together so a concurrent store
        // write cannot leave them describing two different instants.
        let event_limit = MAX_FRAMES.saturating_sub(1);
        let (state, result, events, mut more_events) = self
            .node
            .task_progress(&task_id, last_sequence, event_limit)
            .map_err(map_error)?;
        if matches!(
            state,
            aether_store::TaskState::UnknownResult
                | aether_store::TaskState::OperatorReviewed
        ) {
            return Err(TransportError::OperatorReviewRequired(
                state.as_str().to_owned(),
            ));
        }
        if state == aether_store::TaskState::Succeeded
            && self.node.content_is_expired(&task_id).map_err(map_error)?
        {
            return Err(TransportError::ResultExpired);
        }
        let status = a2a_status(state);
        let result_text = if state == aether_store::TaskState::Succeeded {
            result.unwrap_or_default()
        } else {
            String::new()
        };
        let mut response = Vec::with_capacity(events.len().saturating_add(1));
        let mut exchange_bytes = 0usize;
        let mut cursor = last_sequence;
        for event in events {
            let event_frame = Frame {
                version: 0,
                kind: FrameKind::Event,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: Some(task_id.clone()),
                payload: event_payload(
                    &task_id,
                    event_status(event.event_type, state),
                    Some(event.sequence),
                ),
            };
            let frame_bytes =
                serde_json::to_vec(&event_frame).map_or(0, |encoded| encoded.len());
            if exchange_bytes.saturating_add(frame_bytes) > EVENT_PAGE_BYTE_BUDGET {
                more_events = true;
                break;
            }
            exchange_bytes = exchange_bytes.saturating_add(frame_bytes);
            cursor = event.sequence;
            response.push(event_frame);
        }
        response.push(Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: frame.exchange_id.clone(),
            scope_id: frame.scope_id.clone(),
            task_id: Some(task_id.clone()),
            payload: text_result_payload_with_progress(
                &task_id,
                status,
                &result_text,
                cursor,
                more_events,
            ),
        });
        Ok(response)
    }
}

/// The A2A status of `event`, which happened to a task that is in `current`
/// now.
fn event_status(
    event: aether_store::EventType,
    current: aether_store::TaskState,
) -> A2aTaskStatus {
    a2a_status(event.task_state().unwrap_or(current))
}

const fn a2a_status(state: aether_store::TaskState) -> A2aTaskStatus {
    match state {
        aether_store::TaskState::PendingApproval => A2aTaskStatus::InputRequired,
        aether_store::TaskState::Queued | aether_store::TaskState::Leased => {
            A2aTaskStatus::Submitted
        }
        aether_store::TaskState::Running => A2aTaskStatus::Working,
        aether_store::TaskState::Succeeded => A2aTaskStatus::Completed,
        aether_store::TaskState::Failed
        | aether_store::TaskState::UnknownResult
        | aether_store::TaskState::OperatorReviewed => A2aTaskStatus::Failed,
        aether_store::TaskState::Rejected => A2aTaskStatus::Rejected,
        aether_store::TaskState::Cancelled => A2aTaskStatus::Canceled,
    }
}

impl<T, P> NodeService<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    fn cancel(
        &self,
        authenticated_endpoint: &str,
        mut frame: Frame,
    ) -> Result<Vec<Frame>, TransportError> {
        let cancel: A2aCancel = serde_json::from_value(frame.payload.take())
            .map_err(|_| TransportError::BadFrame)?;
        let task_id = frame.task_id.as_deref().ok_or(TransportError::BadFrame)?;
        if cancel.id != task_id {
            return Err(TransportError::BadFrame);
        }
        self.node
            .cancel_from(
                task_id,
                &EndpointId::new(authenticated_endpoint),
                &frame.scope_id,
            )
            .map_err(map_error)?;
        let event = self
            .node
            .events_after(task_id, 0)
            .map_err(map_error)?
            .into_iter()
            .last()
            .ok_or_else(|| TransportError::Network("task event missing".to_owned()))?;
        Ok(vec![
            Frame {
                version: 0,
                kind: FrameKind::Event,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: frame.task_id.clone(),
                payload: event_payload(
                    task_id,
                    event_status(event.event_type, aether_store::TaskState::Cancelled),
                    Some(event.sequence),
                ),
            },
            Frame {
                version: 0,
                kind: FrameKind::Result,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: frame.task_id.clone(),
                payload: text_result_payload(task_id, A2aTaskStatus::Canceled, ""),
            },
        ])
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an adapter for `map_err`, which hands over the error by value"
)]
fn map_error(error: ProofError) -> TransportError {
    match error {
        ProofError::Denied(_)
        | ProofError::GroupDenied
        | ProofError::Store(StoreError::Unauthorized) => {
            TransportError::PermissionDenied
        }
        ProofError::Store(StoreError::Overloaded) => TransportError::Overloaded,
        ProofError::Store(StoreError::DuplicateConflict) => TransportError::Replay,
        ProofError::Store(_)
        | ProofError::Execution(_)
        | ProofError::TaskMismatch
        | ProofError::Queue
        | ProofError::Shutdown => TransportError::Network("task failed".to_owned()),
    }
}

/// The node that `aether node serve` runs, as the administration sees it.
///
/// The parts that start after the service — the endpoint, the directory
/// publication and the outbox — are attached when they start. Until then,
/// their changes are unsupported.
pub struct RunningNode<T, P> {
    service: Arc<NodeService<T, P>>,
    data_dir: PathBuf,
    /// The endpoint, and the runtime that drives its relay and directory
    /// calls.
    endpoint: OnceLock<(iroh::Endpoint, tokio::runtime::Handle)>,
    directory: OnceLock<Arc<DirectoryPublication>>,
    outbox: OnceLock<Arc<tokio::sync::Notify>>,
}

impl<T, P> RunningNode<T, P> {
    #[must_use]
    pub const fn new(service: Arc<NodeService<T, P>>, data_dir: PathBuf) -> Self {
        Self {
            service,
            data_dir,
            endpoint: OnceLock::new(),
            directory: OnceLock::new(),
            outbox: OnceLock::new(),
        }
    }

    /// Lets the relay and the directory changes use `endpoint`. Only the
    /// first endpoint is kept.
    pub fn attach_endpoint(
        &self,
        endpoint: iroh::Endpoint,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = self.endpoint.set((endpoint, runtime));
    }

    /// Lets the directory and the capability changes renew `publication`.
    /// Only the first publication is kept.
    pub fn attach_directory(&self, publication: Arc<DirectoryPublication>) {
        let _ = self.directory.set(publication);
    }

    /// Lets [`LiveNode::wake_outbox`] start a round of the outbox. Only the
    /// first waker is kept.
    pub fn attach_outbox(&self, waker: Arc<tokio::sync::Notify>) {
        let _ = self.outbox.set(waker);
    }

    /// Runs `publish` with the directory publication and the endpoint id,
    /// and waits for its result. `LiveNode` is synchronous, so this bridges
    /// into the async call without assuming whether its caller (the sync
    /// admin-socket thread, or an async MCP or admin-UI task on this same
    /// runtime) already holds a tokio worker thread: `Handle::spawn` plus a
    /// plain blocking channel avoids driving the future on a foreign thread.
    /// A node without a directory publication answers
    /// [`AdminError::Unsupported`] with `kind`.
    fn publish<F, Fut>(
        &self,
        kind: &'static str,
        publish: F,
    ) -> Result<Result<(), PublicationError>, AdminError>
    where
        F: FnOnce(Arc<DirectoryPublication>, String) -> Fut,
        Fut: Future<Output = Result<(), PublicationError>> + Send + 'static,
    {
        let (Some(publication), Some((endpoint, runtime))) =
            (self.directory.get(), self.endpoint.get())
        else {
            return Err(AdminError::Unsupported(kind));
        };
        let future = publish(Arc::clone(publication), endpoint.id().to_string());
        let (sender, receiver) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let _ = sender.send(future.await);
        });
        receiver.recv().map_err(|_| AdminError::Store)
    }
}

impl<T, P> LiveNode for RunningNode<T, P>
where
    T: TrustProvider + Send + Sync + 'static,
    P: Agent + Clone + Send + Sync + 'static,
{
    fn cancel_task(&self, task_id: &str) -> Result<(), AdminError> {
        self.service.node.cancel(task_id).map_err(|_| AdminError::Task)
    }

    fn queue_limit(&self, id: &str) -> Result<u32, AdminError> {
        self.service
            .node
            .current_queue_limit(id)
            .map_err(|_| AdminError::InvalidField("id"))
    }

    fn set_queue_limit(&self, id: &str, limit: u32) -> Result<(), AdminError> {
        self.service
            .node
            .set_queue_limit(id, limit)
            .map_err(|_| AdminError::InvalidField("id"))
    }

    fn apply_workspace(&self, id: &str, path: &str) -> Result<(), AdminError> {
        let path = Path::new(path);
        if !self.service.workspace_change_is_approved(id, path) {
            return Err(AdminError::InvalidField("id"));
        }
        self.service.apply_workspace(path).map_err(|_| AdminError::InvalidField("path"))
    }

    fn workspace(&self) -> Option<String> {
        self.service.current_workspace().ok().map(|path| path.display().to_string())
    }

    fn apply_grant(&self, _id: &str, grant: &str) -> Result<(), AdminError> {
        self.service
            .apply_stored_grant(grant)
            .map_err(|_| AdminError::InvalidField("grant"))
    }

    fn revoke_grant(&self, id: &str) -> Result<(), AdminError> {
        self.service
            .node
            .revoke_grant(&aether_core::GrantId::new(id))
            .map(|_| ())
            .map_err(|_| AdminError::InvalidField("grant"))
    }

    fn grants(&self) -> Vec<aether_core::Grant> {
        self.service.node.grants().unwrap_or_default()
    }

    fn renew_directory(&self, url: &str) -> Result<(), AdminError> {
        if self
            .directory
            .get()
            .is_some_and(|publication| publication.directory() != url)
        {
            return Err(AdminError::InvalidField("url"));
        }
        self.publish("directory", |publication, endpoint_id| async move {
            publication.renew(&endpoint_id).await.map(|_| ())
        })?
        .map_err(|_| AdminError::InvalidField("url"))
    }

    fn publish_capability(&self, capability: &str) -> Result<(), AdminError> {
        let capability = capability.to_owned();
        self.publish("capability", |publication, endpoint_id| async move {
            publication.publish_with_capability(&endpoint_id, capability).await
        })?
        .map_err(|_| AdminError::InvalidField("capability"))
    }

    fn set_relay(&self, relay: Option<String>) -> Result<(), AdminError> {
        let (endpoint, runtime) =
            self.endpoint.get().ok_or(AdminError::Unsupported("relay"))?;
        let previous =
            crate::settings::setting::<crate::transport::relay::ActiveRelay>(
                &self.data_dir,
            )
            .map_err(|_| AdminError::Store)?;
        runtime
            .block_on(crate::transport::relay::apply_relay(
                &self.data_dir,
                &relay,
                crate::transport::switch_relay(
                    endpoint,
                    previous.as_deref(),
                    relay.as_deref(),
                ),
            ))
            .map_err(|error| match error {
                crate::transport::TransportError::Io => AdminError::Store,
                _ => AdminError::InvalidField("relay"),
            })
    }

    fn wake_outbox(&self) -> Result<(), AdminError> {
        self.outbox.get().ok_or(AdminError::Unsupported("outbox"))?.notify_one();
        Ok(())
    }

    fn subscribe_events(&self) -> Option<tokio::sync::broadcast::Receiver<String>> {
        Some(self.service.subscribe_events())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        net::TcpListener,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, SystemTime},
    };

    use aether_core::{
        AccessMode, AgentId, ApprovalRule, CapabilityId, ClaimId, GrantId,
        PolicyEngine, Scope, ScopeId, ScopeKind, WorkspaceId,
    };
    use aether_store::{NodeStore, QueueLimits, TaskInput};
    use ed25519_dalek::SigningKey;

    use super::NodeService;
    use crate::{
        execution::{Agent, ExecutionError, Executor, Limits},
        identity::{ClaimInput, GrantInput, SignedClaims, signed_claim, signed_grant},
        proof::ProofNode,
        transport::{
            A2aAccessMode, A2aAuthorization, A2aCancel, A2aClaim, A2aRequest, Frame,
            FrameKind, bind_node, request_once, router, text_request_payload,
        },
    };

    const WIRE_TASK_ID: &str = "00000000-0000-4000-8000-000000000002";
    const WIRE_NONCE: &str = "00000000-0000-4000-8000-000000000003";
    const WIRE_REQUEST_ID: &str = "00000000-0000-4000-8000-000000000004";

    #[derive(Clone)]
    struct Fake;

    impl Agent for Fake {
        fn start_turn(
            &mut self,
            _task_id: &str,
            instruction: &str,
            text: &str,
        ) -> Result<String, ExecutionError> {
            Ok(format!("{instruction}:{text}"))
        }

        fn cancel(&self, _task_id: &str) {}
    }

    #[test]
    fn a_pact_task_keeps_its_brand_capability() {
        // A brand can name any capability, also `review`.
        assert_eq!(super::task_skill(Some(ScopeKind::Pact), "review"), "review");
        assert_eq!(
            super::task_skill(Some(ScopeKind::Pact), "brand-copy"),
            "brand-copy"
        );
        assert_eq!(
            super::task_skill(Some(ScopeKind::Direct), "review"),
            crate::execution::GENERAL_SKILL
        );
    }

    #[tokio::test]
    async fn a_direct_review_grant_runs_with_the_general_instructions() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        let database = root.join(aether_store::NODE_DATABASE);
        fs::create_dir_all(&workspace).expect("workspace must create");
        let endpoint = aether_core::EndpointId::new(
            iroh::SecretKey::from_bytes(&[4; 32]).public().to_string(),
        );
        let key = SigningKey::from_bytes(&[8; 32]);
        let now = SystemTime::now();
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let claim = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("claim"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [1; 32],
            },
        );
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [2; 32],
            },
        );
        let node = ProofNode::new(
            PolicyEngine::new(
                SignedClaims::new(
                    BTreeMap::from([(
                        AgentId::new("issuer"),
                        key.verifying_key().to_bytes(),
                    )]),
                    Duration::from_secs(5),
                ),
                [grant],
            ),
            NodeStore::open(&database).expect("store must open"),
            Executor::new(
                Fake,
                Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 32,
                    max_seconds: 1,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let mut payload: A2aRequest = serde_json::from_value(text_request_payload(
            WIRE_TASK_ID,
            "key",
            WIRE_NONCE,
            "remote text",
        ))
        .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: WIRE_REQUEST_ID.to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: (&claim).into(),
        });
        let service = NodeService::new(node);
        // Each poll is a new exchange of the same request, as a caller sends.
        let mut answer = serde_json::Value::Null;
        for poll in 0..100_u32 {
            let response = service
                .handle(
                    endpoint.as_str(),
                    vec![Frame {
                        version: 0,
                        kind: FrameKind::Request,
                        exchange_id: format!("00000000-0000-4000-8000-{poll:012}"),
                        scope_id: "direct".to_owned(),
                        task_id: Some(WIRE_TASK_ID.to_owned()),
                        payload: serde_json::to_value(&payload)
                            .expect("payload must encode"),
                    }],
                )
                .expect("request must complete");
            answer = response.last().expect("result must exist").payload["task"]
                ["artifacts"][0]["parts"][0]["text"]
                .clone();
            if !answer.is_null() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The test agent answers with the skill it ran: the general skill,
        // whose instruction asks for the sent task.
        assert_eq!(answer, "general:remote text");
        let stored = NodeStore::open(&database)
            .expect("store must open")
            .task_execution_data(WIRE_TASK_ID)
            .expect("task must persist");
        assert_eq!(stored.skill, crate::execution::GENERAL_SKILL);
        assert_eq!(stored.request.capability_id.as_str(), "review");
    }

    #[tokio::test]
    async fn signed_a2a_request_admits_and_executes_one_task() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        let database = root.join(aether_store::NODE_DATABASE);
        let server_data = root.join("server");
        let client_data = root.join("client");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let listener = TcpListener::bind("127.0.0.1:0").expect("port must bind");
        let port = listener.local_addr().expect("port must resolve").port();
        drop(listener);
        let server =
            bind_node(&server_data, port, None).await.expect("server must bind");
        let client = bind_node(&client_data, 0, None).await.expect("client must bind");
        let endpoint = aether_core::EndpointId::new(client.id().to_string());
        client.close().await;
        let key = SigningKey::from_bytes(&[8; 32]);
        let now = SystemTime::now();
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let claim = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("claim"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [1; 32],
            },
        );
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [2; 32],
            },
        );
        let node = ProofNode::new(
            PolicyEngine::new(
                SignedClaims::new(
                    BTreeMap::from([(
                        AgentId::new("issuer"),
                        key.verifying_key().to_bytes(),
                    )]),
                    Duration::from_secs(5),
                ),
                [grant],
            ),
            NodeStore::open(&database).expect("store must open"),
            Executor::new(
                Fake,
                Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 32,
                    max_seconds: 1,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let mut payload: A2aRequest = serde_json::from_value(text_request_payload(
            WIRE_TASK_ID,
            "key",
            WIRE_NONCE,
            "remote text",
        ))
        .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: WIRE_REQUEST_ID.to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: (&claim).into(),
        });
        let service = Arc::new(Mutex::new(NodeService::new(node)));
        let server_service = Arc::clone(&service);
        let server_router =
            router(server.clone(), move |authenticated_endpoint, frames| {
                let service = server_service.lock().map_err(|_| {
                    crate::transport::TransportError::Network(
                        "service stopped".to_owned(),
                    )
                })?;
                service.handle(authenticated_endpoint, frames)
            });
        let response = request_once(
            &client_data,
            &server.id().to_string(),
            &format!("127.0.0.1:{port}"),
            Frame {
                version: 0,
                kind: FrameKind::Request,
                exchange_id: "00000000-0000-4000-8000-000000000005".to_owned(),
                scope_id: "direct".to_owned(),
                task_id: Some(WIRE_TASK_ID.to_owned()),
                payload: serde_json::to_value(payload.clone())
                    .expect("payload must encode"),
            },
        )
        .await
        .expect("task must complete");
        server_router.shutdown().await.expect("router must stop");
        server.close().await;
        assert_eq!(
            response.last().expect("result must exist").payload["task"]["artifacts"][0]
                ["parts"][0]["text"],
            "general:remote text"
        );
        let sequences = event_sequences(&response);
        assert!(!sequences.is_empty());
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        let mut replay_payload = payload;
        replay_payload.last_event_sequence = Some(1);
        let replay = service
            .lock()
            .expect("service must unlock")
            .handle(
                endpoint.as_str(),
                vec![Frame {
                    version: 0,
                    kind: FrameKind::Request,
                    exchange_id: "00000000-0000-4000-8000-000000000010".to_owned(),
                    scope_id: "direct".to_owned(),
                    task_id: Some(WIRE_TASK_ID.to_owned()),
                    payload: serde_json::to_value(replay_payload)
                        .expect("payload must encode"),
                }],
            )
            .expect("replay must complete");
        assert!(event_sequences(&replay).iter().all(|sequence| *sequence > 1));
        assert_eq!(
            replay.last().expect("result must exist").payload["task"]["artifacts"][0]["parts"]
                [0]["text"],
            "general:remote text"
        );
        let stored = NodeStore::open(&database)
            .expect("store must open")
            .stored_task(WIRE_TASK_ID)
            .expect("task must persist");
        assert_eq!(stored.skill, "general");
        assert_eq!(stored.remote_text, "remote text");
    }

    #[derive(Clone)]
    struct ManyProgressEvents;

    impl Agent for ManyProgressEvents {
        fn start_turn(
            &mut self,
            _task_id: &str,
            instruction: &str,
            text: &str,
        ) -> Result<String, ExecutionError> {
            Ok(format!("{instruction}:{text}"))
        }

        fn start_turn_with_events(
            &mut self,
            _task_id: &str,
            instruction: &str,
            text: &str,
            events: &mut dyn FnMut(crate::execution::ExecutionEvent),
        ) -> Result<String, ExecutionError> {
            // More than `MAX_FRAMES - 1` progress events, so the durable
            // journal holds more than one page can ever carry in a single
            // exchange.
            for _ in 0..70 {
                events(crate::execution::ExecutionEvent::Progress);
            }
            events(crate::execution::ExecutionEvent::Completed);
            Ok(format!("{instruction}:{text}"))
        }

        fn cancel(&self, _task_id: &str) {}
    }

    #[tokio::test]
    async fn a_task_with_more_events_than_one_page_delivers_them_all() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        let server_data = root.join("server");
        let client_data = root.join("client");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let listener = TcpListener::bind("127.0.0.1:0").expect("port must bind");
        let port = listener.local_addr().expect("port must resolve").port();
        drop(listener);
        let server =
            bind_node(&server_data, port, None).await.expect("server must bind");
        let client = bind_node(&client_data, 0, None).await.expect("client must bind");
        let endpoint = aether_core::EndpointId::new(client.id().to_string());
        client.close().await;
        let key = SigningKey::from_bytes(&[9; 32]);
        let now = SystemTime::now();
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let claim = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("claim"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [3; 32],
            },
        );
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: scope.id.clone(),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [4; 32],
            },
        );
        let node = ProofNode::new(
            PolicyEngine::new(
                SignedClaims::new(
                    BTreeMap::from([(
                        AgentId::new("issuer"),
                        key.verifying_key().to_bytes(),
                    )]),
                    Duration::from_secs(5),
                ),
                [grant],
            ),
            NodeStore::in_dir(&root).expect("store must open"),
            Executor::new(
                ManyProgressEvents,
                Limits {
                    workspace: workspace.clone(),
                    max_context_tokens: 256,
                    max_seconds: 5,
                },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let mut payload: A2aRequest = serde_json::from_value(text_request_payload(
            WIRE_TASK_ID,
            "key",
            WIRE_NONCE,
            "remote text",
        ))
        .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: WIRE_REQUEST_ID.to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: (&claim).into(),
        });
        let service = Arc::new(Mutex::new(NodeService::new(node)));
        let server_router =
            router(server.clone(), move |authenticated_endpoint, frames| {
                let service = service.lock().map_err(|_| {
                    crate::transport::TransportError::Network(
                        "service stopped".to_owned(),
                    )
                })?;
                service.handle(authenticated_endpoint, frames)
            });
        let response = request_once(
            &client_data,
            &server.id().to_string(),
            &format!("127.0.0.1:{port}"),
            Frame {
                version: 0,
                kind: FrameKind::Request,
                exchange_id: "00000000-0000-4000-8000-000000000006".to_owned(),
                scope_id: "direct".to_owned(),
                task_id: Some(WIRE_TASK_ID.to_owned()),
                payload: serde_json::to_value(payload).expect("payload must encode"),
            },
        )
        .await
        .expect("task must complete");
        server_router.shutdown().await.expect("router must stop");
        server.close().await;
        assert_eq!(
            response.last().expect("result must exist").payload["task"]["artifacts"][0]
                ["parts"][0]["text"],
            "general:remote text"
        );
        let sequences = event_sequences(&response);
        // More than one page's worth of events (`MAX_FRAMES - 1`) must have
        // arrived: the client must not have stopped at the first page just
        // because that page's snapshot already looked terminal.
        assert!(
            sequences.len() > crate::transport::MAX_FRAMES - 1,
            "expected more than one page of events, got {}",
            sequences.len()
        );
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            sequences,
            (1..=i64::try_from(sequences.len()).expect("the count must fit"))
                .collect::<Vec<_>>(),
            "every event must be delivered exactly once, in order, with no gaps"
        );
    }

    #[test]
    fn a_request_and_cancel_together_are_rejected_before_admission() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let store = NodeStore::in_dir(&root).expect("store must open");
        let node = ProofNode::new(
            PolicyEngine::new(
                SignedClaims::new(BTreeMap::new(), Duration::from_secs(5)),
                Vec::new(),
            ),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let service = NodeService::new(node);
        let payload = crate::transport::text_request_payload(
            WIRE_TASK_ID,
            "key",
            WIRE_NONCE,
            "remote text",
        );
        let result = service.handle(
            "endpoint",
            vec![
                Frame {
                    version: 0,
                    kind: FrameKind::Request,
                    exchange_id: "00000000-0000-4000-8000-000000000013".to_owned(),
                    scope_id: "direct".to_owned(),
                    task_id: Some(WIRE_TASK_ID.to_owned()),
                    payload,
                },
                Frame {
                    version: 0,
                    kind: FrameKind::Cancel,
                    exchange_id: "00000000-0000-4000-8000-000000000013".to_owned(),
                    scope_id: "direct".to_owned(),
                    task_id: Some(WIRE_TASK_ID.to_owned()),
                    payload: serde_json::to_value(A2aCancel {
                        id: WIRE_TASK_ID.to_owned(),
                        metadata: None,
                        tenant: None,
                    })
                    .expect("cancel must encode"),
                },
            ],
        );
        assert_eq!(result, Err(crate::transport::TransportError::InvalidOrder));
        service.shutdown();
        // Rejecting the mixed batch before admission means the task was
        // never queued at all, not merely left in whatever state it had.
        let store = NodeStore::in_dir(&root).expect("store must reopen");
        assert_eq!(store.state(WIRE_TASK_ID), Err(aether_store::StoreError::NotFound));
    }

    #[test]
    fn peer_cancel_marks_an_owned_queued_task_cancelled() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new(WIRE_TASK_ID),
                        id: aether_core::RequestId::new(WIRE_REQUEST_ID),
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
                    remote_text: "remote text".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must queue");
        let node = ProofNode::new(
            PolicyEngine::new(
                SignedClaims::new(BTreeMap::new(), Duration::from_secs(5)),
                Vec::new(),
            ),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let service = NodeService::new(node);
        let mut event_wake = service.subscribe_events();
        let response = service
            .handle(
                "endpoint",
                vec![Frame {
                    version: 0,
                    kind: FrameKind::Cancel,
                    exchange_id: "00000000-0000-4000-8000-000000000009".to_owned(),
                    scope_id: "direct".to_owned(),
                    task_id: Some(WIRE_TASK_ID.to_owned()),
                    payload: serde_json::to_value(A2aCancel {
                        id: WIRE_TASK_ID.to_owned(),
                        metadata: None,
                        tenant: None,
                    })
                    .expect("cancel must encode"),
                }],
            )
            .expect("cancel must complete");
        assert_eq!(
            event_wake.try_recv().expect("event wake must arrive"),
            WIRE_TASK_ID
        );
        assert_eq!(
            response[0].payload["statusUpdate"]["metadata"]
                [crate::transport::EVENT_SEQUENCE],
            2
        );
        assert_eq!(
            response[1].payload["task"]["status"]["state"],
            "TASK_STATE_CANCELED"
        );
    }

    struct Trusted;

    impl aether_core::TrustProvider for Trusted {
        fn verify(
            &self,
            _claim: &aether_core::Claim,
            _authenticated_endpoint: &aether_core::EndpointId,
            _scope: &Scope,
        ) -> Result<(), aether_core::ProviderError> {
            Ok(())
        }

        fn verify_grant(
            &self,
            _grant: &aether_core::Grant,
            _scope: &Scope,
        ) -> Result<(), aether_core::ProviderError> {
            Ok(())
        }
    }

    #[test]
    fn an_unresolved_task_reports_a_machine_readable_operator_review_error() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let endpoint = aether_core::EndpointId::new("endpoint");
        let now = SystemTime::now();
        let request = aether_core::Request {
            id: aether_core::RequestId::new("request"),
            task_id: aether_core::TaskId::new(WIRE_TASK_ID),
            caller_agent_id: AgentId::new("caller"),
            caller_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            capability_id: CapabilityId::new("review"),
            workspace_id: WorkspaceId::new("workspace"),
            access_mode: AccessMode::ReadOnly,
        };
        let task = crate::execution::Task {
            id: WIRE_TASK_ID.to_owned(),
            skill: "general".to_owned(),
            workspace_id: "workspace".to_owned(),
            remote_text: "remote text".to_owned(),
            context_tokens: 0,
        };
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new(WIRE_TASK_ID),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("caller"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("direct"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: crate::proof::request_fingerprint(
                        &request, &task,
                    ),
                    skill: "general".to_owned(),
                    remote_text: "remote text".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must queue");
        // A lease that already expired when the node starts: `ProofNode`'s
        // constructor recovers it immediately, and since it was `running`,
        // recovery moves it to `unknown_result` rather than requeuing it.
        store.lease_next("worker", "execution", 0).expect("task must lease");
        store.mark_running(WIRE_TASK_ID, "execution").expect("task must run");
        let grant = aether_core::Grant {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new("caller"),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
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
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [grant]),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        assert_eq!(
            node.state(WIRE_TASK_ID).expect("state must load"),
            aether_store::TaskState::UnknownResult
        );
        let service = NodeService::new(node);
        let mut payload: A2aRequest =
            serde_json::from_value(crate::transport::text_request_payload(
                WIRE_TASK_ID,
                "key",
                WIRE_NONCE,
                "remote text",
            ))
            .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: "request".to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: A2aClaim {
                id: "claim".to_owned(),
                issuer_agent_id: "issuer".to_owned(),
                subject_agent_id: "caller".to_owned(),
                subject_endpoint_id: endpoint.as_str().to_owned(),
                scope_id: "direct".to_owned(),
                issued_at: seconds(now),
                expires_at: seconds(now + Duration::from_secs(60)),
                signing_key: hex::encode([0; 32]),
                nonce: hex::encode([0; 32]),
                signature: hex::encode([0; 32]),
            },
        });
        let result = service.handle(
            "endpoint",
            vec![Frame {
                version: 0,
                kind: FrameKind::Request,
                exchange_id: "00000000-0000-4000-8000-000000000011".to_owned(),
                scope_id: "direct".to_owned(),
                task_id: Some(WIRE_TASK_ID.to_owned()),
                payload: serde_json::to_value(payload).expect("payload must encode"),
            }],
        );
        assert_eq!(
            result,
            Err(crate::transport::TransportError::OperatorReviewRequired(
                "unknown_result".to_owned()
            ))
        );
    }

    #[test]
    fn an_expired_result_reports_a_machine_readable_error() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let endpoint = aether_core::EndpointId::new("endpoint");
        let now = SystemTime::now();
        let request = aether_core::Request {
            id: aether_core::RequestId::new("request"),
            task_id: aether_core::TaskId::new(WIRE_TASK_ID),
            caller_agent_id: AgentId::new("caller"),
            caller_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            capability_id: CapabilityId::new("review"),
            workspace_id: WorkspaceId::new("workspace"),
            access_mode: AccessMode::ReadOnly,
        };
        let task = crate::execution::Task {
            id: WIRE_TASK_ID.to_owned(),
            skill: "general".to_owned(),
            workspace_id: "workspace".to_owned(),
            remote_text: "remote text".to_owned(),
            context_tokens: 0,
        };
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new(WIRE_TASK_ID),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("caller"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("direct"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: crate::proof::request_fingerprint(
                        &request, &task,
                    ),
                    skill: "general".to_owned(),
                    remote_text: "remote text".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must queue");
        // Succeed the task, then let its already-past retention deadline
        // clear its content immediately, without waiting a real 24 hours.
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running(WIRE_TASK_ID, "execution").expect("task must run");
        store
            .mark_succeeded(WIRE_TASK_ID, "execution", "final result", 10)
            .expect("task must succeed");
        store.clear_expired_content(10).expect("content must expire");
        let grant = aether_core::Grant {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new("caller"),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
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
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [grant]),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        assert!(
            node.content_is_expired(WIRE_TASK_ID).expect("expiry check must run"),
            "content must already be expired before the request arrives"
        );
        let service = NodeService::new(node);
        let mut payload: A2aRequest =
            serde_json::from_value(crate::transport::text_request_payload(
                WIRE_TASK_ID,
                "key",
                WIRE_NONCE,
                "remote text",
            ))
            .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: "request".to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: A2aClaim {
                id: "claim".to_owned(),
                issuer_agent_id: "issuer".to_owned(),
                subject_agent_id: "caller".to_owned(),
                subject_endpoint_id: endpoint.as_str().to_owned(),
                scope_id: "direct".to_owned(),
                issued_at: seconds(now),
                expires_at: seconds(now + Duration::from_secs(60)),
                signing_key: hex::encode([0; 32]),
                nonce: hex::encode([0; 32]),
                signature: hex::encode([0; 32]),
            },
        });
        let result = service.handle(
            "endpoint",
            vec![Frame {
                version: 0,
                kind: FrameKind::Request,
                exchange_id: "00000000-0000-4000-8000-000000000012".to_owned(),
                scope_id: "direct".to_owned(),
                task_id: Some(WIRE_TASK_ID.to_owned()),
                payload: serde_json::to_value(payload).expect("payload must encode"),
            }],
        );
        assert_eq!(result, Err(crate::transport::TransportError::ResultExpired));
    }

    #[test]
    fn an_event_has_the_status_of_the_state_it_leaves_the_task_in() {
        use aether_store::{EventType, TaskState};

        use crate::transport::A2aTaskStatus;

        for (event, status) in [
            (
                EventType::Entered(TaskState::PendingApproval),
                A2aTaskStatus::InputRequired,
            ),
            (EventType::Entered(TaskState::UnknownResult), A2aTaskStatus::Failed),
            (EventType::Entered(TaskState::OperatorReviewed), A2aTaskStatus::Failed),
            (EventType::Requeued, A2aTaskStatus::Submitted),
            (EventType::LeaseExpired, A2aTaskStatus::Submitted),
            (EventType::Progress, A2aTaskStatus::Working),
        ] {
            assert_eq!(super::event_status(event, TaskState::Succeeded), status);
        }
        assert_eq!(
            super::event_status(EventType::ContentExpired, TaskState::Rejected),
            A2aTaskStatus::Rejected
        );
    }

    #[test]
    fn a_failed_task_maps_to_the_failed_a2a_status() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let endpoint = aether_core::EndpointId::new("endpoint");
        let now = SystemTime::now();
        let request = aether_core::Request {
            id: aether_core::RequestId::new("request"),
            task_id: aether_core::TaskId::new(WIRE_TASK_ID),
            caller_agent_id: AgentId::new("caller"),
            caller_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
            capability_id: CapabilityId::new("review"),
            workspace_id: WorkspaceId::new("workspace"),
            access_mode: AccessMode::ReadOnly,
        };
        let task = crate::execution::Task {
            id: WIRE_TASK_ID.to_owned(),
            skill: "general".to_owned(),
            workspace_id: "workspace".to_owned(),
            remote_text: "remote text".to_owned(),
            context_tokens: 0,
        };
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_scope("direct", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new(WIRE_TASK_ID),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("caller"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("direct"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: crate::proof::request_fingerprint(
                        &request, &task,
                    ),
                    skill: "general".to_owned(),
                    remote_text: "remote text".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must queue");
        store.lease_next("worker", "execution", 10).expect("task must lease");
        store.mark_running(WIRE_TASK_ID, "execution").expect("task must run");
        store.mark_failed(WIRE_TASK_ID, "execution").expect("task must fail");
        let grant = aether_core::Grant {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new("caller"),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new("direct"),
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
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, [grant]),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            scope,
        )
        .expect("node must create");
        let service = NodeService::new(node);
        let mut payload: A2aRequest =
            serde_json::from_value(crate::transport::text_request_payload(
                WIRE_TASK_ID,
                "key",
                WIRE_NONCE,
                "remote text",
            ))
            .expect("payload must decode");
        payload.authorization = Some(A2aAuthorization {
            request_id: "request".to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: endpoint.as_str().to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: A2aClaim {
                id: "claim".to_owned(),
                issuer_agent_id: "issuer".to_owned(),
                subject_agent_id: "caller".to_owned(),
                subject_endpoint_id: endpoint.as_str().to_owned(),
                scope_id: "direct".to_owned(),
                issued_at: seconds(now),
                expires_at: seconds(now + Duration::from_secs(60)),
                signing_key: hex::encode([0; 32]),
                nonce: hex::encode([0; 32]),
                signature: hex::encode([0; 32]),
            },
        });
        let response = service
            .handle(
                "endpoint",
                vec![Frame {
                    version: 0,
                    kind: FrameKind::Request,
                    exchange_id: "00000000-0000-4000-8000-000000000014".to_owned(),
                    scope_id: "direct".to_owned(),
                    task_id: Some(WIRE_TASK_ID.to_owned()),
                    payload: serde_json::to_value(payload)
                        .expect("payload must encode"),
                }],
            )
            .expect("a failed task must still be reported, not treated as an error");
        assert_eq!(
            response.last().expect("result must exist").payload["task"]["status"]["state"],
            "TASK_STATE_FAILED"
        );
    }

    #[test]
    fn rejected_and_cancelled_tasks_map_to_their_a2a_status() {
        type Prepare = fn(&mut NodeStore);
        let cases: [(Prepare, &str); 2] = [
            (
                |store: &mut NodeStore| {
                    store
                        .lease_next("worker", "execution", 10)
                        .expect("task must lease");
                    store
                        .mark_rejected(WIRE_TASK_ID, "execution")
                        .expect("task must reject");
                },
                "TASK_STATE_REJECTED",
            ),
            (
                |store: &mut NodeStore| {
                    store.cancel(WIRE_TASK_ID).expect("task must cancel");
                },
                "TASK_STATE_CANCELED",
            ),
        ];
        for (prepare, expected) in cases {
            let directory = tempfile::tempdir().expect("directory must create");
            let root = directory.path().to_path_buf();
            let workspace = root.join("workspace");
            fs::create_dir_all(&workspace).expect("workspace must create");
            let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
            let endpoint = aether_core::EndpointId::new("endpoint");
            let now = SystemTime::now();
            let request = aether_core::Request {
                id: aether_core::RequestId::new("request"),
                task_id: aether_core::TaskId::new(WIRE_TASK_ID),
                caller_agent_id: AgentId::new("caller"),
                caller_endpoint_id: endpoint.clone(),
                scope_id: ScopeId::new("direct"),
                capability_id: CapabilityId::new("review"),
                workspace_id: WorkspaceId::new("workspace"),
                access_mode: AccessMode::ReadOnly,
            };
            let task = crate::execution::Task {
                id: WIRE_TASK_ID.to_owned(),
                skill: "general".to_owned(),
                workspace_id: "workspace".to_owned(),
                remote_text: "remote text".to_owned(),
                context_tokens: 0,
            };
            let mut store = NodeStore::in_dir(&root).expect("store must open");
            store
                .put_scope("direct", aether_core::ScopeKind::Direct)
                .expect("scope must store");
            store
                .admit(
                    &TaskInput {
                        request: aether_core::Request {
                            task_id: aether_core::TaskId::new(WIRE_TASK_ID),
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
                        request_fingerprint: crate::proof::request_fingerprint(
                            &request, &task,
                        ),
                        skill: "general".to_owned(),
                        remote_text: "remote text".to_owned(),
                    },
                    QueueLimits { global: 1, per_peer: 1 },
                )
                .expect("task must queue");
            prepare(&mut store);
            let grant = aether_core::Grant {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: endpoint.clone(),
                scope_id: ScopeId::new("direct"),
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
            let node = ProofNode::new(
                PolicyEngine::new(Trusted, [grant]),
                store,
                Executor::new(
                    Fake,
                    Limits {
                        workspace: workspace.clone(),
                        max_context_tokens: 32,
                        max_seconds: 1,
                    },
                ),
                QueueLimits { global: 1, per_peer: 1 },
                scope,
            )
            .expect("node must create");
            let service = NodeService::new(node);
            let mut payload: A2aRequest =
                serde_json::from_value(crate::transport::text_request_payload(
                    WIRE_TASK_ID,
                    "key",
                    WIRE_NONCE,
                    "remote text",
                ))
                .expect("payload must decode");
            payload.authorization = Some(A2aAuthorization {
                request_id: "request".to_owned(),
                caller_agent_id: "caller".to_owned(),
                caller_endpoint_id: endpoint.as_str().to_owned(),
                capability_id: "review".to_owned(),
                workspace_id: "workspace".to_owned(),
                access_mode: A2aAccessMode::ReadOnly,
                claim: A2aClaim {
                    id: "claim".to_owned(),
                    issuer_agent_id: "issuer".to_owned(),
                    subject_agent_id: "caller".to_owned(),
                    subject_endpoint_id: endpoint.as_str().to_owned(),
                    scope_id: "direct".to_owned(),
                    issued_at: seconds(now),
                    expires_at: seconds(now + Duration::from_secs(60)),
                    signing_key: hex::encode([0; 32]),
                    nonce: hex::encode([0; 32]),
                    signature: hex::encode([0; 32]),
                },
            });
            let response = service
                .handle(
                    "endpoint",
                    vec![Frame {
                        version: 0,
                        kind: FrameKind::Request,
                        exchange_id: "00000000-0000-4000-8000-000000000015".to_owned(),
                        scope_id: "direct".to_owned(),
                        task_id: Some(WIRE_TASK_ID.to_owned()),
                        payload: serde_json::to_value(payload)
                            .expect("payload must encode"),
                    }],
                )
                .expect("a rejected or cancelled task must still be reported");
            assert_eq!(
                response.last().expect("result must exist").payload["task"]["status"]["state"],
                expected
            );
        }
    }

    fn seconds(time: SystemTime) -> u64 {
        aether_core::time::unix_seconds(time).expect("time must be valid")
    }

    fn event_sequences(response: &[Frame]) -> Vec<i64> {
        response
            .iter()
            .filter(|frame| frame.kind == FrameKind::Event)
            .map(|frame| {
                frame.payload["statusUpdate"]["metadata"]
                    [crate::transport::EVENT_SEQUENCE]
                    .as_i64()
                    .expect("event sequence must exist")
            })
            .collect()
    }

    fn minimal_service(workspace: PathBuf) -> NodeService<Trusted, Fake> {
        let root = workspace.parent().expect("workspace must have a parent").to_owned();
        let store = NodeStore::in_dir(&root).expect("store must open");
        let node = ProofNode::new(
            PolicyEngine::new(Trusted, []),
            store,
            Executor::new(
                Fake,
                Limits { workspace, max_context_tokens: 32, max_seconds: 1 },
            ),
            QueueLimits { global: 1, per_peer: 1 },
            Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
        )
        .expect("node must create");
        NodeService::new(node)
    }

    #[test]
    fn a_workspace_change_is_approved_by_default_with_no_approved_set() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let service = minimal_service(workspace.clone());
        assert!(service.workspace_change_is_approved("any-id", &workspace));
    }

    #[test]
    fn a_workspace_change_is_approved_when_the_id_and_path_match() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let mut service = minimal_service(workspace.clone());
        service.set_approved_workspaces(BTreeMap::from([(
            "reviews".to_owned(),
            workspace.canonicalize().expect("workspace must canonicalize"),
        )]));
        assert!(service.workspace_change_is_approved("reviews", &workspace));
    }

    #[test]
    fn a_workspace_change_is_rejected_for_an_unapproved_id() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let mut service = minimal_service(workspace.clone());
        service.set_approved_workspaces(BTreeMap::from([(
            "reviews".to_owned(),
            workspace.canonicalize().expect("workspace must canonicalize"),
        )]));
        assert!(!service.workspace_change_is_approved("other", &workspace));
    }

    #[test]
    fn a_workspace_change_is_rejected_when_the_path_does_not_match_the_approved_id() {
        let directory = tempfile::tempdir().expect("directory must create");
        let workspace = directory.path().join("workspace");
        let other = directory.path().join("other");
        fs::create_dir_all(&workspace).expect("workspace must create");
        fs::create_dir_all(&other).expect("other workspace must create");
        let mut service = minimal_service(workspace.clone());
        service.set_approved_workspaces(BTreeMap::from([(
            "reviews".to_owned(),
            workspace.canonicalize().expect("workspace must canonicalize"),
        )]));
        assert!(!service.workspace_change_is_approved("reviews", &other));
    }
}
