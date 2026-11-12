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
    identity::{ClaimInput, GrantInput, GrantRecord, signed_claim, signed_grant},
    transport::{A2aAccessMode, A2aAuthorization},
};

#[derive(Debug, Clone, Copy)]
pub struct PolicyLimits {
    pub max_context_tokens: u64,
    pub max_seconds: u64,
    pub queue_global: u64,
    pub queue_per_peer: u64,
}

impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            max_context_tokens: 32,
            max_seconds: 10,
            queue_global: 1,
            queue_per_peer: 1,
        }
    }
}

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
    let endpoint = crate::transport::bind_node(&client_data, 0, None)
        .await
        .map_err(|_| FixtureError)?;
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
    let policy_path = data_dir.join("policy.json");
    let limits = PolicyLimits { max_context_tokens: 128, ..PolicyLimits::default() };
    let mut document = policy_json(
        &workspace,
        std::slice::from_ref(&grant),
        key.verifying_key().to_bytes(),
        limits,
    );
    let object = document.as_object_mut().ok_or(FixtureError)?;
    object.insert(
        "scope".to_owned(),
        json!({"id": scope_id, "kind": scope_kind.as_str()}),
    );
    object.insert(
        "trustedIssuers".to_owned(),
        json!({"fixture-issuer": hex::encode(key.verifying_key().to_bytes())}),
    );
    object.insert("grants".to_owned(), json!([GrantRecord::from(&grant)]));
    fs::write(&policy_path, serde_json::to_vec(&document).map_err(|_| FixtureError)?)
        .map_err(|_| FixtureError)?;
    let authorization = data_dir.join("authorization.json");
    fs::write(
        &authorization,
        serde_json::to_vec(&authorization_json(&claim, &caller_endpoint))
            .map_err(|_| FixtureError)?,
    )
    .map_err(|_| FixtureError)?;
    let fake_codex = data_dir.join("fake-codex");
    write_executable(&fake_codex, fake_codex_script(false, false))
        .map_err(|_| FixtureError)?;
    serde_json::to_string(&json!({
        "serverDataDir": server_data,
        "clientDataDir": client_data,
        "workspace": workspace,
        "policy": policy_path,
        "authorization": authorization,
        "codexBinary": fake_codex,
    }))
    .map_err(|_| FixtureError)
}

#[must_use]
pub fn policy_json(
    workspace: &Path,
    grants: &[aether_core::Grant],
    trusted_key: [u8; 32],
    limits: PolicyLimits,
) -> serde_json::Value {
    json!({
        "trustedIssuers": {"issuer": hex::encode(trusted_key)},
        "grants": grants.iter().map(GrantRecord::from).collect::<Vec<_>>(),
        "workspace": workspace,
        "maxContextTokens": limits.max_context_tokens,
        "maxSeconds": limits.max_seconds,
        "queueGlobal": limits.queue_global,
        "queuePerPeer": limits.queue_per_peer,
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

#[must_use]
pub const fn fake_codex_script(slow_turn: bool, mcp_probe: bool) -> &'static str {
    if mcp_probe {
        r#"#!/bin/sh
names=''
while IFS= read -r line; do
  case "$line" in
    '[mcp_servers.'*']')
      name=${line#'[mcp_servers.'}
      name=${name%']'}
      names="$names$name "
      ;;
  esac
done < "$CODEX_HOME/config.toml"
while IFS= read -r request; do
  case "$request" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' ;;
    *'"id":2'*)
      case "$request" in
        *'"approvalPolicy":"never"'*) ;;
        *) exit 1 ;;
      esac
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thread"}}}'
      ;;
    *'"id":3'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn"}}}'
      printf '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","delta":"mcp: %s"}}\n' "$names"
      printf '%s\n' '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}}'
      ;;
  esac
done
"#
    } else if slow_turn {
        // A node starts Codex with an empty environment. Where `sleep` is
        // not on the shell's default path (NixOS), the turn would end at once.
        "#!/bin/sh\ncommand -v sleep >/dev/null 2>&1 || PATH=/usr/bin:/bin:/run/current-system/sw/bin\nwhile IFS= read -r request; do\n  case \"$request\" in\n    *'\"id\":1'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' ;;\n    *'\"id\":2'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"thread\":{\"id\":\"thread\"}}}' ;;\n    *'\"id\":3'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"turn\":{\"id\":\"turn\"}}}'\n      sleep 1\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread\",\"turnId\":\"turn\",\"delta\":\"fake Codex result\"}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread\",\"turn\":{\"id\":\"turn\",\"status\":\"completed\",\"items\":[]}}}'\n      ;;\n  esac\ndone\n"
    } else {
        "#!/bin/sh\nwhile IFS= read -r request; do\n  case \"$request\" in\n    *'\"id\":1'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' ;;\n    *'\"id\":2'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"thread\":{\"id\":\"thread\"}}}' ;;\n    *'\"id\":3'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"turn\":{\"id\":\"turn\"}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread\",\"turnId\":\"turn\",\"delta\":\"fake Codex result\"}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread\",\"turn\":{\"id\":\"turn\",\"status\":\"completed\",\"items\":[]}}}'\n      ;;\n  esac\ndone\n"
    }
}

#[cfg(unix)]
pub fn write_executable(path: &Path, script: &str) -> Result<(), FixtureError> {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, script).map_err(|_| FixtureError)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| FixtureError)
}

#[cfg(not(unix))]
pub fn write_executable(_path: &Path, _script: &str) -> Result<(), FixtureError> {
    Err(FixtureError)
}
