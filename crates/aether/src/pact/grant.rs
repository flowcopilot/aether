use std::{collections::BTreeSet, path::Path, time::SystemTime};

use aether_core::time::from_unix_seconds;
use aether_core::{
    AgentId, ApprovalRule, CapabilityId, EndpointId, Grant, GrantId, ScopeId,
    WorkspaceId,
};
use aether_store::pact::PactGrant;

use super::{
    config::{BRAND_USER, Brand, SCOPE_ID},
    jwks::PaIdentity,
};

pub const GRANT_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum GrantSignError {
    #[error("the grant cannot be signed")]
    Sign,
}

#[must_use]
pub fn caller_agent_id(issuer: &str) -> String {
    format!("pact:{issuer}")
}

#[must_use]
pub fn caller_endpoint_id(caller: &PaIdentity) -> String {
    let digest = blake3::hash(caller.subject.as_bytes()).to_hex();
    format!("pact:{}#{}", caller.issuer, digest.get(..32).unwrap_or_default())
}

#[must_use]
pub fn pact_grant_record(
    grant_id: &str,
    brand_id: &str,
    pa_issuer: &str,
    pa_subject: &str,
    scopes: &str,
    at: i64,
) -> PactGrant {
    PactGrant {
        grant_id: grant_id.to_owned(),
        brand_id: brand_id.to_owned(),
        pa_issuer: pa_issuer.to_owned(),
        pa_subject: pa_subject.to_owned(),
        brand_user: BRAND_USER.to_owned(),
        scopes: scopes.to_owned(),
        created_at: at,
        expires_at: at.saturating_add(GRANT_SECONDS),
    }
}

#[derive(Clone, Copy)]
pub enum GrantBinding<'a> {
    Redeem,
    Delegation { brand_user: &'a str },
}

#[must_use]
pub fn grant_binds(
    grant: &PactGrant,
    brand_id: &str,
    caller: &PaIdentity,
    at: i64,
    binding: GrantBinding<'_>,
    live_in_policy: bool,
) -> bool {
    if grant.brand_id != brand_id
        || grant.pa_issuer != caller.issuer
        || grant.pa_subject != caller.subject
        || grant.expires_at <= at
        || !live_in_policy
    {
        return false;
    }
    match binding {
        GrantBinding::Redeem => true,
        GrantBinding::Delegation { brand_user } => grant.brand_user == brand_user,
    }
}

pub fn sign_aether_grant(
    data_dir: &Path,
    brand: &Brand,
    caller: &PaIdentity,
    at: i64,
) -> Result<Grant, GrantSignError> {
    let issuer = crate::data_dir::DataDir::new(data_dir)
        .issuer_key()
        .map_err(|_| GrantSignError::Sign)?;
    let issued_secs = u64::try_from(at.saturating_sub(1)).unwrap_or(0);
    let issued_at = from_unix_seconds(issued_secs).unwrap_or(SystemTime::UNIX_EPOCH);
    let expires_secs = u64::try_from(at.saturating_add(GRANT_SECONDS))
        .map_err(|_| GrantSignError::Sign)?;
    let expires_at = from_unix_seconds(expires_secs).ok_or(GrantSignError::Sign)?;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| GrantSignError::Sign)?;
    Ok(crate::identity::signed_grant(
        &issuer,
        crate::identity::GrantInput {
            id: GrantId::new(format!("pact_{}", uuid::Uuid::new_v4().simple())),
            issuer_agent_id: AgentId::new(crate::identity::NODE_ISSUER),
            subject_agent_id: AgentId::new(caller_agent_id(&caller.issuer)),
            subject_endpoint_id: EndpointId::new(caller_endpoint_id(caller)),
            scope_id: ScopeId::new(SCOPE_ID),
            capabilities: BTreeSet::from([CapabilityId::new(&brand.capability)]),
            workspaces: BTreeSet::from([WorkspaceId::new(&brand.workspace)]),
            access_mode: brand.access_mode.clone(),
            approval_rule: ApprovalRule::Automatic,
            issued_at,
            expires_at,
            nonce,
        },
    ))
}
