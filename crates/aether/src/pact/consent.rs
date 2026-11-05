//! The owner's consent (spec §5.3).
//!
//! The node owner is the only Brand user, and the loopback admin UI is the
//! Brand's login: it already holds the one owner session. Approval signs an
//! ordinary Aether grant with the node's issuer key and applies it through
//! the administration service, so the grant list, revocation, and the
//! dispatch-time recheck cover PACT callers too.

use aether_core::time::now;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::{Duration, SystemTime},
};

use aether_core::{
    AgentId, ApprovalRule, CapabilityId, EndpointId, GrantId, ScopeId, WorkspaceId,
};
use aether_store::pact::{PactStore, PendingDevice};

use super::{
    config::{Brand, PactConfig, SCOPE_ID},
    gateway::{GRANT_SECONDS, caller_agent_id, caller_endpoint_id, new_grant},
    jwks::PaIdentity,
};
use crate::admin::SharedAdministration;

/// The origin of the admin UI's consent page, set once the UI listens.
/// Until then, the gateway cannot start a device flow.
#[derive(Default)]
pub struct ConsentLink(OnceLock<String>);

impl ConsentLink {
    /// Sets the origin, such as `https://127.0.0.1:3000`. Only the first
    /// call has an effect.
    pub fn set(&self, origin: String) {
        let _ = self.0.set(origin);
    }

    #[must_use]
    pub fn get(&self) -> Option<&str> {
        self.0.get().map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum ConsentError {
    #[error("the request expired or was already decided")]
    NotPending,
    #[error("the grant cannot be signed")]
    Sign,
    #[error("the node refused the grant")]
    Refused,
    #[error("the decision cannot be stored")]
    Store,
}

/// What the consent page shows (§5.3): the personal agent's issuer
/// origin, the Brand, and each requested scope.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConsentRequest {
    pub user_code: String,
    pub brand_name: String,
    pub pa_issuer: String,
    pub pa_origin: String,
    /// `(scope id, description shown verbatim)`.
    pub scopes: Vec<(String, String)>,
}

pub struct PactConsent {
    config: PactConfig,
    store: Mutex<PactStore>,
    data_dir: PathBuf,
}

impl PactConsent {
    pub fn open(
        config: PactConfig,
        data_dir: &Path,
    ) -> Result<Self, aether_store::StoreError> {
        Ok(Self {
            store: Mutex::new(PactStore::in_dir(data_dir)?),
            config,
            data_dir: data_dir.to_owned(),
        })
    }

    fn store(&self) -> std::sync::MutexGuard<'_, PactStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pending request with `user_code`, ignoring case and spaces.
    #[must_use]
    pub fn pending(&self, user_code: &str) -> Option<ConsentRequest> {
        let (device, brand) = self.pending_device(user_code)?;
        let pa_origin = reqwest::Url::parse(&device.pa_issuer).ok().map_or_else(
            || device.pa_issuer.clone(),
            |url| url.origin().ascii_serialization(),
        );
        let scopes = device
            .scopes
            .split(' ')
            .filter(|scope| *scope == brand.scope_id)
            .map(|scope| (scope.to_owned(), brand.scope_description.clone()))
            .collect();
        Some(ConsentRequest {
            user_code: device.user_code,
            brand_name: brand.name.clone(),
            pa_issuer: device.pa_issuer,
            pa_origin,
            scopes,
        })
    }

    fn pending_device(&self, user_code: &str) -> Option<(PendingDevice, &Brand)> {
        let user_code = normalize_user_code(user_code);
        let device = self.store().pending_device(&user_code, now()).ok()??;
        let brand = self.config.brand(&device.brand_id)?;
        Some((device, brand))
    }

    /// Approves the scopes in `chosen` that were requested. An empty
    /// choice denies. Returns the approved scope ids.
    pub fn approve(
        &self,
        user_code: &str,
        chosen: &[String],
        administration: &SharedAdministration,
    ) -> Result<Vec<String>, ConsentError> {
        let (device, brand) =
            self.pending_device(user_code).ok_or(ConsentError::NotPending)?;
        let granted = device
            .scopes
            .split(' ')
            .filter(|scope| {
                *scope == brand.scope_id && chosen.iter().any(|chosen| chosen == scope)
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if granted.is_empty() {
            self.deny(user_code)?;
            return Ok(granted);
        }
        let caller = PaIdentity {
            issuer: device.pa_issuer.clone(),
            subject: device.pa_subject.clone(),
        };
        let grant = self.sign_grant(brand, &caller)?;
        let grant_id = grant.id.as_str().to_owned();
        let change = crate::admin::ChangeRequest::Grant {
            id: grant_id.clone(),
            grant: crate::identity::encode_grant(&grant)
                .map_err(|_| ConsentError::Sign)?,
        }
        .into_change()
        .map_err(|_| ConsentError::Refused)?;
        administration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply_ui(&change)
            .map_err(|_| ConsentError::Refused)?;
        let record = new_grant(
            grant_id.clone(),
            &brand.id,
            &device.pa_issuer,
            &device.pa_subject,
            &granted.join(" "),
            now(),
        );
        if self.store().approve_device(&device.user_code, &record, now()).is_err() {
            // The request expired between the page and the click: take the
            // live grant back, so nothing is left that no token can use.
            let _ = administration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .revoke_ui(&grant_id);
            return Err(ConsentError::NotPending);
        }
        Ok(granted)
    }

    pub fn deny(&self, user_code: &str) -> Result<(), ConsentError> {
        self.store()
            .deny_device(&normalize_user_code(user_code))
            .map_err(|_| ConsentError::NotPending)
    }

    fn sign_grant(
        &self,
        brand: &Brand,
        caller: &PaIdentity,
    ) -> Result<aether_core::Grant, ConsentError> {
        let issuer = crate::data_dir::DataDir::new(&self.data_dir)
            .issuer_key()
            .map_err(|_| ConsentError::Sign)?;
        let now = SystemTime::now();
        let lifetime = Duration::from_secs(GRANT_SECONDS.unsigned_abs());
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| ConsentError::Sign)?;
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
                issued_at: now.checked_sub(Duration::from_secs(1)).unwrap_or(now),
                expires_at: now.checked_add(lifetime).ok_or(ConsentError::Sign)?,
                nonce,
            },
        ))
    }
}

/// `abcd efgh` and `ABCD-EFGH` name the same code.
fn normalize_user_code(user_code: &str) -> String {
    let letters = user_code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_uppercase())
        .collect::<String>();
    match (letters.get(..4), letters.get(4..)) {
        (Some(first), Some(second)) if !second.is_empty() => {
            format!("{first}-{second}")
        }
        _ => letters,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_user_code;

    #[test]
    fn a_user_code_is_read_without_case_or_separators() {
        assert_eq!(normalize_user_code("bcdf ghjk"), "BCDF-GHJK");
        assert_eq!(normalize_user_code("BCDF-GHJK"), "BCDF-GHJK");
    }
}
