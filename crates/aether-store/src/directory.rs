use rusqlite::{OptionalExtension, params};

use crate::{NodeStore, StoreError};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignedRecord {
    pub id: String,
    pub scope_id: String,
    pub agent_id: String,
    pub record: Vec<u8>,
    pub expires_at: i64,
}

impl SignedRecord {
    /// Reads `id,scope_id,agent_id,record,expires_at`.
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            scope_id: row.get(1)?,
            agent_id: row.get(2)?,
            record: row.get(3)?,
            expires_at: row.get(4)?,
        })
    }
}

impl NodeStore {
    pub fn put_signed_record(
        &self,
        id: &str,
        scope_id: &str,
        agent_id: &str,
        record: &[u8],
        expires_at: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO signed_records(id,scope_id,agent_id,record,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET scope_id=excluded.scope_id,agent_id=excluded.agent_id,record=excluded.record,expires_at=excluded.expires_at",
            params![id, scope_id, agent_id, record, expires_at],
        )?;
        Ok(())
    }

    pub fn signed_record(&self, id: &str) -> Result<SignedRecord, StoreError> {
        self.connection
            .query_row(
                "SELECT id,scope_id,agent_id,record,expires_at FROM signed_records WHERE id=?1",
                [id],
                SignedRecord::from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub fn mark_signed_record_stale(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE signed_records SET expires_at=0 WHERE id=?1 AND expires_at<>0",
            [id],
        )? != 0)
    }

    /// Deletes the signed record `id`; [`StoreError::NotFound`] when there is
    /// none.
    pub fn remove_signed_record(&self, id: &str) -> Result<(), StoreError> {
        match self.connection.execute("DELETE FROM signed_records WHERE id=?1", [id])? {
            0 => Err(StoreError::NotFound),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::*;

    #[test]
    fn remove_signed_record_deletes_one_record_and_reports_a_missing_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::open(directory.path().join("node.sqlite"))
            .expect("store must open");
        store.put_scope("direct", "direct").expect("scope must store");
        store
            .put_signed_record("direct:ana", "direct", "ana", b"{}", i64::MAX)
            .expect("record must store");
        store
            .put_signed_record("direct:bob", "direct", "bob", b"{}", i64::MAX)
            .expect("record must store");
        store.remove_signed_record("direct:ana").expect("record must remove");
        assert_eq!(store.signed_record("direct:ana"), Err(StoreError::NotFound));
        assert!(store.signed_record("direct:bob").is_ok());
        assert_eq!(store.remove_signed_record("direct:ana"), Err(StoreError::NotFound));
    }
}
