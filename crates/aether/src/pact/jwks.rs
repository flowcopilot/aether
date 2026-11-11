//! Personal-agent JWT verification (spec §3.2) for an open registry.
//!
//! Any issuer that serves a JWKS may call the node, so the node fetches URLs
//! its callers choose. The fetch is guarded: HTTPS only, no redirects, no
//! proxy, no private or local address, bounded size and time, a cache, and a
//! global fetch rate.

use aether_core::time::now;
use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm},
};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;

/// The largest discovery document or JWKS the node reads.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
/// How long one discovery or JWKS request may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long fetched keys are used before they are fetched again.
const CACHE_SECONDS: i64 = 5 * 60;
/// The shortest time between two fetches for one issuer, so an unknown
/// `kid` cannot make the node fetch on every request.
const REFETCH_SECONDS: i64 = 30;
const MAX_CACHED_ISSUERS: usize = 1024;
/// The clock skew §3.2 allows.
const SKEW_SECONDS: i64 = 30;
/// The longest lifetime §3.2 allows.
const MAX_LIFETIME_SECONDS: i64 = 300;

/// Who called: the personal agent and its user (§3.3).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PaIdentity {
    pub issuer: String,
    pub subject: String,
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum JwksError {
    #[error("the issuer URL is not allowed")]
    Url,
    #[error("the issuer keys cannot be fetched")]
    Fetch,
    #[error("the issuer keys are invalid")]
    Invalid,
}

/// Where an issuer's keys come from. A trait so tests need no network.
pub trait JwksSource: Send + Sync {
    fn fetch<'a>(
        &'a self,
        issuer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<JwkSet, JwksError>> + Send + 'a>>;
}

/// Which issuer URLs the node may fetch from.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct UrlPolicy {
    /// Allows `http://localhost` and `http://127.0.0.1` for local tests.
    pub allow_insecure_local: bool,
}

impl UrlPolicy {
    /// Whether the node may fetch `url`. An HTTPS URL must name a host that
    /// is not a local or private address literal; the resolver checks the
    /// addresses of a name when it connects.
    #[must_use]
    pub fn allows(self, url: &Url) -> bool {
        if url.username() != "" || url.password().is_some() || url.fragment().is_some()
        {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        // An IPv6 literal keeps its brackets in `host_str`.
        let literal =
            host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok();
        let local = literal.map_or_else(
            || host.eq_ignore_ascii_case("localhost"),
            |ip| ip.is_loopback(),
        );
        match url.scheme() {
            "https" => {
                literal.map_or(!local, is_public)
                    || (local && self.allow_insecure_local)
            }
            "http" => local && self.allow_insecure_local,
            _ => false,
        }
    }
}

/// Whether `ip` is an address on the public internet: not loopback,
/// private, link-local, shared, documentation, benchmarking, reserved,
/// multicast, or unspecified.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => {
            ip.to_ipv4_mapped().map_or_else(|| is_public_v6(ip), is_public_v4)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..=19).contains(&b))
        || a >= 240)
}

const fn is_public_v6(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && second == 0x0db8)
        || (first == 0x0064 && second == 0xff9b))
}

/// Resolves names and drops every address that is not public, so a name
/// that points at the node's own network cannot be fetched. It runs when
/// the client connects, which also covers a name that changes its answer
/// between two lookups.
struct PublicResolver {
    allow_loopback: bool,
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_loopback = self.allow_loopback;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| {
                    is_public(address.ip())
                        || (allow_loopback && address.ip().is_loopback())
                })
                .collect::<Vec<SocketAddr>>();
            if addresses.is_empty() {
                return Err("the name has no public address".into());
            }
            let addresses: reqwest::dns::Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

/// Fetches keys over HTTPS: OIDC discovery first, then
/// `{iss}/.well-known/jwks.json`.
pub struct HttpJwksSource {
    client: Client,
    policy: UrlPolicy,
}

impl HttpJwksSource {
    pub fn new(policy: UrlPolicy) -> Result<Self, JwksError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(FETCH_TIMEOUT)
            .dns_resolver(PublicResolver {
                allow_loopback: policy.allow_insecure_local,
            })
            .build()
            .map_err(|_| JwksError::Fetch)?;
        Ok(Self { client, policy })
    }

    async fn document(&self, url: &Url) -> Result<Option<Vec<u8>>, JwksError> {
        if !self.policy.allows(url) {
            return Err(JwksError::Url);
        }
        let mut response =
            self.client.get(url.clone()).send().await.map_err(|_| JwksError::Fetch)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(JwksError::Fetch);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| JwksError::Fetch)? {
            if body.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
                return Err(JwksError::Invalid);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }
}

#[derive(Deserialize)]
struct Discovery {
    issuer: Option<String>,
    jwks_uri: String,
}

impl JwksSource for HttpJwksSource {
    fn fetch<'a>(
        &'a self,
        issuer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<JwkSet, JwksError>> + Send + 'a>> {
        Box::pin(async move {
            let base = issuer.trim_end_matches('/');
            let discovery_url =
                Url::parse(&format!("{base}/.well-known/openid-configuration"))
                    .map_err(|_| JwksError::Url)?;
            let jwks_url = match self.document(&discovery_url).await? {
                Some(body) => {
                    let discovery: Discovery = serde_json::from_slice(&body)
                        .map_err(|_| JwksError::Invalid)?;
                    // OIDC Discovery §4.3: the document names its own issuer.
                    if discovery.issuer.is_some_and(|named| named != issuer) {
                        return Err(JwksError::Invalid);
                    }
                    Url::parse(&discovery.jwks_uri).map_err(|_| JwksError::Url)?
                }
                None => Url::parse(&format!("{base}/.well-known/jwks.json"))
                    .map_err(|_| JwksError::Url)?,
            };
            let body = self.document(&jwks_url).await?.ok_or(JwksError::Fetch)?;
            serde_json::from_slice(&body).map_err(|_| JwksError::Invalid)
        })
    }
}

struct CachedKeys {
    keys: JwkSet,
    fetched_at: i64,
}

/// Verifies personal-agent JWTs.
pub struct PaVerifier {
    audience: String,
    policy: UrlPolicy,
    source: Arc<dyn JwksSource>,
    cache: Mutex<HashMap<String, CachedKeys>>,
    fetches: DefaultDirectRateLimiter,
}

#[derive(Deserialize)]
struct UnverifiedIssuer {
    iss: String,
}

#[derive(Deserialize)]
struct PaClaims {
    /// `iss` and `aud` are typed as one string so that a list is refused:
    /// `Validation` accepts a list that holds the expected value. It
    /// checks the value itself.
    #[expect(dead_code, reason = "deserialized only to refuse a list")]
    iss: String,
    sub: String,
    #[expect(dead_code, reason = "deserialized only to refuse a list")]
    aud: String,
    iat: i64,
    exp: i64,
}

impl PaVerifier {
    #[must_use]
    pub fn new(
        audience: String,
        policy: UrlPolicy,
        source: Arc<dyn JwksSource>,
    ) -> Self {
        // Twenty fetches at once, then ten a second, for all issuers
        // together: a flood of new issuers cannot turn the node into a
        // request amplifier.
        let quota = Quota::per_second(NonZeroU32::MIN.saturating_add(9))
            .allow_burst(NonZeroU32::MIN.saturating_add(19));
        Self {
            audience,
            policy,
            source,
            cache: Mutex::new(HashMap::new()),
            fetches: RateLimiter::direct(quota),
        }
    }

    /// Verifies the `Authorization` header value. `None` for every failure,
    /// which the caller answers with `401` (§3.4).
    pub async fn verify(&self, authorization: Option<&str>) -> Option<PaIdentity> {
        let token = bearer(authorization?)?;
        let header = jsonwebtoken::decode_header(token).ok()?;
        if !matches!(header.alg, Algorithm::ES256 | Algorithm::RS256) {
            return None;
        }
        let issuer =
            jsonwebtoken::dangerous::insecure_decode_claims::<UnverifiedIssuer>(token)
                .ok()?
                .iss;
        if !self.policy.allows(&Url::parse(&issuer).ok()?) {
            return None;
        }
        let now = now();
        let mut keys = self.keys(&issuer, now, false).await?;
        let mut key = select_key(&keys, header.kid.as_deref(), header.alg);
        if key.is_none() && header.kid.is_some() {
            keys = self.keys(&issuer, now, true).await?;
            key = select_key(&keys, header.kid.as_deref(), header.alg);
        }
        let key = key?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "iat", "exp"]);
        validation.leeway = SKEW_SECONDS.unsigned_abs();
        let claims =
            jsonwebtoken::decode::<PaClaims>(token, &key, &validation).ok()?.claims;
        // `validation` already required `iss` and `aud` and matched them.
        let lifetime = claims.exp.checked_sub(claims.iat)?;
        if claims.iat > now.saturating_add(SKEW_SECONDS)
            || !(0..=MAX_LIFETIME_SECONDS).contains(&lifetime)
            || claims.sub.is_empty()
        {
            return None;
        }
        Some(PaIdentity { issuer, subject: claims.sub })
    }

    /// The issuer's keys: cached, or fetched when the cache is cold or
    /// stale, or when `refetch` asks and the last fetch is old enough.
    async fn keys(&self, issuer: &str, now: i64, refetch: bool) -> Option<JwkSet> {
        let cached = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.get(issuer).map(|entry| (entry.keys.clone(), entry.fetched_at))
        };
        if let Some((keys, fetched_at)) = &cached {
            let age = now.saturating_sub(*fetched_at);
            if age < CACHE_SECONDS && (!refetch || age < REFETCH_SECONDS) {
                return if refetch { None } else { Some(keys.clone()) };
            }
        }
        if self.fetches.check().is_err() {
            return None;
        }
        let keys = self.source.fetch(issuer).await.ok()?;
        self.remember(issuer, keys.clone(), now);
        Some(keys)
    }

    /// Caches `keys`, dropping the oldest issuer when the cache is full.
    fn remember(&self, issuer: &str, keys: JwkSet, now: i64) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= MAX_CACHED_ISSUERS && !cache.contains_key(issuer) {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(issuer, _)| issuer.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(issuer.to_owned(), CachedKeys { keys, fetched_at: now });
    }
}

fn bearer(authorization: &str) -> Option<&str> {
    let (scheme, token) = authorization.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// The key for `alg`: the one named `kid`, or, without a `kid`, the only
/// key of the right type. A key that names another algorithm is not used.
fn select_key(keys: &JwkSet, kid: Option<&str>, alg: Algorithm) -> Option<DecodingKey> {
    let fits = |jwk: &&Jwk| {
        let family = match (&jwk.algorithm, alg) {
            (AlgorithmParameters::EllipticCurve(ec), Algorithm::ES256) => {
                ec.curve == EllipticCurve::P256
            }
            (AlgorithmParameters::RSA(_), Algorithm::RS256) => true,
            _ => false,
        };
        let named = jwk.common.key_algorithm.is_none_or(|named| {
            matches!(
                (named, alg),
                (KeyAlgorithm::ES256, Algorithm::ES256)
                    | (KeyAlgorithm::RS256, Algorithm::RS256)
            )
        });
        family && named
    };
    let mut candidates =
        keys.keys.iter().filter(fits).filter(|jwk| {
            kid.is_none_or(|kid| jwk.common.key_id.as_deref() == Some(kid))
        });
    let key = candidates.next()?;
    if kid.is_none() && candidates.next().is_some() {
        return None;
    }
    DecodingKey::from_jwk(key).ok()
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::missing_panics_doc,
    reason = "test clocks move by seconds, and test helpers panic on purpose"
)]
pub mod tests {
    use super::{
        HttpJwksSource, JwksError, JwksSource, PaIdentity, PaVerifier, UrlPolicy,
        is_public,
    };
    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, jwk::JwkSet};
    use std::{
        future::Future,
        net::IpAddr,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    pub const ISSUER: &str = "https://pa.example";
    pub const AUDIENCE: &str = "https://node.example";

    /// A personal agent's signing key, for tests.
    pub struct TestAgent {
        pub encoding: EncodingKey,
        pub jwks: JwkSet,
        pub kid: String,
    }

    impl TestAgent {
        #[must_use]
        pub fn new(kid: &str) -> Self {
            let document = EcdsaKeyPair::generate_pkcs8(
                &ECDSA_P256_SHA256_FIXED_SIGNING,
                &SystemRandom::new(),
            )
            .expect("key must generate");
            let pair = EcdsaKeyPair::from_pkcs8(
                &ECDSA_P256_SHA256_FIXED_SIGNING,
                document.as_ref(),
            )
            .expect("key must parse");
            let point = pair.public_key().as_ref();
            let jwks = serde_json::from_value(serde_json::json!({
                "keys": [{
                    "kty": "EC",
                    "crv": "P-256",
                    "alg": "ES256",
                    "kid": kid,
                    "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                    "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
                }]
            }))
            .expect("jwks must parse");
            Self {
                encoding: EncodingKey::from_ec_der(document.as_ref()),
                jwks,
                kid: kid.to_owned(),
            }
        }

        /// A token with `claims`, signed with this key.
        #[must_use]
        pub fn token(&self, claims: &serde_json::Value) -> String {
            let mut header = Header::new(Algorithm::ES256);
            header.kid = Some(self.kid.clone());
            jsonwebtoken::encode(&header, claims, &self.encoding)
                .expect("token must sign")
        }

        /// A valid token for `sub`, issued now.
        #[must_use]
        pub fn token_for(&self, sub: &str) -> String {
            let now = super::now();
            self.token(&serde_json::json!({
                "iss": ISSUER, "sub": sub, "aud": AUDIENCE, "iat": now, "exp": now + 120,
            }))
        }
    }

    /// Serves fixed keys and counts the fetches.
    pub struct StaticKeys {
        pub keys: std::sync::Mutex<JwkSet>,
        pub fetches: AtomicUsize,
    }

    impl StaticKeys {
        #[must_use]
        pub fn new(keys: JwkSet) -> Arc<Self> {
            Arc::new(Self {
                keys: std::sync::Mutex::new(keys),
                fetches: AtomicUsize::new(0),
            })
        }
    }

    impl JwksSource for StaticKeys {
        fn fetch<'a>(
            &'a self,
            _issuer: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<JwkSet, JwksError>> + Send + 'a>>
        {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let keys = self.keys.lock().expect("keys lock").clone();
            Box::pin(async move { Ok(keys) })
        }
    }

    fn verifier(source: Arc<StaticKeys>) -> PaVerifier {
        PaVerifier::new(
            AUDIENCE.to_owned(),
            UrlPolicy { allow_insecure_local: false },
            source,
        )
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[tokio::test]
    async fn a_valid_token_names_the_agent_and_its_user() {
        let agent = TestAgent::new("k1");
        let verifier = verifier(StaticKeys::new(agent.jwks.clone()));
        assert_eq!(
            verifier.verify(Some(&bearer(&agent.token_for("user-1")))).await,
            Some(PaIdentity {
                issuer: ISSUER.to_owned(),
                subject: "user-1".to_owned()
            }),
        );
    }

    #[tokio::test]
    async fn every_rule_of_section_three_two_refuses_a_bad_token() {
        let agent = TestAgent::new("k1");
        let other = TestAgent::new("k1");
        let verifier = verifier(StaticKeys::new(agent.jwks.clone()));
        let now = super::now();
        let claims = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut claims = serde_json::json!({
                "iss": ISSUER, "sub": "user-1", "aud": AUDIENCE, "iat": now, "exp": now + 120,
            });
            change(&mut claims);
            claims
        };
        let refused = [
            ("missing", None),
            ("not bearer", Some(format!("Basic {}", agent.token_for("user-1")))),
            ("bad signature", Some(bearer(&other.token_for("user-1")))),
            (
                "wrong audience",
                Some(bearer(
                    &agent
                        .token(&claims(&|c| c["aud"] = "https://other.example".into())),
                )),
            ),
            (
                "audience list",
                Some(bearer(
                    &agent
                        .token(&claims(&|c| c["aud"] = serde_json::json!([AUDIENCE]))),
                )),
            ),
            (
                "issuer list",
                Some(bearer(
                    &agent.token(&claims(&|c| c["iss"] = serde_json::json!([ISSUER]))),
                )),
            ),
            (
                "expired",
                Some(bearer(&agent.token(&claims(&|c| {
                    c["iat"] = (now - 400).into();
                    c["exp"] = (now - 100).into();
                })))),
            ),
            (
                "issued in the future",
                Some(bearer(&agent.token(&claims(&|c| {
                    c["iat"] = (now + 60).into();
                    c["exp"] = (now + 120).into();
                })))),
            ),
            (
                "lives too long",
                Some(bearer(&agent.token(&claims(&|c| c["exp"] = (now + 301).into())))),
            ),
            (
                "no sub",
                Some(bearer(&agent.token(&claims(&|c| {
                    c.as_object_mut().expect("object").remove("sub");
                })))),
            ),
            (
                "empty sub",
                Some(bearer(&agent.token(&claims(&|c| c["sub"] = "".into())))),
            ),
            (
                "no iat",
                Some(bearer(&agent.token(&claims(&|c| {
                    c.as_object_mut().expect("object").remove("iat");
                })))),
            ),
            (
                "http issuer",
                Some(bearer(
                    &agent.token(&claims(&|c| c["iss"] = "http://pa.example".into())),
                )),
            ),
            (
                "local issuer",
                Some(bearer(
                    &agent.token(&claims(&|c| c["iss"] = "https://127.0.0.1".into())),
                )),
            ),
        ];
        for (name, authorization) in refused {
            assert_eq!(verifier.verify(authorization.as_deref()).await, None, "{name}");
        }
        let hs256 = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims(&|_| {}),
            &EncodingKey::from_secret(b"secret"),
        )
        .expect("token must sign");
        assert_eq!(verifier.verify(Some(&bearer(&hs256))).await, None, "HS256");
    }

    #[tokio::test]
    async fn keys_are_cached_and_an_unknown_kid_refetches_at_most_once() {
        let agent = TestAgent::new("k1");
        let source = StaticKeys::new(agent.jwks.clone());
        let verifier = verifier(Arc::clone(&source));
        for _ in 0..3 {
            assert!(
                verifier.verify(Some(&bearer(&agent.token_for("u")))).await.is_some()
            );
        }
        assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
        let rotated = TestAgent::new("k2");
        for _ in 0..3 {
            assert!(
                verifier.verify(Some(&bearer(&rotated.token_for("u")))).await.is_none()
            );
        }
        // The cache is fresh, so an unknown kid does not refetch yet.
        assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_public_https_issuers_are_fetched() {
        let strict = UrlPolicy { allow_insecure_local: false };
        let local = UrlPolicy { allow_insecure_local: true };
        let parse = |url: &str| reqwest::Url::parse(url).expect("URL must parse");
        assert!(strict.allows(&parse("https://pa.example/")));
        for refused in [
            "http://pa.example/",
            "https://localhost/",
            "https://127.0.0.1/",
            "https://10.0.0.1/",
            "https://[::1]/",
            "https://169.254.169.254/",
            "https://user@pa.example/",
            "file:///etc/passwd",
        ] {
            assert!(!strict.allows(&parse(refused)), "{refused}");
        }
        assert!(local.allows(&parse("http://localhost:3001/")));
        assert!(local.allows(&parse("http://127.0.0.1:3001/")));
        assert!(!local.allows(&parse("http://10.0.0.1/")));
    }

    #[test]
    fn non_public_addresses_are_recognized() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "64:ff9b::1",
        ] {
            let ip: IpAddr = address.parse().expect("address must parse");
            assert!(!is_public(ip), "{address}");
        }
        for address in ["1.1.1.1", "2606:4700:4700::1111"] {
            let ip: IpAddr = address.parse().expect("address must parse");
            assert!(is_public(ip), "{address}");
        }
    }

    /// Serves `routes` on a loopback port and returns its base URL.
    async fn serve(routes: axum::Router) -> String {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("port must bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move { axum::serve(listener, routes).await });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn the_http_source_follows_discovery_then_falls_back_to_the_well_known_jwks()
    {
        use axum::{Json, routing::get};
        let agent = TestAgent::new("k1");
        let keys = serde_json::to_value(&agent.jwks).expect("jwks must encode");
        let source = HttpJwksSource::new(UrlPolicy { allow_insecure_local: true })
            .expect("client");

        let fallback = serve(axum::Router::new().route(
            "/.well-known/jwks.json",
            get({
                let keys = keys.clone();
                move || async move { Json(keys) }
            }),
        ))
        .await;
        assert_eq!(source.fetch(&fallback).await.expect("keys must fetch"), agent.jwks);

        let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
        let receiver = std::sync::Arc::new(tokio::sync::Mutex::new(Some(receiver)));
        let base = std::sync::Arc::new(tokio::sync::OnceCell::<String>::new());
        let discovered = serve(
            axum::Router::new()
                .route(
                    "/.well-known/openid-configuration",
                    get({
                        let base = std::sync::Arc::clone(&base);
                        let receiver = std::sync::Arc::clone(&receiver);
                        move || async move {
                            let base = base
                                .get_or_init(|| async move {
                                    receiver
                                        .lock()
                                        .await
                                        .take()
                                        .expect("once")
                                        .await
                                        .expect("base")
                                })
                                .await;
                            Json(serde_json::json!({
                                "issuer": base,
                                "jwks_uri": format!("{base}/keys"),
                            }))
                        }
                    }),
                )
                .route("/keys", get(move || async move { Json(keys) })),
        )
        .await;
        sender.send(discovered.clone()).expect("base must send");
        assert_eq!(
            source.fetch(&discovered).await.expect("keys must fetch"),
            agent.jwks
        );

        let large = serve(axum::Router::new().route(
            "/.well-known/jwks.json",
            get(|| async { "x".repeat(super::MAX_DOCUMENT_BYTES + 1) }),
        ))
        .await;
        assert_eq!(source.fetch(&large).await.err(), Some(JwksError::Invalid));
    }

    #[tokio::test]
    async fn the_http_source_refuses_a_local_issuer_before_any_request() {
        let source = HttpJwksSource::new(UrlPolicy { allow_insecure_local: false })
            .expect("client");
        assert_eq!(
            source.fetch("http://127.0.0.1:9").await.err(),
            Some(JwksError::Url)
        );
    }
}
