use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: u64 = 2;
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

#[must_use]
pub const fn protocol_version() -> u64 {
    PROTOCOL_VERSION
}

#[must_use]
pub const fn protocol_header() -> &'static str {
    PROTOCOL_HEADER
}

#[must_use]
pub const fn endpoint_header() -> &'static str {
    ENDPOINT_HEADER
}

#[must_use]
pub const fn time_header() -> &'static str {
    TIME_HEADER
}

#[must_use]
pub const fn signature_header() -> &'static str {
    SIGNATURE_HEADER
}

#[must_use]
pub const fn default_hub_url() -> &'static str {
    DEFAULT_HUB_URL
}

#[must_use]
pub const fn hub_off_value() -> &'static str {
    HUB_OFF
}

#[must_use]
pub const fn max_wait_seconds() -> u64 {
    MAX_WAIT_SECONDS
}

#[must_use]
pub const fn access_days_min() -> u64 {
    ACCESS_DAYS_MIN
}

#[must_use]
pub const fn access_days_max() -> u64 {
    ACCESS_DAYS_MAX
}

#[must_use]
pub const fn default_access_days() -> u64 {
    DEFAULT_ACCESS_DAYS
}

#[must_use]
pub const fn offer_days() -> &'static [u64] {
    &OFFER_DAYS
}

#[must_use]
pub const fn name_part_pattern() -> &'static str {
    NAME_PART_PATTERN
}

#[must_use]
pub const fn name_number_digits() -> usize {
    NAME_NUMBER_DIGITS
}

#[must_use]
pub const fn max_open_requests() -> u64 {
    MAX_OPEN_REQUESTS
}

#[must_use]
pub const fn agent_search_limit_default() -> u64 {
    AGENT_SEARCH_LIMIT_DEFAULT
}

#[must_use]
pub const fn agent_search_limit_max() -> u64 {
    AGENT_SEARCH_LIMIT_MAX
}

#[must_use]
pub const fn agent_search_max_query_chars() -> usize {
    AGENT_SEARCH_MAX_QUERY_CHARS
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
        DEFAULT_HUB_URL, HUB_OFF, MAX_OPEN_REQUESTS, MAX_WAIT_SECONDS,
        NAME_NUMBER_DIGITS, NAME_PART_PATTERN, OFFER_DAYS, PROTOCOL_HEADER,
        PROTOCOL_VERSION, hub_request_message, listing_message, owner_claim_message,
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
        assert_eq!(contract()["headers"]["protocol"].as_str(), Some(PROTOCOL_HEADER));
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
