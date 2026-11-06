#![expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]

use std::{
    collections::BTreeSet,
    time::{Duration, SystemTime},
};

use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId, Grant,
    GrantId, ProviderError, Scope, ScopeId, TrustProvider, WorkspaceId,
};
use ed25519_dalek::SigningKey;

use crate::identity::{ClaimInput, GrantInput, signed_claim, signed_grant};

pub struct Trusted;

impl TrustProvider for Trusted {
    fn verify(
        &self,
        _claim: &Claim,
        _authenticated_endpoint: &EndpointId,
        _scope: &Scope,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    fn verify_grant(
        &self,
        _grant: &Grant,
        _scope: &Scope,
    ) -> Result<(), ProviderError> {
        Ok(())
    }
}

pub fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

pub fn claim_input(adjust: impl FnOnce(&mut ClaimInput)) -> ClaimInput {
    let now = SystemTime::now();
    let mut input = ClaimInput {
        id: ClaimId::new("claim"),
        issuer_agent_id: AgentId::new("issuer"),
        subject_agent_id: AgentId::new("subject"),
        subject_endpoint_id: EndpointId::new("endpoint"),
        scope_id: ScopeId::new("direct"),
        issued_at: now - Duration::from_secs(1),
        expires_at: now + Duration::from_secs(60),
        nonce: [1; 32],
    };
    adjust(&mut input);
    input
}

pub fn grant_input(adjust: impl FnOnce(&mut GrantInput)) -> GrantInput {
    let now = SystemTime::now();
    let mut input = GrantInput {
        id: GrantId::new("grant"),
        issuer_agent_id: AgentId::new("issuer"),
        subject_agent_id: AgentId::new("subject"),
        subject_endpoint_id: EndpointId::new("endpoint"),
        scope_id: ScopeId::new("direct"),
        capabilities: BTreeSet::from([CapabilityId::new("review")]),
        workspaces: BTreeSet::from([WorkspaceId::new("workspace")]),
        access_mode: AccessMode::ReadOnly,
        approval_rule: ApprovalRule::Automatic,
        issued_at: now - Duration::from_secs(1),
        expires_at: now + Duration::from_secs(60),
        nonce: [2; 32],
    };
    adjust(&mut input);
    input
}

pub fn signed_test_claim(
    key: &SigningKey,
    adjust: impl FnOnce(&mut ClaimInput),
) -> Claim {
    signed_claim(key, claim_input(adjust))
}

pub fn signed_test_grant(
    key: &SigningKey,
    adjust: impl FnOnce(&mut GrantInput),
) -> Grant {
    signed_grant(key, grant_input(adjust))
}
