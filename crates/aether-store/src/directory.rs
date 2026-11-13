//! Signed records that this node keeps: the invitations of its direct
//! contacts and a cache of directory records. Each kind has its own ids, so
//! a directory scope that has the name of the direct scope cannot replace a
//! contact.

use aether_core::ScopeKind;
use rusqlite::{Connection, OptionalExtension, params};

use crate::CachedSql;
use crate::{NodeStore, StoreError, ensure_scope, managed, put_scope};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignedRecord {
    pub scope_id: String,
    pub agent_id: String,
    pub record: Vec<u8>,
    pub expires_at: i64,
}

pub const RECORD_COLUMNS: &str = "scope_id,agent_id,record,expires_at";

impl SignedRecord {
    /// Reads the [`RECORD_COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            scope_id: row.get(0)?,
            agent_id: row.get(1)?,
            record: row.get(2)?,
            expires_at: row.get(3)?,
        })
    }
}

/// Keeps `record` under `id`, in place of a record with the same id.
pub fn upsert_record(
    connection: &Connection,
    id: &str,
    record: &SignedRecord,
) -> Result<(), StoreError> {
    connection.execute_cached(
        &format!(
            "INSERT INTO signed_records(id,{RECORD_COLUMNS}) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at"
        ),
        params![id, record.scope_id, record.agent_id, record.record, record.expires_at],
    )?;
    Ok(())
}

fn contact_id(agent_id: &str) -> String {
    format!("direct:{agent_id}")
}

fn directory_id(scope_id: &str, agent_id: &str) -> String {
    format!("directory:{scope_id}:{agent_id}")
}

/// The `configuration` key of the cursor of `directory`, whose records
/// `trusted_key` signs.
fn cursor_key(directory: &str, trusted_key: &[u8; 32]) -> String {
    let mut value = directory.as_bytes().to_vec();
    value.extend_from_slice(trusted_key);
    format!("directory.cursor.{}", blake3::hash(&value).to_hex())
}

impl NodeStore {
    /// Keeps the invitation `record` of a contact in the direct scope
    /// `record.scope_id`.
    pub fn put_contact_record(&self, record: &SignedRecord) -> Result<(), StoreError> {
        put_scope(&self.connection, &record.scope_id, ScopeKind::Direct)?;
        upsert_record(&self.connection, &contact_id(&record.agent_id), record)
    }

    pub fn contact_record(&self, agent_id: &str) -> Result<SignedRecord, StoreError> {
        self.record(&contact_id(agent_id))
    }

    /// [`StoreError::NotFound`] when there is no contact `agent_id`.
    pub fn remove_contact_record(&self, agent_id: &str) -> Result<(), StoreError> {
        match self.connection.execute_cached(
            "DELETE FROM signed_records WHERE id=?1",
            [contact_id(agent_id)],
        )? {
            0 => Err(StoreError::NotFound),
            _ => Ok(()),
        }
    }

    /// Keeps a copy of a directory `record`. A directory cannot change the
    /// kind of a scope that this node knows: a scope of another kind is
    /// [`StoreError::InvalidState`].
    pub fn cache_directory_record(
        &self,
        record: &SignedRecord,
    ) -> Result<(), StoreError> {
        ensure_scope(&self.connection, &record.scope_id, ScopeKind::Directory)?;
        upsert_record(
            &self.connection,
            &directory_id(&record.scope_id, &record.agent_id),
            record,
        )
    }

    pub fn directory_record(
        &self,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<SignedRecord, StoreError> {
        self.record(&directory_id(scope_id, agent_id))
    }

    /// Marks the directory record of `agent_id` in `scope_id` as expired.
    /// Whether it was not expired yet.
    pub fn mark_directory_record_stale(
        &self,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute_cached(
            "UPDATE signed_records SET expires_at=0 WHERE id=?1 AND expires_at<>0",
            [directory_id(scope_id, agent_id)],
        )? != 0)
    }

    /// The last event of `directory` that this node applied; 0 before the
    /// first.
    pub fn directory_cursor(
        &self,
        directory: &str,
        trusted_key: &[u8; 32],
    ) -> Result<i64, StoreError> {
        managed::cursor(&self.connection, &cursor_key(directory, trusted_key))
    }

    /// Moves the cursor of `directory` forward to `cursor`.
    pub fn advance_directory_cursor(
        &self,
        directory: &str,
        trusted_key: &[u8; 32],
        cursor: i64,
    ) -> Result<(), StoreError> {
        managed::advance_cursor(
            &self.connection,
            &cursor_key(directory, trusted_key),
            cursor,
        )
    }

    /// The records that have not expired at `now`.
    pub fn live_record_count(&self, now: i64) -> Result<usize, StoreError> {
        let count: i64 = self.connection.query_row_cached(
            "SELECT COUNT(*) FROM signed_records WHERE expires_at>?1",
            [now],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| StoreError::InvalidState)
    }

    fn record(&self, id: &str) -> Result<SignedRecord, StoreError> {
        self.connection
            .query_row_cached(
                &format!("SELECT {RECORD_COLUMNS} FROM signed_records WHERE id=?1"),
                [id],
                SignedRecord::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use aether_core::ScopeKind;

    use crate::{NodeStore, SignedRecord, StoreError};

    fn record(scope_id: &str, agent_id: &str, record: &[u8]) -> SignedRecord {
        SignedRecord {
            scope_id: scope_id.to_owned(),
            agent_id: agent_id.to_owned(),
            record: record.to_vec(),
            expires_at: i64::MAX,
        }
    }

    #[test]
    fn removing_a_contact_removes_only_that_contact() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        store.put_contact_record(&record("direct", "ana", b"{}")).expect("ana");
        store.put_contact_record(&record("direct", "bob", b"{}")).expect("bob");
        store.remove_contact_record("ana").expect("record must remove");
        assert_eq!(store.contact_record("ana"), Err(StoreError::NotFound));
        assert!(store.contact_record("bob").is_ok());
        assert_eq!(store.remove_contact_record("ana"), Err(StoreError::NotFound));
    }

    #[test]
    fn a_directory_record_does_not_replace_a_contact_of_the_same_name() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .put_contact_record(&record("direct", "ana", b"contact"))
            .expect("contact");
        store.put_scope("people", ScopeKind::Directory).expect("scope must store");
        store
            .cache_directory_record(&record("people", "ana", b"listed"))
            .expect("cache");
        // A directory may not take over the kind of the direct scope.
        assert_eq!(
            store.cache_directory_record(&record("direct", "ana", b"listed")),
            Err(StoreError::InvalidState)
        );
        assert_eq!(store.contact_record("ana").expect("contact").record, b"contact");
        assert_eq!(
            store.directory_record("people", "ana").expect("listed").record,
            b"listed"
        );
        assert!(store.mark_directory_record_stale("people", "ana").expect("stale"));
        assert_eq!(
            store.directory_record("people", "ana").expect("stale").expires_at,
            0
        );
        assert_eq!(store.contact_record("ana").expect("contact").expires_at, i64::MAX);
    }

    #[test]
    fn a_directory_cursor_only_moves_forward() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        let key = [7; 32];
        assert_eq!(store.directory_cursor("https://directory", &key), Ok(0));
        store.advance_directory_cursor("https://directory", &key, 3).expect("cursor");
        assert_eq!(
            store.advance_directory_cursor("https://directory", &key, 3),
            Err(StoreError::InvalidState)
        );
        assert_eq!(store.directory_cursor("https://directory", &key), Ok(3));
        assert_eq!(store.directory_cursor("https://other", &key), Ok(0));
    }
}
