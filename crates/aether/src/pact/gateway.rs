//! The PACT HTTP+JSON routes (spec §2, §4, §5, §6).
//!
//! Routing happens before authentication (§2.2): an unknown Brand or path
//! is `404` with no A2A body, whatever the token. Every other route but the
//! Agent Card and the OAuth metadata needs a valid personal-agent JWT.

use aether_core::time::now;
use std::{
    num::NonZeroU32,
    sync::{Arc, PoisonError},
    time::Duration,
};

use aether_core::{
    AgentId, CapabilityId, EndpointId, Request, RequestId, ScopeId, TaskId, WorkspaceId,
};
use aether_store::{
    StoreError, TaskState,
    pact::{DevicePoll, NewDeviceAuthorization, PactGrant, PactStore, Reservation},
};
use aws_lc_rs::digest;
use axum::{
    Router,
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{any, get},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::{
    config::{Brand, PactConfig, SCOPE_ID},
    consent::{ConsentLink, SharedPactStore},
    grant::{GrantBinding, caller_agent_id, caller_endpoint_id, grant_binds},
    jwks::{PaIdentity, PaVerifier},
    keys::ProviderKey,
    tokens::{
        ACCESS_TOKEN_SECONDS, DelegationClaims, ReceiptAction, ReceiptClaims,
        sign_delegation, sign_receipt, verify_delegation,
    },
};
use crate::{execution::Task, proof::ProofError};

/// The header that carries the delegation token (§5.5).
pub const DELEGATION_HEADER: &str = "x-a2a-user-delegation";
const A2A_CONTENT_TYPE: &str = "application/a2a+json";
/// RFC 8628 §3.2: how long a device code lives, and how often to poll.
const DEVICE_CODE_SECONDS: i64 = 600;
const POLL_INTERVAL_SECONDS: i64 = 5;
/// A context with no message for this long closes (§4.2).
pub const CONTEXT_IDLE_SECONDS: i64 = 24 * 60 * 60;
/// The most earlier turns a transcript carries.
const MAX_TRANSCRIPT_TURNS: usize = 20;
/// Room for the transcript's own framing text, in characters.
const PROMPT_FRAMING_CHARACTERS: usize = 256;
/// The characters user-code letters are drawn from (RFC 8628 §6.1: no
/// vowels, no look-alikes).
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// What the gateway needs from the node. A trait so the routes can be
/// tested without a Codex executor.
pub trait PactNode: Send + Sync {
    fn admit(
        &self,
        request: &Request,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<String, ProofError>;

    fn snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), ProofError>;

    /// Whether the live policy still holds the Aether grant `grant_id`.
    /// Revoking it in the admin UI revokes the delegation.
    fn has_grant(&self, grant_id: &str) -> Result<bool, NodeUnavailable>;

    /// The ids of tasks as they change.
    fn subscribe_events(&self) -> broadcast::Receiver<String>;
}

impl<T, P> PactNode for crate::node_service::NodeService<T, P>
where
    T: aether_core::TrustProvider + Send + Sync + 'static,
    P: crate::execution::Agent + Clone + Send + Sync + 'static,
{
    fn admit(
        &self,
        request: &Request,
        idempotency_key: &str,
        task: &Task,
    ) -> Result<String, ProofError> {
        self.admit_verified(request, idempotency_key, task)
    }

    fn snapshot(
        &self,
        task_id: &str,
    ) -> Result<(TaskState, Option<String>), ProofError> {
        self.task_snapshot(task_id)
    }

    fn has_grant(&self, grant_id: &str) -> Result<bool, NodeUnavailable> {
        Self::has_grant(self, &aether_core::GrantId::new(grant_id))
            .map_err(|_| NodeUnavailable)
    }

    fn subscribe_events(&self) -> broadcast::Receiver<String> {
        Self::subscribe_events(self)
    }
}

pub struct Gateway {
    config: PactConfig,
    verifier: PaVerifier,
    key: Arc<ProviderKey>,
    store: SharedPactStore,
    node: Arc<dyn PactNode>,
    consent: Arc<ConsentLink>,
    callers: DefaultKeyedRateLimiter<String>,
    /// How long a turn waits for news of its task before it reads the task
    /// again. Each change of the task ends the wait sooner.
    poll_interval: Duration,
}

/// The gateway's routes, relative to `pact.publicUrl`.
pub fn router(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .route("/.well-known/agent-card.json", get(root_card))
        .route("/a2a/jwks.json", get(provider_jwks))
        .route("/a2a/{brand}/{*rest}", any(brand_route))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(gateway)
}

/// §2.1: the standard place redirects to the card when the node has one
/// Brand.
async fn root_card(State(gateway): State<Arc<Gateway>>) -> Response {
    match gateway.config.brands.as_slice() {
        [brand] => Redirect::temporary(&gateway.config.card_url(brand)).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn provider_jwks(State(gateway): State<Arc<Gateway>>) -> Response {
    let mut response = axum::Json(gateway.key.jwks()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    response
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum Route {
    Card,
    Metadata,
    DeviceAuthorization,
    Token,
    Send,
    Unsupported,
    PushUnsupported,
    ListTasks,
    TaskNotFound(String),
    MethodNotAllowed,
    NotFound,
}

/// Matches a path below the interface URL. A known path with another
/// method is `MethodNotAllowed`.
fn route(method: &Method, rest: &str) -> Route {
    let only = |allowed: &[Method], route: Route| {
        if allowed.contains(method) { route } else { Route::MethodNotAllowed }
    };
    match rest {
        ".well-known/agent-card.json" => return only(&[Method::GET], Route::Card),
        "oauth/.well-known/oauth-authorization-server" => {
            return only(&[Method::GET], Route::Metadata);
        }
        "oauth/device_authorization" => {
            return only(&[Method::POST], Route::DeviceAuthorization);
        }
        "oauth/token" => return only(&[Method::POST], Route::Token),
        "message:send" => return only(&[Method::POST], Route::Send),
        "message:stream" => return only(&[Method::POST], Route::Unsupported),
        "tasks" => return only(&[Method::GET], Route::ListTasks),
        "extendedAgentCard" => return only(&[Method::GET], Route::Unsupported),
        _ => {}
    }
    let Some(task) = rest.strip_prefix("tasks/") else {
        return Route::NotFound;
    };
    let segments = task.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        [id] if !id.is_empty() => match id.rsplit_once(':') {
            Some((id, "cancel")) if !id.is_empty() => {
                only(&[Method::POST], Route::TaskNotFound(id.to_owned()))
            }
            Some((id, "subscribe")) if !id.is_empty() => {
                only(&[Method::GET, Method::POST], Route::Unsupported)
            }
            Some(_) => Route::NotFound,
            None => only(&[Method::GET], Route::TaskNotFound((*id).to_owned())),
        },
        [id, "pushNotificationConfigs"] if !id.is_empty() => {
            only(&[Method::GET, Method::POST], Route::PushUnsupported)
        }
        [id, "pushNotificationConfigs", config]
            if !id.is_empty() && !config.is_empty() =>
        {
            only(&[Method::GET, Method::DELETE], Route::PushUnsupported)
        }
        _ => Route::NotFound,
    }
}

async fn brand_route(
    State(gateway): State<Arc<Gateway>>,
    Path((brand, rest)): Path<(String, String)>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(brand) = gateway.config.brand(&brand) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let route = route(&method, &rest);
    match route {
        Route::NotFound => return StatusCode::NOT_FOUND.into_response(),
        Route::MethodNotAllowed => {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        Route::Card => return card(&gateway.config, brand),
        Route::Metadata => return metadata(&gateway.config, brand),
        _ => {}
    }
    let authorization =
        headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let Some(caller) = gateway.verifier.verify(authorization).await else {
        return unauthorized();
    };
    if gateway.callers.check_key(&caller.issuer).is_err() {
        let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    }
    let brand = brand.clone();
    match route {
        Route::Send => Gateway::send(&gateway, brand, caller, headers, body).await,
        Route::DeviceAuthorization => blocking(&gateway, move |gateway| {
            gateway.device_authorization(&brand, &caller, &body)
        })
        .await
        .unwrap_or_else(IntoResponse::into_response),
        Route::Token => {
            blocking(&gateway, move |gateway| gateway.token(&brand, &caller, &body))
                .await
                .unwrap_or_else(IntoResponse::into_response)
        }
        Route::ListTasks => list_tasks(query.as_deref()),
        Route::TaskNotFound(id) => {
            a2a_error(A2aError::TaskNotFound, &format!("Task not found: {id}"))
        }
        Route::Unsupported => {
            a2a_error(A2aError::UnsupportedOperation, "Operation not supported")
        }
        Route::PushUnsupported => a2a_error(
            A2aError::PushNotificationNotSupported,
            "Push notifications are not supported",
        ),
        Route::Card | Route::Metadata | Route::MethodNotAllowed | Route::NotFound => {
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

// -- Agent Card and metadata (§2.1, §5.1) --

/// The version of the card content, not of the build: a card that names
/// the build version tells every reader which Aether release the node runs.
/// Increase it when the shape of the card changes.
const PACT_CARD_VERSION: &str = "1";

fn card(config: &PactConfig, brand: &Brand) -> Response {
    let interface = config.interface_url(brand);
    let mut scopes = serde_json::Map::new();
    scopes
        .insert(brand.scope_id.clone(), Value::from(brand.scope_description.as_str()));
    let card = json!({
        "name": brand.name,
        "description": brand.description,
        "supportedInterfaces": [{
            "url": interface,
            "protocolBinding": "HTTP+JSON",
            "protocolVersion": "1.0",
        }],
        "version": PACT_CARD_VERSION,
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "extendedAgentCard": false,
        },
        // The spec's examples name this scheme `paJwt`; the name is not
        // normative, and the conformance suite reads `platformJwt`.
        "securitySchemes": {
            "platformJwt": {
                "httpAuthSecurityScheme": { "scheme": "Bearer", "bearerFormat": "JWT" },
            },
            "userDelegation": {
                "oauth2SecurityScheme": {
                    "flows": {
                        "deviceCode": {
                            "deviceAuthorizationUrl": config.device_authorization_url(brand),
                            "tokenUrl": config.token_url(brand),
                            "scopes": scopes,
                        },
                    },
                    "oauth2MetadataUrl": config.oauth_metadata_url(brand),
                },
            },
        },
        "securityRequirements": [
            { "schemes": { "platformJwt": { "list": [] } } },
            { "schemes": { "platformJwt": { "list": [] }, "userDelegation": { "list": [] } } },
        ],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id": brand.capability,
            "name": brand.name,
            "description": brand.description,
            "tags": [brand.capability],
        }],
    });
    axum::Json(card).into_response()
}

fn metadata(config: &PactConfig, brand: &Brand) -> Response {
    axum::Json(json!({
        "issuer": config.oauth_issuer(brand),
        "device_authorization_endpoint": config.device_authorization_url(brand),
        "token_endpoint": config.token_url(brand),
        "jwks_uri": config.jwks_url(),
        "scopes_supported": [brand.scope_id],
        "grant_types_supported": [GrantType::DeviceCode.as_str(), GrantType::RefreshToken.as_str()],
    }))
    .into_response()
}

// -- Errors (§3.4, §5.5, §6) --

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum A2aError {
    InvalidParams,
    ContentTypeNotSupported,
    UnsupportedOperation,
    PushNotificationNotSupported,
    TaskNotFound,
    Internal,
}

impl A2aError {
    const fn parts(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::InvalidParams => {
                (StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", "INVALID_PARAMS")
            }
            Self::ContentTypeNotSupported => (
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "CONTENT_TYPE_NOT_SUPPORTED",
            ),
            Self::UnsupportedOperation => (
                StatusCode::BAD_REQUEST,
                "FAILED_PRECONDITION",
                "UNSUPPORTED_OPERATION",
            ),
            Self::PushNotificationNotSupported => (
                StatusCode::BAD_REQUEST,
                "FAILED_PRECONDITION",
                "PUSH_NOTIFICATION_NOT_SUPPORTED",
            ),
            Self::TaskNotFound => {
                (StatusCode::NOT_FOUND, "NOT_FOUND", "TASK_NOT_FOUND")
            }
            Self::Internal => {
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", "INTERNAL")
            }
        }
    }
}

/// The OAuth error codes the endpoints answer with (RFC 6749 §5.2,
/// RFC 8628 §3.5). Each code fixes its own HTTP status, so a code and a
/// status cannot disagree.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum OAuthError {
    InvalidClient,
    InvalidScope,
    InvalidGrant,
    UnsupportedGrantType,
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
    TemporarilyUnavailable,
    ServerError,
}

aether_core::named!(OAuthError {
    InvalidClient => "invalid_client",
    InvalidScope => "invalid_scope",
    InvalidGrant => "invalid_grant",
    UnsupportedGrantType => "unsupported_grant_type",
    AuthorizationPending => "authorization_pending",
    SlowDown => "slow_down",
    AccessDenied => "access_denied",
    ExpiredToken => "expired_token",
    TemporarilyUnavailable => "temporarily_unavailable",
    ServerError => "server_error",
});

impl OAuthError {
    const fn status(self) -> StatusCode {
        match self {
            // RFC 6749 §5.2: a client that fails to authenticate gets 401.
            Self::InvalidClient => StatusCode::UNAUTHORIZED,
            Self::TemporarilyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidScope
            | Self::InvalidGrant
            | Self::UnsupportedGrantType
            | Self::AuthorizationPending
            | Self::SlowDown
            | Self::AccessDenied
            | Self::ExpiredToken => StatusCode::BAD_REQUEST,
        }
    }
}

/// The `grant_type` values the token endpoint takes.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum GrantType {
    DeviceCode,
    RefreshToken,
}

aether_core::named!(GrantType {
    DeviceCode => "urn:ietf:params:oauth:grant-type:device_code",
    RefreshToken => "refresh_token",
});

fn a2a_json(status: StatusCode, body: &Value) -> Response {
    let mut response = (status, body.to_string()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(A2A_CONTENT_TYPE));
    response
}

fn a2a_error(error: A2aError, message: &str) -> Response {
    let (status, name, reason) = error.parts();
    a2a_json(
        status,
        &json!({
            "error": {
                "code": status.as_u16(),
                "status": name,
                "message": message,
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": reason,
                    "domain": "a2a-protocol.org",
                }],
            }
        }),
    )
}

/// `401` with no body (§3.4).
fn unauthorized() -> Response {
    challenge(r#"Bearer realm="a2a""#)
}

/// `401` for a bad delegation token: RFC 6750 `invalid_token` (§5.5).
fn invalid_token() -> Response {
    challenge(r#"Bearer realm="a2a", error="invalid_token""#)
}

fn challenge(value: &'static str) -> Response {
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(value));
    response
}

/// A reply that ends a request early. Small, unlike a `Response`, so the
/// helpers can return it; the route turns it into the response.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Failure {
    A2a(A2aError, &'static str),
    /// `429`: the node's queue is full.
    Overloaded,
    /// An OAuth error and its description.
    OAuth(OAuthError, &'static str),
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Self::A2a(error, message) => a2a_error(error, message),
            Self::Overloaded => {
                let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
                response
            }
            Self::OAuth(error, description) => no_store(
                error.status(),
                json!({ "error": error.as_str(), "error_description": description }),
            ),
        }
    }
}

const fn internal(message: &'static str) -> Failure {
    Failure::A2a(A2aError::Internal, message)
}

/// The node could not read its own policy.
#[derive(Debug)]
pub struct NodeUnavailable;

const fn oauth_store_error() -> Failure {
    Failure::OAuth(OAuthError::ServerError, "The gateway store failed")
}

fn no_store(status: StatusCode, body: Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

// -- Tasks (§2.2) --

fn list_tasks(query: Option<&str>) -> Response {
    let mut page_size = 50_u32;
    for (name, value) in parse_form(query.unwrap_or_default()) {
        if name == "pageSize" {
            match value.parse::<u32>() {
                Ok(size) if (1..=100).contains(&size) => page_size = size,
                _ => {
                    return a2a_error(
                        A2aError::InvalidParams,
                        "pageSize must be 1 to 100",
                    );
                }
            }
        }
    }
    a2a_json(
        StatusCode::OK,
        &json!({ "tasks": [], "nextPageToken": "", "pageSize": page_size, "totalSize": 0 }),
    )
}

// -- message:send (§4, §5.5, §5.6) --

/// A verified delegation for one request.
struct Delegation {
    claims: DelegationClaims,
}

struct Turn<'a> {
    brand: &'a Brand,
    caller: &'a PaIdentity,
    context_id: &'a str,
    message_id: &'a str,
}

/// A turn's reply text (for later transcripts) and its response body.
type TurnOutcome = Result<(Option<String>, Value), Failure>;

/// What checking a message gave: a task to run, or the response.
enum Begun {
    Run(Box<PendingTurn>),
    Answered(Response),
}

/// A stored message whose delegated task has not run yet.
struct PendingTurn {
    brand: Brand,
    caller: PaIdentity,
    context_id: String,
    message_id: String,
    delegation: Delegation,
    request: Request,
    key: String,
    task: Task,
    args_hash: String,
}

impl PendingTurn {
    fn turn(&self) -> Turn<'_> {
        Turn {
            brand: &self.brand,
            caller: &self.caller,
            context_id: &self.context_id,
            message_id: &self.message_id,
        }
    }
}

/// Runs `work` off the async workers, because it uses the PACT store.
async fn blocking<R: Send + 'static>(
    gateway: &Arc<Gateway>,
    work: impl FnOnce(&Gateway) -> R + Send + 'static,
) -> Result<R, Failure> {
    let gateway = Arc::clone(gateway);
    tokio::task::spawn_blocking(move || work(&gateway))
        .await
        .map_err(|_| internal("The node stopped"))
}

/// Returns when `events` names `task_id`, or when some events were lost.
/// Without events (the node is gone) it never returns.
async fn task_changed(events: &mut Option<broadcast::Receiver<String>>, task_id: &str) {
    loop {
        let Some(receiver) = events.as_mut() else {
            return std::future::pending().await;
        };
        match receiver.recv().await {
            Ok(id) if id == task_id => return,
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            Err(broadcast::error::RecvError::Closed) => *events = None,
        }
    }
}

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
}

impl Gateway {
    #[must_use]
    pub fn new(
        config: PactConfig,
        verifier: PaVerifier,
        key: Arc<ProviderKey>,
        store: SharedPactStore,
        node: Arc<dyn PactNode>,
        consent: Arc<ConsentLink>,
    ) -> Self {
        // Per personal agent: 60 requests at once, then 20 a second.
        let quota = Quota::per_second(NonZeroU32::MIN.saturating_add(19))
            .allow_burst(NonZeroU32::MIN.saturating_add(59));
        Self {
            config,
            verifier,
            key,
            store,
            node,
            consent,
            callers: RateLimiter::keyed(quota),
            poll_interval: Duration::from_secs(1),
        }
    }

    /// Closes idle contexts and forgets idle rate-limit entries. The node
    /// calls this periodically.
    pub fn maintain(&self) {
        let now = now();
        let _ = self
            .store()
            .close_idle_contexts(now.saturating_sub(CONTEXT_IDLE_SECONDS), now);
        self.callers.retain_recent();
    }

    fn store(&self) -> std::sync::MutexGuard<'_, PactStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers one `message:send`. The store work runs off the async
    /// workers, and only the wait for the task runs on them.
    async fn send(
        gateway: &Arc<Self>,
        brand: Brand,
        caller: PaIdentity,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let begun = blocking(gateway, move |gateway| {
            gateway.begin_send(brand, caller, &headers, &body)
        })
        .await;
        let pending = match begun {
            Ok(Begun::Run(pending)) => pending,
            Ok(Begun::Answered(response)) => return response,
            Err(failure) => return failure.into_response(),
        };
        let reply = gateway.run_task(&pending).await;
        blocking(gateway, move |gateway| {
            let outcome = reply.and_then(|reply| gateway.turn_reply(&pending, reply));
            gateway.finish_send(&pending.turn(), outcome)
        })
        .await
        .unwrap_or_else(IntoResponse::into_response)
    }

    /// Checks and stores the message. Returns the task to run for a
    /// delegated turn, else the response.
    fn begin_send(
        &self,
        brand: Brand,
        caller: PaIdentity,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Begun {
        let delegation = match headers.get(DELEGATION_HEADER) {
            None => None,
            Some(value) => {
                match self.verify_delegation_header(&brand, &caller, value) {
                    Ok(Some(delegation)) => Some(delegation),
                    Ok(None) => return Begun::Answered(invalid_token()),
                    Err(failure) => return Begun::Answered(failure.into_response()),
                }
            }
        };
        let Ok(request) = serde_json::from_slice::<a2a::SendMessageRequest>(body)
        else {
            return Begun::Answered(a2a_error(
                A2aError::InvalidParams,
                "Invalid request",
            ));
        };
        let message = request.message;
        if message.task_id.is_some() {
            return Begun::Answered(a2a_error(
                A2aError::TaskNotFound,
                "Task not found",
            ));
        }
        if message.role != a2a::Role::User || message.message_id.is_empty() {
            return Begun::Answered(a2a_error(
                A2aError::InvalidParams,
                "Invalid message",
            ));
        }
        let mut texts = Vec::with_capacity(message.parts.len());
        for part in &message.parts {
            match &part.content {
                a2a::PartContent::Text(text) => texts.push(text.as_str()),
                _ => {
                    return Begun::Answered(a2a_error(
                        A2aError::ContentTypeNotSupported,
                        "Only text parts are supported",
                    ));
                }
            }
        }
        let text = texts.join("\n");
        if text.trim().is_empty() {
            return Begun::Answered(a2a_error(
                A2aError::InvalidParams,
                "The message has no text",
            ));
        }
        if text.chars().count() > character_budget(&self.config, &brand) {
            return Begun::Answered(a2a_error(
                A2aError::InvalidParams,
                "The message is too long",
            ));
        }

        let context_id =
            match self.open_context(&brand, &caller, message.context_id.as_deref()) {
                Ok(context_id) => context_id,
                Err(failure) => return Begun::Answered(failure.into_response()),
            };
        if let Some(delegation) = &delegation {
            let bound =
                self.store().bind_context_user(&context_id, &delegation.claims.sub);
            match bound {
                Ok(()) => {}
                Err(StoreError::Unauthorized) => {
                    return Begun::Answered(a2a_error(
                        A2aError::InvalidParams,
                        "Unknown contextId",
                    ));
                }
                Err(_) => {
                    return Begun::Answered(
                        internal("The conversation cannot be stored").into_response(),
                    );
                }
            }
        }
        let reservation = self.store().reserve_message(
            &context_id,
            &message.message_id,
            &text,
            now(),
        );
        match reservation {
            Ok(Reservation::New) => {}
            Ok(Reservation::Existing(Some(stored))) => {
                return Begun::Answered(
                    serde_json::from_str::<Value>(&stored).map_or_else(
                        |_| {
                            a2a_error(
                                A2aError::Internal,
                                "The stored reply is unreadable",
                            )
                        },
                        |stored| a2a_json(StatusCode::OK, &stored),
                    ),
                );
            }
            Ok(Reservation::Existing(None)) => {
                return Begun::Answered(a2a_error(
                    A2aError::InvalidParams,
                    "The message is still being answered",
                ));
            }
            Err(StoreError::NotFound) => {
                return Begun::Answered(a2a_error(
                    A2aError::UnsupportedOperation,
                    "The conversation is closed",
                ));
            }
            Err(_) => {
                return Begun::Answered(a2a_error(
                    A2aError::Internal,
                    "The conversation cannot be stored",
                ));
            }
        }

        let turn = Turn {
            brand: &brand,
            caller: &caller,
            context_id: &context_id,
            message_id: &message.message_id,
        };
        let delegation = match delegation {
            Some(delegation)
                if delegation.claims.scopes().any(|scope| scope == brand.scope_id) =>
            {
                delegation
            }
            other => {
                let outcome = self.step_up(&turn, other.as_ref());
                return Begun::Answered(self.finish_send(&turn, outcome));
            }
        };
        let prepared = self.prepare_task(&turn, &text);
        let (request, key, task, args_hash) = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Begun::Answered(self.finish_send(&turn, Err(failure)));
            }
        };
        Begun::Run(Box::new(PendingTurn {
            brand,
            caller,
            context_id,
            message_id: message.message_id,
            delegation,
            request,
            key,
            task,
            args_hash,
        }))
    }

    /// Stores the reply of a turn, or forgets the message when the turn
    /// failed, and returns the response.
    fn finish_send(&self, turn: &Turn<'_>, outcome: TurnOutcome) -> Response {
        match outcome {
            Ok((reply_text, response)) => {
                if self
                    .store()
                    .complete_message(
                        turn.context_id,
                        turn.message_id,
                        reply_text.as_deref(),
                        &response.to_string(),
                    )
                    .is_err()
                {
                    return a2a_error(A2aError::Internal, "The reply cannot be stored");
                }
                a2a_json(StatusCode::OK, &response)
            }
            Err(failure) => {
                // Nothing was answered: a retry with the same id runs again.
                let _ = self.store().discard_message(turn.context_id, turn.message_id);
                failure.into_response()
            }
        }
    }

    /// The checks of §5.5 step 2, plus this node's own: the grant is bound
    /// to the personal agent's user that started the device flow.
    fn verify_delegation_header(
        &self,
        brand: &Brand,
        caller: &PaIdentity,
        value: &HeaderValue,
    ) -> Result<Option<Delegation>, Failure> {
        let token = value.to_str().ok();
        let Some(token) = token else {
            return Ok(None);
        };
        let Some((scheme, token)) = token.split_once(' ') else {
            return Ok(None);
        };
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Ok(None);
        }
        let Some(claims) = verify_delegation(
            &self.key,
            token.trim(),
            &self.config.oauth_issuer(brand),
            &self.config.interface_url(brand),
        ) else {
            return Ok(None);
        };
        if claims.client_id != caller.issuer {
            return Ok(None);
        }
        let grant =
            self.store().grant(&claims.grant_id).map_err(|_| oauth_store_error())?;
        let Some(grant) = grant else {
            return Ok(None);
        };
        let at = now();
        let live = grant_binds(
            &grant,
            &brand.id,
            caller,
            at,
            GrantBinding::Delegation { brand_user: &claims.sub },
            self.node.has_grant(&grant.grant_id).map_err(|_| oauth_store_error())?,
        );
        Ok(live.then_some(Delegation { claims }))
    }

    /// The context to answer in: a new one, or the caller's own open one.
    fn open_context(
        &self,
        brand: &Brand,
        caller: &PaIdentity,
        context_id: Option<&str>,
    ) -> Result<String, Failure> {
        let Some(context_id) = context_id else {
            let context_id = uuid::Uuid::new_v4().to_string();
            self.store()
                .create_context(
                    &context_id,
                    &brand.id,
                    &caller.issuer,
                    &caller.subject,
                    now(),
                )
                .map_err(|_| internal("The conversation cannot be stored"))?;
            return Ok(context_id);
        };
        // §4.2: another owner's context is refused exactly like an unknown
        // one.
        let context = self
            .store()
            .context(context_id)
            .map_err(|_| internal("The conversation cannot be read"))?
            .filter(|context| {
                context.brand_id == brand.id
                    && context.pa_issuer == caller.issuer
                    && context.pa_subject == caller.subject
            })
            .ok_or(Failure::A2a(A2aError::InvalidParams, "Unknown contextId"))?;
        if context.closed {
            return Err(Failure::A2a(
                A2aError::UnsupportedOperation,
                "The conversation is closed",
            ));
        }
        Ok(context.context_id)
    }

    /// §5.5 step-up: a task in `TASK_STATE_AUTH_REQUIRED` with the missing
    /// scope and a new login link. The conversation stays open.
    fn step_up(&self, turn: &Turn<'_>, delegation: Option<&Delegation>) -> TurnOutcome {
        let mut metadata = serde_json::Map::new();
        metadata.insert("pact.missingScopes".to_owned(), json!([turn.brand.scope_id]));
        if let Ok(authorization) = self.create_device_authorization(
            turn.brand,
            turn.caller,
            &turn.brand.scope_id,
        ) {
            metadata.insert(
                "pact.verificationUriComplete".to_owned(),
                Value::String(authorization.verification_uri_complete),
            );
        }
        if let Some(delegation) = delegation {
            metadata.insert(
                "pact.receipt".to_owned(),
                self.receipt(turn, delegation, Vec::new(), Vec::new())?,
            );
        }
        let text = format!(
            "I need your permission to work for you: {}.",
            turn.brand.scope_description
        );
        let task = json!({
            "task": {
                "id": format!("stepup-{}", uuid::Uuid::new_v4()),
                "contextId": turn.context_id,
                "status": {
                    "state": "TASK_STATE_AUTH_REQUIRED",
                    "message": {
                        "messageId": uuid::Uuid::new_v4().to_string(),
                        "contextId": turn.context_id,
                        "role": "ROLE_AGENT",
                        "parts": [{ "text": text }],
                    },
                },
                "metadata": metadata,
            }
        });
        Ok((None, task))
    }

    /// The Aether task for one delegated turn: its request, idempotency key,
    /// task and the hash of its text.
    fn prepare_task(
        &self,
        turn: &Turn<'_>,
        text: &str,
    ) -> Result<(Request, String, Task, String), Failure> {
        let transcript = self
            .store()
            .transcript(turn.context_id, turn.message_id, MAX_TRANSCRIPT_TURNS)
            .map_err(|_| internal("The conversation cannot be read"))?;
        let remote_text =
            prompt(&transcript, text, character_budget(&self.config, turn.brand));
        let args_hash = sha256(&remote_text);
        // Ids are derived from the message, so a retry after a time-out
        // finds the same task instead of starting a second one.
        let key = turn_key(turn);
        let task_id = format!("pact-{key}");
        let request = Request {
            id: RequestId::new(format!("pact-request-{key}")),
            task_id: TaskId::new(&task_id),
            caller_agent_id: AgentId::new(caller_agent_id(&turn.caller.issuer)),
            caller_endpoint_id: EndpointId::new(caller_endpoint_id(turn.caller)),
            scope_id: ScopeId::new(SCOPE_ID),
            capability_id: CapabilityId::new(&turn.brand.capability),
            workspace_id: WorkspaceId::new(&turn.brand.workspace),
            access_mode: turn.brand.access_mode.clone(),
        };
        let task = Task {
            id: task_id,
            skill: turn.brand.capability.clone(),
            workspace_id: turn.brand.workspace.clone(),
            remote_text,
        };
        Ok((request, key, task, args_hash))
    }

    /// Runs the task of one delegated turn and waits for its result.
    async fn run_task(&self, pending: &PendingTurn) -> Result<String, Failure> {
        let node = Arc::clone(&self.node);
        let (request, key, task) =
            (pending.request.clone(), pending.key.clone(), pending.task.clone());
        let admitted =
            tokio::task::spawn_blocking(move || node.admit(&request, &key, &task))
                .await
                .map_err(|_| internal("The node stopped"))?;
        let task_id = match admitted {
            Ok(task_id) => task_id,
            Err(ProofError::Store(StoreError::Overloaded)) => {
                return Err(Failure::Overloaded);
            }
            Err(_) => return Err(internal("The node refused the task")),
        };
        self.wait_for_result(&task_id).await
    }

    /// The signed reply of a delegated turn that its task answered.
    fn turn_reply(&self, pending: &PendingTurn, reply: String) -> TurnOutcome {
        let turn = pending.turn();
        let actions = vec![ReceiptAction {
            tool: format!("aether.{}", turn.brand.capability),
            args_hash: Some(pending.args_hash.clone()),
        }];
        let receipt = self.receipt(
            &turn,
            &pending.delegation,
            vec![turn.brand.scope_id.clone()],
            actions,
        )?;
        let response = json!({
            "message": {
                "messageId": uuid::Uuid::new_v4().to_string(),
                "contextId": turn.context_id,
                "role": "ROLE_AGENT",
                "parts": [{ "text": reply }],
                "metadata": { "pact.receipt": receipt },
            }
        });
        Ok((Some(reply), response))
    }

    /// Waits until the task ends, or until the policy's time limit and a
    /// margin pass. Each change of the task, or else each poll interval,
    /// reads the task again.
    async fn wait_for_result(&self, task_id: &str) -> Result<String, Failure> {
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(
            self.config.max_seconds.saturating_add(30),
        ));
        // Subscribe before the first read, so no change is lost between them.
        let mut events = Some(self.node.subscribe_events());
        loop {
            let node = Arc::clone(&self.node);
            let id = task_id.to_owned();
            let snapshot = tokio::task::spawn_blocking(move || node.snapshot(&id))
                .await
                .map_err(|_| internal("The node stopped"))?
                .map_err(|_| internal("The task cannot be read"))?;
            let text = match snapshot {
                (TaskState::Succeeded, result) => Some(result.unwrap_or_default()),
                (TaskState::Failed, _) => Some("The task failed.".to_owned()),
                (TaskState::Rejected, _) => {
                    Some("The node refused the task.".to_owned())
                }
                (TaskState::Cancelled, _) => Some("The task was cancelled.".to_owned()),
                (TaskState::UnknownResult | TaskState::OperatorReviewed, _) => Some(
                    "The task ended in an unknown state. The node owner must review it."
                        .to_owned(),
                ),
                (
                    TaskState::PendingApproval
                    | TaskState::Queued
                    | TaskState::Leased
                    | TaskState::Running,
                    _,
                ) => None,
            };
            if let Some(text) = text {
                return Ok(text);
            }
            let now = tokio::time::Instant::now();
            if deadline.is_none_or(|deadline| now >= deadline) {
                return Err(internal(
                    "The task is still running. Send the same message again to read its result.",
                ));
            }
            let poll = now.checked_add(self.poll_interval).unwrap_or(now);
            let until = deadline.map_or(poll, |deadline| deadline.min(poll));
            let _ = tokio::time::timeout_at(until, task_changed(&mut events, task_id))
                .await;
        }
    }

    fn receipt(
        &self,
        turn: &Turn<'_>,
        delegation: &Delegation,
        scopes_used: Vec<String>,
        actions: Vec<ReceiptAction>,
    ) -> Result<Value, Failure> {
        let claims = ReceiptClaims {
            grant_id: delegation.claims.grant_id.clone(),
            user: delegation.claims.sub.clone(),
            pa: turn.caller.issuer.clone(),
            brand: self.config.interface_url(turn.brand),
            scopes_used,
            actions,
            ts: rfc3339(now()),
        };
        let receipt = sign_receipt(&self.key, &claims)
            .map_err(|_| internal("The receipt cannot be signed"))?;
        self.store()
            .put_receipt(
                turn.context_id,
                turn.message_id,
                &claims.grant_id,
                &receipt.jws,
                now(),
            )
            .map_err(|_| internal("The receipt cannot be stored"))?;
        Ok(json!(receipt))
    }

    // -- Device authorization (§5.3, RFC 8628) --

    fn create_device_authorization(
        &self,
        brand: &Brand,
        caller: &PaIdentity,
        scopes: &str,
    ) -> Result<DeviceAuthorization, Failure> {
        let unavailable = Failure::OAuth(
            OAuthError::TemporarilyUnavailable,
            "The node owner's consent page is not running",
        );
        let base = self.consent.get().ok_or(unavailable)?;
        let device_code = random_token("dc_").ok_or(unavailable)?;
        let user_code = user_code().ok_or(unavailable)?;
        let now = now();
        self.store()
            .create_device_authorization(&NewDeviceAuthorization {
                device_code_hash: digest_hex(&device_code),
                user_code: user_code.clone(),
                brand_id: brand.id.clone(),
                pa_issuer: caller.issuer.clone(),
                pa_subject: caller.subject.clone(),
                scopes: scopes.to_owned(),
                interval_seconds: POLL_INTERVAL_SECONDS,
                created_at: now,
                expires_at: now.saturating_add(DEVICE_CODE_SECONDS),
            })
            .map_err(|_| oauth_store_error())?;
        let verification_uri = format!("{base}/pact/consent");
        let verification_uri_complete =
            format!("{verification_uri}?user_code={user_code}");
        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
        })
    }

    fn device_authorization(
        &self,
        brand: &Brand,
        caller: &PaIdentity,
        body: &[u8],
    ) -> Response {
        let form = parse_form(&String::from_utf8_lossy(body));
        let authorization = check_client(&form, caller)
            .and_then(|()| {
                let scopes = scope_list(field(&form, "scope").unwrap_or_default());
                if scopes.is_empty()
                    || scopes.iter().any(|scope| *scope != brand.scope_id)
                {
                    return Err(Failure::OAuth(
                        OAuthError::InvalidScope,
                        "Request scope ids listed on the Agent Card",
                    ));
                }
                Ok(())
            })
            .and_then(|()| {
                self.create_device_authorization(brand, caller, &brand.scope_id)
            });
        match authorization {
            Ok(authorization) => no_store(
                StatusCode::OK,
                json!({
                    "device_code": authorization.device_code,
                    "user_code": authorization.user_code,
                    "verification_uri": authorization.verification_uri,
                    "verification_uri_complete": authorization.verification_uri_complete,
                    "expires_in": DEVICE_CODE_SECONDS,
                    "interval": POLL_INTERVAL_SECONDS,
                }),
            ),
            Err(failure) => failure.into_response(),
        }
    }

    fn token(&self, brand: &Brand, caller: &PaIdentity, body: &[u8]) -> Response {
        let form = parse_form(&String::from_utf8_lossy(body));
        let now = now();
        check_client(&form, caller)
            .and_then(|()| self.redeem(&form, brand, caller, now))
            .and_then(|grant| self.issue_tokens(brand, &grant, now))
            .unwrap_or_else(IntoResponse::into_response)
    }

    /// The grant a token request redeems: an approved device code, or a
    /// refresh token. The grant must still be live and belong to `caller`.
    fn redeem(
        &self,
        form: &[(String, String)],
        brand: &Brand,
        caller: &PaIdentity,
        now: i64,
    ) -> Result<PactGrant, Failure> {
        let grant_type = field(form, "grant_type").and_then(GrantType::parse).ok_or(
            Failure::OAuth(OAuthError::UnsupportedGrantType, "Unsupported grant_type"),
        )?;
        let grant = match grant_type {
            GrantType::DeviceCode => {
                let device_code = field(form, "device_code").unwrap_or_default();
                let poll = self.store().poll_device(
                    &digest_hex(device_code),
                    &brand.id,
                    &caller.issuer,
                    &caller.subject,
                    now,
                );
                approved_grant(poll)?
            }
            GrantType::RefreshToken => {
                let refresh_token = field(form, "refresh_token").unwrap_or_default();
                let used = self
                    .store()
                    .use_refresh_token(&digest_hex(refresh_token), now)
                    .map_err(|_| oauth_store_error())?;
                used.ok_or(Failure::OAuth(
                    OAuthError::InvalidGrant,
                    "Unknown, used or expired refresh_token",
                ))?
            }
        };
        if grant_binds(
            &grant,
            &brand.id,
            caller,
            now,
            GrantBinding::Redeem,
            self.node.has_grant(&grant.grant_id).map_err(|_| oauth_store_error())?,
        ) {
            Ok(grant)
        } else {
            Err(Failure::OAuth(
                OAuthError::InvalidGrant,
                "The grant is not valid for this caller",
            ))
        }
    }

    fn issue_tokens(
        &self,
        brand: &Brand,
        grant: &PactGrant,
        now: i64,
    ) -> Result<Response, Failure> {
        let server_error =
            Failure::OAuth(OAuthError::ServerError, "Tokens cannot be issued");
        let exp = now.saturating_add(ACCESS_TOKEN_SECONDS).min(grant.expires_at);
        let claims = DelegationClaims {
            iss: self.config.oauth_issuer(brand),
            aud: self.config.interface_url(brand),
            sub: grant.brand_user.clone(),
            client_id: grant.pa_issuer.clone(),
            scope: grant.scopes.clone(),
            grant_id: grant.grant_id.clone(),
            iat: now,
            exp,
        };
        let access_token =
            sign_delegation(&self.key, &claims).map_err(|_| server_error)?;
        let refresh_token = random_token("rt_").ok_or(server_error)?;
        self.store()
            .put_refresh_token(
                &digest_hex(&refresh_token),
                &grant.grant_id,
                grant.expires_at,
            )
            .map_err(|_| server_error)?;
        Ok(no_store(
            StatusCode::OK,
            json!({
                "token_type": "Bearer",
                "access_token": access_token,
                "refresh_token": refresh_token,
                "expires_in": exp.saturating_sub(now),
                "scope": grant.scopes,
            }),
        ))
    }
}

/// RFC 8628 §3.4 asks the client to identify itself; here `client_id` must
/// be the issuer of the personal-agent JWT that authenticated the request.
fn check_client(form: &[(String, String)], caller: &PaIdentity) -> Result<(), Failure> {
    if field(form, "client_id") == Some(caller.issuer.as_str()) {
        Ok(())
    } else {
        Err(Failure::OAuth(
            OAuthError::InvalidClient,
            "client_id must equal the personal-agent issuer",
        ))
    }
}

/// The token-endpoint answer to one device-code poll (RFC 8628 §3.5).
pub(crate) fn approved_grant(
    poll: Result<DevicePoll, StoreError>,
) -> Result<PactGrant, Failure> {
    match poll {
        Ok(DevicePoll::Approved(grant)) => Ok(grant),
        Ok(DevicePoll::Pending { slow_down: true }) => {
            Err(Failure::OAuth(OAuthError::SlowDown, "Poll less often"))
        }
        Ok(DevicePoll::Pending { slow_down: false }) => Err(Failure::OAuth(
            OAuthError::AuthorizationPending,
            "Waiting for the node owner",
        )),
        Ok(DevicePoll::Denied) => Err(Failure::OAuth(
            OAuthError::AccessDenied,
            "The node owner denied access",
        )),
        Ok(DevicePoll::Expired) => {
            Err(Failure::OAuth(OAuthError::ExpiredToken, "The device code expired"))
        }
        Ok(DevicePoll::Unknown | DevicePoll::Consumed) => {
            Err(Failure::OAuth(OAuthError::InvalidGrant, "Unknown or used device_code"))
        }
        Err(_) => Err(oauth_store_error()),
    }
}

/// The characters one turn may send to Codex: the policy's context limit
/// (Codex counts one token per three characters), less the skill name and
/// the transcript framing.
fn character_budget(config: &PactConfig, brand: &Brand) -> usize {
    usize::try_from(config.max_context_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(3)
        .saturating_sub(brand.capability.chars().count())
        .saturating_sub(PROMPT_FRAMING_CHARACTERS)
}

/// The text Codex gets: the newest earlier turns that fit in half of
/// `budget`, oldest first, then the new message.
fn prompt(transcript: &[(String, String)], text: &str, budget: usize) -> String {
    let mut room = budget.saturating_sub(text.chars().count()).min(budget / 2);
    let mut kept = Vec::new();
    for (request, reply) in transcript.iter().rev() {
        let size = request
            .chars()
            .count()
            .saturating_add(reply.chars().count())
            .saturating_add(16);
        if size > room {
            break;
        }
        room = room.saturating_sub(size);
        kept.push((request, reply));
    }
    if kept.is_empty() {
        return text.to_owned();
    }
    let mut prompt =
        String::from("Earlier turns of this conversation, oldest first:\n");
    for (request, reply) in kept.into_iter().rev() {
        prompt.push_str("User: ");
        prompt.push_str(request);
        prompt.push_str("\nAgent: ");
        prompt.push_str(reply);
        prompt.push('\n');
    }
    prompt.push_str("\nThe user's new message:\n");
    prompt.push_str(text);
    prompt
}

/// A stable key for one message: the same context and message id give the
/// same task.
fn turn_key(turn: &Turn<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"aether.pact-turn.v1\0");
    for part in [turn.brand.id.as_str(), turn.context_id, turn.message_id] {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().to_hex().get(..32).unwrap_or_default().to_owned()
}

fn parse_form(source: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(source.as_bytes()).into_owned().collect()
}

fn field<'a>(form: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let mut values = form.iter().filter(|(field, _)| field == name);
    let value = values.next()?;
    values.next().is_none().then_some(value.1.as_str())
}

fn scope_list(scope: &str) -> Vec<&str> {
    let mut scopes =
        scope.split(' ').filter(|scope| !scope.is_empty()).collect::<Vec<_>>();
    scopes.dedup();
    scopes
}

fn random_token(prefix: &str) -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// `XXXX-XXXX` from `USER_CODE_ALPHABET`.
fn user_code() -> Option<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    let mut code = String::with_capacity(9);
    for (index, byte) in bytes.iter().enumerate() {
        if index == 4 {
            code.push('-');
        }
        let position = usize::from(*byte).checked_rem(USER_CODE_ALPHABET.len())?;
        code.push(char::from(*USER_CODE_ALPHABET.get(position)?));
    }
    Some(code)
}

/// A stored digest of a secret: device codes and refresh tokens.
fn digest_hex(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

fn sha256(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, value.as_bytes()))
}

#[must_use]
pub fn rfc3339(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|instant| {
            instant.format(&time::format_description::well_known::Rfc3339).ok()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
