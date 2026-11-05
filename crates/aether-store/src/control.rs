//! The database of the control service, a separate process that runs a
//! directory and managed scopes.

use std::path::Path;

use aether_core::ScopeKind;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    ManagedMember, SignedRecord, StoreError,
    directory::{RECORD_COLUMNS, upsert_record},
    ensure_scope,
    managed::members_in_scope,
    migrate::open_connection,
    now, put_scope, sql_count, sql_named,
};

pub const CONTROL_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/control/001_initial.sql"),
    include_str!("../migrations/control/002_directory_events.sql"),
    include_str!("../migrations/control/003_managed_members.sql"),
    include_str!("../migrations/control/004_managed_events.sql"),
    include_str!("../migrations/control/005_drop_unused_tables.sql"),
];

/// What happened to a directory record. The names are the
/// `directory_events.event_type` column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DirectoryEventType {
    Published,
    Renewed,
    Revoked,
}

aether_core::named!(DirectoryEventType {
    Published => "published",
    Renewed => "renewed",
    Revoked => "revoked",
});

/// What happened to a member of a managed scope. The names are the
/// `managed_events.event_type` column.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ManagedEventType {
    Joined,
    Revoked,
}

aether_core::named!(ManagedEventType { Joined => "joined", Revoked => "revoked" });

sql_named!(DirectoryEventType, ManagedEventType);

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryEvent {
    pub sequence: i64,
    pub event_type: DirectoryEventType,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ManagedEvent {
    pub sequence: i64,
    pub event_type: ManagedEventType,
    pub scope_id: String,
    pub agent_id: String,
    pub member: Option<ManagedMember>,
}

/// The control service keeps one directory record for each agent, so the
/// agent id is the record id.
pub struct ControlStore {
    connection: Connection,
}

impl ControlStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), CONTROL_MIGRATIONS)? })
    }

    /// Keeps `record` and its `event`, which publishes or renews it.
    pub fn publish_signed_record(
        &mut self,
        record: &SignedRecord,
        event: DirectoryEventType,
    ) -> Result<(), StoreError> {
        if event == DirectoryEventType::Revoked {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        put_scope(&transaction, &record.scope_id, ScopeKind::Directory)?;
        upsert_record(&transaction, &record.agent_id, record)?;
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![event, record.scope_id, record.agent_id, record.record, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn signed_record(&self, agent_id: &str) -> Result<SignedRecord, StoreError> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {RECORD_COLUMNS} FROM signed_records WHERE agent_id=?1 ORDER BY expires_at DESC LIMIT 1"
                ),
                [agent_id],
                SignedRecord::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn revoke_signed_record(&mut self, agent_id: &str) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        let scope_id: String = transaction
            .query_row(
                "SELECT scope_id FROM signed_records WHERE agent_id=?1 ORDER BY expires_at DESC LIMIT 1",
                [agent_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        transaction
            .execute("DELETE FROM signed_records WHERE agent_id=?1", [agent_id])?;
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES(?1,?2,?3,NULL,?4)",
            params![DirectoryEventType::Revoked, scope_id, agent_id, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_signed_records(
        &self,
        timestamp: i64,
    ) -> Result<Vec<SignedRecord>, StoreError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM signed_records WHERE expires_at>?1 ORDER BY agent_id"
        ))?;
        Ok(statement
            .query_map([timestamp], SignedRecord::from_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn directory_events_after(
        &self,
        sequence: i64,
        limit: usize,
    ) -> Result<Vec<DirectoryEvent>, StoreError> {
        if sequence < 0 || limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,record FROM directory_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![sequence, sql_count(limit)], |row| {
                Ok(DirectoryEvent {
                    sequence: row.get(0)?,
                    event_type: row.get(1)?,
                    scope_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    record: row.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn create_managed_invitation(
        &mut self,
        id: &str,
        scope_id: &str,
        token_hash: &[u8; 32],
        expires_at: i64,
    ) -> Result<(), StoreError> {
        if id.is_empty() || scope_id.is_empty() || expires_at <= now() {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        ensure_scope(&transaction, scope_id, ScopeKind::Managed)?;
        transaction.execute(
            "INSERT INTO managed_invitations(id,scope_id,token_hash,expires_at) VALUES(?1,?2,?3,?4)",
            params![id, scope_id, token_hash, expires_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn redeem_managed_invitation(
        &mut self,
        token_hash: &[u8; 32],
        member: &ManagedMember,
    ) -> Result<(), StoreError> {
        let timestamp = now();
        member.check_new(timestamp)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE managed_invitations SET redeemed_at=?1,redeemed_agent_id=?2 WHERE token_hash=?3 AND redeemed_at IS NULL AND expires_at>?1 AND scope_id=?4",
            params![timestamp, member.agent_id, token_hash, member.scope_id],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO managed_members(scope_id,agent_id,endpoint_id,role,expires_at,revoked_at) VALUES(?1,?2,?3,?4,?5,NULL)",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at],
        )?;
        transaction.execute(
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![ManagedEventType::Joined, member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn revoke_managed_member(
        &mut self,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        if scope_id.is_empty() || agent_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let timestamp = now();
        let transaction = self.connection.transaction()?;
        if transaction.execute(
            "UPDATE managed_members SET revoked_at=?1 WHERE scope_id=?2 AND agent_id=?3 AND revoked_at IS NULL",
            params![timestamp, scope_id, agent_id],
        )? != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES(?1,?2,?3,NULL,NULL,NULL,?4)",
            params![ManagedEventType::Revoked, scope_id, agent_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn managed_members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<ManagedMember>, StoreError> {
        members_in_scope(&self.connection, scope_id)
    }

    pub fn managed_events_after(
        &self,
        scope_id: &str,
        sequence: i64,
        limit: usize,
    ) -> Result<Vec<ManagedEvent>, StoreError> {
        if scope_id.is_empty() || sequence < 0 || limit == 0 {
            return Err(StoreError::InvalidState);
        }
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,endpoint_id,role,expires_at FROM managed_events WHERE scope_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        Ok(statement
            .query_map(params![scope_id, sequence, sql_count(limit)], |row| {
                let event_type = row.get(1)?;
                let member = match event_type {
                    ManagedEventType::Joined => Some(ManagedMember {
                        scope_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        endpoint_id: row.get(4)?,
                        role: row.get(5)?,
                        expires_at: row.get(6)?,
                        revoked_at: None,
                    }),
                    ManagedEventType::Revoked => None,
                };
                Ok(ManagedEvent {
                    sequence: row.get(0)?,
                    event_type,
                    scope_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    member,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::path;

    fn record(agent_id: &str, expires_at: i64) -> SignedRecord {
        SignedRecord {
            scope_id: "scope".to_owned(),
            agent_id: agent_id.to_owned(),
            record: vec![1],
            expires_at,
        }
    }

    #[test]
    fn published_records_are_durable_and_a_revocation_is_an_event() {
        let database = path("control-record");
        let mut store = ControlStore::open(&database).expect("control must open");
        assert_eq!(
            store.publish_signed_record(
                &record("agent", 1),
                DirectoryEventType::Revoked
            ),
            Err(StoreError::InvalidState)
        );
        store
            .publish_signed_record(&record("agent", 1), DirectoryEventType::Published)
            .expect("record must publish");
        store
            .publish_signed_record(&record("agent", 2), DirectoryEventType::Renewed)
            .expect("record must renew");
        drop(store);
        let mut store = ControlStore::open(&database).expect("control must reopen");
        assert_eq!(store.signed_record("agent"), Ok(record("agent", 2)));
        assert_eq!(store.active_signed_records(1).expect("records must load").len(), 1);
        assert!(store.active_signed_records(2).expect("records must load").is_empty());
        store.revoke_signed_record("agent").expect("record must revoke");
        assert_eq!(store.signed_record("agent"), Err(StoreError::NotFound));
        assert_eq!(
            store
                .directory_events_after(0, 10)
                .expect("events must load")
                .iter()
                .map(|event| event.event_type)
                .collect::<Vec<_>>(),
            [
                DirectoryEventType::Published,
                DirectoryEventType::Renewed,
                DirectoryEventType::Revoked
            ]
        );
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn managed_invitation_redemption_is_durable_and_atomic() {
        let database = path("managed-members");
        let mut store = ControlStore::open(&database).expect("control must open");
        let first_token = [1; 32];
        let second_token = [2; 32];
        let expires_at = i64::MAX;
        store
            .create_managed_invitation("first", "managed", &first_token, expires_at)
            .expect("first invitation must store");
        store
            .create_managed_invitation("second", "managed", &second_token, expires_at)
            .expect("second invitation must store");
        let first_member = ManagedMember {
            scope_id: "managed".to_owned(),
            agent_id: "first-agent".to_owned(),
            endpoint_id: "first-endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at,
            revoked_at: None,
        };
        store
            .redeem_managed_invitation(&first_token, &first_member)
            .expect("first invitation must redeem");
        let conflicting_member = ManagedMember {
            agent_id: "other-agent".to_owned(),
            endpoint_id: first_member.endpoint_id.clone(),
            ..first_member.clone()
        };
        assert!(
            store
                .redeem_managed_invitation(&second_token, &conflicting_member)
                .is_err()
        );
        let second_member = ManagedMember {
            agent_id: "second-agent".to_owned(),
            endpoint_id: "second-endpoint".to_owned(),
            ..first_member.clone()
        };
        store
            .redeem_managed_invitation(&second_token, &second_member)
            .expect("failed redemption must not consume the invitation");
        store
            .revoke_managed_member("managed", "first-agent")
            .expect("member must revoke");
        let events =
            store.managed_events_after("managed", 0, 128).expect("events must load");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, ManagedEventType::Joined);
        assert_eq!(events[1].event_type, ManagedEventType::Joined);
        assert_eq!(events[2].event_type, ManagedEventType::Revoked);
        assert!(events[0].member.is_some());
        assert!(events[2].member.is_none());
        assert!(
            store
                .managed_events_after("managed", events[1].sequence, 128)
                .expect("events must resume")
                .iter()
                .all(|event| event.sequence > events[1].sequence)
        );
        drop(store);

        let store = ControlStore::open(&database).expect("control must reopen");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].agent_id, first_member.agent_id);
        assert_eq!(members[0].endpoint_id, first_member.endpoint_id);
        assert_eq!(members[0].role, first_member.role);
        assert_eq!(members[0].expires_at, first_member.expires_at);
        assert!(members[0].revoked_at.is_some());
        assert_eq!(members[1], second_member);
        fs::remove_file(database).expect("database must remove");
    }
}
