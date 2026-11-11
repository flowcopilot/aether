use std::{
    path::PathBuf,
    sync::OnceLock,
    sync::{Arc, Mutex, PoisonError},
};

use aether_store::pact::{PactStore, PendingDevice};

use super::{
    config::{Brand, PactConfig},
    grant::{GrantSignError, pact_grant_record, sign_aether_grant},
    jwks::PaIdentity,
};
use crate::admin::{GrantApplicator, GrantApplyError};

pub type SharedPactStore = Arc<Mutex<PactStore>>;

#[derive(Default)]
pub struct ConsentLink(OnceLock<String>);

impl ConsentLink {
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
    #[error("the grant was applied but could not be rolled back")]
    RollbackFailed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConsentRequest {
    pub user_code: String,
    pub brand_name: String,
    pub pa_issuer: String,
    pub pa_origin: String,
    pub scopes: Vec<(String, String)>,
}

pub struct PactConsent {
    config: PactConfig,
    store: SharedPactStore,
    data_dir: PathBuf,
}

impl PactConsent {
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(config: PactConfig, store: SharedPactStore, data_dir: PathBuf) -> Self {
        Self { config, store, data_dir }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, PactStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn pending(&self, user_code: &str) -> Option<ConsentRequest> {
        let (device, brand) = self.pending_device(user_code).ok()??;
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

    fn pending_device(
        &self,
        user_code: &str,
    ) -> Result<Option<(PendingDevice, &Brand)>, ConsentError> {
        let user_code = normalize_user_code(user_code);
        let device = self
            .store()
            .pending_device(&user_code, aether_core::time::now())
            .map_err(|_| ConsentError::Store)?;
        let Some(device) = device else {
            return Ok(None);
        };
        let brand = self.config.brand(&device.brand_id).ok_or(ConsentError::Store)?;
        Ok(Some((device, brand)))
    }

    pub fn approve<G: GrantApplicator + ?Sized>(
        &self,
        user_code: &str,
        chosen: &[String],
        grants: &G,
    ) -> Result<Vec<String>, ConsentError> {
        let (device, brand) =
            self.pending_device(user_code)?.ok_or(ConsentError::NotPending)?;
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
        let at = aether_core::time::now();
        let caller = PaIdentity {
            issuer: device.pa_issuer.clone(),
            subject: device.pa_subject.clone(),
        };
        let grant =
            sign_aether_grant(&self.data_dir, brand, &caller, at).map_err(|error| {
                match error {
                    GrantSignError::Sign => ConsentError::Sign,
                }
            })?;
        let grant_id = grant.id.as_str().to_owned();
        grants.apply_signed_grant(grant).map_err(|error| match error {
            GrantApplyError::Refused => ConsentError::Refused,
        })?;
        let record = pact_grant_record(
            &grant_id,
            &brand.id,
            &device.pa_issuer,
            &device.pa_subject,
            &granted.join(" "),
            at,
        );
        if self.store().approve_device(&device.user_code, &record, at).is_err() {
            if grants.revoke_signed_grant(&grant_id).is_err() {
                return Err(ConsentError::RollbackFailed);
            }
            return Err(ConsentError::NotPending);
        }
        Ok(granted)
    }

    pub fn deny(&self, user_code: &str) -> Result<(), ConsentError> {
        self.store()
            .deny_device(&normalize_user_code(user_code), aether_core::time::now())
            .map_err(|error| match error {
                aether_store::StoreError::NotFound => ConsentError::NotPending,
                _ => ConsentError::Store,
            })
    }
}

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
