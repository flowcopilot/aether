use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime},
};

use aether_core::{
    AccessMode, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId, EndpointId, Grant,
    GrantId, ProviderError, Scope, ScopeId, ScopeKind, TrustProvider, WorkspaceId,
};
use ed25519_dalek::SigningKey;

use crate::signed::{SignedBytes, sign, verify};

/// The issuer name of the node's own signing key, which the node always
/// trusts. The admin UI signs a friend's access with it.
pub const NODE_ISSUER: &str = "node";

pub struct SignedClaims {
    trusted_keys: BTreeMap<AgentId, [u8; 32]>,
    clock_skew: Duration,
}

pub struct RosterClaims {
    signed_claims: SignedClaims,
    data_dir: crate::data_dir::DataDir,
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
        Self { trusted_keys, clock_skew }
    }

    /// Whether a claim or grant of `scope` with these fields is in force
    /// now and comes from the trusted key of its issuer.
    fn trusts(&self, signed: &Signed<'_>, scope: &Scope) -> bool {
        let now = SystemTime::now();
        // A skew too large for the clock accepts any issue time.
        let latest_issue = now.checked_add(self.clock_skew);
        *signed.scope_id == scope.id
            && latest_issue.is_none_or(|latest| signed.issued_at <= latest)
            && signed.expires_at > now
            && signed.issued_at < signed.expires_at
            && *signed.nonce != [0; 32]
            && self.trusted_keys.get(signed.issuer) == Some(signed.signing_key)
    }
}

/// The fields a claim and a grant share, which [`SignedClaims`] checks.
struct Signed<'a> {
    scope_id: &'a ScopeId,
    issuer: &'a AgentId,
    issued_at: SystemTime,
    expires_at: SystemTime,
    nonce: &'a [u8; 32],
    signing_key: &'a [u8; 32],
}

impl RosterClaims {
    #[must_use]
    pub const fn new(
        signed_claims: SignedClaims,
        data_dir: crate::data_dir::DataDir,
    ) -> Self {
        Self { signed_claims, data_dir }
    }
}

impl TrustProvider for SignedClaims {
    fn verify(
        &self,
        claim: &Claim,
        authenticated_endpoint: &EndpointId,
        scope: &Scope,
    ) -> Result<(), ProviderError> {
        let signed = Signed {
            scope_id: &claim.scope_id,
            issuer: &claim.issuer_agent_id,
            issued_at: claim.issued_at,
            expires_at: claim.expires_at,
            nonce: &claim.nonce,
            signing_key: &claim.signing_key,
        };
        if claim.subject_endpoint_id != *authenticated_endpoint
            || !self.trusts(&signed, scope)
        {
            return Err(ProviderError::Invalid("claim is not trusted".to_owned()));
        }
        verify(&claim.signing_key, &claim_bytes(claim), &claim.signature).map_err(
            |_| ProviderError::Invalid("claim signature is invalid".to_owned()),
        )
    }

    fn verify_grant(&self, grant: &Grant, scope: &Scope) -> Result<(), ProviderError> {
        let signed = Signed {
            scope_id: &grant.scope_id,
            issuer: &grant.issuer_agent_id,
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
            nonce: &grant.nonce,
            signing_key: &grant.signing_key,
        };
        if !self.trusts(&signed, scope) {
            return Err(ProviderError::Invalid("grant is not trusted".to_owned()));
        }
        verify(&grant.signing_key, &grant_bytes(grant), &grant.signature).map_err(
            |_| ProviderError::Invalid("grant signature is invalid".to_owned()),
        )
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
        let store = self.data_dir.store().map_err(|_| ProviderError::Unavailable)?;
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
    claim.signature = sign(signing_key, &claim_bytes(&claim));
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
    grant.signature = sign(signing_key, &grant_bytes(&grant));
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
/// The caller sends the claim with each request; the host's node needs the
/// grant.
pub fn sign_access(
    signing_key: &SigningKey,
    input: &AccessInput,
) -> Result<(Claim, Grant), SignError> {
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
    Ok((claim, grant))
}

/// The JSON form of a grant, in a policy file and in a stored change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantRecord {
    id: String,
    issuer_agent_id: String,
    subject_agent_id: String,
    subject_endpoint_id: String,
    scope_id: String,
    capabilities: Vec<String>,
    workspaces: Vec<String>,
    access_mode: String,
    approval_rule: String,
    issued_at: u64,
    expires_at: u64,
    #[serde(with = "hex::serde")]
    signing_key: [u8; 32],
    #[serde(with = "hex::serde")]
    nonce: [u8; 32],
    #[serde(with = "hex::serde")]
    signature: Vec<u8>,
}

/// A grant whose JSON form does not read, names an empty field, or holds a
/// time outside the clock.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum InvalidGrant {
    #[error("grant is invalid at {0}")]
    Field(String),
    #[error("grant is invalid")]
    Invalid,
}

impl From<&Grant> for GrantRecord {
    fn from(grant: &Grant) -> Self {
        let seconds = |time| aether_core::time::unix_seconds(time).unwrap_or(0);
        Self {
            id: grant.id.as_str().to_owned(),
            issuer_agent_id: grant.issuer_agent_id.as_str().to_owned(),
            subject_agent_id: grant.subject_agent_id.as_str().to_owned(),
            subject_endpoint_id: grant.subject_endpoint_id.as_str().to_owned(),
            scope_id: grant.scope_id.as_str().to_owned(),
            capabilities: grant
                .capabilities
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            workspaces: grant
                .workspaces
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            access_mode: grant.access_mode.as_str().to_owned(),
            approval_rule: grant.approval_rule.as_str().to_owned(),
            issued_at: seconds(grant.issued_at),
            expires_at: seconds(grant.expires_at),
            signing_key: grant.signing_key,
            nonce: grant.nonce,
            signature: grant.signature.clone(),
        }
    }
}

impl TryFrom<GrantRecord> for Grant {
    type Error = InvalidGrant;

    fn try_from(record: GrantRecord) -> Result<Self, Self::Error> {
        let capabilities = record
            .capabilities
            .into_iter()
            .filter(|value| !value.is_empty())
            .map(CapabilityId::new)
            .collect::<BTreeSet<_>>();
        let workspaces = record
            .workspaces
            .into_iter()
            .filter(|value| !value.is_empty())
            .map(WorkspaceId::new)
            .collect::<BTreeSet<_>>();
        let access_mode =
            AccessMode::parse(&record.access_mode).ok_or(InvalidGrant::Invalid)?;
        let approval_rule =
            ApprovalRule::parse(&record.approval_rule).ok_or(InvalidGrant::Invalid)?;
        if record.id.is_empty()
            || record.issuer_agent_id.is_empty()
            || record.subject_agent_id.is_empty()
            || record.subject_endpoint_id.is_empty()
            || record.scope_id.is_empty()
            || capabilities.is_empty()
            || workspaces.is_empty()
        {
            return Err(InvalidGrant::Invalid);
        }
        let time = |seconds| {
            aether_core::time::from_unix_seconds(seconds).ok_or(InvalidGrant::Invalid)
        };
        Ok(Self {
            id: GrantId::new(record.id),
            issuer_agent_id: AgentId::new(record.issuer_agent_id),
            subject_agent_id: AgentId::new(record.subject_agent_id),
            subject_endpoint_id: EndpointId::new(record.subject_endpoint_id),
            scope_id: ScopeId::new(record.scope_id),
            capabilities,
            workspaces,
            access_mode,
            approval_rule,
            issued_at: time(record.issued_at)?,
            expires_at: time(record.expires_at)?,
            signing_key: record.signing_key,
            nonce: record.nonce,
            signature: record.signature,
        })
    }
}

/// The JSON text of `grant`, which [`decode_grant`] reads back.
///
/// # Errors
///
/// [`InvalidGrant::Invalid`] when the grant has no JSON form.
pub fn encode_grant(grant: &Grant) -> Result<String, InvalidGrant> {
    serde_json::to_string(&GrantRecord::from(grant)).map_err(|_| InvalidGrant::Invalid)
}

/// The grant of the JSON text `text`.
///
/// # Errors
///
/// [`InvalidGrant::Field`] names the field that does not read; the other
/// errors are [`InvalidGrant::Invalid`].
pub fn decode_grant(text: &str) -> Result<Grant, InvalidGrant> {
    let mut decoder = serde_json::Deserializer::from_str(text);
    let record: GrantRecord = serde_path_to_error::deserialize(&mut decoder)
        .map_err(|error| InvalidGrant::Field(error.path().to_string()))?;
    Grant::try_from(record)
}

fn random_nonce() -> Result<[u8; 32], SignError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| SignError::Randomness)?;
    Ok(nonce)
}

fn claim_bytes(claim: &Claim) -> Vec<u8> {
    SignedBytes::new(b"aether/claim/v1")
        .string(claim.id.as_str())
        .string(claim.issuer_agent_id.as_str())
        .string(claim.subject_agent_id.as_str())
        .string(claim.subject_endpoint_id.as_str())
        .string(claim.scope_id.as_str())
        .time(claim.issued_at)
        .time(claim.expires_at)
        .raw(&claim.signing_key)
        .raw(&claim.nonce)
        .finish()
}

fn grant_bytes(grant: &Grant) -> Vec<u8> {
    SignedBytes::new(b"aether/grant/v1")
        .string(grant.id.as_str())
        .string(grant.issuer_agent_id.as_str())
        .string(grant.subject_agent_id.as_str())
        .string(grant.subject_endpoint_id.as_str())
        .string(grant.scope_id.as_str())
        .strings(grant.capabilities.iter().map(CapabilityId::as_str))
        .strings(grant.workspaces.iter().map(WorkspaceId::as_str))
        .byte(match grant.access_mode {
            AccessMode::ReadOnly => 0,
            AccessMode::ReadWrite => 1,
        })
        .byte(match grant.approval_rule {
            ApprovalRule::Automatic => 0,
            ApprovalRule::LocalUi => 1,
        })
        .time(grant.issued_at)
        .time(grant.expires_at)
        .raw(&grant.signing_key)
        .raw(&grant.nonce)
        .finish()
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
    use aether_store::ManagedMember;
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
    fn signed_claims_reject_forged_changed_wrong_expired_future_and_cross_scope_claims()
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
    }

    #[test]
    fn signed_grants_reject_changed_and_unknown_grants() {
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
    }

    #[test]
    fn managed_claims_require_an_active_durable_roster_member() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = crate::data_dir::DataDir::new(directory.path());
        let mut store = data_dir.store().expect("store must open");
        store
            .apply_managed_member(
                "https://control.test",
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
        let trust = RosterClaims::new(provider(&key), data_dir);
        let scope = Scope { id: ScopeId::new("managed"), kind: ScopeKind::Managed };
        assert!(trust.verify(&claim, &EndpointId::new("endpoint"), &scope,).is_ok());
        store
            .revoke_managed_member("https://control.test", 2, "managed", "subject")
            .expect("member must revoke");
        assert!(trust.verify(&claim, &EndpointId::new("endpoint"), &scope,).is_err());
    }

    #[test]
    fn a_signed_access_grant_survives_the_policy_form() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let access = super::AccessInput {
            issuer_agent_id: "node".to_owned(),
            subject_agent_id: "friend".to_owned(),
            subject_endpoint_id: "friend-endpoint".to_owned(),
            scope_id: "direct".to_owned(),
            capabilities: vec!["review".to_owned()],
            workspaces: vec!["workspace".to_owned()],
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            lifetime: Duration::from_secs(60),
        };
        let (claim, grant) =
            super::sign_access(&key, &access).expect("access must sign");
        let text = super::encode_grant(&grant).expect("grant must encode");
        let parsed = super::decode_grant(&text).expect("grant must parse back");
        // `SystemTime` keeps sub-second precision; the policy form does not.
        assert_eq!(super::encode_grant(&parsed).expect("grant must encode"), text);
        assert_eq!(claim.subject_endpoint_id.as_str(), "friend-endpoint");
        assert_eq!(claim.signing_key, grant.signing_key);
    }
}
