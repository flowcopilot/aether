use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{ManagedMember, SignedRecord, StoreError, migrate::open_connection, now};

pub const CONTROL_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/control/001_initial.sql"),
    include_str!("../migrations/control/002_directory_events.sql"),
    include_str!("../migrations/control/003_managed_members.sql"),
    include_str!("../migrations/control/004_managed_events.sql"),
];

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryEvent {
    pub sequence: i64,
    pub event_type: String,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ManagedEvent {
    pub sequence: i64,
    pub event_type: String,
    pub scope_id: String,
    pub agent_id: String,
    pub member: Option<ManagedMember>,
}

pub struct ControlStore {
    connection: Connection,
}

impl ControlStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), CONTROL_MIGRATIONS)? })
    }

    pub fn put_scope(&mut self, id: &str, kind: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            params![id, kind],
        )?;
        Ok(())
    }

    pub fn put_signed_record(
        &mut self,
        record: &SignedRecord,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![record.id, record.scope_id, record.agent_id, record.record, record.expires_at],
        )?;
        Ok(())
    }

    pub fn publish_signed_record(
        &mut self,
        record: &SignedRecord,
        event_type: &str,
    ) -> Result<(), StoreError> {
        if !matches!(event_type, "published" | "renewed") {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'directory') ON CONFLICT(id) DO UPDATE SET kind=excluded.kind",
            [&record.scope_id],
        )?;
        transaction.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![record.id, record.scope_id, record.agent_id, record.record, record.expires_at],
        )?;
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![event_type, record.scope_id, record.agent_id, record.record, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn signed_record(&self, agent_id: &str) -> Result<SignedRecord, StoreError> {
        self.connection.query_row(
            "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE agent_id=?1 ORDER BY expires_at DESC LIMIT 1",
            [agent_id],
            SignedRecord::from_row,
        ).optional()?.ok_or(StoreError::NotFound)
    }

    pub fn remove_signed_record(&mut self, agent_id: &str) -> Result<(), StoreError> {
        if self
            .connection
            .execute("DELETE FROM signed_records WHERE agent_id=?1", [agent_id])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        Ok(())
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
        if transaction
            .execute("DELETE FROM signed_records WHERE agent_id=?1", [agent_id])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO directory_events(event_type,scope_id,agent_id,record,created_at) VALUES('revoked',?1,?2,NULL,?3)",
            params![scope_id, agent_id, now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_signed_records(
        &self,
        timestamp: i64,
    ) -> Result<Vec<SignedRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE expires_at>?1 ORDER BY agent_id",
        )?;
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
        let limit = i64::try_from(limit).map_err(|_| StoreError::InvalidState)?;
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,record FROM directory_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![sequence, limit], |row| {
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
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'managed') ON CONFLICT(id) DO NOTHING",
            [scope_id],
        )?;
        let kind: String = transaction.query_row(
            "SELECT kind FROM scopes WHERE id=?1",
            [scope_id],
            |row| row.get(0),
        )?;
        if kind != "managed" {
            return Err(StoreError::InvalidState);
        }
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
        if member.scope_id.is_empty()
            || member.agent_id.is_empty()
            || member.endpoint_id.is_empty()
            || member.role.is_empty()
            || member.expires_at <= now()
            || member.revoked_at.is_some()
        {
            return Err(StoreError::InvalidState);
        }
        let timestamp = now();
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
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES('joined',?1,?2,?3,?4,?5,?6)",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at, timestamp],
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
            "INSERT INTO managed_events(event_type,scope_id,agent_id,endpoint_id,role,expires_at,created_at) VALUES('revoked',?1,?2,NULL,NULL,NULL,?3)",
            params![scope_id, agent_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn managed_members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<ManagedMember>, StoreError> {
        if scope_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let mut statement = self.connection.prepare(
            "SELECT scope_id,agent_id,endpoint_id,role,expires_at,revoked_at FROM managed_members WHERE scope_id=?1 ORDER BY agent_id",
        )?;
        Ok(statement
            .query_map([scope_id], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
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
        let limit = i64::try_from(limit).map_err(|_| StoreError::InvalidState)?;
        let mut statement = self.connection.prepare(
            "SELECT sequence,event_type,scope_id,agent_id,endpoint_id,role,expires_at FROM managed_events WHERE scope_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        Ok(statement
            .query_map(params![scope_id, sequence, limit], |row| {
                let event_type: String = row.get(1)?;
                let member = match event_type.as_str() {
                    "joined" => Some(ManagedMember {
                        scope_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        endpoint_id: row.get(4)?,
                        role: row.get(5)?,
                        expires_at: row.get(6)?,
                        revoked_at: None,
                    }),
                    "revoked" => None,
                    _ => return Err(rusqlite::Error::InvalidQuery),
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
    use crate::{test_support::*, *};

    #[test]
    fn control_records_are_durable() {
        let database = path("control-record");
        let mut store = ControlStore::open(&database).expect("control must open");
        store.put_scope("scope", "directory").expect("scope must store");
        let record = SignedRecord {
            id: "record".to_owned(),
            scope_id: "scope".to_owned(),
            agent_id: "agent".to_owned(),
            record: vec![1],
            expires_at: 1,
        };
        store.put_signed_record(&record).expect("record must store");
        drop(store);
        let mut store = ControlStore::open(database).expect("control must open");
        assert_eq!(store.signed_record("agent"), Ok(record));
        assert_eq!(store.active_signed_records(0).expect("records must load").len(), 1);
        assert!(store.active_signed_records(1).expect("records must load").is_empty());
        store.remove_signed_record("agent").expect("record must remove");
        assert_eq!(store.signed_record("agent"), Err(StoreError::NotFound));
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
        assert_eq!(events[0].event_type, "joined");
        assert_eq!(events[1].event_type, "joined");
        assert_eq!(events[2].event_type, "revoked");
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
