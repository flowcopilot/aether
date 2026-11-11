//! The PACT gateway: a personal agent with its own JWKS, a real `node serve`
//! with the fake Codex, consent through the real admin UI, a delegated turn
//! with a verified receipt, and revocation.

use std::{
    fmt,
    io::BufReader,
    process::{ChildStdout, Stdio},
    time::{Duration, SystemTime},
};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cucumber::{given, then, when};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, jwk::JwkSet,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use aether::fixture::{PolicyLimits, policy_json};

use crate::common::{
    FakeCodex, ServerProcess, TestNodeDir, aether_command, blocking, read_events,
};

/// The scope that the code review offer grants.
const SCOPE: &str = "review:read_only";

#[derive(Default)]
pub struct State {
    agent: Option<PersonalAgent>,
    node: Option<Node>,
    /// The personal agent's plain HTTP client.
    http: reqwest::Client,
    /// The owner's browser on the admin UI.
    browser: Option<reqwest::Client>,
    /// The codes of the device flow that the personal agent started.
    device_code: Option<String>,
    user_code: Option<String>,
    /// The session cookie and the HTML of the consent page.
    cookie: Option<String>,
    page: Option<String>,
    /// The access token that the personal agent got.
    access: Option<String>,
    /// The last JSON body that the node answered.
    answer: Option<Value>,
    /// The claims of the last receipt, after verification.
    claims: Option<Value>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("agent", &self.agent.as_ref().map(|agent| &agent.issuer))
            .field("node", &self.node.as_ref().map(|node| &node.brand))
            .field("user_code", &self.user_code)
            .field("answer", &self.answer)
            .field("claims", &self.claims)
            .finish_non_exhaustive()
    }
}

impl State {
    const fn agent(&self) -> &PersonalAgent {
        self.agent.as_ref().expect("a personal agent must exist")
    }

    const fn node(&self) -> &Node {
        self.node.as_ref().expect("a node must run")
    }

    const fn answer(&self) -> &Value {
        self.answer.as_ref().expect("the node must have answered")
    }

    fn browser(&mut self) -> reqwest::Client {
        self.browser
            .get_or_insert_with(|| {
                reqwest::Client::builder()
                    .danger_accept_invalid_certs(true)
                    .build()
                    .expect("browser must build")
            })
            .clone()
    }

    /// A token that the personal agent signs for the node.
    fn token(&self) -> String {
        self.agent().token("user-1", &self.node().public)
    }

    /// Sends a review request, with the delegated access when given.
    async fn send(
        &self,
        delegation: Option<&str>,
        message_id: &str,
    ) -> reqwest::Response {
        let mut request = self
            .http
            .post(format!("{}/message:send", self.node().brand))
            .bearer_auth(self.token())
            .json(&json!({ "message": {
                "messageId": message_id, "role": "ROLE_USER", "parts": [{ "text": "Review the code" }],
            }}));
        if let Some(delegation) = delegation {
            request =
                request.header("X-A2A-User-Delegation", format!("Bearer {delegation}"));
        }
        request.send().await.expect("send")
    }

    /// Polls the token endpoint of the device flow once.
    async fn poll(&self) -> Value {
        let issuer = self.agent().issuer.clone();
        self.http
            .post(format!("{}/oauth/token", self.node().brand))
            .bearer_auth(self.token())
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                (
                    "device_code",
                    self.device_code
                        .as_deref()
                        .expect("a device flow must have started"),
                ),
                ("client_id", issuer.as_str()),
            ])
            .send()
            .await
            .expect("poll")
            .json()
            .await
            .expect("JSON")
    }

    async fn start_device_flow(&mut self) {
        let issuer = self.agent().issuer.clone();
        let started: Value = self
            .http
            .post(format!("{}/oauth/device_authorization", self.node().brand))
            .bearer_auth(self.token())
            .form(&[("client_id", issuer.as_str()), ("scope", SCOPE)])
            .send()
            .await
            .expect("device authorization")
            .json()
            .await
            .expect("JSON");
        self.device_code =
            Some(started["device_code"].as_str().expect("device code").to_owned());
        self.user_code =
            Some(started["user_code"].as_str().expect("user code").to_owned());
    }

    async fn open_consent_page(&mut self) {
        let browser = self.browser();
        let page = browser
            .get(format!(
                "{}/pact/consent?user_code={}",
                self.node().admin,
                self.user_code.as_deref().expect("a device flow must have started")
            ))
            .send()
            .await
            .expect("consent page");
        let cookie = page
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .collect::<Vec<_>>()
            .join("; ");
        self.cookie = Some(cookie);
        self.page = Some(page.text().await.expect("page text"));
    }

    /// Sends a form from the owner's admin session.
    async fn submit(&mut self, path: &str, form: &[(&str, &str)]) -> reqwest::Response {
        let browser = self.browser();
        let admin = &self.node().admin;
        browser
            .post(format!("{admin}{path}"))
            .header(
                reqwest::header::COOKIE,
                self.cookie.as_deref().expect("the owner must have a session"),
            )
            .header(reqwest::header::ORIGIN, admin)
            .form(form)
            .send()
            .await
            .expect("form must send")
    }

    async fn allow(&mut self) {
        self.open_consent_page().await;
        let user_code =
            self.user_code.clone().expect("a device flow must have started");
        let decided = self
            .submit(
                "/pact/consent",
                &[
                    ("user_code", user_code.as_str()),
                    ("decision", "allow"),
                    ("scope", SCOPE),
                ],
            )
            .await
            .text()
            .await
            .expect("decision text");
        self.page = Some(decided);
    }

    async fn collect_access(&mut self) {
        // The poll interval is five seconds; wait it out once.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let tokens = self.poll().await;
        assert_eq!(tokens["scope"], SCOPE, "{tokens}");
        self.access =
            Some(tokens["access_token"].as_str().expect("access token").to_owned());
    }

    async fn run_delegated_turn(&mut self) {
        let access = self.access.clone().expect("the agent must have access");
        let reply = self.send(Some(&access), "m2").await.json().await.expect("JSON");
        self.answer = Some(reply);
    }

    fn receipt(&self) -> &Value {
        &self.answer()["message"]["metadata"]["pact.receipt"]
    }
}

/// A personal agent: a P-256 key and the JWKS server that publishes it.
struct PersonalAgent {
    issuer: String,
    key: EncodingKey,
    jwks: tokio::task::JoinHandle<()>,
}

impl PersonalAgent {
    async fn start() -> Self {
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
        let jwks = json!({
            "keys": [{
                "kty": "EC", "crv": "P-256", "alg": "ES256", "kid": "pa-1",
                "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
            }]
        });
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("port must bind");
        let issuer = format!("http://{}", listener.local_addr().expect("address"));
        let routes = axum::Router::new().route(
            "/.well-known/jwks.json",
            axum::routing::get(move || async move { axum::Json(jwks) }),
        );
        let jwks = tokio::spawn(async move {
            axum::serve(listener, routes).await.expect("JWKS server must serve");
        });
        Self { issuer, key: EncodingKey::from_ec_der(document.as_ref()), jwks }
    }

    fn token(&self, sub: &str, audience: &str) -> String {
        let now = unix_now();
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("pa-1".to_owned());
        jsonwebtoken::encode(
            &header,
            &json!({ "iss": self.issuer, "sub": sub, "aud": audience, "iat": now, "exp": now + 120 }),
            &self.key,
        )
        .expect("token must sign")
    }
}

impl Drop for PersonalAgent {
    fn drop(&mut self) {
        self.jwks.abort();
    }
}

/// A `node serve` with the PACT gateway and the fake Codex.
struct Node {
    /// `None` after the operator stopped it.
    process: Option<ServerProcess>,
    /// The node's output. It stays open: the node fails when it cannot
    /// write its events.
    _output: BufReader<ChildStdout>,
    /// The public PACT address.
    public: String,
    /// The address of the code review offer.
    brand: String,
    /// The admin UI, without a trailing slash.
    admin: String,
    /// Holds the policy, the workspace and the data of the node.
    _directory: TempDir,
}

impl Node {
    /// Starts a node. The PACT address must be in the policy before the node
    /// binds it, so the port comes from `free_port`; another process can take
    /// it first, and then the node starts again on a new port.
    fn start() -> Self {
        for _ in 0..5 {
            if let Some(node) = Self::try_start(free_port()) {
                return node;
            }
        }
        panic!("the PACT gateway did not get a free port in 5 tries");
    }

    fn try_start(pact_port: u16) -> Option<Self> {
        let node_dir = TestNodeDir::new();
        let fake_codex = node_dir.write_fake_codex(FakeCodex::Standard);
        let public = format!("http://127.0.0.1:{pact_port}");
        let brand = format!("{public}/a2a/review");
        let limits = PolicyLimits {
            max_context_tokens: 4000,
            queue_global: 4,
            queue_per_peer: 2,
            ..PolicyLimits::default()
        };
        let mut policy = policy_json(&node_dir.workspace, &[], [0; 32], limits);
        policy["scope"] = json!({ "id": "direct", "kind": "direct" });
        policy["pact"] = json!({
            "publicUrl": public,
            "audience": public,
            "allowInsecureLocalIssuers": true,
            "brands": [{
                "id": "review",
                "name": "Code review",
                "description": "Reviews code in the owner's workspace.",
                "capability": "review",
                "workspace": "workspace",
                "accessMode": "read_only",
                "scope": { "id": SCOPE, "description": "Review code (read only)" },
            }],
        });
        let policy_path = node_dir.write_policy(&policy);
        let mut process = ServerProcess::spawn(
            aether_command()
                .args(["node", "serve", "--policy"])
                .arg(&policy_path)
                .arg("--data-dir")
                .arg(&node_dir.data_dir)
                .args(["--port", "0", "--admin-port", "0", "--pact-listen"])
                .arg(format!("127.0.0.1:{pact_port}"))
                .env("AETHER_CODEX_BIN", &fake_codex)
                .stdout(Stdio::piped()),
        );
        let mut output = process.stdout();
        // The node stops before `pact_listening` when the PACT port is taken.
        let events = read_events(&mut output, &["pact_listening", "admin_listening"])?;
        let admin = events["admin_listening"]["url"]
            .as_str()
            .expect("admin URL")
            .trim_end_matches('/')
            .to_owned();
        Some(Self {
            process: Some(process),
            _output: output,
            public,
            brand,
            admin,
            _directory: node_dir.temp,
        })
    }
}

fn unix_now() -> i64 {
    aether_core::time::unix_timestamp(SystemTime::now()).expect("clock")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("port must bind")
        .port()
}

/// `application/x-www-form-urlencoded` bodies; this build of `reqwest`
/// has no `form` feature.
trait FormExt {
    fn form(self, pairs: &[(&str, &str)]) -> Self;
}

impl FormExt for reqwest::RequestBuilder {
    fn form(self, pairs: &[(&str, &str)]) -> Self {
        let encode = |value: &str| {
            value
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                        char::from(byte).to_string()
                    } else {
                        format!("%{byte:02X}")
                    }
                })
                .collect::<String>()
        };
        let body = pairs
            .iter()
            .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
    }
}

// Setup.

#[given("a personal agent with its own signing keys")]
async fn personal_agent(world: &mut super::World) {
    world.pact.agent = Some(PersonalAgent::start().await);
}

#[given("a node that offers code review to personal agents")]
async fn node_offers_review(world: &mut super::World) {
    world.pact.node = Some(blocking(Node::start).await);
}

#[given("the owner gave the personal agent read-only review access")]
async fn owner_gave_access(world: &mut super::World) {
    let pact = &mut world.pact;
    pact.start_device_flow().await;
    pact.allow().await;
    let decided = pact.page.as_deref().expect("decision page");
    assert!(decided.contains("Access approved"), "{decided}");
    pact.collect_access().await;
}

// The agent card.

#[when("the personal agent reads the agent card for code review")]
async fn read_card(world: &mut super::World) {
    let pact = &mut world.pact;
    let card: Value = pact
        .http
        .get(format!("{}/.well-known/agent-card.json", pact.node().brand))
        .send()
        .await
        .expect("card must load")
        .json()
        .await
        .expect("card must be JSON");
    pact.answer = Some(card);
}

#[then("the card gives the address of the code review offer")]
fn card_gives_address(world: &mut super::World) {
    let pact = &world.pact;
    assert_eq!(
        pact.answer()["supportedInterfaces"][0]["url"],
        pact.node().brand.as_str()
    );
}

// A turn without consent.

#[when("the personal agent asks for a review without access")]
async fn review_without_access(world: &mut super::World) {
    let pact = &mut world.pact;
    let step_up = pact.send(None, "m1").await.json().await.expect("JSON");
    pact.answer = Some(step_up);
}

#[then("the node says that the owner must give consent")]
fn consent_required(world: &mut super::World) {
    let answer = world.pact.answer();
    assert_eq!(
        answer["task"]["status"]["state"], "TASK_STATE_AUTH_REQUIRED",
        "{answer}"
    );
}

#[then("the answer links to the consent page of the admin UI")]
fn links_to_consent(world: &mut super::World) {
    let pact = &world.pact;
    let link = pact.answer()["task"]["metadata"]["pact.verificationUriComplete"]
        .as_str()
        .expect("link");
    assert!(
        link.starts_with(&format!("{}/pact/consent?user_code=", pact.node().admin)),
        "{link}"
    );
}

// Consent.

#[given("the personal agent asks the owner for read-only review access")]
#[when("the personal agent asks the owner for read-only review access")]
async fn ask_for_access(world: &mut super::World) {
    world.pact.start_device_flow().await;
}

#[then("the request for access is still pending")]
async fn access_pending(world: &mut super::World) {
    let pending = world.pact.poll().await;
    assert_eq!(pending["error"], "authorization_pending", "{pending}");
}

#[when("the owner opens the consent page")]
async fn open_consent_page(world: &mut super::World) {
    world.pact.open_consent_page().await;
}

#[then(expr = "the page shows the access {string}")]
fn page_shows_access(world: &mut super::World, access: String) {
    let page = world.pact.page.as_deref().expect("a page must be open");
    assert!(page.contains(&access), "{page}");
}

#[then("the page names the personal agent")]
fn page_names_agent(world: &mut super::World) {
    let pact = &world.pact;
    let page = pact.page.as_deref().expect("a page must be open");
    assert!(
        page.contains(&pact.agent().issuer),
        "the page must name the agent: {page}"
    );
}

#[when("the owner allows the request on the consent page")]
async fn owner_allows(world: &mut super::World) {
    world.pact.allow().await;
}

#[then(expr = "the page says {string}")]
fn page_says(world: &mut super::World, text: String) {
    let page = world.pact.page.as_deref().expect("a page must be open");
    assert!(page.contains(&text), "{page}");
}

#[then("the personal agent gets read-only review access")]
async fn agent_gets_access(world: &mut super::World) {
    world.pact.collect_access().await;
}

// A delegated turn.

#[given("the personal agent asked for a review with its access")]
#[when("the personal agent asks for a review with its access")]
async fn review_with_access(world: &mut super::World) {
    world.pact.run_delegated_turn().await;
}

#[then("the reply is the result from Codex")]
fn reply_from_codex(world: &mut super::World) {
    let reply = world.pact.answer();
    assert_eq!(
        reply["message"]["parts"],
        json!([{ "text": "fake Codex result" }]),
        "{reply}"
    );
}

#[then("the reply has a receipt that the node's published keys verify")]
async fn receipt_verifies(world: &mut super::World) {
    let pact = &mut world.pact;
    let keys: JwkSet = pact
        .http
        .get(format!("{}/a2a/jwks.json", pact.node().public))
        .send()
        .await
        .expect("jwks")
        .json()
        .await
        .expect("jwks JSON");
    let key = DecodingKey::from_jwk(&keys.keys[0]).expect("provider key");
    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    let receipt = pact.receipt();
    let claims = jsonwebtoken::decode::<Value>(
        receipt["jws"].as_str().expect("jws"),
        &key,
        &validation,
    )
    .expect("receipt must verify")
    .claims;
    assert_eq!(claims, receipt["claims"]);
    pact.claims = Some(claims);
}

#[then("the receipt names the personal agent and the code review offer")]
fn receipt_names(world: &mut super::World) {
    let pact = &world.pact;
    let claims = pact.claims.as_ref().expect("the receipt must be verified");
    assert_eq!(claims["pa"], pact.agent().issuer.as_str());
    assert_eq!(claims["brand"], pact.node().brand.as_str());
    assert!(claims["grantId"].is_string(), "{claims}");
}

// Revocation.

#[when("the owner revokes the access on the sharing page")]
async fn owner_revokes(world: &mut super::World) {
    let pact = &mut world.pact;
    let grant_id =
        pact.receipt()["claims"]["grantId"].as_str().expect("grant id").to_owned();
    // The owner uses the admin session from the consent page.
    let revoked = pact
        .submit("/revoke", &[("grant_id", grant_id.as_str()), ("confirm", "yes")])
        .await;
    let status = revoked.status();
    let text = revoked.text().await.unwrap_or_default();
    assert!(status.is_success(), "{status}: {text}");
}

#[then("the personal agent can no longer get a review with that access")]
async fn access_refused(world: &mut super::World) {
    let pact = &world.pact;
    let access = pact.access.as_deref().expect("the agent must have had access");
    let refused = pact.send(Some(access), "m3").await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[then("the node stops cleanly when the operator interrupts it")]
async fn node_stops(world: &mut super::World) {
    let process = world
        .pact
        .node
        .as_mut()
        .and_then(|node| node.process.take())
        .expect("a node must run");
    let status = blocking(move || process.interrupt()).await;
    assert!(status.success(), "{status}");
}
