use std::{collections::BTreeSet, str::FromStr};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::HubError;

pub const PROTOCOL_VERSION: u64 = 2;
pub const PROTOCOL_VERSION_HEADER: &str = "2";
pub const PROTOCOL_HEADER: &str = "x-aether-protocol";
pub const ENDPOINT_HEADER: &str = "x-aether-endpoint";
pub const TIME_HEADER: &str = "x-aether-time";
pub const SIGNATURE_HEADER: &str = "x-aether-signature";
pub const DEFAULT_HUB_URL: &str = "https://aether-hub.tudorandrei.xyz";
pub const HUB_OFF: &str = "off";
pub const MAX_SKEW_SECONDS: u64 = 300;
pub const MAX_BODY_BYTES: usize = 16_384;
pub const MAX_WAIT_SECONDS: u64 = 30;
pub const ACCESS_DAYS_MIN: u64 = 1;
pub const ACCESS_DAYS_MAX: u64 = 30;
pub const DEFAULT_ACCESS_DAYS: u64 = 7;
pub const OFFER_DAYS: [u64; 3] = [1, 7, 30];
pub const INVITE_SECONDS: i64 = 604_800;
pub const INVITE_PREFIX: &str = "aether-invite:";
pub const MAX_INVITE_BYTES: usize = 8192;
pub const MAX_OPEN_REQUESTS: u64 = 5;
pub const MAX_LISTED_REQUESTS: u64 = 50;
pub const MAX_BLOCKS: u64 = 1000;
pub const MAX_DIRECTORY_AGENTS: u64 = 100;
pub const PRESENCE_ONLINE_SECONDS: u64 = 45;
pub const PRESENCE_LAST_SEEN_STEP_SECONDS: u64 = 900;
pub const PRESENCE_KEEP_SECONDS: u64 = 604_800;
pub const NAME_PART_PATTERN: &str = "^[a-z0-9][a-z0-9._-]{0,31}$";
pub const NAME_NUMBER_MIN: u64 = 1;
pub const NAME_NUMBER_MAX: u64 = 9999;
pub const NAME_NUMBER_DIGITS: usize = 4;
pub const AGENT_SEARCH_LIMIT_MIN: u64 = 1;
pub const AGENT_SEARCH_LIMIT_DEFAULT: u64 = 10;
pub const AGENT_SEARCH_LIMIT_MAX: u64 = 20;
pub const AGENT_SEARCH_MAX_QUERY_CHARS: usize = 128;
pub const AGENT_SEARCH_MIN_WORD_CHARS: u64 = 2;
pub const AGENT_SEARCH_MAX_WORDS: u64 = 8;
pub const MAX_LISTING_BYTES: usize = 8192;
pub const MAX_LISTING_DAYS: u64 = 30;
pub const MAX_LISTING_SKILLS: usize = 5;
pub const MAX_LISTING_DESCRIPTION: usize = 500;
pub const MAX_LISTING_SKILL_ID: usize = 64;
pub const MAX_LISTING_SKILL_NAME: usize = 64;
pub const MAX_LISTING_SKILL_DESCRIPTION: usize = 300;
pub const MAX_LISTING_TAGS: usize = 5;
pub const MAX_LISTING_TAG: usize = 24;
pub const LISTING_HIDDEN_CODEPOINTS: &[(u32, u32)] =
    &[(0, 31), (127, 159), (8203, 8207), (8234, 8238), (8294, 8297), (65279, 65279)];
pub const OWNER_LINK_MAX_DAYS: u64 = 365;
pub const OWNER_CLAIM_SECONDS: i64 = 604_800;
pub const MAX_OPEN_CLAIMS: u64 = 5;
pub const MAX_OWNER_LINKS: u64 = 20;

/// A listing as its bytes hold it. Every text is from a stranger.
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    /// The full name, such as `law#0042`.
    pub name: String,
    pub description: String,
    pub skills: Vec<ListingSkill>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListingSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// A listing that is not valid, not signed by the endpoint it names, or
/// expired.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("the listing is not valid, not signed by its endpoint, or expired")]
pub struct ListingError;

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
) -> Result<(), ListingError> {
    if expires_at <= at {
        return Err(ListingError);
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingError)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|signature| signature.try_into().ok())
        .ok_or(ListingError)?;
    let key = iroh::PublicKey::from_str(endpoint).map_err(|_| ListingError)?;
    key.verify(
        listing_message(endpoint, expires_at, &bytes).as_bytes(),
        &iroh::Signature::from_bytes(&signature),
    )
    .map_err(|_| ListingError)
}

pub fn listing_bytes_match_handle(
    text: &str,
    handle: &str,
) -> Result<(), ListingError> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| ListingError)?;
    parse_listing_bytes(&bytes, handle).map(|_| ())
}

pub fn parse_listing_bytes(
    bytes: &[u8],
    handle: &str,
) -> Result<Listing, ListingError> {
    if bytes.len() > MAX_LISTING_BYTES {
        return Err(ListingError);
    }
    let listing: Listing = serde_json::from_slice(bytes).map_err(|_| ListingError)?;
    if !listing_valid(&listing, handle) {
        return Err(ListingError);
    }
    Ok(listing)
}

fn listing_valid(listing: &Listing, handle: &str) -> bool {
    let ids: BTreeSet<&String> = listing.skills.iter().map(|skill| &skill.id).collect();
    listing.name == handle
        && valid_listing_text(&listing.description, MAX_LISTING_DESCRIPTION)
        && (1..=MAX_LISTING_SKILLS).contains(&listing.skills.len())
        && ids.len() == listing.skills.len()
        && listing.skills.iter().all(valid_listing_skill)
}

fn valid_listing_skill(skill: &ListingSkill) -> bool {
    let tags: BTreeSet<&String> = skill.tags.iter().collect();
    valid_listing_word(&skill.id, MAX_LISTING_SKILL_ID)
        && valid_listing_text(&skill.name, MAX_LISTING_SKILL_NAME)
        && valid_listing_text(&skill.description, MAX_LISTING_SKILL_DESCRIPTION)
        && (1..=MAX_LISTING_TAGS).contains(&skill.tags.len())
        && tags.len() == skill.tags.len()
        && skill.tags.iter().all(|tag| valid_listing_word(tag, MAX_LISTING_TAG))
}

fn listing_hidden(character: char) -> bool {
    let code = u32::from(character);
    LISTING_HIDDEN_CODEPOINTS.iter().any(|&(low, high)| (low..=high).contains(&code))
}

fn valid_listing_text(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(listing_hidden)
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
pub fn hub_sign_target(path: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_owned(),
    }
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

#[must_use]
pub fn wire_error(status: u16, code: Option<&str>) -> HubError {
    match (status, code) {
        (401, _) => HubError::Unauthorized,
        (403, Some("no_account")) => HubError::NoAccount,
        (403, _) => HubError::Forbidden,
        (404, _) => HubError::NotFound,
        (409, Some("limit")) => HubError::Limit,
        (409, _) => HubError::Taken,
        (400 | 413, _) => HubError::Invalid,
        (429, _) => HubError::RateLimited,
        (426, _) => HubError::OldNode,
        (503, _) => HubError::Unreachable,
        _ => HubError::Unexpected,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use regex::Regex;
    use serde::Deserialize;
    use serde_json::Value;

    use super::{
        ACCESS_DAYS_MAX, ACCESS_DAYS_MIN, AGENT_SEARCH_LIMIT_DEFAULT,
        AGENT_SEARCH_LIMIT_MAX, AGENT_SEARCH_LIMIT_MIN, AGENT_SEARCH_MAX_QUERY_CHARS,
        AGENT_SEARCH_MAX_WORDS, AGENT_SEARCH_MIN_WORD_CHARS, DEFAULT_ACCESS_DAYS,
        DEFAULT_HUB_URL, ENDPOINT_HEADER, HUB_OFF, HubError, INVITE_PREFIX,
        INVITE_SECONDS, LISTING_HIDDEN_CODEPOINTS, MAX_BLOCKS, MAX_BODY_BYTES,
        MAX_DIRECTORY_AGENTS, MAX_INVITE_BYTES, MAX_LISTED_REQUESTS, MAX_LISTING_BYTES,
        MAX_LISTING_DAYS, MAX_LISTING_DESCRIPTION, MAX_LISTING_SKILL_DESCRIPTION,
        MAX_LISTING_SKILL_ID, MAX_LISTING_SKILL_NAME, MAX_LISTING_SKILLS,
        MAX_LISTING_TAG, MAX_LISTING_TAGS, MAX_OPEN_CLAIMS, MAX_OPEN_REQUESTS,
        MAX_OWNER_LINKS, MAX_SKEW_SECONDS, MAX_WAIT_SECONDS, NAME_NUMBER_DIGITS,
        NAME_NUMBER_MAX, NAME_NUMBER_MIN, NAME_PART_PATTERN, OFFER_DAYS,
        OWNER_CLAIM_SECONDS, OWNER_LINK_MAX_DAYS, PRESENCE_KEEP_SECONDS,
        PRESENCE_LAST_SEEN_STEP_SECONDS, PRESENCE_ONLINE_SECONDS, PROTOCOL_HEADER,
        PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER, SIGNATURE_HEADER, TIME_HEADER,
        hub_request_message, hub_sign_target, listing_bytes_match_handle,
        listing_message, owner_claim_message, owner_confirm_message, wire_error,
    };
    use crate::hub_client::valid_name_part;

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
        let headers = &contract()["headers"];
        let signing = &contract()["signing"];
        assert_eq!(contract()["protocol"].as_u64(), Some(PROTOCOL_VERSION));
        assert_eq!(PROTOCOL_VERSION_HEADER, "2");
        assert_eq!(headers["protocol"].as_str(), Some(PROTOCOL_HEADER));
        assert_eq!(headers["endpoint"].as_str(), Some(ENDPOINT_HEADER));
        assert_eq!(headers["time"].as_str(), Some(TIME_HEADER));
        assert_eq!(headers["signature"].as_str(), Some(SIGNATURE_HEADER));
        assert_eq!(signing["maxSkewSeconds"].as_u64(), Some(MAX_SKEW_SECONDS));
        assert_eq!(signing["endpoint"].as_str(), Some("^[0-9a-f]{64}$"));
        assert_eq!(
            limits["maxBodyBytes"].as_u64(),
            Some(u64::try_from(MAX_BODY_BYTES).expect("body"))
        );
        assert_eq!(limits["inviteSeconds"].as_i64(), Some(INVITE_SECONDS));
        assert_eq!(limits["invitePrefix"].as_str(), Some(INVITE_PREFIX));
        assert_eq!(
            limits["maxInviteBytes"].as_u64(),
            Some(u64::try_from(MAX_INVITE_BYTES).expect("invite bytes"))
        );
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
        let hidden: Vec<(u32, u32)> = listing["hiddenCodepoints"]
            .as_array()
            .expect("hidden codepoints")
            .iter()
            .map(|range| {
                let pair = range.as_array().expect("range pair");
                (
                    u32::try_from(pair[0].as_u64().expect("low")).expect("low u32"),
                    u32::try_from(pair[1].as_u64().expect("high")).expect("high u32"),
                )
            })
            .collect();
        assert_eq!(hidden, LISTING_HIDDEN_CODEPOINTS);
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
        assert_eq!(limits["maxListedRequests"].as_u64(), Some(MAX_LISTED_REQUESTS));
        assert_eq!(limits["maxBlocks"].as_u64(), Some(MAX_BLOCKS));
        assert_eq!(limits["maxDirectoryAgents"].as_u64(), Some(MAX_DIRECTORY_AGENTS));
        let presence = &limits["presence"];
        assert_eq!(presence["onlineSeconds"].as_u64(), Some(PRESENCE_ONLINE_SECONDS));
        assert_eq!(
            presence["lastSeenStepSeconds"].as_u64(),
            Some(PRESENCE_LAST_SEEN_STEP_SECONDS)
        );
        assert_eq!(presence["keepSeconds"].as_u64(), Some(PRESENCE_KEEP_SECONDS));
        assert_eq!(
            limits["search"]["limit"]["min"].as_u64(),
            Some(AGENT_SEARCH_LIMIT_MIN)
        );
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
        assert_eq!(
            limits["search"]["agents"]["minWordChars"].as_u64(),
            Some(AGENT_SEARCH_MIN_WORD_CHARS)
        );
        assert_eq!(
            limits["search"]["agents"]["maxWords"].as_u64(),
            Some(AGENT_SEARCH_MAX_WORDS)
        );
        let owner_links = &limits["ownerLinks"];
        assert_eq!(owner_links["maxDays"].as_u64(), Some(OWNER_LINK_MAX_DAYS));
        assert_eq!(owner_links["claimSeconds"].as_i64(), Some(OWNER_CLAIM_SECONDS));
        assert_eq!(owner_links["maxOpenClaims"].as_u64(), Some(MAX_OPEN_CLAIMS));
        assert_eq!(owner_links["maxLinks"].as_u64(), Some(MAX_OWNER_LINKS));
        assert_eq!(names["name"].as_str(), Some(NAME_PART_PATTERN));
        assert_eq!(names["numbers"]["min"].as_u64(), Some(NAME_NUMBER_MIN));
        assert_eq!(names["numbers"]["max"].as_u64(), Some(NAME_NUMBER_MAX));
        assert_eq!(
            names["numberDigits"].as_u64(),
            Some(u64::try_from(NAME_NUMBER_DIGITS).expect("digits"))
        );
        assert_eq!(DEFAULT_HUB_URL, "https://aether-hub.tudorandrei.xyz");
        assert_eq!(HUB_OFF, "off");
        let errors = contract()["errors"].as_object().expect("errors");
        for (code, status) in errors {
            let status =
                u16::try_from(status.as_u64().expect("status")).expect("status u16");
            let mapped = wire_error(status, Some(code.as_str()));
            let expected = match code.as_str() {
                "invalid" | "too_large" => HubError::Invalid,
                "unauthorized" => HubError::Unauthorized,
                "no_account" => HubError::NoAccount,
                "forbidden" => HubError::Forbidden,
                "not_found" => HubError::NotFound,
                "taken" => HubError::Taken,
                "limit" => HubError::Limit,
                "upgrade_required" => HubError::OldNode,
                "rate_limited" => HubError::RateLimited,
                "unavailable" => HubError::Unreachable,
                _ => HubError::Unexpected,
            };
            assert_eq!(mapped, expected, "{code}");
        }
    }

    #[test]
    fn valid_name_part_matches_contract_name_pattern() {
        let pattern = Regex::new(NAME_PART_PATTERN).expect("name pattern");
        let alphabet: Vec<char> =
            "abcdefghijklmnopqrstuvwxyz0123456789._-".chars().collect();
        let mut stack = vec![String::new()];
        while let Some(prefix) = stack.pop() {
            if prefix.len() < 4 {
                for character in &alphabet {
                    let mut next = prefix.clone();
                    next.push(*character);
                    stack.push(next);
                }
            }
            assert_eq!(valid_name_part(&prefix), pattern.is_match(&prefix), "{prefix}");
        }
        for len in 1..=32 {
            let name = "a".repeat(len);
            assert_eq!(valid_name_part(&name), pattern.is_match(&name), "{name}");
        }
    }

    #[test]
    fn hub_sign_target_matches_request_vectors() {
        for vector in vectors()["request"].as_array().expect("request vectors") {
            let path_and_query =
                vector["fields"]["pathAndQuery"].as_str().expect("pathAndQuery");
            let (path, query) = path_and_query
                .split_once('?')
                .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
            assert_eq!(
                hub_sign_target(path, query),
                path_and_query,
                "{}",
                vector["name"]
            );
        }
    }

    #[derive(Deserialize)]
    struct ListingValidationVector {
        name: String,
        handle: String,
        #[serde(rename = "listingBytes")]
        listing_bytes: String,
        valid: bool,
    }

    #[test]
    fn listing_validation_vectors_match_protocol_hub() {
        for entry in
            vectors()["listingValidation"].as_array().expect("listing validation")
        {
            let vector: ListingValidationVector =
                serde_json::from_value(entry.clone()).expect("vector shape");
            let bytes = vector.listing_bytes.as_bytes();
            let ok = listing_bytes_match_handle(
                &URL_SAFE_NO_PAD.encode(bytes),
                &vector.handle,
            )
            .is_ok();
            assert_eq!(ok, vector.valid, "{}", vector.name);
        }
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
