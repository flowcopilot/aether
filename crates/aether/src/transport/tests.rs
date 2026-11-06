use super::{
    A2aAccessMode, A2aAuthorization, A2aClaim, A2aRequest, A2aTaskStatus, Frame,
    FrameKind, TransportError, client::endpoint_addr_from_record, event_payload,
    response_last_event_sequence, response_more_events_remain, text_request_payload,
    text_result_payload, text_result_payload_with_progress,
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
    status.payload["statusUpdate"]["status"]["unknown"] = serde_json::Value::Bool(true);
    let mut artifact = result();
    artifact.payload["task"]["artifacts"][0]["unknown"] = serde_json::Value::Bool(true);
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
        decode(&encode(&request_frames).await.expect("encode")).await.expect("decode"),
        request_frames
    );
    assert_eq!(
        decode(&encode(&response_frames).await.expect("encode")).await.expect("decode"),
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
    assert_eq!(decode(&[0, 0, 0, 2, b'{', b'}']).await, Err(TransportError::BadFrame));
    let mut frame = request();
    frame.version = 1;
    assert_eq!(
        encode(&[frame, result()]).await,
        Err(TransportError::UnsupportedVersion)
    );
    let mut response = result();
    response.scope_id = "other".to_owned();
    assert_eq!(encode(&[event(), response]).await, Err(TransportError::InvalidOrder));
}

#[tokio::test]
async fn typed_a2a_payloads_reject_unknown_data_versions_and_bad_exchange_ids() {
    let mut unsupported = request();
    unsupported.payload["a2aVersion"] = serde_json::Value::String("0.3".to_owned());
    assert_eq!(encode(&[unsupported]).await, Err(TransportError::UnsupportedVersion));
    let mut unknown = request();
    unknown.payload["unknown"] = serde_json::Value::Bool(true);
    assert_eq!(encode(&[unknown]).await, Err(TransportError::BadFrame));
    let mut bad_exchange = request();
    bad_exchange.exchange_id = "not-a-uuid".to_owned();
    assert_eq!(encode(&[bad_exchange]).await, Err(TransportError::BadFrame));
    let mut bad_task = request();
    bad_task.task_id = Some("not-a-uuid".to_owned());
    bad_task.payload["task"]["id"] = serde_json::Value::String("not-a-uuid".to_owned());
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
    assert_eq!(request.policy_data("direct"), Err(TransportError::PermissionDenied));
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
        super::validate_response(std::slice::from_ref(&request), &cancel_in_response),
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
        super::validate_response(std::slice::from_ref(&request), &frame_after_final),
        Err(TransportError::InvalidOrder),
        "an event after the final frame must fail closed"
    );
}

#[tokio::test]
async fn oversized_and_cancelled_exchanges_are_bounded() {
    // The codec refuses a length prefix over `MAX_FRAME_BYTES` before it
    // reads the body.
    assert_eq!(decode(&[0xff, 0xff, 0xff, 0xff]).await, Err(TransportError::BadFrame));
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
    assert_eq!(encode(&[request(), cancel]).await, Err(TransportError::InvalidOrder));
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
    let connection =
        peer.connect(receiver.id(), super::ALPN).await.expect("connection must open");
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
                let Ok((mut send, mut receive)) = connection.accept_bi().await else {
                    continue;
                };
                let request = super::read_frames(&mut receive).await.expect("request");
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
    assert_eq!(super::read_frames(&mut receive).await, Err(TransportError::BadFrame));
}

#[tokio::test(start_paused = true)]
async fn an_idle_peer_stops_at_the_stream_time_limit() {
    let (send, mut receive) = tokio::io::duplex(8);
    let start = tokio::time::Instant::now();
    assert_eq!(super::read_frames(&mut receive).await, Err(TransportError::Timeout));
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
    let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), accepted)
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
    let connection =
        caller.connect(receiver.id(), super::ALPN).await.expect("connection must open");
    let (mut send, mut receive) = connection.open_bi().await.expect("stream must open");
    super::write_frames(&mut send, &[request()]).await.expect("request must write");
    send.finish().expect("request must finish");
    let response = super::read_frames(&mut receive).await.expect("response must read");
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
    let connection =
        caller.connect(receiver.id(), super::ALPN).await.expect("connection must open");
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
            let request = frames.first().cloned().ok_or(TransportError::BadFrame)?;
            let gate = gates
                .lock()
                .map_err(|_| TransportError::Network("gate unavailable".to_owned()))?
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
        let (mut send, receive) = connection.open_bi().await.expect("stream must open");
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

    for (index, (_peer, connection, mut receive)) in occupying.into_iter().enumerate() {
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
            let _ = tokio::runtime::Handle::current().block_on(release_gate.acquire());
            let request = frames.first().cloned().ok_or(TransportError::BadFrame)?;
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
        let (mut send, receive) = connection.open_bi().await.expect("stream must open");
        super::write_frames(&mut send, &[request()]).await.expect("request must write");
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
        tasks.spawn(
            async move { service.oneshot(("peer".to_owned(), Vec::new())).await },
        );
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
    let connection = caller.connect(receiver.id(), super::ALPN).await.expect("connect");
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
