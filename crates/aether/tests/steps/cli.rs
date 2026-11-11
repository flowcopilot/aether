//! Steps that drive the `aether` binary as an operator or an agent does.
//!
//! Each submodule covers one feature file. They share [`State`]: a temporary
//! root directory, the long-running processes of the scenario, and the output
//! of the last one-shot command.

mod contacts;
mod control;
mod help;
mod mcp;
mod node;

use std::{path::PathBuf, process::Output};

use cucumber::{then, when};

use crate::common::{Scratch, ServerProcess, aether_command, run, utf8};

#[derive(Debug, Default)]
pub struct State {
    root: Scratch,
    output: Option<Output>,
    /// A running `node serve` or `node admin`.
    node: Option<ServerProcess>,
    /// A running control service.
    control: Option<Control>,
    node_files: node::State,
    contacts: contacts::State,
    roster: control::State,
}

impl State {
    fn root(&mut self) -> PathBuf {
        self.root.path()
    }

    const fn output(&self) -> &Output {
        self.output.as_ref().expect("a command must have run")
    }

    fn stdout(&self) -> String {
        String::from_utf8(self.output().stdout.clone()).expect("output must be UTF-8")
    }

    const fn node(&mut self) -> &mut ServerProcess {
        self.node.as_mut().expect("a node must run")
    }

    const fn control(&mut self) -> &mut Control {
        self.control.as_mut().expect("a control service must run")
    }
}

/// A running control service and the address it reports.
#[derive(Debug)]
struct Control {
    process: ServerProcess,
    url: String,
    config: PathBuf,
    data: PathBuf,
}

impl Control {
    /// Starts the control service of `config` on `port` and reads its URL.
    async fn start(config: PathBuf, data: PathBuf, port: &str) -> Self {
        let mut process = ServerProcess::spawn(aether_command().args([
            "control",
            "--config",
            utf8(&config),
            "--port",
            port,
        ]));
        assert_eq!(process.event(None).await["event"], "process_ready");
        let url = process.event(None).await["url"]
            .as_str()
            .expect("control URL must be reported")
            .to_owned();
        Self { process, url, config, data }
    }

    fn port(&self) -> &str {
        self.url.rsplit(':').next().expect("control URL must include a port")
    }

    fn certificate(&self) -> PathBuf {
        aether::data_dir::DataDir::new(&self.data).control_certificate()
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "aether failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[when(expr = "I run `aether {word}`")]
async fn run_command(world: &mut super::World, command: String) {
    let mut aether = aether_command();
    aether.arg(command);
    world.cli.output = Some(run(aether).await);
}

#[when(expr = "I run the node with the configuration file {string}")]
async fn run_with_configuration(world: &mut super::World, file: String) {
    let mut aether = aether_command();
    aether.args(["node", "--config", &file]);
    world.cli.output = Some(run(aether).await);
}

#[then("the command succeeds")]
fn command_succeeds(world: &mut super::World) {
    assert_success(world.cli.output());
}

#[then(expr = "the command fails with exit code {int}")]
fn command_fails(world: &mut super::World, code: i32) {
    let output = world.cli.output();
    assert_eq!(output.status.code(), Some(code), "unexpected result: {output:?}");
}

#[then(expr = "it reports the {string} event")]
fn reports_event(world: &mut super::World, event: String) {
    let stdout = world.cli.stdout();
    assert!(
        stdout.contains(&format!("\"event\":\"{event}\"")),
        "no {event} event in: {stdout}"
    );
}

#[then(expr = "the error is only {string}")]
fn error_is_only(world: &mut super::World, error: String) {
    let stderr = String::from_utf8(world.cli.output().stderr.clone())
        .expect("error must be UTF-8");
    assert_eq!(stderr.trim(), error);
}
