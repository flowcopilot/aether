//! Signed claims, grants, discovery records and tasks for one caller.

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use aether::{
    discovery::{Invitation, Record, signed_invitation, signed_record},
    execution::Task,
    identity::SignedClaims,
};
use aether_core::{
    AccessMode, AgentId, CapabilityId, Claim, EndpointId, Grant, Request, RequestId,
    Scope, ScopeId, ScopeKind, TaskId, WorkspaceId,
};
use aether_store::{ManagedMember, TaskInput};

use crate::common::{
    discovery_signing_key, proof_issuer_key, review_claim, review_grant,
};

/// The caller's agent and endpoint in every scenario.
pub const CALLER: &str = "caller";
pub const CALLER_ENDPOINT: &str = "caller-endpoint";

/// The scope with the given kind name; its id is the same name.
pub fn scope(name: &str) -> Scope {
    let kind = match name {
        "direct" => ScopeKind::Direct,
        "directory" => ScopeKind::Directory,
        "managed" => ScopeKind::Managed,
        other => unreachable!("unknown scope {other}"),
    };
    Scope { id: ScopeId::new(name), kind }
}

/// The caller's request to review in the workspace, read only.
pub fn request(scope: &Scope) -> Request {
    Request {
        id: RequestId::new("request"),
        task_id: TaskId::new("task"),
        caller_agent_id: AgentId::new(CALLER),
        caller_endpoint_id: EndpointId::new(CALLER_ENDPOINT),
        scope_id: scope.id.clone(),
        capability_id: CapabilityId::new("review"),
        workspace_id: WorkspaceId::new("workspace"),
        access_mode: AccessMode::ReadOnly,
    }
}

/// A review task with untrusted text from the caller.
pub fn task(id: &str) -> Task {
    Task {
        id: id.to_owned(),
        skill: "review".to_owned(),
        workspace_id: "workspace".to_owned(),
        remote_text: "untrusted task text".to_owned(),
    }
}

fn ago(seconds: u64) -> SystemTime {
    SystemTime::now()
        .checked_sub(Duration::from_secs(seconds))
        .expect("the clock must be after the epoch")
}

/// A claim from the issuer that the caller belongs to the scope.
pub fn claim(scope: &Scope) -> Claim {
    let now = SystemTime::now();
    review_claim(
        &proof_issuer_key(),
        "signed-claim",
        "issuer",
        CALLER,
        &EndpointId::new(CALLER_ENDPOINT),
        scope.id.as_str(),
        ago(1),
        now + Duration::from_secs(60),
        [3; 32],
    )
}

/// The same claim, expired one second ago.
pub fn expired_claim(scope: &Scope) -> Claim {
    let mut claim = claim(scope);
    claim.expires_at = ago(1);
    claim
}

/// The trust provider that knows the issuer's key.
pub fn trust_provider() -> SignedClaims {
    SignedClaims::new(
        BTreeMap::from([(
            AgentId::new("issuer"),
            proof_issuer_key().verifying_key().to_bytes(),
        )]),
        Duration::from_secs(5),
    )
}

/// A local grant that lets the caller review the workspace, read only.
pub fn grant(scope: &Scope) -> Grant {
    let now = SystemTime::now();
    review_grant(
        &proof_issuer_key(),
        "grant",
        "issuer",
        CALLER,
        &EndpointId::new(CALLER_ENDPOINT),
        scope.id.as_str(),
        ago(1),
        now + Duration::from_secs(60),
        [4; 32],
    )
}

pub fn expires_soon() -> SystemTime {
    SystemTime::now() + Duration::from_secs(60)
}

pub fn invitation(scope_id: &str, agent_id: &str, endpoint_id: &str) -> Invitation {
    signed_invitation(
        &discovery_signing_key(),
        scope_id.to_owned(),
        agent_id.to_owned(),
        endpoint_id.to_owned(),
        expires_soon(),
    )
}

pub fn record(
    scope_id: &str,
    agent_id: &str,
    endpoint_id: &str,
    address: &str,
) -> Record {
    signed_record(
        &discovery_signing_key(),
        agent_id.to_owned(),
        endpoint_id.to_owned(),
        vec![address.to_owned()],
        scope_id.to_owned(),
        vec!["review".to_owned()],
        expires_soon(),
    )
}

pub fn managed_member(
    agent_id: &str,
    endpoint_id: &str,
    scope_id: &str,
) -> ManagedMember {
    ManagedMember {
        scope_id: scope_id.to_owned(),
        agent_id: agent_id.to_owned(),
        endpoint_id: endpoint_id.to_owned(),
        role: "member".to_owned(),
        expires_at: aether_core::time::unix_timestamp(expires_soon())
            .expect("managed membership expiry must be valid"),
        revoked_at: None,
    }
}

/// A stored task from the caller in the direct scope.
pub fn task_input(id: &str, request_id: &str, idempotency_key: &str) -> TaskInput {
    TaskInput {
        request: aether_core::Request {
            task_id: aether_core::TaskId::new(id),
            id: aether_core::RequestId::new(request_id),
            caller_agent_id: aether_core::AgentId::new(CALLER),
            caller_endpoint_id: aether_core::EndpointId::new(CALLER_ENDPOINT),
            scope_id: aether_core::ScopeId::new("direct"),
            capability_id: aether_core::CapabilityId::new("review"),
            workspace_id: aether_core::WorkspaceId::new("workspace"),
            access_mode: aether_core::AccessMode::ReadOnly,
        },
        idempotency_key: idempotency_key.to_owned(),
        request_fingerprint: "fingerprint".to_owned(),
        skill: "review".to_owned(),
        remote_text: "untrusted task text".to_owned(),
    }
}

/// Splits `"a, b, c"` into its names.
pub fn names(list: &str) -> Vec<String> {
    list.split(',').map(|name| name.trim().to_owned()).collect()
}
