//! Steps for `directory.feature` and `managed.feature`: a node that works
//! with a control service over TLS.

use std::{fs, path::PathBuf, process::Command};

use cucumber::{given, then, when};
use ed25519_dalek::SigningKey;
use serde_json::json;

use super::{Control, assert_success, blocking, run, utf8};
use crate::{common::aether_command, steps::World};

#[derive(Debug, Default)]
pub struct State {
    record: Option<PathBuf>,
    agent_key: Option<PathBuf>,
    token: Option<PathBuf>,
}

impl State {
    fn record(&self) -> PathBuf {
        self.record.clone().expect("a record must exist")
    }
}

/// `aether node directory` with the flags that reach the control service.
fn directory(world: &mut World) -> Command {
    let key = world.cli.roster.agent_key.clone().expect("an agent key must exist");
    let control = world.cli.control();
    let mut command = aether_command();
    command
        .args(["node", "directory", "--directory", &control.url])
        .arg("--directory-certificate")
        .arg(control.certificate())
        .arg("--agent-verifying-key")
        .arg(key);
    command
}

/// `aether node managed` with the flags that reach the control service.
fn managed(world: &mut World) -> Command {
    let control = world.cli.control();
    let mut command = aether_command();
    command
        .args(["node", "managed", "--control", &control.url])
        .arg("--control-certificate")
        .arg(control.certificate());
    command
}

/// `aether control` with the configuration of the running service.
fn control_admin(world: &mut World) -> Command {
    let mut command = aether_command();
    command.arg("control").arg("--config").arg(&world.cli.control().config);
    command
}

async fn succeeds(command: Command) -> std::process::Output {
    let output = run(command).await;
    assert_success(&output);
    output
}

#[given("a running control service")]
async fn running_control(world: &mut World) {
    let root = world.cli.root();
    let data = root.join("control");
    let config = root.join("control.toml");
    fs::write(&config, format!("data_dir = \"{}\"\n", data.display()))
        .expect("configuration must write");
    world.cli.control = Some(Control::start(config, data, "0").await);
}

#[given("a signed test record and an agent key")]
async fn test_record(world: &mut World) {
    let root = world.cli.root();
    let mut command = aether_command();
    command.args(["control", "test-record"]);
    let output = succeeds(command).await;
    let record = root.join("record.json");
    fs::write(&record, output.stdout).expect("record must write");
    let key = root.join("agent-key.txt");
    fs::write(
        &key,
        hex::encode(SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes()),
    )
    .expect("key must write");
    world.cli.roster.record = Some(record);
    world.cli.roster.agent_key = Some(key);
}

#[given("the test record is published and renewed in the directory")]
async fn publish_and_renew(world: &mut World) {
    for operation in ["publish", "renew"] {
        let mut command = directory(world);
        command.arg(operation).arg(world.cli.roster.record());
        succeeds(command).await;
    }
}

#[then(expr = "the directory resolves {string}")]
async fn directory_resolves(world: &mut World, agent: String) {
    let mut command = directory(world);
    command.args(["resolve", &agent]);
    let output = succeeds(command).await;
    assert!(String::from_utf8_lossy(&output.stdout).contains(&agent));
}

#[then(expr = "a directory search for {string} finds {string}")]
async fn directory_search_finds(world: &mut World, query: String, agent: String) {
    let mut command = directory(world);
    command.args(["search", &query]);
    let output = succeeds(command).await;
    assert!(String::from_utf8_lossy(&output.stdout).contains(&agent));
}

#[when("the node syncs from the directory")]
async fn sync_directory(world: &mut World) {
    let client = world.cli.root().join("client");
    let mut command = directory(world);
    command.arg("sync").arg("--data-dir").arg(client);
    world.cli.output = Some(succeeds(command).await);
}

#[then(expr = "the sync fetched {int} record(s)")]
fn sync_fetched(world: &mut World, records: u32) {
    let stdout = world.cli.stdout();
    assert!(
        stdout.contains(&format!("\"records\":{records}")),
        "sync printed: {stdout}"
    );
}

#[when("I revoke the test record in the directory")]
async fn revoke_record(world: &mut World) {
    let mut command = directory(world);
    command.arg("revoke").arg(world.cli.roster.record());
    succeeds(command).await;
}

#[when("the control service restarts on the same port")]
async fn restart_control(world: &mut World) {
    let Control { process, url, config, data } =
        world.cli.control.take().expect("a control service must run");
    assert!(process.interrupt().await.success());
    let port =
        url.rsplit(':').next().expect("control URL must include a port").to_owned();
    let restarted = Control::start(config, data, &port).await;
    assert_eq!(restarted.url, url);
    assert_eq!(restarted.port(), port);
    world.cli.control = Some(restarted);
}

#[then("the control service stops cleanly")]
async fn control_stops(world: &mut World) {
    let control = world.cli.control.take().expect("a control service must run");
    assert!(control.process.interrupt().await.success());
}

#[then(expr = "the node knows the record of {string} in {string} as expired")]
async fn record_expired(world: &mut World, agent: String, scope: String) {
    let client = world.cli.root().join("client");
    let expires_at = blocking(move || {
        aether_store::NodeStore::in_dir(&client)
            .expect("client store must open")
            .directory_record(&scope, &agent)
            .expect("record must remain known")
            .expires_at
    })
    .await;
    assert_eq!(expires_at, 0);
}

#[given(expr = "a managed invitation token for the scope {string}")]
async fn managed_token(world: &mut World, scope: String) {
    let mut command = control_admin(world);
    command.args(["invite", "--scope", &scope, "--expires-in", "60"]);
    let output = succeeds(command).await;
    let invitation: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invitation must decode");
    let token = invitation["token"].as_str().expect("token must be present");
    let file = world.cli.root().join("token.txt");
    fs::write(&file, token).expect("token must write");
    world.cli.roster.token = Some(file);
}

#[given(expr = "a signed record for the member {string} in the scope {string}")]
async fn member_record(world: &mut World, agent: String, scope: String) {
    let root = world.cli.root();
    let member_data = root.join("member");
    let mut command = aether_command();
    command.args([
        "node",
        "record",
        "--data-dir",
        utf8(&member_data),
        "--agent-id",
        &agent,
        "--address",
        "127.0.0.1:4242",
        "--scope",
        &scope,
        "--capability",
        "review",
        "--expires-in",
        "60",
    ]);
    let output = succeeds(command).await;
    let record = root.join("member.json");
    fs::write(&record, output.stdout).expect("member record must write");
    world.cli.roster.record = Some(record);
}

#[when("the member redeems the token")]
async fn redeem_token(world: &mut World) {
    let token = world.cli.roster.token.clone().expect("a token must exist");
    let mut command = managed(world);
    command.args(["redeem", "--token-file"]).arg(token).arg(world.cli.roster.record());
    succeeds(command).await;
}

#[when(expr = "the node syncs the {string} roster")]
async fn sync_roster(world: &mut World, scope: String) {
    let client = world.cli.root().join("client");
    let mut command = managed(world);
    command.args(["sync", &scope, "--data-dir"]).arg(client);
    world.cli.output = Some(succeeds(command).await);
}

#[then(expr = "the sync reports {int} change(s)")]
fn sync_reports_changes(world: &mut World, changes: u32) {
    let stdout = world.cli.stdout();
    assert!(
        stdout.contains(&format!("\"changes\":{changes}")),
        "sync printed: {stdout}"
    );
}

#[when(expr = "the control service revokes the member {string} in the scope {string}")]
async fn revoke_member(world: &mut World, agent: String, scope: String) {
    let mut command = control_admin(world);
    command.args(["revoke-member", "--scope", &scope, "--agent-id", &agent]);
    succeeds(command).await;
}

#[then(expr = "the node knows the first member of {string} as revoked")]
async fn member_revoked(world: &mut World, scope: String) {
    let client = world.cli.root().join("client");
    let revoked = blocking(move || {
        aether_store::NodeStore::in_dir(&client)
            .expect("client store must open")
            .managed_members(&scope)
            .expect("members must load")[0]
            .revoked_at
            .is_some()
    })
    .await;
    assert!(revoked);
}

#[when(expr = "I make a test fixture for the {string} scope")]
async fn make_fixture(world: &mut World, scope: String) {
    let root = world.cli.root();
    let mut command = aether_command();
    command.args(["node", "fixture", "--data-dir", utf8(&root), "--scope", &scope]);
    succeeds(command).await;
}

#[then(expr = "the fixture policy is for the managed scope {string}")]
fn fixture_policy(world: &mut World, scope: String) {
    let policy: serde_json::Value = serde_json::from_slice(
        &fs::read(world.cli.root().join("policy.json")).expect("policy must read"),
    )
    .expect("policy must decode");
    assert_eq!(policy["scope"], json!({"id": scope, "kind": "managed"}));
}

#[then(expr = "the fixture authorization claims the scope {string}")]
fn fixture_authorization(world: &mut World, scope: String) {
    let authorization: serde_json::Value = serde_json::from_slice(
        &fs::read(world.cli.root().join("authorization.json"))
            .expect("authorization must read"),
    )
    .expect("authorization must decode");
    assert_eq!(authorization["claim"]["scopeId"], scope);
}
