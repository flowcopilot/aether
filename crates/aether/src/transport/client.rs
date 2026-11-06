#![allow(clippy::redundant_pub_crate)]

use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::Path,
};

use uuid::Uuid;

use crate::discovery::{Record, validate_record};

use super::{
    server::{bind_node, read_frames, validate_response, write_frames},
    wire::{
        A2aRequest, A2aTask, A2aTaskStatus, ALPN, EVENT_CURSOR, EVENT_SEQUENCE, Frame,
        FrameKind, HANDSHAKE_TIMEOUT, MORE_EVENTS, STREAM_TIMEOUT, TASK_POLL_DEADLINE,
        TASK_POLL_INTERVAL, TransportError,
    },
};

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

#[derive(Debug, Default)]
pub struct Link {
    pub(crate) connection: Option<iroh::endpoint::Connection>,
    pub(crate) one_stream_each: bool,
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

pub fn endpoint_addr_from_record(
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
