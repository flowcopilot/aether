//! The node policy: the policy-file schema, the stored copy the node reads
//! after its first start, and the settings the owner can change.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use aether_core::{Scope, ScopeId, ScopeKind, limits::QueueLimitId};
use aether_store::{NodeStore, QueueLimits, StoreError};
use hex::FromHex;
use serde::{Deserialize, Serialize};

use crate::{agent::codex::TASK_FOLDER, data_dir::DataDir, identity::GrantRecord};

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum PolicyError {
    #[error("node policy cannot be read")]
    Read,
    #[error("node policy is invalid")]
    Invalid,
    #[error("node policy is invalid at {0}")]
    InvalidField(String),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PolicyFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<ScopeFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scopes: Option<Vec<ScopeFile>>,
    /// Issuers besides the node's own key ([`crate::identity::NODE_ISSUER`]), which is always
    /// trusted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) trusted_issuers: BTreeMap<String, String>,
    /// Grants besides the ones added from the admin UI or MCP.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) grants: Vec<GrantRecord>,
    pub(crate) workspace: PathBuf,
    /// The adapter of the agent that answers this node's tasks. Absent:
    /// Codex.
    #[serde(default)]
    pub(crate) adapter: AgentAdapter,
    /// Codex only. The default gives each task a new empty folder. The
    /// explicit legacy value lets an operator run tasks in the configured
    /// workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sandbox: Option<PolicySandbox>,
    pub(crate) max_context_tokens: u32,
    pub(crate) max_seconds: u64,
    pub(crate) queue_global: usize,
    pub(crate) queue_per_peer: usize,
    /// Names workspace paths a live `Change::Workspace` may switch to by
    /// id. Absent or empty (the default for an existing policy file, since
    /// `deny_unknown_fields` still requires this to parse) keeps today's
    /// behavior: any path is accepted. Naming even one here switches that
    /// node to "approved IDs only"; `NodeService` checks each change.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) approved_workspaces: BTreeMap<String, PathBuf>,
    /// Codex only. Lets the spawned Codex process reach the network. Absent or false
    /// keeps today's behavior: no network access at all, since a workspace
    /// can be a real repository handed to an executor that otherwise has no
    /// reason to make outbound requests.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_network: bool,
    /// Codex only. Names of environment variables to copy from `aether node serve`'s
    /// own environment into the spawned Codex process — an allowlist, not
    /// literal values, so a credential (e.g. an API key) never has to be
    /// written into this file. Absent or empty: the spawned process gets
    /// only the `CODEX_HOME` and `HOME` that aether always sets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) environment_allowlist: Vec<String>,
    /// The PACT gateway (see `docs/pact.md`). Absent: the node
    /// has no PACT scope and `--pact-port` is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pact: Option<crate::pact::config::PactFile>,
}

/// The adapter of a node's agent. Each data directory has one agent.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
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
    type Err = PolicyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "connected" => Ok(Self::Connected),
            _ => Err(PolicyError::InvalidField("adapter".to_owned())),
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PolicySandbox {
    TaskFolder,
    WorkspaceWrite,
}

pub(crate) const fn default_sandbox() -> PolicySandbox {
    PolicySandbox::TaskFolder
}

impl PolicySandbox {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::TaskFolder => TASK_FOLDER,
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ScopeFile {
    id: String,
    kind: String,
}

/// The task limits of a node created without a policy file.
const DEFAULT_MAX_CONTEXT_TOKENS: u32 = 20_000;
const DEFAULT_MAX_SECONDS: u64 = 120;
const DEFAULT_QUEUE: usize = 1;

/// The configuration key of the stored policy. A policy file is read only
/// on the first start; after that the node reads this key.
const POLICY_KEY: &str = "node.policy";

impl PolicyFile {
    /// The policy of `source`, a policy file's JSON.
    ///
    /// # Errors
    ///
    /// [`PolicyError::InvalidField`] names the field that does not read.
    pub(crate) fn parse(source: &[u8]) -> Result<Self, PolicyError> {
        let mut decoder = serde_json::Deserializer::from_slice(source);
        serde_path_to_error::deserialize(&mut decoder)
            .map_err(|error| PolicyError::InvalidField(error.path().to_string()))
    }

    /// The policy of a node created without a policy file: the direct scope
    /// and task folders under `workspace`.
    fn direct(workspace: PathBuf) -> Self {
        Self {
            scope: Some(ScopeFile {
                id: crate::direct::SCOPE.to_owned(),
                kind: ScopeKind::Direct.as_str().to_owned(),
            }),
            scopes: None,
            trusted_issuers: BTreeMap::new(),
            grants: Vec::new(),
            workspace,
            adapter: AgentAdapter::default(),
            sandbox: None,
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            max_seconds: DEFAULT_MAX_SECONDS,
            queue_global: DEFAULT_QUEUE,
            queue_per_peer: DEFAULT_QUEUE,
            approved_workspaces: BTreeMap::new(),
            allow_network: false,
            environment_allowlist: Vec::new(),
            pact: None,
        }
    }

    /// This policy with `adapter`, and whether that changed it. The Codex
    /// settings do not apply to a connected agent, so a switch to one
    /// removes them; a switch back to Codex starts from the Codex defaults.
    fn with_adapter(mut self, adapter: AgentAdapter) -> (Self, bool) {
        if self.adapter == adapter {
            return (self, false);
        }
        self.adapter = adapter;
        if adapter == AgentAdapter::Connected {
            self.sandbox = None;
            self.allow_network = false;
            self.environment_allowlist.clear();
        }
        (self, true)
    }

    fn settings(&self) -> PolicySettings {
        PolicySettings {
            workspace: self.workspace.clone(),
            max_context_tokens: self.max_context_tokens,
            max_seconds: self.max_seconds,
            queue_global: self.queue_global,
            queue_per_peer: self.queue_per_peer,
        }
    }
}

/// The stored policy of `store`; `None` before the first start.
fn stored(store: &NodeStore) -> Result<Option<PolicyFile>, PolicyError> {
    match store.configuration(POLICY_KEY) {
        Ok(source) => PolicyFile::parse(source.as_bytes()).map(Some),
        Err(StoreError::NotFound) => Ok(None),
        Err(_) => Err(PolicyError::Read),
    }
}

/// Stores `source` as the policy the node reads from its next start.
pub(crate) fn keep(store: &NodeStore, source: &str) -> Result<(), PolicyError> {
    store.put_configuration(POLICY_KEY, source).map_err(|_| PolicyError::Read)
}

/// The policy a node starts with: the stored one; on the first start the
/// file at `import_path` if there is one, else a direct policy with its
/// task-folder root. When `adapter` is given, it becomes the agent adapter
/// of this data directory; later starts keep it. A new or changed policy
/// comes with its text, which the caller keeps with [`keep`] once the node
/// builds from it.
///
/// # Errors
///
/// A [`PolicyError`] when the store, the file or the task folder fails.
pub(crate) fn load_or_create(
    store: &NodeStore,
    import_path: &Path,
    data_dir: &DataDir,
    adapter: Option<AgentAdapter>,
) -> Result<(PolicyFile, Option<String>), PolicyError> {
    let (policy, first_start) = match stored(store)? {
        Some(policy) => (policy, false),
        None if import_path.is_file() => {
            let source = fs::read(import_path).map_err(|_| PolicyError::Read)?;
            (PolicyFile::parse(&source)?, true)
        }
        None => {
            let workspace = data_dir.default_tasks_root();
            fs::create_dir_all(&workspace).map_err(|_| PolicyError::Read)?;
            let workspace = workspace.canonicalize().map_err(|_| PolicyError::Read)?;
            (PolicyFile::direct(workspace), true)
        }
    };
    let (policy, changed) = match adapter {
        Some(adapter) => policy.with_adapter(adapter),
        None => (policy, false),
    };
    let text = (first_start || changed)
        .then(|| serde_json::to_string(&policy).map_err(|_| PolicyError::Invalid))
        .transpose()?;
    Ok((policy, text))
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

/// The settings of the stored policy; `None` before the first start.
///
/// # Errors
///
/// A [`PolicyError`] when the store fails or the stored policy is invalid.
pub fn policy_settings(data_dir: &Path) -> Result<Option<PolicySettings>, PolicyError> {
    let store = NodeStore::in_dir(data_dir).map_err(|_| PolicyError::Read)?;
    Ok(stored(&store)?.as_ref().map(PolicyFile::settings))
}

/// Saves the common policy settings and keeps every other field of the
/// stored policy. The running node keeps its current settings until
/// restart.
///
/// # Errors
///
/// A [`PolicyError`] when the settings are invalid, no policy is stored, or
/// the store fails.
pub fn save_policy_settings(
    data_dir: &Path,
    settings: &PolicySettings,
) -> Result<(), PolicyError> {
    validate_policy_settings(settings)?;
    let store = NodeStore::in_dir(data_dir).map_err(|_| PolicyError::Read)?;
    let policy = stored(&store)?.ok_or(PolicyError::Read)?;
    let policy = PolicyFile {
        workspace: settings.workspace.clone(),
        max_context_tokens: settings.max_context_tokens,
        max_seconds: settings.max_seconds,
        queue_global: settings.queue_global,
        queue_per_peer: settings.queue_per_peer,
        ..policy
    };
    keep(&store, &serde_json::to_string(&policy).map_err(|_| PolicyError::Invalid)?)?;
    // Older live admin changes must not override the new saved limits.
    for id in [QueueLimitId::Global, QueueLimitId::PerPeer] {
        store
            .remove_applied_change(aether_store::ChangeKind::Limit, id.as_str())
            .map_err(|_| PolicyError::Read)?;
    }
    Ok(())
}

pub fn validate_policy_settings(settings: &PolicySettings) -> Result<(), PolicyError> {
    if !settings.workspace.is_absolute() || !settings.workspace.is_dir() {
        return Err(PolicyError::InvalidField("workspace".to_owned()));
    }
    if settings.max_context_tokens == 0
        || settings.max_seconds == 0
        || settings.queue_global == 0
        || settings.queue_per_peer == 0
    {
        return Err(PolicyError::InvalidField("limits".to_owned()));
    }
    Ok(())
}

/// The task queue limits in force at the next node start: the stored
/// policy with the `admin.limit.*` overrides of `aether.change`, as
/// the node builder reads them. `None` when no policy is stored.
pub fn queue_limits(data_dir: &Path) -> Result<Option<QueueLimits>, PolicyError> {
    let store = NodeStore::in_dir(data_dir).map_err(|_| PolicyError::Read)?;
    let Some(policy) = stored(&store)? else {
        return Ok(None);
    };
    Ok(Some(QueueLimits {
        global: stored_limit(&store, QueueLimitId::Global, policy.queue_global)?,
        per_peer: stored_limit(&store, QueueLimitId::PerPeer, policy.queue_per_peer)?,
    }))
}

pub(crate) fn stored_limit(
    store: &NodeStore,
    id: QueueLimitId,
    fallback: usize,
) -> Result<usize, PolicyError> {
    match store.applied_change(aether_store::ChangeKind::Limit, id.as_str()) {
        Ok(Some(value)) => {
            value.parse::<usize>().ok().filter(|value| *value > 0).ok_or_else(|| {
                PolicyError::InvalidField(format!("admin.limit.{}", id.as_str()))
            })
        }
        Ok(None) => Ok(fallback),
        Err(_) => Err(PolicyError::Invalid),
    }
}

pub(crate) fn parse_scopes(
    scope: Option<ScopeFile>,
    scopes: Option<Vec<ScopeFile>>,
) -> Result<Vec<Scope>, PolicyError> {
    let scopes = match (scope, scopes) {
        (Some(scope), None) => vec![scope],
        (None, Some(scopes)) if !scopes.is_empty() => scopes,
        _ => return Err(PolicyError::Invalid),
    };
    let scopes = scopes.into_iter().map(parse_scope).collect::<Result<Vec<_>, _>>()?;
    let mut ids = BTreeSet::new();
    if scopes.iter().any(|scope| !ids.insert(scope.id.clone())) {
        return Err(PolicyError::Invalid);
    }
    Ok(scopes)
}

fn parse_scope(file: ScopeFile) -> Result<Scope, PolicyError> {
    let kind = ScopeKind::parse(&file.kind).ok_or(PolicyError::Invalid)?;
    if file.id.is_empty() {
        return Err(PolicyError::Invalid);
    }
    Ok(Scope { id: ScopeId::new(file.id), kind })
}

pub(crate) fn decode_key(value: &str) -> Result<[u8; 32], PolicyError> {
    <[u8; 32]>::from_hex(value).map_err(|_| PolicyError::Invalid)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AgentAdapter, PolicyFile, PolicySandbox};

    #[test]
    fn a_switch_to_a_connected_adapter_removes_the_codex_settings() {
        let mut policy = PolicyFile::direct(PathBuf::from("/w"));
        policy.sandbox = Some(PolicySandbox::WorkspaceWrite);
        policy.allow_network = true;
        policy.environment_allowlist = vec!["PATH".to_owned()];
        let (policy, changed) = policy.with_adapter(AgentAdapter::Connected);
        assert!(changed);
        assert!(policy.sandbox.is_none());
        assert!(!policy.allow_network);
        assert!(policy.environment_allowlist.is_empty());
        let source = serde_json::to_vec(&policy).expect("policy must encode");
        let policy = PolicyFile::parse(&source).expect("policy must decode");
        assert_eq!(policy.adapter, AgentAdapter::Connected);
        let (_, changed) = policy.with_adapter(AgentAdapter::Connected);
        assert!(!changed);
    }
}
