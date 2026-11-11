//! Steps for two nodes that exchange requests over Iroh.

use std::{
    fmt,
    net::UdpSocket,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aether::{
    discovery::Record,
    transport::{
        A2aTaskStatus, Frame, FrameKind, TransportError, bind_node, event_payload,
        request_once, request_once_record, router, text_request_payload,
        text_result_payload,
    },
};
use cucumber::{given, then, when};
use tempfile::TempDir;

use super::fixtures::{invitation, managed_member, record};
use crate::common::{
    discovery_signing_key, load_contact, load_directory_record, open_store,
    store_contact_invitation, store_directory_record,
};
use crate::{common::blocking, steps::World};

const WIRE_NONCE: &str = "00000000-0000-4000-8000-000000000009";

/// How the client found the serving node.
#[derive(Debug)]
enum Route {
    /// By endpoint id and address.
    Endpoint(String),
    /// By a signed directory record.
    Record(Box<Record>),
}

#[derive(Default)]
pub struct State {
    root: Option<TempDir>,
    port: u16,
    server: Option<iroh::Endpoint>,
    serving: Option<iroh::protocol::Router>,
    route: Option<Route>,
    /// The most handlers that ran at the same time.
    peak: Arc<AtomicUsize>,
    responses: Vec<Result<Vec<Frame>, TransportError>>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("port", &self.port)
            .field("route", &self.route)
            .field("peak", &self.peak)
            .finish_non_exhaustive()
    }
}

impl State {
    fn root(&self) -> std::path::PathBuf {
        self.root.as_ref().expect("a serving node must run").path().to_owned()
    }

    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    async fn bind(&mut self) -> String {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("port must reserve");
        self.port = socket.local_addr().expect("port must resolve").port();
        drop(socket);
        let root = tempfile::tempdir().expect("temporary directory must create");
        let server = bind_node(&root.path().join("server"), self.port, None)
            .await
            .expect("server must bind");
        let endpoint_id = server.id().to_string();
        self.root = Some(root);
        self.server = Some(server);
        endpoint_id
    }

    async fn stop(&mut self) {
        if let Some(serving) = self.serving.take() {
            serving.shutdown().await.expect("router must stop");
        }
        if let Some(server) = self.server.take() {
            server.close().await;
        }
    }
}

/// Answers a request with a submitted event and then an `ok` result.
fn answer(request: &[Frame]) -> Result<Vec<Frame>, TransportError> {
    let first = request.first().ok_or(TransportError::BadFrame)?;
    let task_id = first.task_id.as_deref().unwrap_or_default();
    let frame = |kind, payload| Frame {
        version: 0,
        kind,
        exchange_id: first.exchange_id.clone(),
        scope_id: first.scope_id.clone(),
        task_id: first.task_id.clone(),
        payload,
    };
    Ok(vec![
        frame(FrameKind::Event, event_payload(task_id, A2aTaskStatus::Submitted, None)),
        frame(
            FrameKind::Result,
            text_result_payload(task_id, A2aTaskStatus::Completed, "ok"),
        ),
    ])
}

fn review_request(exchange_id: &str, scope_id: &str, task_id: &str) -> Frame {
    Frame {
        version: 0,
        kind: FrameKind::Request,
        exchange_id: exchange_id.to_owned(),
        scope_id: scope_id.to_owned(),
        task_id: Some(task_id.to_owned()),
        payload: text_request_payload(task_id, "key", WIRE_NONCE, "review"),
    }
}

#[given(expr = "a serving node that the client finds in the {string} scope")]
async fn serving_node_in_scope(world: &mut World, scope_id: String) {
    let state = &mut world.proof.transport;
    let endpoint_id = state.bind().await;
    let root = state.root();
    let route = match scope_id.as_str() {
        "direct" => {
            let signed = invitation(&scope_id, "server", &endpoint_id);
            let endpoint = blocking({
                let root = root.clone();
                move || {
                    let store = open_store(&root);
                    store_contact_invitation(&store, &signed)
                        .expect("direct invitation must store");
                    load_contact(&store, "server")
                        .expect("direct invitation must load")
                        .endpoint_id
                }
            })
            .await;
            Route::Endpoint(endpoint)
        }
        "directory" => {
            let signed = record(&scope_id, "server", &endpoint_id, &state.address());
            let trusted = discovery_signing_key().verifying_key().to_bytes();
            let loaded = blocking({
                let root = root.clone();
                move || {
                    let store = open_store(&root);
                    store_directory_record(&store, &signed)
                        .expect("directory record must store");
                    load_directory_record(&root, &scope_id, "server", trusted)
                        .expect("directory record must load")
                }
            })
            .await;
            Route::Record(Box::new(loaded))
        }
        "managed" => {
            let member = blocking({
                let root = root.clone();
                move || {
                    let mut store = open_store(&root);
                    store
                        .apply_managed_member(
                            "https://control.test",
                            1,
                            &managed_member("server", &endpoint_id, "managed"),
                        )
                        .expect("managed membership must apply");
                    store
                        .managed_members("managed")
                        .expect("managed roster must load")
                        .into_iter()
                        .next()
                        .expect("managed roster must contain member")
                }
            })
            .await;
            Route::Endpoint(member.endpoint_id)
        }
        other => unreachable!("unknown scope {other}"),
    };
    state.route = Some(route);
    let server = state.server.clone().expect("the server must bind");
    state.serving = Some(router(server, |_, request| answer(&request)));
}

#[given("a serving node whose handler waits until two requests arrive")]
async fn serving_node_with_barrier(world: &mut World) {
    let state = &mut world.proof.transport;
    let endpoint_id = state.bind().await;
    state.route = Some(Route::Endpoint(endpoint_id));
    let barrier = Arc::new(Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::clone(&state.peak);
    let server = state.server.clone().expect("the server must bind");
    state.serving = Some(router(server, move |_, request| {
        let current = active.fetch_add(1, Ordering::Relaxed) + 1;
        peak.fetch_max(current, Ordering::Relaxed);
        barrier.wait();
        active.fetch_sub(1, Ordering::Relaxed);
        answer(&request)
    }));
}

#[when(expr = "the client sends a review request in the {string} scope")]
async fn client_sends(world: &mut World, scope_id: String) {
    let state = &mut world.proof.transport;
    let client = state.root().join("client");
    let request = review_request(
        "00000000-0000-4000-8000-000000000001",
        &scope_id,
        "00000000-0000-4000-8000-000000000002",
    );
    let response = match state.route.as_ref().expect("the client must find the node") {
        Route::Record(record) => request_once_record(&client, record, request).await,
        Route::Endpoint(endpoint_id) => {
            request_once(&client, endpoint_id, &state.address(), request).await
        }
    };
    state.stop().await;
    state.responses.push(response);
}

#[when("two clients send a review request at the same time")]
async fn two_clients_send(world: &mut World) {
    let state = &mut world.proof.transport;
    let root = state.root();
    let address = state.address();
    let Some(Route::Endpoint(endpoint_id)) = state.route.as_ref() else {
        unreachable!("the clients find the node by endpoint");
    };
    let client_one = root.join("concurrent-client-one");
    let client_two = root.join("concurrent-client-two");
    let first = request_once(
        &client_one,
        endpoint_id,
        &address,
        review_request(
            "00000000-0000-4000-8000-000000000005",
            "direct",
            "00000000-0000-4000-8000-000000000007",
        ),
    );
    let second = request_once(
        &client_two,
        endpoint_id,
        &address,
        review_request(
            "00000000-0000-4000-8000-000000000006",
            "direct",
            "00000000-0000-4000-8000-000000000008",
        ),
    );
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(first, second)
    })
    .await;
    state.stop().await;
    let (first, second) = result.expect("both client requests must complete");
    state.responses.push(first);
    state.responses.push(second);
}

#[then(
    expr = "the client receives a submitted event and then a result, both in the {string} scope"
)]
fn receives_event_and_result(world: &mut World, scope_id: String) {
    let [response] = world.proof.transport.responses.as_slice() else {
        unreachable!("the client must send one request");
    };
    let response = response.as_ref().expect("client request must complete");
    let kinds = response.iter().map(|frame| frame.kind).collect::<Vec<_>>();
    assert_eq!(kinds, [FrameKind::Event, FrameKind::Result]);
    assert!(response.iter().all(|frame| frame.scope_id == scope_id));
}

#[then("both requests complete")]
fn both_complete(world: &mut World) {
    let responses = &world.proof.transport.responses;
    assert_eq!(responses.len(), 2);
    for response in responses {
        assert!(response.is_ok(), "request must complete: {response:?}");
    }
}

#[then("the node ran both handlers at the same time")]
fn ran_both_at_once(world: &mut World) {
    assert_eq!(world.proof.transport.peak.load(Ordering::Relaxed), 2);
}
