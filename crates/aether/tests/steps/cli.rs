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

use std::{
    fmt,
    io::BufReader,
    path::{Path, PathBuf},
    process::{ChildStdout, Command, ExitStatus, Output, Stdio},
};

use cucumber::{then, when};
use serde_json::Value;
use tempfile::TempDir;

use crate::common::{ServerProcess, aether_command, read_event};

#[derive(Debug, Default)]
pub struct State {
    /// The temporary directory that holds every file of the scenario.
    root: Option<TempDir>,
    /// The output of the last command that ran to completion.
    output: Option<Output>,
    /// A running `node serve` or `node admin`.
    node: Option<Process>,
    /// A running control service.
    control: Option<Control>,
    /// The state of the node feature files.
    node_files: node::State,
    /// The state of the direct contact and record feature files.
    contacts: contacts::State,
    /// The state of the directory and managed feature files.
    roster: control::State,
}

impl State {
    /// The scenario's temporary directory. The first call creates it.
    fn root(&mut self) -> PathBuf {
        self.root
            .get_or_insert_with(|| tempfile::tempdir().expect("directory must create"))
            .path()
            .to_path_buf()
    }

    const fn output(&self) -> &Output {
        self.output.as_ref().expect("a command must have run")
    }

    fn stdout(&self) -> String {
        String::from_utf8(self.output().stdout.clone()).expect("output must be UTF-8")
    }

    const fn node(&mut self) -> &mut Process {
        self.node.as_mut().expect("a node must run")
    }

    const fn control(&mut self) -> &mut Control {
        self.control.as_mut().expect("a control service must run")
    }
}

/// A long-running `aether` process with its standard output.
pub struct Process {
    server: ServerProcess,
    stdout: Option<BufReader<ChildStdout>>,
}

impl fmt::Debug for Process {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Process").finish_non_exhaustive()
    }
}

impl Process {
    /// Starts `command` with its standard output piped.
    fn spawn(command: &mut Command) -> Self {
        let mut server = ServerProcess::spawn(command.stdout(Stdio::piped()));
        let stdout = Some(server.stdout());
        Self { server, stdout }
    }

    /// Reads the next event the process prints, or with a `name`, the next
    /// event with that name.
    async fn event(&mut self, name: Option<&'static str>) -> Value {
        let mut stdout = self.stdout.take().expect("output must be readable");
        let (stdout, event) = tokio::task::spawn_blocking(move || {
            let event = read_event(&mut stdout, name);
            (stdout, event)
        })
        .await
        .expect("read task must finish");
        self.stdout = Some(stdout);
        event
    }

    /// Stops the process with SIGINT and returns its exit status. The output
    /// stays open until the process exits: a closed pipe would make it panic.
    async fn interrupt(self) -> ExitStatus {
        let Self { server, stdout } = self;
        tokio::task::spawn_blocking(move || {
            let status = server.interrupt();
            drop(stdout);
            status
        })
        .await
        .expect("stop task must finish")
    }
}

/// A running control service and the address it reports.
#[derive(Debug)]
struct Control {
    process: Process,
    url: String,
    config: PathBuf,
    data: PathBuf,
}

impl Control {
    /// Starts the control service of `config` on `port` and reads its URL.
    async fn start(config: PathBuf, data: PathBuf, port: &str) -> Self {
        let mut process = Process::spawn(aether_command().args([
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
        self.data.join("control-cert.der")
    }
}

/// Runs `command` to completion off the scenario task.
async fn run(mut command: Command) -> Output {
    tokio::task::spawn_blocking(move || command.output().expect("aether must start"))
        .await
        .expect("command task must finish")
}

/// Runs `blocking` off the scenario task, for store and file work.
async fn blocking<T: Send + 'static>(
    blocking: impl FnOnce() -> T + Send + 'static,
) -> T {
    tokio::task::spawn_blocking(blocking).await.expect("blocking task must finish")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path must be UTF-8")
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
