//! A hub for tests. It serves the routes of `server/PROTOCOL.md` that a node
//! uses, and it checks each signature as the real hub does. Each request is
//! recorded with its headers.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::protocol::MAX_WAIT_SECONDS;
use super::{AgentKind, AgentRow, HubClient, protocol};

/// One request as the hub received it.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// The path, and `?` with the query when there is one: what the node
    /// signed.
    pub target: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// One invite on the hub.
#[derive(Debug, Clone)]
pub struct TestInvite {
    pub id: String,
    pub sender: String,
    pub receiver: String,
    pub days: u64,
    pub status: String,
    pub bundle: Option<String>,
    pub created_at: u64,
    /// The receiver when it blocked the sender: the row is for the sender
    /// only.
    pub hidden_from: Option<String>,
    /// `invite`, or `request` for a row that the receiver asked for.
    pub origin: String,
}

/// One owner link or claim on the hub.
#[derive(Debug, Clone)]
pub struct TestOwnerLink {
    pub id: String,
    pub service: String,
    pub personal: String,
    pub expires_at: u64,
    pub confirmed: bool,
}

/// What the hub knows. Tests read and change it through [`TestHub::state`].
#[derive(Debug, Default)]
pub struct HubState {
    /// Handle by endpoint ID.
    pub accounts: BTreeMap<String, String>,
    pub kinds: BTreeMap<String, AgentKind>,
    pub searchable: BTreeSet<String>,
    /// (owner, blocked) endpoint pairs.
    pub blocks: BTreeSet<(String, String)>,
    /// The most blocks an owner can have; `None` is no limit.
    pub block_limit: Option<usize>,
    pub versions: BTreeMap<String, u64>,
    pub invites: Vec<TestInvite>,
    /// (owner, contact) endpoint pairs.
    pub contacts: BTreeSet<(String, String)>,
    pub online: BTreeSet<String>,
    pub listings: BTreeMap<String, Value>,
    pub owner_links: Vec<TestOwnerLink>,
    pub requests: Vec<Recorded>,
    /// Acts as a hub from before protocol 2: no `GET /v1/hub`, and a claim
    /// body with `name` is invalid.
    pub old: bool,
    /// Request paths whose answer is lost: the hub does the request, then
    /// drops the connection before it answers.
    pub lose_answers: BTreeSet<String>,
    next_id: u64,
}

impl HubState {
    /// Gives `endpoint` the full name `handle`, such as `ana#0001`.
    pub fn add_account(&mut self, endpoint: &str, handle: &str) {
        self.accounts.insert(endpoint.to_owned(), handle.to_owned());
        self.versions.entry(endpoint.to_owned()).or_insert(1);
    }

    /// Adds an invite from `sender` to `receiver` and returns its id.
    pub fn add_invite(&mut self, sender: &str, receiver: &str, status: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        let id = format!("{:032x}", self.next_id);
        self.invites.push(TestInvite {
            id: id.clone(),
            sender: sender.to_owned(),
            receiver: receiver.to_owned(),
            days: 7,
            status: status.to_owned(),
            bundle: None,
            created_at: now(),
            hidden_from: None,
            origin: "invite".to_owned(),
        });
        self.bump(&[sender, receiver]);
        id
    }

    /// Adds a request from `requester` to the service agent `service` and
    /// returns its id. The service is the sender of the row.
    pub fn add_request(&mut self, service: &str, requester: &str, days: u64) -> String {
        let id = self.add_invite(service, requester, "requested");
        if let Some(invite) = self.invites.iter_mut().find(|invite| invite.id == id) {
            invite.origin = "request".to_owned();
            invite.days = days;
        }
        id
    }

    /// `owner` blocks `blocked`: the rows and the contact pair between them
    /// go. `false` at the block limit.
    fn block(&mut self, owner: &str, blocked: &str) -> bool {
        let count = self.blocks.iter().filter(|(by, _)| by == owner).count();
        if self.block_limit.is_some_and(|limit| count >= limit) {
            return false;
        }
        self.blocks.insert((owner.to_owned(), blocked.to_owned()));
        self.invites.retain(|invite| {
            !((invite.sender == owner && invite.receiver == blocked)
                || (invite.sender == blocked && invite.receiver == owner))
        });
        self.contacts.remove(&(owner.to_owned(), blocked.to_owned()));
        self.contacts.remove(&(blocked.to_owned(), owner.to_owned()));
        self.bump(&[owner, blocked]);
        true
    }

    fn bump(&mut self, endpoints: &[&str]) {
        for endpoint in endpoints {
            if let Some(version) = self.versions.get_mut(*endpoint) {
                *version = version.saturating_add(1);
            }
        }
    }

    fn handle(&self, endpoint: &str) -> String {
        self.accounts.get(endpoint).cloned().unwrap_or_default()
    }

    fn kind(&self, endpoint: &str) -> AgentKind {
        self.kinds.get(endpoint).copied().unwrap_or(AgentKind::Personal)
    }

    fn account_json(&self, endpoint: &str) -> Value {
        json!({
            "handle": self.handle(endpoint),
            "endpoint": endpoint,
            "kind": self.kind(endpoint),
            "searchable": self.searchable.contains(endpoint),
        })
    }

    fn owner_link_json(&self, link: &TestOwnerLink, asker: &str) -> Value {
        let personal_side = link.personal == asker;
        json!({
            "id": link.id,
            "role": if personal_side {
                AgentKind::Personal.as_str()
            } else {
                AgentKind::Service.as_str()
            },
            "service": self.handle(&link.service),
            "personal": self.handle(&link.personal),
            "expiresAt": link.expires_at,
            "confirmed": link.confirmed,
            "personalEndpoint": (!personal_side).then(|| link.personal.clone()),
            "serviceEndpoint": (personal_side && link.confirmed).then(|| link.service.clone()),
        })
    }

    /// A search row of the listed `service`, as `asker` sees it.
    fn agent_json(&self, asker: &str, service: &str, held: &Value) -> Value {
        let contact = self.contacts.contains(&(asker.to_owned(), service.to_owned()));
        let mut row = json!({
            "handle": self.handle(service),
            "listing": held["listing"],
            "signature": held["signature"],
            "expiresAt": held["expiresAt"],
            "online": self.online.contains(service),
            "contact": contact,
        });
        if contact {
            row["endpoint"] = json!(service);
        }
        row
    }

    /// A people row of `person`, as `asker` sees it.
    fn person_json(&self, asker: &str, person: &str) -> Value {
        let contact = self.contacts.contains(&(asker.to_owned(), person.to_owned()));
        let mut row = json!({ "handle": self.handle(person), "contact": contact });
        if contact {
            row["online"] = json!(self.online.contains(person));
            row["endpoint"] = json!(person);
        }
        row
    }

    fn invite_json(&self, invite: &TestInvite, asker: &str) -> Value {
        let sender = invite.sender == asker;
        json!({
            "id": invite.id,
            "from": self.handle(&invite.sender),
            "to": self.handle(&invite.receiver),
            "role": if sender { "sender" } else { "receiver" },
            "days": invite.days,
            "status": invite.status,
            "origin": invite.origin,
            "receiverEndpoint": (sender && invite.status != "pending")
                .then(|| invite.receiver.clone()),
            "bundle": (!sender && invite.status == "delivered")
                .then(|| invite.bundle.clone())
                .flatten(),
            "senderEndpoint": (!sender && invite.status == "delivered")
                .then(|| invite.sender.clone()),
            "createdAt": invite.created_at,
        })
    }

    fn snapshot(&self, asker: &str) -> Value {
        let invites: Vec<Value> = self
            .invites
            .iter()
            .filter(|invite| invite.sender == asker || invite.receiver == asker)
            .filter(|invite| invite.hidden_from.as_deref() != Some(asker))
            .map(|invite| self.invite_json(invite, asker))
            .collect();
        json!({
            "version": self.versions.get(asker).copied().unwrap_or_default(),
            "invites": invites,
        })
    }
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<HubState>>,
    changed: Arc<Notify>,
}

/// A running test hub on `127.0.0.1`.
pub struct TestHub {
    pub url: String,
    shared: Shared,
    server: tokio::task::JoinHandle<()>,
}

impl TestHub {
    pub async fn start() -> Self {
        let shared = Shared {
            state: Arc::new(Mutex::new(HubState::default())),
            changed: Arc::new(Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test hub must bind");
        let address = listener.local_addr().expect("test hub must have an address");
        let router = Router::new().fallback(handle).with_state(shared.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self { url: format!("http://{address}"), shared, server }
    }

    pub fn state(&self) -> MutexGuard<'_, HubState> {
        self.shared.state.lock().expect("test hub state must lock")
    }

    /// A copy of every request so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.state().requests.clone()
    }

    /// Lists the service agent of `host` as `handle`, with the listing
    /// `bytes` signed for one hour by the endpoint key in `signer`. Returns
    /// the search row as a stranger gets it.
    pub fn add_listing(
        &self,
        host: &Path,
        signer: &Path,
        handle: &str,
        bytes: &[u8],
    ) -> AgentRow {
        let endpoint = crate::data_dir::DataDir::new(host)
            .endpoint_key()
            .expect("key")
            .public()
            .to_string();
        let key = crate::data_dir::DataDir::new(signer).endpoint_key().expect("key");
        let expires_at = now().saturating_add(3600);
        let row = AgentRow {
            handle: handle.to_owned(),
            listing: URL_SAFE_NO_PAD.encode(bytes),
            signature: protocol::sign_listing(&key, bytes, expires_at),
            expires_at,
            online: false,
            contact: false,
            endpoint: None,
            owner: None,
        };
        let mut state = self.state();
        state.add_account(&endpoint, handle);
        state.kinds.insert(endpoint.clone(), AgentKind::Service);
        state.listings.insert(
            endpoint,
            json!({
                "listed": true,
                "listing": row.listing,
                "signature": row.signature,
                "expiresAt": expires_at,
            }),
        );
        drop(state);
        row
    }

    /// A client for this hub that signs with the endpoint key in `data_dir`.
    pub fn client(&self, data_dir: &Path) -> HubClient {
        HubClient::for_data_dir(&self.url, data_dir)
            .expect("test hub client must build")
    }
}

impl Drop for TestHub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn now() -> u64 {
    aether_core::time::unix_seconds(SystemTime::now()).unwrap_or_default()
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, axum::Json(json!({ "error": code }))).into_response()
}

fn ok(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

/// The signed target as Plug reads it: the path, and `?` with the query
/// only when the query string is not empty.
fn target(uri: &Uri) -> String {
    protocol::hub_sign_target(uri.path(), uri.query())
}

/// The endpoint that signed the request, when the signature verifies.
fn authenticate(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Option<String> {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let endpoint = header(protocol::ENDPOINT_HEADER)?;
    let time = header(protocol::TIME_HEADER)?;
    let signature = URL_SAFE_NO_PAD.decode(header(protocol::SIGNATURE_HEADER)?).ok()?;
    let signature = iroh::Signature::from_bytes(&signature.try_into().ok()?);
    let key = iroh::PublicKey::from_str(endpoint).ok()?;
    let message =
        protocol::hub_request_message(method.as_str(), &target(uri), time, body);
    key.verify(message.as_bytes(), &signature).ok()?;
    Some(endpoint.to_owned())
}

/// Whether `endpoint` signed `message` with the unpadded base64url
/// `signature`.
fn signed(endpoint: &str, message: &str, signature: &str) -> bool {
    let Some(bytes) = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
    else {
        return false;
    };
    iroh::PublicKey::from_str(endpoint).is_ok_and(|key| {
        key.verify(message.as_bytes(), &iroh::Signature::from_bytes(&bytes)).is_ok()
    })
}

fn query_value<'a>(uri: &'a Uri, name: &str) -> Option<&'a str> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(endpoint) = authenticate(&method, &uri, &headers, &body) else {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    {
        let mut state = shared.state.lock().expect("test hub state must lock");
        state.requests.push(Recorded {
            method: method.to_string(),
            target: target(&uri),
            headers: headers
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                })
                .collect(),
            body: body.to_vec(),
        });
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let segments: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
    if method == Method::GET && segments == ["v1", "invites"] {
        return list_invites(&shared, &endpoint, &uri).await;
    }
    let query: BTreeMap<String, String> = reqwest::Url::parse(&format!(
        "http://hub.test/?{}",
        uri.query().unwrap_or_default()
    ))
    .map(|url| url.query_pairs().into_owned().collect())
    .unwrap_or_default();
    let mut state = shared.state.lock().expect("test hub state must lock");
    let response = route(&mut state, &endpoint, &method, &segments, &body, &query);
    let lose_answer = state.lose_answers.contains(uri.path());
    drop(state);
    shared.changed.notify_waiters();
    if lose_answer {
        // Ends the connection task without an answer, as a timeout after
        // the hub did the request would. No panic message is printed.
        std::panic::resume_unwind(Box::new("the test hub lost the answer"));
    }
    response
}

async fn list_invites(shared: &Shared, endpoint: &str, uri: &Uri) -> Response {
    let after = query_value(uri, "after").and_then(|value| value.parse::<u64>().ok());
    let wait = query_value(uri, "wait")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
        .min(MAX_WAIT_SECONDS);
    let notified = shared.changed.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    {
        let state = shared.state.lock().expect("test hub state must lock");
        if !state.accounts.contains_key(endpoint) {
            return error(StatusCode::FORBIDDEN, "no_account");
        }
        if after != state.versions.get(endpoint).copied() || wait == 0 {
            return ok(StatusCode::OK, state.snapshot(endpoint));
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(wait), notified).await;
    let state = shared.state.lock().expect("test hub state must lock");
    ok(StatusCode::OK, state.snapshot(endpoint))
}

fn route(
    state: &mut HubState,
    endpoint: &str,
    method: &Method,
    segments: &[&str],
    body: &Value,
    query: &BTreeMap<String, String>,
) -> Response {
    match (method.as_str(), segments) {
        ("GET", ["v1", "account"]) => {
            if state.accounts.contains_key(endpoint) {
                ok(StatusCode::OK, state.account_json(endpoint))
            } else {
                error(StatusCode::NOT_FOUND, "not_found")
            }
        }
        ("GET", ["v1", "hub"]) if state.old => {
            error(StatusCode::NOT_FOUND, "not_found")
        }
        ("GET", ["v1", "hub"]) => ok(StatusCode::OK, json!({ "protocol": 2 })),
        ("PUT", ["v1", "account"]) if state.old => {
            error(StatusCode::BAD_REQUEST, "invalid")
        }
        ("PUT", ["v1", "account"]) => {
            let existing = state.accounts.contains_key(endpoint);
            let kind = match body.get("kind") {
                None => None,
                Some(value) => {
                    let Some(parsed) = value.as_str().and_then(AgentKind::parse) else {
                        return error(StatusCode::BAD_REQUEST, "invalid");
                    };
                    Some(parsed)
                }
            };
            if kind.is_some_and(|kind| existing && kind != state.kind(endpoint)) {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            let kind = kind.unwrap_or_else(|| state.kind(endpoint));
            if let Some(searchable) = body["searchable"].as_bool() {
                if searchable && kind == AgentKind::Service {
                    return error(StatusCode::BAD_REQUEST, "invalid");
                }
                if searchable {
                    state.searchable.insert(endpoint.to_owned());
                } else {
                    state.searchable.remove(endpoint);
                }
            }
            state.kinds.insert(endpoint.to_owned(), kind);
            let Some(name) = body["name"].as_str() else {
                if existing {
                    return ok(StatusCode::OK, state.account_json(endpoint));
                }
                return error(StatusCode::BAD_REQUEST, "invalid");
            };
            // The hub adds the number: the next one for this name.
            let handle = match state.accounts.get(endpoint) {
                Some(handle)
                    if handle.split_once('#').map(|(part, _)| part) == Some(name) =>
                {
                    handle.clone()
                }
                _ => {
                    let taken = state
                        .accounts
                        .values()
                        .filter(|handle| {
                            handle.split_once('#').map(|(part, _)| part) == Some(name)
                        })
                        .count();
                    format!("{name}#{:04}", taken.saturating_add(1))
                }
            };
            state.add_account(endpoint, &handle);
            ok(StatusCode::OK, state.account_json(endpoint))
        }
        _ if !state.accounts.contains_key(endpoint) => {
            error(StatusCode::FORBIDDEN, "no_account")
        }
        ("POST", ["v1", "invites"]) => {
            let (Some(to), Some(days)) = (body["to"].as_str(), body["days"].as_u64())
            else {
                return error(StatusCode::BAD_REQUEST, "invalid");
            };
            let Some(receiver) = state
                .accounts
                .iter()
                .find(|(_, name)| name.as_str() == to)
                .map(|(receiver, _)| receiver.clone())
            else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            if receiver == endpoint {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            state.invites.retain(|invite| {
                !(invite.sender == endpoint && invite.receiver == receiver)
            });
            let hidden =
                state.blocks.contains(&(receiver.clone(), endpoint.to_owned()));
            let id = state.add_invite(endpoint, &receiver, "pending");
            if let Some(invite) =
                state.invites.iter_mut().find(|invite| invite.id == id)
            {
                invite.days = days;
                invite.hidden_from = hidden.then(|| receiver.clone());
            }
            let invite = state
                .invites
                .iter()
                .find(|invite| invite.id == id)
                .cloned()
                .expect("the new invite must exist");
            ok(StatusCode::CREATED, state.invite_json(&invite, endpoint))
        }
        ("POST", ["v1", "requests"]) => {
            let (Some(to), Some(days)) = (body["to"].as_str(), body["days"].as_u64())
            else {
                return error(StatusCode::BAD_REQUEST, "invalid");
            };
            let Some(service) = state
                .accounts
                .iter()
                .find(|(service, name)| {
                    name.as_str() == to && state.listings.contains_key(*service)
                })
                .map(|(service, _)| service.clone())
            else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            // The service is the sender of a request row.
            if let Some(open) = state
                .invites
                .iter()
                .find(|invite| invite.sender == service && invite.receiver == endpoint)
                .cloned()
            {
                return ok(StatusCode::CREATED, state.invite_json(&open, endpoint));
            }
            let open = state
                .invites
                .iter()
                .filter(|invite| {
                    invite.receiver == endpoint && invite.origin == "request"
                })
                .count();
            if open
                >= usize::try_from(protocol::MAX_OPEN_REQUESTS)
                    .expect("max open requests")
            {
                return error(StatusCode::CONFLICT, "limit");
            }
            let id = state.add_request(&service, endpoint, days);
            let invite = state
                .invites
                .iter()
                .find(|invite| invite.id == id)
                .cloned()
                .expect("the new request must exist");
            ok(StatusCode::CREATED, state.invite_json(&invite, endpoint))
        }
        ("POST", ["v1", "invites", id, "block"]) => {
            // The receiver of an invite blocks its sender; the service of a
            // request blocks the node that asked.
            let Some(other) = state
                .invites
                .iter()
                .find(|invite| invite.id == *id && invite.hidden_from.is_none())
                .and_then(|invite| match invite.origin.as_str() {
                    "request" if invite.sender == endpoint => {
                        Some(invite.receiver.clone())
                    }
                    "invite" if invite.receiver == endpoint => {
                        Some(invite.sender.clone())
                    }
                    _ => None,
                })
            else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            if !state.block(endpoint, &other) {
                return error(StatusCode::CONFLICT, "limit");
            }
            StatusCode::NO_CONTENT.into_response()
        }
        ("PUT", ["v1", "blocks", blocked]) => {
            if blocked.parse::<iroh::EndpointId>().is_err() || *blocked == endpoint {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            if !state.block(endpoint, blocked) {
                return error(StatusCode::CONFLICT, "limit");
            }
            StatusCode::NO_CONTENT.into_response()
        }
        ("POST", ["v1", "invites", id, "approve"]) => {
            let Some(index) = state.invites.iter().position(|invite| {
                invite.id == *id
                    && (invite.sender == endpoint || invite.receiver == endpoint)
            }) else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            let mut invite = state.invites[index].clone();
            if invite.sender != endpoint || invite.status != "requested" {
                return error(StatusCode::FORBIDDEN, "forbidden");
            }
            match body.get("days").map(Value::as_u64) {
                None => {}
                Some(Some(days))
                    if (crate::hub_client::protocol::ACCESS_DAYS_MIN
                        ..=crate::hub_client::protocol::ACCESS_DAYS_MAX)
                        .contains(&days) =>
                {
                    invite.days = days;
                }
                Some(_) => return error(StatusCode::BAD_REQUEST, "invalid"),
            }
            invite.status = "accepted".to_owned();
            state.invites[index] = invite.clone();
            state.bump(&[invite.sender.as_str(), invite.receiver.as_str()]);
            ok(StatusCode::OK, state.invite_json(&invite, endpoint))
        }
        ("GET", ["v1", "blocks"]) => {
            let blocks: Vec<Value> = state
                .blocks
                .iter()
                .filter(|(owner, _)| owner == endpoint)
                .map(|(_, blocked)| {
                    json!({ "endpoint": blocked, "handle": state.accounts.get(blocked) })
                })
                .collect();
            ok(StatusCode::OK, json!({ "blocks": blocks }))
        }
        ("DELETE", ["v1", "blocks", blocked]) => {
            if blocked.parse::<iroh::EndpointId>().is_err() {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            if state.blocks.remove(&(endpoint.to_owned(), (*blocked).to_owned())) {
                StatusCode::NO_CONTENT.into_response()
            } else {
                error(StatusCode::NOT_FOUND, "not_found")
            }
        }
        ("POST", ["v1", "invites", id, action]) => {
            let Some(index) = state.invites.iter().position(|invite| {
                invite.id == *id
                    && (invite.sender == endpoint || invite.receiver == endpoint)
            }) else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            let mut invite = state.invites[index].clone();
            let sender = invite.sender == endpoint;
            match *action {
                "accept" if !sender && invite.status == "pending" => {
                    invite.status = "accepted".to_owned();
                }
                "bundle" if sender && invite.status == "accepted" => {
                    let Some(bundle) = body["bundle"].as_str() else {
                        return error(StatusCode::BAD_REQUEST, "invalid");
                    };
                    invite.status = "delivered".to_owned();
                    invite.bundle = Some(bundle.to_owned());
                    // After a request, only the requester gets a contact.
                    if invite.origin != "request" {
                        state
                            .contacts
                            .insert((invite.sender.clone(), invite.receiver.clone()));
                    }
                    state
                        .contacts
                        .insert((invite.receiver.clone(), invite.sender.clone()));
                }
                "accept" | "bundle" => {
                    return error(StatusCode::FORBIDDEN, "forbidden");
                }
                _ => return error(StatusCode::NOT_FOUND, "not_found"),
            }
            state.invites[index] = invite.clone();
            state.bump(&[invite.sender.as_str(), invite.receiver.as_str()]);
            ok(StatusCode::OK, state.invite_json(&invite, endpoint))
        }
        ("DELETE", ["v1", "invites", id]) => {
            let Some(index) = state.invites.iter().position(|invite| {
                invite.id == *id
                    && (invite.sender == endpoint || invite.receiver == endpoint)
            }) else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            let invite = state.invites.remove(index);
            state.bump(&[invite.sender.as_str(), invite.receiver.as_str()]);
            StatusCode::NO_CONTENT.into_response()
        }
        ("GET", ["v1", "contacts"]) => {
            let contacts: Vec<Value> = state
                .contacts
                .iter()
                .filter(|(owner, _)| owner == endpoint)
                .map(|(_, contact)| {
                    json!({
                        "handle": state.handle(contact),
                        "kind": state.kind(contact).as_str(),
                        "endpoint": contact,
                        "online": state.online.contains(contact),
                        "lastSeen": null,
                    })
                })
                .collect();
            ok(StatusCode::OK, json!({ "contacts": contacts }))
        }
        ("PUT", ["v1", "listing"]) => {
            let handle = state.handle(endpoint);
            let valid = state.kind(endpoint) == AgentKind::Service
                && body.as_object().is_some_and(|object| {
                    object.len() == 4 && object.get("listed") == Some(&json!(true))
                })
                && match (
                    body["listing"].as_str(),
                    body["signature"].as_str(),
                    body["expiresAt"].as_u64(),
                ) {
                    (Some(text), Some(signature), Some(expires_at)) => {
                        protocol::verify_listing(
                            text,
                            signature,
                            endpoint,
                            expires_at,
                            now(),
                        )
                        .is_ok()
                            && protocol::listing_bytes_match_handle(text, &handle)
                                .is_ok()
                    }
                    _ => false,
                };
            if !valid {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            state.listings.insert(endpoint.to_owned(), body.clone());
            ok(StatusCode::OK, body.clone())
        }
        ("GET", ["v1", "listing"]) => match state.listings.get(endpoint) {
            Some(listing) => ok(StatusCode::OK, listing.clone()),
            None => error(StatusCode::NOT_FOUND, "not_found"),
        },
        ("DELETE", ["v1", "listing"]) => {
            state.listings.remove(endpoint);
            StatusCode::NO_CONTENT.into_response()
        }
        ("GET", ["v1", "agents"]) => {
            let words: Vec<String> = query
                .get("q")
                .map(|q| {
                    q.split(|character: char| !character.is_alphanumeric())
                        .filter(|word| word.chars().count() >= 2)
                        .map(str::to_lowercase)
                        .collect()
                })
                .unwrap_or_default();
            if words.is_empty() {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            let agents: Vec<Value> = state
                .listings
                .iter()
                .filter(|(_, held)| {
                    let text = held["listing"]
                        .as_str()
                        .and_then(|text| URL_SAFE_NO_PAD.decode(text).ok())
                        .map(|bytes| String::from_utf8_lossy(&bytes).to_lowercase())
                        .unwrap_or_default();
                    let text_words: Vec<&str> = text
                        .split(|character: char| !character.is_alphanumeric())
                        .collect();
                    words.iter().all(|word| {
                        text_words
                            .iter()
                            .any(|text_word| text_word.starts_with(word.as_str()))
                    })
                })
                .map(|(service, held)| state.agent_json(endpoint, service, held))
                .collect();
            ok(StatusCode::OK, json!({ "agents": agents }))
        }
        ("GET", ["v1", "agents", name, number]) => {
            let handle = format!("{name}#{number}");
            match state
                .accounts
                .iter()
                .find(|(_, held)| **held == handle)
                .and_then(|(service, _)| Some((service, state.listings.get(service)?)))
            {
                Some((service, held)) => {
                    ok(StatusCode::OK, state.agent_json(endpoint, service, held))
                }
                None => error(StatusCode::NOT_FOUND, "not_found"),
            }
        }
        ("GET", ["v1", "people", name, number]) => {
            let handle = format!("{name}#{number}");
            match state.accounts.iter().find(|(person, held)| {
                **held == handle && state.kind(person) == AgentKind::Personal
            }) {
                Some((person, _)) => {
                    ok(StatusCode::OK, state.person_json(endpoint, person))
                }
                None => error(StatusCode::NOT_FOUND, "not_found"),
            }
        }
        ("GET", ["v1", "people"]) => {
            let Some(prefix) = query.get("q") else {
                return error(StatusCode::BAD_REQUEST, "invalid");
            };
            let people: Vec<Value> = state
                .accounts
                .iter()
                .filter(|(person, handle)| {
                    state.searchable.contains(*person)
                        && handle.starts_with(prefix.as_str())
                })
                .map(|(person, _)| state.person_json(endpoint, person))
                .collect();
            ok(StatusCode::OK, json!({ "people": people }))
        }
        ("POST", ["v1", "owner-links"]) => {
            if state.kind(endpoint) != AgentKind::Personal {
                return error(StatusCode::FORBIDDEN, "forbidden");
            }
            let (Some(service_handle), Some(expires_at), Some(signature)) = (
                body["service"].as_str(),
                body["expiresAt"].as_u64(),
                body["signature"].as_str(),
            ) else {
                return error(StatusCode::BAD_REQUEST, "invalid");
            };
            let Some(service) = state
                .accounts
                .iter()
                .find(|(service, handle)| {
                    handle.as_str() == service_handle
                        && state.kind(service) == AgentKind::Service
                })
                .map(|(service, _)| service.clone())
            else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            let message =
                protocol::owner_claim_message(endpoint, service_handle, expires_at);
            if !signed(endpoint, &message, signature) {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            state
                .owner_links
                .retain(|link| !(link.service == service && link.personal == endpoint));
            state.next_id = state.next_id.saturating_add(1);
            let link = TestOwnerLink {
                id: format!("{:032x}", state.next_id),
                service,
                personal: endpoint.to_owned(),
                expires_at,
                confirmed: false,
            };
            state.owner_links.push(link.clone());
            ok(StatusCode::CREATED, state.owner_link_json(&link, endpoint))
        }
        ("GET", ["v1", "owner-links"]) => {
            let links: Vec<Value> = state
                .owner_links
                .iter()
                .filter(|link| link.personal == endpoint || link.service == endpoint)
                .map(|link| state.owner_link_json(link, endpoint))
                .collect();
            ok(StatusCode::OK, json!({ "links": links }))
        }
        ("POST", ["v1", "owner-links", id, "confirm"]) => {
            let Some(index) = state
                .owner_links
                .iter()
                .position(|link| link.id == *id && link.service == endpoint)
            else {
                return error(StatusCode::NOT_FOUND, "not_found");
            };
            let link = state.owner_links[index].clone();
            if link.confirmed {
                return error(StatusCode::FORBIDDEN, "forbidden");
            }
            let message = protocol::owner_confirm_message(
                endpoint,
                &link.personal,
                link.expires_at,
            );
            if !body["signature"]
                .as_str()
                .is_some_and(|signature| signed(endpoint, &message, signature))
            {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            if state.owner_links.iter().any(|other| {
                other.service == endpoint && other.id != link.id && other.confirmed
            }) {
                return error(StatusCode::CONFLICT, "limit");
            }
            state.owner_links[index].confirmed = true;
            let link = state.owner_links[index].clone();
            ok(StatusCode::OK, state.owner_link_json(&link, endpoint))
        }
        ("DELETE", ["v1", "owner-links", id]) => {
            let before = state.owner_links.len();
            state.owner_links.retain(|link| {
                !(link.id == *id
                    && (link.service == endpoint || link.personal == endpoint))
            });
            if state.owner_links.len() == before {
                return error(StatusCode::NOT_FOUND, "not_found");
            }
            StatusCode::NO_CONTENT.into_response()
        }
        ("DELETE", ["v1", "contacts", contact]) => {
            if contact.parse::<iroh::EndpointId>().is_err() {
                return error(StatusCode::BAD_REQUEST, "invalid");
            }
            let pair = (endpoint.to_owned(), (*contact).to_owned());
            if !state.contacts.remove(&pair) {
                return error(StatusCode::NOT_FOUND, "not_found");
            }
            state.contacts.remove(&((*contact).to_owned(), endpoint.to_owned()));
            StatusCode::NO_CONTENT.into_response()
        }
        _ => error(StatusCode::NOT_FOUND, "not_found"),
    }
}
