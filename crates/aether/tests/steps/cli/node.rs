//! Steps for `node-start.feature` and `policy.feature`: `node serve` under a
//! home directory, and the policy it keeps in its database.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use aether::admin_socket::{
    Request as AdminRequest, Response as AdminResponse, send as send_admin,
};
use cucumber::{given, then, when};
use serde_json::json;

use super::{Process, blocking, run};
use crate::{common::aether_command, steps::World};

const EXPIRED_TASK_ID: &str = "00000000-0000-4000-8000-000000000042";

#[derive(Debug, Default)]
pub struct State {
    home: Option<PathBuf>,
    owner_auth: Option<PathBuf>,
    port: Option<u16>,
    owner_page: Option<String>,
    /// An old `policy.json` and the node data directory that imports it.
    policy_file: Option<PathBuf>,
}

impl State {
    fn home(&self) -> PathBuf {
        self.home.clone().expect("a home directory must exist")
    }
}

/// `aether node serve` under the scenario's home directory.
fn node_serve(world: &World) -> Command {
    let mut command = aether_command();
    command.args(["node", "serve"]).env("HOME", world.cli.node_files.home());
    if let Some(auth) = &world.cli.node_files.owner_auth {
        command.env("CODEX_HOME", auth.parent().expect("login must be in a directory"));
    }
    command
}

/// Starts `command` and reads the event that says the node listens for peers.
async fn start(world: &mut World, mut command: Command) {
    let mut process = Process::spawn(&mut command);
    let startup = process.event(None).await;
    assert_eq!(startup["event"], "node_listening");
    assert!(
        startup["endpoint_id"].as_str().is_some(),
        "server endpoint must be logged"
    );
    let port = startup["port"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .expect("server must report its bound port");
    world.cli.node_files.port = Some(port);
    world.cli.node = Some(process);
}

#[given("a new home directory")]
fn new_home(world: &mut World) {
    let home = world.cli.root().join("home");
    fs::create_dir_all(&home).expect("home must create");
    world.cli.node_files.home = Some(home);
}

#[given("the owner is logged in to Codex")]
fn owner_logged_in(world: &mut World) {
    let codex_home = world.cli.node_files.home().join(".codex");
    fs::create_dir_all(&codex_home).expect("owner Codex home must create");
    let auth = aether::data_dir::codex_login(&codex_home);
    fs::write(&auth, "test credentials").expect("owner login must write");
    world.cli.node_files.owner_auth = Some(auth);
}

#[given("the outbox holds a sent task that expired")]
async fn expired_outbox_task(world: &mut World) {
    let data_dir = world.cli.node_files.home().join(".aether/host");
    blocking(move || {
        fs::create_dir_all(&data_dir).expect("data directory must create");
        let now = aether_core::time::unix_timestamp(SystemTime::now()).expect("time");
        aether_store::NodeStore::in_dir(&data_dir)
            .expect("store must open")
            .insert_sent_task(&aether_store::SentTaskInput {
                task_id: EXPIRED_TASK_ID.to_owned(),
                idempotency_key: "key".to_owned(),
                agent_id: "ana#0001".to_owned(),
                endpoint_id: "endpoint".to_owned(),
                authorization_json: "{}".to_owned(),
                text: "late".to_owned(),
                created_at: now - 100,
                expires_at: now - 1,
            })
            .expect("the task must insert");
    })
    .await;
}

#[when("the node starts")]
async fn node_starts(world: &mut World) {
    let command = node_serve(world);
    start(world, command).await;
}

#[when("the node starts and opens its owner page")]
async fn node_starts_with_owner_page(world: &mut World) {
    node_starts(world).await;
    let admin = world.cli.node().event(None).await;
    assert_eq!(admin["event"], "admin_listening");
    world.cli.node_files.owner_page = Some(
        admin["url"].as_str().expect("owner page URL must be reported").to_owned(),
    );
}

#[when(expr = "the node starts with the invite name {string}")]
async fn node_starts_with_invite_name(world: &mut World, name: String) {
    let mut command = node_serve(world);
    command.args(["--invite-name", &name]);
    start(world, command).await;
}

#[when("the node starts without a hub")]
async fn node_starts_without_hub(world: &mut World) {
    let mut command = node_serve(world);
    command.args(["--hub-url", "off"]);
    start(world, command).await;
}

#[when(expr = "I start the node with the invite name {string}")]
async fn start_with_invite_name(world: &mut World, name: String) {
    let mut command = node_serve(world);
    command.args(["--invite-name", &name]);
    world.cli.output = Some(run(command).await);
}

#[when("the node stops")]
async fn node_stops(world: &mut World) {
    world.cli.node.take().expect("a node must run").interrupt().await;
}

#[when("the node is woken to send its outbox")]
async fn wake_outbox(world: &mut World) {
    let socket = aether::data_dir::DataDir::new(
        world.cli.node_files.home().join(".aether/host"),
    )
    .admin_socket();
    blocking(move || {
        let started = Instant::now();
        while !matches!(
            send_admin(&socket, &AdminRequest::OutboxWake),
            Ok(AdminResponse::Applied)
        ) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the node must take the wake"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await;
}

#[then("it listens for peers")]
fn listens_for_peers(world: &mut World) {
    assert!(world.cli.node_files.port.is_some_and(|port| port != 0));
}

#[then(expr = "the owner page is on {string}")]
fn owner_page_is_on(world: &mut World, prefix: String) {
    let url = world.cli.node_files.owner_page.as_deref().expect("owner page must open");
    assert!(url.starts_with(&prefix), "owner page at {url}");
}

#[then(expr = "the node database is at {string} in the home directory")]
fn node_database_at(world: &mut World, path: String) {
    assert!(world.cli.node_files.home().join(path).exists());
}

#[then("the node uses the owner's Codex login")]
fn node_uses_owner_login(world: &mut World) {
    let home = world.cli.node_files.home();
    assert_eq!(
        fs::read_link(aether::data_dir::codex_login(
            &aether::data_dir::DataDir::new(home.join(".aether/host")).codex_home(),
        ))
        .expect("node must link the owner's login"),
        world.cli.node_files.owner_auth.clone().expect("owner must be logged in")
    );
}

#[then("the sent task is marked as not delivered")]
async fn sent_task_not_delivered(world: &mut World) {
    let data_dir = world.cli.node_files.home().join(".aether/host");
    let state = blocking(move || {
        let started = Instant::now();
        loop {
            let task = aether_store::NodeStore::in_dir(&data_dir)
                .expect("store must open")
                .sent_task(EXPIRED_TASK_ID)
                .expect("read")
                .expect("the task exists");
            if task.state != aether_store::SentState::Waiting
                || started.elapsed() >= Duration::from_secs(10)
            {
                break task.state;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await;
    assert_eq!(state, aether_store::SentState::NotDelivered);
}

#[then(expr = "the task workspace {string} exists in the home directory")]
fn task_workspace_exists(world: &mut World, path: String) {
    assert!(world.cli.node_files.home().join(path).is_dir());
}

#[then(
    expr = "the policy in the node database uses that workspace and allows {int} context tokens"
)]
async fn policy_uses_workspace(world: &mut World, tokens: u32) {
    let home = world.cli.node_files.home();
    let settings = blocking(move || {
        aether::policy::policy_settings(&home.join(".aether/host"))
            .expect("policy must read")
            .expect("policy must exist")
    })
    .await;
    let workspace = world.cli.node_files.home().join(".aether/tasks");
    assert_eq!(
        settings.workspace,
        workspace.canonicalize().expect("workspace must resolve")
    );
    assert_eq!(settings.max_context_tokens, tokens);
}

#[then(expr = "there is no policy file {string} in the home directory")]
fn no_policy_file(world: &mut World, path: String) {
    assert!(!world.cli.node_files.home().join(path).exists());
}

#[given(expr = "a policy file that allows {int} seconds for each task")]
fn policy_file(world: &mut World, seconds: u32) {
    let root = world.cli.root();
    let workspace = root.join("tasks");
    fs::create_dir_all(&workspace).expect("workspace must create");
    let policy = root.join("policy.json");
    fs::write(
        &policy,
        serde_json::to_vec(&json!({
            "scope": {"id": "direct", "kind": "direct"},
            "workspace": workspace,
            "maxContextTokens": 20000,
            "maxSeconds": seconds,
            "queueGlobal": 1,
            "queuePerPeer": 1
        }))
        .expect("policy must encode"),
    )
    .expect("policy must write");
    world.cli.node_files.policy_file = Some(policy);
}

fn policy_paths(world: &mut World) -> (PathBuf, PathBuf) {
    let policy =
        world.cli.node_files.policy_file.clone().expect("a policy file must exist");
    (policy, world.cli.root().join("node"))
}

#[when(regex = r"^the node loads its policy(?: again)?$")]
async fn node_loads_policy(world: &mut World) {
    let (policy, data_dir) = policy_paths(world);
    blocking(move || {
        aether::node_builder::load_or_create_service(
            &policy,
            &data_dir,
            "codex".into(),
            None,
        )
        .expect("the node must load its policy");
    })
    .await;
}

#[when(expr = "the owner changes the time limit to {int} seconds in the node database")]
async fn owner_changes_time_limit(world: &mut World, seconds: u64) {
    let (_, data_dir) = policy_paths(world);
    blocking(move || {
        let mut settings = aether::policy::policy_settings(&data_dir)
            .expect("policy must read")
            .expect("policy must exist");
        settings.max_seconds = seconds;
        aether::policy::save_policy_settings(&data_dir, &settings)
            .expect("policy must save");
    })
    .await;
}

#[when("the policy file is replaced with text that is not a policy")]
fn policy_file_replaced(world: &mut World) {
    let (policy, _) = policy_paths(world);
    fs::write(&policy, "invalid old file").expect("old policy must change");
}

#[then(expr = "the policy allows {int} seconds for each task")]
async fn policy_allows_seconds(world: &mut World, seconds: u64) {
    let (_, data_dir) = policy_paths(world);
    let settings = blocking(move || {
        aether::policy::policy_settings(&data_dir)
            .expect("policy must read")
            .expect("policy must exist")
    })
    .await;
    assert_eq!(settings.max_seconds, seconds);
}
