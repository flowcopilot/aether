//! The public listing of a service agent on the hub (`PUT /v1/listing` in
//! `protocol/hub/contract.json`).
//!
//! The listing reuses the text of the Agent Card, so the owner writes it one
//! time. It holds the full name, the description and the skill, and no
//! endpoint or address: everyone can read it. The node signs the listing
//! bytes with its endpoint key, and the hub returns them exactly as sent, so
//! a reader can check them against the endpoint that it gets later.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{agent_card::CardSettings, untrusted};

pub use crate::hub_client::protocol::{
    MAX_LISTING_BYTES, MAX_LISTING_DAYS, MAX_LISTING_DESCRIPTION as MAX_DESCRIPTION,
    MAX_LISTING_SKILL_DESCRIPTION as MAX_SKILL_DESCRIPTION,
    MAX_LISTING_SKILL_ID as MAX_SKILL_ID, MAX_LISTING_SKILL_NAME as MAX_SKILL_NAME,
    MAX_LISTING_SKILLS as MAX_SKILLS, MAX_LISTING_TAG as MAX_TAG,
    MAX_LISTING_TAGS as MAX_TAGS, OWNER_LINK_MAX_DAYS,
};

/// How long a new listing stays on the hub. The hub accepts 30 days at most.
pub const LISTING_LIFETIME: Duration =
    Duration::from_hours(MAX_LISTING_DAYS.saturating_mul(24));
/// A listing with less time left than this is signed again.
pub const RENEW_BEFORE: Duration = Duration::from_hours(7 * 24);

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum ListingError {
    #[error("the listing is not valid")]
    Invalid,
    #[error("the listing signature does not verify")]
    Signature,
    #[error("the listing has expired")]
    Expired,
}

/// A listing that passed the checks. Every text is from a stranger.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Listing {
    /// The full name, such as `law#0042`.
    pub name: String,
    pub description: String,
    pub skills: Vec<ListingSkill>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ListingSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// What this node last sent to the hub, kept under the setting
/// [`settings::ListingState`].
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListingState {
    pub listed: bool,
    /// Unix time.
    pub expires_at: u64,
    /// The base64url text of the listing bytes.
    pub listing: String,
}

/// The listing bytes for the card text `settings` and the full name
/// `full_name`.
#[must_use]
pub fn listing_bytes(settings: &CardSettings, full_name: &str) -> Vec<u8> {
    json!({
        "name": full_name,
        "description": settings.description,
        "skills": [{
            "id": crate::execution::GENERAL_SKILL,
            "name": settings.skill_name,
            "description": settings.skill_description,
            "tags": settings.tags,
        }],
    })
    .to_string()
    .into_bytes()
}

const fn wire_error(
    error: crate::hub_client::protocol::ListingWireError,
) -> ListingError {
    match error {
        crate::hub_client::protocol::ListingWireError::Invalid => ListingError::Invalid,
        crate::hub_client::protocol::ListingWireError::Signature => {
            ListingError::Signature
        }
        crate::hub_client::protocol::ListingWireError::Expired => ListingError::Expired,
    }
}

/// The signature of `bytes` until `expires_at` by the endpoint `key`, as
/// unpadded base64url.
#[must_use]
pub fn sign(key: &iroh::SecretKey, bytes: &[u8], expires_at: u64) -> String {
    crate::hub_client::protocol::sign_listing(key, bytes, expires_at)
}

/// Checks that the endpoint `endpoint` signed the listing `text` until
/// `expires_at`, and that `expires_at` is later than the Unix time `at`.
pub fn verify(
    text: &str,
    signature: &str,
    endpoint: &str,
    expires_at: u64,
    at: u64,
) -> Result<(), ListingError> {
    crate::hub_client::protocol::verify_listing(
        text, signature, endpoint, expires_at, at,
    )
    .map_err(wire_error)
}

/// How long an owner link lasts. The hub accepts 365 days at most.
pub const OWNER_LINK_LIFETIME: Duration =
    Duration::from_hours(OWNER_LINK_MAX_DAYS.saturating_mul(24));

/// The claim of the personal agent with the endpoint `key` for the service
/// agent `service_handle`, as unpadded base64url.
#[must_use]
pub fn sign_owner_claim(
    key: &iroh::SecretKey,
    service_handle: &str,
    expires_at: u64,
) -> String {
    let message = crate::hub_client::protocol::owner_claim_message(
        &key.public().to_string(),
        service_handle,
        expires_at,
    );
    URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes())
}

/// The confirm of the service agent with the endpoint `key` for the
/// personal agent `personal`, as unpadded base64url.
#[must_use]
pub fn sign_owner_confirm(
    key: &iroh::SecretKey,
    personal: &str,
    expires_at: u64,
) -> String {
    let message = crate::hub_client::protocol::owner_confirm_message(
        &key.public().to_string(),
        personal,
        expires_at,
    );
    URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes())
}

/// The listing in `text` (base64url) when it passes the hub's rules and
/// names `handle`. Every text is cleaned for display.
pub fn parse(text: &str, handle: &str) -> Result<Listing, ListingError> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingError::Invalid)?;
    let raw = crate::hub_client::protocol::parse_listing_bytes(&bytes, handle)
        .map_err(wire_error)?;
    Ok(Listing {
        name: untrusted::single_line(&raw.name, raw.name.chars().count()),
        description: untrusted::single_line(&raw.description, MAX_DESCRIPTION),
        skills: raw
            .skills
            .into_iter()
            .map(|skill| ListingSkill {
                id: untrusted::single_line(&skill.id, MAX_SKILL_ID),
                name: untrusted::single_line(&skill.name, MAX_SKILL_NAME),
                description: untrusted::single_line(
                    &skill.description,
                    MAX_SKILL_DESCRIPTION,
                ),
                tags: skill
                    .tags
                    .iter()
                    .map(|tag| untrusted::single_line(tag, MAX_TAG))
                    .collect(),
            })
            .collect(),
    })
}

pub mod settings {
    use aether_store::StoreError;

    use crate::settings::Setting;

    /// The listing that this node last sent to its hub as its public
    /// listing, if any. `None` forgets it.
    pub struct ListingState;

    impl Setting for ListingState {
        const KEY: &'static str = "node.listing_state";
        type Value = Option<super::ListingState>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(serde_json::from_str(&stored).ok())
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            value
                .as_ref()
                .map(|state| {
                    serde_json::to_string(state).map_err(|_| StoreError::InvalidState)
                })
                .transpose()
        }
    }
}

const RENEW_INTERVAL: std::time::Duration = std::time::Duration::from_hours(6);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum HubPublishError {
    NoName,
    Key,
    Store,
    Hub(crate::hub_client::HubError),
}

pub(crate) async fn publish_to_hub(
    data_dir: &std::path::Path,
    hub: &crate::hub_client::HubClient,
    now: u64,
) -> Result<ListingState, HubPublishError> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let name =
        crate::settings::setting::<crate::hub_client::settings::HubName>(data_dir)
            .ok()
            .flatten()
            .ok_or(HubPublishError::NoName)?;
    let settings = crate::agent_card::settings(data_dir);
    let bytes = listing_bytes(&settings, &name);
    let key = crate::data_dir::DataDir::new(data_dir)
        .endpoint_key()
        .map_err(|_| HubPublishError::Key)?;
    let expires_at = now.saturating_add(LISTING_LIFETIME.as_secs());
    let signature = sign(&key, &bytes, expires_at);
    let text = URL_SAFE_NO_PAD.encode(&bytes);
    hub.put_listing(&text, &signature, expires_at)
        .await
        .map_err(HubPublishError::Hub)?;
    let state = ListingState { listed: true, expires_at, listing: text };
    crate::settings::store_setting::<settings::ListingState>(
        data_dir,
        &Some(state.clone()),
    )
    .map_err(|_| HubPublishError::Store)?;
    Ok(state)
}

pub(crate) async fn stop_hub_listing(
    data_dir: &std::path::Path,
    hub: &crate::hub_client::HubClient,
) -> Result<(), HubPublishError> {
    hub.delete_listing().await.map_err(HubPublishError::Hub)?;
    crate::settings::store_setting::<settings::ListingState>(data_dir, &None)
        .map_err(|_| HubPublishError::Store)
}

pub(crate) const fn hub_publish_message(error: HubPublishError) -> &'static str {
    match error {
        HubPublishError::NoName => "Choose your name on Aether first.",
        HubPublishError::Key | HubPublishError::Store => {
            "The listing was not saved on this computer. Try again."
        }
        HubPublishError::Hub(crate::hub_client::HubError::Invalid) => {
            "The hub refused the listing. Check the text of your agent's card."
        }
        HubPublishError::Hub(error) => crate::hub_membership::hub_message(error),
    }
}

pub async fn listing_renewal_worker(
    ctx: std::sync::Arc<crate::node_serve_context::NodeServeContext>,
) {
    loop {
        renew_listing(&ctx).await;
        tokio::time::sleep(RENEW_INTERVAL).await;
    }
}

async fn renew_listing(ctx: &crate::node_serve_context::NodeServeContext) {
    use crate::hub_client::{AgentKind, HubAccess};
    use aether_core::time::now_seconds;

    let now = now_seconds();
    let due = crate::settings::setting::<settings::ListingState>(&ctx.data_dir)
        .ok()
        .flatten()
        .is_some_and(|state| {
            state.listed
                && state.expires_at.saturating_sub(now) < RENEW_BEFORE.as_secs()
        });
    let service = crate::settings::setting::<crate::hub_client::settings::AgentKind>(
        &ctx.data_dir,
    )
    .ok()
    .flatten()
        == Some(AgentKind::Service);
    if !(due && service) {
        return;
    }
    let hub = match ctx.hub.access() {
        HubAccess::Ready(client) => *client,
        HubAccess::Unavailable => {
            println!("{}", serde_json::json!({ "event": "hub_access_unavailable" }));
            return;
        }
        HubAccess::Off => return,
    };
    let event = match publish_to_hub(&ctx.data_dir, &hub, now).await {
        Ok(_) => "listing_renewed",
        Err(_) => "listing_renew_failed",
    };
    println!("{}", serde_json::json!({ "event": event }));
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::{Value, json};

    use super::{ListingError, listing_bytes, parse, sign, verify};
    use crate::agent_card::CardSettings;

    const NOW: u64 = 1_800_000_000;

    fn settings() -> CardSettings {
        CardSettings {
            description: "Reads contracts and answers questions about them.".to_owned(),
            skill_name: "Contracts".to_owned(),
            skill_description: "Reads a contract and lists its risks.".to_owned(),
            tags: vec!["law".to_owned(), "contracts".to_owned()],
            version: 3,
        }
    }

    fn encode(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn a_signed_listing_verifies_for_its_endpoint() {
        let key = iroh::SecretKey::generate();
        let bytes = listing_bytes(&settings(), "law#0042");
        let signature = sign(&key, &bytes, NOW + 60);
        let text = encode(&bytes);
        let endpoint = key.public().to_string();
        assert_eq!(verify(&text, &signature, &endpoint, NOW + 60, NOW), Ok(()));
        let other = iroh::SecretKey::generate().public().to_string();
        assert_eq!(
            verify(&text, &signature, &other, NOW + 60, NOW),
            Err(ListingError::Signature)
        );

        let listing = parse(&text, "law#0042").expect("the listing must parse");
        assert_eq!(listing.name, "law#0042");
        assert_eq!(listing.description, settings().description);
        assert_eq!(listing.skills.len(), 1);
        assert_eq!(listing.skills[0].id, "general");
        assert_eq!(listing.skills[0].tags, ["law", "contracts"]);
        // The bytes hold no endpoint and no address.
        assert!(!String::from_utf8_lossy(&bytes).contains(&endpoint));
    }

    #[test]
    fn a_changed_or_expired_listing_is_refused() {
        let key = iroh::SecretKey::generate();
        let endpoint = key.public().to_string();
        let bytes = listing_bytes(&settings(), "law#0042");
        let signature = sign(&key, &bytes, NOW + 60);
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert_eq!(
            verify(&encode(&changed), &signature, &endpoint, NOW + 60, NOW),
            Err(ListingError::Signature)
        );
        assert_eq!(
            verify(&encode(&bytes), &signature, &endpoint, NOW + 61, NOW),
            Err(ListingError::Signature)
        );
        assert_eq!(
            verify(&encode(&bytes), &signature, &endpoint, NOW + 60, NOW + 60),
            Err(ListingError::Expired)
        );
        assert_eq!(
            verify(&encode(&bytes), "AAAA", &endpoint, NOW + 60, NOW),
            Err(ListingError::Signature)
        );
    }

    #[test]
    fn a_listing_with_another_name_or_extra_keys_is_refused() {
        let good: Value =
            serde_json::from_slice(&listing_bytes(&settings(), "law#0042"))
                .expect("the listing must be JSON");
        assert!(parse(&encode(good.to_string().as_bytes()), "law#0043").is_err());
        assert!(parse(&encode(good.to_string().as_bytes()), "law").is_err());

        let mut changes: Vec<(&str, Value)> = Vec::new();
        let mut extra = good.clone();
        extra["url"] = json!("https://example.com");
        changes.push(("an extra key", extra));
        let mut skill_key = good.clone();
        skill_key["skills"][0]["examples"] = json!(["x"]);
        changes.push(("an extra skill key", skill_key));
        let mut hidden = good.clone();
        hidden["description"] = json!("abc\u{202E}def");
        changes.push(("a bidirectional character", hidden));
        let mut zero_width = good.clone();
        zero_width["skills"][0]["name"] = json!("a\u{200B}b");
        changes.push(("a zero-width character", zero_width));
        let mut line = good.clone();
        line["description"] = json!("one\ntwo");
        changes.push(("a line break", line));
        let mut twice = good.clone();
        twice["skills"] = json!([good["skills"][0], good["skills"][0]]);
        changes.push(("one skill ID two times", twice));
        let mut tag = good.clone();
        tag["skills"][0]["tags"] = json!(["Law"]);
        changes.push(("an upper-case tag", tag));
        for (label, value) in changes {
            assert_eq!(
                parse(&encode(value.to_string().as_bytes()), "law#0042"),
                Err(ListingError::Invalid),
                "{label}"
            );
        }
        let duplicate = format!(
            r#"{{"name":"law#0042","name":"law#0042","description":"x","skills":{}}}"#,
            good["skills"]
        );
        assert_eq!(
            parse(&encode(duplicate.as_bytes()), "law#0042"),
            Err(ListingError::Invalid)
        );
        assert_eq!(parse("not base64!", "law#0042"), Err(ListingError::Invalid));
    }
}
