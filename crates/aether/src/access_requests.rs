//! Access requests to listed service agents.
//!
//! An agent only prepares a request: [`draft`] keeps it on this computer.
//! The person reads the target and the listing on the caller page and
//! confirms it there. Only then does the node send the request to the hub.

use std::{path::Path, time::Duration};

use aether_store::{AccessRequestDraft, KeptListing, NodeStore, StoreError};

use crate::hub_client::{AgentRow, valid_full_name};

/// How long a draft waits for the person.
pub const DRAFT_LIFETIME: Duration = Duration::from_hours(24);

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum DraftError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("5 requests already wait for confirmation on the Aether page")]
    Limit,
    #[error(
        "a request to this agent already waits for confirmation on the Aether page"
    )]
    Duplicate,
    #[error("the requests are unavailable")]
    Store,
}

/// Keeps a request to the listed agent `target_name` for `days` days, with
/// its hub `row`, until the person confirms or discards it.
pub fn draft(
    data_dir: &Path,
    target_name: &str,
    row: &AgentRow,
    days: u64,
) -> Result<AccessRequestDraft, DraftError> {
    if !valid_full_name(target_name) {
        return Err(DraftError::Invalid(
            "use the full name of a listed agent, such as law#0042",
        ));
    }
    if row.handle != target_name
        || crate::listing::parse(&row.listing, target_name).is_err()
    {
        return Err(DraftError::Invalid("the listing of this agent is not valid"));
    }
    let now = aether_core::time::unix_timestamp(std::time::SystemTime::now())
        .ok_or(DraftError::Store)?;
    let listing_expires_at =
        i64::try_from(row.expires_at).map_err(|_| DraftError::Invalid("bad expiry"))?;
    if listing_expires_at <= now {
        return Err(DraftError::Invalid("the listing of this agent has expired"));
    }
    if !(1..=30).contains(&days) {
        return Err(DraftError::Invalid("days must be from 1 to 30"));
    }
    if crate::direct::contact_by_name(data_dir, target_name).is_ok() {
        return Err(DraftError::Invalid("this agent is a contact already"));
    }
    let lifetime = i64::try_from(DRAFT_LIFETIME.as_secs()).unwrap_or(i64::MAX);
    let draft = AccessRequestDraft {
        id: uuid::Uuid::new_v4().to_string(),
        target_name: target_name.to_owned(),
        listing: KeptListing {
            text: row.listing.clone(),
            signature: row.signature.clone(),
            expires_at: listing_expires_at,
        },
        days: u32::try_from(days).map_err(|_| DraftError::Invalid("bad days"))?,
        created_at: now,
        expires_at: now.saturating_add(lifetime),
    };
    let mut store = NodeStore::in_dir(data_dir).map_err(|_| DraftError::Store)?;
    match store.add_access_request_draft(&draft) {
        Ok(()) => Ok(draft),
        Err(StoreError::Overloaded) => Err(DraftError::Limit),
        Err(StoreError::DuplicateConflict) => Err(DraftError::Duplicate),
        Err(_) => Err(DraftError::Store),
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::{DraftError, draft};
    use crate::{agent_card::CardSettings, hub_client::AgentRow};

    /// A hub row for `handle` with the listing `bytes`.
    fn row(handle: &str, bytes: &[u8]) -> AgentRow {
        let expires_at = aether_core::time::unix_seconds(std::time::SystemTime::now())
            .expect("the clock must be after 1970")
            .saturating_add(3600);
        AgentRow {
            handle: handle.to_owned(),
            listing: URL_SAFE_NO_PAD.encode(bytes),
            signature: "AAAA".to_owned(),
            expires_at,
            online: true,
            contact: false,
            endpoint: None,
            owner: None,
        }
    }

    #[test]
    fn a_draft_needs_a_listing_that_parses() {
        let directory = tempfile::tempdir().expect("directory must create");
        let good = crate::listing::listing_bytes(&CardSettings::default(), "law#0042");
        let mut extra: serde_json::Value =
            serde_json::from_slice(&good).expect("the listing is JSON");
        extra["url"] = serde_json::json!("https://example.com");
        let other = crate::listing::listing_bytes(&CardSettings::default(), "law#0043");
        for bad in [extra.to_string().into_bytes(), other, b"not json".to_vec()] {
            assert_eq!(
                draft(directory.path(), "law#0042", &row("law#0042", &bad), 7),
                Err(DraftError::Invalid("the listing of this agent is not valid"))
            );
        }
        let good_row = row("law#0042", &good);
        assert!(draft(directory.path(), "law", &good_row, 7).is_err());
        assert!(draft(directory.path(), "law#0042", &good_row, 31).is_err());
        let expired = crate::hub_client::AgentRow { expires_at: 1, ..good_row.clone() };
        assert!(draft(directory.path(), "law#0042", &expired, 7).is_err());

        let kept = draft(directory.path(), "law#0042", &good_row, 7)
            .expect("a good listing must draft");
        assert_eq!(kept.target_name, "law#0042");
        assert_eq!(kept.listing.text, good_row.listing);
        assert_eq!(
            draft(directory.path(), "law#0042", &good_row, 7),
            Err(DraftError::Duplicate)
        );
    }
}
