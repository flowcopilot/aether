//! The node's PACT signing key: a P-256 key that signs delegation tokens
//! and receipts with ES256 (spec §5.4, §5.6). The node's Ed25519 keys stay
//! separate, because PACT accepts only `ES256` and `RS256`.

use std::{fs, io, path::Path};

use aws_lc_rs::{
    digest,
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Serialize, de::DeserializeOwned};

pub const KEY_FILE: &str = "pact-signing.key";

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum KeyError {
    #[error("the PACT signing key cannot be read or written")]
    Io,
    #[error("the PACT signing key is not a P-256 PKCS#8 key")]
    Invalid,
    #[error("the system cannot give random bytes for a key")]
    Randomness,
    #[error("a token cannot be signed")]
    Sign,
}

pub struct ProviderKey {
    encoding: EncodingKey,
    decoding: DecodingKey,
    kid: String,
    x: String,
    y: String,
}

impl ProviderKey {
    /// Loads `<data_dir>/pact-signing.key`, or creates it owner-only. A
    /// symbolic link is refused.
    pub fn load_or_create(data_dir: &Path) -> Result<Self, KeyError> {
        let path = data_dir.join(KEY_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Self::from_pkcs8(&fs::read(&path).map_err(|_| KeyError::Io)?)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let document = EcdsaKeyPair::generate_pkcs8(
                    &ECDSA_P256_SHA256_FIXED_SIGNING,
                    &SystemRandom::new(),
                )
                .map_err(|_| KeyError::Randomness)?;
                fs::create_dir_all(data_dir).map_err(|_| KeyError::Io)?;
                crate::key_file::write_owner_only(&path, document.as_ref())
                    .map_err(|_| KeyError::Io)?;
                Self::from_pkcs8(document.as_ref())
            }
            // Not a plain file (a symbolic link, a directory), or unreadable.
            Ok(_) | Err(_) => Err(KeyError::Io),
        }
    }

    pub fn from_pkcs8(der: &[u8]) -> Result<Self, KeyError> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der)
            .map_err(|_| KeyError::Invalid)?;
        // An uncompressed SEC1 point: 0x04, then 32 bytes of x, 32 of y.
        let point = pair.public_key().as_ref();
        let (Some(x), Some(y)) = (point.get(1..33), point.get(33..65)) else {
            return Err(KeyError::Invalid);
        };
        let x = URL_SAFE_NO_PAD.encode(x);
        let y = URL_SAFE_NO_PAD.encode(y);
        // The RFC 7638 thumbprint: SHA-256 of the required members in
        // lexicographic order, with no white space.
        let thumbprint = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
        let kid = URL_SAFE_NO_PAD
            .encode(digest::digest(&digest::SHA256, thumbprint.as_bytes()));
        let decoding =
            DecodingKey::from_ec_components(&x, &y).map_err(|_| KeyError::Invalid)?;
        Ok(Self { encoding: EncodingKey::from_ec_der(der), decoding, kid, x, y })
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key as a JWK Set, served at the `jwks_uri` (§5.1).
    #[must_use]
    pub fn jwks(&self) -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "EC",
                "crv": "P-256",
                "x": self.x,
                "y": self.y,
                "kid": self.kid,
                "alg": "ES256",
                "use": "sig",
            }]
        })
    }

    /// Signs `claims` as a compact JWS with header `typ`.
    pub fn sign(&self, typ: &str, claims: &impl Serialize) -> Result<String, KeyError> {
        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some(typ.to_owned());
        header.kid = Some(self.kid.clone());
        jsonwebtoken::encode(&header, claims, &self.encoding)
            .map_err(|_| KeyError::Sign)
    }

    /// Verifies a token this key signed, with header `typ`, and the claim
    /// rules in `validation`. `None` for any failure.
    #[must_use]
    pub fn verify<T: DeserializeOwned>(
        &self,
        token: &str,
        typ: &str,
        validation: &Validation,
    ) -> Option<T> {
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg != Algorithm::ES256 || header.typ.as_deref() != Some(typ) {
            return None;
        }
        jsonwebtoken::decode::<T>(token, &self.decoding, validation)
            .ok()
            .map(|data| data.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::{KEY_FILE, ProviderKey};
    use jsonwebtoken::{Algorithm, Validation};

    #[test]
    fn the_key_is_created_once_and_loaded_again() {
        let directory = tempfile::tempdir().expect("directory must create");
        let first =
            ProviderKey::load_or_create(directory.path()).expect("key must create");
        let again =
            ProviderKey::load_or_create(directory.path()).expect("key must load");
        assert_eq!(first.kid(), again.kid());
        assert_eq!(first.jwks(), again.jwks());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.path().join(KEY_FILE))
                .expect("key file must exist")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_signed_token_verifies_only_with_its_type_and_key() {
        let directory = tempfile::tempdir().expect("directory must create");
        let key =
            ProviderKey::load_or_create(directory.path()).expect("key must create");
        let other = tempfile::tempdir().expect("directory must create");
        let other = ProviderKey::load_or_create(other.path()).expect("key must create");
        let mut validation = Validation::new(Algorithm::ES256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        let claims = serde_json::json!({ "grantId": "g" });
        let token = key.sign("pact-receipt+jws", &claims).expect("token must sign");
        assert_eq!(
            key.verify::<serde_json::Value>(&token, "pact-receipt+jws", &validation),
            Some(claims),
        );
        assert_eq!(
            key.verify::<serde_json::Value>(&token, "at+jwt", &validation),
            None
        );
        assert_eq!(
            other.verify::<serde_json::Value>(&token, "pact-receipt+jws", &validation),
            None,
        );
    }
}
