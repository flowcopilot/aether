use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::hub_client::{HubAccess, NodeHub};

#[derive(Clone)]
pub struct NodeServeContext {
    pub data_dir: PathBuf,
    pub hub: Arc<NodeHub>,
    pub hub_sign: Arc<Mutex<()>>,
    pub caller: bool,
}

impl NodeServeContext {
    #[must_use]
    pub fn hub_access(&self) -> HubAccess {
        self.hub.access()
    }
}
