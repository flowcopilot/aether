use rusqlite::{OptionalExtension, Transaction, params};

use crate::{NodeStore, StoreError, now};

/// A member of a managed scope. Its JSON form is the control service wire
/// form.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedMember {
    pub scope_id: String,
    pub agent_id: String,
    pub endpoint_id: String,
    pub role: String,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

impl ManagedMember {
    /// Reads `scope_id,agent_id,endpoint_id,role,expires_at,revoked_at`.
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            scope_id: row.get(0)?,
            agent_id: row.get(1)?,
            endpoint_id: row.get(2)?,
            role: row.get(3)?,
            expires_at: row.get(4)?,
            revoked_at: row.get(5)?,
        })
    }
}

impl NodeStore {
    pub fn apply_managed_member(
        &mut self,
        cursor_key: &str,
        cursor: i64,
        member: &ManagedMember,
    ) -> Result<(), StoreError> {
        if cursor_key.is_empty()
            || cursor <= 0
            || member.scope_id.is_empty()
            || member.agent_id.is_empty()
            || member.endpoint_id.is_empty()
            || member.role.is_empty()
            || member.expires_at <= 0
            || member.revoked_at.is_some()
        {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        advance_managed_cursor(&transaction, cursor_key, cursor)?;
        transaction.execute(
            "INSERT INTO scopes(id,kind) VALUES(?1,'managed') ON CONFLICT(id) DO NOTHING",
            [&member.scope_id],
        )?;
        let kind: String = transaction.query_row(
            "SELECT kind FROM scopes WHERE id=?1",
            [&member.scope_id],
            |row| row.get(0),
        )?;
        if kind != "managed" {
            return Err(StoreError::InvalidState);
        }
        transaction.execute(
            "INSERT INTO managed_members(scope_id,agent_id,endpoint_id,role,expires_at,revoked_at) VALUES(?1,?2,?3,?4,?5,NULL)
             ON CONFLICT(scope_id,agent_id) DO UPDATE SET endpoint_id=excluded.endpoint_id,role=excluded.role,expires_at=excluded.expires_at,revoked_at=NULL",
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn revoke_managed_member(
        &mut self,
        cursor_key: &str,
        cursor: i64,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        if cursor_key.is_empty()
            || cursor <= 0
            || scope_id.is_empty()
            || agent_id.is_empty()
        {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        advance_managed_cursor(&transaction, cursor_key, cursor)?;
        if transaction.execute(
            "UPDATE managed_members SET revoked_at=?1 WHERE scope_id=?2 AND agent_id=?3 AND revoked_at IS NULL",
            params![now(), scope_id, agent_id],
        )? != 1 {
            return Err(StoreError::NotFound);
        }
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

    pub fn all_managed_members(&self) -> Result<Vec<ManagedMember>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT scope_id,agent_id,endpoint_id,role,expires_at,revoked_at FROM managed_members ORDER BY scope_id,agent_id",
        )?;
        Ok(statement
            .query_map([], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
    }
}

fn advance_managed_cursor(
    transaction: &Transaction<'_>,
    cursor_key: &str,
    cursor: i64,
) -> Result<(), StoreError> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT value FROM configuration WHERE key=?1",
            [cursor_key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        let existing = existing.parse::<i64>().map_err(|_| StoreError::InvalidState)?;
        if cursor <= existing {
            return Err(StoreError::InvalidState);
        }
    }
    transaction.execute(
        "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![cursor_key, cursor.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{test_support::*, *};

    #[test]
    fn managed_roster_events_and_cursors_are_durable() {
        let database = path("managed-roster");
        let mut store = NodeStore::open(&database).expect("node store must open");
        let member = ManagedMember {
            scope_id: "managed".to_owned(),
            agent_id: "member-agent".to_owned(),
            endpoint_id: "member-endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at: i64::MAX,
            revoked_at: None,
        };
        store
            .apply_managed_member("managed.cursor", 1, &member)
            .expect("member event must apply");
        assert_eq!(store.configuration("managed.cursor"), Ok("1".to_owned()));
        assert_eq!(
            store.managed_members("managed").expect("members must load"),
            vec![member.clone()]
        );
        assert_eq!(store.summary().expect("summary must load").agents, 1);
        assert_eq!(
            store.apply_managed_member("managed.cursor", 1, &member),
            Err(StoreError::InvalidState)
        );
        store
            .revoke_managed_member("managed.cursor", 2, "managed", "member-agent")
            .expect("revocation event must apply");
        assert_eq!(store.summary().expect("summary must load").agents, 0);
        drop(store);

        let store = NodeStore::open(&database).expect("node store must reopen");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 1);
        assert!(members[0].revoked_at.is_some());
        assert_eq!(store.configuration("managed.cursor"), Ok("2".to_owned()));
        fs::remove_file(database).expect("database must remove");
    }
}
