//! Steps for `mcp.feature`: an agent that talks JSON-RPC to `aether mcp serve`.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use cucumber::{given, then, when};

use super::{Process, utf8};
use crate::{common::aether_command, steps::World};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"aether-test","version":"1.0.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
"#;

const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#;

const ADD_WORKSPACE: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aether.change","arguments":{"change":{"type":"workspace","id":"workspace","path":"project"},"dry_run":false}}}
"#;

/// Starts `mcp serve`, writes `requests`, closes its input and waits for it
/// to stop.
async fn mcp_session(world: &mut World, mut command: Command, requests: String) {
    let output = tokio::task::spawn_blocking(move || {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("mcp server must start");
        let mut stdin = child.stdin.take().expect("mcp standard input must open");
        stdin.write_all(requests.as_bytes()).expect("mcp request must write");
        drop(stdin);
        child.wait_with_output().expect("mcp server must stop")
    })
    .await
    .expect("mcp task must finish");
    world.cli.output = Some(output);
}

#[given("a running owner page for a managed scope")]
async fn running_owner_page(world: &mut World) {
    let data_dir = world.cli.root();
    let mut process = Process::spawn(aether_command().args([
        "node",
        "admin",
        "--managed-scope",
        "--data-dir",
        utf8(&data_dir),
        "--port",
        "0",
    ]));
    assert_eq!(process.event(None).await["event"], "admin_listening");
    world.cli.node = Some(process);
}

#[when("an agent starts an MCP session and lists the tools")]
async fn list_tools(world: &mut World) {
    let mut command = aether_command();
    command.args(["mcp", "serve"]);
    mcp_session(world, command, format!("{INITIALIZE}{LIST_TOOLS}")).await;
}

#[when("an agent asks over the node's admin socket to add a workspace")]
async fn add_workspace(world: &mut World) {
    let socket = world.cli.root().join("admin.sock");
    let mut command = aether_command();
    command.args(["mcp", "serve", "--socket", utf8(&socket)]);
    mcp_session(world, command, format!("{INITIALIZE}{ADD_WORKSPACE}")).await;
}

#[then(expr = "the reply contains {string}")]
fn reply_contains(world: &mut World, text: String) {
    let stdout = world.cli.stdout();
    assert!(stdout.contains(&text), "no {text} in: {stdout}");
}

#[then(expr = "the reply has the field {string}")]
fn reply_has_field(world: &mut World, field: String) {
    let stdout = world.cli.stdout();
    assert!(stdout.contains(&format!("\"{field}\"")), "no field {field} in: {stdout}");
}
