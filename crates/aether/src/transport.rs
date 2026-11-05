use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    path::Path,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use hex::FromHex;
use serde::{Deserialize, Serialize};
use tokio_util::{
    codec::{FramedRead, FramedWrite, LengthDelimitedCodec},
    sync::CancellationToken,
    task::TaskTracker,
};
use tower::{
    BoxError, ServiceBuilder, ServiceExt, service_fn, util::BoxCloneSyncService,
};
use uuid::Uuid;

use aether_core::{
    AccessMode, AgentId, CapabilityId, Claim, ClaimId, EndpointId, Request, RequestId,
    ScopeId, TaskId, WorkspaceId, time::from_unix_seconds,
};

use crate::{
    data_dir::KeyFileError,
    discovery::{Record, validate_record},
};

pub use a2a::{
    CancelTaskRequest as A2aCancel, JsonRpcError as A2aError, Message as A2aMessage,
    Part as A2aPart, PartContent as A2aPartContent, Role as A2aRole,
    SendMessageResponse as A2aResult, StreamResponse as A2aEvent, Task as A2aTask,
    TaskState as A2aTaskStatus, TaskStatus as A2aTaskStatusValue,
};

pub const ALPN: &[u8] = b"aether/a2a/0";
pub const A2A_PROTOCOL_VERSION: &str = "1.0";
const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_EXCHANGE_BYTES: usize = 4_194_304;
pub const MAX_FRAMES: usize = 64;
const MAX_CONNECTIONS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a server-side connection handler waits, after it has written
/// and finished a response, for the peer's next stream or its close. It is
/// not a read or write budget like `STREAM_TIMEOUT`. A client that polls a
/// task keeps its connection and opens one stream for each poll, so this is
/// longer than the poll interval of the outbox (`outbox::POLL_INTERVAL`).
/// A client that sends one request closes within microseconds of reading
/// its response, and the handler stops at once. The wait only bounds how
/// long a handler holds its `MAX_CONNECTIONS` permit for a peer that stops
/// polling or disconnects uncleanly. Dropping the `Connection` closes it
/// (`noq`'s `ConnectionRef::drop` calls the same `close` a peer-initiated
/// close would), so this does not truncate an already-`finish()`ed
/// response.
const RESPONSE_LINGER: Duration = Duration::from_secs(2);
/// At most half of `PEER_REQUESTS_PER_SECOND`, so one polling task never
/// trips the node's per-peer rate limit and a second concurrent task from
/// the same peer still fits under it.
const TASK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TASK_POLL_DEADLINE: Duration = Duration::from_secs(120);
const PEER_REQUESTS_PER_SECOND: u32 = 8;

type PeerLimiter = DefaultKeyedRateLimiter<String>;
type HandlerService = BoxCloneSyncService<(String, Vec<Frame>), Vec<Frame>, BoxError>;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Request,
    Event,
    Cancel,
    Result,
    Error,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Frame {
    pub version: u8,
    pub kind: FrameKind,
    pub exchange_id: String,
    pub scope_id: String,
    pub task_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct A2aRequest {
    pub a2a_version: String,
    pub idempotency_key: String,
    pub request_nonce: String,
    pub task: A2aTask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<A2aAuthorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_sequence: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct A2aAuthorization {
    pub request_id: String,
    pub caller_agent_id: String,
    pub caller_endpoint_id: String,
    pub capability_id: String,
    pub workspace_id: String,
    pub access_mode: A2aAccessMode,
    pub claim: A2aClaim,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2aAccessMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct A2aClaim {
    pub id: String,
    pub issuer_agent_id: String,
    pub subject_agent_id: String,
    pub subject_endpoint_id: String,
    pub scope_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub signing_key: String,
    pub nonce: String,
    pub signature: String,
}

impl From<AccessMode> for A2aAccessMode {
    fn from(mode: AccessMode) -> Self {
        match mode {
            AccessMode::ReadOnly => Self::ReadOnly,
            AccessMode::ReadWrite => Self::ReadWrite,
        }
    }
}

impl From<A2aAccessMode> for AccessMode {
    fn from(mode: A2aAccessMode) -> Self {
        match mode {
            A2aAccessMode::ReadOnly => Self::ReadOnly,
            A2aAccessMode::ReadWrite => Self::ReadWrite,
        }
    }
}

impl A2aAuthorization {
    /// What a caller sends with each request under the access `input`, whose
    /// signed claim is `claim`. It names the first capability and workspace
    /// of `input`; each request sets its own request id.
    #[must_use]
    pub fn for_access(input: &crate::identity::AccessInput, claim: &Claim) -> Self {
        Self {
            request_id: "pending".to_owned(),
            caller_agent_id: input.subject_agent_id.clone(),
            caller_endpoint_id: input.subject_endpoint_id.clone(),
            capability_id: input.capabilities.first().cloned().unwrap_or_default(),
            workspace_id: input.workspaces.first().cloned().unwrap_or_default(),
            access_mode: input.access_mode.clone().into(),
            claim: claim.into(),
        }
    }
}

impl TryFrom<&A2aClaim> for Claim {
    type Error = TransportError;

    fn try_from(claim: &A2aClaim) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ClaimId::new(claim.id.clone()),
            issuer_agent_id: AgentId::new(claim.issuer_agent_id.clone()),
            subject_agent_id: AgentId::new(claim.subject_agent_id.clone()),
            subject_endpoint_id: EndpointId::new(claim.subject_endpoint_id.clone()),
            scope_id: ScopeId::new(claim.scope_id.clone()),
            issued_at: from_unix_seconds(claim.issued_at)
                .ok_or(TransportError::BadFrame)?,
            expires_at: from_unix_seconds(claim.expires_at)
                .ok_or(TransportError::BadFrame)?,
            signing_key: decode_key(&claim.signing_key)?,
            nonce: decode_key(&claim.nonce)?,
            signature: hex::decode(&claim.signature)
                .map_err(|_| TransportError::BadFrame)?,
        })
    }
}

impl From<&Claim> for A2aClaim {
    fn from(claim: &Claim) -> Self {
        let seconds = |time| aether_core::time::unix_seconds(time).unwrap_or(0);
        Self {
            id: claim.id.as_str().to_owned(),
            issuer_agent_id: claim.issuer_agent_id.as_str().to_owned(),
            subject_agent_id: claim.subject_agent_id.as_str().to_owned(),
            subject_endpoint_id: claim.subject_endpoint_id.as_str().to_owned(),
            scope_id: claim.scope_id.as_str().to_owned(),
            issued_at: seconds(claim.issued_at),
            expires_at: seconds(claim.expires_at),
            signing_key: hex::encode(claim.signing_key),
            nonce: hex::encode(claim.nonce),
            signature: hex::encode(&claim.signature),
        }
    }
}

#[must_use]
pub fn text_request_payload(
    task_id: &str,
    idempotency_key: &str,
    request_nonce: &str,
    text: &str,
) -> serde_json::Value {
    payload(A2aRequest {
        a2a_version: A2A_PROTOCOL_VERSION.to_owned(),
        idempotency_key: idempotency_key.to_owned(),
        request_nonce: request_nonce.to_owned(),
        task: A2aTask {
            id: task_id.to_owned(),
            context_id: task_id.to_owned(),
            status: A2aTaskStatusValue {
                state: A2aTaskStatus::Submitted,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: Some(vec![A2aMessage {
                message_id: Uuid::new_v4().to_string(),
                context_id: Some(task_id.to_owned()),
                task_id: Some(task_id.to_owned()),
                role: A2aRole::User,
                parts: vec![A2aPart::text(text)],
                metadata: None,
                extensions: None,
                reference_task_ids: None,
            }]),
            metadata: None,
        },
        authorization: None,
        last_event_sequence: None,
    })
}

impl A2aRequest {
    pub fn policy_data(
        &self,
        scope_id: &str,
    ) -> Result<(Request, Claim), TransportError> {
        let authorization =
            self.authorization.as_ref().ok_or(TransportError::PermissionDenied)?;
        if authorization.claim.scope_id != scope_id
            || authorization.caller_agent_id.is_empty()
            || authorization.caller_endpoint_id.is_empty()
            || authorization.capability_id.is_empty()
            || authorization.workspace_id.is_empty()
        {
            return Err(TransportError::BadFrame);
        }
        let request = Request {
            id: RequestId::new(authorization.request_id.clone()),
            task_id: TaskId::new(self.task.id.clone()),
            caller_agent_id: AgentId::new(authorization.caller_agent_id.clone()),
            caller_endpoint_id: EndpointId::new(
                authorization.caller_endpoint_id.clone(),
            ),
            scope_id: ScopeId::new(scope_id),
            capability_id: CapabilityId::new(authorization.capability_id.clone()),
            workspace_id: WorkspaceId::new(authorization.workspace_id.clone()),
            access_mode: authorization.access_mode.into(),
        };
        Ok((request, Claim::try_from(&authorization.claim)?))
    }
}

fn decode_key(value: &str) -> Result<[u8; 32], TransportError> {
    <[u8; 32]>::from_hex(value).map_err(|_| TransportError::BadFrame)
}

/// The metadata key of an event's sequence in the task's event journal.
pub const EVENT_SEQUENCE: &str = "aetherEventSequence";
/// The metadata key of the last event sequence a result page delivers.
pub const EVENT_CURSOR: &str = "aetherEventCursor";
/// The metadata key that marks a result page with more events after it.
pub const MORE_EVENTS: &str = "aetherMoreEvents";

#[must_use]
pub fn event_payload(
    task_id: &str,
    status: A2aTaskStatus,
    sequence: Option<i64>,
) -> serde_json::Value {
    payload(A2aEvent::StatusUpdate(a2a::TaskStatusUpdateEvent {
        task_id: task_id.to_owned(),
        context_id: task_id.to_owned(),
        status: A2aTaskStatusValue { state: status, message: None, timestamp: None },
        metadata: sequence.map(|sequence| {
            HashMap::from([(
                EVENT_SEQUENCE.to_owned(),
                serde_json::Value::from(sequence),
            )])
        }),
    }))
}

#[must_use]
pub fn text_result_payload(
    task_id: &str,
    status: A2aTaskStatus,
    text: &str,
) -> serde_json::Value {
    result_payload(task_id, status, text, None)
}

/// Like [`text_result_payload`], but for a bounded page of a task's progress.
///
/// `event_cursor` names the last event sequence delivered so far, and
/// `more_events` marks whether the durable event journal holds events
/// beyond this page. A client that stops as soon as it sees a terminal
/// status, without checking `more_events`, can otherwise silently drop
/// events a truncated page omitted.
#[must_use]
pub fn text_result_payload_with_progress(
    task_id: &str,
    status: A2aTaskStatus,
    text: &str,
    event_cursor: i64,
    more_events: bool,
) -> serde_json::Value {
    let mut metadata = HashMap::from([(
        EVENT_CURSOR.to_owned(),
        serde_json::Value::from(event_cursor),
    )]);
    if more_events {
        metadata.insert(MORE_EVENTS.to_owned(), serde_json::Value::from(true));
    }
    result_payload(task_id, status, text, Some(metadata))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "a2a's `TaskState` is not `Copy`; callers build it for this call"
)]
fn result_payload(
    task_id: &str,
    status: A2aTaskStatus,
    text: &str,
    metadata: Option<HashMap<String, serde_json::Value>>,
) -> serde_json::Value {
    payload(A2aResult::Task(A2aTask {
        id: task_id.to_owned(),
        context_id: task_id.to_owned(),
        status: A2aTaskStatusValue {
            state: status.clone(),
            message: None,
            timestamp: None,
        },
        artifacts: (status == A2aTaskStatus::Completed).then(|| {
            vec![a2a::Artifact {
                artifact_id: Uuid::new_v4().to_string(),
                name: None,
                description: None,
                parts: vec![A2aPart::text(text)],
                metadata: None,
                extensions: None,
            }]
        }),
        history: None,
        metadata,
    }))
}

/// The JSON form of an a2a value. The a2a types are plain structs with
/// string map keys, which always serialize; were one ever to fail, the null
/// payload is refused by the peer's own frame checks.
fn payload(value: impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TransportError {
    #[error("exchange exceeds the configured limit")]
    ExchangeTooLarge,
    #[error("frame is invalid")]
    BadFrame,
    #[error("protocol version is unsupported")]
    UnsupportedVersion,
    #[error("frame order is invalid")]
    InvalidOrder,
    #[error("exchange was already used")]
    Replay,
    #[error("node is overloaded")]
    Overloaded,
    #[error("request is not authorized")]
    PermissionDenied,
    #[error("authenticated endpoint does not match the claim")]
    EndpointMismatch,
    #[error("request scope does not match the claim")]
    ScopeMismatch,
    #[error("endpoint key operation failed")]
    Key,
    #[error("input or output operation failed")]
    Io,
    #[error("network address is invalid")]
    Address,
    #[error("network operation timed out")]
    Timeout,
    #[error("network operation failed")]
    Network(String),
    /// The task's durable state (`unknown_result` or `operator_reviewed`)
    /// requires operator review before it can be retried or replayed.
    #[error("task requires operator review")]
    OperatorReviewRequired(String),
    /// The task succeeded, but its retained output has already expired.
    #[error("task result has expired")]
    ResultExpired,
}

/// The checks every exchange shares: a count within bounds, one version,
/// one exchange and scope, and a well-formed task id on every frame.
/// Returns the first frame.
fn validate_header(frames: &[Frame]) -> Result<&Frame, TransportError> {
    let Some((first, _)) = frames.split_first() else {
        return Err(TransportError::InvalidOrder);
    };
    if frames.len() > MAX_FRAMES {
        return Err(TransportError::InvalidOrder);
    }
    if frames.iter().any(|frame| frame.version != 0) {
        return Err(TransportError::UnsupportedVersion);
    }
    if frames.iter().any(|frame| {
        frame.exchange_id != first.exchange_id || frame.scope_id != first.scope_id
    }) {
        return Err(TransportError::InvalidOrder);
    }
    if !valid_exchange_id(&first.exchange_id)
        || first.scope_id.is_empty()
        || frames.iter().any(|frame| {
            frame.task_id.as_deref().is_none_or(|task_id| {
                task_id.is_empty() || Uuid::parse_str(task_id).is_err()
            })
        })
    {
        return Err(TransportError::BadFrame);
    }
    Ok(first)
}

/// Whether `frames` are events closed by one result or error.
fn events_then_outcome(frames: &[Frame]) -> bool {
    frames.split_last().is_some_and(|(last, events)| {
        matches!(last.kind, FrameKind::Result | FrameKind::Error)
            && events.iter().all(|frame| frame.kind == FrameKind::Event)
    })
}

fn validate_order(frames: &[Frame]) -> Result<(), TransportError> {
    let first = validate_header(frames)?;
    let in_order = match first.kind {
        FrameKind::Event => events_then_outcome(frames),
        // A request, a cancel, a result, or an error travels alone.
        FrameKind::Request
        | FrameKind::Cancel
        | FrameKind::Result
        | FrameKind::Error => frames.len() == 1,
    };
    if !in_order {
        return Err(TransportError::InvalidOrder);
    }
    validate_payloads(frames)
}

fn validate_payloads(frames: &[Frame]) -> Result<(), TransportError> {
    for frame in frames {
        match frame.kind {
            FrameKind::Request => {
                let payload: A2aRequest = decode_payload(&frame.payload)?;
                validate_task_parts(
                    frame.payload.get("task").unwrap_or(&serde_json::Value::Null),
                )?;
                if payload.a2a_version != A2A_PROTOCOL_VERSION {
                    return Err(TransportError::UnsupportedVersion);
                }
                if payload.authorization.as_ref().is_some_and(|authorization| {
                    Uuid::parse_str(&authorization.request_id).is_err()
                }) {
                    return Err(TransportError::BadFrame);
                }
                if payload.idempotency_key.is_empty()
                    || Uuid::parse_str(&payload.request_nonce).is_err()
                    || payload.task.id != frame.task_id.as_deref().unwrap_or_default()
                    || payload.task.status.state != A2aTaskStatus::Submitted
                    || !matches!(
                        payload.task.history.as_deref(),
                        Some([A2aMessage {
                            role: A2aRole::User,
                            parts,
                            ..
                        }]) if matches!(
                            parts.as_slice(),
                            [A2aPart { content: A2aPartContent::Text(_), .. }]
                        )
                    )
                {
                    return Err(TransportError::BadFrame);
                }
            }
            FrameKind::Event => {
                let raw = only_field(&frame.payload, "statusUpdate")?;
                let event: a2a::TaskStatusUpdateEvent = decode_payload(raw)?;
                validate_parts(raw.pointer("/status/message/parts"))?;
                if event.task_id != frame.task_id.as_deref().unwrap_or_default()
                    || event.context_id != event.task_id
                    || !matches!(
                        event.status.state,
                        A2aTaskStatus::Submitted
                            | A2aTaskStatus::Working
                            | A2aTaskStatus::Completed
                            | A2aTaskStatus::Failed
                            | A2aTaskStatus::Canceled
                    )
                {
                    return Err(TransportError::BadFrame);
                }
            }
            FrameKind::Cancel => {
                let payload: A2aCancel = decode_payload(&frame.payload)?;
                if payload.id != frame.task_id.as_deref().unwrap_or_default() {
                    return Err(TransportError::BadFrame);
                }
            }
            FrameKind::Result => {
                let raw = only_field(&frame.payload, "task")?;
                let task: A2aTask = decode_payload(raw)?;
                validate_task_parts(raw)?;
                if task.id != frame.task_id.as_deref().unwrap_or_default()
                    || !matches!(
                        task.status.state,
                        A2aTaskStatus::Submitted
                            | A2aTaskStatus::InputRequired
                            | A2aTaskStatus::Working
                            | A2aTaskStatus::Completed
                            | A2aTaskStatus::Failed
                            | A2aTaskStatus::Rejected
                            | A2aTaskStatus::Canceled
                    )
                    || (task.status.state == A2aTaskStatus::Completed
                        && task.artifacts.as_ref().is_none_or(Vec::is_empty))
                {
                    return Err(TransportError::BadFrame);
                }
            }
            FrameKind::Error => {
                let payload: A2aError = decode_payload(&frame.payload)?;
                if payload.message.is_empty() {
                    return Err(TransportError::BadFrame);
                }
            }
        }
    }
    Ok(())
}

/// Decodes `payload` and refuses any field that `T` does not name, at any
/// depth that `T` decodes itself. The a2a types do not deny unknown fields,
/// and a peer's unknown field must fail rather than vanish.
fn decode_payload<'a, T: Deserialize<'a>>(
    payload: &'a serde_json::Value,
) -> Result<T, TransportError> {
    let mut unknown = false;
    let decoded = serde_ignored::deserialize(payload, |_| unknown = true)
        .map_err(|_| TransportError::BadFrame)?;
    if unknown { Err(TransportError::BadFrame) } else { Ok(decoded) }
}

/// The value of an object's single field `name`. a2a's response enums read
/// their variant through a map and a second decoder, so their wrapper key
/// and everything under it are checked here and by decoding the inner type.
fn only_field<'a>(
    value: &'a serde_json::Value,
    name: &str,
) -> Result<&'a serde_json::Value, TransportError> {
    match value.as_object() {
        Some(object) if object.len() == 1 => {
            object.get(name).ok_or(TransportError::BadFrame)
        }
        _ => Err(TransportError::BadFrame),
    }
}

/// Checks every part a task holds. a2a reads a part through a map, so no
/// typed decode sees its keys.
fn validate_task_parts(task: &serde_json::Value) -> Result<(), TransportError> {
    let holders = task.get("history").and_then(serde_json::Value::as_array);
    let artifacts = task.get("artifacts").and_then(serde_json::Value::as_array);
    for holder in holders.into_iter().chain(artifacts).flatten() {
        validate_parts(holder.get("parts"))?;
    }
    validate_parts(task.pointer("/status/message/parts"))
}

fn validate_parts(parts: Option<&serde_json::Value>) -> Result<(), TransportError> {
    let parts = parts.and_then(serde_json::Value::as_array).into_iter().flatten();
    for part in parts {
        let part = part.as_object().ok_or(TransportError::BadFrame)?;
        let known = ["text", "raw", "url", "data", "filename", "mediaType", "metadata"];
        let content = ["text", "raw", "url", "data"];
        if part.keys().any(|key| !known.contains(&key.as_str()))
            || content.iter().filter(|key| part.contains_key(**key)).count() != 1
        {
            return Err(TransportError::BadFrame);
        }
    }
    Ok(())
}

fn valid_exchange_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

pub async fn bind_node(
    data_dir: impl AsRef<Path>,
    port: u16,
    relay_url: Option<&str>,
) -> Result<iroh::Endpoint, TransportError> {
    let key = crate::data_dir::DataDir::new(data_dir.as_ref())
        .endpoint_key()
        .map_err(key_error)?;
    let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()]);
    let builder = if let Some(relay_url) = relay_url {
        let relay_url = relay_url.parse().map_err(|_| TransportError::Address)?;
        builder.relay_mode(iroh::RelayMode::custom([relay_url]))
    } else {
        // An empty map rather than `RelayMode::Disabled`: a disabled endpoint
        // never connects to a relay that `switch_relay` adds later.
        builder.relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
    };
    let endpoint = builder
        .bind_addr(SocketAddr::from(([0, 0, 0, 0], port)))
        .map_err(|_| TransportError::Address)?
        .bind()
        .await
        .map_err(|error| TransportError::Network(error.to_string()))?;
    if relay_url.is_some() {
        tokio::time::timeout(Duration::from_secs(20), endpoint.online())
            .await
            .map_err(|_| TransportError::Timeout)?;
    }
    Ok(endpoint)
}

/// Makes `relay` the relay a running `endpoint` listens through.
///
/// It replaces `previous`; `None` leaves the endpoint on the local network
/// only. A new relay must come online within a few seconds, so an admin page
/// gets its answer inside its own request limit.
pub async fn switch_relay(
    endpoint: &iroh::Endpoint,
    previous: Option<&str>,
    relay: Option<&str>,
) -> Result<(), TransportError> {
    let relay = relay
        .map(str::parse::<iroh::RelayUrl>)
        .transpose()
        .map_err(|_| TransportError::Address)?;
    if let Some(previous) = previous.and_then(|url| url.parse::<iroh::RelayUrl>().ok())
        && Some(&previous) != relay.as_ref()
    {
        endpoint.remove_relay(&previous).await;
    }
    if let Some(relay) = relay {
        endpoint
            .insert_relay(relay.clone(), Arc::new(iroh::RelayConfig::from(relay)))
            .await;
        tokio::time::timeout(Duration::from_secs(8), endpoint.online())
            .await
            .map_err(|_| TransportError::Timeout)?;
    }
    Ok(())
}

pub fn router<F>(endpoint: iroh::Endpoint, handler: F) -> iroh::protocol::Router
where
    F: Fn(&str, Vec<Frame>) -> Result<Vec<Frame>, TransportError>
        + Send
        + Sync
        + 'static,
{
    iroh::protocol::Router::builder(endpoint)
        .accept(
            ALPN,
            AetherProtocol {
                handler: handler_service(handler),
                limiter: Arc::new(peer_limiter()),
                permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
                cancellation: CancellationToken::new(),
                tasks: TaskTracker::new(),
            },
        )
        .spawn()
}

struct AetherProtocol {
    handler: HandlerService,
    limiter: Arc<PeerLimiter>,
    permits: Arc<tokio::sync::Semaphore>,
    cancellation: CancellationToken,
    tasks: TaskTracker,
}

impl std::fmt::Debug for AetherProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AetherProtocol")
    }
}

impl iroh::protocol::ProtocolHandler for AetherProtocol {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            connection.close(0u32.into(), b"overload");
            return Ok(());
        };
        let handler = self.handler.clone();
        let limiter = Arc::clone(&self.limiter);
        let cancellation = self.cancellation.clone();
        self.tasks
            .spawn(async move {
                let _permit = permit;
                tokio::select! {
                    () = cancellation.cancelled() => Ok(()),
                    result = serve_connected(connection, handler, limiter) => result,
                }
            })
            .await
            .map_err(iroh::protocol::AcceptError::from_err)?
            .map_err(iroh::protocol::AcceptError::from_err)
    }

    async fn shutdown(&self) {
        self.cancellation.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// Serves the streams of one connection one after the other, so a peer
/// cannot run more handlers through one connection than through one
/// `MAX_CONNECTIONS` permit. Each stream goes through the per-peer limiter.
/// The first stream must come within `STREAM_TIMEOUT`; after a response the
/// connection ends when the peer closes it or opens no stream within
/// `RESPONSE_LINGER`.
async fn serve_connected(
    connection: iroh::endpoint::Connection,
    handler: HandlerService,
    limiter: Arc<PeerLimiter>,
) -> Result<(), TransportError> {
    let authenticated_endpoint = connection.remote_id().to_string();
    let mut first = true;
    loop {
        let wait = if first { STREAM_TIMEOUT } else { RESPONSE_LINGER };
        let (send, receive) =
            match tokio::time::timeout(wait, connection.accept_bi()).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) if first => {
                    return Err(TransportError::Network(error.to_string()));
                }
                Err(_) if first => return Err(TransportError::Timeout),
                Ok(Err(_)) | Err(_) => return Ok(()),
            };
        first = false;
        serve_stream(&authenticated_endpoint, send, receive, &handler, &limiter)
            .await?;
    }
}

async fn serve_stream(
    authenticated_endpoint: &str,
    mut send: iroh::endpoint::SendStream,
    mut receive: iroh::endpoint::RecvStream,
    handler: &HandlerService,
    limiter: &PeerLimiter,
) -> Result<(), TransportError> {
    let request = read_frames(&mut receive).await?;
    if limiter.check_key(&authenticated_endpoint.to_owned()).is_err() {
        let response = error_response(&request, &TransportError::Overloaded);
        write_frames(&mut send, &response).await?;
        return send
            .finish()
            .map_err(|error| TransportError::Network(error.to_string()));
    }
    let request_frame = request.first().ok_or(TransportError::BadFrame)?;
    let span = tracing::info_span!(
        "aether_exchange",
        peer = %authenticated_endpoint,
        scope = %request_frame.scope_id,
        exchange = %request_frame.exchange_id,
        task = %request_frame.task_id.as_deref().unwrap_or_default(),
    );
    let _enter = span.enter();
    let response = handler
        .clone()
        .oneshot((authenticated_endpoint.to_owned(), request.clone()))
        .await
        .map_err(handler_error);
    let response = match response {
        Ok(response) if validate_response(&request, &response).is_ok() => response,
        Ok(_) => error_response(&request, &TransportError::InvalidOrder),
        Err(error) => error_response(&request, &error),
    };
    write_frames(&mut send, &response).await?;
    send.finish().map_err(|error| TransportError::Network(error.to_string()))
}

fn handler_service<F>(handler: F) -> HandlerService
where
    F: Fn(&str, Vec<Frame>) -> Result<Vec<Frame>, TransportError>
        + Send
        + Sync
        + 'static,
{
    let handler = Arc::new(handler);
    BoxCloneSyncService::new(
        ServiceBuilder::new()
            .load_shed()
            .concurrency_limit(MAX_CONNECTIONS)
            .timeout(HANDLER_TIMEOUT)
            .service(service_fn(move |(peer, frames): (String, Vec<Frame>)| {
                let handler = Arc::clone(&handler);
                async move {
                    tokio::task::spawn_blocking(move || handler(&peer, frames))
                        .await
                        .map_err(|_| {
                            TransportError::Network("handler stopped".to_owned())
                        })?
                }
            })),
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an adapter for `map_err`, which hands over the error by value"
)]
fn handler_error(error: BoxError) -> TransportError {
    if error.is::<tower::load_shed::error::Overloaded>() {
        TransportError::Overloaded
    } else if error.is::<tower::timeout::error::Elapsed>() {
        TransportError::Timeout
    } else if let Some(error) = error.downcast_ref::<TransportError>() {
        error.clone()
    } else {
        TransportError::Network("handler failed".to_owned())
    }
}

fn peer_limiter() -> PeerLimiter {
    // The constant is not zero; the fallback only keeps the conversion total.
    let requests = std::num::NonZeroU32::new(PEER_REQUESTS_PER_SECOND)
        .unwrap_or(std::num::NonZeroU32::MIN);
    RateLimiter::keyed(Quota::per_second(requests))
}

fn error_response(request: &[Frame], error: &TransportError) -> Vec<Frame> {
    let Some(request) = request.first() else {
        return Vec::new();
    };
    vec![Frame {
        version: 0,
        kind: FrameKind::Error,
        exchange_id: request.exchange_id.clone(),
        scope_id: request.scope_id.clone(),
        task_id: request.task_id.clone(),
        payload: payload(A2aError {
            code: a2a_error_code(error),
            message: a2a_error_message(error).to_owned(),
            data: a2a_error_data(error),
        }),
    }]
}

// JSON-RPC reserves -32000 through -32099 for implementation-defined server
// errors; a2a-lf's own codes occupy -32001 through -32009, so these two
// aether-specific outcomes use the next free values in that range.
const OPERATOR_REVIEW_REQUIRED_CODE: i32 = -32010;
const RESULT_EXPIRED_CODE: i32 = -32011;

const fn a2a_error_code(error: &TransportError) -> i32 {
    match error {
        TransportError::UnsupportedVersion => a2a::error_code::VERSION_NOT_SUPPORTED,
        TransportError::PermissionDenied | TransportError::Overloaded => {
            a2a::error_code::INVALID_REQUEST
        }
        TransportError::Replay
        | TransportError::ExchangeTooLarge
        | TransportError::BadFrame
        | TransportError::InvalidOrder
        | TransportError::EndpointMismatch
        | TransportError::ScopeMismatch
        | TransportError::Key
        | TransportError::Io
        | TransportError::Address => a2a::error_code::INVALID_PARAMS,
        TransportError::Timeout | TransportError::Network(_) => {
            a2a::error_code::INTERNAL_ERROR
        }
        TransportError::OperatorReviewRequired(_) => OPERATOR_REVIEW_REQUIRED_CODE,
        TransportError::ResultExpired => RESULT_EXPIRED_CODE,
    }
}

/// Safe state data a caller can act on without parsing the error message.
/// Never includes task input or output, matching the redaction rules that
/// already govern logs, UI, and MCP responses.
fn a2a_error_data(error: &TransportError) -> Option<serde_json::Value> {
    match error {
        TransportError::OperatorReviewRequired(state) => {
            Some(serde_json::json!({ "state": state }))
        }
        _ => None,
    }
}

const fn a2a_error_message(error: &TransportError) -> &'static str {
    match error {
        TransportError::UnsupportedVersion => "unsupported version",
        TransportError::PermissionDenied => "permission denied",
        TransportError::Replay => "replay rejected",
        TransportError::Overloaded => "node overloaded",
        TransportError::Timeout => "operation timed out",
        TransportError::ExchangeTooLarge => "exchange limit exceeded",
        TransportError::BadFrame | TransportError::InvalidOrder => "invalid request",
        TransportError::EndpointMismatch | TransportError::ScopeMismatch => {
            "request rejected"
        }
        TransportError::Key
        | TransportError::Io
        | TransportError::Address
        | TransportError::Network(_) => "internal error",
        TransportError::OperatorReviewRequired(_) => "operator review required",
        TransportError::ResultExpired => "task result has expired",
    }
}

fn frame_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_type::<u32>()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_codec()
}

async fn read_frames<R>(receive: &mut R) -> Result<Vec<Frame>, TransportError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let frames = async {
        let mut reader = FramedRead::new(receive, frame_codec());
        let mut frames = Vec::new();
        let mut total = 0usize;
        while let Some(bytes) = reader.next().await {
            let bytes = bytes.map_err(|_| TransportError::BadFrame)?;
            total = framed_total(total, bytes.len())?;
            if total > MAX_EXCHANGE_BYTES || frames.len() == MAX_FRAMES {
                return Err(TransportError::ExchangeTooLarge);
            }
            let frame =
                serde_json::from_slice(&bytes).map_err(|_| TransportError::BadFrame)?;
            frames.push(frame);
        }
        validate_order(&frames)?;
        Ok(frames)
    };
    tokio::time::timeout(STREAM_TIMEOUT, frames)
        .await
        .map_err(|_| TransportError::Timeout)?
}

/// The exchange size after one more frame of `length` bytes, counting its
/// 4-byte length prefix.
fn framed_total(total: usize, length: usize) -> Result<usize, TransportError> {
    length
        .checked_add(4)
        .and_then(|framed| total.checked_add(framed))
        .ok_or(TransportError::ExchangeTooLarge)
}

async fn write_frames<W>(send: &mut W, frames: &[Frame]) -> Result<(), TransportError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    validate_order(frames)?;
    let write = async {
        let mut writer = FramedWrite::new(send, frame_codec());
        let mut total = 0usize;
        for frame in frames {
            let bytes =
                serde_json::to_vec(frame).map_err(|_| TransportError::BadFrame)?;
            total = framed_total(total, bytes.len())?;
            if total > MAX_EXCHANGE_BYTES {
                return Err(TransportError::ExchangeTooLarge);
            }
            writer
                .send(Bytes::from(bytes))
                .await
                .map_err(|error| TransportError::Network(error.to_string()))?;
        }
        Ok(())
    };
    tokio::time::timeout(STREAM_TIMEOUT, write)
        .await
        .map_err(|_| TransportError::Timeout)?
}

fn validate_response(
    request: &[Frame],
    response: &[Frame],
) -> Result<(), TransportError> {
    let request = request.first().ok_or(TransportError::BadFrame)?;
    if response.is_empty()
        || response.iter().any(|frame| {
            frame.exchange_id != request.exchange_id
                || frame.scope_id != request.scope_id
                || frame.task_id != request.task_id
        })
    {
        return Err(TransportError::InvalidOrder);
    }
    validate_response_frames(response)
}

fn validate_response_frames(frames: &[Frame]) -> Result<(), TransportError> {
    validate_header(frames)?;
    if !events_then_outcome(frames) {
        return Err(TransportError::InvalidOrder);
    }
    validate_payloads(frames)
}

pub fn ip_endpoint_addr(
    endpoint_id: &str,
    address: &str,
) -> Result<iroh::EndpointAddr, TransportError> {
    let remote_id = endpoint_id.parse().map_err(|_| TransportError::Address)?;
    let remote_address = address
        .to_socket_addrs()
        .map_err(|_| TransportError::Address)?
        .next()
        .ok_or(TransportError::Address)?;
    Ok(iroh::EndpointAddr::from_parts(
        remote_id,
        [iroh::TransportAddr::Ip(remote_address)],
    ))
}

pub fn relay_endpoint_addr(
    endpoint_id: &str,
    relay_url: &str,
) -> Result<iroh::EndpointAddr, TransportError> {
    let remote_id = endpoint_id.parse().map_err(|_| TransportError::Address)?;
    let relay_url: iroh::RelayUrl =
        relay_url.parse().map_err(|_| TransportError::Address)?;
    Ok(iroh::EndpointAddr::from_parts(
        remote_id,
        [iroh::TransportAddr::Relay(relay_url)],
    ))
}

/// One QUIC connection to one host, kept while one task is followed.
///
/// The first exchange opens the connection. Each later exchange opens one
/// new stream on it. When a stream on a kept connection fails, the link
/// connects again one time and sends the request again. Thus only a request
/// after the first one can go twice, and those requests repeat the task ID
/// and idempotency key of the first one. A host that serves one stream for
/// each connection (a node before connection reuse) closes the kept
/// connection after its answer; from then on the link opens a new
/// connection for each exchange and closes it after the answer, as before.
#[derive(Debug, Default)]
pub struct Link {
    connection: Option<iroh::endpoint::Connection>,
    one_stream_each: bool,
}

impl Link {
    /// One exchange of `request` with `remote` on `endpoint`: the snapshot
    /// that the host sends now. It does not poll.
    pub async fn exchange(
        &mut self,
        endpoint: &iroh::Endpoint,
        remote: iroh::EndpointAddr,
        request: Frame,
    ) -> Result<Vec<Frame>, TransportError> {
        let response = if let Some(connection) = self.connection.take() {
            if let Ok(response) = exchange_stream(&connection, request.clone()).await {
                self.connection = Some(connection);
                response
            } else {
                // The host closed the kept connection: an older host serves
                // one stream for each connection.
                connection.close(0u32.into(), b"done");
                self.one_stream_each = true;
                self.connect_and_exchange(endpoint, remote, request.clone()).await?
            }
        } else {
            self.connect_and_exchange(endpoint, remote, request.clone()).await?
        };
        validate_response(&[request], &response)?;
        Ok(response)
    }

    async fn connect_and_exchange(
        &mut self,
        endpoint: &iroh::Endpoint,
        remote: iroh::EndpointAddr,
        request: Frame,
    ) -> Result<Vec<Frame>, TransportError> {
        let connection =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, endpoint.connect(remote, ALPN))
                .await
                .map_err(|_| TransportError::Timeout)?
                .map_err(|error| TransportError::Network(error.to_string()))?;
        let response = exchange_stream(&connection, request).await;
        if response.is_ok() && !self.one_stream_each {
            self.connection = Some(connection);
        } else {
            connection.close(0u32.into(), b"done");
        }
        response
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.close(0u32.into(), b"done");
        }
    }
}

pub async fn request_once(
    data_dir: impl AsRef<Path>,
    endpoint_id: &str,
    address: &str,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let remote = ip_endpoint_addr(endpoint_id, address)?;
    request_once_at(data_dir, remote, None, request).await
}

fn endpoint_addr_from_record(
    record: &Record,
) -> Result<iroh::EndpointAddr, TransportError> {
    validate_record(record).map_err(|_| TransportError::Address)?;
    let remote_id = record.endpoint_id.parse().map_err(|_| TransportError::Address)?;
    let addresses = record
        .addresses
        .iter()
        .map(|address| {
            address
                .parse::<SocketAddr>()
                .map(iroh::TransportAddr::Ip)
                .map_err(|_| TransportError::Address)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(iroh::EndpointAddr::from_parts(remote_id, addresses))
}

pub async fn request_once_record(
    data_dir: impl AsRef<Path>,
    record: &Record,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    request_once_at(data_dir, endpoint_addr_from_record(record)?, None, request).await
}

pub async fn request_once_via_relay(
    data_dir: impl AsRef<Path>,
    endpoint_id: &str,
    relay_url: &str,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let remote = relay_endpoint_addr(endpoint_id, relay_url)?;
    let relay_url: iroh::RelayUrl =
        relay_url.parse().map_err(|_| TransportError::Address)?;
    request_once_at(data_dir, remote, Some(relay_url.to_string()), request).await
}

async fn request_once_at(
    data_dir: impl AsRef<Path>,
    remote: iroh::EndpointAddr,
    relay_url: Option<String>,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let endpoint = bind_node(data_dir, 0, relay_url.as_deref()).await?;
    let result = request_with_polling_on(&endpoint, remote, request).await;
    endpoint.close().await;
    result
}

#[must_use]
pub fn response_exit_code(response: &[Frame]) -> std::process::ExitCode {
    // Only a completed task is success: a failure, a task still running, and
    // a response that cannot be read are not.
    match response_task_status(response) {
        Ok(A2aTaskStatus::Completed) => std::process::ExitCode::SUCCESS,
        Err(_)
        | Ok(
            A2aTaskStatus::Failed
            | A2aTaskStatus::Canceled
            | A2aTaskStatus::Rejected
            | A2aTaskStatus::InputRequired
            | A2aTaskStatus::Unspecified
            | A2aTaskStatus::AuthRequired
            | A2aTaskStatus::Submitted
            | A2aTaskStatus::Working,
        ) => std::process::ExitCode::from(1),
    }
}

#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a2a's `TaskState` is not `Copy`; callers read it for this call"
)]
const fn is_terminal_task_status(status: A2aTaskStatus) -> bool {
    matches!(
        status,
        A2aTaskStatus::Completed
            | A2aTaskStatus::Failed
            | A2aTaskStatus::Canceled
            | A2aTaskStatus::Rejected
    )
}

pub fn response_task_status(
    response: &[Frame],
) -> Result<A2aTaskStatus, TransportError> {
    let frame = response.last().ok_or(TransportError::BadFrame)?;
    if frame.kind != FrameKind::Result {
        return Err(TransportError::BadFrame);
    }
    let task: A2aTask = serde_json::from_value(
        frame.payload.get("task").cloned().ok_or(TransportError::BadFrame)?,
    )
    .map_err(|_| TransportError::BadFrame)?;
    Ok(task.status.state)
}

#[must_use]
pub fn response_last_event_sequence(response: &[Frame]) -> u64 {
    let from_events = response
        .iter()
        .filter(|frame| frame.kind == FrameKind::Event)
        .filter_map(|frame| {
            frame
                .payload
                .pointer(&format!("/statusUpdate/metadata/{EVENT_SEQUENCE}"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|sequence| u64::try_from(sequence).ok())
        })
        .max()
        .unwrap_or(0);
    let from_cursor = response
        .last()
        .filter(|frame| frame.kind == FrameKind::Result)
        .and_then(|frame| {
            frame
                .payload
                .pointer(&format!("/task/metadata/{EVENT_CURSOR}"))
                .and_then(serde_json::Value::as_i64)
        })
        .and_then(|cursor| u64::try_from(cursor).ok())
        .unwrap_or(0);
    from_events.max(from_cursor)
}

/// `true` when the final frame's task snapshot reports durable events
/// beyond the page this response carried (see
/// [`text_result_payload_with_progress`]).
#[must_use]
pub fn response_more_events_remain(response: &[Frame]) -> bool {
    response.last().filter(|frame| frame.kind == FrameKind::Result).is_some_and(
        |frame| {
            frame
                .payload
                .pointer(&format!("/task/metadata/{MORE_EVENTS}"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        },
    )
}

async fn request_with_polling_on(
    endpoint: &iroh::Endpoint,
    remote: iroh::EndpointAddr,
    mut request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let mut payload: A2aRequest = serde_json::from_value(request.payload.clone())
        .map_err(|_| TransportError::BadFrame)?;
    let idempotency_key = payload.idempotency_key.clone();
    let task_id = request.task_id.clone().ok_or(TransportError::BadFrame)?;
    let scope_id = request.scope_id.clone();
    let mut last_sequence = payload.last_event_sequence.unwrap_or(0);
    let started = std::time::Instant::now();
    // Every poll's events are kept: a task that outlives one page of events
    // must not have the earlier pages silently dropped in favor of only the
    // last poll's response.
    let mut collected: Vec<Frame> = Vec::new();
    let mut link = Link::default();

    loop {
        payload.idempotency_key = idempotency_key.clone();
        payload.request_nonce = Uuid::new_v4().to_string();
        payload.last_event_sequence = Some(last_sequence);
        request.exchange_id = Uuid::new_v4().to_string();
        request.scope_id = scope_id.clone();
        request.task_id = Some(task_id.clone());
        request.payload =
            serde_json::to_value(&payload).map_err(|_| TransportError::BadFrame)?;
        let response = link.exchange(endpoint, remote.clone(), request.clone()).await?;
        let is_error =
            response.last().is_some_and(|frame| frame.kind == FrameKind::Error);
        last_sequence = last_sequence.max(response_last_event_sequence(&response));
        // A terminal-looking status is not enough to stop: the page may
        // have been truncated by the frame or byte bound before every
        // durable event was delivered.
        let more_events = response_more_events_remain(&response);
        let terminal =
            !is_error && is_terminal_task_status(response_task_status(&response)?);
        let done = is_error || (terminal && !more_events);
        for frame in response {
            if frame.kind == FrameKind::Event || done {
                collected.push(frame);
            }
        }
        if done {
            return Ok(collected);
        }
        if started.elapsed() >= TASK_POLL_DEADLINE {
            return Err(TransportError::Timeout);
        }
        tokio::time::sleep(TASK_POLL_INTERVAL).await;
    }
}

async fn exchange_stream(
    connection: &iroh::endpoint::Connection,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let (mut send, mut receive) =
        tokio::time::timeout(STREAM_TIMEOUT, connection.open_bi())
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|error| TransportError::Network(error.to_string()))?;
    write_frames(&mut send, &[request]).await?;
    send.finish().map_err(|error| TransportError::Network(error.to_string()))?;
    read_frames(&mut receive).await
}

/// The transport error of an endpoint key that cannot load.
pub(crate) const fn key_error(error: KeyFileError) -> TransportError {
    match error {
        KeyFileError::Unreadable | KeyFileError::WrongSize => TransportError::Key,
        KeyFileError::Io => TransportError::Io,
    }
}

pub mod relay;

#[cfg(test)]
mod tests {
    use super::{
        A2aAccessMode, A2aAuthorization, A2aClaim, A2aRequest, A2aTaskStatus, Frame,
        FrameKind, TransportError, endpoint_addr_from_record, event_payload,
        response_last_event_sequence, response_more_events_remain,
        text_request_payload, text_result_payload, text_result_payload_with_progress,
    };
    use crate::discovery::signed_record;
    use ed25519_dalek::SigningKey;
    use std::{
        collections::HashMap,
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tower::ServiceExt;

    const TASK_ID: &str = "00000000-0000-4000-8000-000000000002";

    /// Frames through the same codec and checks as a live stream.
    async fn encode(frames: &[Frame]) -> Result<Vec<u8>, TransportError> {
        let mut bytes = Vec::new();
        super::write_frames(&mut bytes, frames).await?;
        Ok(bytes)
    }

    async fn decode(mut bytes: &[u8]) -> Result<Vec<Frame>, TransportError> {
        super::read_frames(&mut bytes).await
    }

    fn request() -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Request,
            exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: text_request_payload(
                TASK_ID,
                "key",
                "00000000-0000-4000-8000-000000000003",
                "work",
            ),
        }
    }
    fn result() -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: text_result_payload(TASK_ID, A2aTaskStatus::Completed, "done"),
        }
    }

    fn event() -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Event,
            exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: event_payload(TASK_ID, A2aTaskStatus::Submitted, None),
        }
    }

    #[tokio::test]
    async fn unknown_fields_fail_at_every_depth() {
        let mut wrapper = event();
        wrapper.payload["other"] = serde_json::Value::Bool(true);
        let mut status = event();
        status.payload["statusUpdate"]["status"]["unknown"] =
            serde_json::Value::Bool(true);
        let mut artifact = result();
        artifact.payload["task"]["artifacts"][0]["unknown"] =
            serde_json::Value::Bool(true);
        let mut part = result();
        part.payload["task"]["artifacts"][0]["parts"][0]["unknown"] =
            serde_json::Value::Bool(true);
        let mut two_contents = result();
        two_contents.payload["task"]["artifacts"][0]["parts"][0]["url"] =
            serde_json::Value::String("https://example.invalid".to_owned());
        for frames in [
            vec![wrapper, result()],
            vec![status, result()],
            vec![artifact],
            vec![part],
            vec![two_contents],
        ] {
            assert_eq!(encode(&frames).await, Err(TransportError::BadFrame));
        }
        assert!(encode(&[event(), result()]).await.is_ok());
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let request_frames = vec![request()];
        let response_frames = vec![event(), result()];
        assert_eq!(
            decode(&encode(&request_frames).await.expect("encode"))
                .await
                .expect("decode"),
            request_frames
        );
        assert_eq!(
            decode(&encode(&response_frames).await.expect("encode"))
                .await
                .expect("decode"),
            response_frames
        );
    }

    #[test]
    fn progress_metadata_reports_the_cursor_and_whether_events_remain() {
        let truncated = Frame {
            payload: text_result_payload_with_progress(
                TASK_ID,
                A2aTaskStatus::Working,
                "",
                5,
                true,
            ),
            ..result()
        };
        assert!(response_more_events_remain(std::slice::from_ref(&truncated)));
        assert_eq!(response_last_event_sequence(&[truncated]), 5);

        let final_page = Frame {
            payload: text_result_payload_with_progress(
                TASK_ID,
                A2aTaskStatus::Completed,
                "done",
                9,
                false,
            ),
            ..result()
        };
        assert!(!response_more_events_remain(std::slice::from_ref(&final_page)));
        assert_eq!(response_last_event_sequence(&[final_page]), 9);

        // A plain result carries no progress metadata at all; both readers
        // must default safely rather than panic on the missing fields.
        assert!(!response_more_events_remain(&[result()]));
        assert_eq!(response_last_event_sequence(&[result()]), 0);

        // The event frames' own sequence numbers still win when they are
        // ahead of the snapshot's cursor.
        let mut event_frame = event();
        event_frame.payload = event_payload(TASK_ID, A2aTaskStatus::Working, Some(12));
        assert_eq!(
            response_last_event_sequence(&[event_frame, truncated_with_cursor(5)]),
            12
        );
    }

    fn truncated_with_cursor(cursor: i64) -> Frame {
        Frame {
            payload: text_result_payload_with_progress(
                TASK_ID,
                A2aTaskStatus::Working,
                "",
                cursor,
                true,
            ),
            ..result()
        }
    }

    #[tokio::test]
    async fn malformed_and_version_mismatch_frames_fail() {
        assert_eq!(
            decode(&[0, 0, 0, 2, b'{', b'}']).await,
            Err(TransportError::BadFrame)
        );
        let mut frame = request();
        frame.version = 1;
        assert_eq!(
            encode(&[frame, result()]).await,
            Err(TransportError::UnsupportedVersion)
        );
        let mut response = result();
        response.scope_id = "other".to_owned();
        assert_eq!(
            encode(&[event(), response]).await,
            Err(TransportError::InvalidOrder)
        );
    }

    #[tokio::test]
    async fn typed_a2a_payloads_reject_unknown_data_versions_and_bad_exchange_ids() {
        let mut unsupported = request();
        unsupported.payload["a2aVersion"] = serde_json::Value::String("0.3".to_owned());
        assert_eq!(
            encode(&[unsupported]).await,
            Err(TransportError::UnsupportedVersion)
        );
        let mut unknown = request();
        unknown.payload["unknown"] = serde_json::Value::Bool(true);
        assert_eq!(encode(&[unknown]).await, Err(TransportError::BadFrame));
        let mut bad_exchange = request();
        bad_exchange.exchange_id = "not-a-uuid".to_owned();
        assert_eq!(encode(&[bad_exchange]).await, Err(TransportError::BadFrame));
        let mut bad_task = request();
        bad_task.task_id = Some("not-a-uuid".to_owned());
        bad_task.payload["task"]["id"] =
            serde_json::Value::String("not-a-uuid".to_owned());
        assert_eq!(encode(&[bad_task]).await, Err(TransportError::BadFrame));
        let mut bad_nonce = request();
        bad_nonce.payload["requestNonce"] =
            serde_json::Value::String("not-a-uuid".to_owned());
        assert_eq!(encode(&[bad_nonce]).await, Err(TransportError::BadFrame));
    }

    #[tokio::test]
    async fn official_a2a_json_uses_supported_parts_and_rejects_unknown_fields() {
        let mut frame = request();
        frame.payload = serde_json::json!({
            "a2aVersion": "1.0",
            "idempotencyKey": "key",
            "requestNonce": "00000000-0000-4000-8000-000000000003",
            "task": {
                "id": TASK_ID,
                "contextId": TASK_ID,
                "status": { "state": "TASK_STATE_SUBMITTED" },
                "history": [{
                    "messageId": "00000000-0000-4000-8000-000000000004",
                    "contextId": TASK_ID,
                    "taskId": TASK_ID,
                    "role": "ROLE_USER",
                    "parts": [{ "text": "work" }]
                }]
            }
        });
        assert!(encode(std::slice::from_ref(&frame)).await.is_ok());

        let mut unsupported = frame.clone();
        unsupported.payload["task"]["history"][0]["parts"][0] =
            serde_json::json!({ "url": "https://example.invalid" });
        assert_eq!(encode(&[unsupported]).await, Err(TransportError::BadFrame));

        let mut unknown = frame;
        unknown.payload["task"]["unknown"] = serde_json::Value::Bool(true);
        assert_eq!(encode(&[unknown]).await, Err(TransportError::BadFrame));
    }

    #[test]
    fn request_authorization_maps_to_policy_data() {
        let mut request: A2aRequest = serde_json::from_value(text_request_payload(
            TASK_ID,
            "key",
            "00000000-0000-4000-8000-000000000003",
            "work",
        ))
        .expect("request must decode");
        request.authorization = Some(A2aAuthorization {
            request_id: "00000000-0000-4000-8000-000000000004".to_owned(),
            caller_agent_id: "caller".to_owned(),
            caller_endpoint_id: "endpoint".to_owned(),
            capability_id: "review".to_owned(),
            workspace_id: "workspace".to_owned(),
            access_mode: A2aAccessMode::ReadOnly,
            claim: A2aClaim {
                id: "claim".to_owned(),
                issuer_agent_id: "issuer".to_owned(),
                subject_agent_id: "caller".to_owned(),
                subject_endpoint_id: "endpoint".to_owned(),
                scope_id: "direct".to_owned(),
                issued_at: 1,
                expires_at: 2,
                signing_key: "00".repeat(32),
                nonce: "01".repeat(32),
                signature: "02".repeat(64),
            },
        });
        let (policy, claim) = request.policy_data("direct").expect("data must map");
        assert_eq!(policy.caller_endpoint_id.as_str(), "endpoint");
        assert_eq!(policy.capability_id.as_str(), "review");
        assert_eq!(claim.issuer_agent_id.as_str(), "issuer");
        request.authorization = None;
        assert_eq!(
            request.policy_data("direct"),
            Err(TransportError::PermissionDenied)
        );
    }

    #[test]
    fn invalid_response_frames_are_rejected() {
        let request = request();
        let mut mismatched = vec![event(), result()];
        mismatched[0].exchange_id = "00000000-0000-4000-8000-000000009999".to_owned();
        assert_eq!(
            super::validate_response(std::slice::from_ref(&request), &mismatched),
            Err(TransportError::InvalidOrder)
        );
        let request_frame = vec![request.clone(), request.clone()];
        assert_eq!(
            super::validate_response_frames(&request_frame),
            Err(TransportError::InvalidOrder)
        );
        let missing_final = vec![event()];
        assert_eq!(
            super::validate_response(std::slice::from_ref(&request), &missing_final),
            Err(TransportError::InvalidOrder)
        );
        let mut wrong_scope = vec![event(), result()];
        wrong_scope[1].scope_id = "other-scope".to_owned();
        assert_eq!(
            super::validate_response(std::slice::from_ref(&request), &wrong_scope),
            Err(TransportError::InvalidOrder),
            "a response scope id that differs from the request must fail closed"
        );
        let mut wrong_task = vec![event(), result()];
        wrong_task[1].task_id = Some("00000000-0000-4000-8000-000000009998".to_owned());
        assert_eq!(
            super::validate_response(std::slice::from_ref(&request), &wrong_task),
            Err(TransportError::InvalidOrder),
            "a response task id that differs from the request must fail closed"
        );
        let mut cancel_in_response = vec![event()];
        let mut stray_cancel = event();
        stray_cancel.kind = FrameKind::Cancel;
        cancel_in_response.push(stray_cancel);
        cancel_in_response.push(result());
        assert_eq!(
            super::validate_response(
                std::slice::from_ref(&request),
                &cancel_in_response
            ),
            Err(TransportError::InvalidOrder),
            "a cancel frame must never appear inside a response"
        );
        let duplicate_final = vec![result(), result()];
        assert_eq!(
            super::validate_response(std::slice::from_ref(&request), &duplicate_final),
            Err(TransportError::InvalidOrder),
            "two final frames in one response must fail closed"
        );
        let frame_after_final = vec![result(), event()];
        assert_eq!(
            super::validate_response(
                std::slice::from_ref(&request),
                &frame_after_final
            ),
            Err(TransportError::InvalidOrder),
            "an event after the final frame must fail closed"
        );
    }

    #[tokio::test]
    async fn oversized_and_cancelled_exchanges_are_bounded() {
        // The codec refuses a length prefix over `MAX_FRAME_BYTES` before it
        // reads the body.
        assert_eq!(
            decode(&[0xff, 0xff, 0xff, 0xff]).await,
            Err(TransportError::BadFrame)
        );
        let cancel = Frame {
            version: 0,
            kind: FrameKind::Cancel,
            exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: super::payload(super::A2aCancel {
                id: TASK_ID.to_owned(),
                metadata: None,
                tenant: None,
            }),
        };
        assert!(encode(std::slice::from_ref(&cancel)).await.is_ok());
        assert_eq!(
            encode(&[request(), cancel]).await,
            Err(TransportError::InvalidOrder)
        );
    }

    #[test]
    fn signed_discovery_record_builds_a_direct_endpoint_address() {
        let signing_key = SigningKey::from_bytes(&[3; 32]);
        let endpoint_id = iroh::SecretKey::generate().public().to_string();
        let record = signed_record(
            &signing_key,
            "agent".to_owned(),
            endpoint_id,
            vec!["127.0.0.1:4433".to_owned()],
            "scope".to_owned(),
            vec!["review".to_owned()],
            std::time::SystemTime::now() + Duration::from_secs(60),
        );
        assert!(endpoint_addr_from_record(&record).is_ok());
    }

    #[test]
    fn peer_rate_limit_isolated_by_authenticated_endpoint() {
        let limiter = super::peer_limiter();
        for _ in 0..super::PEER_REQUESTS_PER_SECOND {
            assert!(limiter.check_key(&"peer-one".to_owned()).is_ok());
        }
        assert!(limiter.check_key(&"peer-one".to_owned()).is_err());
        assert!(limiter.check_key(&"peer-two".to_owned()).is_ok());
    }

    #[test]
    fn task_polling_stays_under_the_peer_rate_limit() {
        let polls_per_second = 1000 / super::TASK_POLL_INTERVAL.as_millis();
        assert!(polls_per_second * 2 <= u128::from(super::PEER_REQUESTS_PER_SECOND));
    }

    #[test]
    fn a_kept_connection_outlives_the_outbox_poll_interval() {
        assert!(super::RESPONSE_LINGER > crate::outbox::POLL_INTERVAL);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_connection_serves_one_stream_after_another() {
        let (receiver, lookup) = bound_receiver().await;
        let peer = bind_peer(&lookup).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let router = super::router(receiver.clone(), {
            let calls = Arc::clone(&calls);
            move |_peer, frames| {
                calls.fetch_add(1, Ordering::SeqCst);
                let request = frames.first().ok_or(TransportError::BadFrame)?;
                Ok(vec![matching_result(request)])
            }
        });
        let connection = peer
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("connection must open");
        for _ in 0..3 {
            let (mut send, mut receive) =
                connection.open_bi().await.expect("stream must open");
            super::write_frames(&mut send, &[probe_request()])
                .await
                .expect("request must write");
            send.finish().expect("request must finish");
            let response =
                super::read_frames(&mut receive).await.expect("response must read");
            assert_eq!(response[0].kind, FrameKind::Result);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        connection.close(0u32.into(), b"done");
        router.shutdown().await.expect("router must stop");
        receiver.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rate_limited_peer_reads_the_overloaded_error_frame() {
        let (receiver, lookup) = bound_receiver().await;
        let peer = bind_peer(&lookup).await;
        let router = super::router(receiver.clone(), |_peer, frames| {
            let request = frames.first().ok_or(TransportError::BadFrame)?;
            Ok(vec![matching_result(request)])
        });
        let mut overloaded = false;
        for _ in 0..super::PEER_REQUESTS_PER_SECOND * 4 {
            let response = try_round_trip(&peer, &receiver, &probe_request())
                .await
                .expect("every exchange, even a rate-limited one, must be readable");
            if response[0].kind == FrameKind::Error {
                assert_eq!(response[0].payload["message"], "node overloaded");
                overloaded = true;
                break;
            }
        }
        assert!(overloaded, "the peer must reach the rate limit");
        router.shutdown().await.expect("router must stop");
        receiver.close().await;
    }

    #[tokio::test]
    async fn a_link_returns_one_snapshot_for_each_exchange_on_one_connection() {
        let (receiver, lookup) = bound_receiver().await;
        let peer = bind_peer(&lookup).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let router = super::router(receiver.clone(), {
            let calls = Arc::clone(&calls);
            move |_peer, frames| {
                calls.fetch_add(1, Ordering::SeqCst);
                let request = frames.first().ok_or(TransportError::BadFrame)?;
                Ok(vec![Frame {
                    payload: text_result_payload(TASK_ID, A2aTaskStatus::Working, ""),
                    ..matching_result(request)
                }])
            }
        });
        let mut link = super::Link::default();
        let response = link
            .exchange(&peer, receiver.addr(), probe_request())
            .await
            .expect("the exchange must answer");
        assert_eq!(response.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let first = link.connection.as_ref().map(iroh::endpoint::Connection::stable_id);
        for _ in 0..2 {
            link.exchange(&peer, receiver.addr(), probe_request())
                .await
                .expect("the exchange must answer");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            link.connection.as_ref().map(iroh::endpoint::Connection::stable_id),
            first
        );
        drop(link);
        router.shutdown().await.expect("router must stop");
        receiver.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_link_sends_each_request_once_to_a_host_with_one_stream_each() {
        let (receiver, lookup) = bound_receiver().await;
        let peer = bind_peer(&lookup).await;
        let served = Arc::new(AtomicUsize::new(0));
        // A host from before connection reuse: one stream for each
        // connection, then a short wait for the peer to close.
        let host = tokio::spawn({
            let receiver = receiver.clone();
            let served = Arc::clone(&served);
            async move {
                while let Some(incoming) = receiver.accept().await {
                    let Ok(connection) = incoming.await else { continue };
                    let Ok((mut send, mut receive)) = connection.accept_bi().await
                    else {
                        continue;
                    };
                    let request =
                        super::read_frames(&mut receive).await.expect("request");
                    served.fetch_add(1, Ordering::SeqCst);
                    super::write_frames(&mut send, &[matching_result(&request[0])])
                        .await
                        .expect("response");
                    send.finish().expect("finish");
                    let _ = tokio::time::timeout(
                        Duration::from_millis(300),
                        connection.closed(),
                    )
                    .await;
                }
            }
        });
        let mut link = super::Link::default();
        for _ in 0..3 {
            let response = link
                .exchange(&peer, receiver.addr(), probe_request())
                .await
                .expect("every exchange must answer");
            assert_eq!(response[0].kind, FrameKind::Result);
        }
        assert_eq!(served.load(Ordering::SeqCst), 3);
        assert!(link.one_stream_each);
        assert!(link.connection.is_none());
        drop(link);
        receiver.close().await;
        host.await.expect("host must stop");
    }

    #[test]
    fn valid_request_failures_return_a_safe_a2a_error() {
        let response =
            super::error_response(&[request()], &TransportError::PermissionDenied);
        assert_eq!(response.len(), 1);
        assert_eq!(response[0].kind, FrameKind::Error);
        assert_eq!(response[0].payload["code"], a2a::error_code::INVALID_REQUEST);
        assert_eq!(response[0].payload["message"], "permission denied");
        let response = super::error_response(&[request()], &TransportError::Overloaded);
        assert_eq!(response[0].payload["message"], "node overloaded");
    }

    #[tokio::test]
    async fn partial_length_delimited_frame_fails_closed() {
        use tokio::io::AsyncWriteExt;

        let (mut send, mut receive) = tokio::io::duplex(8);
        send.write_all(&[0, 0, 0, 8, b'{']).await.expect("bytes must write");
        drop(send);
        assert_eq!(
            super::read_frames(&mut receive).await,
            Err(TransportError::BadFrame)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_peer_stops_at_the_stream_time_limit() {
        let (send, mut receive) = tokio::io::duplex(8);
        let start = tokio::time::Instant::now();
        assert_eq!(
            super::read_frames(&mut receive).await,
            Err(TransportError::Timeout)
        );
        assert!(start.elapsed() >= super::STREAM_TIMEOUT);
        drop(send);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_reader_stops_at_the_stream_time_limit() {
        let (mut send, receive) = tokio::io::duplex(8);
        let start = tokio::time::Instant::now();
        assert_eq!(
            super::write_frames(&mut send, &[event(), result()]).await,
            Err(TransportError::Timeout)
        );
        assert!(start.elapsed() >= super::STREAM_TIMEOUT);
        drop(receive);
    }

    #[tokio::test]
    async fn local_endpoints_connect_with_the_aether_alpn() {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let caller = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("caller must bind");
        let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("receiver must bind");
        lookup.add_endpoint_info(caller.addr());
        lookup.add_endpoint_info(receiver.addr());
        let receiver_task = receiver.clone();
        let accepted = tokio::spawn(async move {
            receiver_task
                .accept()
                .await
                .expect("connection must arrive")
                .await
                .expect("connection must accept")
        });
        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            caller.connect(receiver.id(), super::ALPN),
        )
        .await
        .expect("connection must not stall")
        .expect("connection must open");
        let accepted =
            tokio::time::timeout(std::time::Duration::from_secs(5), accepted)
                .await
                .expect("accept must not stall")
                .expect("accept task must complete");
        connected.close(0u32.into(), b"done");
        accepted.close(0u32.into(), b"done");
        caller.close().await;
        receiver.close().await;
    }

    #[tokio::test]
    async fn router_routes_aether_alpn_and_shuts_down() {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let caller = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("caller must bind");
        let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("receiver must bind");
        lookup.add_endpoint_info(caller.addr());
        lookup.add_endpoint_info(receiver.addr());
        let router = super::router(receiver.clone(), |_peer, request| {
            Ok(super::error_response(&request, &TransportError::PermissionDenied))
        });
        let connection = caller
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("connection must open");
        let (mut send, mut receive) =
            connection.open_bi().await.expect("stream must open");
        super::write_frames(&mut send, &[request()]).await.expect("request must write");
        send.finish().expect("request must finish");
        let response =
            super::read_frames(&mut receive).await.expect("response must read");
        assert_eq!(response[0].kind, FrameKind::Error);
        router.shutdown().await.expect("router must stop");
        caller.close().await;
    }

    #[tokio::test]
    async fn router_shutdown_cancels_an_idle_handler() {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let caller = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("caller must bind");
        let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("receiver must bind");
        lookup.add_endpoint_info(caller.addr());
        lookup.add_endpoint_info(receiver.addr());
        let router = super::router(receiver.clone(), |_peer, request| Ok(request));
        let connection = caller
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("connection must open");
        tokio::time::timeout(Duration::from_secs(1), router.shutdown())
            .await
            .expect("router shutdown must not wait for a stream")
            .expect("router must stop");
        connection.close(0u32.into(), b"done");
        caller.close().await;
    }

    /// Exchange ids must be UUIDs (`valid_exchange_id`), so a plain
    /// `format!("occupant-{index}")` is rejected before it ever reaches the
    /// handler; encode the index into a UUID-shaped string instead.
    fn occupant_id(index: usize) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    /// A request whose exchange id is guaranteed not to collide with any
    /// `occupant_id` used for `MAX_CONNECTIONS` (32) occupants (those use
    /// hex suffixes `0`..`1f`).
    fn probe_request() -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Request,
            exchange_id: "00000000-0000-4000-8000-0000000000ff".to_owned(),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: text_request_payload(
                TASK_ID,
                "key",
                "00000000-0000-4000-8000-000000000003",
                "work",
            ),
        }
    }

    fn occupant_request(index: usize) -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Request,
            exchange_id: occupant_id(index),
            scope_id: "scope".to_owned(),
            task_id: Some(TASK_ID.to_owned()),
            payload: text_request_payload(
                TASK_ID,
                "key",
                "00000000-0000-4000-8000-000000000003",
                "work",
            ),
        }
    }

    /// Attempts one full request/response exchange over a fresh connection,
    /// returning `None` on any failure. `connect` alone cannot distinguish
    /// an accepted connection from one the router will reject for lack of a
    /// permit (`AetherProtocol::accept` closes it only after the transport
    /// handshake already succeeded), so callers must retry this whole
    /// exchange, not just the connect, to detect when capacity returns.
    async fn try_round_trip(
        caller: &iroh::Endpoint,
        receiver: &iroh::Endpoint,
        request: &Frame,
    ) -> Option<Vec<Frame>> {
        let connection = caller.connect(receiver.id(), super::ALPN).await.ok()?;
        let (mut send, mut receive) = connection.open_bi().await.ok()?;
        super::write_frames(&mut send, std::slice::from_ref(request)).await.ok()?;
        send.finish().ok()?;
        let response = super::read_frames(&mut receive).await.ok()?;
        connection.close(0u32.into(), b"done");
        Some(response)
    }

    fn matching_result(request: &Frame) -> Frame {
        Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: request.exchange_id.clone(),
            scope_id: request.scope_id.clone(),
            task_id: request.task_id.clone(),
            payload: text_result_payload(TASK_ID, A2aTaskStatus::Completed, "done"),
        }
    }

    /// Binds a receiver `iroh` endpoint plus the `MemoryLookup` other peers
    /// register with to reach it.
    async fn bound_receiver() -> (iroh::Endpoint, iroh::address_lookup::MemoryLookup) {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("receiver must bind");
        lookup.add_endpoint_info(receiver.addr());
        (receiver, lookup)
    }

    /// Binds one more peer endpoint registered against `lookup`. `iroh`'s
    /// connection machinery does not support many independent concurrent
    /// connections from a single peer identity — opening `MAX_CONNECTIONS`
    /// connections from one shared caller endpoint deadlocked after only
    /// 8-11 ever reached the router's handler, confirmed with a real
    /// wall-clock wait far longer than any handshake could plausibly need.
    /// "Many slow peers" means many distinct peers anyway, so each
    /// occupying connection below gets its own freshly bound endpoint.
    async fn bind_peer(lookup: &iroh::address_lookup::MemoryLookup) -> iroh::Endpoint {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("peer endpoint must bind");
        lookup.add_endpoint_info(endpoint.addr());
        endpoint
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn router_sheds_load_over_slow_peers_and_recovers() {
        let (receiver, lookup) = bound_receiver().await;
        let prober = bind_peer(&lookup).await;
        let started = Arc::new(AtomicUsize::new(0));
        let gates: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // The handler runs on a blocking thread, so it waits on its gate
        // through the runtime handle.
        let router = super::router(receiver.clone(), {
            let started = Arc::clone(&started);
            let gates = Arc::clone(&gates);
            move |_peer, frames| {
                started.fetch_add(1, Ordering::Release);
                let request =
                    frames.first().cloned().ok_or(TransportError::BadFrame)?;
                let gate = gates
                    .lock()
                    .map_err(|_| {
                        TransportError::Network("gate unavailable".to_owned())
                    })?
                    .get(&request.exchange_id)
                    .cloned();
                if let Some(gate) = gate {
                    tokio::runtime::Handle::current().block_on(gate.notified());
                }
                Ok(vec![matching_result(&request)])
            }
        });

        let mut occupying = Vec::new();
        for index in 0..super::MAX_CONNECTIONS {
            gates
                .lock()
                .expect("gates must lock")
                .insert(occupant_id(index), Arc::new(tokio::sync::Notify::new()));
            let peer = bind_peer(&lookup).await;
            let connection = peer
                .connect(receiver.id(), super::ALPN)
                .await
                .expect("an occupying connection must open");
            let (mut send, receive) =
                connection.open_bi().await.expect("stream must open");
            super::write_frames(&mut send, &[occupant_request(index)])
                .await
                .expect("request must write");
            send.finish().expect("request must finish");
            occupying.push((peer, connection, receive));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while started.load(Ordering::Acquire) != super::MAX_CONNECTIONS {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("every slow peer must reach the handler");

        // Immediate overload: the transport-level handshake still succeeds
        // (the router only decides what to do with the connection once it
        // is handed to `AetherProtocol::accept`), but with no permit free
        // the connection is closed right away rather than left to queue.
        let overloaded = prober
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("an overloaded connection still opens at the transport level");
        tokio::time::timeout(Duration::from_secs(3), overloaded.closed())
            .await
            .expect("an overloaded connection must be closed promptly, not queued");

        // Successful service after capacity returns: releasing exactly one
        // slow peer's handler frees exactly one permit.
        gates
            .lock()
            .expect("gates must lock")
            .get(&occupant_id(0))
            .expect("occupant 0 must have a gate")
            .notify_one();
        let (_finished_peer, finished_connection, mut finished_receive) =
            occupying.remove(0);
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            super::read_frames(&mut finished_receive),
        )
        .await
        .expect("the released peer must respond")
        .expect("the response must decode");
        assert_eq!(response[0].kind, FrameKind::Result);
        finished_connection.close(0u32.into(), b"done");

        // `connect` succeeds at the transport level even when the router
        // will reject the connection for lack of a permit (as `overloaded`
        // above did): only a full round trip proves a permit was actually
        // free, so retry the whole exchange rather than just the connect.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(response) =
                    try_round_trip(&prober, &receiver, &probe_request()).await
                {
                    assert_eq!(response[0].kind, FrameKind::Result);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("capacity must become available again after a permit is freed");

        for (index, (_peer, connection, mut receive)) in
            occupying.into_iter().enumerate()
        {
            gates
                .lock()
                .expect("gates must lock")
                .get(&occupant_id(index + 1))
                .expect("remaining occupants must have a gate")
                .notify_one();
            super::read_frames(&mut receive).await.expect("response must decode");
            connection.close(0u32.into(), b"done");
        }
        router.shutdown().await.expect("router must stop");
        receiver.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn router_releases_a_permit_when_a_silent_peer_disconnects() {
        let (receiver, lookup) = bound_receiver().await;
        let prober = bind_peer(&lookup).await;
        let started = Arc::new(AtomicUsize::new(0));
        let release_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let router = super::router(receiver.clone(), {
            let started = Arc::clone(&started);
            let release_gate = Arc::clone(&release_gate);
            move |_peer, frames| {
                started.fetch_add(1, Ordering::Release);
                let _ =
                    tokio::runtime::Handle::current().block_on(release_gate.acquire());
                let request =
                    frames.first().cloned().ok_or(TransportError::BadFrame)?;
                Ok(vec![matching_result(&request)])
            }
        });

        let mut occupying = Vec::new();
        // Fill every permit but one with a peer that has sent a request and
        // is waiting on the (still closed) release gate.
        for _ in 0..super::MAX_CONNECTIONS - 1 {
            let peer = bind_peer(&lookup).await;
            let connection = peer
                .connect(receiver.id(), super::ALPN)
                .await
                .expect("an occupying connection must open");
            let (mut send, receive) =
                connection.open_bi().await.expect("stream must open");
            super::write_frames(&mut send, &[request()])
                .await
                .expect("request must write");
            send.finish().expect("request must finish");
            occupying.push((peer, connection, receive));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while started.load(Ordering::Acquire) != super::MAX_CONNECTIONS - 1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("every busy peer must reach the handler");

        // The last permit is held by a peer that never opens a stream at
        // all, then disconnects outright — the case this test targets.
        let silent_peer = bind_peer(&lookup).await;
        let silent = silent_peer
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("the silent connection must open");

        let overloaded = prober
            .connect(receiver.id(), super::ALPN)
            .await
            .expect("an overloaded connection still opens at the transport level");
        tokio::time::timeout(Duration::from_secs(3), overloaded.closed())
            .await
            .expect("an overloaded connection must be closed promptly, not queued");

        let disconnect_start = tokio::time::Instant::now();
        silent.close(0u32.into(), b"gone");

        // The other 31 handlers are still blocked on `release_gate`, so a
        // full round trip would hang here; instead, distinguish "the router
        // closed this connection for overload" from "the router accepted it
        // and is waiting on a stream" by whether `closed()` resolves within
        // a short window.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let Ok(connection) = prober.connect(receiver.id(), super::ALPN).await
                else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                if tokio::time::timeout(Duration::from_millis(300), connection.closed())
                    .await
                    .is_err()
                {
                    connection.close(0u32.into(), b"done");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a disconnected silent peer must free its permit");
        assert!(
            disconnect_start.elapsed() < super::STREAM_TIMEOUT,
            "a disconnect must free its permit well before the stream time limit"
        );

        release_gate.add_permits(super::MAX_CONNECTIONS - 1);
        for (_peer, connection, mut receive) in occupying {
            super::read_frames(&mut receive).await.expect("response must decode");
            connection.close(0u32.into(), b"done");
        }
        router.shutdown().await.expect("router must stop");
        receiver.close().await;
    }

    #[tokio::test]
    async fn tower_sheds_a_handler_when_the_shared_limit_is_full() {
        let gate = Arc::new(Barrier::new(super::MAX_CONNECTIONS + 1));
        let started = Arc::new(AtomicUsize::new(0));
        let service = super::handler_service({
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            move |_peer, _frames| {
                started.fetch_add(1, Ordering::Release);
                gate.wait();
                Ok(Vec::new())
            }
        });
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..super::MAX_CONNECTIONS {
            let service = service.clone();
            tasks.spawn(async move {
                service.oneshot(("peer".to_owned(), Vec::new())).await
            });
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while started.load(Ordering::Acquire) != super::MAX_CONNECTIONS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("handlers must start");
        let overload = service
            .oneshot(("peer".to_owned(), Vec::new()))
            .await
            .expect_err("limit must shed load");
        assert_eq!(super::handler_error(overload), TransportError::Overloaded);
        gate.wait();
        while tasks.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn local_endpoints_exchange_a_bounded_request() {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let caller = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("caller must bind");
        let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![super::ALPN.to_vec()])
            .address_lookup(lookup.clone())
            .bind()
            .await
            .expect("receiver must bind");
        lookup.add_endpoint_info(caller.addr());
        lookup.add_endpoint_info(receiver.addr());
        let receiver_task = receiver.clone();
        let served = tokio::spawn(async move {
            let connection =
                receiver_task.accept().await.expect("incoming").await.expect("accept");
            let (mut send, mut receive) = connection.accept_bi().await.expect("stream");
            super::read_frames(&mut receive).await.expect("frames");
            super::write_frames(
                &mut send,
                &[
                    super::Frame {
                        version: 0,
                        kind: super::FrameKind::Event,
                        exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
                        scope_id: "scope".to_owned(),
                        task_id: Some(TASK_ID.to_owned()),
                        payload: event_payload(TASK_ID, A2aTaskStatus::Submitted, None),
                    },
                    super::Frame {
                        version: 0,
                        kind: super::FrameKind::Result,
                        exchange_id: "00000000-0000-4000-8000-000000000001".to_owned(),
                        scope_id: "scope".to_owned(),
                        task_id: Some(TASK_ID.to_owned()),
                        payload: text_result_payload(
                            TASK_ID,
                            A2aTaskStatus::Completed,
                            "ok",
                        ),
                    },
                ],
            )
            .await
            .expect("response");
            send.finish().expect("finish");
            connection
        });
        let connection =
            caller.connect(receiver.id(), super::ALPN).await.expect("connect");
        let (mut send, mut receive) = connection.open_bi().await.expect("stream");
        super::write_frames(&mut send, &[request()]).await.expect("request");
        send.finish().expect("finish");
        let response = super::read_frames(&mut receive).await.expect("response");
        assert_eq!(response[1].kind, super::FrameKind::Result);
        let accepted = served.await.expect("server must complete");
        caller.close().await;
        accepted.close(0u32.into(), b"done");
        receiver.close().await;
    }
}
