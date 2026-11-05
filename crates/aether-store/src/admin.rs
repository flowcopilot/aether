use rusqlite::{OptionalExtension, params};

use crate::{NodeStore, StoreError, now, sql_count};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingChange {
    pub id: String,
    pub change_type: String,
    pub value: String,
}

impl PendingChange {
    /// Reads `id,change_type,value`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { id: row.get(0)?, change_type: row.get(1)?, value: row.get(2)? })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AuditRecord {
    pub actor_type: String,
    pub action: String,
    pub result: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditActor {
    Mcp,
    Ui,
}

impl AuditActor {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditAction {
    Change,
}

impl AuditAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AuditResult {
    Pending,
    Applied,
    Confirmed,
    Failed,
}

impl AuditResult {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

impl NodeStore {
    pub fn has_grant(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .connection
            .query_row("SELECT 1 FROM grants WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn queue_change(
        &mut self,
        id: &str,
        change_type: &str,
        value: &str,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO pending_changes(id,change_type,value,expires_at) VALUES(?1,?2,?3,?4)",
            params![id, change_type, value, expires_at],
        )?;
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','pending',?1)",
            [now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_change(
        &self,
        actor: AuditActor,
        result: AuditResult,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,'change',?2,?3)",
            params![actor.as_str(), result.as_str(), now()],
        )?;
        Ok(())
    }

    /// Restores the value that an applied change replaced and records the
    /// failure. `previous` is `None` when the change created the key.
    pub fn revert_change(
        &mut self,
        id: &str,
        change_type: &str,
        previous: Option<&str>,
        actor: AuditActor,
    ) -> Result<(), StoreError> {
        let key = format!("admin.{change_type}.{id}");
        let tx = self.connection.transaction()?;
        match previous {
            Some(value) => tx.execute(
                "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?,
            None => tx.execute("DELETE FROM configuration WHERE key=?1", [key])?,
        };
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,'change','failed',?2)",
            params![actor.as_str(), now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn confirm_pending_change(&mut self, id: &str) -> Result<(), StoreError> {
        let tx = self.connection.transaction()?;
        let (change_type, value, expires_at): (String, String, i64) = tx
            .query_row(
                "SELECT change_type,value,expires_at FROM pending_changes WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if expires_at <= now() {
            return Err(StoreError::InvalidState);
        }
        tx.execute(
            "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("admin.{change_type}.{id}"), value],
        )?;
        tx.execute("DELETE FROM pending_changes WHERE id=?1", [id])?;
        tx.execute(
            "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('ui','change','confirmed',?1)",
            [now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_changes(&self) -> Result<Vec<PendingChange>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT id,change_type,value FROM pending_changes WHERE expires_at>?1 ORDER BY id")?;
        Ok(statement
            .query_map([now()], PendingChange::from_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Reads a pending change's type and value regardless of expiry, so a
    /// caller can classify "never existed" separately from "expired" (which
    /// `confirm_pending_change` already distinguishes on the write path).
    pub fn pending_change_ignoring_expiry(
        &self,
        id: &str,
    ) -> Result<PendingChange, StoreError> {
        self.connection
            .query_row(
                "SELECT id,change_type,value FROM pending_changes WHERE id=?1",
                [id],
                PendingChange::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>, StoreError> {
        self.audit_records_page(0, usize::MAX)
    }

    /// At most `limit` audit records in order, after the first `skip`.
    pub fn audit_records_page(
        &self,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT actor_type,action,result FROM audit_records ORDER BY id LIMIT ?1 OFFSET ?2",
        )?;
        let records = statement
            .query_map([sql_count(limit), sql_count(skip)], |row| {
                Ok(AuditRecord {
                    actor_type: row.get(0)?,
                    action: row.get(1)?,
                    result: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if records.iter().all(audit_record_is_safe) {
            Ok(records)
        } else {
            Err(StoreError::InvalidState)
        }
    }
}

fn audit_record_is_safe(record: &AuditRecord) -> bool {
    matches!(record.actor_type.as_str(), "mcp" | "ui")
        && record.action == "change"
        && matches!(
            record.result.as_str(),
            "pending" | "applied" | "confirmed" | "failed"
        )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::params;

    use super::*;
    use crate::{test_support::*, *};

    fn add_audit(
        store: &NodeStore,
        actor: AuditActor,
        action: AuditAction,
        result: AuditResult,
    ) {
        store
            .connection
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,?2,?3,?4)",
                params![actor.as_str(), action.as_str(), result.as_str(), now()],
            )
            .expect("audit must store");
    }

    #[test]
    fn audit_records_are_durable_and_redacted() {
        let (store, database) = store();
        add_audit(&store, AuditActor::Mcp, AuditAction::Change, AuditResult::Pending);
        add_audit(&store, AuditActor::Ui, AuditAction::Change, AuditResult::Confirmed);
        assert_eq!(
            store.audit_records().expect("audit must load"),
            vec![
                super::AuditRecord {
                    actor_type: "mcp".to_owned(),
                    action: "change".to_owned(),
                    result: "pending".to_owned(),
                },
                super::AuditRecord {
                    actor_type: "ui".to_owned(),
                    action: "change".to_owned(),
                    result: "confirmed".to_owned(),
                },
            ]
        );
        store
            .connection
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("unsafe audit record must insert for the test");
        assert_eq!(store.audit_records(), Err(StoreError::InvalidState));
        fs::remove_file(database).expect("database must remove");
    }
}
