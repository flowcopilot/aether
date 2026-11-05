use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use crate::{NodeStore, StoreError};

impl NodeStore {
    pub fn put_configuration(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO configuration(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn configuration(&self, key: &str) -> Result<String, StoreError> {
        self.connection
            .query_row("SELECT value FROM configuration WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Removes `key`. Removing a key that is not there is not an error.
    pub fn delete_configuration(&self, key: &str) -> Result<(), StoreError> {
        self.connection.execute("DELETE FROM configuration WHERE key=?1", [key])?;
        Ok(())
    }

    pub fn configuration_values(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, StoreError> {
        let pattern = format!("{prefix}%");
        let mut statement = self.connection.prepare(
            "SELECT value FROM configuration WHERE key LIKE ?1 ORDER BY key",
        )?;
        statement
            .query_map([pattern], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn configuration_key_counts(
        &self,
        prefix: &str,
    ) -> Result<BTreeMap<String, usize>, StoreError> {
        let pattern = format!("{prefix}%");
        let mut statement = self
            .connection
            .prepare("SELECT key FROM configuration WHERE key LIKE ?1")?;
        let mut counts = BTreeMap::new();
        for key in statement.query_map([pattern], |row| row.get::<_, String>(0))? {
            let key = key?;
            if let Some(rest) = key.strip_prefix(prefix)
                && let Some((category, _)) = rest.split_once('.')
            {
                let count: &mut usize = counts.entry(category.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test_support::*;

    #[test]
    fn configuration_values_lists_only_the_matching_prefix_in_key_order() {
        let (store, database) = store();
        store
            .put_configuration("admin.grant.b", "grant-b")
            .expect("configuration must store");
        store
            .put_configuration("admin.grant.a", "grant-a")
            .expect("configuration must store");
        store
            .put_configuration("admin.limit.queue_global", "5")
            .expect("configuration must store");
        assert_eq!(
            store.configuration_values("admin.grant.").expect("values must load"),
            vec!["grant-a".to_owned(), "grant-b".to_owned()]
        );
        assert!(
            store
                .configuration_values("admin.nothing.")
                .expect("values must load")
                .is_empty()
        );
        drop(store);
        fs::remove_file(database).expect("database must remove");
    }
}
