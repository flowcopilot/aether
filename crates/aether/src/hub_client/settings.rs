//! The node's hub settings: the hub it uses, its name and kind there.

use std::path::Path;

use aether_store::StoreError;

use crate::settings::{Setting, setting, store_setting};

/// The hub last set from the admin page: a URL, or `off`. `None` returns
/// to the default hub.
pub struct Hub;

impl Setting for Hub {
    const KEY: &'static str = "node.hub_url";
    type Value = Option<String>;

    fn absent() -> Result<Self::Value, StoreError> {
        Ok(None)
    }

    fn decode(stored: String) -> Result<Self::Value, StoreError> {
        Ok(Some(stored))
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
        Ok(value.clone())
    }
}

/// This node's name on its hub, as the hub last gave it, if it passes
/// [`crate::hub_client::valid_full_name`]. `None` forgets it, for example when the
/// node moves to another hub.
pub struct HubName;

impl Setting for HubName {
    const KEY: &'static str = "node.hub_name";
    type Value = Option<String>;

    fn absent() -> Result<Self::Value, StoreError> {
        Ok(None)
    }

    fn decode(stored: String) -> Result<Self::Value, StoreError> {
        Ok(Some(stored).filter(|name| crate::hub_client::valid_full_name(name)))
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
        Ok(value.clone())
    }
}

/// The type of this node's agent, once it has one. Keep it with
/// [`store_agent_kind`], which does not change it.
pub struct AgentKind;

impl Setting for AgentKind {
    const KEY: &'static str = "node.agent_kind";
    type Value = Option<super::AgentKind>;

    fn absent() -> Result<Self::Value, StoreError> {
        Ok(None)
    }

    fn decode(stored: String) -> Result<Self::Value, StoreError> {
        Ok(super::AgentKind::parse(&stored))
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
        Ok(value.map(|kind| kind.as_str().to_owned()))
    }
}

/// This node's name on its hub, or `None` when it has none or the store
/// cannot be read.
#[must_use]
pub fn hub_name(data_dir: &Path) -> Option<String> {
    setting::<HubName>(data_dir).ok().flatten()
}

/// The type of this node's agent, or `None` when it has none yet or the
/// store cannot be read.
#[must_use]
pub fn agent_kind(data_dir: &Path) -> Option<super::AgentKind> {
    setting::<AgentKind>(data_dir).ok().flatten()
}

/// Keeps the type of this node's agent. The type does not change: another
/// type than the stored one is [`StoreError::InvalidState`].
///
/// # Errors
///
/// [`StoreError::InvalidState`] for another type, or the error of the
/// store.
pub fn store_agent_kind(
    data_dir: &Path,
    kind: super::AgentKind,
) -> Result<(), StoreError> {
    match setting::<AgentKind>(data_dir)? {
        Some(stored) if stored == kind => Ok(()),
        Some(_) => Err(StoreError::InvalidState),
        None => store_setting::<AgentKind>(data_dir, &Some(kind)),
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, store_agent_kind};
    use crate::settings::setting;

    #[test]
    fn the_agent_kind_does_not_change_once_stored() {
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(setting::<AgentKind>(directory.path()).ok().flatten(), None);
        store_agent_kind(directory.path(), super::super::AgentKind::Personal)
            .expect("kind must store");
        store_agent_kind(directory.path(), super::super::AgentKind::Personal)
            .expect("the same kind is kept");
        assert!(
            store_agent_kind(directory.path(), super::super::AgentKind::Service)
                .is_err()
        );
        assert_eq!(
            setting::<AgentKind>(directory.path()).ok().flatten(),
            Some(super::super::AgentKind::Personal)
        );
    }
}
