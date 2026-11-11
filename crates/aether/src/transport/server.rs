#![allow(clippy::redundant_pub_crate)]

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use tokio_util::{
    codec::{FramedRead, FramedWrite, LengthDelimitedCodec},
    sync::CancellationToken,
    task::TaskTracker,
};
use tower::{
    BoxError, ServiceBuilder, ServiceExt, service_fn, util::BoxCloneSyncService,
};

use super::wire::{
    A2aError, ALPN, Frame, FrameKind, HANDLER_TIMEOUT, MAX_CONNECTIONS,
    MAX_EXCHANGE_BYTES, MAX_FRAME_BYTES, MAX_FRAMES, PEER_REQUESTS_PER_SECOND,
    RESPONSE_LINGER, STREAM_TIMEOUT, TransportError, events_then_outcome, key_error,
    payload, validate_header, validate_order, validate_payloads,
};

type PeerLimiter = DefaultKeyedRateLimiter<String>;
type HandlerService = BoxCloneSyncService<(String, Vec<Frame>), Vec<Frame>, BoxError>;

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
    /// Which stream a connection waits for.
    #[derive(Clone, Copy)]
    enum Phase {
        /// The first: the peer connected to send it, so its absence is an
        /// error.
        AwaitFirst,
        /// A next one after a response: the peer may close instead.
        Linger,
    }
    let authenticated_endpoint = connection.remote_id().to_string();
    let mut phase = Phase::AwaitFirst;
    loop {
        let wait = match phase {
            Phase::AwaitFirst => STREAM_TIMEOUT,
            Phase::Linger => RESPONSE_LINGER,
        };
        let (send, receive) =
            match (phase, tokio::time::timeout(wait, connection.accept_bi()).await) {
                (_, Ok(Ok(stream))) => stream,
                (Phase::AwaitFirst, Ok(Err(error))) => {
                    return Err(TransportError::Network(error.to_string()));
                }
                (Phase::AwaitFirst, Err(_)) => return Err(TransportError::Timeout),
                (Phase::Linger, Ok(Err(_)) | Err(_)) => return Ok(()),
            };
        phase = Phase::Linger;
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

pub(crate) fn handler_service<F>(handler: F) -> HandlerService
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
pub(crate) fn handler_error(error: BoxError) -> TransportError {
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

pub(crate) fn peer_limiter() -> PeerLimiter {
    // The constant is not zero; the fallback only keeps the conversion total.
    let requests = std::num::NonZeroU32::new(PEER_REQUESTS_PER_SECOND)
        .unwrap_or(std::num::NonZeroU32::MIN);
    RateLimiter::keyed(Quota::per_second(requests))
}

pub(crate) fn error_response(request: &[Frame], error: &TransportError) -> Vec<Frame> {
    let Some(request) = request.first() else {
        return Vec::new();
    };
    vec![Frame::reply_to(
        request,
        FrameKind::Error,
        payload(A2aError {
            code: a2a_error_code(error),
            message: a2a_error_message(error).to_owned(),
            data: a2a_error_data(error),
        }),
    )]
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

pub(crate) async fn read_frames<R>(
    receive: &mut R,
) -> Result<Vec<Frame>, TransportError>
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

pub(crate) async fn write_frames<W>(
    send: &mut W,
    frames: &[Frame],
) -> Result<(), TransportError>
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

pub(crate) fn validate_response(
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

pub(crate) fn validate_response_frames(frames: &[Frame]) -> Result<(), TransportError> {
    validate_header(frames)?;
    if !events_then_outcome(frames) {
        return Err(TransportError::InvalidOrder);
    }
    validate_payloads(frames)
}
