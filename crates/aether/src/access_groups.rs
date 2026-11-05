use std::path::Path;

use aether_store::{AccessGroup, NodeStore, StoreError};

#[derive(Debug, Clone, Copy)]
pub enum Action {
    Create,
    Add,
    Remove,
    Delete,
}

fn store(data_dir: &Path) -> Result<NodeStore, &'static str> {
    if !data_dir.join(aether_store::NODE_DATABASE).is_file() {
        return Err("the node store is unavailable");
    }
    NodeStore::in_dir(data_dir).map_err(|_| "the node store is unavailable")
}

/// The group of the grants that the owner gave after an access request.
pub const REQUESTS_GROUP: &str = "requests";

pub fn list(data_dir: &Path) -> Result<Vec<AccessGroup>, &'static str> {
    store(data_dir)?.access_groups().map_err(|_| "the groups are unavailable")
}

/// Whether `group` can be a group name: 1 to 64 letters, digits, `.`, `_`
/// or `-`. Unlike an agent name, it has no hub number.
#[must_use]
pub fn valid_group_name(group: &str) -> bool {
    (1..=64).contains(&group.len())
        && group
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub fn change(
    data_dir: &Path,
    action: Action,
    group: &str,
    agent: Option<&str>,
    endpoint_id: Option<&str>,
    peers: &[(String, String)],
) -> Result<(), &'static str> {
    if !valid_group_name(group) {
        return Err(
            "use 1 to 64 letters, digits, dots, dashes or underscores for a group name",
        );
    }
    let store = store(data_dir)?;
    match action {
        Action::Create => store
            .create_access_group(group)
            .map_err(|_| "the group already exists or cannot be created"),
        Action::Delete => {
            if group == "friends" {
                return Err("friends is the default group and cannot be deleted");
            }
            if group == REQUESTS_GROUP {
                return Err(
                    "requests holds the people who asked for access and cannot be deleted",
                );
            }
            store
                .delete_access_group(group)
                .map_err(|_| "the group does not exist or cannot be deleted")
        }
        Action::Add | Action::Remove => {
            if agent.is_some_and(|agent| !crate::direct::valid_name(agent)) {
                return Err("select a valid person");
            }
            if agent.is_none() && endpoint_id.is_none() {
                return Err("select a person");
            }
            let identities = if matches!(action, Action::Add) {
                peers
                    .iter()
                    .filter(|(name, endpoint)| {
                        agent.is_none_or(|agent| name == agent)
                            && endpoint_id.is_none_or(|id| endpoint == id)
                    })
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
            } else {
                store
                    .access_groups()
                    .map_err(|_| "the groups are unavailable")?
                    .into_iter()
                    .find(|entry| entry.name == group)
                    .ok_or("the group does not exist")?
                    .members
                    .into_iter()
                    .filter(|member| {
                        agent.is_none_or(|agent| member.agent_id == agent)
                            && endpoint_id.is_none_or(|id| member.endpoint_id == id)
                    })
                    .map(|member| (member.agent_id, member.endpoint_id))
                    .collect::<std::collections::BTreeSet<_>>()
            };
            if identities.len() != 1 {
                return Err("select one person and endpoint ID");
            }
            let (agent, endpoint) =
                identities.into_iter().next().ok_or("select one person")?;
            let result = match action {
                Action::Add => store.add_access_group_member(group, &agent, &endpoint),
                Action::Remove => {
                    store.remove_access_group_member(group, &agent, &endpoint)
                }
                Action::Create | Action::Delete => {
                    return Err("this group action is invalid");
                }
            };
            result.map_err(|error| match error {
                StoreError::NotFound => "the group or person is not in this list",
                _ => "the group cannot be changed",
            })
        }
    }
}

/// Why [`join_and_apply`] failed.
#[derive(Debug)]
pub enum JoinError<E> {
    /// The groups could not be read or changed.
    Store(StoreError),
    /// The grant did not apply.
    Apply(E),
}

/// Puts `agent` at `endpoint_id` in `group`, then runs `apply`, which applies
/// the person's new grant to the node. The person joins first: the node
/// admits a direct task only from a member of a group, and it puts a new
/// person who is in no group into friends. When `apply` fails, a person who
/// was not in `group` before leaves it again.
pub fn join_and_apply<T, E>(
    data_dir: &Path,
    group: &str,
    agent: &str,
    endpoint_id: &str,
    apply: impl FnOnce() -> Result<T, E>,
) -> Result<T, JoinError<E>> {
    let store = NodeStore::in_dir(data_dir).map_err(JoinError::Store)?;
    let was_member = store
        .access_groups()
        .map_err(JoinError::Store)?
        .into_iter()
        .find(|entry| entry.name == group)
        .is_some_and(|entry| {
            entry.members.iter().any(|member| {
                member.agent_id == agent && member.endpoint_id == endpoint_id
            })
        });
    store
        .add_access_group_member(group, agent, endpoint_id)
        .map_err(JoinError::Store)?;
    apply().map_err(|error| {
        if !was_member {
            let _ = store.remove_access_group_member(group, agent, endpoint_id);
        }
        JoinError::Apply(error)
    })
}

#[cfg(test)]
mod tests {
    use super::{Action, change, list, valid_group_name};

    #[test]
    fn the_requests_group_cannot_be_deleted() {
        let directory = tempfile::tempdir().expect("directory must create");
        drop(
            aether_store::NodeStore::in_dir(directory.path()).expect("store must open"),
        );
        for group in ["friends", super::REQUESTS_GROUP] {
            assert!(
                change(directory.path(), Action::Delete, group, None, None, &[])
                    .is_err(),
                "{group}"
            );
        }
        let groups: Vec<String> = list(directory.path())
            .expect("groups must list")
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(groups, ["friends", "requests"]);
    }

    #[test]
    fn a_group_name_cannot_hold_a_hub_number() {
        let directory = tempfile::tempdir().expect("directory must create");
        drop(
            aether_store::NodeStore::in_dir(directory.path()).expect("store must open"),
        );
        assert!(valid_group_name("work"));
        assert!(!valid_group_name("work#0001"));
        assert!(
            change(directory.path(), Action::Create, "work#0001", None, None, &[])
                .is_err()
        );
        // A member keeps the full name that the hub gave.
        let peers = vec![("bob#0042".to_owned(), "endpoint-bob".to_owned())];
        change(
            directory.path(),
            Action::Add,
            "friends",
            Some("bob#0042"),
            None,
            &peers,
        )
        .expect("a member with a hub number must add");
    }

    #[test]
    fn named_group_changes_incoming_access() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store =
            aether_store::NodeStore::in_dir(directory.path()).expect("store must open");
        drop(store);
        let peers = vec![("bob".to_owned(), "endpoint-bob".to_owned())];
        change(directory.path(), Action::Create, "work", None, None, &peers)
            .expect("group must create");
        change(directory.path(), Action::Add, "work", Some("bob"), None, &peers)
            .expect("member must add");
        assert_eq!(
            list(directory.path())
                .expect("groups must list")
                .iter()
                .find(|group| group.name == "work")
                .expect("work must exist")
                .members
                .len(),
            1
        );
        change(directory.path(), Action::Remove, "work", Some("bob"), None, &[])
            .expect("member must remove without an active grant");
        change(directory.path(), Action::Delete, "work", None, None, &peers)
            .expect("group must delete");
        assert!(
            list(directory.path())
                .expect("groups must list")
                .iter()
                .all(|group| group.name != "work")
        );
    }

    #[test]
    fn endpoint_id_selects_one_of_two_people_with_the_same_name() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store =
            aether_store::NodeStore::in_dir(directory.path()).expect("store must open");
        drop(store);
        let peers = vec![
            ("bob".to_owned(), "endpoint-a".to_owned()),
            ("bob".to_owned(), "endpoint-b".to_owned()),
        ];
        change(directory.path(), Action::Create, "work", None, None, &peers)
            .expect("group must create");
        assert!(
            change(directory.path(), Action::Add, "work", Some("bob"), None, &peers)
                .is_err()
        );
        change(
            directory.path(),
            Action::Add,
            "work",
            Some("bob"),
            Some("endpoint-b"),
            &peers,
        )
        .expect("one endpoint must add");
        let members = &list(directory.path())
            .expect("groups must list")
            .into_iter()
            .find(|group| group.name == "work")
            .expect("group must exist")
            .members;
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].endpoint_id, "endpoint-b");
    }
}
