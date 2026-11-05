use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Duration, SystemTime},
};

use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId, Grant,
    GrantId, ProviderError, Scope, ScopeId, ScopeKind, TrustProvider, WorkspaceId,
};
use aether_store::NodeStore;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub struct SignedClaims {
    trusted_keys: BTreeMap<AgentId, [u8; 32]>,
    revoked_keys: BTreeSet<[u8; 32]>,
    clock_skew: Duration,
}

pub struct RosterClaims {
    signed_claims: SignedClaims,
    database: PathBuf,
}

pub struct ClaimInput {
    pub id: ClaimId,
    pub issuer_agent_id: AgentId,
    pub subject_agent_id: AgentId,
    pub subject_endpoint_id: EndpointId,
    pub scope_id: ScopeId,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub nonce: [u8; 32],
}

pub struct GrantInput {
    pub id: GrantId,
    pub issuer_agent_id: AgentId,
    pub subject_agent_id: AgentId,
    pub subject_endpoint_id: EndpointId,
    pub scope_id: ScopeId,
    pub capabilities: BTreeSet<CapabilityId>,
    pub workspaces: BTreeSet<WorkspaceId>,
    pub access_mode: AccessMode,
    pub approval_rule: ApprovalRule,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub nonce: [u8; 32],
}

impl SignedClaims {
    #[must_use]
    pub const fn new(
        trusted_keys: BTreeMap<AgentId, [u8; 32]>,
        clock_skew: Duration,
    ) -> Self {
        Self { trusted_keys, revoked_keys: BTreeSet::new(), clock_skew }
    }

    pub fn revoke(&mut self, key: [u8; 32]) {
        self.revoked_keys.insert(key);
    }
}

impl RosterClaims {
    #[must_use]
    pub const fn new(signed_claims: SignedClaims, database: PathBuf) -> Self {
        Self { signed_claims, database }
    }
}

impl TrustProvider for SignedClaims {
    fn verify(
        &self,
        claim: &Claim,
        authenticated_endpoint: &EndpointId,
        scope: &Scope,
    ) -> Result<(), ProviderError> {
        let now = SystemTime::now();
        // A skew too large for the clock accepts any issue time.
        let latest_issue = now.checked_add(self.clock_skew);
        if claim.subject_endpoint_id != *authenticated_endpoint
            || claim.scope_id != scope.id
            || latest_issue.is_some_and(|latest| claim.issued_at > latest)
            || claim.expires_at <= now
            || claim.issued_at >= claim.expires_at
            || claim.nonce == [0; 32]
            || self.revoked_keys.contains(&claim.signing_key)
            || self.trusted_keys.get(&claim.issuer_agent_id) != Some(&claim.signing_key)
        {
            return Err(ProviderError::Invalid("claim is not trusted".to_owned()));
        }
        let key = VerifyingKey::from_bytes(&claim.signing_key)
            .map_err(|_| ProviderError::Invalid("claim key is invalid".to_owned()))?;
        let signature =
            Signature::try_from(claim.signature.as_slice()).map_err(|_| {
                ProviderError::Invalid("claim signature is invalid".to_owned())
            })?;
        key.verify_strict(&claim_bytes(claim), &signature).map_err(|_| {
            ProviderError::Invalid("claim signature is invalid".to_owned())
        })
    }

    fn verify_grant(&self, grant: &Grant, scope: &Scope) -> Result<(), ProviderError> {
        let now = SystemTime::now();
        let latest_issue = now.checked_add(self.clock_skew);
        if grant.scope_id != scope.id
            || latest_issue.is_some_and(|latest| grant.issued_at > latest)
            || grant.expires_at <= now
            || grant.issued_at >= grant.expires_at
            || grant.nonce == [0; 32]
            || self.revoked_keys.contains(&grant.signing_key)
            || self.trusted_keys.get(&grant.issuer_agent_id) != Some(&grant.signing_key)
        {
            return Err(ProviderError::Invalid("grant is not trusted".to_owned()));
        }
        let key = VerifyingKey::from_bytes(&grant.signing_key)
            .map_err(|_| ProviderError::Invalid("grant key is invalid".to_owned()))?;
        let signature =
            Signature::try_from(grant.signature.as_slice()).map_err(|_| {
                ProviderError::Invalid("grant signature is invalid".to_owned())
            })?;
        key.verify_strict(&grant_bytes(grant), &signature).map_err(|_| {
            ProviderError::Invalid("grant signature is invalid".to_owned())
        })
    }
}

impl TrustProvider for RosterClaims {
    fn verify(
        &self,
        claim: &Claim,
        authenticated_endpoint: &EndpointId,
        scope: &Scope,
    ) -> Result<(), ProviderError> {
        self.signed_claims.verify(claim, authenticated_endpoint, scope)?;
        if scope.kind != ScopeKind::Managed {
            return Ok(());
        }
        let store =
            NodeStore::open(&self.database).map_err(|_| ProviderError::Unavailable)?;
        let now = aether_core::time::unix_timestamp(SystemTime::now())
            .ok_or(ProviderError::Unavailable)?;
        let active = store
            .managed_members(scope.id.as_str())
            .map_err(|_| ProviderError::Unavailable)?
            .into_iter()
            .any(|member| {
                member.agent_id == claim.subject_agent_id.as_str()
                    && member.endpoint_id == authenticated_endpoint.as_str()
                    && member.expires_at > now
                    && member.revoked_at.is_none()
            });
        if active {
            Ok(())
        } else {
            Err(ProviderError::Invalid("managed member is not active".to_owned()))
        }
    }

    fn verify_grant(&self, grant: &Grant, scope: &Scope) -> Result<(), ProviderError> {
        self.signed_claims.verify_grant(grant, scope)
    }
}

#[must_use]
pub fn signed_claim(signing_key: &SigningKey, input: ClaimInput) -> Claim {
    let mut claim = Claim {
        id: input.id,
        issuer_agent_id: input.issuer_agent_id,
        subject_agent_id: input.subject_agent_id,
        subject_endpoint_id: input.subject_endpoint_id,
        scope_id: input.scope_id,
        issued_at: input.issued_at,
        expires_at: input.expires_at,
        signing_key: signing_key.verifying_key().to_bytes(),
        nonce: input.nonce,
        signature: Vec::new(),
    };
    claim.signature = signing_key.sign(&claim_bytes(&claim)).to_bytes().to_vec();
    claim
}

#[must_use]
pub fn signed_grant(signing_key: &SigningKey, input: GrantInput) -> Grant {
    let mut grant = Grant {
        id: input.id,
        issuer_agent_id: input.issuer_agent_id,
        subject_agent_id: input.subject_agent_id,
        subject_endpoint_id: input.subject_endpoint_id,
        scope_id: input.scope_id,
        capabilities: input.capabilities,
        workspaces: input.workspaces,
        access_mode: input.access_mode,
        approval_rule: input.approval_rule,
        issued_at: input.issued_at,
        expires_at: input.expires_at,
        signing_key: signing_key.verifying_key().to_bytes(),
        nonce: input.nonce,
        signature: Vec::new(),
    };
    grant.signature = signing_key.sign(&grant_bytes(&grant)).to_bytes().to_vec();
    grant
}

/// What a host gives one caller: who may call, from which endpoint, and
/// with what access, for how long.
pub struct AccessInput {
    pub issuer_agent_id: String,
    pub subject_agent_id: String,
    pub subject_endpoint_id: String,
    pub scope_id: String,
    pub capabilities: Vec<String>,
    pub workspaces: Vec<String>,
    pub access_mode: AccessMode,
    pub approval_rule: ApprovalRule,
    pub lifetime: Duration,
}

/// Why [`sign_access`] cannot sign.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum SignError {
    #[error("the system cannot give random bytes for a nonce")]
    Randomness,
    #[error("the access lifetime is too long for the clock")]
    Lifetime,
}

/// Signs a claim and a grant for one caller.
///
/// The caller sends the returned authorization with each request; the
/// host's node needs the grant. The authorization names the first
/// capability and workspace of `input`.
pub fn sign_access(
    signing_key: &SigningKey,
    input: &AccessInput,
) -> Result<(crate::transport::A2aAuthorization, Grant), SignError> {
    let now = SystemTime::now();
    // One second back, so a peer whose clock is slightly behind does not
    // see a claim issued in the future.
    let issued_at = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
    let expires_at = now.checked_add(input.lifetime).ok_or(SignError::Lifetime)?;
    let issuer = AgentId::new(input.issuer_agent_id.clone());
    let subject = AgentId::new(input.subject_agent_id.clone());
    let subject_endpoint = EndpointId::new(input.subject_endpoint_id.clone());
    let scope_id = ScopeId::new(input.scope_id.clone());
    let claim = signed_claim(
        signing_key,
        ClaimInput {
            id: ClaimId::new(uuid::Uuid::new_v4().to_string()),
            issuer_agent_id: issuer.clone(),
            subject_agent_id: subject.clone(),
            subject_endpoint_id: subject_endpoint.clone(),
            scope_id: scope_id.clone(),
            issued_at,
            expires_at,
            nonce: random_nonce()?,
        },
    );
    let grant = signed_grant(
        signing_key,
        GrantInput {
            id: GrantId::new(uuid::Uuid::new_v4().to_string()),
            issuer_agent_id: issuer,
            subject_agent_id: subject,
            subject_endpoint_id: subject_endpoint,
            scope_id,
            capabilities: input
                .capabilities
                .iter()
                .cloned()
                .map(CapabilityId::new)
                .collect(),
            workspaces: input
                .workspaces
                .iter()
                .cloned()
                .map(WorkspaceId::new)
                .collect(),
            access_mode: input.access_mode.clone(),
            approval_rule: input.approval_rule,
            issued_at,
            expires_at,
            nonce: random_nonce()?,
        },
    );
    let authorization = crate::transport::A2aAuthorization {
        request_id: "pending".to_owned(),
        caller_agent_id: input.subject_agent_id.clone(),
        caller_endpoint_id: input.subject_endpoint_id.clone(),
        capability_id: input.capabilities.first().cloned().unwrap_or_default(),
        workspace_id: input.workspaces.first().cloned().unwrap_or_default(),
        access_mode: match input.access_mode {
            AccessMode::ReadOnly => crate::transport::A2aAccessMode::ReadOnly,
            AccessMode::ReadWrite => crate::transport::A2aAccessMode::ReadWrite,
        },
        claim: (&claim).into(),
    };
    Ok((authorization, grant))
}

fn random_nonce() -> Result<[u8; 32], SignError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| SignError::Randomness)?;
    Ok(nonce)
}

fn claim_bytes(claim: &Claim) -> Vec<u8> {
    let mut bytes = b"aether/claim/v1".to_vec();
    append_string(&mut bytes, claim.id.as_str());
    append_string(&mut bytes, claim.issuer_agent_id.as_str());
    append_string(&mut bytes, claim.subject_agent_id.as_str());
    append_string(&mut bytes, claim.subject_endpoint_id.as_str());
    append_string(&mut bytes, claim.scope_id.as_str());
    append_time(&mut bytes, claim.issued_at);
    append_time(&mut bytes, claim.expires_at);
    bytes.extend_from_slice(&claim.signing_key);
    bytes.extend_from_slice(&claim.nonce);
    bytes
}

fn grant_bytes(grant: &Grant) -> Vec<u8> {
    let mut bytes = b"aether/grant/v1".to_vec();
    append_string(&mut bytes, grant.id.as_str());
    append_string(&mut bytes, grant.issuer_agent_id.as_str());
    append_string(&mut bytes, grant.subject_agent_id.as_str());
    append_string(&mut bytes, grant.subject_endpoint_id.as_str());
    append_string(&mut bytes, grant.scope_id.as_str());
    append_strings(&mut bytes, grant.capabilities.iter().map(CapabilityId::as_str));
    append_strings(&mut bytes, grant.workspaces.iter().map(WorkspaceId::as_str));
    bytes.push(match grant.access_mode {
        AccessMode::ReadOnly => 0,
        AccessMode::ReadWrite => 1,
    });
    bytes.push(match grant.approval_rule {
        ApprovalRule::Automatic => 0,
        ApprovalRule::LocalUi => 1,
    });
    append_time(&mut bytes, grant.issued_at);
    append_time(&mut bytes, grant.expires_at);
    bytes.extend_from_slice(&grant.signing_key);
    bytes.extend_from_slice(&grant.nonce);
    bytes
}

fn append_string(bytes: &mut Vec<u8>, value: &str) {
    append_length(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}

/// A length as a big-endian `u64`, which holds every `usize` on the
/// platforms Rust supports; the saturation only keeps the conversion total.
fn append_length(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&u64::try_from(length).unwrap_or(u64::MAX).to_be_bytes());
}

fn append_strings<'a>(bytes: &mut Vec<u8>, values: impl Iterator<Item = &'a str>) {
    let values = values.collect::<Vec<_>>();
    append_length(bytes, values.len());
    for value in values {
        append_string(bytes, value);
    }
}

fn append_time(bytes: &mut Vec<u8>, value: SystemTime) {
    let seconds = aether_core::time::unix_seconds(value).unwrap_or(0);
    bytes.extend_from_slice(&seconds.to_be_bytes());
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use super::{
        ClaimInput, GrantInput, RosterClaims, SignedClaims, signed_claim, signed_grant,
    };
    use aether_core::{
        AccessMode, AgentId, ApprovalRule, CapabilityId, ClaimId, EndpointId, GrantId,
        Scope, ScopeId, ScopeKind, TrustProvider, WorkspaceId,
    };
    use aether_store::{ManagedMember, NodeStore};
    use ed25519_dalek::SigningKey;
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::{Duration, SystemTime},
    };

    fn claim(key: &SigningKey) -> aether_core::Claim {
        let now = SystemTime::now();
        signed_claim(
            key,
            ClaimInput {
                id: ClaimId::new("claim"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("subject"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [1; 32],
            },
        )
    }

    fn provider(key: &SigningKey) -> SignedClaims {
        SignedClaims::new(
            BTreeMap::from([(AgentId::new("issuer"), key.verifying_key().to_bytes())]),
            Duration::from_secs(5),
        )
    }

    fn grant(key: &SigningKey) -> aether_core::Grant {
        let now = SystemTime::now();
        signed_grant(
            key,
            GrantInput {
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
            },
        )
    }

    fn verify(provider: &SignedClaims, claim: &aether_core::Claim) -> bool {
        provider
            .verify(
                claim,
                &EndpointId::new("endpoint"),
                &Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .is_ok()
    }

    fn verify_grant(provider: &SignedClaims, grant: &aether_core::Grant) -> bool {
        provider
            .verify_grant(
                grant,
                &Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct },
            )
            .is_ok()
    }

    #[test]
    fn signed_claims_reject_forged_changed_wrong_expired_future_revoked_and_cross_scope_claims()
     {
        let key = SigningKey::from_bytes(&[3; 32]);
        let valid = claim(&key);
        assert!(verify(&provider(&key), &valid));
        let forged = signed_claim(
            &SigningKey::from_bytes(&[4; 32]),
            ClaimInput {
                id: ClaimId::new("forged"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("subject"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                issued_at: SystemTime::now() - Duration::from_secs(1),
                expires_at: SystemTime::now() + Duration::from_secs(60),
                nonce: [4; 32],
            },
        );
        assert!(!verify(&provider(&key), &forged));
        let mut changed = valid.clone();
        changed.subject_agent_id = AgentId::new("other");
        assert!(!verify(&provider(&key), &changed));
        assert!(!verify(&SignedClaims::new(BTreeMap::new(), Duration::ZERO), &valid));
        let expired = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("expired"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("subject"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                issued_at: SystemTime::now() - Duration::from_secs(120),
                expires_at: SystemTime::now() - Duration::from_secs(60),
                nonce: [5; 32],
            },
        );
        assert!(!verify(&provider(&key), &expired));
        let future = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("future"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("subject"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("direct"),
                issued_at: SystemTime::now() + Duration::from_secs(60),
                expires_at: SystemTime::now() + Duration::from_secs(120),
                nonce: [2; 32],
            },
        );
        assert!(!verify(&provider(&key), &future));
        assert!(
            provider(&key)
                .verify(
                    &valid,
                    &EndpointId::new("endpoint"),
                    &Scope { id: ScopeId::new("other"), kind: ScopeKind::Direct },
                )
                .is_err()
        );
        let mut revoked = provider(&key);
        revoked.revoke(key.verifying_key().to_bytes());
        assert!(!verify(&revoked, &valid));
    }

    #[test]
    fn signed_grants_reject_changed_unknown_and_revoked_grants() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let valid = grant(&key);
        assert!(verify_grant(&provider(&key), &valid));
        let mut changed = valid.clone();
        changed.access_mode = AccessMode::ReadWrite;
        assert!(!verify_grant(&provider(&key), &changed));
        assert!(!verify_grant(
            &SignedClaims::new(BTreeMap::new(), Duration::ZERO),
            &valid
        ));
        let mut revoked = provider(&key);
        revoked.revoke(key.verifying_key().to_bytes());
        assert!(!verify_grant(&revoked, &valid));
    }

    #[test]
    fn managed_claims_require_an_active_durable_roster_member() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join("node.sqlite");
        let mut store = NodeStore::open(&database).expect("store must open");
        store
            .apply_managed_member(
                "managed.cursor",
                1,
                &ManagedMember {
                    scope_id: "managed".to_owned(),
                    agent_id: "subject".to_owned(),
                    endpoint_id: "endpoint".to_owned(),
                    role: "member".to_owned(),
                    expires_at: i64::MAX,
                    revoked_at: None,
                },
            )
            .expect("member must store");
        let key = SigningKey::from_bytes(&[6; 32]);
        let now = SystemTime::now();
        let claim = signed_claim(
            &key,
            ClaimInput {
                id: ClaimId::new("managed-claim"),
                issuer_agent_id: AgentId::new("issuer"),
                subject_agent_id: AgentId::new("subject"),
                subject_endpoint_id: EndpointId::new("endpoint"),
                scope_id: ScopeId::new("managed"),
                issued_at: now - Duration::from_secs(1),
                expires_at: now + Duration::from_secs(60),
                nonce: [6; 32],
            },
        );
        let trust = RosterClaims::new(provider(&key), database);
        let scope = Scope { id: ScopeId::new("managed"), kind: ScopeKind::Managed };
        assert!(trust.verify(&claim, &EndpointId::new("endpoint"), &scope,).is_ok());
        store
            .revoke_managed_member("managed.cursor", 2, "managed", "subject")
            .expect("member must revoke");
        assert!(trust.verify(&claim, &EndpointId::new("endpoint"), &scope,).is_err());
    }
}
