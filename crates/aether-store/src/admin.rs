//! Admin changes: the changes that wait for the owner's confirmation, the
//! applied changes the node replays at start, and the audit log. The
//! applied changes live in `configuration` under `admin.`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{NodeStore, StoreError, config, now, sql_count, sql_named};

/// The kind of an admin change. The names are the
/// `pending_changes.change_type` column and part of the key of an applied
/// change.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum ChangeKind {
    Workspace,
    Grant,
    Directory,
    Capability,
    Limit,
}

aether_core::named!(ChangeKind {
    Workspace => "workspace",
    Grant => "grant",
    Directory => "directory",
    Capability => "capability",
    Limit => "limit",
});

impl ChangeKind {
    const ALL: [Self; 5] =
        [Self::Workspace, Self::Grant, Self::Directory, Self::Capability, Self::Limit];
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuditActor {
    Mcp,
    Ui,
}

aether_core::named!(AuditActor { Mcp => "mcp", Ui => "ui" });

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuditAction {
    Change,
}

aether_core::named!(AuditAction { Change => "change" });

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuditResult {
    Pending,
    Applied,
    Confirmed,
    Failed,
}

aether_core::named!(AuditResult {
    Pending => "pending",
    Applied => "applied",
    Confirmed => "confirmed",
    Failed => "failed",
});

sql_named!(ChangeKind, AuditActor, AuditAction, AuditResult);

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingChange {
    pub id: String,
    pub kind: ChangeKind,
    pub value: String,
}

impl PendingChange {
    /// Reads `id,change_type,value`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self { id: row.get(0)?, kind: row.get(1)?, value: row.get(2)? })
    }
}

/// An audit record. It holds names only, never a changed value.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AuditRecord {
    pub actor: AuditActor,
    pub action: AuditAction,
    pub result: AuditResult,
}

fn change_key(kind: ChangeKind, id: &str) -> String {
    format!("admin.{}.{id}", kind.as_str())
}

fn change_prefix(kind: ChangeKind) -> String {
    format!("admin.{}.", kind.as_str())
}

/// Marks a grant that the owner revoked, so that the policy file does not
/// restore it at the next start.
fn revoked_key(id: &str) -> String {
    format!("admin.revoked.{id}")
}

/// The admin revision that a pending change was queued at.
fn pending_revision_key(id: &str) -> String {
    format!("admin.pending_revision.{id}")
}

/// The path of the workspace that a workspace change made live last.
const CURRENT_WORKSPACE_KEY: &str = "admin.current.workspace";

/// How many changes were applied live. A pending change queued at another
/// revision is stale.
const REVISION_KEY: &str = "admin.revision";

fn audit(
    connection: &Connection,
    actor: AuditActor,
    result: AuditResult,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, AuditAction::Change, result, now()],
    )?;
    Ok(())
}

impl NodeStore {
    /// The value of the applied change `id` of `kind`.
    pub fn applied_change(
        &self,
        kind: ChangeKind,
        id: &str,
    ) -> Result<Option<String>, StoreError> {
        config::get(&self.connection, &change_key(kind, id))
    }

    /// The values of every applied change of `kind`, by id.
    pub fn applied_changes(&self, kind: ChangeKind) -> Result<Vec<String>, StoreError> {
        config::values(&self.connection, &change_prefix(kind))
    }

    pub fn applied_change_counts(
        &self,
    ) -> Result<BTreeMap<ChangeKind, usize>, StoreError> {
        ChangeKind::ALL
            .into_iter()
            .map(|kind| {
                Ok((kind, config::count(&self.connection, &change_prefix(kind))?))
            })
            .collect()
    }

    /// Removing a change that is not there is not an error.
    pub fn remove_applied_change(
        &self,
        kind: ChangeKind,
        id: &str,
    ) -> Result<(), StoreError> {
        config::delete(&self.connection, &change_key(kind, id))
    }

    /// Keeps `value` as the applied change `id` of `kind`, and returns the
    /// value it replaced.
    pub fn apply_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        value: &str,
    ) -> Result<Option<String>, StoreError> {
        let transaction = self.connection.transaction()?;
        let key = change_key(kind, id);
        let previous = config::get(&transaction, &key)?;
        config::put(&transaction, &key, value)?;
        transaction.commit()?;
        Ok(previous)
    }

    /// Queues the change `id` until the owner confirms it, with the admin
    /// `revision` it was queued at.
    pub fn queue_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        value: &str,
        expires_at: i64,
        revision: u64,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO pending_changes(id,change_type,value,expires_at) VALUES(?1,?2,?3,?4)",
            params![id, kind, value, expires_at],
        )?;
        config::put(&transaction, &pending_revision_key(id), &revision.to_string())?;
        audit(&transaction, AuditActor::Mcp, AuditResult::Pending)?;
        transaction.commit()?;
        Ok(())
    }

    /// The admin revision that the pending change `id` was queued at; none
    /// for a change that an older node queued.
    pub fn pending_revision(&self, id: &str) -> Result<Option<u64>, StoreError> {
        config::get(&self.connection, &pending_revision_key(id))?
            .map(|value| value.parse().map_err(|_| StoreError::InvalidState))
            .transpose()
    }

    pub fn record_change(
        &self,
        actor: AuditActor,
        result: AuditResult,
    ) -> Result<(), StoreError> {
        audit(&self.connection, actor, result)
    }

    /// Records that `actor` applied a change live, which moved the admin
    /// revision to `revision`.
    pub fn record_applied_change(
        &mut self,
        actor: AuditActor,
        revision: u64,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        config::put(&transaction, REVISION_KEY, &revision.to_string())?;
        audit(&transaction, actor, AuditResult::Applied)?;
        transaction.commit()?;
        Ok(())
    }

    /// The admin revision; 0 before the first applied change.
    pub fn admin_revision(&self) -> Result<u64, StoreError> {
        config::get(&self.connection, REVISION_KEY)?
            .map_or(Ok(0), |value| value.parse().map_err(|_| StoreError::InvalidState))
    }

    /// Restores the value that an applied change replaced and records the
    /// failure. `previous` is `None` when the change created the value.
    pub fn revert_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        previous: Option<&str>,
        actor: AuditActor,
    ) -> Result<(), StoreError> {
        let key = change_key(kind, id);
        let transaction = self.connection.transaction()?;
        match previous {
            Some(value) => config::put(&transaction, &key, value)?,
            None => config::delete(&transaction, &key)?,
        }
        audit(&transaction, actor, AuditResult::Failed)?;
        transaction.commit()?;
        Ok(())
    }

    /// Applies the pending change `id` and returns the value it replaced.
    /// An expired change is [`StoreError::InvalidState`].
    pub fn confirm_pending_change(
        &mut self,
        id: &str,
    ) -> Result<Option<String>, StoreError> {
        let transaction = self.connection.transaction()?;
        let (kind, value, expires_at): (ChangeKind, String, i64) = transaction
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
        let key = change_key(kind, id);
        let previous = config::get(&transaction, &key)?;
        config::put(&transaction, &key, &value)?;
        transaction.execute("DELETE FROM pending_changes WHERE id=?1", [id])?;
        config::delete(&transaction, &pending_revision_key(id))?;
        audit(&transaction, AuditActor::Ui, AuditResult::Confirmed)?;
        transaction.commit()?;
        Ok(previous)
    }

    pub fn pending_changes(&self) -> Result<Vec<PendingChange>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id,change_type,value FROM pending_changes WHERE expires_at>?1 ORDER BY id",
        )?;
        Ok(statement
            .query_map([now()], PendingChange::from_row)?
            .collect::<Result<_, _>>()?)
    }

    /// The pending change `id`, expired or not.
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

    /// Removes the applied grant `id` and marks it revoked.
    pub fn revoke_applied_grant(&mut self, id: &str) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        config::put(&transaction, &revoked_key(id), "revoked")?;
        config::delete(&transaction, &change_key(ChangeKind::Grant, id))?;
        transaction.commit()?;
        Ok(())
    }

    pub fn grant_revoked(&self, id: &str) -> Result<bool, StoreError> {
        Ok(config::get(&self.connection, &revoked_key(id))?.is_some())
    }

    pub fn current_workspace(&self) -> Result<Option<String>, StoreError> {
        config::get(&self.connection, CURRENT_WORKSPACE_KEY)
    }

    pub fn set_current_workspace(&self, path: &str) -> Result<(), StoreError> {
        config::put(&self.connection, CURRENT_WORKSPACE_KEY, path)
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>, StoreError> {
        self.audit_records_page(0, usize::MAX)
    }

    pub fn audit_records_page(
        &self,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT actor_type,action,result FROM audit_records ORDER BY id LIMIT ?1 OFFSET ?2",
        )?;
        Ok(statement
            .query_map([sql_count(limit), sql_count(skip)], |row| {
                Ok(AuditRecord {
                    actor: row.get(0)?,
                    action: row.get(1)?,
                    result: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    #[cfg(test)]
    pub(crate) fn insert_invalid_audit_row_for_test(
        &self,
    ) -> Result<usize, StoreError> {
        self.connection
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuditActor, AuditResult, ChangeKind};
    use crate::{NodeStore, StoreError, test_support};

    #[test]
    fn audit_records_are_durable_and_hold_names_only() {
        let (directory, mut store) = test_support::temp_node_store();
        store
            .queue_change(ChangeKind::Grant, "grant", "private-credential", i64::MAX, 0)
            .expect("change must queue");
        store.confirm_pending_change("grant").expect("change must confirm");
        drop(store);
        let store = NodeStore::in_dir(directory.path()).expect("store must reopen");
        assert_eq!(
            store
                .audit_records()
                .expect("audit must load")
                .into_iter()
                .map(|record| (record.actor, record.result))
                .collect::<Vec<_>>(),
            [
                (AuditActor::Mcp, AuditResult::Pending),
                (AuditActor::Ui, AuditResult::Confirmed)
            ]
        );
        store
            .insert_invalid_audit_row_for_test()
            .expect("invalid audit row must insert for the test");
        assert!(matches!(store.audit_records(), Err(StoreError::Sql(_))));
    }

    #[test]
    fn a_confirmed_change_returns_the_value_it_replaced_and_its_revision_goes() {
        let (_directory, mut store) = test_support::temp_node_store();
        assert_eq!(
            store.apply_change(ChangeKind::Limit, "queue_global", "2"),
            Ok(None)
        );
        store
            .queue_change(ChangeKind::Limit, "queue_global", "9", i64::MAX, 4)
            .expect("change must queue");
        assert_eq!(store.pending_revision("queue_global"), Ok(Some(4)));
        assert_eq!(
            store.confirm_pending_change("queue_global"),
            Ok(Some("2".to_owned()))
        );
        assert_eq!(store.pending_revision("queue_global"), Ok(None));
        assert_eq!(
            store.applied_change(ChangeKind::Limit, "queue_global"),
            Ok(Some("9".to_owned()))
        );
        store
            .revert_change(ChangeKind::Limit, "queue_global", Some("2"), AuditActor::Ui)
            .expect("change must revert");
        assert_eq!(
            store.applied_change(ChangeKind::Limit, "queue_global"),
            Ok(Some("2".to_owned()))
        );
    }

    #[test]
    fn counts_and_lists_hold_only_applied_changes_of_their_kind() {
        let (_directory, mut store) = test_support::temp_node_store();
        store.apply_change(ChangeKind::Grant, "b", "grant-b").expect("grant");
        store.apply_change(ChangeKind::Grant, "a", "grant-a").expect("grant");
        store.apply_change(ChangeKind::Workspace, "w", "/work").expect("workspace");
        store.set_current_workspace("/work").expect("workspace");
        store.revoke_applied_grant("b").expect("revocation");
        store.record_applied_change(AuditActor::Ui, 3).expect("revision");
        assert_eq!(
            store.applied_changes(ChangeKind::Grant),
            Ok(vec!["grant-a".to_owned()])
        );
        let counts = store.applied_change_counts().expect("counts");
        assert_eq!(counts.get(&ChangeKind::Grant), Some(&1));
        assert_eq!(counts.get(&ChangeKind::Workspace), Some(&1));
        assert_eq!(counts.get(&ChangeKind::Limit), Some(&0));
        assert_eq!(store.grant_revoked("b"), Ok(true));
        assert_eq!(store.grant_revoked("a"), Ok(false));
        assert_eq!(store.admin_revision(), Ok(3));
        assert_eq!(store.current_workspace(), Ok(Some("/work".to_owned())));
    }
}
