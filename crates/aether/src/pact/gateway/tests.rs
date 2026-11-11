#![expect(
    clippy::arithmetic_side_effects,
    reason = "the calendar oracle steps days and seconds, far from overflow"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use aether_core::Request;
use aether_store::{StoreError, TaskState, pact::PactStore};
use axum::{
    body::Body,
    http::{Method, Request as HttpRequest, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    DELEGATION_HEADER, Failure, Gateway, NodeUnavailable, OAuthError, PactNode,
    approved_grant, caller_endpoint_id, rfc3339, router,
};
use crate::{
    execution::Task,
    pact::{
        config::PactConfig,
        consent::{ConsentLink, SharedPactStore},
        grant::pact_grant_record,
        jwks::{
            PaIdentity, PaVerifier, UrlPolicy,
            tests::{AUDIENCE, ISSUER, StaticKeys, TestAgent},
        },
        keys::ProviderKey,
        tokens::verify_receipt,
    },
    policy::PactPolicySection,
    proof::ProofError,
};

const PUBLIC: &str = "https://node.example";
const BRAND: &str = "https://node.example/a2a/review";

struct TaskSnapshot {
    polls_before_terminal: u32,
    polls_seen: AtomicU32,
    state: TaskState,
    result: Option<String>,
}

#[derive(Default)]
struct FakeNode {
    grants: Mutex<BTreeSet<String>>,
    policy_unreadable: AtomicBool,
    admitted: Mutex<Vec<(Request, String, Task)>>,
    pending_outcomes: Mutex<Vec<(u32, TaskState, Option<String>)>>,
    snapshots: Mutex<BTreeMap<String, TaskSnapshot>>,
    always_running: AtomicBool,
}

impl FakeNode {
    fn queue_outcome(
        &self,
        polls_before_terminal: u32,
        state: TaskState,
        result: Option<String>,
    ) {
        self.pending_outcomes.lock().expect("lock").push((
            polls_before_terminal,
            state,
            result,
        ));
    }

    fn keep_running(&self) {
        self.always_running.store(true, Ordering::Release);
    }
}

impl PactNode for FakeNode {
    fn admit(
        &self,
        request: &Request,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<String, ProofError> {
        self.admitted.lock().expect("lock").push((
            request.clone(),
            idempotency_key.to_owned(),
            task.clone(),
        ));
        let outcome = self.pending_outcomes.lock().expect("lock").pop();
        if let Some((polls, state, result)) = outcome {
            self.snapshots.lock().expect("lock").insert(
                task.id.clone(),
                TaskSnapshot {
                    polls_before_terminal: polls,
                    polls_seen: AtomicU32::new(0),
                    state,
                    result,
                },
            );
        }
        Ok(task.id.clone())
    }

    fn snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), ProofError> {
        if self.always_running.load(Ordering::Acquire) {
            return Ok((TaskState::Running, None));
        }
        if let Some(entry) = self.snapshots.lock().expect("lock").get(task_id) {
            let seen = entry.polls_seen.fetch_add(1, Ordering::AcqRel);
            if seen < entry.polls_before_terminal {
                return Ok((TaskState::Running, None));
            }
            return Ok((entry.state, entry.result.clone()));
        }
        let turns = self.admitted.lock().expect("lock").len();
        Ok((TaskState::Succeeded, Some(format!("answer {turns}"))))
    }

    fn has_grant(&self, grant_id: &str) -> Result<bool, NodeUnavailable> {
        if self.policy_unreadable.load(Ordering::SeqCst) {
            return Err(NodeUnavailable);
        }
        Ok(self.grants.lock().expect("lock").contains(grant_id))
    }

    /// No events: each task here ends at once.
    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<String> {
        tokio::sync::broadcast::channel(1).1
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    app: axum::Router,
    agent: TestAgent,
    node: Arc<FakeNode>,
    key: Arc<ProviderKey>,
    store: SharedPactStore,
}

fn config() -> PactConfig {
    serde_json::from_value::<PactPolicySection>(json!({
        "publicUrl": PUBLIC,
        "audience": AUDIENCE,
        "brands": [
            {
                "id": "review",
                "name": "Code review",
                "description": "Reviews code.",
                "capability": "review",
                "workspace": "workspace",
                "accessMode": "read_only",
                "scope": { "id": "review:read_only", "description": "Review code (read only)" },
            },
            {
                "id": "other",
                "name": "Other",
                "description": "Another Brand.",
                "capability": "other",
                "workspace": "workspace",
                "accessMode": "read_only",
                "scope": { "id": "other:read_only", "description": "Other work" },
            },
        ],
    }))
    .expect("file must decode")
    .parse(1000, 60)
    .expect("config must parse")
}

fn fixture() -> Fixture {
    fixture_with(|_| {})
}

fn fixture_with(setup: impl FnOnce(&FakeNode)) -> Fixture {
    fixture_with_gateway(setup, |gateway| gateway)
}

fn fixture_with_gateway(
    setup_node: impl FnOnce(&FakeNode),
    setup_gateway: impl FnOnce(Gateway) -> Gateway,
) -> Fixture {
    let directory = tempfile::tempdir().expect("directory must create");
    let agent = TestAgent::new("k1");
    let key = Arc::new(ProviderKey::load_or_create(directory.path()).expect("key"));
    let node = Arc::new(FakeNode::default());
    setup_node(&node);
    let link = Arc::new(ConsentLink::default());
    link.set("https://127.0.0.1:3000".to_owned());
    let path = directory.path().join(aether_store::pact::PACT_DATABASE);
    let store: SharedPactStore =
        Arc::new(Mutex::new(PactStore::open(&path).expect("store must open")));
    let gateway = setup_gateway(
        Gateway::new(
            config(),
            PaVerifier::new(
                AUDIENCE.to_owned(),
                UrlPolicy { allow_insecure_local: false },
                StaticKeys::new(agent.jwks.clone()),
            ),
            Arc::clone(&key),
            Arc::clone(&store),
            node.clone(),
            link,
        )
        .with_poll_interval(Duration::from_millis(1)),
    );
    Fixture {
        app: router(Arc::new(gateway)),
        agent,
        node,
        key,
        _directory: directory,
        store,
    }
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    challenge: Option<String>,
    body: Value,
    empty: bool,
}

impl Fixture {
    fn store(&self) -> std::sync::MutexGuard<'_, PactStore> {
        self.store.lock().expect("store lock")
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        delegation: Option<&str>,
        body: &str,
    ) -> Reply {
        let mut request = HttpRequest::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(delegation) = delegation {
            request = request.header(DELEGATION_HEADER, format!("Bearer {delegation}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).expect("request"))
            .await
            .expect("router must answer");
        let status = response.status();
        let content_type = header_text(response.headers(), header::CONTENT_TYPE);
        let challenge = header_text(response.headers(), header::WWW_AUTHENTICATE);
        let bytes =
            axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        Reply {
            status,
            content_type,
            challenge,
            empty: bytes.is_empty(),
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn send(&self, sub: &str, delegation: Option<&str>, message: Value) -> Reply {
        let token = self.agent.token_for(sub);
        self.call(
            Method::POST,
            "/a2a/review/message:send",
            Some(&token),
            delegation,
            &json!({ "message": message }).to_string(),
        )
        .await
    }

    /// Runs the device flow for `sub` and approves it, as the consent page
    /// would. Returns the token response.
    async fn delegate(&self, sub: &str) -> Value {
        let token = self.agent.token_for(sub);
        let authorization = self
            .call(
                Method::POST,
                "/a2a/review/oauth/device_authorization",
                Some(&token),
                None,
                &format!("client_id={ISSUER}&scope=review%3Aread_only"),
            )
            .await;
        assert_eq!(authorization.status, StatusCode::OK, "{}", authorization.body);
        let user_code = authorization.body["user_code"].as_str().expect("user code");
        let grant_id = format!("pact_{sub}");
        self.store()
            .approve_device(
                user_code,
                &pact_grant_record(
                    &grant_id,
                    "review",
                    ISSUER,
                    sub,
                    "review:read_only",
                    now(),
                ),
                now(),
            )
            .expect("approval must store");
        self.node.grants.lock().expect("lock").insert(grant_id);
        let device_code = authorization.body["device_code"].as_str().expect("code");
        let tokens = self
            .call(
                Method::POST,
                "/a2a/review/oauth/token",
                Some(&self.agent.token_for(sub)),
                None,
                &format!(
                    "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={device_code}&client_id={ISSUER}"
                ),
            )
            .await;
        assert_eq!(tokens.status, StatusCode::OK, "{}", tokens.body);
        tokens.body
    }
}

fn now() -> i64 {
    aether_core::time::unix_timestamp(std::time::SystemTime::now()).expect("clock")
}

fn header_text(
    headers: &axum::http::HeaderMap,
    name: header::HeaderName,
) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
}

fn user_message(id: &str, text: &str) -> Value {
    json!({ "messageId": id, "role": "ROLE_USER", "parts": [{ "text": text }] })
}

fn reason(reply: &Reply) -> &str {
    reply.body["error"]["details"][0]["reason"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn the_agent_card_advertises_both_profiles_and_needs_no_token() {
    let fixture = fixture();
    let card = fixture
        .call(Method::GET, "/a2a/review/.well-known/agent-card.json", None, None, "")
        .await;
    assert_eq!(card.status, StatusCode::OK);
    // Only these fields: a provider or a build version would name the
    // software that the node runs.
    let mut keys: Vec<&str> = card
        .body
        .as_object()
        .expect("the card is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "description",
            "name",
            "securityRequirements",
            "securitySchemes",
            "skills",
            "supportedInterfaces",
            "version",
        ]
    );
    assert_eq!(card.body["version"], "1");
    assert_eq!(card.body["supportedInterfaces"][0]["url"], BRAND);
    assert_eq!(card.body["supportedInterfaces"][0]["protocolBinding"], "HTTP+JSON");
    assert_eq!(card.body["supportedInterfaces"][0]["protocolVersion"], "1.0");
    assert_eq!(
        card.body["securitySchemes"]["platformJwt"]["httpAuthSecurityScheme"]["scheme"],
        "Bearer",
    );
    let flow = &card.body["securitySchemes"]["userDelegation"]["oauth2SecurityScheme"];
    assert_eq!(
        flow["flows"]["deviceCode"]["scopes"]["review:read_only"],
        "Review code (read only)",
    );
    assert_eq!(
        card.body["securityRequirements"][0],
        json!({ "schemes": { "platformJwt": { "list": [] } } })
    );
    let metadata = fixture
        .call(
            Method::GET,
            "/a2a/review/oauth/.well-known/oauth-authorization-server",
            None,
            None,
            "",
        )
        .await;
    assert_eq!(metadata.body["issuer"], format!("{BRAND}/oauth"));
    assert_eq!(metadata.body["jwks_uri"], format!("{PUBLIC}/a2a/jwks.json"));
    let jwks = fixture.call(Method::GET, "/a2a/jwks.json", None, None, "").await;
    assert_eq!(jwks.body, fixture.key.jwks());
    let unknown = fixture
        .call(Method::GET, "/a2a/missing/.well-known/agent-card.json", None, None, "")
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(unknown.empty);
}

#[tokio::test]
async fn routing_happens_before_authentication() {
    let fixture = fixture();
    let token = fixture.agent.token_for("u");
    for (method, path, token, status) in [
        (Method::GET, "/a2a/review/unknown", None, StatusCode::NOT_FOUND),
        (Method::GET, "/a2a/review/message:send", None, StatusCode::METHOD_NOT_ALLOWED),
        (Method::PUT, "/a2a/review/message:send", None, StatusCode::METHOD_NOT_ALLOWED),
        (Method::POST, "/a2a/review/tasks", None, StatusCode::METHOD_NOT_ALLOWED),
        (
            Method::DELETE,
            "/a2a/review/tasks/t/pushNotificationConfigs",
            None,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::GET,
            "/a2a/missing/tasks",
            Some(token.as_str()),
            StatusCode::NOT_FOUND,
        ),
        (Method::GET, "/a2a/review/tasks", None, StatusCode::UNAUTHORIZED),
        (Method::POST, "/a2a/review/message:send", None, StatusCode::UNAUTHORIZED),
    ] {
        let reply = fixture.call(method.clone(), path, token, None, "{").await;
        assert_eq!(reply.status, status, "{method} {path}");
        assert!(reply.empty, "{method} {path} must have no A2A body");
    }
    let missing = fixture.call(Method::GET, "/a2a/review/tasks", None, None, "").await;
    assert_eq!(missing.challenge.as_deref(), Some(r#"Bearer realm="a2a""#));
}

#[tokio::test]
async fn task_routes_answer_with_their_a2a_errors() {
    let fixture = fixture();
    let token = fixture.agent.token_for("u");
    let token = Some(token.as_str());
    let listed = fixture
        .call(
            Method::GET,
            "/a2a/review/tasks?pageSize=20&contextId=ignored",
            token,
            None,
            "",
        )
        .await;
    assert_eq!(listed.content_type.as_deref(), Some("application/a2a+json"));
    assert_eq!(
        listed.body,
        json!({ "tasks": [], "nextPageToken": "", "pageSize": 20, "totalSize": 0 }),
    );
    let default = fixture.call(Method::GET, "/a2a/review/tasks", token, None, "").await;
    assert_eq!(default.body["pageSize"], 50);
    for size in ["0", "101", "x"] {
        let invalid = fixture
            .call(
                Method::GET,
                &format!("/a2a/review/tasks?pageSize={size}"),
                token,
                None,
                "",
            )
            .await;
        assert_eq!(reason(&invalid), "INVALID_PARAMS", "{size}");
    }
    let missing =
        fixture.call(Method::GET, "/a2a/review/tasks/t-1", token, None, "").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["error"]["message"], "Task not found: t-1");
    assert_eq!(missing.body["error"]["code"], 404);
    assert_eq!(missing.body["error"]["status"], "NOT_FOUND");
    assert_eq!(missing.body["error"]["details"][0]["domain"], "a2a-protocol.org");
    let stream =
        fixture.call(Method::POST, "/a2a/review/message:stream", token, None, "").await;
    assert_eq!(reason(&stream), "UNSUPPORTED_OPERATION");
    let push = fixture
        .call(
            Method::GET,
            "/a2a/review/tasks/t-1/pushNotificationConfigs",
            token,
            None,
            "",
        )
        .await;
    assert_eq!(reason(&push), "PUSH_NOTIFICATION_NOT_SUPPORTED");
}

#[tokio::test]
async fn malformed_messages_get_their_a2a_errors() {
    let fixture = fixture();
    let mut with_task = user_message("m", "hello");
    with_task["taskId"] = "task-1".into();
    let task = fixture.send("u", None, with_task).await;
    assert_eq!(reason(&task), "TASK_NOT_FOUND");
    assert_eq!(task.body["error"]["message"], "Task not found");
    let token = fixture.agent.token_for("u");
    let broken = fixture
        .call(Method::POST, "/a2a/review/message:send", Some(&token), None, "{")
        .await;
    assert_eq!(reason(&broken), "INVALID_PARAMS");
    let raw = fixture
        .send("u", None, json!({ "messageId": "m", "role": "ROLE_USER", "parts": [{ "raw": "aGVsbG8=" }] }))
        .await;
    assert_eq!(reason(&raw), "CONTENT_TYPE_NOT_SUPPORTED");
    let agent = fixture
        .send("u", None, json!({ "messageId": "m", "role": "ROLE_AGENT", "parts": [{ "text": "hi" }] }))
        .await;
    assert_eq!(reason(&agent), "INVALID_PARAMS");
    let blank = fixture.send("u", None, user_message("m", "  ")).await;
    assert_eq!(reason(&blank), "INVALID_PARAMS");
    let long = fixture.send("u", None, user_message("m", &"x".repeat(4000))).await;
    assert_eq!(reason(&long), "INVALID_PARAMS");
}

#[tokio::test]
async fn without_a_delegation_a_turn_steps_up_and_runs_nothing() {
    let fixture = fixture();
    let reply = fixture.send("u", None, user_message("m1", "Review my code")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.content_type.as_deref(), Some("application/a2a+json"));
    let task = &reply.body["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_AUTH_REQUIRED");
    assert_eq!(task["metadata"]["pact.missingScopes"], json!(["review:read_only"]));
    let link = task["metadata"]["pact.verificationUriComplete"].as_str().expect("link");
    assert!(
        link.starts_with("https://127.0.0.1:3000/pact/consent?user_code="),
        "{link}"
    );
    assert!(fixture.node.admitted.lock().expect("lock").is_empty());

    // §4.3: the same messageId returns the stored reply.
    let context_id = task["contextId"].as_str().expect("context").to_owned();
    let mut retry = user_message("m1", "Review my code");
    retry["contextId"] = context_id.clone().into();
    assert_eq!(fixture.send("u", None, retry.clone()).await.body, reply.body);

    // §4.2: another user's, or another Brand's, context is unknown.
    let other = fixture.send("someone-else", None, retry.clone()).await;
    assert_eq!(reason(&other), "INVALID_PARAMS");
    assert_eq!(other.body["error"]["message"], "Unknown contextId");
    let token = fixture.agent.token_for("u");
    let other_brand = fixture
        .call(
            Method::POST,
            "/a2a/other/message:send",
            Some(&token),
            None,
            &json!({ "message": retry }).to_string(),
        )
        .await;
    assert_eq!(other_brand.body["error"]["message"], "Unknown contextId");
}

#[tokio::test]
async fn the_device_flow_follows_rfc_8628() {
    let owned = fixture();
    let fixture = &owned;
    let token = fixture.agent.token_for("u");
    let token = Some(token.as_str());
    let path = "/a2a/review/oauth/device_authorization";
    let wrong_client = fixture
        .call(
            Method::POST,
            path,
            token,
            None,
            "client_id=https%3A%2F%2Fother.example&scope=review%3Aread_only",
        )
        .await;
    assert_eq!(wrong_client.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_client.body["error"], "invalid_client");
    let wrong_scope = fixture
        .call(
            Method::POST,
            path,
            token,
            None,
            &format!("client_id={ISSUER}&scope=other%3Aread_only"),
        )
        .await;
    assert_eq!(wrong_scope.body["error"], "invalid_scope");
    let started = fixture
        .call(
            Method::POST,
            path,
            token,
            None,
            &format!("client_id={ISSUER}&scope=review%3Aread_only"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK);
    assert_eq!(started.body["interval"], 5);
    assert_eq!(started.body["expires_in"], 600);
    let code = started.body["device_code"].as_str().expect("code");
    let poll = |sub: &'static str| {
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={code}&client_id={ISSUER}"
        );
        let token = fixture.agent.token_for(sub);
        async move {
            fixture
                .call(
                    Method::POST,
                    "/a2a/review/oauth/token",
                    Some(&token),
                    None,
                    &body,
                )
                .await
        }
    };
    assert_eq!(poll("u").await.body["error"], "authorization_pending");
    assert_eq!(poll("u").await.body["error"], "slow_down");
    // Another user of the same personal agent cannot use the code.
    assert_eq!(poll("someone-else").await.body["error"], "invalid_grant");
    let user_code = started.body["user_code"].as_str().expect("user code");
    fixture.store().deny_device(user_code).expect("deny must store");
    assert_eq!(poll("u").await.body["error"], "access_denied");
    let unsupported = fixture
        .call(
            Method::POST,
            "/a2a/review/oauth/token",
            token,
            None,
            &format!("grant_type=password&client_id={ISSUER}"),
        )
        .await;
    assert_eq!(unsupported.body["error"], "unsupported_grant_type");
}

#[tokio::test]
async fn terminal_task_states_surface_in_delegated_replies() {
    for (state, expected) in [
        (TaskState::Failed, "The task failed."),
        (TaskState::Rejected, "The node refused the task."),
        (TaskState::Cancelled, "The task was cancelled."),
        (
            TaskState::UnknownResult,
            "The task ended in an unknown state. The node owner must review it.",
        ),
        (
            TaskState::OperatorReviewed,
            "The task ended in an unknown state. The node owner must review it.",
        ),
    ] {
        let fixture = fixture_with(|node| {
            node.queue_outcome(0, state, None);
        });
        let tokens = fixture.delegate("u").await;
        let access = tokens["access_token"].as_str().expect("access token");
        let reply =
            fixture.send("u", Some(access), user_message("m1", "Review main.rs")).await;
        assert_eq!(reply.status, StatusCode::OK, "{state:?} {}", reply.body);
        assert_eq!(
            reply.body["message"]["parts"],
            json!([{ "text": expected }]),
            "{state:?}"
        );
    }
}

#[tokio::test]
async fn a_task_that_never_finishes_times_out() {
    let fixture = fixture_with_gateway(FakeNode::keep_running, |gateway| {
        gateway
            .with_poll_interval(Duration::from_millis(5))
            .with_task_wait_deadline(Duration::from_millis(40))
    });
    let tokens = fixture.delegate("u").await;
    let access = tokens["access_token"].as_str().expect("access token");
    let reply =
        fixture.send("u", Some(access), user_message("m1", "Review main.rs")).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        reply.body["error"]["message"],
        "The task is still running. Send the same message again to read its result."
    );
}

#[tokio::test]
async fn a_delegated_turn_runs_codex_and_carries_a_signed_receipt() {
    let fixture = fixture();
    let tokens = fixture.delegate("u").await;
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "review:read_only");
    let access = tokens["access_token"].as_str().expect("access token");

    let first =
        fixture.send("u", Some(access), user_message("m1", "Review main.rs")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let message = &first.body["message"];
    assert_eq!(message["role"], "ROLE_AGENT");
    assert_eq!(message["parts"], json!([{ "text": "answer 1" }]));
    let receipt = &message["metadata"]["pact.receipt"];
    let claims = verify_receipt(&fixture.key, receipt["jws"].as_str().expect("jws"))
        .expect("receipt must verify");
    assert_eq!(serde_json::to_value(&claims).expect("claims"), receipt["claims"]);
    assert_eq!(claims.grant_id, "pact_u");
    assert_eq!(claims.user, "owner");
    assert_eq!(claims.pa, ISSUER);
    assert_eq!(claims.brand, BRAND);
    assert_eq!(claims.scopes_used, ["review:read_only"]);
    assert_eq!(fixture.store().receipts("pact_u").expect("receipts").len(), 1);

    let context_id = message["contextId"].as_str().expect("context").to_owned();
    let mut second = user_message("m2", "And lib.rs?");
    second["contextId"] = context_id.into();
    let second = fixture.send("u", Some(access), second).await;
    assert_eq!(second.body["message"]["parts"], json!([{ "text": "answer 2" }]));

    let admitted = fixture.node.admitted.lock().expect("lock").clone();
    let (request, key, task) = admitted.last().expect("a second task");
    assert_eq!(request.scope_id.as_str(), "pact");
    assert_eq!(request.caller_agent_id.as_str(), format!("pact:{ISSUER}"));
    assert_eq!(
        request.caller_endpoint_id.as_str(),
        caller_endpoint_id(&PaIdentity {
            issuer: ISSUER.to_owned(),
            subject: "u".to_owned()
        }),
    );
    assert_eq!(request.capability_id.as_str(), "review");
    assert_eq!(task.id, format!("pact-{key}"));
    assert!(
        task.remote_text.contains("User: Review main.rs\nAgent: answer 1"),
        "{}",
        task.remote_text
    );
    assert!(task.remote_text.ends_with("And lib.rs?"));
}

#[tokio::test]
async fn an_unreadable_policy_during_a_refresh_is_a_server_error() {
    let fixture = fixture();
    let tokens = fixture.delegate("u").await;
    let refresh = tokens["refresh_token"].as_str().expect("refresh token").to_owned();
    fixture.node.policy_unreadable.store(true, std::sync::atomic::Ordering::SeqCst);
    let token = fixture.agent.token_for("u");
    let refreshed = fixture
        .call(
            Method::POST,
            "/a2a/review/oauth/token",
            Some(&token),
            None,
            &format!(
                "grant_type=refresh_token&refresh_token={refresh}&client_id={ISSUER}"
            ),
        )
        .await;
    assert_eq!(refreshed.body["error"], "server_error");
}

#[tokio::test]
async fn a_revoked_or_foreign_delegation_is_an_invalid_token() {
    let fixture = fixture();
    let tokens = fixture.delegate("u").await;
    let access = tokens["access_token"].as_str().expect("access token").to_owned();
    let refresh = tokens["refresh_token"].as_str().expect("refresh token").to_owned();

    // The same token from another user of the personal agent.
    let foreign =
        fixture.send("someone-else", Some(&access), user_message("m", "hi")).await;
    assert_eq!(foreign.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        foreign.challenge.as_deref(),
        Some(r#"Bearer realm="a2a", error="invalid_token""#)
    );
    assert!(foreign.empty);

    // A refresh token works once.
    let refresh_body =
        format!("grant_type=refresh_token&refresh_token={refresh}&client_id={ISSUER}");
    let token = fixture.agent.token_for("u");
    let refreshed = fixture
        .call(
            Method::POST,
            "/a2a/review/oauth/token",
            Some(&token),
            None,
            &refresh_body,
        )
        .await;
    assert_eq!(refreshed.status, StatusCode::OK);
    let again = fixture
        .call(
            Method::POST,
            "/a2a/review/oauth/token",
            Some(&token),
            None,
            &refresh_body,
        )
        .await;
    assert_eq!(again.body["error"], "invalid_grant");

    // Revoking the Aether grant revokes the delegation and its refresh.
    fixture.node.grants.lock().expect("lock").clear();
    let revoked = fixture.send("u", Some(&access), user_message("m", "hi")).await;
    assert_eq!(revoked.status, StatusCode::UNAUTHORIZED);
    let next = refreshed.body["refresh_token"].as_str().expect("rotated");
    let after = fixture
        .call(
            Method::POST,
            "/a2a/review/oauth/token",
            Some(&token),
            None,
            &format!(
                "grant_type=refresh_token&refresh_token={next}&client_id={ISSUER}"
            ),
        )
        .await;
    assert_eq!(after.body["error"], "invalid_grant");
}

#[tokio::test]
async fn a_context_keeps_the_brand_user_it_first_ran_as() {
    let fixture = fixture();
    let started = fixture.send("u", None, user_message("m1", "hello")).await;
    let context_id =
        started.body["task"]["contextId"].as_str().expect("context").to_owned();
    fixture.store().bind_context_user(&context_id, "somebody").expect("bind");
    let tokens = fixture.delegate("u").await;
    let mut next = user_message("m2", "hello");
    next["contextId"] = context_id.into();
    let refused = fixture
        .send("u", Some(tokens["access_token"].as_str().expect("token")), next)
        .await;
    assert_eq!(reason(&refused), "INVALID_PARAMS");
}

/// The oracle for `rfc3339`: a calendar that steps one day at a time with
/// the Gregorian leap rule, sharing no arithmetic with the code under test.
fn step_day((year, month, day): (i64, i64, i64), forward: bool) -> (i64, i64, i64) {
    let leap = |year: i64| (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let length = |year: i64, month: i64| match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if forward {
        if day < length(year, month) {
            (year, month, day + 1)
        } else if month < 12 {
            (year, month + 1, 1)
        } else {
            (year + 1, 1, 1)
        }
    } else if day > 1 {
        (year, month, day - 1)
    } else if month > 1 {
        (year, month - 1, length(year, month - 1))
    } else {
        (year - 1, 12, 31)
    }
}

/// Every day from 1600-01-01 to 2400-12-31, two whole 400-year cycles of
/// the Gregorian calendar, against the day-by-day oracle.
#[test]
fn rfc3339_agrees_with_a_day_by_day_calendar_from_1600_to_2400() {
    for forward in [true, false] {
        let mut date = if forward { (1970, 1, 1) } else { (1969, 12, 31) };
        let mut days: i64 = if forward { 0 } else { -1 };
        while (1600..=2400).contains(&date.0) {
            let (year, month, day) = date;
            for (seconds, clock) in [
                (0, "00:00:00"),
                (1, "00:00:01"),
                (3_599, "00:59:59"),
                (86_399, "23:59:59"),
            ] {
                assert_eq!(
                    rfc3339(days * 86_400 + seconds),
                    format!("{year:04}-{month:02}-{day:02}T{clock}Z"),
                    "day {days}",
                );
            }
            date = step_day(date, forward);
            days += if forward { 1 } else { -1 };
        }
    }
}

fn decode_form_field(body: &str) -> String {
    form_urlencoded::parse(body.as_bytes())
        .find(|(name, _)| name == "x")
        .map_or_else(String::new, |(_, value)| value.into_owned())
}

#[test]
fn every_percent_escape_decodes_to_its_byte() {
    for byte in 0_u8..=0x7f {
        let expected = char::from(byte).to_string();
        assert_eq!(
            decode_form_field(&format!("x=%{byte:02X}")),
            expected,
            "{byte:#04x}"
        );
        assert_eq!(
            decode_form_field(&format!("x=%{byte:02x}")),
            expected,
            "{byte:#04x}"
        );
    }
    for text in ["", "a b", "ok:read", "é", "日本語", "🦀+%&=", "\u{0}\u{7f}"] {
        let escaped = text.bytes().fold(String::new(), |mut escaped, byte| {
            let _ = write!(escaped, "%{byte:02X}");
            escaped
        });
        assert_eq!(decode_form_field(&format!("x={escaped}")), text);
    }
}

#[test]
fn rfc3339_matches_known_instants() {
    for (seconds, text) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_791_374_400, "2026-10-07T12:00:00Z"),
        (4_107_542_399, "2100-02-28T23:59:59Z"),
    ] {
        assert_eq!(rfc3339(seconds), text);
    }
}

#[test]
fn a_store_failure_while_polling_a_device_code_is_a_server_error() {
    assert_eq!(
        approved_grant(Err(StoreError::InvalidState)),
        Err(Failure::OAuth(OAuthError::ServerError, "The gateway store failed")),
    );
}
