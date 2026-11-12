use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::hub_client::NodeHub;

#[derive(Clone)]
pub struct NodeServeContext {
    pub data_dir: PathBuf,
    pub hub: Arc<NodeHub>,
    pub hub_sign: Arc<Mutex<()>>,
    pub caller: bool,
}
