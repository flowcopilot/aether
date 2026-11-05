use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, ClaimId, EndpointId, GrantId,
    ScopeId, ScopeKind, WorkspaceId,
};
use ed25519_dalek::SigningKey;
use serde_json::json;

use crate::{
    identity::{ClaimInput, GrantInput, signed_claim, signed_grant},
    transport::{A2aAccessMode, A2aAuthorization},
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FixtureError;

impl std::fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("test fixture cannot be created")
    }
}

impl std::error::Error for FixtureError {}

pub async fn create_for_scope(
    data_dir: &Path,
    scope_id: &str,
) -> Result<String, FixtureError> {
    // The fixture names each scope after its kind. A `pact` scope has no
    // Iroh caller to make a fixture for.
    let scope_kind = ScopeKind::parse(scope_id)
        .filter(|kind| *kind != ScopeKind::Pact)
        .ok_or(FixtureError)?;
    let server_data = data_dir.join("server");
    let client_data = data_dir.join("client");
    let workspace = data_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|_| FixtureError)?;
    let endpoint =
        crate::transport::bind_node(&client_data, 0).await.map_err(|_| FixtureError)?;
    let caller_endpoint = EndpointId::new(endpoint.id().to_string());
    endpoint.close().await;
    let mut key_bytes = [0; 32];
    getrandom::fill(&mut key_bytes).map_err(|_| FixtureError)?;
    let key = SigningKey::from_bytes(&key_bytes);
    let now = SystemTime::now();
    let issued_at = now.checked_sub(Duration::from_secs(1)).ok_or(FixtureError)?;
    let expires_at = now.checked_add(Duration::from_secs(300)).ok_or(FixtureError)?;
    let claim = signed_claim(
        &key,
        ClaimInput {
            id: ClaimId::new("fixture-claim"),
            issuer_agent_id: AgentId::new("fixture-issuer"),
            subject_agent_id: AgentId::new("fixture-caller"),
            subject_endpoint_id: caller_endpoint.clone(),
            scope_id: ScopeId::new(scope_id),
            issued_at,
            expires_at,
            nonce: [1; 32],
        },
    );
    let grant = signed_grant(
        &key,
        GrantInput {
            id: GrantId::new("fixture-grant"),
            issuer_agent_id: AgentId::new("fixture-issuer"),
            subject_agent_id: AgentId::new("fixture-caller"),
            subject_endpoint_id: caller_endpoint.clone(),
            scope_id: ScopeId::new(scope_id),
            capabilities: BTreeSet::from([CapabilityId::new("review")]),
            workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at,
            expires_at,
            nonce: [2; 32],
        },
    );
    let policy = data_dir.join("policy.json");
    fs::write(
        &policy,
        serde_json::to_vec(&policy_json(
            &workspace,
            &grant,
            key.verifying_key().to_bytes(),
            scope_id,
            scope_kind,
        ))
        .map_err(|_| FixtureError)?,
    )
    .map_err(|_| FixtureError)?;
    let authorization = data_dir.join("authorization.json");
    fs::write(
        &authorization,
        serde_json::to_vec(&authorization_json(&claim, &caller_endpoint))
            .map_err(|_| FixtureError)?,
    )
    .map_err(|_| FixtureError)?;
    let fake_codex = data_dir.join("fake-codex");
    fs::write(&fake_codex, FAKE_CODEX_SCRIPT).map_err(|_| FixtureError)?;
    set_executable(&fake_codex)?;
    serde_json::to_string(&json!({
        "serverDataDir": server_data,
        "clientDataDir": client_data,
        "workspace": workspace,
        "policy": policy,
        "authorization": authorization,
        "codexBinary": fake_codex,
    }))
    .map_err(|_| FixtureError)
}

fn policy_json(
    workspace: &Path,
    grant: &aether_core::Grant,
    trusted_key: [u8; 32],
    scope_id: &str,
    scope_kind: ScopeKind,
) -> serde_json::Value {
    json!({
        "scope": {"id": scope_id, "kind": scope_kind.as_str()},
        "trustedIssuers": {"fixture-issuer": hex::encode(trusted_key)},
        "grants": [crate::node_config::grant_json(grant)],
        "workspace": workspace,
        "maxContextTokens": 128,
        "maxSeconds": 10,
        "queueGlobal": 1,
        "queuePerPeer": 1,
    })
}

fn authorization_json(
    claim: &aether_core::Claim,
    endpoint: &EndpointId,
) -> A2aAuthorization {
    A2aAuthorization {
        request_id: "fixture-request".to_owned(),
        caller_agent_id: "fixture-caller".to_owned(),
        caller_endpoint_id: endpoint.as_str().to_owned(),
        capability_id: "review".to_owned(),
        workspace_id: "workspace".to_owned(),
        access_mode: A2aAccessMode::ReadOnly,
        claim: claim.into(),
    }
}

/// A fake `codex app-server`: it answers the handshake and the turn, and
/// replies with the text `fake Codex result`.
pub const FAKE_CODEX_SCRIPT: &str = "#!/bin/sh\nwhile IFS= read -r request; do\n  case \"$request\" in\n    *'\"id\":1'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' ;;\n    *'\"id\":2'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"thread\":{\"id\":\"thread\"}}}' ;;\n    *'\"id\":3'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"turn\":{\"id\":\"turn\"}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread\",\"turnId\":\"turn\",\"delta\":\"fake Codex result\"}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread\",\"turn\":{\"id\":\"turn\",\"status\":\"completed\",\"items\":[]}}}'\n      ;;\n  esac\ndone\n";

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), FixtureError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| FixtureError)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), FixtureError> {
    Err(FixtureError)
}
