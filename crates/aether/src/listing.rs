//! The public listing of a service agent on the hub (`PUT /v1/listing` in
//! `server/PROTOCOL.md`).
//!
//! The listing reuses the text of the Agent Card, so the owner writes it one
//! time. It holds the full name, the description and the skill, and no
//! endpoint or address: everyone can read it. The node signs the listing
//! bytes with its endpoint key, and the hub returns them exactly as sent, so
//! a reader can check them against the endpoint that it gets later.

use std::{collections::BTreeSet, str::FromStr, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    agent_card::{
        CardSettings, MAX_DESCRIPTION, MAX_SKILL_DESCRIPTION, MAX_SKILL_NAME, MAX_TAG,
        MAX_TAGS,
    },
    untrusted,
};

/// How long a new listing stays on the hub. The hub accepts 30 days at most.
pub const LISTING_LIFETIME: Duration = Duration::from_hours(30 * 24);
/// A listing with less time left than this is signed again.
pub const RENEW_BEFORE: Duration = Duration::from_hours(7 * 24);
/// The most bytes of the listing that the hub keeps.
pub const MAX_LISTING_BYTES: usize = 8 * 1024;
/// The most skills of a listing.
pub const MAX_SKILLS: usize = 5;
/// The most characters of a skill ID, from `[a-z0-9-]`.
const MAX_SKILL_ID: usize = 64;

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum ListingError {
    #[error("the listing is not valid")]
    Invalid,
    #[error("the listing signature does not verify")]
    Signature,
    #[error("the listing has expired")]
    Expired,
}

/// The listing bytes as JSON. A key two times is refused by the derive.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawListing {
    name: String,
    description: String,
    skills: Vec<RawSkill>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkill {
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
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

/// What this node last sent to the hub, kept under
/// `node_config::settings::ListingState`.
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

/// The message that the endpoint key signs.
fn message(endpoint: &str, expires_at: u64, bytes: &[u8]) -> String {
    format!(
        "aether-listing/v1\n{endpoint}\ntrue\n{expires_at}\n{}",
        hex::encode(Sha256::digest(bytes))
    )
}

/// The signature of `bytes` until `expires_at` by the endpoint `key`, as
/// unpadded base64url.
#[must_use]
pub fn sign(key: &iroh::SecretKey, bytes: &[u8], expires_at: u64) -> String {
    let message = message(&key.public().to_string(), expires_at, bytes);
    URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes())
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
    if expires_at <= at {
        return Err(ListingError::Expired);
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingError::Invalid)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|signature| signature.try_into().ok())
        .ok_or(ListingError::Signature)?;
    let key =
        iroh::PublicKey::from_str(endpoint).map_err(|_| ListingError::Signature)?;
    key.verify(
        message(endpoint, expires_at, &bytes).as_bytes(),
        &iroh::Signature::from_bytes(&signature),
    )
    .map_err(|_| ListingError::Signature)
}

/// How long an owner link lasts. The hub accepts 365 days at most.
pub const OWNER_LINK_LIFETIME: Duration = Duration::from_hours(365 * 24);

/// The message that the personal agent `personal` signs to claim the service
/// agent `service_handle` as its own until `expires_at`.
#[must_use]
pub fn owner_claim_message(
    personal: &str,
    service_handle: &str,
    expires_at: u64,
) -> String {
    format!("aether-owner/v1\nclaim\n{personal}\n{service_handle}\n{expires_at}")
}

/// The message that the service agent `service` signs to confirm the claim
/// of the personal agent `personal` until `expires_at`.
#[must_use]
pub fn owner_confirm_message(service: &str, personal: &str, expires_at: u64) -> String {
    format!("aether-owner/v1\nconfirm\n{service}\n{personal}\n{expires_at}")
}

/// The claim of the personal agent with the endpoint `key` for the service
/// agent `service_handle`, as unpadded base64url.
#[must_use]
pub fn sign_owner_claim(
    key: &iroh::SecretKey,
    service_handle: &str,
    expires_at: u64,
) -> String {
    let message =
        owner_claim_message(&key.public().to_string(), service_handle, expires_at);
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
    let message =
        owner_confirm_message(&key.public().to_string(), personal, expires_at);
    URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes())
}

/// The listing in `text` (base64url) when it passes the hub's rules and
/// names `handle`. Every text is cleaned for display.
pub fn parse(text: &str, handle: &str) -> Result<Listing, ListingError> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingError::Invalid)?;
    if bytes.len() > MAX_LISTING_BYTES {
        return Err(ListingError::Invalid);
    }
    let raw: RawListing =
        serde_json::from_slice(&bytes).map_err(|_| ListingError::Invalid)?;
    let ids: BTreeSet<&String> = raw.skills.iter().map(|skill| &skill.id).collect();
    let valid = raw.name == handle
        && valid_text(&raw.description, MAX_DESCRIPTION)
        && (1..=MAX_SKILLS).contains(&raw.skills.len())
        && ids.len() == raw.skills.len()
        && raw.skills.iter().all(valid_skill);
    if !valid {
        return Err(ListingError::Invalid);
    }
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

fn valid_skill(skill: &RawSkill) -> bool {
    let tags: BTreeSet<&String> = skill.tags.iter().collect();
    valid_word(&skill.id, MAX_SKILL_ID)
        && valid_text(&skill.name, MAX_SKILL_NAME)
        && valid_text(&skill.description, MAX_SKILL_DESCRIPTION)
        && (1..=MAX_TAGS).contains(&skill.tags.len())
        && tags.len() == skill.tags.len()
        && skill.tags.iter().all(|tag| valid_word(tag, MAX_TAG))
}

/// The hub's text rule: 1 to `max_chars` characters, not only white space,
/// with no control, bidirectional or zero-width characters.
fn valid_text(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text
            .chars()
            .any(|character| character.is_control() || untrusted::hidden(character))
}

/// 1 to `max` characters of `[a-z0-9-]`.
fn valid_word(word: &str, max: usize) -> bool {
    (1..=max).contains(&word.len())
        && word.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        })
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
    fn the_owner_claim_signs_the_service_full_name() {
        let key = iroh::SecretKey::generate();
        let personal = key.public().to_string();
        let signature = super::sign_owner_claim(&key, "law#0042", NOW);
        let bytes: [u8; 64] = URL_SAFE_NO_PAD
            .decode(&signature)
            .expect("base64url")
            .try_into()
            .expect("64 bytes");
        let signature = iroh::Signature::from_bytes(&bytes);
        let message = super::owner_claim_message(&personal, "law#0042", NOW);
        assert_eq!(
            message,
            format!("aether-owner/v1\nclaim\n{personal}\nlaw#0042\n{NOW}")
        );
        assert!(key.public().verify(message.as_bytes(), &signature).is_ok());
        let other = super::owner_claim_message(&personal, "law#0043", NOW);
        assert!(key.public().verify(other.as_bytes(), &signature).is_err());
        assert_eq!(
            super::owner_confirm_message("s", "p", 7),
            "aether-owner/v1\nconfirm\ns\np\n7"
        );
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
