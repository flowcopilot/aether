use std::time::SystemTime;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Record {
    pub agent_id: String,
    pub endpoint_id: String,
    pub addresses: Vec<String>,
    pub scope_id: String,
    pub capabilities: Vec<String>,
    pub expires_at: SystemTime,
    pub signing_key: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Invitation {
    pub scope_id: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub expires_at: SystemTime,
    pub signing_key: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DiscoveryError {
    Expired,
    Forged,
    NotFound,
    Unavailable,
}

pub fn validate_record(record: &Record) -> Result<(), DiscoveryError> {
    if record.addresses.is_empty()
        || record.addresses.len() > 16
        || record.addresses.iter().any(|address| {
            address.is_empty() || address.len() > 256 || address.contains('\0')
        })
    {
        return Err(DiscoveryError::Forged);
    }
    validate(
        record.expires_at,
        &record.signing_key,
        &record.signature,
        &record_bytes(record),
    )
}

pub fn validate_invitation(invitation: &Invitation) -> Result<(), DiscoveryError> {
    validate(
        invitation.expires_at,
        &invitation.signing_key,
        &invitation.signature,
        &invitation_bytes(invitation),
    )
}

fn validate(
    expires_at: SystemTime,
    signing_key: &[u8; 32],
    signature: &[u8],
    message: &[u8],
) -> Result<(), DiscoveryError> {
    if expires_at <= SystemTime::now() {
        return Err(DiscoveryError::Expired);
    }
    let key =
        VerifyingKey::from_bytes(signing_key).map_err(|_| DiscoveryError::Forged)?;
    let signature =
        Signature::try_from(signature).map_err(|_| DiscoveryError::Forged)?;
    key.verify_strict(message, &signature).map_err(|_| DiscoveryError::Forged)
}

#[must_use]
pub fn signed_record(
    signing_key: &SigningKey,
    agent_id: String,
    endpoint_id: String,
    addresses: Vec<String>,
    scope_id: String,
    capabilities: Vec<String>,
    expires_at: SystemTime,
) -> Record {
    let mut record = Record {
        agent_id,
        endpoint_id,
        addresses,
        scope_id,
        capabilities,
        expires_at,
        signing_key: signing_key.verifying_key().to_bytes(),
        signature: Vec::new(),
    };
    record.signature = signing_key.sign(&record_bytes(&record)).to_bytes().to_vec();
    record
}

#[must_use]
pub fn signed_invitation(
    signing_key: &SigningKey,
    scope_id: String,
    agent_id: String,
    endpoint_id: String,
    expires_at: SystemTime,
) -> Invitation {
    let mut invitation = Invitation {
        scope_id,
        agent_id,
        endpoint_id,
        expires_at,
        signing_key: signing_key.verifying_key().to_bytes(),
        signature: Vec::new(),
    };
    invitation.signature =
        signing_key.sign(&invitation_bytes(&invitation)).to_bytes().to_vec();
    invitation
}

/// Signs the Agent Card `card` of the agent `agent_id` at `endpoint_id`.
#[must_use]
pub fn sign_agent_card(
    signing_key: &SigningKey,
    endpoint_id: &str,
    agent_id: &str,
    card: &[u8],
) -> Vec<u8> {
    signing_key.sign(&agent_card_bytes(endpoint_id, agent_id, card)).to_bytes().to_vec()
}

/// Checks that `signing_key` signed `card` for the agent `agent_id` at
/// `endpoint_id`.
pub fn verify_agent_card(
    signing_key: &[u8; 32],
    endpoint_id: &str,
    agent_id: &str,
    card: &[u8],
    signature: &[u8],
) -> Result<(), DiscoveryError> {
    let key =
        VerifyingKey::from_bytes(signing_key).map_err(|_| DiscoveryError::Forged)?;
    let signature =
        Signature::try_from(signature).map_err(|_| DiscoveryError::Forged)?;
    key.verify_strict(&agent_card_bytes(endpoint_id, agent_id, card), &signature)
        .map_err(|_| DiscoveryError::Forged)
}

fn agent_card_bytes(endpoint_id: &str, agent_id: &str, card: &[u8]) -> Vec<u8> {
    let mut bytes = b"aether/agent-card/v1".to_vec();
    append_string(&mut bytes, endpoint_id);
    append_string(&mut bytes, agent_id);
    append_length(&mut bytes, card.len());
    bytes.extend_from_slice(card);
    bytes
}

fn record_bytes(record: &Record) -> Vec<u8> {
    let mut bytes = b"aether/discovery-record/v1".to_vec();
    append_string(&mut bytes, &record.agent_id);
    append_string(&mut bytes, &record.endpoint_id);
    append_length(&mut bytes, record.addresses.len());
    for address in &record.addresses {
        append_string(&mut bytes, address);
    }
    append_string(&mut bytes, &record.scope_id);
    append_length(&mut bytes, record.capabilities.len());
    for capability in &record.capabilities {
        append_string(&mut bytes, capability);
    }
    append_time(&mut bytes, record.expires_at);
    bytes.extend_from_slice(&record.signing_key);
    bytes
}

fn invitation_bytes(invitation: &Invitation) -> Vec<u8> {
    let mut bytes = b"aether/direct-invitation/v1".to_vec();
    append_string(&mut bytes, &invitation.scope_id);
    append_string(&mut bytes, &invitation.agent_id);
    append_string(&mut bytes, &invitation.endpoint_id);
    append_time(&mut bytes, invitation.expires_at);
    bytes.extend_from_slice(&invitation.signing_key);
    bytes
}

fn append_string(bytes: &mut Vec<u8>, value: &str) {
    append_length(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}

/// A length as a `u64`, which holds every `usize` on the platforms Rust
/// supports; the saturation only keeps the conversion total.
fn append_length(bytes: &mut Vec<u8>, length: usize) {
    append_u64(bytes, u64::try_from(length).unwrap_or(u64::MAX));
}

fn append_time(bytes: &mut Vec<u8>, value: SystemTime) {
    let seconds = aether_core::time::unix_seconds(value).unwrap_or(0);
    append_u64(bytes, seconds);
}

fn append_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use super::{
        DiscoveryError, Record, sign_agent_card, signed_invitation, signed_record,
        validate_invitation, validate_record, verify_agent_card,
    };

    #[test]
    fn an_agent_card_signature_binds_endpoint_name_and_bytes() {
        let key = signing_key("host");
        let public = key.verifying_key().to_bytes();
        let signature = sign_agent_card(&key, "endpoint", "tudor#0042", b"{}");
        assert_eq!(
            verify_agent_card(&public, "endpoint", "tudor#0042", b"{}", &signature),
            Ok(())
        );
        for (endpoint, agent, card) in [
            ("other", "tudor#0042", b"{}".as_slice()),
            ("endpoint", "tudor#0043", b"{}".as_slice()),
            ("endpoint", "tudor#0042", b"{ }".as_slice()),
        ] {
            assert_eq!(
                verify_agent_card(&public, endpoint, agent, card, &signature),
                Err(DiscoveryError::Forged)
            );
        }
        let other = signing_key("other").verifying_key().to_bytes();
        assert_eq!(
            verify_agent_card(&other, "endpoint", "tudor#0042", b"{}", &signature),
            Err(DiscoveryError::Forged)
        );
    }
    use ed25519_dalek::SigningKey;
    use std::time::{Duration, SystemTime};

    fn signing_key(agent: &str) -> SigningKey {
        SigningKey::from_bytes(&[agent.bytes().next().unwrap_or_default(); 32])
    }

    fn record(agent: &str, capability: &str) -> Record {
        signed_record(
            &signing_key(agent),
            agent.to_owned(),
            format!("endpoint-{agent}"),
            vec!["127.0.0.1:1".to_owned()],
            "directory".to_owned(),
            vec![capability.to_owned()],
            SystemTime::now() + Duration::from_secs(60),
        )
    }

    #[test]
    fn discovery_signatures_bind_every_signed_field_and_key() {
        assert_eq!(validate_record(&record("agent", "review")), Ok(()));
        let mut changed_record = record("agent", "review");
        changed_record.endpoint_id = "other-endpoint".to_owned();
        assert_eq!(validate_record(&changed_record), Err(DiscoveryError::Forged));
        let mut changed_address_record = record("agent", "review");
        changed_address_record.addresses = vec!["127.0.0.1:2".to_owned()];
        assert_eq!(
            validate_record(&changed_address_record),
            Err(DiscoveryError::Forged)
        );
        let mut missing_address_record = record("agent", "review");
        missing_address_record.addresses.clear();
        assert_eq!(
            validate_record(&missing_address_record),
            Err(DiscoveryError::Forged)
        );
        let mut wrong_key_record = record("agent", "review");
        wrong_key_record.signing_key = signing_key("other").verifying_key().to_bytes();
        assert_eq!(validate_record(&wrong_key_record), Err(DiscoveryError::Forged));

        let mut changed_invitation = signed_invitation(
            &signing_key("agent"),
            "direct".to_owned(),
            "agent".to_owned(),
            "endpoint".to_owned(),
            SystemTime::now() + Duration::from_secs(60),
        );
        changed_invitation.scope_id = "other".to_owned();
        assert_eq!(
            validate_invitation(&changed_invitation),
            Err(DiscoveryError::Forged)
        );
    }
}
