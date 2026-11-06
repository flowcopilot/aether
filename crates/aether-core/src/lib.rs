use std::{collections::BTreeSet, fmt, time::SystemTime};

pub mod limits;
pub mod time;

pub const PROTOCOL_FAMILY: &str = "aether";

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(AgentId);
identifier!(EndpointId);
identifier!(ScopeId);
identifier!(CapabilityId);
identifier!(WorkspaceId);

impl WorkspaceId {
    #[must_use]
    pub fn allows_path_token(value: &str) -> bool {
        !value.contains("..") && !value.starts_with('/')
    }
}

identifier!(GrantId);
identifier!(ClaimId);
identifier!(TaskId);
identifier!(RequestId);

/// Gives an enum one table of names, read by `as_str` and `parse`. The
/// names appear in policy files, wire data and database columns, so a
/// changed name breaks all three.
#[macro_export]
macro_rules! named {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            #[must_use]
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

/// Where a caller reaches the node from.
///
/// A `Pact` caller carries no `Claim`: its identity is verified outside
/// this crate, so `PolicyEngine::admit_verified` admits it and
/// `PolicyEngine::admit` refuses it.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum ScopeKind {
    Direct,
    Directory,
    Managed,
    Pact,
}

named!(ScopeKind {
    Direct => "direct",
    Directory => "directory",
    Managed => "managed",
    Pact => "pact",
});

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Scope {
    pub id: ScopeId,
    pub kind: ScopeKind,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}

named!(AccessMode { ReadOnly => "read_only", ReadWrite => "read_write" });

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum ApprovalRule {
    Automatic,
    LocalUi,
}

named!(ApprovalRule { Automatic => "automatic", LocalUi => "local_ui" });

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Claim {
    pub id: ClaimId,
    pub issuer_agent_id: AgentId,
    pub subject_agent_id: AgentId,
    pub subject_endpoint_id: EndpointId,
    pub scope_id: ScopeId,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub signing_key: [u8; 32],
    pub nonce: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Grant {
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
    pub signing_key: [u8; 32],
    pub nonce: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Request {
    pub id: RequestId,
    pub task_id: TaskId,
    pub caller_agent_id: AgentId,
    pub caller_endpoint_id: EndpointId,
    pub scope_id: ScopeId,
    pub capability_id: CapabilityId,
    pub workspace_id: WorkspaceId,
    pub access_mode: AccessMode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProviderError {
    Unavailable,
    Invalid(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("provider unavailable"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ProviderError {}

pub trait TrustProvider: Send + Sync {
    fn verify(
        &self,
        claim: &Claim,
        authenticated_endpoint: &EndpointId,
        scope: &Scope,
    ) -> Result<(), ProviderError>;

    fn verify_grant(&self, grant: &Grant, scope: &Scope) -> Result<(), ProviderError>;
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Denial {
    EndpointMismatch,
    ClaimDenied,
    ClaimExpired,
    ScopeMismatch,
    LocalGrantMissing,
    LocalGrantExpired,
    GrantDenied,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Admission {
    Approved,
    Denied(Denial),
}

pub struct PolicyEngine<T> {
    trust: T,
    grants: Vec<Grant>,
}

impl<T> PolicyEngine<T>
where
    T: TrustProvider,
{
    #[must_use]
    pub fn new(trust: T, grants: impl IntoIterator<Item = Grant>) -> Self {
        Self { trust, grants: grants.into_iter().collect() }
    }

    #[must_use]
    pub fn has_grant(&self, id: &GrantId) -> bool {
        self.grants.iter().any(|grant| grant.id == *id)
    }

    #[must_use]
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// Whether the grant `id` was there.
    pub fn remove_grant(&mut self, id: &GrantId) -> bool {
        let before = self.grants.len();
        self.grants.retain(|grant| grant.id != *id);
        self.grants.len() != before
    }

    pub fn replace_grant(&mut self, grant: Grant) {
        if let Some(existing) =
            self.grants.iter_mut().find(|existing| existing.id == grant.id)
        {
            *existing = grant;
        } else {
            self.grants.push(grant);
        }
    }

    pub fn verify_grant(&self, grant: &Grant, scope: &Scope) -> Result<(), Denial> {
        if grant.scope_id != scope.id {
            return Err(Denial::ScopeMismatch);
        }
        if self.trust.verify_grant(grant, scope).is_err() {
            return Err(Denial::GrantDenied);
        }
        Ok(())
    }

    /// Whether a grant still covers `request`, which was admitted earlier
    /// and is about to run.
    pub fn permits_dispatch(
        &self,
        request: &Request,
        scope: &Scope,
    ) -> Result<(), Denial> {
        if request.scope_id != scope.id {
            return Err(Denial::ScopeMismatch);
        }
        self.match_grant(request, scope)
    }

    #[must_use]
    pub fn admit(
        &self,
        request: &Request,
        claim: &Claim,
        scope: &Scope,
        authenticated_endpoint: &EndpointId,
    ) -> Admission {
        if &request.caller_endpoint_id != authenticated_endpoint {
            return Admission::Denied(Denial::EndpointMismatch);
        }
        if scope.kind == ScopeKind::Pact
            || request.scope_id != scope.id
            || claim.scope_id != scope.id
            || claim.subject_endpoint_id != *authenticated_endpoint
            || claim.subject_agent_id != request.caller_agent_id
        {
            return Admission::Denied(Denial::ScopeMismatch);
        }
        if claim.expires_at <= SystemTime::now() {
            return Admission::Denied(Denial::ClaimExpired);
        }
        if self.trust.verify(claim, authenticated_endpoint, scope).is_err() {
            return Admission::Denied(Denial::ClaimDenied);
        }
        self.admission(request, scope)
    }

    /// Admits a request in a `pact` scope from a caller whose identity was
    /// verified outside this crate as `authenticated_endpoint`. The local
    /// grant still decides, as for `admit`. Every other scope kind is
    /// refused, because its callers must present a claim.
    #[must_use]
    pub fn admit_verified(
        &self,
        request: &Request,
        scope: &Scope,
        authenticated_endpoint: &EndpointId,
    ) -> Admission {
        if &request.caller_endpoint_id != authenticated_endpoint {
            return Admission::Denied(Denial::EndpointMismatch);
        }
        if scope.kind != ScopeKind::Pact || request.scope_id != scope.id {
            return Admission::Denied(Denial::ScopeMismatch);
        }
        self.admission(request, scope)
    }

    fn admission(&self, request: &Request, scope: &Scope) -> Admission {
        match self.match_grant(request, scope) {
            Ok(()) => Admission::Approved,
            Err(denial) => Admission::Denied(denial),
        }
    }

    /// The grant step of every admission, run once the caller's identity is
    /// established.
    fn match_grant(&self, request: &Request, scope: &Scope) -> Result<(), Denial> {
        let grants = self.grants.iter().filter(|grant| {
            grant.subject_agent_id == request.caller_agent_id
                && grant.subject_endpoint_id == request.caller_endpoint_id
                && grant.scope_id == request.scope_id
                && grant.capabilities.contains(&request.capability_id)
                && grant.workspaces.contains(&request.workspace_id)
                && grant.access_mode == request.access_mode
        });
        let mut expired = false;
        let mut denied = false;
        for grant in grants {
            if grant.expires_at <= SystemTime::now() {
                expired = true;
            } else if self.trust.verify_grant(grant, scope).is_ok() {
                return Ok(());
            } else {
                denied = true;
            }
        }
        if denied {
            Err(Denial::GrantDenied)
        } else if expired {
            Err(Denial::LocalGrantExpired)
        } else {
            Err(Denial::LocalGrantMissing)
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use super::{
        AccessMode, Admission, AgentId, ApprovalRule, CapabilityId, Claim, ClaimId,
        Denial, EndpointId, Grant, GrantId, PolicyEngine, ProviderError, Request,
        RequestId, Scope, ScopeId, ScopeKind, TaskId, TrustProvider, WorkspaceId,
    };
    use std::{
        collections::BTreeSet,
        time::{Duration, SystemTime},
    };

    struct Trusted;

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

    fn ids()
    -> (AgentId, EndpointId, Scope, CapabilityId, WorkspaceId, Request, Claim, Grant)
    {
        let agent = AgentId::new("agent");
        let endpoint = EndpointId::new("endpoint");
        let scope = Scope { id: ScopeId::new("direct"), kind: ScopeKind::Direct };
        let capability = CapabilityId::new("review");
        let workspace = WorkspaceId::new("workspace");
        let request = Request {
            id: RequestId::new("request"),
            task_id: TaskId::new("task"),
            caller_agent_id: agent.clone(),
            caller_endpoint_id: endpoint.clone(),
            scope_id: scope.id.clone(),
            capability_id: capability.clone(),
            workspace_id: workspace.clone(),
            access_mode: AccessMode::ReadOnly,
        };
        let claim = Claim {
            id: ClaimId::new("claim"),
            issuer_agent_id: agent.clone(),
            subject_agent_id: agent.clone(),
            subject_endpoint_id: endpoint.clone(),
            scope_id: scope.id.clone(),
            issued_at: SystemTime::now() - Duration::from_secs(1),
            expires_at: SystemTime::now() + Duration::from_secs(60),
            signing_key: [0; 32],
            nonce: [1; 32],
            signature: vec![],
        };
        let grant = Grant {
            id: GrantId::new("grant"),
            issuer_agent_id: AgentId::new("issuer"),
            subject_agent_id: agent.clone(),
            subject_endpoint_id: endpoint.clone(),
            scope_id: scope.id.clone(),
            capabilities: BTreeSet::from([capability.clone()]),
            workspaces: BTreeSet::from([workspace.clone()]),
            access_mode: AccessMode::ReadOnly,
            approval_rule: ApprovalRule::Automatic,
            issued_at: SystemTime::now() - Duration::from_secs(1),
            expires_at: SystemTime::now() + Duration::from_secs(60),
            signing_key: [2; 32],
            nonce: [3; 32],
            signature: vec![4; 64],
        };
        (agent, endpoint, scope, capability, workspace, request, claim, grant)
    }

    #[test]
    fn a_grant_from_another_scope_cannot_admit_work() {
        let (_, endpoint, scope, _, _, request, claim, mut grant) = ids();
        grant.scope_id = ScopeId::new("directory");
        let engine = PolicyEngine::new(Trusted, [grant]);
        assert_eq!(
            engine.admit(&request, &claim, &scope, &endpoint,),
            Admission::Denied(Denial::LocalGrantMissing),
        );
    }

    /// The `ids()` request and grant, moved into a `pact` scope.
    fn pact_ids() -> (EndpointId, Scope, Request, Claim, Grant) {
        let (_, endpoint, _, _, _, mut request, mut claim, mut grant) = ids();
        let scope = Scope { id: ScopeId::new("pact"), kind: ScopeKind::Pact };
        request.scope_id = scope.id.clone();
        claim.scope_id = scope.id.clone();
        grant.scope_id = scope.id.clone();
        (endpoint, scope, request, claim, grant)
    }

    #[test]
    fn a_claim_cannot_admit_work_in_a_pact_scope() {
        let (endpoint, scope, request, claim, grant) = pact_ids();
        let engine = PolicyEngine::new(Trusted, [grant]);
        assert_eq!(
            engine.admit(&request, &claim, &scope, &endpoint,),
            Admission::Denied(Denial::ScopeMismatch),
        );
    }

    #[test]
    fn verified_admission_needs_a_pact_scope_and_a_grant() {
        let (endpoint, scope, request, _, grant) = pact_ids();
        assert_eq!(
            PolicyEngine::new(Trusted, [grant.clone()])
                .admit_verified(&request, &scope, &endpoint,),
            Admission::Approved,
        );
        assert_eq!(
            PolicyEngine::new(Trusted, []).admit_verified(&request, &scope, &endpoint,),
            Admission::Denied(Denial::LocalGrantMissing),
        );
        let (_, endpoint, direct, _, _, direct_request, _, direct_grant) = ids();
        assert_eq!(
            PolicyEngine::new(Trusted, [direct_grant]).admit_verified(
                &direct_request,
                &direct,
                &endpoint,
            ),
            Admission::Denied(Denial::ScopeMismatch),
        );
        assert_eq!(
            PolicyEngine::new(Trusted, [grant]).admit_verified(
                &request,
                &scope,
                &EndpointId::new("another endpoint"),
            ),
            Admission::Denied(Denial::EndpointMismatch),
        );
    }

    #[test]
    fn names_stay_the_same_and_parse_back() {
        for (kind, name) in [
            (ScopeKind::Direct, "direct"),
            (ScopeKind::Directory, "directory"),
            (ScopeKind::Managed, "managed"),
            (ScopeKind::Pact, "pact"),
        ] {
            assert_eq!(kind.as_str(), name);
            assert_eq!(ScopeKind::parse(name), Some(kind));
        }
        for (mode, name) in
            [(AccessMode::ReadOnly, "read_only"), (AccessMode::ReadWrite, "read_write")]
        {
            assert_eq!(mode.as_str(), name);
            assert_eq!(AccessMode::parse(name), Some(mode));
        }
        for (rule, name) in [
            (ApprovalRule::Automatic, "automatic"),
            (ApprovalRule::LocalUi, "local_ui"),
        ] {
            assert_eq!(rule.as_str(), name);
            assert_eq!(ApprovalRule::parse(name), Some(rule));
        }
        assert_eq!(ScopeKind::parse("Direct"), None);
    }
}
