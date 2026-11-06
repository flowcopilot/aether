//! Delegation tokens (spec §5.4) and receipts (§5.6), both signed with the
//! node's [`ProviderKey`].

use jsonwebtoken::{Algorithm, Validation};
use serde::{Deserialize, Serialize};

use super::keys::{KeyError, ProviderKey};

/// The JOSE `typ` of a delegation token, as the reference Provider sets it.
pub const ACCESS_TOKEN_TYPE: &str = "at+jwt";
/// The JOSE `typ` of a receipt.
pub const RECEIPT_TYPE: &str = "pact-receipt+jws";
/// Delegation tokens live one hour (§5.4: "SHOULD be ≤ 1 h").
pub const ACCESS_TOKEN_SECONDS: i64 = 60 * 60;

/// The claims of a delegation token (§5.4).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub client_id: String,
    pub scope: String,
    pub grant_id: String,
    pub iat: i64,
    pub exp: i64,
}

impl DelegationClaims {
    /// The granted scope ids.
    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        self.scope.split(' ').filter(|scope| !scope.is_empty())
    }
}

pub fn sign_delegation(
    key: &ProviderKey,
    claims: &DelegationClaims,
) -> Result<String, KeyError> {
    key.sign(ACCESS_TOKEN_TYPE, claims)
}

/// Verifies a delegation token for one Brand.
///
/// `issuer` is the Brand's authorization server and `audience` its
/// interface URL. `None` for any failure; the caller answers `401` with
/// `error="invalid_token"` (§5.5).
#[must_use]
pub fn verify_delegation(
    key: &ProviderKey,
    token: &str,
    issuer: &str,
    audience: &str,
) -> Option<DelegationClaims> {
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.set_required_spec_claims(&["iss", "aud", "sub", "iat", "exp"]);
    validation.leeway = 5;
    key.verify(token, ACCESS_TOKEN_TYPE, &validation)
}

/// One action a receipt reports (§5.6).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptAction {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_hash: Option<String>,
}

/// The claims of a receipt (§5.6).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptClaims {
    pub grant_id: String,
    pub user: String,
    pub pa: String,
    pub brand: String,
    pub scopes_used: Vec<String>,
    pub actions: Vec<ReceiptAction>,
    pub ts: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SignedReceipt {
    pub jws: String,
    pub claims: ReceiptClaims,
}

pub fn sign_receipt(
    key: &ProviderKey,
    claims: &ReceiptClaims,
) -> Result<SignedReceipt, KeyError> {
    let jws = key.sign(RECEIPT_TYPE, claims)?;
    Ok(SignedReceipt { jws, claims: claims.clone() })
}

/// Verifies a receipt this node signed.
#[must_use]
pub fn verify_receipt(key: &ProviderKey, jws: &str) -> Option<ReceiptClaims> {
    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_aud = false;
    key.verify(jws, RECEIPT_TYPE, &validation)
}

#[cfg(test)]
mod tests {
    use super::{
        DelegationClaims, ReceiptAction, ReceiptClaims, sign_delegation, sign_receipt,
        verify_delegation, verify_receipt,
    };
    use crate::pact::keys::ProviderKey;

    fn key() -> (tempfile::TempDir, ProviderKey) {
        let directory = tempfile::tempdir().expect("directory must create");
        let key =
            ProviderKey::load_or_create(directory.path()).expect("key must create");
        (directory, key)
    }

    fn claims(iat: i64, exp: i64) -> DelegationClaims {
        DelegationClaims {
            iss: "https://node.example/a2a/review/oauth".to_owned(),
            aud: "https://node.example/a2a/review".to_owned(),
            sub: "owner".to_owned(),
            client_id: "https://pa.example".to_owned(),
            scope: "review:read_only".to_owned(),
            grant_id: "g".to_owned(),
            iat,
            exp,
        }
    }

    fn now() -> i64 {
        aether_core::time::unix_timestamp(std::time::SystemTime::now()).expect("clock")
    }

    #[test]
    fn a_delegation_token_verifies_for_its_brand_only() {
        let (_directory, key) = key();
        let claims = claims(now(), now() + 3600);
        let token = sign_delegation(&key, &claims).expect("token must sign");
        assert_eq!(
            verify_delegation(&key, &token, &claims.iss, &claims.aud),
            Some(claims.clone()),
        );
        assert_eq!(
            verify_delegation(
                &key,
                &token,
                &claims.iss,
                "https://node.example/a2a/other"
            ),
            None,
        );
        assert_eq!(
            verify_delegation(&key, &token, "https://other.example", &claims.aud),
            None,
        );
        assert_eq!(claims.scopes().collect::<Vec<_>>(), ["review:read_only"]);
    }

    #[test]
    fn an_expired_delegation_token_is_refused() {
        let (_directory, key) = key();
        let claims = claims(now() - 7200, now() - 3600);
        let token = sign_delegation(&key, &claims).expect("token must sign");
        assert_eq!(verify_delegation(&key, &token, &claims.iss, &claims.aud), None);
    }

    #[test]
    fn a_receipt_carries_its_claims_and_their_jws() {
        let (_directory, key) = key();
        let claims = ReceiptClaims {
            grant_id: "g".to_owned(),
            user: "owner".to_owned(),
            pa: "https://pa.example".to_owned(),
            brand: "https://node.example/a2a/review".to_owned(),
            scopes_used: vec!["review:read_only".to_owned()],
            actions: vec![ReceiptAction {
                tool: "codex_turn".to_owned(),
                args_hash: Some("hash".to_owned()),
            }],
            ts: "2026-10-07T12:00:00Z".to_owned(),
        };
        let receipt = sign_receipt(&key, &claims).expect("receipt must sign");
        assert_eq!(receipt.claims.scopes_used[0], "review:read_only");
        assert_eq!(receipt.claims.actions[0].args_hash.as_deref(), Some("hash"));
        assert_eq!(verify_receipt(&key, &receipt.jws), Some(claims));
    }
}
