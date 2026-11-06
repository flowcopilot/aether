//! The JSON forms of directory records, invitations and roster events, as
//! the control service and its clients send them, and the routes they use.

use std::time::{Duration, SystemTime};

use aether_core::time::{from_unix_seconds, unix_seconds};
use aether_store::{DirectoryEventType, ManagedEventType, ManagedMember, SignedRecord};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{DiscoveryError, Invitation, Record, signed_record, validate_invitation};

/// `/v1/records`: publish and search; `/v1/records/{agent}`: resolve and
/// revoke.
pub const RECORDS: [&str; 2] = ["v1", "records"];
/// `/v1/events`: the directory changes after a cursor.
pub const EVENTS: [&str; 2] = ["v1", "events"];
/// `/v1/managed/redeem`: join a managed scope with an invitation token.
pub const MANAGED_REDEEM: [&str; 3] = ["v1", "managed", "redeem"];
/// `/v1/managed/{scope}/…`: the roster of one managed scope.
pub const MANAGED: [&str; 2] = ["v1", "managed"];
/// The roster operations under [`MANAGED`] and a scope.
pub const MEMBERS: &str = "members";
pub const MANAGED_EVENTS: &str = "events";
pub const MANAGED_MEMBER_ROLE: &str = "member";

/// The most events one page of changes holds. A client reads pages until
/// one is empty.
pub const EVENT_PAGE: usize = 128;

/// The path of `segments`, as a server route.
#[must_use]
pub fn route(segments: &[&str]) -> String {
    segments.iter().fold(String::new(), |path, segment| format!("{path}/{segment}"))
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireRecord {
    pub agent_id: String,
    pub endpoint_id: String,
    pub addresses: Vec<String>,
    pub scope_id: String,
    pub capabilities: Vec<String>,
    pub expires_at: u64,
    #[serde(with = "hex::serde")]
    pub signing_key: [u8; 32],
    #[serde(with = "hex::serde")]
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireInvitation {
    pub scope_id: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub expires_at: u64,
    #[serde(with = "hex::serde")]
    pub signing_key: [u8; 32],
    #[serde(with = "hex::serde")]
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireDirectoryEvent {
    pub cursor: i64,
    pub event: DirectoryEventType,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Option<WireRecord>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireManagedRedeem {
    pub token: String,
    pub record: WireRecord,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireManagedEvent {
    pub cursor: i64,
    pub event: ManagedEventType,
    pub scope_id: String,
    pub agent_id: String,
    pub member: Option<ManagedMember>,
}

impl WireRecord {
    /// A record signed with a public test key, which `aether control
    /// test-record` prints for trying a directory by hand.
    #[must_use]
    pub fn test_record() -> Self {
        let key = SigningKey::from_bytes(&[7; 32]);
        Self::from_record(&signed_record(
            &key,
            "test-agent".to_owned(),
            "https://127.0.0.1:9443".to_owned(),
            vec!["127.0.0.1:9443".to_owned()],
            "test-scope".to_owned(),
            vec!["test".to_owned()],
            // A clock an hour from its end cannot hold the expiry; the record
            // then expires at once.
            SystemTime::now()
                .checked_add(Duration::from_secs(3_600))
                .unwrap_or_else(SystemTime::now),
        ))
    }

    #[must_use]
    pub fn from_record(record: &Record) -> Self {
        Self {
            agent_id: record.agent_id.clone(),
            endpoint_id: record.endpoint_id.clone(),
            addresses: record.addresses.clone(),
            scope_id: record.scope_id.clone(),
            capabilities: record.capabilities.clone(),
            expires_at: unix_seconds(record.expires_at).unwrap_or(0),
            signing_key: record.signing_key,
            signature: record.signature.clone(),
        }
    }

    /// The record, not yet verified.
    ///
    /// # Errors
    ///
    /// [`DiscoveryError::Malformed`] when the expiry is not a time.
    pub fn to_record(&self) -> Result<Record, DiscoveryError> {
        Ok(Record {
            agent_id: self.agent_id.clone(),
            endpoint_id: self.endpoint_id.clone(),
            addresses: self.addresses.clone(),
            scope_id: self.scope_id.clone(),
            capabilities: self.capabilities.clone(),
            expires_at: from_unix_seconds(self.expires_at)
                .ok_or(DiscoveryError::Malformed)?,
            signing_key: self.signing_key,
            signature: self.signature.clone(),
        })
    }

    pub fn signed_store_record(&self) -> Result<SignedRecord, DiscoveryError> {
        Ok(SignedRecord {
            scope_id: self.scope_id.clone(),
            agent_id: self.agent_id.clone(),
            record: serde_json::to_vec(self).map_err(|_| DiscoveryError::Malformed)?,
            expires_at: i64::try_from(self.expires_at)
                .map_err(|_| DiscoveryError::Malformed)?,
        })
    }
}

impl WireInvitation {
    #[must_use]
    pub fn from_invitation(invitation: &Invitation) -> Self {
        Self {
            scope_id: invitation.scope_id.clone(),
            agent_id: invitation.agent_id.clone(),
            endpoint_id: invitation.endpoint_id.clone(),
            expires_at: unix_seconds(invitation.expires_at).unwrap_or(0),
            signing_key: invitation.signing_key,
            signature: invitation.signature.clone(),
        }
    }

    /// The invitation, verified.
    ///
    /// # Errors
    ///
    /// The [`DiscoveryError`] of a malformed, expired or forged invitation.
    pub fn to_invitation(&self) -> Result<Invitation, DiscoveryError> {
        let invitation = Invitation {
            scope_id: self.scope_id.clone(),
            agent_id: self.agent_id.clone(),
            endpoint_id: self.endpoint_id.clone(),
            expires_at: from_unix_seconds(self.expires_at)
                .ok_or(DiscoveryError::Malformed)?,
            signing_key: self.signing_key,
            signature: self.signature.clone(),
        };
        validate_invitation(&invitation)?;
        Ok(invitation)
    }

    pub fn signed_store_record(&self) -> Result<SignedRecord, DiscoveryError> {
        Ok(SignedRecord {
            scope_id: self.scope_id.clone(),
            agent_id: self.agent_id.clone(),
            record: serde_json::to_vec(self).map_err(|_| DiscoveryError::Malformed)?,
            expires_at: i64::try_from(self.expires_at)
                .map_err(|_| DiscoveryError::Malformed)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENTS, MANAGED_REDEEM, RECORDS, route};

    #[test]
    fn routes_are_the_paths_of_their_segments() {
        assert_eq!(route(&RECORDS), "/v1/records");
        assert_eq!(route(&EVENTS), "/v1/events");
        assert_eq!(route(&MANAGED_REDEEM), "/v1/managed/redeem");
    }
}
