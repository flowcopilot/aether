use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::CachedSql;
use crate::{NodeStore, StoreError, sql_named};

/// The side of a hub invite that this node's page acted on. The strings are
/// the `hub_approvals.side` column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalSide {
    /// This node offers its agent: its page sent the invite or approved a
    /// request.
    Sender,
    /// This node gets access: its page accepted an invite or sent a request.
    Receiver,
}

/// How a hub invite started. The strings are the `hub_approvals.kind`
/// column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalKind {
    /// The host invited someone by name.
    Invite,
    /// Someone asked a listed service agent for access.
    Request,
}

aether_core::named!(ApprovalSide { Sender => "sender", Receiver => "receiver" });
aether_core::named!(ApprovalKind { Invite => "invite", Request => "request" });
sql_named!(ApprovalSide, ApprovalKind);

/// A hub invite that this node's own page sent or approved. The node signs
/// a grant for a hub row only when such a record names that row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HubApproval {
    pub invite_id: String,
    pub side: ApprovalSide,
    pub kind: ApprovalKind,
    /// The name of the other side on the hub.
    pub peer_name: String,
    /// The endpoint of the other side, once the node signed for it. A record
    /// never moves to another endpoint.
    pub peer_endpoint: Option<String>,
    /// The most days of access that the owner approved.
    pub days: u32,
    pub access_group: String,
    /// The signed line, kept so that a second delivery signs nothing new.
    pub bundle: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    /// For a request that this node sent: the listing it was sent for.
    pub listing: Option<KeptListing>,
}

/// A public listing as the hub returned it: the base64url text, the
/// signature of the endpoint key, and the expiry in Unix seconds.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeptListing {
    pub text: String,
    pub signature: String,
    pub expires_at: i64,
}

/// An access request that an agent prepared and the person has not
/// confirmed yet.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessRequestDraft {
    pub id: String,
    /// The full name of the listed service agent.
    pub target_name: String,
    pub listing: KeptListing,
    pub days: u32,
    pub created_at: i64,
    pub expires_at: i64,
}

/// The most drafts that wait for the person at one time.
pub const MAX_ACCESS_REQUEST_DRAFTS: i64 = 5;

impl NodeStore {
    /// Keeps `approval`, in place of a record with the same invite id.
    pub fn put_hub_approval(&self, approval: &HubApproval) -> Result<(), StoreError> {
        self.connection.execute_cached(
            "INSERT OR REPLACE INTO hub_approvals(invite_id,side,kind,peer_name,peer_endpoint,days,access_group,bundle,created_at,expires_at,listing,listing_signature,listing_expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                approval.invite_id,
                approval.side,
                approval.kind,
                approval.peer_name,
                approval.peer_endpoint,
                approval.days,
                approval.access_group,
                approval.bundle,
                approval.created_at,
                approval.expires_at,
                approval.listing.as_ref().map(|listing| &listing.text),
                approval.listing.as_ref().map(|listing| &listing.signature),
                approval.listing.as_ref().map(|listing| listing.expires_at),
            ],
        )?;
        Ok(())
    }

    pub fn hub_approval(
        &self,
        invite_id: &str,
    ) -> Result<Option<HubApproval>, StoreError> {
        Ok(self
            .connection
            .query_row_cached(
                "SELECT invite_id,side,kind,peer_name,peer_endpoint,days,access_group,bundle,created_at,expires_at,listing,listing_signature,listing_expires_at FROM hub_approvals WHERE invite_id=?1",
                [invite_id],
                |row| {
                    let listing = match (row.get(10)?, row.get(11)?, row.get(12)?) {
                        (Some(text), Some(signature), Some(expires_at)) => {
                            Some(KeptListing { text, signature, expires_at })
                        }
                        _ => None,
                    };
                    Ok(HubApproval {
                        invite_id: row.get(0)?,
                        side: row.get(1)?,
                        kind: row.get(2)?,
                        peer_name: row.get(3)?,
                        peer_endpoint: row.get(4)?,
                        days: row.get(5)?,
                        access_group: row.get(6)?,
                        bundle: row.get(7)?,
                        created_at: row.get(8)?,
                        expires_at: row.get(9)?,
                        listing,
                    })
                },
            )
            .optional()?)
    }

    /// Binds `approval`, the record that the caller read, to `endpoint` and
    /// keeps the signed `bundle`. A record that already names another
    /// endpoint does not move, and a record that another approval with the
    /// same invite ID replaced (another name or another time) is not pinned:
    /// [`StoreError::Unauthorized`].
    pub fn pin_hub_approval(
        &self,
        approval: &HubApproval,
        endpoint: &str,
        bundle: &str,
    ) -> Result<(), StoreError> {
        let invite_id = approval.invite_id.as_str();
        let changed = self.connection.execute_cached(
            "UPDATE hub_approvals SET peer_endpoint=?2, bundle=?3 WHERE invite_id=?1 AND peer_name=?4 AND created_at=?5 AND (peer_endpoint IS NULL OR peer_endpoint=?2)",
            params![invite_id, endpoint, bundle, approval.peer_name, approval.created_at],
        )?;
        if changed == 1 {
            return Ok(());
        }
        match self.hub_approval(invite_id)? {
            Some(_) => Err(StoreError::Unauthorized),
            None => Err(StoreError::NotFound),
        }
    }

    /// Removes the approval of `invite_id`. Removing one that is not there is
    /// not an error.
    pub fn remove_hub_approval(&self, invite_id: &str) -> Result<(), StoreError> {
        self.connection.execute_cached(
            "DELETE FROM hub_approvals WHERE invite_id=?1",
            [invite_id],
        )?;
        Ok(())
    }

    /// Removes every approval that expired at Unix time `now`, and returns
    /// how many it removed.
    pub fn clear_expired_hub_approvals(&self, now: i64) -> Result<usize, StoreError> {
        Ok(self.connection.execute_cached(
            "DELETE FROM hub_approvals WHERE expires_at <= ?1",
            [now],
        )?)
    }

    /// Keeps a draft that waits for the person. [`StoreError::Overloaded`]
    /// when [`MAX_ACCESS_REQUEST_DRAFTS`] drafts wait, and [`StoreError::DuplicateConflict`] when a
    /// draft names the same agent.
    pub fn add_access_request_draft(
        &mut self,
        draft: &AccessRequestDraft,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let open: i64 = transaction.query_row_cached(
            "SELECT COUNT(*) FROM access_request_drafts WHERE expires_at > ?1",
            [draft.created_at],
            |row| row.get(0),
        )?;
        if open >= MAX_ACCESS_REQUEST_DRAFTS {
            return Err(StoreError::Overloaded);
        }
        transaction.execute_cached(
            "DELETE FROM access_request_drafts WHERE expires_at <= ?1",
            [draft.created_at],
        )?;
        let inserted = transaction.execute_cached(
            "INSERT OR IGNORE INTO access_request_drafts(id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                draft.id,
                draft.target_name,
                draft.listing.text,
                draft.listing.signature,
                draft.listing.expires_at,
                draft.days,
                draft.created_at,
                draft.expires_at,
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::DuplicateConflict);
        }
        transaction.commit()?;
        Ok(())
    }

    /// The drafts that wait at Unix time `now`, oldest first.
    pub fn access_request_drafts(
        &self,
        now: i64,
    ) -> Result<Vec<AccessRequestDraft>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at FROM access_request_drafts WHERE expires_at > ?1 ORDER BY created_at, id",
        )?;
        let drafts = statement
            .query_map([now], draft_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(drafts)
    }

    pub fn access_request_draft(
        &self,
        id: &str,
    ) -> Result<Option<AccessRequestDraft>, StoreError> {
        Ok(self
            .connection
            .query_row_cached(
                "SELECT id,target_name,listing,signature,listing_expires_at,days,created_at,expires_at FROM access_request_drafts WHERE id=?1",
                [id],
                draft_from_row,
            )
            .optional()?)
    }

    /// Removes the draft `id`. Removing one that is not there is not an
    /// error.
    pub fn remove_access_request_draft(&self, id: &str) -> Result<(), StoreError> {
        self.connection
            .execute_cached("DELETE FROM access_request_drafts WHERE id=?1", [id])?;
        Ok(())
    }

    /// Whether this node asked the agent `name` for access, or prepared a
    /// request to it, and the request is open at Unix time `now`.
    pub fn access_requested(&self, name: &str, now: i64) -> Result<bool, StoreError> {
        Ok(self.connection.query_row_cached(
            "SELECT EXISTS(SELECT 1 FROM access_request_drafts WHERE target_name=?1 AND expires_at > ?2) OR EXISTS(SELECT 1 FROM hub_approvals WHERE peer_name=?1 AND side=?3 AND kind=?4 AND expires_at > ?2)",
            params![name, now, ApprovalSide::Receiver, ApprovalKind::Request],
            |row| row.get(0),
        )?)
    }

    /// Keeps the block of `endpoint`, with the name it had, at Unix time
    /// `now`. A second block of one endpoint keeps the first row.
    pub fn put_hub_block(
        &self,
        endpoint: &str,
        name: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute_cached(
            "INSERT OR IGNORE INTO hub_blocks(endpoint,name,created_at) VALUES (?1,?2,?3)",
            params![endpoint, name, now],
        )?;
        Ok(())
    }

    pub fn hub_blocked(&self, endpoint: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row_cached(
            "SELECT EXISTS(SELECT 1 FROM hub_blocks WHERE endpoint=?1)",
            [endpoint],
            |row| row.get(0),
        )?)
    }

    /// Removes the block of `endpoint`. Removing one that is not there is not
    /// an error.
    pub fn remove_hub_block(&self, endpoint: &str) -> Result<(), StoreError> {
        self.connection
            .execute_cached("DELETE FROM hub_blocks WHERE endpoint=?1", [endpoint])?;
        Ok(())
    }
}

fn draft_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccessRequestDraft> {
    Ok(AccessRequestDraft {
        id: row.get(0)?,
        target_name: row.get(1)?,
        listing: KeptListing {
            text: row.get(2)?,
            signature: row.get(3)?,
            expires_at: row.get(4)?,
        },
        days: row.get(5)?,
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::*;

    fn approval(invite_id: &str, expires_at: i64) -> HubApproval {
        HubApproval {
            invite_id: invite_id.to_owned(),
            side: ApprovalSide::Sender,
            kind: ApprovalKind::Invite,
            peer_name: "ana".to_owned(),
            peer_endpoint: None,
            days: 7,
            access_group: "friends".to_owned(),
            bundle: None,
            created_at: 100,
            expires_at,
            listing: None,
        }
    }

    fn draft(id: &str, target: &str, created_at: i64) -> AccessRequestDraft {
        AccessRequestDraft {
            id: id.to_owned(),
            target_name: target.to_owned(),
            listing: KeptListing {
                text: "listing".to_owned(),
                signature: "signature".to_owned(),
                expires_at: created_at.saturating_add(1_000),
            },
            days: 7,
            created_at,
            expires_at: created_at.saturating_add(24 * 60 * 60),
        }
    }

    #[test]
    fn access_request_drafts_stop_at_five() {
        let (_directory, mut store) = crate::test_support::temp_node_store();
        for index in 0_i64..5 {
            store
                .add_access_request_draft(&draft(
                    &format!("d{index}"),
                    &format!("law{index}#0001"),
                    index.saturating_add(100),
                ))
                .expect("a draft must keep");
        }
        assert_eq!(
            store.add_access_request_draft(&draft("d5", "law5#0001", 200)),
            Err(StoreError::Overloaded)
        );
        store.remove_access_request_draft("d0").expect("a draft must go");
        assert_eq!(
            store.add_access_request_draft(&draft("d6", "law1#0001", 200)),
            Err(StoreError::DuplicateConflict)
        );
        store
            .add_access_request_draft(&draft("d5", "law5#0001", 200))
            .expect("a fifth draft must keep again");
        let drafts = store.access_request_drafts(200).expect("drafts must list");
        assert_eq!(drafts.len(), 5);
        assert_eq!(drafts[0], draft("d1", "law1#0001", 101));
        assert_eq!(
            store.access_request_draft("d5").expect("draft"),
            Some(draft("d5", "law5#0001", 200))
        );

        // After 24 hours a draft is gone, and it no longer counts.
        let later = 101_i64.saturating_add(24 * 60 * 60);
        assert_eq!(store.access_request_drafts(later).expect("drafts").len(), 4);
    }

    #[test]
    fn the_requests_group_exists_after_migration_and_blocks_are_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("node.sqlite");
        let store = NodeStore::open(&path).expect("store must open");
        let groups: Vec<String> = store
            .access_groups()
            .expect("groups must read")
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(groups, ["friends", "requests"]);

        store.put_hub_block("endpoint-a", "ana#0001", 10).expect("block must keep");
        store.put_hub_block("endpoint-a", "ana#0002", 11).expect("block must keep");
        drop(store);
        let store = NodeStore::open(&path).expect("store must open again");
        assert!(store.hub_blocked("endpoint-a").expect("block must read"));
        assert!(!store.hub_blocked("endpoint-b").expect("block must read"));
        store.remove_hub_block("endpoint-a").expect("block must go");
        store.remove_hub_block("endpoint-a").expect("a second removal is no error");
        assert!(!store.hub_blocked("endpoint-a").expect("block must read"));
    }

    #[test]
    fn a_replaced_hub_approval_does_not_take_the_line_of_the_old_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        let read = approval("invite", 900);
        store.put_hub_approval(&read).expect("approval must store");
        // The hub gives back the same invite ID for another name before the
        // worker pins the line that it signed for the first one.
        let replaced = HubApproval { peer_name: "eve#0001".to_owned(), ..read.clone() };
        store.put_hub_approval(&replaced).expect("approval must store");
        assert_eq!(
            store.pin_hub_approval(&read, "endpoint-a", "aether-invite:one"),
            Err(StoreError::Unauthorized)
        );
        let kept = store.hub_approval("invite").expect("read").expect("exists");
        assert_eq!((kept.peer_endpoint, kept.bundle), (None, None));
    }

    #[test]
    fn a_hub_approval_pins_one_endpoint_and_keeps_its_bundle() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_hub_approval(&approval("invite", 900)).expect("approval must store");
        assert_eq!(
            store.hub_approval("invite").expect("approval must read"),
            Some(approval("invite", 900))
        );
        let read = approval("invite", 900);
        store
            .pin_hub_approval(&read, "endpoint-a", "aether-invite:one")
            .expect("the first endpoint must pin");
        store
            .pin_hub_approval(&read, "endpoint-a", "aether-invite:one")
            .expect("the same endpoint may pin again");
        assert_eq!(
            store.pin_hub_approval(&read, "endpoint-b", "aether-invite:two"),
            Err(StoreError::Unauthorized)
        );
        let pinned = store
            .hub_approval("invite")
            .expect("approval must read")
            .expect("approval exists");
        assert_eq!(pinned.peer_endpoint.as_deref(), Some("endpoint-a"));
        assert_eq!(pinned.bundle.as_deref(), Some("aether-invite:one"));
        assert_eq!(
            store.pin_hub_approval(
                &approval("missing", 900),
                "endpoint-a",
                "aether-invite:one"
            ),
            Err(StoreError::NotFound)
        );
        store.remove_hub_approval("invite").expect("approval must remove");
        assert_eq!(store.hub_approval("invite").expect("approval must read"), None);
    }

    #[test]
    fn expired_hub_approvals_are_cleared() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_hub_approval(&approval("old", 500)).expect("approval must store");
        store.put_hub_approval(&approval("new", 1_500)).expect("approval must store");
        assert_eq!(store.clear_expired_hub_approvals(1_000), Ok(1));
        assert_eq!(store.hub_approval("old").expect("approval must read"), None);
        assert!(store.hub_approval("new").expect("approval must read").is_some());
    }
}
