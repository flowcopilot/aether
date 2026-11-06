use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use aether_store::StoreError;

use super::{HubClient, HubError, hub_setting, resolve_url, settings::Hub};
use crate::settings::setting;

#[derive(Clone)]
struct HubState {
    stored: Option<String>,
    client: Option<HubClient>,
}

#[derive(Clone)]
pub struct NodeHub {
    data_dir: PathBuf,
    flag: Arc<Option<String>>,
    state: Arc<Mutex<Option<HubState>>>,
}

impl NodeHub {
    #[must_use]
    pub fn new(data_dir: PathBuf, flag: Option<String>) -> Self {
        Self { data_dir, flag: Arc::new(flag), state: Arc::new(Mutex::new(None)) }
    }

    #[must_use]
    pub fn with_flag(self, flag: Option<String>) -> Self {
        Self { flag: Arc::new(flag), ..self }
    }

    pub fn resolve_url(&self) -> Result<Option<String>, StoreError> {
        let stored = setting::<Hub>(&self.data_dir)?;
        Ok(resolve_url(self.flag.as_deref(), stored))
    }

    pub fn client(&self) -> Result<Option<HubClient>, HubError> {
        let stored =
            setting::<Hub>(&self.data_dir).map_err(|_| HubError::Unexpected)?;
        let cached = self
            .slot()
            .as_ref()
            .filter(|state| state.stored == stored)
            .and_then(|state| state.client.clone());
        if let Some(client) = cached {
            return Ok(Some(client));
        }
        let client = match resolve_url(self.flag.as_deref(), stored.clone()) {
            None => None,
            Some(url) => Some(HubClient::for_data_dir(&url, &self.data_dir)?),
        };
        *self.slot() = Some(HubState { stored, client: client.clone() });
        Ok(client)
    }

    pub fn set_stored_url(&self, url: &str) -> Result<Option<HubClient>, HubError> {
        let stored = hub_setting(url)?;
        super::set_hub_url(&self.data_dir, &stored)
            .map_err(|_| HubError::Unexpected)?;
        *self.slot() = None;
        self.client()
    }

    pub fn prime(&self, client: HubClient) -> Result<(), StoreError> {
        let stored = setting::<Hub>(&self.data_dir)?;
        *self.slot() = Some(HubState { stored, client: Some(client) });
        Ok(())
    }

    fn slot(&self) -> MutexGuard<'_, Option<HubState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for NodeHub {
    fn default() -> Self {
        Self::new(PathBuf::new(), None)
    }
}

pub fn hub_client(
    flag: Option<&str>,
    data_dir: &Path,
) -> Result<Option<HubClient>, HubError> {
    NodeHub::new(data_dir.to_path_buf(), flag.map(str::to_owned)).client()
}

#[cfg(test)]
mod tests {
    use aether_store::NodeStore;

    use super::NodeHub;
    use crate::{
        hub_client::{DEFAULT_HUB_URL, HUB_OFF, settings::Hub},
        settings::store_setting,
    };

    fn broken_store(data_dir: &std::path::Path) {
        std::fs::create_dir_all(data_dir).expect("directory");
        let _ = NodeStore::in_dir(data_dir).expect("store");
        std::fs::write(data_dir.join("node.sqlite"), b"not sqlite")
            .expect("break store");
    }

    #[test]
    fn a_store_read_error_does_not_fall_back_to_the_public_hub() {
        let directory = tempfile::tempdir().expect("directory");
        store_setting::<Hub>(directory.path(), &Some(HUB_OFF.to_owned()))
            .expect("store hub off");
        broken_store(directory.path());
        let hub = NodeHub::new(directory.path().to_path_buf(), None);
        assert!(hub.resolve_url().is_err());
        assert!(hub.client().is_err());
    }

    #[test]
    fn the_flag_beats_the_page_setting() {
        let directory = tempfile::tempdir().expect("directory");
        store_setting::<Hub>(
            directory.path(),
            &Some("https://stored.example".to_owned()),
        )
        .expect("store");
        let hub = NodeHub::new(
            directory.path().to_path_buf(),
            Some("https://flag.example/".to_owned()),
        );
        assert_eq!(
            hub.resolve_url().ok().flatten().as_deref(),
            Some("https://flag.example")
        );
        let client = hub.client().expect("client").expect("url");
        assert_eq!(client.url(), "https://flag.example");
    }

    #[test]
    fn the_default_hub_is_used_when_nothing_else_is_set() {
        let directory = tempfile::tempdir().expect("directory");
        let hub = NodeHub::new(directory.path().to_path_buf(), None);
        assert_eq!(hub.resolve_url().ok().flatten().as_deref(), Some(DEFAULT_HUB_URL));
    }
}
