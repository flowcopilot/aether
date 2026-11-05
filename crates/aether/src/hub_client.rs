//! The client of the Aether hub (`server/`), which matches two people by
//! name so that neither copies a code. `server/PROTOCOL.md` is the contract.
//!
//! The node's endpoint key is its hub account: every request is signed with
//! it, and the hub knows the node by its endpoint ID.

use std::time::{Duration, SystemTime};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Method, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::node_config::settings;

/// The public hub that a node uses when nothing else is set.
pub const DEFAULT_HUB_URL: &str = "https://aether-hub.tudorandrei.xyz";

/// The setting value that turns the hub off.
pub const HUB_OFF: &str = "off";

/// The hub a node uses: `flag` (`--hub-url` or `AETHER_HUB_URL`), else the
/// one set on the page, else [`DEFAULT_HUB_URL`]. `None` when the chosen
/// value is [`HUB_OFF`] or empty.
#[must_use]
pub fn resolve_url(flag: Option<&str>, stored: Option<String>) -> Option<String> {
    let url = flag
        .map(str::to_owned)
        .or(stored)
        .unwrap_or_else(|| DEFAULT_HUB_URL.to_owned());
    let url = url.trim();
    (!url.is_empty() && url != HUB_OFF).then(|| url.trim_end_matches('/').to_owned())
}

/// The hub setting for a hub URL an owner typed: [`HUB_OFF`] when it is
/// empty, no setting for the public hub, so that it follows the default,
/// else the URL.
///
/// # Errors
///
/// [`HubError::InvalidUrl`] when `url` is not a hub origin.
pub fn hub_setting(url: &str) -> Result<Option<String>, HubError> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        return Ok(Some(HUB_OFF.to_owned()));
    }
    parse_origin(url)?;
    Ok((url != DEFAULT_HUB_URL).then(|| url.to_owned()))
}

/// Keeps `stored` as the hub of the node at `data_dir`. A name belongs to
/// one hub, so the node forgets its name: the next hub gives its own.
///
/// # Errors
///
/// A [`StoreError`](aether_store::StoreError) when the store fails.
pub fn set_hub_url(
    data_dir: &std::path::Path,
    stored: &Option<String>,
) -> Result<(), aether_store::StoreError> {
    crate::node_config::store_setting::<settings::Hub>(data_dir, stored).and_then(
        |()| crate::node_config::store_setting::<settings::HubName>(data_dir, &None),
    )
}

/// The longest long poll the hub accepts, in seconds.
pub const MAX_WAIT_SECONDS: u64 = 30;

/// The header that names the hub protocol of this node. It is not in the
/// signed message: it only lets a hub refuse a node that is too old.
pub const PROTOCOL_HEADER: &str = "x-aether-protocol";
/// The hub protocol that this node speaks.
pub const PROTOCOL_VERSION: &str = "2";

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum HubError {
    #[error("hub URL must be an HTTPS origin, or HTTP on a loopback address")]
    InvalidUrl,
    #[error("the hub cannot be reached")]
    Unreachable,
    #[error("the hub refused the signature")]
    Unauthorized,
    #[error("this node has no name on the hub")]
    NoAccount,
    #[error("the hub refused the request")]
    Forbidden,
    #[error("the hub does not know this name or invite")]
    NotFound,
    #[error("this name belongs to someone else")]
    Taken,
    #[error("the hub refused the request as invalid")]
    Invalid,
    #[error("too many requests to the hub")]
    RateLimited,
    #[error("the hub returned an unexpected answer")]
    Unexpected,
    #[error("this hub is older than this node")]
    OldHub,
    #[error("this node is older than the hub")]
    OldNode,
    #[error("a hub limit is reached")]
    Limit,
}

/// The type of the agent of a hub account. It does not change.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// Stands for one person; friends that the person invites send it tasks.
    #[default]
    Personal,
    /// Offers a service; strangers ask for access, and the owner approves.
    Service,
}

impl AgentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Service => "service",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "personal" => Some(Self::Personal),
            "service" => Some(Self::Service),
            _ => None,
        }
    }
}

/// This node's name on the hub.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Account {
    /// The full name, such as `ana#0042`.
    pub handle: String,
    pub endpoint: String,
    /// A protocol 2 hub always sends it; the default only lets a reply parse.
    #[serde(default)]
    pub kind: AgentKind,
    #[serde(default)]
    pub searchable: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    /// A request that waits for the service agent's owner.
    Requested,
    Accepted,
    Delivered,
    /// A status from a newer hub. A page does not show it, and no worker
    /// acts on it.
    #[serde(other)]
    Unknown,
}

/// How a hub row started.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// A host offered access to a name.
    #[default]
    Invite,
    /// A node asked a listed service agent for access. The service is the
    /// sender, because it signs the line.
    Request,
    /// An origin from a newer hub. No worker acts on it.
    #[serde(other)]
    Unknown,
}

/// One open invite that this node sent or received. `from` and `to` are
/// full names, such as `ana#0042`.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub id: String,
    pub from: String,
    pub to: String,
    pub role: Role,
    pub days: u64,
    pub status: Status,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub receiver_endpoint: Option<String>,
    #[serde(default)]
    pub bundle: Option<String>,
    /// The endpoint that sent the line, for the receiver of a delivered
    /// invite only.
    #[serde(default)]
    pub sender_endpoint: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Invites {
    pub version: u64,
    pub invites: Vec<Invite>,
}

/// A node this node is connected with, and whether it is online.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// The full name, such as `ana#0042`.
    pub handle: String,
    pub endpoint: String,
    pub online: bool,
}

#[derive(Deserialize)]
struct Contacts {
    contacts: Vec<Contact>,
}

/// An endpoint that this node blocked, with its current full name, or
/// `None` when it has no account now.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Block {
    pub endpoint: String,
    pub handle: Option<String>,
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Vec<Block>,
}

#[derive(Serialize)]
struct ClaimBody<'a> {
    name: &'a str,
    /// Only on the first claim: the hub sets the type once.
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<AgentKind>,
}

#[derive(Serialize)]
struct SearchBody {
    searchable: bool,
}

/// The public listing of this service agent, as the hub keeps it.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldListing {
    pub listed: bool,
    /// The base64url text of the listing bytes, exactly as the node sent it.
    pub listing: String,
    /// The unpadded base64url signature of the endpoint key.
    pub signature: String,
    /// Unix time.
    pub expires_at: u64,
}

/// One listed service agent from a search or a lookup.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    /// The full name, such as `law#0042`.
    pub handle: String,
    /// The base64url listing text, exactly as the service node sent it.
    pub listing: String,
    pub signature: String,
    pub expires_at: u64,
    pub online: bool,
    #[serde(default)]
    pub contact: bool,
    /// Only for a contact of this node.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// The full name of the owner's personal agent, when the two linked.
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Deserialize)]
struct AgentRows {
    agents: Vec<AgentRow>,
}

/// One personal agent from a lookup or a name search.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct PersonRow {
    /// The full name, such as `ana#0042`.
    pub handle: String,
    #[serde(default)]
    pub contact: bool,
    /// Only for a contact of this node.
    #[serde(default)]
    pub online: Option<bool>,
    /// Only for a contact of this node.
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Deserialize)]
struct PersonRows {
    people: Vec<PersonRow>,
}

/// An owner link or claim between a service agent and the personal agent of
/// its owner, as one side sees it.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerLink {
    pub id: String,
    /// `personal` or `service`: the side of this node.
    pub role: String,
    /// The full name of the service agent.
    pub service: String,
    /// The full name of the personal agent.
    pub personal: String,
    pub expires_at: u64,
    pub confirmed: bool,
    /// For the service side only.
    #[serde(default)]
    pub personal_endpoint: Option<String>,
    /// For the personal side, once the link is active.
    #[serde(default)]
    pub service_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct OwnerLinks {
    links: Vec<OwnerLink>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnerClaimBody<'a> {
    service: &'a str,
    expires_at: u64,
    signature: &'a str,
}

#[derive(Serialize)]
struct SignatureBody<'a> {
    signature: &'a str,
}

#[derive(Deserialize)]
struct HubInfo {
    protocol: u64,
}

#[derive(Serialize)]
struct InviteBody<'a> {
    to: &'a str,
    days: u64,
}

#[derive(Serialize)]
struct ApproveBody {
    days: u64,
}

#[derive(Serialize)]
struct BundleBody<'a> {
    bundle: &'a str,
}

/// One hub client, kept while the resolved hub URL stays the same, so that
/// each call does not build a new HTTP client.
#[derive(Clone, Default)]
pub struct HubCache(std::sync::Arc<std::sync::Mutex<Option<(String, HubClient)>>>);

impl HubCache {
    /// The client for the hub at `url` that signs with the key in
    /// `data_dir`. A new URL builds a new client.
    pub fn client(
        &self,
        url: &str,
        data_dir: &std::path::Path,
    ) -> Result<HubClient, HubError> {
        let cached = self
            .slot()
            .as_ref()
            .filter(|(cached, _)| cached == url)
            .map(|(_, client)| client.clone());
        if let Some(client) = cached {
            return Ok(client);
        }
        let client = HubClient::for_data_dir(url, data_dir)?;
        *self.slot() = Some((url.to_owned(), client.clone()));
        Ok(client)
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<(String, HubClient)>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// [`HubClient::configured`], from the cache.
    #[must_use]
    pub fn configured(
        &self,
        flag: Option<&str>,
        data_dir: &std::path::Path,
    ) -> Option<HubClient> {
        let url = resolve_url(
            flag,
            crate::node_config::setting::<settings::Hub>(data_dir).ok().flatten(),
        )?;
        self.client(&url, data_dir).ok()
    }
}

#[derive(Clone)]
pub struct HubClient {
    client: Client,
    origin: Url,
    key: iroh::SecretKey,
    /// The protocol of the hub, once it answered 2 or more. A client is for
    /// one hub URL, so the answer stays true.
    protocol: std::sync::Arc<std::sync::OnceLock<u64>>,
}

impl HubClient {
    /// A client for the hub at `origin` that signs with `key`.
    pub fn new(origin: &str, key: iroh::SecretKey) -> Result<Self, HubError> {
        let origin = parse_origin(origin)?;
        // The long poll holds a request open for up to MAX_WAIT_SECONDS.
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(MAX_WAIT_SECONDS + 15))
            .build()
            .map_err(|_| HubError::InvalidUrl)?;
        Ok(Self { client, origin, key, protocol: std::sync::Arc::default() })
    }

    /// A client that signs with the endpoint key in `data_dir`, the key the
    /// node uses on the network.
    pub fn for_data_dir(
        origin: &str,
        data_dir: &std::path::Path,
    ) -> Result<Self, HubError> {
        let key = crate::data_dir::DataDir::new(data_dir)
            .endpoint_key()
            .map_err(|_| HubError::Unexpected)?;
        Self::new(origin, key)
    }

    /// The hub that a node at `data_dir` uses: `flag` (`--hub-url` or
    /// `AETHER_HUB_URL`), else the one set on the page, else the public hub.
    /// `None` when the hub is off.
    #[must_use]
    pub fn configured(flag: Option<&str>, data_dir: &std::path::Path) -> Option<Self> {
        let url = resolve_url(
            flag,
            crate::node_config::setting::<settings::Hub>(data_dir).ok().flatten(),
        )?;
        Self::for_data_dir(&url, data_dir).ok()
    }

    /// Whether each contact is online, by endpoint ID. Empty when the hub
    /// does not answer within `wait`.
    pub async fn presence(
        &self,
        wait: Duration,
    ) -> std::collections::BTreeMap<String, bool> {
        tokio::time::timeout(wait, self.contacts())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(|contact| (contact.endpoint, contact.online))
            .collect()
    }

    /// The hub's origin, without the final `/`.
    #[must_use]
    pub fn url(&self) -> String {
        self.origin.as_str().trim_end_matches('/').to_owned()
    }

    /// This node's endpoint ID, which is its hub account.
    #[must_use]
    pub fn endpoint_id(&self) -> String {
        self.key.public().to_string()
    }

    /// This node's name, or `None` before it claims one.
    pub async fn account(&self) -> Result<Option<Account>, HubError> {
        match self.send(Method::GET, "/v1/account", &[], None).await {
            Ok(body) => parse(&body).map(Some),
            Err(HubError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The hub protocol: `GET /v1/hub`. A hub from before protocol 2 has no
    /// such route and answers 404, which is protocol 1.
    pub async fn protocol(&self) -> Result<u64, HubError> {
        if let Some(protocol) = self.protocol.get() {
            return Ok(*protocol);
        }
        match self.send(Method::GET, "/v1/hub", &[], None).await {
            Ok(body) => {
                let protocol = parse::<HubInfo>(&body)?.protocol;
                if protocol >= 2 {
                    let _ = self.protocol.set(protocol);
                }
                Ok(protocol)
            }
            Err(HubError::NotFound) => Ok(1),
            Err(error) => Err(error),
        }
    }

    /// [`HubError::OldHub`] unless the hub speaks protocol 2 or later.
    async fn require_protocol_2(&self) -> Result<(), HubError> {
        if self.protocol().await? >= 2 { Ok(()) } else { Err(HubError::OldHub) }
    }

    /// Claims the name `name`, or changes this node's name to it. The hub
    /// adds the number. `kind` is for the first claim only.
    pub async fn claim(
        &self,
        name: &str,
        kind: Option<AgentKind>,
    ) -> Result<Account, HubError> {
        self.require_protocol_2().await?;
        let body = json(&ClaimBody { name, kind })?;
        parse(&self.send(Method::PUT, "/v1/account", &[], Some(body)).await?)
    }

    /// Lets people find this personal agent by name search, or stops it.
    pub async fn set_searchable(&self, searchable: bool) -> Result<Account, HubError> {
        self.require_protocol_2().await?;
        let body = json(&SearchBody { searchable })?;
        parse(&self.send(Method::PUT, "/v1/account", &[], Some(body)).await?)
    }

    /// Lists this service agent on the hub until `expires_at`, with the
    /// listing `text` (base64url) and the `signature` of the endpoint key.
    pub async fn put_listing(
        &self,
        text: &str,
        signature: &str,
        expires_at: u64,
    ) -> Result<HeldListing, HubError> {
        self.require_protocol_2().await?;
        let body = json(&HeldListing {
            listed: true,
            listing: text.to_owned(),
            signature: signature.to_owned(),
            expires_at,
        })?;
        parse(&self.send(Method::PUT, "/v1/listing", &[], Some(body)).await?)
    }

    /// The listing of this node on the hub, or `None` when it has none.
    pub async fn listing(&self) -> Result<Option<HeldListing>, HubError> {
        self.require_protocol_2().await?;
        match self.send(Method::GET, "/v1/listing", &[], None).await {
            Ok(body) => parse(&body).map(Some),
            Err(HubError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Stops the listing of this node.
    pub async fn delete_listing(&self) -> Result<(), HubError> {
        self.require_protocol_2().await?;
        self.send(Method::DELETE, "/v1/listing", &[], None).await.map(drop)
    }

    /// The listed service agents that match the words of `query`.
    pub async fn find_agents(
        &self,
        query: &str,
        limit: u8,
    ) -> Result<Vec<AgentRow>, HubError> {
        self.require_protocol_2().await?;
        let limit = limit.to_string();
        let body = self
            .send(Method::GET, "/v1/agents", &[("q", query), ("limit", &limit)], None)
            .await?;
        parse::<AgentRows>(&body).map(|rows| rows.agents)
    }

    /// The listed service agent with the full name `full_name`.
    pub async fn agent(&self, full_name: &str) -> Result<AgentRow, HubError> {
        let path = name_path("/v1/agents", full_name)?;
        self.require_protocol_2().await?;
        parse(&self.send(Method::GET, &path, &[], None).await?)
    }

    /// The personal agent with the full name `full_name`.
    pub async fn person(&self, full_name: &str) -> Result<PersonRow, HubError> {
        let path = name_path("/v1/people", full_name)?;
        self.require_protocol_2().await?;
        parse(&self.send(Method::GET, &path, &[], None).await?)
    }

    /// The personal agents that opted in to name search and whose name
    /// starts with `prefix`.
    pub async fn find_people(&self, prefix: &str) -> Result<Vec<PersonRow>, HubError> {
        self.require_protocol_2().await?;
        let body = self.send(Method::GET, "/v1/people", &[("q", prefix)], None).await?;
        parse::<PersonRows>(&body).map(|rows| rows.people)
    }

    /// Offers this node's agent to the person with the full name `to` for
    /// `days` days.
    pub async fn invite(&self, to: &str, days: u64) -> Result<Invite, HubError> {
        let body = json(&InviteBody { to, days })?;
        parse(&self.send(Method::POST, "/v1/invites", &[], Some(body)).await?)
    }

    /// The open invites. With `after` equal to the hub's current version,
    /// the hub waits up to `wait` for a change before it answers.
    pub async fn invites(
        &self,
        after: Option<u64>,
        wait: Duration,
    ) -> Result<Invites, HubError> {
        let after = after.map(|version| version.to_string());
        let wait = wait.as_secs().min(MAX_WAIT_SECONDS).to_string();
        let query = match &after {
            Some(after) => vec![("after", after.as_str()), ("wait", wait.as_str())],
            None => Vec::new(),
        };
        parse(&self.send(Method::GET, "/v1/invites", &query, None).await?)
    }

    pub async fn accept(&self, id: &str) -> Result<Invite, HubError> {
        let path = format!("/v1/invites/{}/accept", segment(id)?);
        parse(&self.send(Method::POST, &path, &[], None).await?)
    }

    /// Claims the service agent `service` as this personal agent's own until
    /// `expires_at`, with the claim `signature` of the endpoint key.
    pub async fn claim_owner_link(
        &self,
        service: &str,
        expires_at: u64,
        signature: &str,
    ) -> Result<OwnerLink, HubError> {
        self.require_protocol_2().await?;
        let body = json(&OwnerClaimBody { service, expires_at, signature })?;
        parse(&self.send(Method::POST, "/v1/owner-links", &[], Some(body)).await?)
    }

    /// The owner links and claims of this node, on either side.
    pub async fn owner_links(&self) -> Result<Vec<OwnerLink>, HubError> {
        self.require_protocol_2().await?;
        let body = self.send(Method::GET, "/v1/owner-links", &[], None).await?;
        parse::<OwnerLinks>(&body).map(|links| links.links)
    }

    /// The service agent confirms the claim `id` with `signature`.
    pub async fn confirm_owner_link(
        &self,
        id: &str,
        signature: &str,
    ) -> Result<OwnerLink, HubError> {
        let path = format!("/v1/owner-links/{}/confirm", segment(id)?);
        self.require_protocol_2().await?;
        let body = json(&SignatureBody { signature })?;
        parse(&self.send(Method::POST, &path, &[], Some(body)).await?)
    }

    /// Removes the owner link or claim `id`.
    pub async fn delete_owner_link(&self, id: &str) -> Result<(), HubError> {
        let path = format!("/v1/owner-links/{}", segment(id)?);
        self.require_protocol_2().await?;
        self.send(Method::DELETE, &path, &[], None).await.map(drop)
    }

    /// Asks the listed service agent `to` for access for `days` days.
    pub async fn request_access(
        &self,
        to: &str,
        days: u64,
    ) -> Result<Invite, HubError> {
        self.require_protocol_2().await?;
        let body = json(&InviteBody { to, days })?;
        parse(&self.send(Method::POST, "/v1/requests", &[], Some(body)).await?)
    }

    /// The service agent approves the request `id` for `days` days.
    pub async fn approve(&self, id: &str, days: u64) -> Result<Invite, HubError> {
        let path = format!("/v1/invites/{}/approve", segment(id)?);
        self.require_protocol_2().await?;
        let body = json(&ApproveBody { days })?;
        parse(&self.send(Method::POST, &path, &[], Some(body)).await?)
    }

    /// Blocks `endpoint`: the hub removes the open rows and the contact
    /// between the two, and hides the rows that it makes later.
    pub async fn block(&self, endpoint: &str) -> Result<(), HubError> {
        let endpoint =
            endpoint.parse::<iroh::EndpointId>().map_err(|_| HubError::Invalid)?;
        self.require_protocol_2().await?;
        let path = format!("/v1/blocks/{endpoint}");
        self.send(Method::PUT, &path, &[], None).await.map(drop)
    }

    /// Gives the signed invite line for an accepted invite.
    pub async fn deliver(&self, id: &str, bundle: &str) -> Result<Invite, HubError> {
        let path = format!("/v1/invites/{}/bundle", segment(id)?);
        let body = json(&BundleBody { bundle })?;
        parse(&self.send(Method::POST, &path, &[], Some(body)).await?)
    }

    /// The nodes this node is connected with through the hub.
    pub async fn contacts(&self) -> Result<Vec<Contact>, HubError> {
        let body = self.send(Method::GET, "/v1/contacts", &[], None).await?;
        parse::<Contacts>(&body).map(|contacts| contacts.contacts)
    }

    /// Blocks the sender of the received invite `id`: its invites no longer
    /// show here.
    pub async fn block_invite(&self, id: &str) -> Result<(), HubError> {
        let path = format!("/v1/invites/{}/block", segment(id)?);
        self.require_protocol_2().await?;
        self.send(Method::POST, &path, &[], None).await.map(drop)
    }

    /// The endpoints that this node blocked.
    pub async fn blocks(&self) -> Result<Vec<Block>, HubError> {
        self.require_protocol_2().await?;
        let body = self.send(Method::GET, "/v1/blocks", &[], None).await?;
        parse::<Blocks>(&body).map(|blocks| blocks.blocks)
    }

    /// Removes the block of `endpoint`.
    pub async fn unblock(&self, endpoint: &str) -> Result<(), HubError> {
        let endpoint =
            endpoint.parse::<iroh::EndpointId>().map_err(|_| HubError::Invalid)?;
        self.require_protocol_2().await?;
        let path = format!("/v1/blocks/{endpoint}");
        self.send(Method::DELETE, &path, &[], None).await.map(drop)
    }

    /// Removes an invite: a decline, a cancel, or the end of a delivery.
    pub async fn remove(&self, id: &str) -> Result<(), HubError> {
        let path = format!("/v1/invites/{}", segment(id)?);
        self.send(Method::DELETE, &path, &[], None).await.map(drop)
    }

    /// Sends one signed request. `query` holds the query pairs; the node
    /// signs the URL exactly as it sends it.
    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, HubError> {
        let url = request_url(&self.origin, path, query)?;
        let body = body.unwrap_or_default();
        let time = aether_core::time::unix_seconds(SystemTime::now())
            .ok_or(HubError::Unexpected)?
            .to_string();
        let signature = self.key.sign(
            signed_message(method.as_str(), &signed_target(&url), &time, &body)
                .as_bytes(),
        );
        let mut request = self
            .client
            .request(method, url)
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
            .header("x-aether-endpoint", self.endpoint_id())
            .header("x-aether-time", time)
            .header("x-aether-signature", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        if !body.is_empty() {
            request = request.header("content-type", "application/json").body(body);
        }
        let response = request.send().await.map_err(|_| HubError::Unreachable)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| HubError::Unreachable)?;
        if status.is_success() {
            return Ok(bytes.to_vec());
        }
        Err(status_error(status, &bytes))
    }
}

/// The URL of a request: `path` on `origin`, with the encoded `query` pairs.
/// In `url`, `query_pairs_mut` adds a `?` even with no pair, so an empty
/// query adds nothing.
fn request_url(
    origin: &Url,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Url, HubError> {
    let mut url = origin.join(path).map_err(|_| HubError::Invalid)?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}

/// The path and query that the node signs, as the hub reads them: the path,
/// and `?` with the query only when the query is not empty
/// (`path_and_query/1` in `server/lib/aether_hub/verify.ex`).
fn signed_target(url: &Url) -> String {
    match url.query() {
        Some(query) if !query.is_empty() => format!("{}?{query}", url.path()),
        _ => url.path().to_owned(),
    }
}

/// The text that the hub verifies the signature against.
fn signed_message(
    method: &str,
    path_and_query: &str,
    time: &str,
    body: &[u8],
) -> String {
    format!(
        "aether-hub/v1\n{method}\n{path_and_query}\n{time}\n{}",
        hex::encode(Sha256::digest(body))
    )
}

fn status_error(status: StatusCode, body: &[u8]) -> HubError {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let code = serde_json::from_slice::<ErrorBody>(body).map(|body| body.error).ok();
    match (status, code.as_deref()) {
        (StatusCode::UNAUTHORIZED, _) => HubError::Unauthorized,
        (StatusCode::FORBIDDEN, Some("no_account")) => HubError::NoAccount,
        (StatusCode::FORBIDDEN, _) => HubError::Forbidden,
        (StatusCode::NOT_FOUND, _) => HubError::NotFound,
        (StatusCode::CONFLICT, Some("limit")) => HubError::Limit,
        (StatusCode::CONFLICT, _) => HubError::Taken,
        (StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE, _) => {
            HubError::Invalid
        }
        (StatusCode::TOO_MANY_REQUESTS, _) => HubError::RateLimited,
        (StatusCode::UPGRADE_REQUIRED, _) => HubError::OldNode,
        _ => HubError::Unexpected,
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, HubError> {
    serde_json::from_slice(body).map_err(|_| HubError::Unexpected)
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, HubError> {
    serde_json::to_vec(value).map_err(|_| HubError::Invalid)
}

/// The path `base/{name}/{number}` of the full name `full_name`: two
/// segments, because `#` starts a URL fragment.
fn name_path(base: &str, full_name: &str) -> Result<String, HubError> {
    let (name, number) = split_full_name(full_name).ok_or(HubError::Invalid)?;
    Ok(format!("{base}/{name}/{number:04}"))
}

/// An invite id as one path segment. The hub makes ids of 32 hex
/// characters, so anything else is refused rather than escaped.
fn segment(id: &str) -> Result<&str, HubError> {
    if !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        Ok(id)
    } else {
        Err(HubError::Invalid)
    }
}

fn parse_origin(origin: &str) -> Result<Url, HubError> {
    let mut url = Url::parse(origin).map_err(|_| HubError::InvalidUrl)?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(HubError::InvalidUrl);
    }
    url.set_path("/");
    Ok(url)
}

/// Whether `name` can be the name part of a hub name:
/// `^[a-z0-9][a-z0-9._-]{0,31}$`.
#[must_use]
pub fn valid_name_part(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte)
        })
}

/// The name and the number of a full hub name such as `ana#0042`. The hub
/// never gives `0000`.
#[must_use]
pub fn split_full_name(full_name: &str) -> Option<(&str, u16)> {
    let (name, number) = full_name.split_once('#')?;
    (valid_name_part(name)
        && number.len() == 4
        && number.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| number.parse::<u16>().ok())
    .flatten()
    .filter(|number| *number != 0)
    .map(|number| (name, number))
}

/// Whether `full_name` is a full hub name such as `ana#0042`.
#[must_use]
pub fn valid_full_name(full_name: &str) -> bool {
    split_full_name(full_name).is_some()
}

#[cfg(test)]
pub(crate) mod test_hub;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::StatusCode;

    use super::{
        DEFAULT_HUB_URL, HubError, Invites, Origin, PROTOCOL_HEADER, PROTOCOL_VERSION,
        Status, parse_origin, request_url, resolve_url, signed_message, signed_target,
        split_full_name, status_error, test_hub::TestHub, valid_full_name,
        valid_name_part,
    };

    #[test]
    fn the_hub_cache_follows_a_changed_url() {
        let directory = tempfile::tempdir().expect("directory must create");
        let cache = super::HubCache::default();
        for url in ["https://one.example", "https://one.example", "https://two.example"]
        {
            let client =
                cache.client(url, directory.path()).expect("client must build");
            assert_eq!(client.url(), url);
        }
    }

    #[test]
    fn a_free_text_query_is_signed_as_it_is_sent() {
        let origin = parse_origin("https://hub.example").expect("origin must parse");
        let url = request_url(&origin, "/v1/agents", &[("q", "review go#1 ü&x")])
            .expect("url must build");
        assert_eq!(
            url.as_str(),
            "https://hub.example/v1/agents?q=review+go%231+%C3%BC%26x"
        );
        assert_eq!(signed_target(&url), "/v1/agents?q=review+go%231+%C3%BC%26x");
    }

    #[tokio::test]
    async fn agent_search_sends_name_and_number_as_two_segments() {
        let hub = TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        let client = hub.client(directory.path());
        hub.state().add_account(&client.endpoint_id(), "me#0001");
        assert_eq!(client.agent("law.firm#0042").await, Err(HubError::NotFound));
        assert_eq!(client.person("ana#0007").await, Err(HubError::NotFound));
        assert_eq!(client.find_agents("contract review", 5).await, Ok(Vec::new()));
        assert_eq!(client.agent("law#42").await, Err(HubError::Invalid));
        assert_eq!(client.person("law").await, Err(HubError::Invalid));
        let targets: Vec<String> = hub
            .requests()
            .into_iter()
            .map(|request| request.target)
            .filter(|target| target != "/v1/hub")
            .collect();
        assert_eq!(
            targets,
            [
                "/v1/agents/law.firm/0042",
                "/v1/people/ana/0007",
                "/v1/agents?q=contract+review&limit=5",
            ]
        );
    }

    #[tokio::test]
    async fn a_request_without_a_query_is_signed_as_the_bare_path() {
        let origin = parse_origin("https://hub.example").expect("origin must parse");
        let url = request_url(&origin, "/v1/account", &[]).expect("url must build");
        assert_eq!(url.as_str(), "https://hub.example/v1/account");
        assert_eq!(signed_target(&url), "/v1/account");

        let hub = TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(hub.client(directory.path()).account().await, Ok(None));
        assert_eq!(hub.requests()[0].target, "/v1/account");
    }

    #[tokio::test]
    async fn the_hub_receives_the_signed_path_and_query() {
        let hub = TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        let client = hub.client(directory.path());
        client.claim("bob", None).await.expect("name must claim");
        let invites =
            client.invites(Some(7), Duration::ZERO).await.expect("invites must list");
        assert!(invites.invites.is_empty());
        let requests = hub.requests();
        // A claim asks for the protocol of the hub first.
        assert_eq!(requests[0].target, "/v1/hub");
        let claim = &requests[1];
        assert_eq!(
            (claim.method.as_str(), claim.target.as_str()),
            ("PUT", "/v1/account")
        );
        assert_eq!(claim.body, br#"{"name":"bob"}"#);
        let last = requests.last().expect("a request must arrive");
        assert_eq!(
            (last.method.as_str(), last.target.as_str()),
            ("GET", "/v1/invites?after=7&wait=0")
        );
    }

    #[tokio::test]
    async fn every_hub_request_carries_the_protocol_header() {
        let hub = TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        let client = hub.client(directory.path());
        let _ = client.account().await;
        client.claim("bob", None).await.expect("name must claim");
        let _ = client.invites(None, Duration::ZERO).await;
        let _ = client.contacts().await;
        let requests = hub.requests();
        assert_eq!(requests.len(), 5);
        for request in &requests {
            assert_eq!(
                request.headers.get(PROTOCOL_HEADER).map(String::as_str),
                Some(PROTOCOL_VERSION),
                "{}",
                request.target
            );
        }
    }

    #[test]
    fn an_unknown_invite_status_does_not_break_the_list() {
        let invites: Invites = serde_json::from_str(
            r#"{"version":3,"invites":[{"id":"a","from":"ana","to":"bob","role":"sender","days":7,"status":"waiting","origin":"offer","createdAt":1}]}"#,
        )
        .expect("the list must parse");
        assert_eq!(invites.invites[0].status, Status::Unknown);
        assert_eq!(invites.invites[0].origin, Origin::Unknown);
    }

    #[test]
    fn the_flag_beats_the_page_setting_and_the_default_comes_last() {
        let stored = || Some("https://stored.example".to_owned());
        assert_eq!(
            resolve_url(Some("https://flag.example/"), stored()).as_deref(),
            Some("https://flag.example")
        );
        assert_eq!(
            resolve_url(None, stored()).as_deref(),
            Some("https://stored.example")
        );
        assert_eq!(resolve_url(None, None).as_deref(), Some(DEFAULT_HUB_URL));
        assert_eq!(resolve_url(Some("off"), stored()), None);
        assert_eq!(resolve_url(None, Some("off".to_owned())), None);
        assert_eq!(resolve_url(Some(""), None), None);
    }

    #[test]
    fn the_signed_message_names_the_request_and_hashes_the_body() {
        assert_eq!(
            signed_message("GET", "/v1/invites?after=3&wait=25", "1760000000", b""),
            "aether-hub/v1\nGET\n/v1/invites?after=3&wait=25\n1760000000\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_hub_origin_is_https_or_loopback_http() {
        assert!(parse_origin("https://hub.example").is_ok());
        assert!(parse_origin("http://127.0.0.1:4000").is_ok());
        assert!(parse_origin("http://localhost:4000/").is_ok());
        for origin in [
            "http://hub.example",
            "https://hub.example/path",
            "https://user@hub.example",
            "https://hub.example/?q=1",
            "not a url",
        ] {
            assert_eq!(
                parse_origin(origin).err(),
                Some(HubError::InvalidUrl),
                "{origin}"
            );
        }
    }

    #[test]
    fn a_name_part_is_short_lowercase_and_starts_with_a_letter_or_digit() {
        assert!(valid_name_part("bob"));
        assert!(valid_name_part("0x.b_o-b"));
        for name in ["", "Bob", "-bob", "bob smith", "bob#0042", &"b".repeat(33)] {
            assert!(!valid_name_part(name), "{name}");
        }
    }

    #[tokio::test]
    async fn the_claim_body_names_the_kind_only_on_the_first_claim() {
        let hub = TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        let client = hub.client(directory.path());
        let account = client
            .claim("law", Some(super::AgentKind::Service))
            .await
            .expect("name must claim");
        assert_eq!(account.kind, super::AgentKind::Service);
        client.claim("law-firm", None).await.expect("rename must work");
        let claims: Vec<Vec<u8>> = hub
            .requests()
            .into_iter()
            .filter(|request| request.method == "PUT")
            .map(|request| request.body)
            .collect();
        assert_eq!(
            claims,
            [
                br#"{"name":"law","kind":"service"}"#.to_vec(),
                br#"{"name":"law-firm"}"#.to_vec()
            ]
        );
    }

    #[test]
    fn a_full_name_splits_into_name_and_number() {
        assert_eq!(split_full_name("ana#0042"), Some(("ana", 42)));
        assert_eq!(split_full_name("a.b-c#9999"), Some(("a.b-c", 9999)));
        for bad in
            ["ana", "ana#42", "ana#00042", "ana#0000", "Ana#0042", "ana#00a2", "#0042"]
        {
            assert_eq!(split_full_name(bad), None, "{bad}");
            assert!(!valid_full_name(bad));
        }
    }

    #[test]
    fn status_error_maps_limit_apart_from_taken() {
        assert_eq!(
            status_error(StatusCode::CONFLICT, br#"{"error":"limit"}"#),
            HubError::Limit
        );
        assert_eq!(
            status_error(StatusCode::CONFLICT, br#"{"error":"taken"}"#),
            HubError::Taken
        );
    }

    #[test]
    fn status_error_maps_upgrade_required_to_old_node() {
        assert_eq!(
            status_error(
                StatusCode::UPGRADE_REQUIRED,
                br#"{"error":"upgrade_required"}"#
            ),
            HubError::OldNode
        );
        assert_eq!(
            status_error(StatusCode::CONFLICT, br#"{"error":"taken"}"#),
            HubError::Taken
        );
    }
}
