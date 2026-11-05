use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Grant, GrantId, PolicyEngine,
    Scope, ScopeId, ScopeKind, TrustProvider, WorkspaceId,
};
use aether_store::{NodeStore, QueueLimits, StoreError};
use hex::FromHex;
use serde::Deserialize;

use crate::{
    agent::{
        NodeAgent,
        codex::{
            CodexAppServer, CodexLimits, MAX_ENVIRONMENT, TASK_FOLDER,
            valid_environment_key, valid_environment_value,
        },
        connected::{self, ConnectedAgent},
    },
    execution::{Executor, Limits},
    identity::{RosterClaims, SignedClaims},
    node_service::NodeService,
    proof::ProofNode,
};

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum NodeConfigError {
    #[error("node policy cannot be read")]
    Read,
    #[error("node policy is invalid")]
    Invalid,
    #[error("node policy is invalid at {0}")]
    InvalidField(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NodePolicyFile {
    scope: Option<ScopeFile>,
    scopes: Option<Vec<ScopeFile>>,
    /// Issuers besides the node's own key (`NODE_ISSUER`), which is always
    /// trusted.
    #[serde(default)]
    trusted_issuers: BTreeMap<String, String>,
    /// Grants besides the ones added from the admin UI or MCP.
    #[serde(default)]
    grants: Vec<GrantFile>,
    workspace: PathBuf,
    /// The adapter of the agent that answers this node's tasks. Absent:
    /// Codex.
    #[serde(default)]
    adapter: AgentAdapter,
    /// Codex only. The default gives each task a new empty folder. The
    /// explicit legacy value lets an operator run tasks in the configured
    /// workspace.
    sandbox: Option<PolicySandbox>,
    max_context_tokens: u32,
    max_seconds: u64,
    queue_global: usize,
    queue_per_peer: usize,
    /// Names workspace paths a live `Change::Workspace` may switch to by
    /// id. Absent or empty (the default for an existing policy file, since
    /// `deny_unknown_fields` still requires this to parse) keeps today's
    /// behavior: any path is accepted. Naming even one here switches that
    /// node to "approved IDs only" — see `load_service`'s own comment for
    /// how a change is checked against it.
    #[serde(default)]
    approved_workspaces: BTreeMap<String, PathBuf>,
    /// Codex only. Lets the spawned Codex process reach the network. Absent or false
    /// keeps today's behavior: no network access at all, since a workspace
    /// can be a real repository handed to an executor that otherwise has no
    /// reason to make outbound requests.
    #[serde(default)]
    allow_network: bool,
    /// Codex only. Names of environment variables to copy from `aether node serve`'s
    /// own environment into the spawned Codex process — an allowlist, not
    /// literal values, so a credential (e.g. an API key) never has to be
    /// written into this file. Absent or empty: the spawned process gets
    /// only the `CODEX_HOME` and `HOME` that aether always sets.
    #[serde(default)]
    environment_allowlist: Vec<String>,
    /// The PACT gateway (`plans/pact-provider/PLAN.md`). Absent: the node
    /// has no PACT scope and `--pact-port` is refused.
    #[serde(default)]
    pact: Option<crate::pact::config::PactFile>,
}

/// The adapter of a node's agent. Each data directory has one agent.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentAdapter {
    /// The node starts `codex app-server` for each task.
    #[default]
    Codex,
    /// An agent that runs on its own (the Hermes plugin, for example)
    /// attaches to `<data-dir>/agent.sock`. It runs with its owner's own
    /// setup, so the Codex sandbox fields do not apply to it.
    Connected,
}

impl AgentAdapter {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Connected => "connected",
        }
    }
}

impl std::str::FromStr for AgentAdapter {
    type Err = NodeConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "connected" => Ok(Self::Connected),
            _ => Err(NodeConfigError::InvalidField("adapter".to_owned())),
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PolicySandbox {
    TaskFolder,
    WorkspaceWrite,
}

const fn default_sandbox() -> PolicySandbox {
    PolicySandbox::TaskFolder
}

impl PolicySandbox {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TaskFolder => TASK_FOLDER,
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScopeFile {
    id: String,
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantFile {
    id: String,
    issuer_agent_id: String,
    subject_agent_id: String,
    subject_endpoint_id: String,
    scope_id: String,
    capabilities: Vec<String>,
    workspaces: Vec<String>,
    access_mode: String,
    approval_rule: String,
    issued_at: u64,
    expires_at: u64,
    #[serde(with = "hex::serde")]
    signing_key: [u8; 32],
    #[serde(with = "hex::serde")]
    nonce: [u8; 32],
    #[serde(with = "hex::serde")]
    signature: Vec<u8>,
}

/// The issuer name of the node's own signing key, which the node always
/// trusts. The admin UI signs a friend's access with it.
pub const NODE_ISSUER: &str = "node";

/// The node's own issuer key, `<data-dir>/issuer.key`, created owner-only
/// on first use.
pub fn node_issuer_key(
    data_dir: &Path,
) -> Result<ed25519_dalek::SigningKey, NodeConfigError> {
    crate::discovery::load_or_create_signing_key(data_dir.join("issuer.key"))
        .map_err(|_| NodeConfigError::InvalidField("issuer.key".to_owned()))
}

const POLICY_KEY: &str = "node.policy";

/// The policy `source` with `adapter`, and whether that changed it. The
/// Codex settings do not apply to a connected agent, so a switch to one
/// removes them; a switch back to Codex starts from the Codex defaults.
fn with_adapter(
    source: &[u8],
    adapter: AgentAdapter,
) -> Result<(Vec<u8>, bool), NodeConfigError> {
    let mut policy: serde_json::Value =
        serde_json::from_slice(source).map_err(|_| NodeConfigError::Invalid)?;
    let object = policy.as_object_mut().ok_or(NodeConfigError::Invalid)?;
    let current = match object.get("adapter") {
        None => AgentAdapter::Codex,
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| NodeConfigError::InvalidField("adapter".to_owned()))?,
    };
    if current == adapter {
        return Ok((source.to_vec(), false));
    }
    object.insert("adapter".to_owned(), serde_json::json!(adapter.as_str()));
    if adapter == AgentAdapter::Connected {
        for field in ["sandbox", "allowNetwork", "environmentAllowlist"] {
            object.remove(field);
        }
    }
    let source = serde_json::to_vec(&policy).map_err(|_| NodeConfigError::Invalid)?;
    Ok((source, true))
}

/// Settings the owner can change on the Advanced page. They take effect at
/// the next node start, when the executor and queue are built.
#[derive(Clone)]
pub struct PolicySettings {
    pub workspace: PathBuf,
    pub max_context_tokens: u32,
    pub max_seconds: u64,
    pub queue_global: usize,
    pub queue_per_peer: usize,
}

pub fn policy_settings(
    data_dir: &Path,
) -> Result<Option<PolicySettings>, NodeConfigError> {
    let store = NodeStore::in_dir(data_dir).map_err(|_| NodeConfigError::Read)?;
    let source = match store.configuration(POLICY_KEY) {
        Ok(source) => source,
        Err(StoreError::NotFound) => return Ok(None),
        Err(_) => return Err(NodeConfigError::Read),
    };
    let policy: NodePolicyFile =
        serde_json::from_str(&source).map_err(|_| NodeConfigError::Invalid)?;
    Ok(Some(PolicySettings {
        workspace: policy.workspace,
        max_context_tokens: policy.max_context_tokens,
        max_seconds: policy.max_seconds,
        queue_global: policy.queue_global,
        queue_per_peer: policy.queue_per_peer,
    }))
}

/// Save the common policy settings without removing advanced fields from an
/// imported policy. The running node keeps its current settings until restart.
pub fn save_policy_settings(
    data_dir: &Path,
    settings: &PolicySettings,
) -> Result<(), NodeConfigError> {
    validate_policy_settings(settings)?;
    let store = NodeStore::in_dir(data_dir).map_err(|_| NodeConfigError::Read)?;
    let source = store.configuration(POLICY_KEY).map_err(|_| NodeConfigError::Read)?;
    let mut policy: serde_json::Value =
        serde_json::from_str(&source).map_err(|_| NodeConfigError::Invalid)?;
    let object = policy.as_object_mut().ok_or(NodeConfigError::Invalid)?;
    object.insert("workspace".to_owned(), serde_json::json!(settings.workspace));
    object.insert(
        "maxContextTokens".to_owned(),
        serde_json::json!(settings.max_context_tokens),
    );
    object.insert("maxSeconds".to_owned(), serde_json::json!(settings.max_seconds));
    object.insert("queueGlobal".to_owned(), serde_json::json!(settings.queue_global));
    object
        .insert("queuePerPeer".to_owned(), serde_json::json!(settings.queue_per_peer));
    let source =
        serde_json::to_string(&policy).map_err(|_| NodeConfigError::Invalid)?;
    store.put_configuration(POLICY_KEY, &source).map_err(|_| NodeConfigError::Read)?;
    // Older live admin changes must not override the new saved limits.
    for id in ["queue_global", "queue_per_peer"] {
        store
            .delete_configuration(&format!("admin.limit.{id}"))
            .map_err(|_| NodeConfigError::Read)?;
    }
    Ok(())
}

pub fn validate_policy_settings(
    settings: &PolicySettings,
) -> Result<(), NodeConfigError> {
    if !settings.workspace.is_absolute() || !settings.workspace.is_dir() {
        return Err(NodeConfigError::InvalidField("workspace".to_owned()));
    }
    if settings.max_context_tokens == 0
        || settings.max_seconds == 0
        || settings.queue_global == 0
        || settings.queue_per_peer == 0
    {
        return Err(NodeConfigError::InvalidField("limits".to_owned()));
    }
    Ok(())
}

/// Open the one node database. On the first start, import an old policy file
/// if present; otherwise create a direct policy and its task-folder root.
pub fn load_or_create_service(
    import_path: &Path,
    data_dir: &Path,
    codex_binary: PathBuf,
) -> Result<NodeService<RosterClaims, NodeAgent>, NodeConfigError> {
    load_or_create_service_with_adapter(import_path, data_dir, codex_binary, None)
}

/// `load_or_create_service`, and when `adapter` is given, save it as the
/// agent adapter of this data directory first. Later starts keep it.
pub fn load_or_create_service_with_adapter(
    import_path: &Path,
    data_dir: &Path,
    codex_binary: PathBuf,
    adapter: Option<AgentAdapter>,
) -> Result<NodeService<RosterClaims, NodeAgent>, NodeConfigError> {
    fs::create_dir_all(data_dir).map_err(|_| NodeConfigError::Read)?;
    let store = NodeStore::in_dir(data_dir).map_err(|_| NodeConfigError::Read)?;
    let (source, first_start) = match store.configuration(POLICY_KEY) {
        Ok(source) => (source.into_bytes(), false),
        Err(StoreError::NotFound) if import_path.is_file() => {
            (fs::read(import_path).map_err(|_| NodeConfigError::Read)?, true)
        }
        Err(StoreError::NotFound) => {
            let workspace = data_dir.parent().unwrap_or(data_dir).join("tasks");
            fs::create_dir_all(&workspace).map_err(|_| NodeConfigError::Read)?;
            let workspace =
                workspace.canonicalize().map_err(|_| NodeConfigError::Read)?;
            (
                serde_json::to_vec(&serde_json::json!({
                    "scope": {"id": "direct", "kind": "direct"},
                    "workspace": workspace,
                    "maxContextTokens": 20000,
                    "maxSeconds": 120,
                    "queueGlobal": 1,
                    "queuePerPeer": 1
                }))
                .map_err(|_| NodeConfigError::Invalid)?,
                true,
            )
        }
        Err(_) => return Err(NodeConfigError::Read),
    };
    let (source, changed) = match adapter {
        Some(adapter) => with_adapter(&source, adapter)?,
        None => (source, false),
    };
    let service = load_service_source(&source, data_dir, codex_binary)?;
    if first_start || changed {
        let source = String::from_utf8(source).map_err(|_| NodeConfigError::Invalid)?;
        store
            .put_configuration(POLICY_KEY, &source)
            .map_err(|_| NodeConfigError::Read)?;
    }
    Ok(service)
}

/// A node setting in the configuration table, with the type of its value.
pub trait Setting {
    /// The configuration key.
    const KEY: &'static str;
    type Value;

    /// The value when the key is not stored.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the setting has no default.
    fn absent() -> Result<Self::Value, StoreError> {
        Err(StoreError::NotFound)
    }

    /// The value of the stored text.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the text is not a value.
    fn decode(stored: String) -> Result<Self::Value, StoreError>;

    /// The text to store for `value`; `None` removes the key.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the value has no text form.
    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError>;
}

/// The stored value of the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails, or as [`Setting::absent`] and
/// [`Setting::decode`] return.
pub fn setting<S: Setting>(data_dir: &Path) -> Result<S::Value, StoreError> {
    match NodeStore::in_dir(data_dir)?.configuration(S::KEY) {
        Ok(stored) => S::decode(stored),
        Err(StoreError::NotFound) => S::absent(),
        Err(error) => Err(error),
    }
}

/// Keeps `value` for the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails or the value has no text form.
pub fn store_setting<S: Setting>(
    data_dir: &Path,
    value: &S::Value,
) -> Result<(), StoreError> {
    let store = NodeStore::in_dir(data_dir)?;
    match S::encode(value)? {
        Some(text) => store.put_configuration(S::KEY, &text),
        None => store.delete_configuration(S::KEY),
    }
}

/// Whether `url` is a relay URL an owner may choose: an `http` or `https`
/// URL with a host, in the form iroh reads.
#[must_use]
pub fn valid_relay_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|parsed| {
        matches!(parsed.scheme(), "https" | "http") && parsed.has_host()
    }) && url.parse::<iroh::RelayUrl>().is_ok()
}

/// Waits for `switch`, which moves a running node to `relay`, and then
/// keeps `relay` as the saved and the active relay.
///
/// # Errors
///
/// The error of `switch`, or [`TransportError::Io`] when the store fails.
///
/// [`TransportError::Io`]: crate::transport::TransportError::Io
pub async fn apply_relay(
    data_dir: &Path,
    relay: &Option<String>,
    switch: impl Future<Output = Result<(), crate::transport::TransportError>>,
) -> Result<(), crate::transport::TransportError> {
    switch.await?;
    store_setting::<settings::Relay>(data_dir, relay)
        .and_then(|()| store_setting::<settings::ActiveRelay>(data_dir, relay))
        .map_err(|_| crate::transport::TransportError::Io)
}

/// The node settings.
pub mod settings {
    use aether_store::StoreError;

    use super::Setting;

    const DEFAULT_RELAY_URL: &str = "https://aether-relay.tudorandrei.xyz";
    const RELAY_OFF: &str = "off";

    fn relay(stored: String) -> Option<String> {
        (stored != RELAY_OFF).then_some(stored)
    }

    fn relay_text(relay: Option<&String>) -> String {
        relay.map_or_else(|| RELAY_OFF.to_owned(), Clone::clone)
    }

    /// The owner's relay choice, kept for the next start. `None` keeps the
    /// node on the local network. A new node has the deployed relay.
    pub struct Relay;

    impl Setting for Relay {
        const KEY: &'static str = "node.relay_url";
        type Value = Option<String>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(Some(DEFAULT_RELAY_URL.to_owned()))
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(relay(stored))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(Some(relay_text(value.as_ref())))
        }
    }

    /// The relay this running node uses for connect-code invites. The
    /// saved [`Relay`] can differ when a command-line override or an MCP
    /// edit is present.
    pub struct ActiveRelay;

    impl Setting for ActiveRelay {
        const KEY: &'static str = "node.runtime.relay_url";
        type Value = Option<String>;

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(relay(stored))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(Some(relay_text(value.as_ref())))
        }
    }

    /// The name of connect codes while the node has no name on the hub.
    pub struct HostName;

    impl Setting for HostName {
        const KEY: &'static str = "node.runtime.host_name";
        type Value = String;

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(stored)
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(Some(value.clone()))
        }
    }

    /// The hub last set from the admin page: a URL, or `off`. `None`
    /// returns to the default hub.
    pub struct Hub;

    impl Setting for Hub {
        const KEY: &'static str = "node.hub_url";
        type Value = Option<String>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(Some(stored))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(value.clone())
        }
    }

    /// This node's name on its hub, as the hub last gave it, if it passes
    /// [`crate::direct::valid_name`]. `None` forgets it, for example when
    /// the node moves to another hub.
    pub struct HubName;

    impl Setting for HubName {
        const KEY: &'static str = "node.hub_name";
        type Value = Option<String>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(Some(stored).filter(|name| crate::direct::valid_name(name)))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(value.clone())
        }
    }

    /// The type of this node's agent, once it has one. Keep it with
    /// [`super::store_agent_kind`], which does not change it.
    pub struct AgentKind;

    impl Setting for AgentKind {
        const KEY: &'static str = "node.agent_kind";
        type Value = Option<crate::hub_client::AgentKind>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(crate::hub_client::AgentKind::parse(&stored))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(value.map(|kind| kind.as_str().to_owned()))
        }
    }

    /// The listing that this node last sent to its hub as its public
    /// listing, if any. `None` forgets it.
    pub struct ListingState;

    impl Setting for ListingState {
        const KEY: &'static str = "node.listing_state";
        type Value = Option<crate::listing::ListingState>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(serde_json::from_str(&stored).ok())
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            value
                .as_ref()
                .map(|state| {
                    serde_json::to_string(state).map_err(|_| StoreError::InvalidState)
                })
                .transpose()
        }
    }
}

/// Keeps the type of this node's agent. The type does not change: another
/// type than the stored one is [`StoreError::InvalidState`].
pub fn store_agent_kind(
    data_dir: &Path,
    kind: crate::hub_client::AgentKind,
) -> Result<(), StoreError> {
    match setting::<settings::AgentKind>(data_dir).ok().flatten() {
        Some(stored) if stored == kind => Ok(()),
        Some(_) => Err(StoreError::InvalidState),
        None => store_setting::<settings::AgentKind>(data_dir, &Some(kind)),
    }
}

/// The task queue limits in force at the next node start: the stored
/// policy with the `admin.limit.*` overrides of `aether.change`, as
/// [`load_service`] reads them. `None` when no policy is stored.
pub fn queue_limits(data_dir: &Path) -> Result<Option<QueueLimits>, NodeConfigError> {
    let Some(policy) = policy_settings(data_dir)? else {
        return Ok(None);
    };
    let store = NodeStore::in_dir(data_dir).map_err(|_| NodeConfigError::Read)?;
    Ok(Some(QueueLimits {
        global: stored_limit(&store, "queue_global", policy.queue_global)?,
        per_peer: stored_limit(&store, "queue_per_peer", policy.queue_per_peer)?,
    }))
}

pub fn load_service(
    path: &Path,
    data_dir: &Path,
    codex_binary: PathBuf,
) -> Result<NodeService<RosterClaims, NodeAgent>, NodeConfigError> {
    let source = fs::read(path).map_err(|_| NodeConfigError::Read)?;
    load_service_source(&source, data_dir, codex_binary)
}

fn load_service_source(
    source: &[u8],
    data_dir: &Path,
    codex_binary: PathBuf,
) -> Result<NodeService<RosterClaims, NodeAgent>, NodeConfigError> {
    let mut decoder = serde_json::Deserializer::from_slice(source);
    let policy: NodePolicyFile = serde_path_to_error::deserialize(&mut decoder)
        .map_err(|error| NodeConfigError::InvalidField(error.path().to_string()))?;
    let mut scopes = parse_scopes(policy.scope, policy.scopes)?;
    if scopes.iter().any(|scope| scope.kind == ScopeKind::Pact) {
        // The gateway owns the one `pact` scope; it is not declared by hand.
        return Err(NodeConfigError::InvalidField("scopes".to_owned()));
    }
    let pact = policy
        .pact
        .map(|pact| pact.parse(policy.max_context_tokens, policy.max_seconds))
        .transpose()
        .map_err(|error| NodeConfigError::InvalidField(error.to_string()))?;
    if pact.is_some() {
        let id = ScopeId::new(crate::pact::config::SCOPE_ID);
        if scopes.iter().any(|scope| scope.id == id) {
            return Err(NodeConfigError::InvalidField("scopes".to_owned()));
        }
        scopes.push(Scope { id, kind: ScopeKind::Pact });
    }
    if policy.trusted_issuers.contains_key(NODE_ISSUER) {
        return Err(NodeConfigError::InvalidField(format!(
            "trustedIssuers.{NODE_ISSUER}"
        )));
    }
    let mut trusted_issuers = policy
        .trusted_issuers
        .into_iter()
        .map(|(issuer, key)| Ok((AgentId::new(issuer), decode_key(&key)?)))
        .collect::<Result<BTreeMap<_, _>, NodeConfigError>>()?;
    if policy.max_context_tokens == 0 || policy.max_seconds == 0 {
        return Err(NodeConfigError::Invalid);
    }
    let sandbox = match policy.adapter {
        AgentAdapter::Codex => Some(policy.sandbox.unwrap_or_else(default_sandbox)),
        // The node cannot hold a connected agent to the Codex settings, so
        // a policy that sets them is refused, not silently ignored.
        AgentAdapter::Connected => {
            if policy.sandbox.is_some() {
                return Err(NodeConfigError::InvalidField("sandbox".to_owned()));
            }
            if policy.allow_network {
                return Err(NodeConfigError::InvalidField("allowNetwork".to_owned()));
            }
            if !policy.environment_allowlist.is_empty() {
                return Err(NodeConfigError::InvalidField(
                    "environmentAllowlist".to_owned(),
                ));
            }
            None
        }
    };
    if matches!(sandbox, Some(PolicySandbox::TaskFolder)) && policy.allow_network {
        return Err(NodeConfigError::InvalidField("allowNetwork".to_owned()));
    }
    fs::create_dir_all(data_dir).map_err(|_| NodeConfigError::Invalid)?;
    let node_issuer = node_issuer_key(data_dir)?;
    trusted_issuers
        .insert(AgentId::new(NODE_ISSUER), node_issuer.verifying_key().to_bytes());
    let mut store =
        NodeStore::in_dir(data_dir).map_err(|_| NodeConfigError::Invalid)?;
    let mut grants =
        policy.grants.into_iter().map(parse_grant).collect::<Result<Vec<_>, _>>()?;
    // A grant revoked from the admin UI stays revoked, even when the policy
    // file still lists it.
    grants.retain(|grant| {
        store
            .configuration(&crate::admin::revoked_grant_key(grant.id.as_str()))
            .is_err()
    });
    let signed_claims = SignedClaims::new(trusted_issuers, Duration::from_secs(5));
    for grant in &grants {
        let scope = scopes
            .iter()
            .find(|scope| scope.id == grant.scope_id)
            .ok_or(NodeConfigError::Invalid)?;
        signed_claims
            .verify_grant(grant, scope)
            .map_err(|_| NodeConfigError::Invalid)?;
    }
    let queue_limits = QueueLimits {
        global: stored_limit(&store, "queue_global", policy.queue_global)?,
        per_peer: stored_limit(&store, "queue_per_peer", policy.queue_per_peer)?,
    };
    let stored_workspace = match store.configuration("admin.current.workspace") {
        Ok(value) => Some(value),
        Err(StoreError::NotFound) => None,
        Err(_) => return Err(NodeConfigError::Invalid),
    };
    let stored_grants = store
        .configuration_values("admin.grant.")
        .map_err(|_| NodeConfigError::Invalid)?;
    let mut initial_members = grants
        .iter()
        .filter(|grant| {
            scopes.iter().any(|scope| {
                scope.id == grant.scope_id && scope.kind == ScopeKind::Direct
            })
        })
        .map(|grant| {
            (
                grant.subject_agent_id.as_str().to_owned(),
                grant.subject_endpoint_id.as_str().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    for value in &stored_grants {
        if let Ok(grant) = parse_grant_json(value)
            && scopes.iter().any(|scope| {
                scope.id == grant.scope_id && scope.kind == ScopeKind::Direct
            })
        {
            initial_members.push((
                grant.subject_agent_id.as_str().to_owned(),
                grant.subject_endpoint_id.as_str().to_owned(),
            ));
        }
    }
    store
        .initialize_access_groups(&initial_members)
        .map_err(|_| NodeConfigError::Invalid)?;
    let trust = RosterClaims::new(signed_claims, data_dir.join("node.sqlite"));
    // The folder must exist; say which field names it.
    let workspace = policy
        .workspace
        .canonicalize()
        .map_err(|_| NodeConfigError::InvalidField("workspace".to_owned()))?;
    let agent = match sandbox {
        Some(sandbox) => NodeAgent::Codex(codex_agent(
            data_dir,
            codex_binary,
            CodexLimits {
                workspace: workspace.clone(),
                sandbox: sandbox.as_str().to_owned(),
                // Nobody reads a remote task, so nothing waits for an
                // approval. An MCP write tool runs only when the agent's
                // config sets `approval_mode = "approve"` for it.
                approval: "never".to_owned(),
                allow_network: policy.allow_network,
                environment: BTreeMap::new(),
                max_context_tokens: policy.max_context_tokens,
                max_seconds: policy.max_seconds,
            },
            &policy.environment_allowlist,
        )?),
        None => NodeAgent::Connected(
            ConnectedAgent::listen(
                &connected::socket_path(data_dir),
                Duration::from_secs(policy.max_seconds),
            )
            .map_err(|_| NodeConfigError::InvalidField("adapter".to_owned()))?,
        ),
    };
    let adapter = agent.kind();
    let executor = Executor::new(
        agent,
        Limits {
            workspace,
            max_context_tokens: policy.max_context_tokens,
            max_seconds: policy.max_seconds,
        },
    );
    let node = ProofNode::with_scopes(
        PolicyEngine::new(trust, grants),
        store,
        executor,
        queue_limits,
        scopes,
    )
    .map_err(|_| NodeConfigError::Invalid)?;
    let mut service = NodeService::new(node);
    service.set_task_folder_mode(matches!(sandbox, Some(PolicySandbox::TaskFolder)));
    service.set_agent_adapter(adapter);
    service.set_pact_config(pact);
    // Fail closed the same way the policy file's own `workspace` field
    // already does: a named-but-missing approved directory is a
    // misconfiguration to catch at startup, not something to silently
    // drop from the approved set.
    let approved_workspaces = policy
        .approved_workspaces
        .into_iter()
        .map(|(id, path)| {
            path.canonicalize().map(|path| (id, path)).map_err(|_| {
                NodeConfigError::InvalidField("approvedWorkspaces".to_owned())
            })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    service.set_approved_workspaces(approved_workspaces);
    // A validated live workspace or grant change survives only in
    // `configuration`; the node built above always starts from the policy
    // file's baseline. Replay both here so a restart resumes the state the
    // node was actually running with, not the file's stale defaults. An
    // unreadable stored workspace fails startup outright (fail closed, the
    // same as an invalid policy-file workspace above); a grant that no
    // longer verifies (commonly: it expired since it was granted) is
    // skipped rather than blocking startup, since that is its expected
    // eventual lifecycle, not a sign of corruption.
    if let Some(path) = stored_workspace.filter(|_| !service.task_folder_mode()) {
        service.apply_workspace(Path::new(&path)).map_err(|_| {
            NodeConfigError::InvalidField("admin.current.workspace".to_owned())
        })?;
    }
    for grant in stored_grants {
        if service.apply_stored_grant(&grant).is_err() {
            eprintln!(
                "aether: stored grant could not be restored on restart, skipping"
            );
        }
    }
    Ok(service)
}

/// The Codex adapter of a node: its own Codex home, which shares only the
/// owner's login, and the allowlisted environment.
fn codex_agent(
    data_dir: &Path,
    codex_binary: PathBuf,
    limits: CodexLimits,
    environment_allowlist: &[String],
) -> Result<CodexAppServer, NodeConfigError> {
    let owner_codex_home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
        });
    let codex_home = prepare_codex_home(data_dir, owner_codex_home.as_deref())?;
    let environment = codex_environment(&codex_home, environment_allowlist, |name| {
        std::env::var(name).ok()
    })?;
    Ok(CodexAppServer::new(codex_binary, CodexLimits { environment, ..limits }))
}

/// Variables that choose where Codex reads its configuration, credentials or
/// state. aether sets `CODEX_HOME` and `HOME` itself; letting the policy
/// pass the node owner's own values would load the owner's `~/.codex` setup
/// (its MCP servers, hooks, plugins, memories) for a remote peer's task. The
/// agent's own MCP servers are in the node's Codex home.
const RESERVED_ENVIRONMENT: &[&str] = &[
    "HOME",
    "CODEX_HOME",
    "CODEX_SQLITE_HOME",
    "CODEX_AUTH",
    "CODEX_URL",
    "CODEX_EXEC_SERVER_URL",
    "CODEX_MANAGED_PACKAGE_ROOT",
];

/// The Codex home aether owns for this node.
#[must_use]
pub fn codex_home(data_dir: &Path) -> PathBuf {
    data_dir.join("codex-home")
}

/// Creates the node's Codex home. It shares only the owner's login file, when
/// one exists. Codex writes refreshed credentials through the link, so both
/// processes use the same token file. The owner's `~/.codex` settings stay
/// outside this home; a `config.toml` here is the agent's own (its MCP
/// servers) and is kept as it is. Without `CODEX_HOME`, Codex would load the
/// owner's full setup.
fn prepare_codex_home(
    data_dir: &Path,
    owner_codex_home: Option<&Path>,
) -> Result<PathBuf, NodeConfigError> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let codex_home = codex_home(data_dir);
    fs::create_dir_all(codex_home.join("home"))
        .map_err(|_| NodeConfigError::Invalid)?;
    fs::set_permissions(&codex_home, fs::Permissions::from_mode(0o700))
        .map_err(|_| NodeConfigError::Invalid)?;
    let auth = codex_home.join("auth.json");
    if let Err(error) = fs::symlink_metadata(&auth) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(NodeConfigError::Invalid);
        }
        if let Some(owner_auth) = owner_codex_home.map(|home| home.join("auth.json")) {
            let owner_auth = std::path::absolute(owner_auth)
                .map_err(|_| NodeConfigError::Invalid)?;
            if owner_auth != auth && owner_auth.is_file() {
                symlink(owner_auth, auth).map_err(|_| NodeConfigError::Invalid)?;
            }
        }
    }
    codex_home.canonicalize().map_err(|_| NodeConfigError::Invalid)
}

/// The spawned Codex environment: the allowlisted variables plus the node's
/// own `CODEX_HOME` and `HOME`.
fn codex_environment(
    codex_home: &Path,
    names: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, NodeConfigError> {
    let mut environment = allowed_environment(names, lookup)?;
    let home = codex_home.join("home");
    for (name, path) in [("CODEX_HOME", codex_home), ("HOME", home.as_path())] {
        let path = path.to_str().ok_or(NodeConfigError::Invalid)?;
        environment.insert(name.to_owned(), path.to_owned());
    }
    Ok(environment)
}

/// Copies each allowlisted variable out of `lookup`. Fails closed, like a
/// missing workspace: a name that is unset, malformed, reserved, or over the
/// executor's own limit would otherwise pass startup and then reject or
/// break every task at run time, with nothing pointing back at the policy.
fn allowed_environment(
    names: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, NodeConfigError> {
    // Two slots stay free for the `CODEX_HOME` and `HOME` aether adds.
    if names.len() > MAX_ENVIRONMENT - 2 {
        return Err(NodeConfigError::InvalidField("environmentAllowlist".to_owned()));
    }
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            lookup(name)
                .filter(|value| {
                    valid_environment_key(name)
                        && valid_environment_value(value)
                        && !RESERVED_ENVIRONMENT.contains(&name.as_str())
                })
                .map(|value| (name.clone(), value))
                .ok_or_else(|| {
                    NodeConfigError::InvalidField(format!(
                        "environmentAllowlist[{index}]"
                    ))
                })
        })
        .collect()
}

fn stored_limit(
    store: &NodeStore,
    id: &str,
    fallback: usize,
) -> Result<usize, NodeConfigError> {
    match store.configuration(&format!("admin.limit.{id}")) {
        Ok(value) => {
            value.parse::<usize>().ok().filter(|value| *value > 0).ok_or_else(|| {
                NodeConfigError::InvalidField(format!("admin.limit.{id}"))
            })
        }
        Err(StoreError::NotFound) => Ok(fallback),
        Err(_) => Err(NodeConfigError::Invalid),
    }
}

fn parse_scopes(
    scope: Option<ScopeFile>,
    scopes: Option<Vec<ScopeFile>>,
) -> Result<Vec<Scope>, NodeConfigError> {
    let scopes = match (scope, scopes) {
        (Some(scope), None) => vec![scope],
        (None, Some(scopes)) if !scopes.is_empty() => scopes,
        _ => return Err(NodeConfigError::Invalid),
    };
    let scopes = scopes.into_iter().map(parse_scope).collect::<Result<Vec<_>, _>>()?;
    let mut ids = BTreeSet::new();
    if scopes.iter().any(|scope| !ids.insert(scope.id.clone())) {
        return Err(NodeConfigError::Invalid);
    }
    Ok(scopes)
}

fn parse_scope(file: ScopeFile) -> Result<Scope, NodeConfigError> {
    let kind = ScopeKind::parse(&file.kind).ok_or(NodeConfigError::Invalid)?;
    if file.id.is_empty() {
        return Err(NodeConfigError::Invalid);
    }
    Ok(Scope { id: ScopeId::new(file.id), kind })
}

pub fn parse_grant_json(value: &str) -> Result<Grant, NodeConfigError> {
    let mut decoder = serde_json::Deserializer::from_slice(value.as_bytes());
    let file: GrantFile = serde_path_to_error::deserialize(&mut decoder)
        .map_err(|error| NodeConfigError::InvalidField(error.path().to_string()))?;
    parse_grant(file)
}

/// The policy-file form of `grant`, which `parse_grant_json` reads back.
#[must_use]
pub fn grant_json(grant: &Grant) -> serde_json::Value {
    let seconds = |time| aether_core::time::unix_seconds(time).unwrap_or(0);
    serde_json::json!({
        "id": grant.id.as_str(),
        "issuerAgentId": grant.issuer_agent_id.as_str(),
        "subjectAgentId": grant.subject_agent_id.as_str(),
        "subjectEndpointId": grant.subject_endpoint_id.as_str(),
        "scopeId": grant.scope_id.as_str(),
        "capabilities": grant.capabilities.iter().map(CapabilityId::as_str).collect::<Vec<_>>(),
        "workspaces": grant.workspaces.iter().map(WorkspaceId::as_str).collect::<Vec<_>>(),
        "accessMode": grant.access_mode.as_str(),
        "approvalRule": grant.approval_rule.as_str(),
        "issuedAt": seconds(grant.issued_at),
        "expiresAt": seconds(grant.expires_at),
        "signingKey": hex::encode(grant.signing_key),
        "nonce": hex::encode(grant.nonce),
        "signature": hex::encode(&grant.signature),
    })
}

fn parse_grant(file: GrantFile) -> Result<Grant, NodeConfigError> {
    let capabilities = file
        .capabilities
        .into_iter()
        .filter(|value| !value.is_empty())
        .map(CapabilityId::new)
        .collect::<BTreeSet<_>>();
    let workspaces = file
        .workspaces
        .into_iter()
        .filter(|value| !value.is_empty())
        .map(WorkspaceId::new)
        .collect::<BTreeSet<_>>();
    let access_mode =
        AccessMode::parse(&file.access_mode).ok_or(NodeConfigError::Invalid)?;
    let approval_rule =
        ApprovalRule::parse(&file.approval_rule).ok_or(NodeConfigError::Invalid)?;
    if file.id.is_empty()
        || file.issuer_agent_id.is_empty()
        || file.subject_agent_id.is_empty()
        || file.subject_endpoint_id.is_empty()
        || file.scope_id.is_empty()
        || capabilities.is_empty()
        || workspaces.is_empty()
    {
        return Err(NodeConfigError::Invalid);
    }
    Ok(Grant {
        id: GrantId::new(file.id),
        issuer_agent_id: AgentId::new(file.issuer_agent_id),
        subject_agent_id: AgentId::new(file.subject_agent_id),
        subject_endpoint_id: aether_core::EndpointId::new(file.subject_endpoint_id),
        scope_id: ScopeId::new(file.scope_id),
        capabilities,
        workspaces,
        access_mode,
        approval_rule,
        issued_at: time(file.issued_at)?,
        expires_at: time(file.expires_at)?,
        signing_key: file.signing_key,
        nonce: file.nonce,
        signature: file.signature,
    })
}

fn time(seconds: u64) -> Result<SystemTime, NodeConfigError> {
    aether_core::time::from_unix_seconds(seconds).ok_or(NodeConfigError::Invalid)
}

fn decode_key(value: &str) -> Result<[u8; 32], NodeConfigError> {
    <[u8; 32]>::from_hex(value).map_err(|_| NodeConfigError::Invalid)
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use aether_core::{
        AccessMode, AgentId, ApprovalRule, CapabilityId, EndpointId, GrantId, ScopeId,
        WorkspaceId,
    };
    use aether_store::{NodeStore, QueueLimits};
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    use super::{
        AgentAdapter, MAX_ENVIRONMENT, NODE_ISSUER, NodeConfigError, NodePolicyFile,
        allowed_environment, codex_environment, grant_json, load_or_create_service,
        load_or_create_service_with_adapter, load_service, node_issuer_key,
        parse_grant_json, prepare_codex_home, setting, settings, store_agent_kind,
        store_setting, with_adapter,
    };
    use crate::{
        hub_client::AgentKind,
        identity::{AccessInput, GrantInput, sign_access, signed_grant},
    };

    #[test]
    fn the_agent_kind_does_not_change_once_stored() {
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(
            setting::<settings::AgentKind>(directory.path()).ok().flatten(),
            None
        );
        store_agent_kind(directory.path(), AgentKind::Personal)
            .expect("kind must store");
        store_agent_kind(directory.path(), AgentKind::Personal)
            .expect("the same kind is kept");
        assert!(store_agent_kind(directory.path(), AgentKind::Service).is_err());
        assert_eq!(
            setting::<settings::AgentKind>(directory.path()).ok().flatten(),
            Some(AgentKind::Personal)
        );
    }

    #[test]
    fn relay_uses_the_deployed_default_until_the_owner_changes_it() {
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(
            setting::<settings::Relay>(directory.path()).ok().flatten().as_deref(),
            Some("https://aether-relay.tudorandrei.xyz")
        );

        store_setting::<settings::Relay>(
            directory.path(),
            &Some("https://other.example.com".to_owned()),
        )
        .expect("relay choice must save");
        assert_eq!(
            setting::<settings::Relay>(directory.path()).ok().flatten().as_deref(),
            Some("https://other.example.com")
        );

        store_setting::<settings::Relay>(directory.path(), &None)
            .expect("local choice must save");
        assert_eq!(setting::<settings::Relay>(directory.path()).ok().flatten(), None);
    }

    #[test]
    fn a_signed_access_grant_survives_the_policy_form() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let (authorization, grant) = sign_access(
            &key,
            &AccessInput {
                issuer_agent_id: "node".to_owned(),
                subject_agent_id: "friend".to_owned(),
                subject_endpoint_id: "friend-endpoint".to_owned(),
                scope_id: "direct".to_owned(),
                capabilities: vec!["review".to_owned()],
                workspaces: vec!["workspace".to_owned()],
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                lifetime: Duration::from_secs(60),
            },
        )
        .expect("access must sign");
        let parsed = parse_grant_json(&grant_json(&grant).to_string())
            .expect("grant must parse back");
        // `SystemTime` keeps sub-second precision; the policy form does not.
        assert_eq!(grant_json(&parsed), grant_json(&grant));
        assert_eq!(authorization.claim.subject_endpoint_id, "friend-endpoint");
        assert_eq!(authorization.claim.signing_key, hex::encode(grant.signing_key));
    }

    #[test]
    fn signed_policy_loads_and_changed_grant_fails() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = SigningKey::from_bytes(&[9; 32]);
        let now = SystemTime::now();
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [3; 32],
            },
        );
        let policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        assert!(load_service(&path, &root, "codex".into()).is_ok());
        let store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_configuration("admin.limit.queue_global", "2")
            .expect("queue limit must store");
        drop(store);
        let service = load_service(&path, &root, "codex".into())
            .expect("stored queue limit must load");
        assert_eq!(
            service.queue_limits().expect("queue limits must load"),
            QueueLimits { global: 2, per_peer: 1 }
        );
        drop(service);
        let mut changed = policy;
        changed["grants"][0]["workspaces"] = json!(["other"]);
        fs::write(&path, serde_json::to_vec(&changed).expect("policy must encode"))
            .expect("policy must write");
        assert!(load_service(&path, &root, "codex".into()).is_err());
    }

    #[test]
    fn policy_decode_error_names_the_invalid_field() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("policy.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "scope": {"id": "direct", "kind": "direct"},
                "trustedIssuers": {},
                "grants": [],
                "workspace": ".",
                "maxContextTokens": "many",
                "maxSeconds": 1,
                "queueGlobal": 1,
                "queuePerPeer": 1,
            }))
            .expect("policy must encode"),
        )
        .expect("policy must write");
        let Err(error) = load_service(&path, &path, "codex".into()) else {
            panic!("policy with a string limit must fail");
        };
        assert_eq!(error, NodeConfigError::InvalidField("maxContextTokens".to_owned()));
    }

    fn minimal_policy(root: &std::path::Path) -> PathBuf {
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let path = root.join("policy.json");
        fs::write(
            &path,
            json!({
                "scope": {"id": "direct", "kind": "direct"},
                "workspace": workspace,
                "maxContextTokens": 1,
                "maxSeconds": 1,
                "queueGlobal": 1,
                "queuePerPeer": 1,
            })
            .to_string(),
        )
        .expect("policy must write");
        path
    }

    #[test]
    fn task_folders_are_the_policy_default_and_refuse_workspace_changes() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = minimal_policy(directory.path());
        let service = load_service(&path, directory.path(), "codex".into())
            .expect("policy must load");
        assert!(service.task_folder_mode());
        assert!(!service.workspace_change_is_approved("workspace", directory.path()));
        assert!(service.apply_workspace(directory.path()).is_err());
    }

    #[test]
    fn a_connected_adapter_listens_on_the_owner_only_agent_socket() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("directory must create");
        let path = minimal_policy(directory.path());
        let mut policy: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("policy must read"))
                .expect("policy must decode");
        policy["adapter"] = json!("connected");
        fs::write(&path, policy.to_string()).expect("policy must write");
        let service = load_service(&path, directory.path(), "codex".into())
            .expect("policy must load");
        assert!(!service.task_folder_mode());
        let socket = crate::agent::connected::socket_path(directory.path());
        let mode =
            fs::metadata(&socket).expect("socket must exist").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!crate::node_config::codex_home(directory.path()).exists());
    }

    #[test]
    fn the_adapter_choice_is_saved_and_kept_on_the_next_start() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("host");
        let import = directory.path().join("missing-policy.json");
        let service = load_or_create_service_with_adapter(
            &import,
            &data_dir,
            "codex".into(),
            Some(AgentAdapter::Connected),
        )
        .expect("node must start");
        assert_eq!(service.agent_adapter(), "connected");
        drop(service);
        let service = load_or_create_service(&import, &data_dir, "codex".into())
            .expect("node must start again");
        assert_eq!(service.agent_adapter(), "connected");
        drop(service);
        let service = load_or_create_service_with_adapter(
            &import,
            &data_dir,
            "codex".into(),
            Some(AgentAdapter::Codex),
        )
        .expect("node must start with Codex");
        assert_eq!(service.agent_adapter(), "codex");
        assert!(service.task_folder_mode());
    }

    #[test]
    fn a_switch_to_a_connected_adapter_removes_the_codex_settings() {
        let source = json!({"workspace": "/w", "sandbox": "workspace-write", "allowNetwork": true})
            .to_string();
        let (source, changed) =
            with_adapter(source.as_bytes(), AgentAdapter::Connected)
                .expect("adapter must set");
        assert!(changed);
        let policy: serde_json::Value =
            serde_json::from_slice(&source).expect("policy must decode");
        assert_eq!(policy, json!({"workspace": "/w", "adapter": "connected"}));
        let (_, changed) =
            with_adapter(&source, AgentAdapter::Connected).expect("adapter must set");
        assert!(!changed);
    }

    #[test]
    fn a_connected_adapter_refuses_the_codex_settings() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = minimal_policy(directory.path());
        let base: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("policy must read"))
                .expect("policy must decode");
        for (field, value) in [
            ("sandbox", json!("task-folder")),
            ("allowNetwork", json!(true)),
            ("environmentAllowlist", json!(["PATH"])),
        ] {
            let mut policy = base.clone();
            policy["adapter"] = json!("connected");
            policy[field] = value;
            fs::write(&path, policy.to_string()).expect("policy must write");
            assert_eq!(
                load_service(&path, directory.path(), "codex".into()).err(),
                Some(NodeConfigError::InvalidField(field.to_owned()))
            );
        }
    }

    fn friend_access(issuer: &str) -> AccessInput {
        AccessInput {
            issuer_agent_id: issuer.to_owned(),
            subject_agent_id: "friend".to_owned(),
            subject_endpoint_id: "friend-endpoint".to_owned(),
            scope_id: "direct".to_owned(),
            capabilities: vec!["review".to_owned()],
            workspaces: vec!["workspace".to_owned()],
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            lifetime: Duration::from_secs(60),
        }
    }

    #[test]
    fn a_policy_without_grants_trusts_only_the_node_key() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path();
        let service = load_service(&minimal_policy(root), root, "codex".into())
            .expect("a policy without grants must load");
        let key = node_issuer_key(root).expect("issuer key must load");
        assert_eq!(
            node_issuer_key(root).expect("issuer key must load again").to_bytes(),
            key.to_bytes(),
            "the key file must be reused, not replaced"
        );
        let (_, grant) =
            sign_access(&key, &friend_access(NODE_ISSUER)).expect("access must sign");
        service
            .apply_stored_grant(&grant_json(&grant).to_string())
            .expect("a grant signed by the node key must apply");
        assert!(service.has_grant(&grant.id).expect("grants must read"));
        let stranger = SigningKey::from_bytes(&[8; 32]);
        let (_, forged) = sign_access(&stranger, &friend_access(NODE_ISSUER))
            .expect("access must sign");
        assert!(service.apply_stored_grant(&grant_json(&forged).to_string()).is_err());
    }

    #[test]
    fn a_revoked_grant_stays_revoked_after_a_restart() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path();
        let path = minimal_policy(root);
        let outside = SigningKey::from_bytes(&[4; 32]);
        let (_, file_grant) =
            sign_access(&outside, &friend_access("issuer")).expect("access must sign");
        let mut policy: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("policy must read"))
                .expect("policy must decode");
        policy["trustedIssuers"] =
            json!({ "issuer": hex::encode(outside.verifying_key().to_bytes()) });
        policy["grants"] = json!([grant_json(&file_grant)]);
        fs::write(&path, policy.to_string()).expect("policy must write");

        let service = std::sync::Arc::new(
            load_service(&path, root, "codex".into()).expect("policy must load"),
        );
        let issuer = node_issuer_key(root).expect("issuer key must load");
        let (_, ui_grant) = sign_access(&issuer, &friend_access(NODE_ISSUER))
            .expect("access must sign");
        let mut administration = crate::admin::Administration::with_store(
            NodeStore::in_dir(root).expect("store must open"),
        );
        administration.set_live(std::sync::Arc::new(
            crate::node_service::RunningNode::new(
                std::sync::Arc::clone(&service),
                root.to_path_buf(),
            ),
        ));
        let change = crate::admin::ChangeRequest::Grant {
            id: ui_grant.id.as_str().to_owned(),
            grant: grant_json(&ui_grant).to_string(),
        }
        .into_change()
        .expect("change must parse");
        administration.apply_ui(&change).expect("the UI grant must apply");
        assert!(service.has_grant(&file_grant.id).expect("grants must read"));
        assert!(service.has_grant(&ui_grant.id).expect("grants must read"));

        for grant in [&file_grant, &ui_grant] {
            administration.revoke_ui(grant.id.as_str()).expect("grant must revoke");
            assert!(!service.has_grant(&grant.id).expect("grants must read"));
        }
        service.shutdown();
        drop(administration);

        let restarted = load_service(&path, root, "codex".into())
            .expect("policy must load after revocation");
        assert!(!restarted.has_grant(&file_grant.id).expect("grants must read"));
        assert!(!restarted.has_grant(&ui_grant.id).expect("grants must read"));
    }

    #[test]
    fn a_policy_cannot_name_another_key_as_the_node_issuer() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path();
        let path = minimal_policy(root);
        let mut policy: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("policy must read"))
                .expect("policy must decode");
        policy["trustedIssuers"] = json!({ NODE_ISSUER: hex::encode([7; 32]) });
        fs::write(&path, policy.to_string()).expect("policy must write");
        let Err(error) = load_service(&path, root, "codex".into()) else {
            panic!("the node issuer must not be redefined");
        };
        assert_eq!(
            error,
            NodeConfigError::InvalidField("trustedIssuers.node".to_owned())
        );
    }

    #[test]
    fn signed_policy_loads_direct_and_directory_scopes() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = SigningKey::from_bytes(&[11; 32]);
        let now = SystemTime::now();
        let direct_grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("direct-grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [4; 32],
            },
        );
        let directory_grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("directory-grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("directory"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [5; 32],
            },
        );
        let mut policy =
            policy(&workspace, &direct_grant, key.verifying_key().to_bytes());
        policy.as_object_mut().expect("policy must be an object").remove("scope");
        policy["scopes"] = json!([
            {"id": "direct", "kind": "direct"},
            {"id": "directory", "kind": "directory"},
        ]);
        policy["grants"] =
            json!([grant_json(&direct_grant), grant_json(&directory_grant),]);
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        let service = load_service(&path, &root, "codex".into())
            .expect("multi-scope policy must load");
        assert_eq!(
            service.scope_kind_for("direct"),
            Some(aether_core::ScopeKind::Direct)
        );
        assert_eq!(
            service.scope_kind_for("directory"),
            Some(aether_core::ScopeKind::Directory)
        );
        assert!(!service.has_scope_kind(aether_core::ScopeKind::Managed));
    }

    #[test]
    fn stored_workspace_and_grant_changes_are_restored_on_restart() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        let other_workspace = root.join("other-workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        fs::create_dir_all(&other_workspace).expect("other workspace must create");
        let key = SigningKey::from_bytes(&[13; 32]);
        let now = SystemTime::now();
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [6; 32],
            },
        );
        let mut policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        policy["sandbox"] = json!("workspace-write");
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        let service =
            load_service(&path, &root, "codex".into()).expect("policy must load");
        assert_eq!(
            service.current_workspace().expect("workspace must load"),
            workspace.canonicalize().expect("workspace must canonicalize")
        );
        drop(service);

        let restored_grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("restored"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("other-caller"),
                subject_endpoint_id: EndpointId::new("other-endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [7; 32],
            },
        );
        let store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_configuration(
                "admin.current.workspace",
                other_workspace.to_str().expect("path must be valid utf-8"),
            )
            .expect("workspace must store");
        store
            .put_configuration(
                "admin.grant.restored",
                &grant_json(&restored_grant).to_string(),
            )
            .expect("grant must store");
        drop(store);

        let restarted = load_service(&path, &root, "codex".into())
            .expect("stored workspace and grant must restore");
        assert_eq!(
            restarted.current_workspace().expect("workspace must load"),
            other_workspace.canonicalize().expect("workspace must canonicalize")
        );
        assert!(matches!(restarted.has_grant(&GrantId::new("restored")), Ok(true)));
    }

    #[test]
    fn a_missing_stored_workspace_fails_node_startup() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = SigningKey::from_bytes(&[14; 32]);
        let now = SystemTime::now();
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [8; 32],
            },
        );
        let mut policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        policy["sandbox"] = json!("workspace-write");
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        let service =
            load_service(&path, &root, "codex".into()).expect("policy must load");
        drop(service);
        let store = NodeStore::in_dir(&root).expect("store must open");
        store
            .put_configuration(
                "admin.current.workspace",
                root.join("missing").to_str().expect("path must be valid utf-8"),
            )
            .expect("workspace must store");
        drop(store);
        let Err(error) = load_service(&path, &root, "codex".into()) else {
            panic!("a missing stored workspace must fail startup");
        };
        assert_eq!(
            error,
            NodeConfigError::InvalidField("admin.current.workspace".to_owned())
        );
    }

    #[test]
    fn an_approved_workspace_naming_a_missing_path_fails_node_startup() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = SigningKey::from_bytes(&[15; 32]);
        let now = SystemTime::now();
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [9; 32],
            },
        );
        let mut policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        policy["approvedWorkspaces"] = json!({"other": root.join("missing").to_str().expect("path must be valid utf-8")});
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        let Err(error) = load_service(&path, &root, "codex".into()) else {
            panic!("an approved workspace naming a missing path must fail");
        };
        assert_eq!(
            error,
            NodeConfigError::InvalidField("approvedWorkspaces".to_owned())
        );
    }

    #[test]
    fn an_approved_workspace_naming_an_existing_path_loads() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        let other = root.join("other");
        fs::create_dir_all(&workspace).expect("workspace must create");
        fs::create_dir_all(&other).expect("other workspace must create");
        let key = SigningKey::from_bytes(&[16; 32]);
        let now = SystemTime::now();
        let grant = signed_grant(
            &key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [10; 32],
            },
        );
        let mut policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        policy["approvedWorkspaces"] =
            json!({"other": other.to_str().expect("path must be valid utf-8")});
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        let service =
            load_service(&path, &root, "codex".into()).expect("policy must load");
        assert!(service.has_approved_workspaces());
        assert_eq!(
            service.approved_workspace("other"),
            Some(other.canonicalize().expect("other must canonicalize").as_path())
        );
        assert_eq!(service.approved_workspace("missing"), None);
    }

    #[test]
    fn a_policy_without_network_or_environment_fields_keeps_both_off() {
        let file: NodePolicyFile = serde_json::from_value(policy(
            std::path::Path::new("/workspace"),
            &test_grant(&SigningKey::from_bytes(&[17; 32]), [11; 32]),
            [0; 32],
        ))
        .expect("policy must decode");
        assert!(!file.allow_network);
        assert!(file.environment_allowlist.is_empty());
    }

    #[test]
    fn a_policy_with_network_and_an_allowlist_loads() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = SigningKey::from_bytes(&[18; 32]);
        let grant = test_grant(&key, [12; 32]);
        let mut policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        policy["sandbox"] = json!("workspace-write");
        policy["allowNetwork"] = json!(true);
        policy["environmentAllowlist"] = json!(["PATH"]);
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        assert!(load_service(&path, &root, "codex".into()).is_ok());
        policy["environmentAllowlist"] = json!(["PATH", "AETHER_TEST_UNSET_VARIABLE"]);
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        assert_eq!(
            load_service(&path, &root, "codex".into()).err(),
            Some(NodeConfigError::InvalidField("environmentAllowlist[1]".to_owned()))
        );
    }

    #[test]
    fn allowed_environment_copies_only_named_variables() {
        let lookup = |name: &str| match name {
            "HOME" => Some("/home/owner".to_owned()),
            "CODEX_HOME" => Some("/home/owner/.codex".to_owned()),
            "PATH" => Some("/usr/bin".to_owned()),
            "NUL" => Some("a\0b".to_owned()),
            "OTHER" => Some("other".to_owned()),
            _ => None,
        };
        assert_eq!(
            allowed_environment(&["PATH".to_owned()], lookup),
            Ok(BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]))
        );
        assert_eq!(allowed_environment(&[], lookup), Ok(BTreeMap::new()));
        for (names, field) in [
            (vec!["PATH", "MISSING"], "environmentAllowlist[1]"),
            (vec!["NUL"], "environmentAllowlist[0]"),
            (vec!["BAD-NAME"], "environmentAllowlist[0]"),
            (vec!["PATH", "HOME"], "environmentAllowlist[1]"),
            (vec!["CODEX_HOME"], "environmentAllowlist[0]"),
        ] {
            let names = names.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                allowed_environment(&names, lookup),
                Err(NodeConfigError::InvalidField(field.to_owned()))
            );
        }
        let too_many = vec!["OTHER".to_owned(); MAX_ENVIRONMENT - 1];
        assert_eq!(
            allowed_environment(&too_many, lookup),
            Err(NodeConfigError::InvalidField("environmentAllowlist".to_owned()))
        );
    }

    #[test]
    fn codex_runs_with_the_node_codex_home_not_the_owner_home() {
        let directory = tempfile::tempdir().expect("directory must create");
        let codex_home =
            prepare_codex_home(directory.path(), None).expect("home must create");
        assert!(codex_home.starts_with(
            directory.path().canonicalize().expect("directory must canonicalize")
        ));
        assert!(codex_home.join("home").is_dir());
        let lookup = |name: &str| (name == "PATH").then(|| "/usr/bin".to_owned());
        let environment = codex_environment(&codex_home, &["PATH".to_owned()], lookup)
            .expect("environment must build");
        let path =
            |path: PathBuf| path.to_str().expect("path must be UTF-8").to_owned();
        assert_eq!(
            environment,
            BTreeMap::from([
                ("CODEX_HOME".to_owned(), path(codex_home.clone())),
                ("HOME".to_owned(), path(codex_home.join("home"))),
                ("PATH".to_owned(), "/usr/bin".to_owned()),
            ])
        );

        let owner_home = directory.path().join("owner-codex");
        fs::create_dir(&owner_home).expect("owner home must create");
        fs::write(owner_home.join("auth.json"), "owner login")
            .expect("owner login must write");
        fs::write(owner_home.join("config.toml"), "[mcp_servers.owner_only]\n")
            .expect("owner config must write");
        fs::write(codex_home.join("auth.json"), "node login")
            .expect("node login must write");
        prepare_codex_home(directory.path(), Some(&owner_home))
            .expect("existing node login must remain");
        assert_eq!(
            fs::read_to_string(codex_home.join("auth.json"))
                .expect("node login must read"),
            "node login"
        );
        // The owner's MCP servers stay out; the agent's own config is the
        // owner's choice for this node and is kept as it is.
        assert!(!codex_home.join("config.toml").exists());
        let agent_config = "[mcp_servers.agent_probe]\ncommand = \"/bin/sh\"\n";
        fs::write(codex_home.join("config.toml"), agent_config)
            .expect("agent config must write");
        prepare_codex_home(directory.path(), Some(&owner_home))
            .expect("the node Codex home must prepare again");
        assert_eq!(
            fs::read_to_string(codex_home.join("config.toml"))
                .expect("agent config must read"),
            agent_config
        );
    }

    fn test_grant(key: &SigningKey, nonce: [u8; 32]) -> aether_core::Grant {
        let now = SystemTime::now();
        signed_grant(
            key,
            GrantInput {
                id: GrantId::new("grant"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("caller"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                capabilities: BTreeSet::from([CapabilityId::new("review")]),
                workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
                access_mode: AccessMode::ReadOnly,
                approval_rule: ApprovalRule::Automatic,
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce,
            },
        )
    }

    fn policy(
        workspace: &std::path::Path,
        grant: &aether_core::Grant,
        trusted_key: [u8; 32],
    ) -> serde_json::Value {
        json!({
            "scope": {"id": "direct", "kind": "direct"},
            "trustedIssuers": {"issuer": hex::encode(trusted_key)},
            "grants": [grant_json(grant)],
            "workspace": workspace,
            "maxContextTokens": 32,
            "maxSeconds": 1,
            "queueGlobal": 1,
            "queuePerPeer": 1,
        })
    }
}
