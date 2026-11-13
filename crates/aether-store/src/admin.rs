//! Admin changes: the changes that wait for the owner's confirmation, the
//! applied changes the node replays at start, and the audit log. The
//! applied changes live in `configuration` under `admin.`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::CachedSql;
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
    /// The policy settings that an MCP client asked to save.
    Policy,
}

aether_core::named!(ChangeKind {
    Workspace => "workspace",
    Grant => "grant",
    Directory => "directory",
    Capability => "capability",
    Limit => "limit",
    Policy => "policy",
});

impl ChangeKind {
    const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Grant,
        Self::Directory,
        Self::Capability,
        Self::Limit,
        Self::Policy,
    ];
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

/// What an applied change replaced, so that [`NodeStore::revert_change`]
/// can put it back.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Replaced {
    /// The value of the change's id; `None` when the change created it.
    pub value: Option<String>,
    /// The current workspace; read only for a workspace change.
    workspace: Option<String>,
    /// The audit record of the change.
    audit: i64,
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

/// Writes an audit record and returns its row id.
fn audit(
    connection: &Connection,
    actor: AuditActor,
    result: AuditResult,
) -> Result<i64, StoreError> {
    connection.execute_cached(
        "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, AuditAction::Change, result, now()],
    )?;
    Ok(connection.last_insert_rowid())
}

/// Keeps `value` as the applied change `id` of `kind`, with the admin
/// `revision` that it moves the node to and its `applied` audit record. A
/// workspace change also becomes the current workspace. The caller commits.
fn keep_change(
    transaction: &Connection,
    kind: ChangeKind,
    id: &str,
    value: &str,
    actor: AuditActor,
    revision: u64,
) -> Result<Replaced, StoreError> {
    let key = change_key(kind, id);
    let previous = config::get(transaction, &key)?;
    config::put(transaction, &key, value)?;
    let workspace = if kind == ChangeKind::Workspace {
        let workspace = config::get(transaction, CURRENT_WORKSPACE_KEY)?;
        config::put(transaction, CURRENT_WORKSPACE_KEY, value)?;
        workspace
    } else {
        None
    };
    config::put(transaction, REVISION_KEY, &revision.to_string())?;
    let audit = audit(transaction, actor, AuditResult::Applied)?;
    Ok(Replaced { value: previous, workspace, audit })
}

/// Removes each expired pending change with its revision.
fn delete_expired(transaction: &Connection) -> Result<(), StoreError> {
    let expired = {
        let mut statement = transaction
            .prepare_cached("SELECT id FROM pending_changes WHERE expires_at<=?1")?;
        statement
            .query_map([now()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for id in expired {
        transaction.execute_cached("DELETE FROM pending_changes WHERE id=?1", [&id])?;
        config::delete(transaction, &pending_revision_key(&id))?;
    }
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

    /// Keeps `value` as the applied change `id` of `kind` in one
    /// transaction with the admin `revision` it moves the node to and its
    /// audit record, and returns what it replaced. The caller applies the
    /// change to the running node after this, so a process that stops in
    /// between has the revision and the record, and its next start makes
    /// the value live.
    pub fn apply_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        value: &str,
        actor: AuditActor,
        revision: u64,
    ) -> Result<Replaced, StoreError> {
        let transaction = self.connection.transaction()?;
        let replaced = keep_change(&transaction, kind, id, value, actor, revision)?;
        transaction.commit()?;
        Ok(replaced)
    }

    /// Queues the change `id` until the owner confirms it, with the admin
    /// `revision` it was queued at. An expired change leaves first, so that
    /// its id is free again.
    pub fn queue_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        value: &str,
        expires_at: i64,
        revision: u64,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        delete_expired(&transaction)?;
        transaction.execute_cached(
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

    /// Writes an audit record and returns its id for [`Self::fail_change`].
    pub fn record_change(
        &self,
        actor: AuditActor,
        result: AuditResult,
    ) -> Result<i64, StoreError> {
        audit(&self.connection, actor, result)
    }

    /// Marks the audit record `audit` as failed.
    pub fn fail_change(&self, audit: i64) -> Result<(), StoreError> {
        self.connection.execute_cached(
            "UPDATE audit_records SET result=?1 WHERE id=?2",
            params![AuditResult::Failed, audit],
        )?;
        Ok(())
    }

    /// The admin revision; 0 before the first applied change.
    pub fn admin_revision(&self) -> Result<u64, StoreError> {
        config::get(&self.connection, REVISION_KEY)?
            .map_or(Ok(0), |value| value.parse().map_err(|_| StoreError::InvalidState))
    }

    /// Restores what the applied change `id` of `kind` replaced and marks
    /// its audit record as failed. The admin revision stays: a pending
    /// change that was queued before the attempt is stale.
    pub fn revert_change(
        &mut self,
        kind: ChangeKind,
        id: &str,
        replaced: &Replaced,
    ) -> Result<(), StoreError> {
        let key = change_key(kind, id);
        let transaction = self.connection.transaction()?;
        match &replaced.value {
            Some(value) => config::put(&transaction, &key, value)?,
            None => config::delete(&transaction, &key)?,
        }
        if kind == ChangeKind::Workspace {
            match &replaced.workspace {
                Some(path) => config::put(&transaction, CURRENT_WORKSPACE_KEY, path)?,
                None => config::delete(&transaction, CURRENT_WORKSPACE_KEY)?,
            }
        }
        transaction.execute_cached(
            "UPDATE audit_records SET result=?1 WHERE id=?2",
            params![AuditResult::Failed, replaced.audit],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Applies the pending change `id` as [`Self::apply_change`] does, in
    /// one transaction with the removal of its row, and returns what it
    /// replaced. An expired change is [`StoreError::InvalidState`], and it
    /// leaves the table.
    pub fn confirm_pending_change(
        &mut self,
        id: &str,
        revision: u64,
    ) -> Result<Replaced, StoreError> {
        let transaction = self.connection.transaction()?;
        let (kind, value, expires_at): (ChangeKind, String, i64) = transaction
            .query_row_cached(
                "SELECT change_type,value,expires_at FROM pending_changes WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if expires_at <= now() {
            delete_expired(&transaction)?;
            transaction.commit()?;
            return Err(StoreError::InvalidState);
        }
        transaction.execute_cached("DELETE FROM pending_changes WHERE id=?1", [id])?;
        config::delete(&transaction, &pending_revision_key(id))?;
        audit(&transaction, AuditActor::Ui, AuditResult::Confirmed)?;
        let replaced =
            keep_change(&transaction, kind, id, &value, AuditActor::Ui, revision)?;
        transaction.commit()?;
        Ok(replaced)
    }

    pub fn pending_changes(&self) -> Result<Vec<PendingChange>, StoreError> {
        let mut statement = self.connection.prepare_cached(
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
            .query_row_cached(
                "SELECT id,change_type,value FROM pending_changes WHERE id=?1",
                [id],
                PendingChange::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Removes the applied grant `id`, marks it revoked, and writes the
    /// audit record of `actor`, in one transaction.
    pub fn revoke_applied_grant(
        &mut self,
        id: &str,
        actor: AuditActor,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        config::put(&transaction, &revoked_key(id), "revoked")?;
        config::delete(&transaction, &change_key(ChangeKind::Grant, id))?;
        audit(&transaction, actor, AuditResult::Applied)?;
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
        let mut statement = self.connection.prepare_cached(
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
            .execute_cached(
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
        store.confirm_pending_change("grant", 1).expect("change must confirm");
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
                (AuditActor::Ui, AuditResult::Confirmed),
                (AuditActor::Ui, AuditResult::Applied)
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
        let first = store
            .apply_change(ChangeKind::Limit, "queue_global", "2", AuditActor::Ui, 1)
            .expect("change must apply");
        assert_eq!(first.value, None);
        store
            .queue_change(ChangeKind::Limit, "queue_global", "9", i64::MAX, 4)
            .expect("change must queue");
        assert_eq!(store.pending_revision("queue_global"), Ok(Some(4)));
        let replaced =
            store.confirm_pending_change("queue_global", 5).expect("must confirm");
        assert_eq!(replaced.value.as_deref(), Some("2"));
        assert_eq!(store.pending_revision("queue_global"), Ok(None));
        assert_eq!(
            store.applied_change(ChangeKind::Limit, "queue_global"),
            Ok(Some("9".to_owned()))
        );
        assert_eq!(store.admin_revision(), Ok(5));
        store
            .revert_change(ChangeKind::Limit, "queue_global", &replaced)
            .expect("change must revert");
        assert_eq!(
            store.applied_change(ChangeKind::Limit, "queue_global"),
            Ok(Some("2".to_owned()))
        );
        assert_eq!(store.admin_revision(), Ok(5));
    }

    #[test]
    fn a_change_keeps_its_revision_and_audit_record_with_its_value() {
        let (_directory, mut store) = test_support::temp_node_store();
        let replaced = store
            .apply_change(ChangeKind::Workspace, "w", "/work", AuditActor::Mcp, 7)
            .expect("change must apply");
        assert_eq!(store.admin_revision(), Ok(7));
        assert_eq!(store.current_workspace(), Ok(Some("/work".to_owned())));
        assert_eq!(results(&store), [AuditResult::Applied]);
        let second = store
            .apply_change(ChangeKind::Workspace, "v", "/other", AuditActor::Mcp, 8)
            .expect("change must apply");
        store.revert_change(ChangeKind::Workspace, "v", &second).expect("revert");
        assert_eq!(store.current_workspace(), Ok(Some("/work".to_owned())));
        assert_eq!(store.applied_change(ChangeKind::Workspace, "v"), Ok(None));
        assert_eq!(results(&store), [AuditResult::Applied, AuditResult::Failed]);
        store.revert_change(ChangeKind::Workspace, "w", &replaced).expect("revert");
        assert_eq!(store.current_workspace(), Ok(None));
    }

    #[test]
    fn an_expired_pending_change_frees_its_id() {
        let (_directory, mut store) = test_support::temp_node_store();
        store
            .queue_change(ChangeKind::Workspace, "w", "/old", 1, 3)
            .expect("change must queue");
        assert_eq!(store.confirm_pending_change("w", 1), Err(StoreError::InvalidState));
        store
            .queue_change(ChangeKind::Workspace, "w", "/new", i64::MAX, 5)
            .expect("the id of an expired change must queue again");
        assert_eq!(store.pending_revision("w"), Ok(Some(5)));
        assert_eq!(
            store.pending_change_ignoring_expiry("w").map(|change| change.value),
            Ok("/new".to_owned())
        );
        store.queue_change(ChangeKind::Workspace, "x", "/x", 1, 5).expect("queue");
        assert_eq!(store.confirm_pending_change("x", 1), Err(StoreError::InvalidState));
        assert_eq!(
            store.pending_change_ignoring_expiry("x"),
            Err(StoreError::NotFound)
        );
        assert_eq!(store.pending_revision("x"), Ok(None));
    }

    fn results(store: &NodeStore) -> Vec<AuditResult> {
        store
            .audit_records()
            .expect("audit must load")
            .into_iter()
            .map(|record| record.result)
            .collect()
    }

    #[test]
    fn counts_and_lists_hold_only_applied_changes_of_their_kind() {
        let (_directory, mut store) = test_support::temp_node_store();
        for (kind, id, value, revision) in [
            (ChangeKind::Grant, "b", "grant-b", 1),
            (ChangeKind::Grant, "a", "grant-a", 2),
            (ChangeKind::Workspace, "w", "/work", 3),
        ] {
            store
                .apply_change(kind, id, value, AuditActor::Ui, revision)
                .expect("change must apply");
        }
        store.revoke_applied_grant("b", AuditActor::Ui).expect("revocation");
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

    #[test]
    fn names_stay_the_same() {
        for (kind, name) in [
            (ChangeKind::Workspace, "workspace"),
            (ChangeKind::Grant, "grant"),
            (ChangeKind::Directory, "directory"),
            (ChangeKind::Capability, "capability"),
            (ChangeKind::Limit, "limit"),
            (ChangeKind::Policy, "policy"),
        ] {
            assert_eq!(kind.as_str(), name);
        }
        assert_eq!(AuditActor::Mcp.as_str(), "mcp");
        assert_eq!(AuditActor::Ui.as_str(), "ui");
        assert_eq!(AuditResult::Pending.as_str(), "pending");
        assert_eq!(AuditResult::Applied.as_str(), "applied");
        assert_eq!(AuditResult::Confirmed.as_str(), "confirmed");
        assert_eq!(AuditResult::Failed.as_str(), "failed");
    }
}
