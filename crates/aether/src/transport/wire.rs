#![allow(clippy::redundant_pub_crate)]

use std::{collections::HashMap, time::Duration};

use hex::FromHex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use aether_core::{
    AccessMode, AgentId, CapabilityId, Claim, ClaimId, EndpointId, Request, RequestId,
    ScopeId, TaskId, WorkspaceId, time::from_unix_seconds,
};

use crate::data_dir::KeyFileError;

pub use a2a::{
    CancelTaskRequest as A2aCancel, JsonRpcError as A2aError, Message as A2aMessage,
    Part as A2aPart, PartContent as A2aPartContent, Role as A2aRole,
    SendMessageResponse as A2aResult, StreamResponse as A2aEvent, Task as A2aTask,
    TaskState as A2aTaskStatus, TaskStatus as A2aTaskStatusValue,
};

pub const ALPN: &[u8] = b"aether/a2a/0";
pub const A2A_PROTOCOL_VERSION: &str = "1.0";
pub(crate) const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_EXCHANGE_BYTES: usize = 4_194_304;
pub const MAX_FRAMES: usize = 64;
pub(crate) const MAX_CONNECTIONS: usize = 32;
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a server-side connection handler waits, after it has written
/// and finished a response, for the peer's next stream or its close. It is
/// not a read or write budget like `STREAM_TIMEOUT`. A client that polls a
/// task keeps its connection and opens one stream for each poll, so this is
/// longer than a one-second poll interval so polling clients can reuse the
/// connection between polls.
/// A client that sends one request closes within microseconds of reading
/// its response, and the handler stops at once. The wait only bounds how
/// long a handler holds its `MAX_CONNECTIONS` permit for a peer that stops
/// polling or disconnects uncleanly. Dropping the `Connection` closes it
/// (`noq`'s `ConnectionRef::drop` calls the same `close` a peer-initiated
/// close would), so this does not truncate an already-`finish()`ed
/// response.
pub(crate) const RESPONSE_LINGER: Duration = Duration::from_secs(2);
/// At most half of `PEER_REQUESTS_PER_SECOND`, so one polling task never
/// trips the node's per-peer rate limit and a second concurrent task from
/// the same peer still fits under it.
pub(crate) const TASK_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const TASK_POLL_DEADLINE: Duration = Duration::from_secs(120);
pub(crate) const PEER_REQUESTS_PER_SECOND: u32 = 8;

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

impl Frame {
    /// The frame of `kind` that answers `request`: version 0, in the same
    /// exchange, scope and task.
    #[must_use]
    pub fn reply_to(
        request: &Self,
        kind: FrameKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            version: 0,
            kind,
            exchange_id: request.exchange_id.clone(),
            scope_id: request.scope_id.clone(),
            task_id: request.task_id.clone(),
            payload,
        }
    }
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
pub(crate) fn payload(value: impl Serialize) -> serde_json::Value {
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
pub(crate) fn validate_header(frames: &[Frame]) -> Result<&Frame, TransportError> {
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

pub(crate) fn events_then_outcome(frames: &[Frame]) -> bool {
    frames.split_last().is_some_and(|(last, events)| {
        matches!(last.kind, FrameKind::Result | FrameKind::Error)
            && events.iter().all(|frame| frame.kind == FrameKind::Event)
    })
}

pub(crate) fn validate_order(frames: &[Frame]) -> Result<(), TransportError> {
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

pub(crate) fn validate_payloads(frames: &[Frame]) -> Result<(), TransportError> {
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

pub(crate) fn valid_exchange_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

pub(crate) const fn key_error(error: KeyFileError) -> TransportError {
    match error {
        KeyFileError::Unreadable | KeyFileError::WrongSize => TransportError::Key,
        KeyFileError::Io => TransportError::Io,
    }
}
