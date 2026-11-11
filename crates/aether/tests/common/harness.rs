use std::{
    fs,
    io::BufRead,
    path::{Path, PathBuf},
};

use aether::fixture::{self, PolicyLimits, write_executable};
use serde_json::Value;
use tempfile::TempDir;

use super::read_event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeCodex {
    Standard,
    Slow,
    McpProbe,
}

impl FakeCodex {
    const fn script(self) -> &'static str {
        match self {
            Self::Standard => fixture::fake_codex_script(false, false),
            Self::Slow => fixture::fake_codex_script(true, false),
            Self::McpProbe => fixture::fake_codex_script(false, true),
        }
    }

    /// Writes the fake Codex as the executable `fake-codex` in `dir`.
    pub fn write_in(self, dir: &Path) -> PathBuf {
        let path = dir.join("fake-codex");
        write_executable(&path, self.script()).expect("fake Codex must write");
        path
    }
}

pub struct TestNodeDir {
    pub temp: TempDir,
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
}

impl TestNodeDir {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("directory must create");
        let root = temp.path().to_path_buf();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let data_dir = root.join("node");
        Self { temp, root, workspace, data_dir }
    }

    pub fn write_policy(&self, policy: &Value) -> PathBuf {
        let path = self.root.join("policy.json");
        fs::write(&path, serde_json::to_vec(policy).expect("policy must encode"))
            .expect("policy must write");
        path
    }
}

pub fn policy_for_grants(
    workspace: &Path,
    grants: &[aether_core::Grant],
    trusted_key: [u8; 32],
    limits: PolicyLimits,
) -> Value {
    fixture::policy_json(workspace, grants, trusted_key, limits)
}

pub fn read_node_listening(output: &mut impl BufRead) -> (String, u16) {
    let startup = read_event(output, None);
    assert_eq!(startup["event"], "node_listening");
    let endpoint_id =
        startup["endpoint_id"].as_str().expect("server endpoint must be logged");
    let port = startup["port"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .expect("server must report its bound port");
    (endpoint_id.to_owned(), port)
}
