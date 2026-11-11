//! Helpers that the integration tests share.

pub mod harness;
pub mod signing;
pub mod store;

pub use aether::fixture::PolicyLimits;
pub use harness::{FakeCodex, TestNodeDir, policy_for_grants, read_node_listening};
pub use signing::{
    discovery_signing_key, host_signing_key, proof_issuer_key, review_claim,
    review_grant,
};
pub use store::{
    load_contact, load_directory_record, open_store, store_contact_invitation,
    store_directory_record,
};

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, ExitStatus, Output},
};

use serde_json::Value;
use tempfile::TempDir;

/// The `aether` binary, without the public hub: local integration tests do
/// not depend on public services.
pub fn aether_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aether"));
    command.env("AETHER_HUB_URL", "off");
    command
}

/// Runs `work` on a blocking thread, so it does not stop the other scenarios.
/// A panic in `work` surfaces as that panic.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// Runs `command` to the end and returns its output.
pub async fn run(mut command: Command) -> Output {
    blocking(move || command.output().expect("aether must start")).await
}

pub fn utf8(path: &Path) -> &str {
    path.to_str().expect("path must be UTF-8")
}

/// A temporary directory that is created on first use.
#[derive(Debug, Default)]
pub struct Scratch(Option<TempDir>);

impl Scratch {
    pub fn path(&mut self) -> PathBuf {
        self.0
            .get_or_insert_with(|| tempfile::tempdir().expect("directory must create"))
            .path()
            .to_path_buf()
    }
}

/// A long-running aether process. Dropping it kills the process, so a test
/// that fails half way does not leave a server running.
pub struct ServerProcess(Child);

impl ServerProcess {
    /// Starts `command` without the public relay.
    pub fn spawn(command: &mut Command) -> Self {
        command.env("AETHER_RELAY_URL", "off");
        Self(command.spawn().expect("server process must start"))
    }

    /// Stops the process the way an operator does, with SIGINT, and
    /// returns its exit status.
    pub fn interrupt(mut self) -> ExitStatus {
        let id = self.0.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-INT", &id])
                .status()
                .expect("interrupt command must start")
                .success()
        );
        self.0.wait().expect("server process must exit")
    }

    pub fn stdout(&mut self) -> BufReader<ChildStdout> {
        BufReader::new(self.0.stdout.take().expect("server output must be piped"))
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reads JSON lines until each event in `names` has been seen once, in any
/// order: a node prints events from concurrent tasks. `None` when the
/// process stops first.
pub fn read_events(
    output: &mut impl BufRead,
    names: &[&str],
) -> Option<std::collections::BTreeMap<String, Value>> {
    let mut seen = std::collections::BTreeMap::new();
    while seen.len() < names.len() {
        let mut line = String::new();
        if output.read_line(&mut line).expect("event line must read") == 0 {
            return None;
        }
        let event: Value =
            serde_json::from_str(&line).expect("event line must be JSON");
        if let Some(name) = event["event"].as_str().filter(|name| names.contains(name))
        {
            seen.entry(name.to_owned()).or_insert(event);
        }
    }
    Some(seen)
}

/// Reads one JSON line that the process writes, such as a startup event.
/// With a `name`, skips the lines until the event with that name.
pub fn read_event(output: &mut impl BufRead, name: Option<&str>) -> Value {
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).expect("event line must read"),
            0,
            "the process stopped"
        );
        let event: Value =
            serde_json::from_str(&line).expect("event line must be JSON");
        if name.is_none_or(|name| event["event"] == name) {
            return event;
        }
    }
}
