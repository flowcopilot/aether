use std::{collections::BTreeSet, str::FromStr};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: u64 = 2;
pub const PROTOCOL_VERSION_HEADER: &str = "2";
pub const PROTOCOL_HEADER: &str = "x-aether-protocol";
pub const ENDPOINT_HEADER: &str = "x-aether-endpoint";
pub const TIME_HEADER: &str = "x-aether-time";
pub const SIGNATURE_HEADER: &str = "x-aether-signature";
pub const DEFAULT_HUB_URL: &str = "https://aether-hub.tudorandrei.xyz";
pub const HUB_OFF: &str = "off";
pub const MAX_WAIT_SECONDS: u64 = 30;
pub const ACCESS_DAYS_MIN: u64 = 1;
pub const ACCESS_DAYS_MAX: u64 = 30;
pub const DEFAULT_ACCESS_DAYS: u64 = 7;
pub const OFFER_DAYS: [u64; 3] = [1, 7, 30];
pub const MAX_OPEN_REQUESTS: u64 = 5;
pub const AGENT_SEARCH_LIMIT_DEFAULT: u64 = 10;
pub const AGENT_SEARCH_LIMIT_MAX: u64 = 20;
pub const AGENT_SEARCH_MAX_QUERY_CHARS: usize = 128;
pub const NAME_PART_PATTERN: &str = "^[a-z0-9][a-z0-9._-]{0,31}$";
pub const NAME_NUMBER_DIGITS: usize = 4;
pub const INVITE_SECONDS: i64 = 604_800;
pub const MAX_LISTING_BYTES: usize = 8192;
pub const MAX_LISTING_DAYS: u64 = 30;
pub const MAX_LISTING_SKILLS: usize = 5;
pub const MAX_LISTING_DESCRIPTION: usize = 500;
pub const MAX_LISTING_SKILL_ID: usize = 64;
pub const MAX_LISTING_SKILL_NAME: usize = 64;
pub const MAX_LISTING_SKILL_DESCRIPTION: usize = 300;
pub const MAX_LISTING_TAGS: usize = 5;
pub const MAX_LISTING_TAG: usize = 24;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ListingWireError {
    Invalid,
    Signature,
    Expired,
}

#[must_use]
pub fn sign_listing(key: &iroh::SecretKey, bytes: &[u8], expires_at: u64) -> String {
    let endpoint = key.public().to_string();
    let message = listing_message(&endpoint, expires_at, bytes);
    URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes())
}

pub fn verify_listing(
    text: &str,
    signature: &str,
    endpoint: &str,
    expires_at: u64,
    at: u64,
) -> Result<(), ListingWireError> {
    if expires_at <= at {
        return Err(ListingWireError::Expired);
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingWireError::Invalid)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|signature| signature.try_into().ok())
        .ok_or(ListingWireError::Signature)?;
    let key =
        iroh::PublicKey::from_str(endpoint).map_err(|_| ListingWireError::Signature)?;
    key.verify(
        listing_message(endpoint, expires_at, &bytes).as_bytes(),
        &iroh::Signature::from_bytes(&signature),
    )
    .map_err(|_| ListingWireError::Signature)
}

pub fn listing_bytes_match_handle(
    text: &str,
    handle: &str,
) -> Result<(), ListingWireError> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingWireError::Invalid)?;
    if bytes.len() > MAX_LISTING_BYTES {
        return Err(ListingWireError::Invalid);
    }
    let raw: RawListing =
        serde_json::from_slice(&bytes).map_err(|_| ListingWireError::Invalid)?;
    if listing_payload_valid(&raw, handle) {
        Ok(())
    } else {
        Err(ListingWireError::Invalid)
    }
}

#[derive(Deserialize)]
struct RawListing {
    name: String,
    description: String,
    skills: Vec<RawSkill>,
}

#[derive(Deserialize)]
struct RawSkill {
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
}

fn listing_payload_valid(raw: &RawListing, handle: &str) -> bool {
    let ids: BTreeSet<&String> = raw.skills.iter().map(|skill| &skill.id).collect();
    raw.name == handle
        && valid_listing_text(&raw.description, MAX_LISTING_DESCRIPTION)
        && (1..=MAX_LISTING_SKILLS).contains(&raw.skills.len())
        && ids.len() == raw.skills.len()
        && raw.skills.iter().all(valid_listing_skill)
}

fn valid_listing_skill(skill: &RawSkill) -> bool {
    let tags: BTreeSet<&String> = skill.tags.iter().collect();
    valid_listing_word(&skill.id, MAX_LISTING_SKILL_ID)
        && valid_listing_text(&skill.name, MAX_LISTING_SKILL_NAME)
        && valid_listing_text(&skill.description, MAX_LISTING_SKILL_DESCRIPTION)
        && (1..=MAX_LISTING_TAGS).contains(&skill.tags.len())
        && tags.len() == skill.tags.len()
        && skill.tags.iter().all(|tag| valid_listing_word(tag, MAX_LISTING_TAG))
}

fn valid_listing_text(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(|character| {
            character.is_control() || crate::untrusted::hidden(character)
        })
}

fn valid_listing_word(word: &str, max: usize) -> bool {
    (1..=max).contains(&word.len())
        && word.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        })
}

#[must_use]
pub fn hub_request_message(
    method: &str,
    path_and_query: &str,
    time: &str,
    body: &[u8],
) -> String {
    format!(
        "aether-hub/v1\n{method}\n{path_and_query}\n{time}\n{}",
        hex::encode(Sha256::digest(body))
    )
}

#[must_use]
pub fn listing_message(
    endpoint: &str,
    expires_at: u64,
    listing_bytes: &[u8],
) -> String {
    format!(
        "aether-listing/v1\n{endpoint}\ntrue\n{expires_at}\n{}",
        hex::encode(Sha256::digest(listing_bytes))
    )
}

#[must_use]
pub fn owner_claim_message(
    personal_endpoint: &str,
    service_handle: &str,
    expires_at: u64,
) -> String {
    format!(
        "aether-owner/v1\nclaim\n{personal_endpoint}\n{service_handle}\n{expires_at}"
    )
}

#[must_use]
pub fn owner_confirm_message(
    service_endpoint: &str,
    personal_endpoint: &str,
    expires_at: u64,
) -> String {
    format!(
        "aether-owner/v1\nconfirm\n{service_endpoint}\n{personal_endpoint}\n{expires_at}"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubWireError {
    Invalid,
    Unauthorized,
    NoAccount,
    Forbidden,
    NotFound,
    Taken,
    Limit,
    RateLimited,
    OldNode,
    Unexpected,
}

#[must_use]
pub fn wire_error(status: u16, code: Option<&str>) -> HubWireError {
    match (status, code) {
        (401, _) => HubWireError::Unauthorized,
        (403, Some("no_account")) => HubWireError::NoAccount,
        (403, _) => HubWireError::Forbidden,
        (404, _) => HubWireError::NotFound,
        (409, Some("limit")) => HubWireError::Limit,
        (409, _) => HubWireError::Taken,
        (400 | 413, _) => HubWireError::Invalid,
        (429, _) => HubWireError::RateLimited,
        (426, _) => HubWireError::OldNode,
        _ => HubWireError::Unexpected,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde::Deserialize;
    use serde_json::Value;

    use super::{
        ACCESS_DAYS_MAX, ACCESS_DAYS_MIN, AGENT_SEARCH_LIMIT_DEFAULT,
        AGENT_SEARCH_LIMIT_MAX, AGENT_SEARCH_MAX_QUERY_CHARS, DEFAULT_ACCESS_DAYS,
        DEFAULT_HUB_URL, HUB_OFF, INVITE_SECONDS, MAX_LISTING_BYTES, MAX_LISTING_DAYS,
        MAX_LISTING_DESCRIPTION, MAX_LISTING_SKILL_DESCRIPTION, MAX_LISTING_SKILL_ID,
        MAX_LISTING_SKILL_NAME, MAX_LISTING_SKILLS, MAX_LISTING_TAG, MAX_LISTING_TAGS,
        MAX_OPEN_REQUESTS, MAX_WAIT_SECONDS, NAME_NUMBER_DIGITS, NAME_PART_PATTERN,
        OFFER_DAYS, PROTOCOL_HEADER, PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
        hub_request_message, listing_message, owner_claim_message,
        owner_confirm_message,
    };

    const CONTRACT_JSON: &str = include_str!("../../../../protocol/hub/contract.json");
    const VECTORS_JSON: &str = include_str!("../../../../protocol/hub/vectors.json");

    static CONTRACT: OnceLock<Value> = OnceLock::new();
    static VECTORS: OnceLock<Value> = OnceLock::new();

    fn contract() -> &'static Value {
        CONTRACT.get_or_init(|| {
            serde_json::from_str(CONTRACT_JSON)
                .expect("protocol/hub/contract.json is JSON")
        })
    }

    fn vectors() -> &'static Value {
        VECTORS.get_or_init(|| {
            serde_json::from_str(VECTORS_JSON)
                .expect("protocol/hub/vectors.json is JSON")
        })
    }

    #[test]
    fn the_rust_table_matches_protocol_hub() {
        let limits = &contract()["limits"];
        let names = &contract()["names"];
        assert_eq!(contract()["protocol"].as_u64(), Some(PROTOCOL_VERSION));
        assert_eq!(PROTOCOL_VERSION_HEADER, "2");
        assert_eq!(contract()["headers"]["protocol"].as_str(), Some(PROTOCOL_HEADER));
        assert_eq!(limits["inviteSeconds"].as_i64(), Some(INVITE_SECONDS));
        let listing = &limits["listing"];
        assert_eq!(listing["maxDays"].as_u64(), Some(MAX_LISTING_DAYS));
        assert_eq!(
            listing["maxBytes"].as_u64(),
            Some(u64::try_from(MAX_LISTING_BYTES).expect("bytes"))
        );
        assert_eq!(
            listing["maxDescription"].as_u64(),
            Some(u64::try_from(MAX_LISTING_DESCRIPTION).expect("description"))
        );
        assert_eq!(
            listing["maxSkills"].as_u64(),
            Some(u64::try_from(MAX_LISTING_SKILLS).expect("skills"))
        );
        assert_eq!(
            listing["maxSkillName"].as_u64(),
            Some(u64::try_from(MAX_LISTING_SKILL_NAME).expect("skill name"))
        );
        assert_eq!(
            listing["maxSkillDescription"].as_u64(),
            Some(
                u64::try_from(MAX_LISTING_SKILL_DESCRIPTION)
                    .expect("skill description")
            )
        );
        assert_eq!(
            listing["maxTags"].as_u64(),
            Some(u64::try_from(MAX_LISTING_TAGS).expect("tags"))
        );
        assert_eq!(
            listing["tag"]
                .as_str()
                .and_then(|pattern| pattern.strip_prefix("^[a-z0-9-]{1,"))
                .and_then(|rest| rest.strip_suffix("}$"))
                .and_then(|max| max.parse::<usize>().ok()),
            Some(MAX_LISTING_TAG)
        );
        assert_eq!(
            listing["skillId"]
                .as_str()
                .and_then(|pattern| pattern.strip_prefix("^[a-z0-9-]{1,"))
                .and_then(|rest| rest.strip_suffix("}$"))
                .and_then(|max| max.parse::<usize>().ok()),
            Some(MAX_LISTING_SKILL_ID)
        );
        assert_eq!(limits["maxWaitSeconds"].as_u64(), Some(MAX_WAIT_SECONDS));
        assert_eq!(limits["accessDays"]["min"].as_u64(), Some(ACCESS_DAYS_MIN));
        assert_eq!(limits["accessDays"]["max"].as_u64(), Some(ACCESS_DAYS_MAX));
        assert_eq!(limits["defaultDays"].as_u64(), Some(DEFAULT_ACCESS_DAYS));
        assert_eq!(
            limits["offerDays"]
                .as_array()
                .expect("offer days")
                .iter()
                .map(|day| day.as_u64().expect("day"))
                .collect::<Vec<_>>(),
            OFFER_DAYS.to_vec()
        );
        assert_eq!(limits["maxOpenRequests"].as_u64(), Some(MAX_OPEN_REQUESTS));
        assert_eq!(
            limits["search"]["limit"]["default"].as_u64(),
            Some(AGENT_SEARCH_LIMIT_DEFAULT)
        );
        assert_eq!(
            limits["search"]["limit"]["max"].as_u64(),
            Some(AGENT_SEARCH_LIMIT_MAX)
        );
        assert_eq!(
            limits["search"]["agents"]["maxQueryChars"].as_u64(),
            Some(u64::try_from(AGENT_SEARCH_MAX_QUERY_CHARS).expect("query chars"))
        );
        assert_eq!(names["name"].as_str(), Some(NAME_PART_PATTERN));
        assert_eq!(
            names["numberDigits"].as_u64(),
            Some(u64::try_from(NAME_NUMBER_DIGITS).expect("digits"))
        );
        assert_eq!(DEFAULT_HUB_URL, "https://aether-hub.tudorandrei.xyz");
        assert_eq!(HUB_OFF, "off");
    }

    #[derive(Deserialize)]
    struct Vector {
        name: String,
        fields: serde_json::Map<String, serde_json::Value>,
        message: String,
        signature: String,
        signer: String,
    }

    fn check_vectors(
        kind: &str,
        build: fn(&serde_json::Map<String, serde_json::Value>) -> String,
    ) {
        for vector in vectors()[kind].as_array().expect("vector list") {
            let vector: Vector =
                serde_json::from_value(vector.clone()).expect("vector shape");
            assert_eq!(
                build(&vector.fields),
                vector.message,
                "{}: {}",
                kind,
                vector.name
            );
            let seed = hex::decode(
                vectors()["keys"][&vector.signer]["seed"].as_str().expect("seed"),
            )
            .expect("seed hex");
            let seed: [u8; 32] = seed.try_into().expect("seed length");
            let signed = iroh::SecretKey::from_bytes(&seed)
                .sign(vector.message.as_bytes())
                .to_bytes();
            assert_eq!(
                URL_SAFE_NO_PAD.encode(signed),
                vector.signature,
                "{}: {}",
                kind,
                vector.name
            );
        }
    }

    #[test]
    fn signing_vectors_match_protocol_hub() {
        check_vectors("request", |fields| {
            hub_request_message(
                fields["method"].as_str().expect("method"),
                fields["pathAndQuery"].as_str().expect("path"),
                fields["time"].as_str().expect("time"),
                fields["body"].as_str().unwrap_or("").as_bytes(),
            )
        });
        check_vectors("listing", |fields| {
            listing_message(
                fields["endpoint"].as_str().expect("endpoint"),
                fields["expiresAt"].as_u64().expect("expires"),
                fields["listingBytes"].as_str().expect("bytes").as_bytes(),
            )
        });
        check_vectors("ownerClaim", |fields| {
            owner_claim_message(
                fields["personalEndpoint"].as_str().expect("personal"),
                fields["serviceHandle"].as_str().expect("service"),
                fields["expiresAt"].as_u64().expect("expires"),
            )
        });
        check_vectors("ownerConfirm", |fields| {
            owner_confirm_message(
                fields["serviceEndpoint"].as_str().expect("service"),
                fields["personalEndpoint"].as_str().expect("personal"),
                fields["expiresAt"].as_u64().expect("expires"),
            )
        });
    }
}
