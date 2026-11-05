//! The `pact` section of `policy.json`, and the URLs it gives each Brand.

use aether_core::AccessMode;
use reqwest::Url;
use serde::{Deserialize, Serialize};

/// The id of the scope every PACT task runs in.
pub const SCOPE_ID: &str = "pact";
/// The only Brand user (decision of 7 October 2026: the node owner).
pub const BRAND_USER: &str = "owner";

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum PactConfigError {
    #[error("pact.{0} is invalid")]
    Field(&'static str),
    #[error("pact.brands[{0}] is invalid")]
    Brand(usize),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PactFile {
    public_url: String,
    audience: String,
    #[serde(default)]
    allow_insecure_local_issuers: bool,
    brands: Vec<BrandFile>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrandFile {
    id: String,
    name: String,
    description: String,
    capability: String,
    workspace: String,
    access_mode: String,
    scope: ScopeFile,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScopeFile {
    id: String,
    description: String,
}

/// The gateway's configuration.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PactConfig {
    /// The public base URL, with no trailing slash.
    pub public_url: String,
    /// The `aud` personal agents put in their JWTs (§3.1).
    pub audience: String,
    pub allow_insecure_local_issuers: bool,
    pub brands: Vec<Brand>,
    /// From the policy's own limits: a turn's transcript is cut to fit.
    pub max_context_tokens: u32,
    /// From the policy's own limits: how long a turn may run.
    pub max_seconds: u64,
}

/// One Brand: an Aether capability offered to personal agents.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Brand {
    pub id: String,
    pub name: String,
    pub description: String,
    pub capability: String,
    pub workspace: String,
    pub access_mode: AccessMode,
    pub scope_id: String,
    pub scope_description: String,
}

impl PactFile {
    pub fn parse(
        self,
        max_context_tokens: u32,
        max_seconds: u64,
    ) -> Result<PactConfig, PactConfigError> {
        let public_url = Url::parse(&self.public_url)
            .ok()
            .filter(public_url_allowed)
            .map(|url| url.as_str().trim_end_matches('/').to_owned())
            .ok_or(PactConfigError::Field("publicUrl"))?;
        if self.audience.is_empty() || self.audience.contains(char::is_whitespace) {
            return Err(PactConfigError::Field("audience"));
        }
        if self.brands.is_empty() {
            return Err(PactConfigError::Field("brands"));
        }
        let mut brands: Vec<Brand> = Vec::with_capacity(self.brands.len());
        for (index, brand) in self.brands.into_iter().enumerate() {
            let access_mode = AccessMode::parse(&brand.access_mode)
                .ok_or(PactConfigError::Brand(index))?;
            let valid = valid_id(&brand.id)
                && !brands.iter().any(|known| known.id == brand.id)
                && !brand.name.trim().is_empty()
                && valid_id(&brand.capability)
                && valid_id(&brand.workspace)
                && valid_scope_id(&brand.scope.id)
                && !brand.scope.description.trim().is_empty();
            if !valid {
                return Err(PactConfigError::Brand(index));
            }
            brands.push(Brand {
                id: brand.id,
                name: brand.name,
                description: brand.description,
                capability: brand.capability,
                workspace: brand.workspace,
                access_mode,
                scope_id: brand.scope.id,
                scope_description: brand.scope.description,
            });
        }
        Ok(PactConfig {
            public_url,
            audience: self.audience,
            allow_insecure_local_issuers: self.allow_insecure_local_issuers,
            brands,
            max_context_tokens,
            max_seconds,
        })
    }
}

/// HTTPS with a host and no credentials, query or fragment; plain HTTP
/// only on a loopback host, for local tests.
fn public_url_allowed(url: &Url) -> bool {
    let bare = url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    bare && (url.scheme() == "https" || (url.scheme() == "http" && loopback))
}

/// A path segment: 1 to 64 letters, digits, `-`, `_` or `.`.
fn valid_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// An OAuth scope token (RFC 6749 §3.3), at most 128 bytes.
fn valid_scope_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().all(|byte| matches!(byte, 0x21 | 0x23..=0x5B | 0x5D..=0x7E))
}

impl PactConfig {
    #[must_use]
    pub fn brand(&self, id: &str) -> Option<&Brand> {
        self.brands.iter().find(|brand| brand.id == id)
    }

    /// Where the Provider keys are published (§5.1 `jwks_uri`).
    #[must_use]
    pub fn jwks_url(&self) -> String {
        format!("{}/a2a/jwks.json", self.public_url)
    }

    /// The Brand's interface URL (§2.1), also the `aud` of its tokens.
    #[must_use]
    pub fn interface_url(&self, brand: &Brand) -> String {
        format!("{}/a2a/{}", self.public_url, brand.id)
    }

    #[must_use]
    pub fn card_url(&self, brand: &Brand) -> String {
        format!("{}/.well-known/agent-card.json", self.interface_url(brand))
    }

    /// The Brand's authorization server: the `iss` of its tokens.
    #[must_use]
    pub fn oauth_issuer(&self, brand: &Brand) -> String {
        format!("{}/oauth", self.interface_url(brand))
    }

    #[must_use]
    pub fn oauth_metadata_url(&self, brand: &Brand) -> String {
        format!("{}/.well-known/oauth-authorization-server", self.oauth_issuer(brand))
    }

    #[must_use]
    pub fn device_authorization_url(&self, brand: &Brand) -> String {
        format!("{}/device_authorization", self.oauth_issuer(brand))
    }

    #[must_use]
    pub fn token_url(&self, brand: &Brand) -> String {
        format!("{}/token", self.oauth_issuer(brand))
    }
}

#[cfg(test)]
mod tests {
    use super::{PactConfigError, PactFile};

    fn file(change: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
        let mut value = serde_json::json!({
            "publicUrl": "https://node.example/",
            "audience": "https://node.example",
            "brands": [{
                "id": "review",
                "name": "Code review",
                "description": "Reviews code.",
                "capability": "review",
                "workspace": "workspace",
                "accessMode": "read_only",
                "scope": { "id": "review:read_only", "description": "Review code" },
            }],
        });
        change(&mut value);
        value
    }

    fn parse(value: serde_json::Value) -> Result<super::PactConfig, PactConfigError> {
        serde_json::from_value::<PactFile>(value)
            .expect("file must decode")
            .parse(1000, 60)
    }

    #[test]
    fn a_valid_section_gives_each_brand_its_urls() {
        let config = parse(file(|_| {})).expect("config must parse");
        let brand = config.brand("review").expect("brand must exist");
        assert_eq!(config.public_url, "https://node.example");
        assert_eq!(config.interface_url(brand), "https://node.example/a2a/review");
        assert_eq!(
            config.oauth_metadata_url(brand),
            "https://node.example/a2a/review/oauth/.well-known/oauth-authorization-server",
        );
        assert_eq!(config.jwks_url(), "https://node.example/a2a/jwks.json");
    }

    #[test]
    fn unsafe_urls_ids_and_scopes_are_refused() {
        for (name, value, error) in [
            (
                "http",
                file(|v| v["publicUrl"] = "http://node.example".into()),
                PactConfigError::Field("publicUrl"),
            ),
            (
                "query",
                file(|v| v["publicUrl"] = "https://node.example/?a=1".into()),
                PactConfigError::Field("publicUrl"),
            ),
            (
                "audience",
                file(|v| v["audience"] = "a b".into()),
                PactConfigError::Field("audience"),
            ),
            (
                "no brands",
                file(|v| v["brands"] = serde_json::json!([])),
                PactConfigError::Field("brands"),
            ),
            (
                "brand id",
                file(|v| v["brands"][0]["id"] = "a/b".into()),
                PactConfigError::Brand(0),
            ),
            (
                "scope id",
                file(|v| v["brands"][0]["scope"]["id"] = "a b".into()),
                PactConfigError::Brand(0),
            ),
            (
                "access mode",
                file(|v| v["brands"][0]["accessMode"] = "all".into()),
                PactConfigError::Brand(0),
            ),
        ] {
            assert_eq!(parse(value).err(), Some(error), "{name}");
        }
        let twice = file(|v| {
            let brand = v["brands"][0].clone();
            v["brands"].as_array_mut().expect("array").push(brand);
        });
        assert_eq!(parse(twice).err(), Some(PactConfigError::Brand(1)));
        assert!(
            parse(file(|v| v["publicUrl"] = "http://127.0.0.1:8080".into())).is_ok()
        );
    }
}
