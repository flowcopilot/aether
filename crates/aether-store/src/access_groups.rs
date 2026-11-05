use rusqlite::params;

use crate::{NodeStore, StoreError};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessGroup {
    pub name: String,
    pub members: Vec<AccessGroupMember>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessGroupMember {
    pub agent_id: String,
    pub endpoint_id: String,
}

impl NodeStore {
    pub fn create_access_group(&self, name: &str) -> Result<(), StoreError> {
        self.connection
            .execute("INSERT INTO access_groups(name) VALUES (?1)", [name])?;
        Ok(())
    }

    /// Moves existing direct grants into the default list once. Later
    /// restarts do not restore members that the owner removed.
    pub fn initialize_access_groups(
        &mut self,
        identities: &[(String, String)],
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        let initialized: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM configuration WHERE key='access_groups.initialized')",
            [],
            |row| row.get(0),
        )?;
        if !initialized {
            for (agent, endpoint) in identities {
                transaction.execute(
                    "INSERT OR IGNORE INTO access_group_members(group_name,agent_id,endpoint_id) VALUES ('friends',?1,?2)",
                    params![agent, endpoint],
                )?;
            }
            transaction.execute(
                "INSERT INTO configuration(key,value) VALUES ('access_groups.initialized','1')",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_access_group(&self, name: &str) -> Result<(), StoreError> {
        if self.connection.execute("DELETE FROM access_groups WHERE name=?1", [name])?
            == 0
        {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn add_access_group_member(
        &self,
        group: &str,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<(), StoreError> {
        if !self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_groups WHERE name=?1)",
            [group],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(StoreError::NotFound);
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO access_group_members(group_name,agent_id,endpoint_id) VALUES (?1,?2,?3)",
            params![group, agent_id, endpoint_id],
        )?;
        Ok(())
    }

    pub fn remove_access_group_member(
        &self,
        group: &str,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<(), StoreError> {
        if self.connection.execute(
            "DELETE FROM access_group_members WHERE group_name=?1 AND agent_id=?2 AND endpoint_id=?3",
            params![group, agent_id, endpoint_id],
        )? == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn access_group_allows(
        &self,
        agent_id: &str,
        endpoint_id: &str,
    ) -> Result<bool, StoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_group_members WHERE agent_id=?1 AND endpoint_id=?2)",
            params![agent_id, endpoint_id],
            |row| row.get(0),
        ).map_err(StoreError::from)
    }

    pub fn access_groups(&self) -> Result<Vec<AccessGroup>, StoreError> {
        let mut groups =
            self.connection.prepare("SELECT name FROM access_groups ORDER BY name")?;
        let mut members = self.connection.prepare(
            "SELECT agent_id,endpoint_id FROM access_group_members WHERE group_name=?1 ORDER BY agent_id,endpoint_id",
        )?;
        groups
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|name| {
                let name = name?;
                let entries = members
                    .query_map([&name], |row| {
                        Ok(AccessGroupMember {
                            agent_id: row.get(0)?,
                            endpoint_id: row.get(1)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(AccessGroup { name, members: entries })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn access_groups_persist_membership_and_do_not_restore_removed_members() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("node.sqlite");
        let mut store = NodeStore::open(&path).expect("store must open");
        store
            .initialize_access_groups(&[("bob".to_owned(), "endpoint-a".to_owned())])
            .expect("current access must move into friends");
        assert!(
            store.access_group_allows("bob", "endpoint-a").expect("access must read")
        );
        assert!(
            !store.access_group_allows("bob", "endpoint-b").expect("access must read")
        );
        store.create_access_group("work").expect("group must create");
        store
            .add_access_group_member("work", "bob", "endpoint-a")
            .expect("member must add");
        store
            .remove_access_group_member("friends", "bob", "endpoint-a")
            .expect("member must remove");
        drop(store);
        let mut store = NodeStore::open(&path).expect("store must reopen");
        store
            .initialize_access_groups(&[("bob".to_owned(), "endpoint-a".to_owned())])
            .expect("migration must not restore an old member");
        assert_eq!(
            store
                .access_groups()
                .expect("groups must list")
                .iter()
                .find(|group| group.name == "friends")
                .expect("friends must exist")
                .members
                .len(),
            0
        );
        assert!(
            store
                .access_group_allows("bob", "endpoint-a")
                .expect("work access must read")
        );
        store.delete_access_group("work").expect("group must delete");
        assert!(
            !store.access_group_allows("bob", "endpoint-a").expect("access must read")
        );
    }
}
