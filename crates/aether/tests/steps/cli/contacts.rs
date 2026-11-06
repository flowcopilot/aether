//! Steps for `direct-contacts.feature` and `discovery-records.feature`: the
//! signed invitations and records that one node gives to another.

use std::{fs, path::PathBuf};

use aether::discovery::wire::{WireInvitation, WireRecord};
use cucumber::{given, then, when};

use super::{assert_success, run, utf8};
use crate::{common::aether_command, steps::World};

#[derive(Debug, Default)]
pub struct State {
    invitation: Option<WireInvitation>,
    invitation_file: Option<PathBuf>,
    records: Vec<(serde_json::Value, WireRecord)>,
}

impl State {
    const fn invitation(&self) -> &WireInvitation {
        self.invitation.as_ref().expect("an invitation must exist")
    }
}

/// Writes `invitation` to a file for `node direct import`.
fn write_invitation(world: &mut World, name: &str, invitation: &WireInvitation) {
    let path = world.cli.root().join(name);
    fs::write(&path, serde_json::to_vec(invitation).expect("invitation must encode"))
        .expect("invitation must write");
    world.cli.contacts.invitation_file = Some(path);
}

#[given(expr = "the agent {string} makes a direct invitation")]
async fn direct_invitation(world: &mut World, agent: String) {
    let server_data = world.cli.root().join("server");
    let mut command = aether_command();
    command.args([
        "node",
        "invite",
        "--data-dir",
        utf8(&server_data),
        "--agent-id",
        &agent,
        "--expires-in",
        "60",
    ]);
    let output = run(command).await;
    assert_success(&output);
    let invitation: WireInvitation =
        serde_json::from_slice(&output.stdout).expect("invitation must decode");
    write_invitation(world, "invitation.json", &invitation);
    world.cli.contacts.invitation = Some(invitation);
}

#[given("the invitation is changed to name another endpoint")]
fn forge_invitation(world: &mut World) {
    let mut forged = world.cli.contacts.invitation().clone();
    "other-endpoint".clone_into(&mut forged.endpoint_id);
    write_invitation(world, "forged-invitation.json", &forged);
}

#[when("a node imports the invitation")]
async fn import_invitation(world: &mut World) {
    let client_data = world.cli.root().join("client");
    let file =
        world.cli.contacts.invitation_file.clone().expect("invitation must be written");
    let mut command = aether_command();
    command.args([
        "node",
        "direct",
        "--data-dir",
        utf8(&client_data),
        "import",
        utf8(&file),
    ]);
    world.cli.output = Some(run(command).await);
}

#[then(expr = "the invitation names the agent {string} and the scope {string}")]
fn invitation_names(world: &mut World, agent: String, scope: String) {
    let invitation = world.cli.contacts.invitation();
    assert_eq!(invitation.scope_id, scope);
    assert_eq!(invitation.agent_id, agent);
}

#[then(expr = "the node resolves {string} to the same invitation")]
async fn resolves_to_invitation(world: &mut World, agent: String) {
    let client_data = world.cli.root().join("client");
    let mut command = aether_command();
    command.args([
        "node",
        "direct",
        "--data-dir",
        utf8(&client_data),
        "resolve",
        &agent,
    ]);
    let output = run(command).await;
    assert_success(&output);
    let resolved: WireInvitation =
        serde_json::from_slice(&output.stdout).expect("contact must decode");
    assert_eq!(&resolved, world.cli.contacts.invitation());
}

#[when(expr = "the node signs a discovery record for the agent {string}")]
async fn sign_record(world: &mut World, agent: String) {
    let data_dir = world.cli.root().join("node");
    let mut command = aether_command();
    command.args([
        "node",
        "record",
        "--data-dir",
        utf8(&data_dir),
        "--agent-id",
        &agent,
        "--address",
        "127.0.0.1:4242",
        "--scope",
        "directory",
        "--capability",
        "review",
        "--expires-in",
        "60",
    ]);
    let output = run(command).await;
    assert_success(&output);
    let json = serde_json::from_slice(&output.stdout).expect("record must be JSON");
    let record = serde_json::from_slice(&output.stdout).expect("record must decode");
    world.cli.contacts.records.push((json, record));
}

#[then("the record carries its key and signature as hex")]
fn record_hex(world: &mut World) {
    let (json, record) =
        world.cli.contacts.records.last().expect("a record must exist");
    assert_eq!(json["signingKey"].as_str().map(str::len), Some(64));
    assert_eq!(json["signature"].as_str().map(str::len), Some(128));
    assert_eq!(record.signature.len(), 64);
}

#[then(
    expr = "the record names the agent {string}, the address {string}, the scope {string} and the capability {string}"
)]
fn record_names(
    world: &mut World,
    agent: String,
    address: String,
    scope: String,
    capability: String,
) {
    let (_, record) = world.cli.contacts.records.last().expect("a record must exist");
    assert_eq!(record.agent_id, agent);
    assert_eq!(record.addresses, [address]);
    assert_eq!(record.scope_id, scope);
    assert_eq!(record.capabilities, [capability]);
}

#[then("both records have the same endpoint and signing key")]
fn records_share_key(world: &mut World) {
    let records = &world.cli.contacts.records;
    assert_eq!(records.len(), 2, "two records must exist");
    let (first, second) = (&records[0].1, &records[1].1);
    assert_eq!(first.endpoint_id, second.endpoint_id);
    assert_eq!(first.signing_key, second.signing_key);
}
