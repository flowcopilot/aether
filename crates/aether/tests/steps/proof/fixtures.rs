//! Signed claims, grants, discovery records and tasks for one caller.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime},
};

use aether::{
    discovery::{
        DiscoveryError, Invitation, Record, signed_invitation, signed_record,
        validate_invitation, validate_record,
    },
    execution::Task,
    identity::{ClaimInput, GrantInput, SignedClaims, signed_claim, signed_grant},
};
use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId, Grant,
    GrantId, Request, RequestId, Scope, ScopeId, ScopeKind, TaskId, WorkspaceId,
};
use aether_store::{ManagedMember, TaskInput};
use ed25519_dalek::SigningKey;

/// The caller's agent and endpoint in every scenario.
pub const CALLER: &str = "caller";
pub const CALLER_ENDPOINT: &str = "caller-endpoint";

/// Runs blocking work off the task that polls the scenarios.
pub async fn blocking<R, F>(work: F) -> R
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.expect("blocking step must finish")
}

/// Direct invitations kept in memory. Each one passes the node's signature
/// check when it is imported and when it is resolved.
#[derive(Debug, Default)]
pub struct DirectContacts {
    contacts: BTreeMap<String, Invitation>,
}

impl DirectContacts {
    pub fn import(&mut self, invitation: Invitation) -> Result<(), DiscoveryError> {
        validate_invitation(&invitation)?;
        self.contacts.insert(invitation.agent_id.clone(), invitation);
        Ok(())
    }

    pub fn resolve(&self, agent_id: &str) -> Result<&Invitation, DiscoveryError> {
        let invitation = self.contacts.get(agent_id).ok_or(DiscoveryError::NotFound)?;
        validate_invitation(invitation)?;
        Ok(invitation)
    }
}

/// A directory kept in memory. Each record passes the node's signature
/// check when it is published and when it is resolved.
#[derive(Debug, Default)]
pub struct Directory {
    records: BTreeMap<String, Record>,
}

impl Directory {
    pub fn publish(&mut self, record: Record) -> Result<(), DiscoveryError> {
        validate_record(&record)?;
        self.records.insert(record.agent_id.clone(), record);
        Ok(())
    }

    pub fn resolve(&self, agent_id: &str) -> Result<&Record, DiscoveryError> {
        let record = self.records.get(agent_id).ok_or(DiscoveryError::NotFound)?;
        validate_record(record)?;
        Ok(record)
    }
}

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
        context_tokens: 1,
    }
}

fn issuer_key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

fn ago(seconds: u64) -> SystemTime {
    SystemTime::now()
        .checked_sub(Duration::from_secs(seconds))
        .expect("the clock must be after the epoch")
}

/// A claim from the issuer that the caller belongs to the scope.
pub fn claim(scope: &Scope) -> Claim {
    let now = SystemTime::now();
    signed_claim(
        &issuer_key(),
        ClaimInput {
            id: ClaimId::new("signed-claim"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new(CALLER),
            subject_endpoint_id: EndpointId::new(CALLER_ENDPOINT),
            scope_id: scope.id.clone(),
            issued_at: ago(1),
            expires_at: now + Duration::from_secs(60),
            nonce: [3; 32],
        },
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
            issuer_key().verifying_key().to_bytes(),
        )]),
        Duration::from_secs(5),
    )
}

/// A local grant that lets the caller review the workspace, read only.
pub fn grant(scope: &Scope) -> Grant {
    let now = SystemTime::now();
    signed_grant(
        &issuer_key(),
        GrantInput {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: AgentId::new(CALLER),
            subject_endpoint_id: EndpointId::new(CALLER_ENDPOINT),
            scope_id: scope.id.clone(),
            capabilities: BTreeSet::from([CapabilityId::new("review")]),
            workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at: ago(1),
            expires_at: now + Duration::from_secs(60),
            nonce: [4; 32],
        },
    )
}

fn discovery_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

pub fn expires_soon() -> SystemTime {
    SystemTime::now() + Duration::from_secs(60)
}

pub fn invitation(scope_id: &str, agent_id: &str, endpoint_id: &str) -> Invitation {
    signed_invitation(
        &discovery_key(),
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
        &discovery_key(),
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
        id: id.to_owned(),
        request_id: request_id.to_owned(),
        idempotency_key: idempotency_key.to_owned(),
        caller_agent_id: CALLER.to_owned(),
        caller_endpoint_id: CALLER_ENDPOINT.to_owned(),
        request_fingerprint: "fingerprint".to_owned(),
        scope_id: "direct".to_owned(),
        capability_id: "review".to_owned(),
        workspace_id: "workspace".to_owned(),
        access_mode: "read_only".to_owned(),
        skill: "review".to_owned(),
        remote_text: "untrusted task text".to_owned(),
    }
}

/// Splits `"a, b, c"` into its names.
pub fn names(list: &str) -> Vec<String> {
    list.split(',').map(|name| name.trim().to_owned()).collect()
}
