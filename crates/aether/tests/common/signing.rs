use std::{
    collections::BTreeSet,
    time::SystemTime,
};

use aether::identity::{ClaimInput, GrantInput, signed_claim, signed_grant};
use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId, Grant,
    GrantId, ScopeId, WorkspaceId,
};
use ed25519_dalek::SigningKey;

pub fn host_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

pub fn proof_issuer_key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

pub fn discovery_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

pub fn review_claim(
    key: &SigningKey,
    id: &str,
    issuer: &str,
    subject: &str,
    endpoint: &EndpointId,
    scope: &str,
    issued_at: SystemTime,
    expires_at: SystemTime,
    nonce: [u8; 32],
) -> Claim {
    signed_claim(
        key,
        ClaimInput {
            id: ClaimId::new(id),
            issuer_agent_id: AgentId::new(issuer),
            subject_agent_id: AgentId::new(subject),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new(scope),
            issued_at,
            expires_at,
            nonce,
        },
    )
}

pub fn review_grant(
    key: &SigningKey,
    id: &str,
    issuer: &str,
    subject: &str,
    endpoint: &EndpointId,
    scope: &str,
    issued_at: SystemTime,
    expires_at: SystemTime,
    nonce: [u8; 32],
) -> Grant {
    signed_grant(
        key,
        GrantInput {
            id: GrantId::new(id),
            issuer_agent_id: AgentId::new(issuer),
            subject_agent_id: AgentId::new(subject),
            subject_endpoint_id: endpoint.clone(),
            scope_id: ScopeId::new(scope),
            capabilities: BTreeSet::from([CapabilityId::new("review")]),
            workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at,
            expires_at,
            nonce,
        },
    )
}
