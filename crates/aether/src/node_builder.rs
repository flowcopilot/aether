//! Builds a node's `NodeService` from its policy: the executor and its
//! agent, the trust, the queue, and the live admin changes replayed on top.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use aether_core::{
    AgentId, Grant, PolicyEngine, Scope, ScopeId, ScopeKind, TrustProvider,
};

use crate::{
    agent::{
        NodeAgent,
        codex::{CodexAppServer, CodexLimits, CodexSandbox, build_node_environment},
        connected::ConnectedAgent,
    },
    data_dir::{DataDir, codex_login},
    execution::{Executor, Limits},
    identity::{NODE_ISSUER, RosterClaims, SignedClaims, decode_grant},
    node_service::{NodeOptions, NodeService},
    policy::{
        AgentAdapter, PolicyError, PolicyFile, PolicySandbox, decode_key,
        default_sandbox, keep, load_or_create, parse_scopes, stored_queue_limits,
        stored_running_limit,
    },
    proof::ProofNode,
};

/// Opens the one node database and builds the node from its policy.
///
/// [`crate::policy`] says where the policy comes from on the first start.
/// When `adapter` is given, it becomes the agent adapter of this data
/// directory; later starts keep it.
///
/// # Errors
///
/// A [`PolicyError`] when the policy is invalid or cannot be read, or the
/// node cannot be built from it.
pub fn load_or_create_service(
    import_path: &Path,
    data_dir: &Path,
    codex_binary: PathBuf,
    adapter: Option<AgentAdapter>,
) -> Result<NodeService<RosterClaims, NodeAgent>, PolicyError> {
    let layout = DataDir::new(data_dir);
    let store = layout.store().map_err(|_| PolicyError::Read)?;
    let (policy, new_source) = load_or_create(&store, import_path, &layout, adapter)?;
    let service = build(policy, data_dir, codex_binary)?;
    if let Some(source) = new_source {
        keep(&store, &source)?;
    }
    Ok(service)
}

fn build(
    policy: PolicyFile,
    data_dir: &Path,
    codex_binary: PathBuf,
) -> Result<NodeService<RosterClaims, NodeAgent>, PolicyError> {
    let queue = policy.queue;
    let mut scopes = parse_scopes(policy.scope, policy.scopes)?;
    if scopes.iter().any(|scope| scope.kind == ScopeKind::Pact) {
        // The gateway owns the one `pact` scope; it is not declared by hand.
        return Err(PolicyError::InvalidField("scopes".to_owned()));
    }
    let pact = policy
        .pact
        .map(|pact| pact.parse(policy.max_context_tokens, policy.max_seconds))
        .transpose()
        .map_err(|error| PolicyError::InvalidField(error.to_string()))?;
    if pact.is_some() {
        let id = ScopeId::new(crate::pact::config::SCOPE_ID);
        if scopes.iter().any(|scope| scope.id == id) {
            return Err(PolicyError::InvalidField("scopes".to_owned()));
        }
        scopes.push(Scope { id, kind: ScopeKind::Pact });
    }
    if policy.trusted_issuers.contains_key(NODE_ISSUER) {
        return Err(PolicyError::InvalidField(format!("trustedIssuers.{NODE_ISSUER}")));
    }
    let mut trusted_issuers = policy
        .trusted_issuers
        .into_iter()
        .map(|(issuer, key)| Ok((AgentId::new(issuer), decode_key(&key)?)))
        .collect::<Result<BTreeMap<_, _>, PolicyError>>()?;
    if policy.max_context_tokens == 0 || policy.max_seconds == 0 {
        return Err(PolicyError::Invalid);
    }
    let sandbox = match policy.adapter {
        AgentAdapter::Codex => Some(policy.sandbox.unwrap_or_else(default_sandbox)),
        // The node cannot hold a connected agent to the Codex settings, so
        // a policy that sets them is refused, not silently ignored.
        AgentAdapter::Connected => {
            if policy.sandbox.is_some() {
                return Err(PolicyError::InvalidField("sandbox".to_owned()));
            }
            if policy.allow_network {
                return Err(PolicyError::InvalidField("allowNetwork".to_owned()));
            }
            if !policy.environment_allowlist.is_empty() {
                return Err(PolicyError::InvalidField(
                    "environmentAllowlist".to_owned(),
                ));
            }
            None
        }
    };
    if matches!(sandbox, Some(PolicySandbox::TaskFolder)) && policy.allow_network {
        return Err(PolicyError::InvalidField("allowNetwork".to_owned()));
    }
    let layout = DataDir::new(data_dir);
    let node_issuer = layout
        .issuer_key()
        .map_err(|_| PolicyError::InvalidField("issuer.key".to_owned()))?;
    trusted_issuers
        .insert(AgentId::new(NODE_ISSUER), node_issuer.verifying_key().to_bytes());
    let mut store = layout.store().map_err(|_| PolicyError::Invalid)?;
    // A grant revoked from the admin UI stays revoked, even when the policy
    // file still lists it.
    let mut grants = Vec::new();
    for grant in &policy.grants {
        let grant = Grant::try_from(grant.clone()).map_err(|_| PolicyError::Invalid)?;
        if !store.grant_revoked(grant.id.as_str()).map_err(|_| PolicyError::Invalid)? {
            grants.push(grant);
        }
    }
    let signed_claims = SignedClaims::new(trusted_issuers, Duration::from_secs(5));
    for grant in &grants {
        let scope = scopes
            .iter()
            .find(|scope| scope.id == grant.scope_id)
            .ok_or(PolicyError::Invalid)?;
        signed_claims.verify_grant(grant, scope).map_err(|_| PolicyError::Invalid)?;
    }
    let queue_limits = stored_queue_limits(&store, queue)?;
    let running_limit = stored_running_limit(&store, queue)?;
    let stored_workspace =
        store.current_workspace().map_err(|_| PolicyError::Invalid)?;
    let stored_grants = store
        .applied_changes(aether_store::ChangeKind::Grant)
        .map_err(|_| PolicyError::Invalid)?;
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
        if let Ok(grant) = decode_grant(value)
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
        .map_err(|_| PolicyError::Invalid)?;
    let trust = RosterClaims::new(signed_claims, &layout);
    let workspace = resolve_workspace(&policy.workspace, &layout)?;
    let agent = match sandbox {
        Some(sandbox) => NodeAgent::Codex(codex_agent(
            data_dir,
            codex_binary,
            match sandbox {
                PolicySandbox::TaskFolder => CodexSandbox::TaskFolder,
                PolicySandbox::WorkspaceWrite => CodexSandbox::WorkspaceWrite,
            },
            policy.allow_network,
            &policy.environment_allowlist,
        )?),
        None => NodeAgent::Connected(
            ConnectedAgent::listen(&layout.agent_socket())
                .map_err(|_| PolicyError::InvalidField("adapter".to_owned()))?,
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
    .map_err(|_| PolicyError::Invalid)?;
    node.set_running_limit(running_limit).map_err(|_| PolicyError::Invalid)?;
    let task_folder_mode = matches!(sandbox, Some(PolicySandbox::TaskFolder));
    // Fail closed the same way the policy file's own `workspace` field
    // already does: a named-but-missing approved directory is a
    // misconfiguration to catch at startup, not something to silently
    // drop from the approved set.
    let approved_workspaces = policy
        .approved_workspaces
        .into_iter()
        .map(|(id, path)| {
            path.canonicalize()
                .map(|path| (id, path))
                .map_err(|_| PolicyError::InvalidField("approvedWorkspaces".to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let service = NodeService::new(
        node,
        NodeOptions {
            approved_workspaces,
            task_folder_mode,
            agent_adapter: adapter,
            pact,
        },
    );
    // A validated live workspace or grant change survives only in
    // `configuration`; the node built above always starts from the policy
    // file's baseline. Replay both here so a restart resumes the state the
    // node was actually running with, not the file's stale defaults. An
    // unreadable stored workspace fails startup outright (fail closed, the
    // same as an invalid policy-file workspace above); a grant that no
    // longer verifies (commonly: it expired since it was granted) is
    // skipped rather than blocking startup, since that is its expected
    // eventual lifecycle, not a sign of corruption.
    if let Some(path) = stored_workspace.filter(|_| !task_folder_mode) {
        service.apply_workspace(Path::new(&path)).map_err(|_| {
            PolicyError::InvalidField("admin.current.workspace".to_owned())
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

/// The canonical policy workspace. The default task-folder root beside the
/// data directory belongs to the node, so it is made again when it is gone
/// (a cleaned home directory, a migrated machine). Any other folder belongs
/// to the owner: a missing one fails startup and the error names it.
fn resolve_workspace(
    workspace: &Path,
    layout: &DataDir,
) -> Result<PathBuf, PolicyError> {
    let error = |reason: std::io::Error| PolicyError::Workspace {
        path: workspace.display().to_string(),
        reason: reason.kind().to_string(),
    };
    match workspace.canonicalize() {
        Ok(path) => Ok(path),
        Err(reason)
            if reason.kind() == std::io::ErrorKind::NotFound
                && is_default_tasks_root(workspace, layout) =>
        {
            fs::create_dir_all(workspace).map_err(error)?;
            workspace.canonicalize().map_err(error)
        }
        Err(reason) => Err(error(reason)),
    }
}

/// Whether `workspace` is the default task-folder root. The policy stores
/// that root canonicalized, so the data directory's parent is resolved too.
fn is_default_tasks_root(workspace: &Path, layout: &DataDir) -> bool {
    let default = layout.default_tasks_root();
    if workspace == default {
        return true;
    }
    match (default.parent(), default.file_name()) {
        (Some(parent), Some(name)) => {
            parent.canonicalize().is_ok_and(|parent| parent.join(name) == workspace)
        }
        _ => false,
    }
}

/// The Codex adapter of a node: its own Codex home, which shares only the
/// owner's login, and the allowlisted environment.
fn codex_agent(
    data_dir: &Path,
    codex_binary: PathBuf,
    sandbox: CodexSandbox,
    allow_network: bool,
    environment_allowlist: &[String],
) -> Result<CodexAppServer, PolicyError> {
    let owner_codex_home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
        });
    let codex_home = prepare_codex_home(data_dir, owner_codex_home.as_deref())?;
    let environment = build_node_environment(
        &codex_home,
        environment_allowlist,
        sandbox == CodexSandbox::TaskFolder,
        |name| std::env::var(name).ok(),
    )?;
    CodexAppServer::new(
        codex_binary,
        CodexLimits { sandbox, allow_network, environment },
    )
    .map_err(|_| PolicyError::InvalidField("environmentAllowlist".to_owned()))
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
) -> Result<PathBuf, PolicyError> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let codex_home = DataDir::new(data_dir).codex_home();
    fs::create_dir_all(codex_home.join("home")).map_err(|_| PolicyError::Invalid)?;
    fs::set_permissions(&codex_home, fs::Permissions::from_mode(0o700))
        .map_err(|_| PolicyError::Invalid)?;
    let auth = codex_login(&codex_home);
    if let Err(error) = fs::symlink_metadata(&auth) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(PolicyError::Invalid);
        }
        if let Some(owner_auth) = owner_codex_home.map(codex_login) {
            let owner_auth =
                std::path::absolute(owner_auth).map_err(|_| PolicyError::Invalid)?;
            if owner_auth != auth && owner_auth.is_file() {
                symlink(owner_auth, auth).map_err(|_| PolicyError::Invalid)?;
            }
        }
    }
    codex_home.canonicalize().map_err(|_| PolicyError::Invalid)
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
        build, build_node_environment, load_or_create_service, prepare_codex_home,
    };
    use crate::agent::codex::MAX_ENVIRONMENT;
    use crate::{
        identity::NODE_ISSUER,
        policy::{AgentAdapter, PolicyError, PolicyFile},
    };
    use crate::{
        identity::{
            AccessInput, GrantInput, GrantRecord, encode_grant, sign_access,
            signed_grant,
        },
        test_support::{signed_test_grant, test_signing_key},
    };

    #[test]
    fn signed_policy_loads_and_changed_grant_fails() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let key = test_signing_key();
        let grant = signed_test_grant(&key, |input| {
            input.subject_agent_id = AgentId::new("caller");
            input.nonce = [3; 32];
        });
        let policy = policy(&workspace, &grant, key.verifying_key().to_bytes());
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        assert!(load_service(&path, &root, "codex".into()).is_ok());
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .apply_change(
                aether_store::ChangeKind::Limit,
                aether_core::limits::QueueLimitId::Global.as_str(),
                "2",
                aether_store::AuditActor::Ui,
                1,
            )
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
        let Err(error) =
            load_or_create_service(&path, directory.path(), "codex".into(), None)
        else {
            panic!("policy with a string limit must fail");
        };
        assert_eq!(error, PolicyError::InvalidField("maxContextTokens".to_owned()));
    }

    #[test]
    fn the_public_loader_imports_a_policy_file_on_first_start() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let path = minimal_policy(&root);
        let service = load_or_create_service(&path, &root, "codex".into(), None)
            .expect("policy must load through the public path");
        assert!(service.queue_limits().expect("limits").global >= 1);
    }

    fn load_service(
        path: &std::path::Path,
        data_dir: &std::path::Path,
        codex_binary: PathBuf,
    ) -> Result<
        crate::node_service::NodeService<
            crate::identity::RosterClaims,
            crate::agent::NodeAgent,
        >,
        PolicyError,
    > {
        let source = fs::read(path).map_err(|_| PolicyError::Read)?;
        build(PolicyFile::parse(&source)?, data_dir, codex_binary)
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
    fn the_node_runs_one_task_at_a_time_unless_its_owner_says_more() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        // A policy file from before the limit existed.
        let path = minimal_policy(&root);
        let service = load_service(&path, &root, "codex".into()).expect("policy loads");
        assert_eq!(service.running_limit().expect("limit reads"), 1);
        drop(service);
        // A limit that the owner changed on the running node is kept.
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .apply_change(
                aether_store::ChangeKind::Limit,
                aether_core::limits::QueueLimitId::Running.as_str(),
                "3",
                aether_store::AuditActor::Ui,
                1,
            )
            .expect("running limit must store");
        drop(store);
        let service = load_service(&path, &root, "codex".into())
            .expect("stored running limit must load");
        assert_eq!(service.running_limit().expect("limit reads"), 3);
        drop(service);
        // A policy file that names the limit starts with it.
        let mut policy: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("policy reads"))
                .expect("policy decodes");
        policy["runningTasks"] = json!(2);
        let other = tempfile::tempdir().expect("directory must create");
        let other_root = other.path().to_path_buf();
        let other_path = minimal_policy(&other_root);
        policy["workspace"] = json!(other_root.join("workspace"));
        fs::write(&other_path, policy.to_string()).expect("policy writes");
        let service = load_service(&other_path, &other_root, "codex".into())
            .expect("policy with a running limit loads");
        assert_eq!(service.running_limit().expect("limit reads"), 2);
        // A value that the owner could not set is refused at start.
        policy["runningTasks"] = json!(33);
        fs::write(&other_path, policy.to_string()).expect("policy writes");
        assert!(matches!(
            load_service(&other_path, &other_root, "codex".into()),
            Err(PolicyError::InvalidField(field)) if field == "runningTasks"
        ));
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
        let socket = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let mode =
            fs::metadata(&socket).expect("socket must exist").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!crate::data_dir::DataDir::new(directory.path()).codex_home().exists());
    }

    #[test]
    fn the_adapter_choice_is_saved_and_kept_on_the_next_start() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("host");
        let import = directory.path().join("missing-policy.json");
        let service = load_or_create_service(
            &import,
            &data_dir,
            "codex".into(),
            Some(AgentAdapter::Connected),
        )
        .expect("node must start");
        assert_eq!(service.agent_adapter(), AgentAdapter::Connected);
        drop(service);
        let service = load_or_create_service(&import, &data_dir, "codex".into(), None)
            .expect("node must start again");
        assert_eq!(service.agent_adapter(), AgentAdapter::Connected);
        drop(service);
        let service = load_or_create_service(
            &import,
            &data_dir,
            "codex".into(),
            Some(AgentAdapter::Codex),
        )
        .expect("node must start with Codex");
        assert_eq!(service.agent_adapter(), AgentAdapter::Codex);
        assert!(service.task_folder_mode());
    }

    #[test]
    fn a_deleted_default_task_folder_is_made_again_on_the_next_start() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("host");
        let import = directory.path().join("missing-policy.json");
        let tasks = directory.path().join("tasks");
        let service = load_or_create_service(
            &import,
            &data_dir,
            "codex".into(),
            Some(AgentAdapter::Connected),
        )
        .expect("node must start");
        drop(service);
        fs::remove_dir_all(&tasks).expect("task folder must delete");
        load_or_create_service(&import, &data_dir, "codex".into(), None)
            .expect("node must start without its task folder");
        assert!(tasks.is_dir());
    }

    #[test]
    fn a_missing_owner_workspace_fails_startup_and_names_the_path() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = minimal_policy(directory.path());
        let workspace = directory.path().join("workspace");
        fs::remove_dir_all(&workspace).expect("workspace must delete");
        let Err(error) = load_service(&path, directory.path(), "codex".into()) else {
            panic!("a missing owner workspace must fail startup");
        };
        assert_eq!(
            error,
            PolicyError::Workspace {
                path: workspace.display().to_string(),
                reason: std::io::ErrorKind::NotFound.to_string(),
            }
        );
        assert!(!workspace.exists());
        assert!(error.to_string().contains(&workspace.display().to_string()));
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
                Some(PolicyError::InvalidField(field.to_owned()))
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
        let key = crate::data_dir::DataDir::new(root)
            .issuer_key()
            .expect("issuer key must load");
        assert_eq!(
            crate::data_dir::DataDir::new(root)
                .issuer_key()
                .expect("issuer key must load again")
                .to_bytes(),
            key.to_bytes(),
            "the key file must be reused, not replaced"
        );
        let (_, grant) =
            sign_access(&key, &friend_access(NODE_ISSUER)).expect("access must sign");
        service
            .apply_stored_grant(&encode_grant(&grant).expect("grant must encode"))
            .expect("a grant signed by the node key must apply");
        assert!(service.has_grant(&grant.id).expect("grants must read"));
        let stranger = SigningKey::from_bytes(&[8; 32]);
        let (_, forged) = sign_access(&stranger, &friend_access(NODE_ISSUER))
            .expect("access must sign");
        assert!(
            service
                .apply_stored_grant(&encode_grant(&forged).expect("grant must encode"))
                .is_err()
        );
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
        policy["grants"] = json!([GrantRecord::from(&file_grant)]);
        fs::write(&path, policy.to_string()).expect("policy must write");

        let service = std::sync::Arc::new(
            load_service(&path, root, "codex".into()).expect("policy must load"),
        );
        let issuer = crate::data_dir::DataDir::new(root)
            .issuer_key()
            .expect("issuer key must load");
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
            grant: ui_grant.clone(),
        }
        .into_change()
        .expect("change must parse");
        administration
            .apply(&change, aether_store::AuditActor::Ui)
            .expect("the UI grant must apply");
        assert!(service.has_grant(&file_grant.id).expect("grants must read"));
        assert!(service.has_grant(&ui_grant.id).expect("grants must read"));

        for grant in [&file_grant, &ui_grant] {
            administration
                .revoke(grant.id.as_str(), aether_store::AuditActor::Ui)
                .expect("grant must revoke");
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
        assert_eq!(error, PolicyError::InvalidField("trustedIssuers.node".to_owned()));
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
        policy["grants"] = json!([
            GrantRecord::from(&direct_grant),
            GrantRecord::from(&directory_grant),
        ]);
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
        let mut store = NodeStore::in_dir(&root).expect("store must open");
        store
            .set_current_workspace(
                other_workspace.to_str().expect("path must be valid utf-8"),
            )
            .expect("workspace must store");
        store
            .apply_change(
                aether_store::ChangeKind::Grant,
                "restored",
                &encode_grant(&restored_grant).expect("grant must encode"),
                aether_store::AuditActor::Ui,
                1,
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
            .set_current_workspace(
                root.join("missing").to_str().expect("path must be valid utf-8"),
            )
            .expect("workspace must store");
        drop(store);
        let Err(error) = load_service(&path, &root, "codex".into()) else {
            panic!("a missing stored workspace must fail startup");
        };
        assert_eq!(
            error,
            PolicyError::InvalidField("admin.current.workspace".to_owned())
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
        assert_eq!(error, PolicyError::InvalidField("approvedWorkspaces".to_owned()));
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
            Some(PolicyError::InvalidField("environmentAllowlist[1]".to_owned()))
        );
    }

    #[test]
    fn allowed_environment_copies_only_named_variables() {
        let directory = tempfile::tempdir().expect("directory must create");
        let codex_home =
            prepare_codex_home(directory.path(), None).expect("home must create");
        let lookup = |name: &str| match name {
            "HOME" => Some("/home/owner".to_owned()),
            "CODEX_HOME" => Some("/home/owner/.codex".to_owned()),
            "PATH" => Some("/usr/bin".to_owned()),
            "NUL" => Some("a\0b".to_owned()),
            "OTHER" => Some("other".to_owned()),
            _ => None,
        };
        let path =
            |path: PathBuf| path.to_str().expect("path must be UTF-8").to_owned();
        assert_eq!(
            build_node_environment(&codex_home, &["PATH".to_owned()], false, lookup),
            Ok(BTreeMap::from([
                ("CODEX_HOME".to_owned(), path(codex_home.clone())),
                ("HOME".to_owned(), path(codex_home.join("home"))),
                ("PATH".to_owned(), "/usr/bin".to_owned()),
            ]))
        );
        for (names, field) in [
            (vec!["PATH", "MISSING"], "environmentAllowlist[1]"),
            (vec!["NUL"], "environmentAllowlist[0]"),
            (vec!["BAD-NAME"], "environmentAllowlist[0]"),
            (vec!["PATH", "HOME"], "environmentAllowlist[1]"),
            (vec!["CODEX_HOME"], "environmentAllowlist[0]"),
        ] {
            let names = names.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                build_node_environment(&codex_home, &names, false, lookup),
                Err(PolicyError::InvalidField(field.to_owned()))
            );
        }
        let too_many = vec!["OTHER".to_owned(); MAX_ENVIRONMENT - 1];
        assert_eq!(
            build_node_environment(&codex_home, &too_many, false, lookup),
            Err(PolicyError::InvalidField("environmentAllowlist".to_owned()))
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
        let environment =
            build_node_environment(&codex_home, &["PATH".to_owned()], false, lookup)
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
            "grants": [GrantRecord::from(grant)],
            "workspace": workspace,
            "maxContextTokens": 32,
            "maxSeconds": 1,
            "queueGlobal": 1,
            "queuePerPeer": 1,
        })
    }
}
