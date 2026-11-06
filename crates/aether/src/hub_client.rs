//! The client of the Aether hub (`server/`), which matches two people by
//! name so that neither copies a code. `server/PROTOCOL.md` is the contract.
//!
//! The node's endpoint key is its hub account: every request is signed with
//! it, and the hub knows the node by its endpoint ID.

use std::time::{Duration, SystemTime};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Method, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
pub mod full_name;
pub mod node_hub;
pub mod protocol;
mod types;

pub use self::full_name::{
    FullName, split_full_name, valid_full_name, valid_name_part,
};
pub use self::node_hub::{NodeHub, hub_client as configured_hub_client};
pub use self::protocol::{
    DEFAULT_HUB_URL, HUB_OFF, INVITE_SECONDS, MAX_WAIT_SECONDS, PROTOCOL_HEADER,
    PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
pub use self::types::{
    Account, AgentKind, AgentRow, Block, Contact, HeldListing, HubError, Invite,
    Invites, Origin, OwnerLink, PersonRow, Role, Status,
};

/// The hub a node uses: `flag` (`--hub-url` or `AETHER_HUB_URL`), else the
/// one set on the page, else [`DEFAULT_HUB_URL`]. `None` when the chosen
/// value is [`HUB_OFF`] or empty.
#[must_use]
pub fn resolve_url(flag: Option<&str>, stored: Option<String>) -> Option<String> {
    let url = flag
        .map(str::to_owned)
        .or(stored)
        .unwrap_or_else(|| protocol::DEFAULT_HUB_URL.to_owned());
    let url = url.trim();
    (!url.is_empty() && url != protocol::HUB_OFF)
        .then(|| url.trim_end_matches('/').to_owned())
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
        return Ok(Some(protocol::HUB_OFF.to_owned()));
    }
    parse_origin(url)?;
    Ok((url != protocol::DEFAULT_HUB_URL).then(|| url.to_owned()))
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
    crate::settings::store_setting::<crate::hub_client::settings::Hub>(data_dir, stored)
        .and_then(|()| {
            crate::settings::store_setting::<crate::hub_client::settings::HubName>(
                data_dir, &None,
            )
        })
}

use types::{
    AgentRows, ApproveBody, Blocks, BundleBody, ClaimBody, Contacts, HubInfo,
    InviteBody, OwnerClaimBody, OwnerLinks, PersonRows, SearchBody, SignatureBody,
};

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
            .timeout(Duration::from_secs(protocol::MAX_WAIT_SECONDS + 15))
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

    pub async fn listing(&self) -> Result<Option<HeldListing>, HubError> {
        self.require_protocol_2().await?;
        match self.send(Method::GET, "/v1/listing", &[], None).await {
            Ok(body) => parse(&body).map(Some),
            Err(HubError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn delete_listing(&self) -> Result<(), HubError> {
        self.require_protocol_2().await?;
        self.send(Method::DELETE, "/v1/listing", &[], None).await.map(drop)
    }

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

    pub async fn agent(&self, full_name: &str) -> Result<AgentRow, HubError> {
        let path = name_path("/v1/agents", full_name)?;
        self.require_protocol_2().await?;
        parse(&self.send(Method::GET, &path, &[], None).await?)
    }

    pub async fn person(&self, full_name: &str) -> Result<PersonRow, HubError> {
        let path = name_path("/v1/people", full_name)?;
        self.require_protocol_2().await?;
        parse(&self.send(Method::GET, &path, &[], None).await?)
    }

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
        let wait = wait.as_secs().min(protocol::MAX_WAIT_SECONDS).to_string();
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

    pub async fn blocks(&self) -> Result<Vec<Block>, HubError> {
        self.require_protocol_2().await?;
        let body = self.send(Method::GET, "/v1/blocks", &[], None).await?;
        parse::<Blocks>(&body).map(|blocks| blocks.blocks)
    }

    pub async fn unblock(&self, endpoint: &str) -> Result<(), HubError> {
        let endpoint =
            endpoint.parse::<iroh::EndpointId>().map_err(|_| HubError::Invalid)?;
        self.require_protocol_2().await?;
        let path = format!("/v1/blocks/{endpoint}");
        self.send(Method::DELETE, &path, &[], None).await.map(drop)
    }

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
            protocol::hub_request_message(
                method.as_str(),
                &signed_target(&url),
                &time,
                &body,
            )
            .as_bytes(),
        );
        let mut request = self
            .client
            .request(method, url)
            .header(protocol::PROTOCOL_HEADER, protocol::PROTOCOL_VERSION_HEADER)
            .header(protocol::ENDPOINT_HEADER, self.endpoint_id())
            .header(protocol::TIME_HEADER, time)
            .header(
                protocol::SIGNATURE_HEADER,
                URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            );
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

fn status_error(status: StatusCode, body: &[u8]) -> HubError {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let code = serde_json::from_slice::<ErrorBody>(body).map(|body| body.error).ok();
    match protocol::wire_error(status.as_u16(), code.as_deref()) {
        protocol::HubWireError::Invalid => HubError::Invalid,
        protocol::HubWireError::Unauthorized => HubError::Unauthorized,
        protocol::HubWireError::NoAccount => HubError::NoAccount,
        protocol::HubWireError::Forbidden => HubError::Forbidden,
        protocol::HubWireError::NotFound => HubError::NotFound,
        protocol::HubWireError::Taken => HubError::Taken,
        protocol::HubWireError::Limit => HubError::Limit,
        protocol::HubWireError::RateLimited => HubError::RateLimited,
        protocol::HubWireError::OldNode => HubError::OldNode,
        protocol::HubWireError::Unexpected => HubError::Unexpected,
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

#[cfg(test)]
pub(crate) mod test_hub;

pub mod settings;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::StatusCode;

    use super::{
        DEFAULT_HUB_URL, HubError, Invites, NodeHub, Origin, PROTOCOL_HEADER,
        PROTOCOL_VERSION_HEADER, Status, parse_origin, protocol, request_url,
        resolve_url, signed_target, status_error, test_hub::TestHub,
    };

    #[test]
    fn the_node_hub_cache_follows_a_changed_url() {
        use crate::hub_client::settings::Hub;
        use crate::settings::store_setting;

        let directory = tempfile::tempdir().expect("directory must create");
        let hub = NodeHub::new(directory.path().to_path_buf(), None);
        for url in ["https://one.example", "https://two.example"] {
            store_setting::<Hub>(directory.path(), &Some(url.to_owned()))
                .expect("store");
            assert_eq!(hub.client().expect("hub").expect("url").url(), url);
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
                Some(PROTOCOL_VERSION_HEADER),
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
            protocol::hub_request_message(
                "GET",
                "/v1/invites?after=3&wait=25",
                "1760000000",
                b"",
            ),
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
    }
}
