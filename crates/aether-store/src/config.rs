//! The `configuration` table: one value for each key. Each feature keeps its
//! keys under its own prefix: `admin.` ([`crate::admin`]), `managed.cursor.`
//! ([`crate::managed`]), `directory.cursor.` ([`crate::directory`]),
//! `access_groups.` ([`crate::access_groups`]), and in the `aether` crate
//! `node.`, `caller.` and `outbox.`.

use rusqlite::{Connection, OptionalExtension, params};

use crate::CachedSql;
use crate::{NodeStore, StoreError};

pub fn get(connection: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    Ok(connection
        .query_row_cached(
            "SELECT value FROM configuration WHERE key=?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn put(connection: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    connection.execute_cached(
        "INSERT INTO configuration(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Removing a key that is not there is not an error.
pub fn delete(connection: &Connection, key: &str) -> Result<(), StoreError> {
    connection.execute_cached("DELETE FROM configuration WHERE key=?1", [key])?;
    Ok(())
}

/// The values of the keys that start with `prefix`, in key order.
pub fn values(
    connection: &Connection,
    prefix: &str,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT value FROM configuration WHERE substr(key,1,length(?1))=?1 ORDER BY key",
    )?;
    Ok(statement.query_map([prefix], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

/// How many keys start with `prefix`.
pub fn count(connection: &Connection, prefix: &str) -> Result<usize, StoreError> {
    let count: i64 = connection.query_row_cached(
        "SELECT COUNT(*) FROM configuration WHERE substr(key,1,length(?1))=?1",
        [prefix],
        |row| row.get(0),
    )?;
    usize::try_from(count).map_err(|_| StoreError::InvalidState)
}

impl NodeStore {
    pub fn put_configuration(&self, key: &str, value: &str) -> Result<(), StoreError> {
        put(&self.connection, key, value)
    }

    pub fn configuration(&self, key: &str) -> Result<String, StoreError> {
        get(&self.connection, key)?.ok_or(StoreError::NotFound)
    }

    /// Removing a key that is not there is not an error.
    pub fn delete_configuration(&self, key: &str) -> Result<(), StoreError> {
        delete(&self.connection, key)
    }

    pub fn configuration_values(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, StoreError> {
        values(&self.connection, prefix)
    }
}

#[cfg(test)]
mod tests {
    use crate::NodeStore;

    #[test]
    fn configuration_values_lists_only_the_matching_prefix_in_key_order() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        for (key, value) in
            [("caller.card.b", "b"), ("caller.card.a", "a"), ("caller.contact.c", "c")]
        {
            store.put_configuration(key, value).expect("configuration must store");
        }
        assert_eq!(
            store.configuration_values("caller.card.").expect("values must load"),
            ["a", "b"]
        );
        assert_eq!(
            store.configuration_values("CALLER.").expect("values must load"),
            Vec::<String>::new()
        );
    }
}
