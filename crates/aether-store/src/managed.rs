use aether_core::ScopeKind;
use rusqlite::{Connection, OptionalExtension, params};

use crate::CachedSql;
use crate::{NodeStore, StoreError, ensure_scope, now};

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

const MEMBER_COLUMNS: &str = "scope_id,agent_id,endpoint_id,role,expires_at,revoked_at";

impl ManagedMember {
    /// Reads the [`MEMBER_COLUMNS`].
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            scope_id: row.get(0)?,
            agent_id: row.get(1)?,
            endpoint_id: row.get(2)?,
            role: row.get(3)?,
            expires_at: row.get(4)?,
            revoked_at: row.get(5)?,
        })
    }

    /// [`StoreError::InvalidState`] unless every field is set, the member is
    /// not revoked and it expires after `expires_after`.
    pub const fn check_new(&self, expires_after: i64) -> Result<(), StoreError> {
        if self.scope_id.is_empty()
            || self.agent_id.is_empty()
            || self.endpoint_id.is_empty()
            || self.role.is_empty()
            || self.expires_at <= expires_after
            || self.revoked_at.is_some()
        {
            Err(StoreError::InvalidState)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn is_active(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

/// The members of `scope_id`, by agent.
pub fn members_in_scope(
    connection: &Connection,
    scope_id: &str,
) -> Result<Vec<ManagedMember>, StoreError> {
    if scope_id.is_empty() {
        return Err(StoreError::InvalidState);
    }
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {MEMBER_COLUMNS} FROM managed_members WHERE scope_id=?1 ORDER BY agent_id"
    ))?;
    Ok(statement
        .query_map([scope_id], ManagedMember::from_row)?
        .collect::<Result<_, _>>()?)
}

/// The `configuration` key of the roster cursor of `scope_id` at the control
/// service `control`.
fn cursor_key(control: &str, scope_id: &str) -> String {
    let mut value = control.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(scope_id.as_bytes());
    format!("managed.cursor.{}", blake3::hash(&value).to_hex())
}

impl NodeStore {
    /// The last roster event of `scope_id` at `control` that this node
    /// applied; 0 before the first.
    pub fn managed_cursor(
        &self,
        control: &str,
        scope_id: &str,
    ) -> Result<i64, StoreError> {
        cursor(&self.connection, &cursor_key(control, scope_id))
    }

    /// Applies the roster event `cursor` of `control`: `member` joined.
    ///
    /// A member that this node knows as revoked stays revoked. The control
    /// service never lets a revoked agent join its scope again, so such an
    /// event is an old one that a second cursor of the scope reads again
    /// (the cursor has the control URL in its key, the roster does not).
    pub fn apply_managed_member(
        &mut self,
        control: &str,
        cursor: i64,
        member: &ManagedMember,
    ) -> Result<(), StoreError> {
        member.check_new(0)?;
        let transaction = self.connection.transaction()?;
        advance_cursor(&transaction, &cursor_key(control, &member.scope_id), cursor)?;
        ensure_scope(&transaction, &member.scope_id, ScopeKind::Managed)?;
        transaction.execute_cached(
            &format!(
                "INSERT INTO managed_members({MEMBER_COLUMNS}) VALUES(?1,?2,?3,?4,?5,NULL)
                 ON CONFLICT(scope_id,agent_id) DO UPDATE SET endpoint_id=excluded.endpoint_id,role=excluded.role,expires_at=excluded.expires_at
                 WHERE managed_members.revoked_at IS NULL"
            ),
            params![member.scope_id, member.agent_id, member.endpoint_id, member.role, member.expires_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Applies the roster event `cursor` of `control`: `agent_id` left
    /// `scope_id`. For a member that is revoked already only the cursor
    /// moves, so a second cursor of the scope gets past the event. An agent
    /// that the roster does not have is [`StoreError::NotFound`].
    pub fn revoke_managed_member(
        &mut self,
        control: &str,
        cursor: i64,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        if scope_id.is_empty() || agent_id.is_empty() {
            return Err(StoreError::InvalidState);
        }
        let transaction = self.connection.transaction()?;
        advance_cursor(&transaction, &cursor_key(control, scope_id), cursor)?;
        if transaction.execute_cached(
            "UPDATE managed_members SET revoked_at=COALESCE(revoked_at,?1) WHERE scope_id=?2 AND agent_id=?3",
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
        members_in_scope(&self.connection, scope_id)
    }

    /// The members of every managed scope who are not revoked and have not
    /// expired at `now`.
    pub fn active_member_count(&self, now: i64) -> Result<usize, StoreError> {
        let count: i64 = self.connection.query_row_cached(
            "SELECT COUNT(*) FROM managed_members WHERE expires_at>?1 AND revoked_at IS NULL",
            [now],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| StoreError::InvalidState)
    }

    pub fn all_managed_members(&self) -> Result<Vec<ManagedMember>, StoreError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {MEMBER_COLUMNS} FROM managed_members ORDER BY scope_id,agent_id"
        ))?;
        Ok(statement
            .query_map([], ManagedMember::from_row)?
            .collect::<Result<_, _>>()?)
    }
}

/// The cursor kept under `key`; 0 when there is none.
pub fn cursor(connection: &Connection, key: &str) -> Result<i64, StoreError> {
    let stored: Option<String> = connection
        .query_row_cached(
            "SELECT value FROM configuration WHERE key=?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        None => Ok(0),
        Some(value) => match value.parse::<i64>() {
            Ok(cursor) if cursor >= 0 => Ok(cursor),
            _ => Err(StoreError::InvalidState),
        },
    }
}

/// Moves the cursor kept under `key` forward to `cursor`; a cursor that does
/// not move forward is [`StoreError::InvalidState`].
pub fn advance_cursor(
    connection: &Connection,
    key: &str,
    next: i64,
) -> Result<(), StoreError> {
    if next <= cursor(connection, key)? {
        return Err(StoreError::InvalidState);
    }
    connection.execute_cached(
        "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, next.to_string()],
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
            .apply_managed_member("control", 1, &member)
            .expect("member event must apply");
        assert_eq!(store.managed_cursor("control", "managed"), Ok(1));
        assert_eq!(
            store.managed_members("managed").expect("members must load"),
            vec![member.clone()]
        );
        assert_eq!(store.summary().expect("summary must load").agents, 1);
        assert_eq!(
            store.apply_managed_member("control", 1, &member),
            Err(StoreError::InvalidState)
        );
        store
            .revoke_managed_member("control", 2, "managed", "member-agent")
            .expect("revocation event must apply");
        assert_eq!(store.summary().expect("summary must load").agents, 0);
        drop(store);

        let store = NodeStore::open(&database).expect("node store must reopen");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 1);
        assert!(members[0].revoked_at.is_some());
        assert_eq!(store.managed_cursor("control", "managed"), Ok(2));
        fs::remove_file(database).expect("database must remove");
    }

    /// The roster cursor has the control URL in its key. A second spelling
    /// of the URL reads the events of the scope again from the start.
    #[test]
    fn a_second_control_url_does_not_bring_a_revoked_member_back() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        let member = ManagedMember {
            scope_id: "managed".to_owned(),
            agent_id: "member-agent".to_owned(),
            endpoint_id: "member-endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at: i64::MAX,
            revoked_at: None,
        };
        store.apply_managed_member("https://control", 1, &member).expect("join");
        store
            .revoke_managed_member("https://control", 2, "managed", "member-agent")
            .expect("revoke");
        let revoked = store.managed_members("managed").expect("members must load");
        assert!(revoked[0].revoked_at.is_some());

        store.apply_managed_member("https://control/", 1, &member).expect("old join");
        assert_eq!(store.managed_members("managed").expect("members"), revoked);
        store
            .revoke_managed_member("https://control/", 2, "managed", "member-agent")
            .expect("the old revoke must move the second cursor");
        assert_eq!(store.managed_members("managed").expect("members"), revoked);
        assert_eq!(store.managed_cursor("https://control/", "managed"), Ok(2));
        assert_eq!(
            store.revoke_managed_member("https://control/", 3, "managed", "stranger"),
            Err(StoreError::NotFound)
        );
        assert_eq!(store.managed_cursor("https://control/", "managed"), Ok(2));
    }

    #[test]
    fn a_managed_member_keeps_its_wire_form() {
        let member = ManagedMember {
            scope_id: "scope".to_owned(),
            agent_id: "agent".to_owned(),
            endpoint_id: "endpoint".to_owned(),
            role: "member".to_owned(),
            expires_at: 1_900_000_000,
            revoked_at: None,
        };
        let wire = serde_json::json!({
            "scopeId": "scope",
            "agentId": "agent",
            "endpointId": "endpoint",
            "role": "member",
            "expiresAt": 1_900_000_000,
            "revokedAt": null,
        });
        assert_eq!(serde_json::to_value(&member).expect("member must encode"), wire);
        let revoked = ManagedMember { revoked_at: Some(1_800_000_000), ..member };
        let mut revoked_wire = wire;
        revoked_wire["revokedAt"] = serde_json::json!(1_800_000_000);
        assert_eq!(
            serde_json::from_value::<ManagedMember>(revoked_wire.clone())
                .expect("member must decode"),
            revoked
        );
        assert_eq!(
            serde_json::to_value(&revoked).expect("member must encode"),
            revoked_wire
        );
    }
}
