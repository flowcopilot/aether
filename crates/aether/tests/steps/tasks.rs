//! Signed tasks sent to a running `node serve`, and what the node does with them.
//!
//! A scenario sets up a host node (its agent, its policy, a directory
//! listing), one or more callers with signed authorizations, and then starts
//! the host as a real `aether node serve` process. The callers use the real
//! `aether node request` command, except where a scenario drives the outbox
//! or a raw transport frame.

use std::{
    fmt, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Output, Stdio},
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

use aether::{
    admin::ChangeRequest,
    admin_socket::{
        Request as AdminRequest, Response as AdminResponse, send as send_admin,
    },
    direct::InvitationBundle,
    discovery::wire::WireRecord,
    transport::{
        A2aAccessMode, A2aAuthorization, A2aRequest, Frame, FrameKind, bind_node,
        request_once, text_request_payload,
    },
};
use aether_core::{Claim, EndpointId};
use aether_store::{NodeStore, SentState, SentTask, TaskState};
use cucumber::{given, then, when};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{
    FakeCodex, PolicyLimits, ServerProcess, aether_command, host_signing_key,
    policy_for_grants, read_event, read_node_listening, review_claim, review_grant,
};

type World = super::World;

const WIRE_TASK_ID: &str = "00000000-0000-4000-8000-000000000002";
const WIRE_REQUEST_ID: &str = "00000000-0000-4000-8000-000000000003";
const WIRE_NONCE: &str = "00000000-0000-4000-8000-000000000004";
/// The task id of a task that a scenario follows through its states.
const FOLLOWED_TASK_ID: &str = "00000000-0000-4000-8000-0000000000c1";
/// The agent id under which a caller imports the host's invitation.
const HOST_AGENT_ID: &str = "server";
/// The roster cursor of the managed scope.
const ROSTER_CONTROL: &str = "https://control.test";

/// The agent that answers the tasks of the host node.
#[derive(Debug, Default, Clone, Copy)]
enum Agent {
    /// Codex that answers at once.
    #[default]
    Codex,
    /// Codex that takes a second to answer.
    SlowCodex,
    /// Codex that answers with the MCP servers it sees.
    McpProbe,
    /// An agent that connects to the agent socket of the node.
    Connected,
}

/// How a caller names the host in `node request`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Contact {
    /// By endpoint id and address.
    #[default]
    Endpoint,
    /// By the agent id of an imported invitation, at the host's address.
    Invited,
    /// By the agent id of an imported one-line invite that also stored the
    /// authorization.
    StoredInvite,
    /// By the agent id in the directory.
    Directory,
}

/// One caller node: its data directory, endpoint and signed authorizations.
#[derive(Debug)]
struct Caller {
    data: PathBuf,
    endpoint: EndpointId,
    /// The scope, the signed claim and the authorization file, per scope.
    authorizations: Vec<(String, Claim, PathBuf)>,
    contact: Contact,
}

impl Caller {
    fn authorization(&self, scope: &str) -> &(String, Claim, PathBuf) {
        self.authorizations
            .iter()
            .find(|(id, ..)| id == scope)
            .expect("the caller must have an authorization for the scope")
    }

    fn first_scope(&self) -> &str {
        &self.authorizations.first().expect("the caller must have a scope").0
    }
}

/// The `node serve` process of the host.
struct Host {
    process: ServerProcess,
    /// Kept open so the node can keep writing its events.
    _stdout: BufReader<ChildStdout>,
    id: String,
    port: u16,
}

impl Host {
    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// How the host lists itself in the directory.
#[derive(Debug)]
struct Listing {
    agent_id: String,
    ttl: u64,
}

/// A directory (control) service. The process is gone once it stopped.
struct Directory {
    process: Option<(ServerProcess, BufReader<ChildStdout>)>,
    url: String,
    certificate: PathBuf,
}

/// One `node request` the scenario sent, so it can send it again.
#[derive(Debug, Clone)]
struct Request {
    caller: usize,
    scope: String,
    text: String,
    task_id: Option<String>,
    idempotency_key: Option<String>,
}

/// Requests that run in the background. Dropping them kills them, so a
/// failed scenario leaves no process behind.
#[derive(Debug, Default)]
struct Running(Vec<Child>);

impl Drop for Running {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The outbox of the first caller.
struct Outbox {
    tasks: Vec<SentTask>,
    states: Vec<SentState>,
    endpoint: Option<iroh::Endpoint>,
}

#[derive(Default)]
pub struct State {
    root: Option<TempDir>,
    agent: Agent,
    max_context_tokens: Option<u64>,
    owner_codex_home: Option<PathBuf>,
    callers: Vec<Caller>,
    /// The next claim or grant nonce.
    nonce: u8,
    listing: Option<Listing>,
    directory: Option<Directory>,
    host: Option<Host>,
    /// The event in which the host published its directory record.
    published: Option<Value>,
    record_key: Option<PathBuf>,
    record: Option<WireRecord>,
    agent_task: Option<JoinHandle<Value>>,
    /// The task that the connected agent got.
    agent_got: Option<Value>,
    request: Option<Request>,
    output: Option<Output>,
    running: Running,
    admin: Option<AdminResponse>,
    rejection: Option<Vec<Frame>>,
    outbox: Option<Outbox>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("agent", &self.agent)
            .field("callers", &self.callers)
            .field("listing", &self.listing)
            .field("host", &self.host.as_ref().map(Host::address))
            .field("request", &self.request)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

/// Runs blocking work off the cucumber task, and passes its panic on.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// Runs a command to completion off the cucumber task.
async fn run(mut command: Command) -> Output {
    blocking(move || command.output().expect("aether must start")).await
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path must be UTF-8")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Polls the server's store instead of sleeping a fixed time: on a loaded
/// runner, a spawned `node request` can take far longer than any fixed sleep
/// to reach the server.
fn wait_for_state(server_data: &Path, task_id: &str, wanted: TaskState) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state =
            NodeStore::in_dir(server_data).and_then(|store| store.state(task_id));
        if state.as_ref().is_ok_and(|state| *state == wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {task_id} to be {wanted:?}, last state {state:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn overload_task_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-0000000000b{index}")
}

fn signed_request(claim: &Claim, endpoint: &EndpointId) -> Frame {
    let mut payload: A2aRequest = serde_json::from_value(text_request_payload(
        WIRE_TASK_ID,
        "key",
        WIRE_NONCE,
        "remote text",
    ))
    .expect("request payload must decode");
    payload.authorization = Some(signed_authorization(claim, endpoint));
    Frame {
        version: 0,
        kind: FrameKind::Request,
        exchange_id: "00000000-0000-4000-8000-000000000008".to_owned(),
        scope_id: "direct".to_owned(),
        task_id: Some(WIRE_TASK_ID.to_owned()),
        payload: serde_json::to_value(payload).expect("request payload must encode"),
    }
}

fn signed_authorization(claim: &Claim, endpoint: &EndpointId) -> A2aAuthorization {
    A2aAuthorization {
        request_id: WIRE_REQUEST_ID.to_owned(),
        caller_agent_id: "caller".to_owned(),
        caller_endpoint_id: endpoint.as_str().to_owned(),
        capability_id: "review".to_owned(),
        workspace_id: "workspace".to_owned(),
        access_mode: A2aAccessMode::ReadOnly,
        claim: claim.into(),
    }
}

/// A courier that reaches the host at one IP address, for a test without a
/// relay.
struct AddressCourier {
    endpoint: iroh::Endpoint,
    address: String,
}

impl aether::outbox::Courier for AddressCourier {
    fn exchange<'a>(
        &'a self,
        bundle: &'a InvitationBundle,
        link: &'a mut aether::transport::Link,
        request: Frame,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Vec<Frame>, aether::transport::TransportError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let remote = aether::transport::ip_endpoint_addr(
                &bundle.invitation.endpoint_id,
                &self.address,
            )?;
            link.exchange(&self.endpoint, remote, request).await
        })
    }
}

fn unix_now() -> i64 {
    aether_core::time::unix_timestamp(SystemTime::now()).expect("the time must read")
}

fn far_future() -> i64 {
    aether_core::time::unix_timestamp(SystemTime::now() + Duration::from_hours(9600))
        .expect("time must be valid")
}

impl State {
    fn root(&mut self) -> PathBuf {
        self.root
            .get_or_insert_with(|| tempfile::tempdir().expect("directory must create"))
            .path()
            .to_path_buf()
    }

    fn server_data(&mut self) -> PathBuf {
        self.root().join("server")
    }

    const fn host(&self) -> &Host {
        self.host.as_ref().expect("the host node must run")
    }

    const fn directory(&self) -> &Directory {
        self.directory.as_ref().expect("a directory service must run")
    }

    const fn output(&self) -> &Output {
        self.output.as_ref().expect("a request must have run")
    }

    const fn next_nonce(&mut self) -> [u8; 32] {
        self.nonce += 1;
        [self.nonce; 32]
    }

    async fn add_caller(&mut self) -> usize {
        let index = self.callers.len();
        let data = self.root().join(format!("client-{index}"));
        let client = bind_node(&data, 0, None).await.expect("client must bind");
        let endpoint = EndpointId::new(client.id().to_string());
        client.close().await;
        self.callers.push(Caller {
            data,
            endpoint,
            authorizations: Vec::new(),
            contact: Contact::Endpoint,
        });
        index
    }

    /// Signs a claim for the caller in `scope` and writes its authorization.
    fn authorize(&mut self, caller: usize, scope: &str) {
        let now = SystemTime::now();
        let nonce = self.next_nonce();
        let root = self.root();
        let caller = &mut self.callers[caller];
        let claim = review_claim(
            &host_signing_key(),
            &format!("{scope}-claim-{}", caller.endpoint.as_str()),
            "issuer",
            "caller",
            &caller.endpoint,
            scope,
            now - Duration::from_secs(1),
            now + Duration::from_secs(60),
            nonce,
        );
        let path = root
            .join(format!("{scope}-{}-authorization.json", caller.endpoint.as_str()));
        fs::write(
            &path,
            serde_json::to_vec(&signed_authorization(&claim, &caller.endpoint))
                .expect("authorization must encode"),
        )
        .expect("authorization must write");
        caller.authorizations.push((scope.to_owned(), claim, path));
    }

    /// The policy of the host: one grant for each caller in each scope.
    fn write_policy(&mut self) -> PathBuf {
        let root = self.root();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace must create");
        let now = SystemTime::now();
        let entries: Vec<(EndpointId, String)> = self
            .callers
            .iter()
            .flat_map(|caller| {
                caller
                    .authorizations
                    .iter()
                    .map(|(scope, ..)| (caller.endpoint.clone(), scope.clone()))
            })
            .collect();
        let mut grants = Vec::new();
        let mut scopes: Vec<String> = Vec::new();
        for (endpoint, scope) in entries {
            let nonce = self.next_nonce();
            grants.push(review_grant(
                &host_signing_key(),
                &format!("{scope}-grant-{}", endpoint.as_str()),
                "issuer",
                "caller",
                &endpoint,
                scope.as_str(),
                now - Duration::from_secs(1),
                now + Duration::from_secs(60),
                nonce,
            ));
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        let mut limits = PolicyLimits::default();
        if let Some(tokens) = self.max_context_tokens {
            limits.max_context_tokens = tokens;
        }
        let mut policy = policy_for_grants(
            &workspace,
            &grants,
            host_signing_key().verifying_key().to_bytes(),
            limits,
        );
        let scope_entries: Vec<Value> =
            scopes.iter().map(|scope| json!({"id": scope, "kind": scope})).collect();
        if let [scope] = scope_entries.as_slice() {
            policy["scope"] = scope.clone();
        } else {
            policy["scopes"] = Value::Array(scope_entries);
        }
        if matches!(self.agent, Agent::Connected) {
            policy["adapter"] = json!("connected");
        }
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).expect("policy must encode"))
            .expect("policy must write");
        path
    }

    async fn start_host(&mut self) {
        let root = self.root();
        let server_data = self.server_data();
        let policy = self.write_policy();
        let codex_kind = match self.agent {
            Agent::Codex => Some(FakeCodex::Standard),
            Agent::SlowCodex => Some(FakeCodex::Slow),
            Agent::McpProbe => Some(FakeCodex::McpProbe),
            Agent::Connected => None,
        };
        let codex_bin = root.join("fake-codex");
        if let Some(kind) = codex_kind {
            aether::fixture::write_executable(&codex_bin, kind.script())
                .expect("fake Codex must write");
        }
        let mut command = aether_command();
        command
            .args([
                "node",
                "serve",
                "--policy",
                utf8(&policy),
                "--data-dir",
                utf8(&server_data),
            ])
            .args(["--port", "0", "--admin-port", "0"])
            .env(
                "AETHER_CODEX_BIN",
                if codex_kind.is_some() {
                    codex_bin
                } else {
                    root.join("no-codex-here")
                },
            )
            .stdout(Stdio::piped());
        if let Some(home) = &self.owner_codex_home {
            command.env("CODEX_HOME", home);
        }
        if let Some(listing) = &self.listing {
            let directory = self.directory();
            command.args([
                "--directory",
                &directory.url,
                "--directory-certificate",
                utf8(&directory.certificate),
                "--agent-id",
                &listing.agent_id,
                "--address",
                "127.0.0.1:0",
                "--scope",
                self.callers.first().expect("a caller must exist").first_scope(),
                "--capability",
                "review",
                "--directory-record-ttl",
                &listing.ttl.to_string(),
            ]);
        }
        let listed = self.listing.is_some();
        let mut process = ServerProcess::spawn(&mut command);
        let mut output = process.stdout();
        let (output, id, port, published) = blocking(move || {
            let (id, port) = read_node_listening(&mut output);
            let published = listed.then(|| {
                let published = read_event(&mut output, None);
                assert_eq!(published["event"], "directory_record_published");
                published
            });
            assert_eq!(read_event(&mut output, None)["event"], "admin_listening");
            (output, id, port, published)
        })
        .await;
        if let Some(published) = &published {
            let signing_key = published["signing_key"]
                .as_str()
                .expect("record signing key must be reported");
            let record_key = root.join("record-key.txt");
            fs::write(&record_key, signing_key).expect("record key must write");
            self.record_key = Some(record_key);
        }
        self.published = published;
        self.host = Some(Host { process, _stdout: output, id, port });
    }

    /// `node invite` on the host: the invitation JSON.
    async fn invitation(&mut self) -> Vec<u8> {
        let mut command = aether_command();
        command.args([
            "node",
            "invite",
            "--data-dir",
            utf8(&self.server_data()),
            "--agent-id",
            HOST_AGENT_ID,
            "--expires-in",
            "60",
        ]);
        let output = run(command).await;
        assert!(output.status.success(), "{output:?}");
        output.stdout
    }

    fn request_command(&self, request: &Request) -> Command {
        let caller = &self.callers[request.caller];
        let (_, _, authorization) = caller.authorization(&request.scope);
        let mut command = aether_command();
        command.args(["node", "request"]);
        if caller.contact == Contact::StoredInvite {
            command.env("AETHER_DATA_DIR", &caller.data);
        } else {
            command.arg("--data-dir").arg(&caller.data);
        }
        match caller.contact {
            Contact::Endpoint => {
                let host = self.host();
                command.args(["--endpoint-id", &host.id, "--address", &host.address()]);
            }
            Contact::Invited | Contact::StoredInvite => {
                command.args([
                    "--agent-id",
                    HOST_AGENT_ID,
                    "--address",
                    &self.host().address(),
                ]);
            }
            Contact::Directory => {
                let directory = self.directory();
                command.args([
                    "--directory",
                    &directory.url,
                    "--directory-certificate",
                    utf8(&directory.certificate),
                    "--agent-id",
                    &self.listing.as_ref().expect("the host must be listed").agent_id,
                    "--agent-verifying-key",
                    utf8(
                        self.record_key.as_ref().expect("the record key must be known"),
                    ),
                ]);
            }
        }
        command.args(["--scope", &request.scope, "--text", &request.text]);
        if caller.contact != Contact::StoredInvite {
            command.arg("--authorization").arg(authorization);
        }
        if let Some(task_id) = &request.task_id {
            command.args(["--task-id", task_id]);
        }
        if let Some(key) = &request.idempotency_key {
            command.args(["--idempotency-key", key]);
        }
        command
    }

    fn request(&self, text: &str) -> Request {
        Request {
            caller: 0,
            scope: self
                .callers
                .first()
                .expect("a caller must exist")
                .first_scope()
                .to_owned(),
            text: text.to_owned(),
            task_id: None,
            idempotency_key: None,
        }
    }

    async fn send(&mut self, request: Request) {
        let command = self.request_command(&request);
        self.request = Some(request);
        self.output = Some(run(command).await);
    }

    /// Starts a request in the background and waits until the host has its
    /// task in the `wanted` state.
    async fn start_in_background(&mut self, request: Request, wanted: TaskState) {
        let task_id = request.task_id.clone().expect("a followed task must have an id");
        let mut command = self.request_command(&request);
        let child =
            command.stdout(Stdio::piped()).spawn().expect("request command must start");
        self.running.0.push(child);
        self.request = Some(request);
        let server_data = self.server_data();
        blocking(move || wait_for_state(&server_data, &task_id, wanted)).await;
    }

    async fn store<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut NodeStore) -> T + Send + 'static,
    ) -> T {
        let server_data = self.server_data();
        blocking(move || {
            fs::create_dir_all(&server_data)
                .expect("server data directory must create");
            let mut store = NodeStore::in_dir(&server_data).expect("store must open");
            work(&mut store)
        })
        .await
    }

    async fn resolve_record(&self) -> WireRecord {
        let mut command = self.directory_command();
        command.args([
            "resolve",
            &self.listing.as_ref().expect("the host must be listed").agent_id,
        ]);
        let output = run(command).await;
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).expect("record must decode")
    }

    fn directory_command(&self) -> Command {
        let directory = self.directory();
        let mut command = aether_command();
        command.args([
            "node",
            "directory",
            "--directory",
            &directory.url,
            "--directory-certificate",
            utf8(&directory.certificate),
            "--agent-verifying-key",
            utf8(self.record_key.as_ref().expect("the record key must be known")),
        ]);
        command
    }

    async fn change_directory(&mut self, url: String) {
        let socket = aether::data_dir::DataDir::new(self.server_data()).admin_socket();
        let response = blocking(move || {
            send_admin(
                &socket,
                &AdminRequest::Change {
                    change: Box::new(ChangeRequest::Directory {
                        id: "directory".to_owned(),
                        url,
                    }),
                    dry_run: false,
                },
            )
            .expect("change must send")
        })
        .await;
        self.admin = Some(response);
    }

    const fn outbox(&mut self) -> &mut Outbox {
        self.outbox.as_mut().expect("the outbox must have tasks")
    }
}

// The host node and its agent.

#[given("a host node whose agent is Codex")]
const fn codex_agent(world: &mut World) {
    world.tasks.agent = Agent::Codex;
}

#[given("a host node whose agent takes a second to answer")]
const fn slow_agent(world: &mut World) {
    world.tasks.agent = Agent::SlowCodex;
}

#[given("a host node whose Codex answers with the MCP servers it sees")]
const fn mcp_probe_agent(world: &mut World) {
    world.tasks.agent = Agent::McpProbe;
}

#[given("a host node whose agent connects to the node")]
const fn connected_agent(world: &mut World) {
    world.tasks.agent = Agent::Connected;
}

/// The agent's MCP servers live in the node's own Codex home.
#[given(expr = "the agent of the node has the MCP server {string}")]
fn node_mcp_server(world: &mut World, name: String) {
    let home = aether::data_dir::DataDir::new(world.tasks.server_data()).codex_home();
    fs::create_dir_all(&home).expect("node Codex home must create");
    fs::write(
        home.join("config.toml"),
        format!("[mcp_servers.{name}]\ncommand = \"/bin/sh\"\n"),
    )
    .expect("agent config must write");
}

#[given(expr = "the owner's own Codex has the MCP server {string}")]
fn owner_mcp_server(world: &mut World, name: String) {
    let home = world.tasks.root().join("owner-codex");
    fs::create_dir_all(&home).expect("owner Codex home must create");
    fs::write(
        home.join("config.toml"),
        format!("[mcp_servers.{name}]\ncommand = \"/bin/sh\"\n"),
    )
    .expect("owner config must write");
    world.tasks.owner_codex_home = Some(home);
}

#[given(expr = "the host node takes tasks of at most {int} context token(s)")]
const fn max_context_tokens(world: &mut World, tokens: u64) {
    world.tasks.max_context_tokens = Some(tokens);
}

#[given("the host node is running")]
async fn host_running(world: &mut World) {
    world.tasks.start_host().await;
}

#[then("the host node stops cleanly")]
async fn host_stops(world: &mut World) {
    let state = &mut world.tasks;
    // Background requests finish on their own once the agent is free; only
    // their end matters here, not their outcome.
    let running = std::mem::take(&mut state.running.0);
    blocking(move || {
        for child in running {
            let _ = child.wait_with_output();
        }
    })
    .await;
    if let Some(endpoint) =
        state.outbox.as_mut().and_then(|outbox| outbox.endpoint.take())
    {
        endpoint.close().await;
    }
    let host = state.host.take().expect("the host node must run");
    let Host { process, _stdout: output, .. } = host;
    let status = blocking(move || process.interrupt()).await;
    drop(output);
    assert!(status.success(), "the host node exited with {status}");
}

// Callers.

#[given(expr = "a caller with a signed authorization for the {string} scope")]
#[given(expr = "the caller also has a signed authorization for the {string} scope")]
async fn caller_with_authorization(world: &mut World, scope: String) {
    let state = &mut world.tasks;
    if state.callers.is_empty() {
        state.add_caller().await;
    }
    state.authorize(0, &scope);
}

#[given(expr = "{int} callers with a signed authorization for the {string} scope")]
async fn callers_with_authorization(world: &mut World, count: usize, scope: String) {
    // Separate caller identities, not one shared across several
    // connections: iroh does not support several independent concurrent
    // connections from a single peer identity.
    for _ in 0..count {
        let caller = world.tasks.add_caller().await;
        world.tasks.authorize(caller, &scope);
    }
}

#[given("the caller imported the host's invitation")]
async fn imported_invitation(world: &mut World) {
    let state = &mut world.tasks;
    let invitation = state.invitation().await;
    let path = state.root().join("invitation.json");
    fs::write(&path, invitation).expect("invitation must write");
    let caller = &mut state.callers[0];
    let mut command = aether_command();
    command.args([
        "node",
        "direct",
        "--data-dir",
        utf8(&caller.data),
        "import",
        utf8(&path),
    ]);
    let output = run(command).await;
    assert!(output.status.success(), "{output:?}");
    caller.contact = Contact::Invited;
}

/// The one-line invite carries the authorization, so a request after its
/// import needs neither `--authorization` nor the invitation file.
#[given("the caller imported the host's one-line invite")]
async fn imported_one_line_invite(world: &mut World) {
    let state = &mut world.tasks;
    let invitation = state.invitation().await;
    let caller = &mut state.callers[0];
    let (_, _, authorization) = caller.authorization("direct");
    let invite = InvitationBundle::v1(
        serde_json::from_slice(&invitation).expect("invitation must decode"),
        serde_json::from_slice(
            &fs::read(authorization).expect("authorization must read"),
        )
        .expect("authorization must decode"),
        None,
    )
    .encode()
    .expect("the invite must encode");
    let mut command = aether_command();
    command
        .args(["node", "direct", "import", &invite])
        .env("AETHER_DATA_DIR", &caller.data);
    let output = run(command).await;
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains("direct_contact_imported"), "{output:?}");
    caller.contact = Contact::StoredInvite;
}

#[given("the caller finds the host through the directory")]
fn finds_through_directory(world: &mut World) {
    world.tasks.callers[0].contact = Contact::Directory;
}

// Requests.

#[given(expr = "the caller sent the task {string}")]
#[when(expr = "the caller sends the task {string}")]
async fn send_task(world: &mut World, text: String) {
    let request = world.tasks.request(&text);
    world.tasks.send(request).await;
}

#[when(expr = "the caller sends the task {string} without an authorization file")]
async fn send_task_with_stored_authorization(world: &mut World, text: String) {
    assert_eq!(world.tasks.callers[0].contact, Contact::StoredInvite);
    send_task(world, text).await;
}

#[when(expr = "the caller sends the task {string} in the {string} scope")]
async fn send_task_in_scope(world: &mut World, text: String, scope: String) {
    let request = Request { scope, ..world.tasks.request(&text) };
    world.tasks.send(request).await;
}

#[given(expr = "the caller sent the task {string} with the idempotency key {string}")]
#[when(expr = "the caller sends the task {string} with the idempotency key {string}")]
async fn send_task_with_key(world: &mut World, text: String, key: String) {
    // A fresh task id is generated for every invocation regardless of
    // `--idempotency-key`, so a reused key for a different text can never
    // be the same idempotent retry.
    let request = Request { idempotency_key: Some(key), ..world.tasks.request(&text) };
    world.tasks.send(request).await;
}

#[given(
    expr = "the caller sent the task {string} with a task id and the idempotency key {string}"
)]
async fn send_task_with_id_and_key(world: &mut World, text: String, key: String) {
    let request = Request {
        task_id: Some(FOLLOWED_TASK_ID.to_owned()),
        idempotency_key: Some(key),
        ..world.tasks.request(&text)
    };
    world.tasks.send(request).await;
}

/// The same task id, idempotency key and text: an idempotent retry that
/// reaches the task the host already admitted.
#[when("the caller retries the task")]
async fn send_same_task(world: &mut World) {
    let request = world.tasks.request.clone().expect("a task must have been sent");
    world.tasks.send(request).await;
}

#[given(expr = "the caller got the answer {string}")]
#[then(expr = "the caller gets the answer {string}")]
fn gets_answer(world: &mut World, answer: String) {
    let output = world.tasks.output();
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(output).contains(&answer), "{output:?}");
}

#[then(expr = "the answer does not mention {string}")]
fn answer_does_not_mention(world: &mut World, text: String) {
    let output = world.tasks.output();
    assert!(!stdout(output).contains(&text), "{output:?}");
}

#[then(expr = "the answer mentions {string}")]
fn answer_mentions(world: &mut World, text: String) {
    let output = world.tasks.output();
    assert!(stdout(output).contains(&text), "{output:?}");
}

/// A request that the node rejects or ends without an answer is not a CLI
/// success. An error frame is a delivered answer, so it prints to stdout.
#[then(expr = "the request fails with {string}")]
fn request_fails(world: &mut World, text: String) {
    let output = world.tasks.output();
    assert!(!output.status.success(), "{output:?}");
    assert!(stdout(output).contains(&text), "{output:?}");
}

#[when("another endpoint sends the caller's signed task")]
async fn other_endpoint_frame(world: &mut World) {
    let state = &mut world.tasks;
    let attacker = state.root().join("attacker");
    let caller = &state.callers[0];
    let (_, claim, _) = caller.authorization("direct");
    let host = state.host();
    let response = request_once(
        &attacker,
        &host.id,
        &host.address(),
        signed_request(claim, &caller.endpoint),
    )
    .await
    .expect("denial response must arrive");
    state.rejection = Some(response);
}

#[then(expr = "the host node answers {string}")]
fn host_answers(world: &mut World, message: String) {
    let response = world.tasks.rejection.as_ref().expect("a frame must have been sent");
    assert_eq!(response[0].kind, FrameKind::Error);
    assert_eq!(response[0].payload["message"], message.as_str());
}

/// The same denial through the CLI process, not the library call.
#[when(
    expr = "another endpoint sends the task {string} with the caller's authorization"
)]
async fn other_endpoint_request(world: &mut World, text: String) {
    let state = &mut world.tasks;
    let attacker = state.root().join("attacker");
    let (_, _, authorization) = state.callers[0].authorization("direct");
    let host = state.host();
    let mut command = aether_command();
    command
        .args(["node", "request", "--data-dir", utf8(&attacker)])
        .args(["--endpoint-id", &host.id, "--address", &host.address()])
        .args([
            "--scope",
            "direct",
            "--text",
            &text,
            "--authorization",
            utf8(authorization),
        ]);
    state.output = Some(run(command).await);
}

// The connected agent.

#[given("the connected agent is ready")]
async fn connected_agent_ready(world: &mut World) {
    let socket =
        aether::data_dir::DataDir::new(world.tasks.server_data()).agent_socket();
    let agent = blocking(move || {
        let mut agent =
            UnixStream::connect(socket).expect("the agent must connect to the node");
        agent
            .write_all(b"{\"type\":\"ready\",\"protocol\":1}\n")
            .expect("ready must send");
        agent
    })
    .await;
    let reader = BufReader::new(agent.try_clone().expect("stream must clone"));
    let mut writer = agent.try_clone().expect("stream must clone");
    // A thread of its own, not the cucumber task: it blocks on the socket.
    world.tasks.agent_task = Some(std::thread::spawn(move || {
        let line =
            reader.lines().next().expect("a task must arrive").expect("task line");
        let task: Value = serde_json::from_str(&line).expect("task is JSON");
        let reply = json!({
            "type": "result",
            "id": task["id"],
            "text": format!("connected agent result: {}", task["text"].as_str().unwrap_or_default()),
        });
        writer.write_all(format!("{reply}\n").as_bytes()).expect("result must send");
        task
    }));
}

/// The task that the connected agent got, once it answered.
async fn connected_agent_task(world: &mut World) -> Value {
    if let Some(thread) = world.tasks.agent_task.take() {
        let task =
            blocking(move || thread.join().expect("the agent must answer")).await;
        world.tasks.agent_got = Some(task);
    }
    world.tasks.agent_got.clone().expect("the agent must be connected")
}

#[then("the connected agent got only the task id, the text and the instructions")]
async fn connected_agent_fields(world: &mut World) {
    let output = world.tasks.output();
    assert!(output.status.success(), "{output:?}");
    let task = connected_agent_task(world).await;
    let mut fields: Vec<_> =
        task.as_object().expect("task is an object").keys().cloned().collect();
    fields.sort();
    assert_eq!(fields, ["id", "instructions", "text", "type"]);
    assert_eq!(task["type"], "task");
}

#[then("the instructions tell the agent that the task text is untrusted")]
async fn connected_agent_instructions(world: &mut World) {
    let task = connected_agent_task(world).await;
    assert!(
        task["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("untrusted remote task text")),
        "{task}"
    );
}

// The outbox.

#[given("the caller saved the host's invite in its contacts")]
async fn saved_invite(world: &mut World) {
    let state = &mut world.tasks;
    let invitation = state.invitation().await;
    let caller = &state.callers[0];
    let (_, _, authorization) = caller.authorization("direct");
    let bundle = InvitationBundle::v1(
        serde_json::from_slice(&invitation).expect("invitation must decode"),
        serde_json::from_slice(
            &fs::read(authorization).expect("authorization must read"),
        )
        .expect("authorization must decode"),
        Some("https://relay.test".to_owned()),
    );
    let data = caller.data.clone();
    blocking(move || {
        aether::direct::import_bundle(&data, &bundle).expect("the contact must import");
    })
    .await;
}

#[given(expr = "the caller put {int} tasks in its outbox")]
async fn enqueue_tasks(world: &mut World, count: usize) {
    let data = world.tasks.callers[0].data.clone();
    let tasks = blocking(move || {
        (0..count)
            .map(|index| {
                aether::outbox::enqueue(
                    &data,
                    HOST_AGENT_ID,
                    &format!("task {index}"),
                    unix_now(),
                )
                .expect("the task must enqueue")
            })
            .collect()
    })
    .await;
    world.tasks.outbox = Some(Outbox { tasks, states: Vec::new(), endpoint: None });
}

fn courier(
    state: &State,
    endpoint: &iroh::Endpoint,
) -> Arc<dyn aether::outbox::Courier> {
    Arc::new(AddressCourier {
        endpoint: endpoint.clone(),
        address: state.host().address(),
    })
}

#[given(expr = "the outbox sent the {int} tasks at once")]
#[when(expr = "the outbox sends the {int} tasks at once")]
async fn outbox_attempts(world: &mut World, count: usize) {
    let state = &mut world.tasks;
    let data = state.callers[0].data.clone();
    let endpoint = bind_node(&data, 0, None).await.expect("client must bind");
    let courier = courier(state, &endpoint);
    let tasks = state.outbox().tasks.clone();
    assert_eq!(tasks.len(), count);
    let mut states = Vec::new();
    for task in &tasks {
        states.push(
            aether::outbox::attempt(&data, courier.as_ref(), task, Duration::ZERO)
                .await
                .expect("the attempt must run"),
        );
    }
    let outbox = state.outbox();
    outbox.states = states;
    outbox.endpoint = Some(endpoint);
}

#[then(expr = "the third task waits because {string}")]
async fn third_task_waits(world: &mut World, reason: String) {
    let state = &mut world.tasks;
    let data = state.callers[0].data.clone();
    let outbox = state.outbox();
    assert_eq!(outbox.states[2], SentState::Waiting, "{:?}", outbox.states);
    let task_id = outbox.tasks[2].task_id.clone();
    let full = blocking(move || {
        NodeStore::in_dir(&data)
            .expect("store must open")
            .sent_task(&task_id)
            .expect("read")
            .expect("the task exists")
    })
    .await;
    assert_eq!(full.reason.as_deref(), Some(reason.as_str()));
}

#[when("the outbox runs until it has no more work")]
async fn outbox_runs(world: &mut World) {
    let state = &mut world.tasks;
    let data = state.callers[0].data.clone();
    let endpoint =
        state.outbox().endpoint.clone().expect("the outbox must have sent its tasks");
    let courier = courier(state, &endpoint);
    let finished = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let report =
                aether::outbox::run_once(&data, courier.clone(), None, unix_now() + 15)
                    .await
                    .expect("the round must run");
            if !report.active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    assert!(finished.is_ok(), "the outbox must finish");
}

#[then(expr = "every task in the outbox has the answer {string}")]
async fn outbox_answers(world: &mut World, answer: String) {
    let state = &mut world.tasks;
    let data = state.callers[0].data.clone();
    let tasks = state.outbox().tasks.clone();
    blocking(move || {
        let store = NodeStore::in_dir(&data).expect("store must open");
        for task in &tasks {
            let kept =
                store.sent_task(&task.task_id).expect("read").expect("the task exists");
            assert_eq!(kept.state, SentState::Answered, "{kept:?}");
            assert_eq!(kept.answer.as_deref(), Some(answer.as_str()));
        }
    })
    .await;
}

#[then(expr = "the host node ran {int} tasks")]
async fn host_ran_tasks(world: &mut World, count: usize) {
    let tasks =
        world.tasks.store(|store| store.summary().expect("summary").tasks).await;
    assert_eq!(tasks, count);
}

// Admission and the task lifecycle.

#[given(regex = r"^the task of caller (\d+) is (running|queued)$")]
async fn caller_task_in_state(world: &mut World, caller: usize, wanted: String) {
    let index = caller - 1;
    let wanted = match wanted.as_str() {
        "running" => TaskState::Running,
        "queued" => TaskState::Queued,
        other => unreachable!("the step does not match {other}"),
    };
    let request = Request {
        caller: index,
        task_id: Some(overload_task_id(index)),
        ..world.tasks.request(&format!("request {index}"))
    };
    world.tasks.start_in_background(request, wanted).await;
}

#[when(expr = "caller {int} sends a task")]
async fn caller_sends_task(world: &mut World, caller: usize) {
    let index = caller - 1;
    let request = Request {
        caller: index,
        task_id: Some(overload_task_id(index)),
        ..world.tasks.request(&format!("request {index}"))
    };
    world.tasks.send(request).await;
}

#[given("the caller's task is running")]
async fn caller_task_running(world: &mut World) {
    let request = Request {
        task_id: Some(FOLLOWED_TASK_ID.to_owned()),
        idempotency_key: Some("followed-key".to_owned()),
        ..world.tasks.request("remote text")
    };
    world.tasks.start_in_background(request, TaskState::Running).await;
}

#[when("the owner cancels the caller's task")]
async fn owner_cancels(world: &mut World) {
    let socket =
        aether::data_dir::DataDir::new(world.tasks.server_data()).admin_socket();
    let task_id = world
        .tasks
        .request
        .as_ref()
        .and_then(|request| request.task_id.clone())
        .expect("a followed task must run");
    let response = blocking(move || {
        send_admin(&socket, &AdminRequest::Cancel { task_id })
            .expect("cancel request must send")
    })
    .await;
    world.tasks.admin = Some(response);
}

#[then("the host node confirms the cancellation")]
fn cancellation_confirmed(world: &mut World) {
    let response = world.tasks.admin.as_ref().expect("the owner must have asked");
    assert!(matches!(response, AdminResponse::Cancelled), "{response:?}");
}

#[then(expr = "the caller's running request fails with {string}")]
async fn running_request_fails(world: &mut World, text: String) {
    let child = world.tasks.running.0.pop().expect("a request must run");
    let output =
        blocking(move || child.wait_with_output().expect("request must finish")).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(stdout(&output).contains(&text), "{output:?}");
}

/// The real retention window is 24 hours. The store takes the cutoff as a
/// parameter, so a far-future cutoff is the same as 24 hours later. The
/// server keeps running on its own connection to the same database.
#[when("the stored result passes its retention window")]
async fn result_expires(world: &mut World) {
    let expired = world
        .tasks
        .store(|store| {
            store.clear_expired_content(far_future()).expect("content must expire")
        })
        .await;
    assert_eq!(expired, 1);
}

/// The lease of a running task expires after 30 seconds, and the recovery
/// runs every 30 seconds. The store takes the cutoff as a parameter, so a
/// far-future cutoff recovers the lease at once. The late completion of the
/// real execution is then refused by the task state machine.
#[when("the lease of the running task expires")]
async fn lease_expires(world: &mut World) {
    let task_id = world
        .tasks
        .request
        .as_ref()
        .and_then(|request| request.task_id.clone())
        .expect("a followed task must run");
    let recovered = world
        .tasks
        .store(|store| {
            store.recover_expired_leases(far_future()).expect("lease must recover")
        })
        .await;
    assert_eq!(recovered, vec![task_id]);
}

// Managed scopes.

#[given("the caller is a member of the managed roster")]
async fn roster_member(world: &mut World) {
    let caller = &world.tasks.callers[0];
    let member = aether_store::ManagedMember {
        scope_id: "managed".to_owned(),
        agent_id: "caller".to_owned(),
        endpoint_id: caller.endpoint.as_str().to_owned(),
        role: "member".to_owned(),
        expires_at: aether_core::time::unix_timestamp(
            SystemTime::now() + Duration::from_secs(60),
        )
        .expect("membership expiry must be valid"),
        revoked_at: None,
    };
    world
        .tasks
        .store(move |store| {
            store
                .apply_managed_member(ROSTER_CONTROL, 1, &member)
                .expect("managed membership must apply");
        })
        .await;
}

#[when("the roster revokes the caller")]
async fn roster_revokes(world: &mut World) {
    world
        .tasks
        .store(|store| {
            store
                .revoke_managed_member(ROSTER_CONTROL, 2, "managed", "caller")
                .expect("managed membership must revoke");
        })
        .await;
}

// The directory.

#[given("a directory service is running")]
async fn directory_running(world: &mut World) {
    let state = &mut world.tasks;
    let root = state.root();
    let control_data = root.join("control");
    let config = root.join("control.toml");
    fs::write(&config, format!("data_dir = \"{}\"\n", control_data.display()))
        .expect("control configuration must write");
    let mut process = ServerProcess::spawn(
        aether_command()
            .args(["control", "--config", utf8(&config), "--port", "0"])
            .stdout(Stdio::piped()),
    );
    let mut output = process.stdout();
    let (output, url) = blocking(move || {
        read_event(&mut output, None);
        let url = read_event(&mut output, None)["url"]
            .as_str()
            .expect("control URL must be reported")
            .to_owned();
        (output, url)
    })
    .await;
    state.directory = Some(Directory {
        process: Some((process, output)),
        url,
        certificate: aether::data_dir::DataDir::new(&control_data)
            .control_certificate(),
    });
}

#[given(
    expr = "the host node lists itself in the directory as {string} for {int} seconds at a time"
)]
fn host_listing(world: &mut World, agent_id: String, ttl: u64) {
    world.tasks.listing = Some(Listing { agent_id, ttl });
}

/// The agent id must survive the JSON event and the URL.
#[then(expr = "the host node reports its published record for {string}")]
fn published_record(world: &mut World, agent_id: String) {
    let published =
        world.tasks.published.as_ref().expect("the record must be published");
    assert_eq!(published["agent_id"], agent_id.as_str());
}

#[given("the directory gives the endpoint and the address of the host node")]
#[then("the directory gives the endpoint and the address of the host node")]
async fn directory_resolves(world: &mut World) {
    let record = world.tasks.resolve_record().await;
    let host = world.tasks.host();
    assert_eq!(record.endpoint_id, host.id);
    assert_eq!(record.addresses, [host.address()]);
    world.tasks.record = Some(record);
}

#[when(expr = "the record is {int} seconds old")]
async fn record_ages(_world: &mut World, seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

#[then("the directory gives a record that expires later")]
async fn record_renewed(world: &mut World) {
    let renewed = world.tasks.resolve_record().await;
    let record = world.tasks.record.as_ref().expect("a record must have been resolved");
    assert!(renewed.expires_at > record.expires_at, "{renewed:?} after {record:?}");
}

#[when("the directory service stops")]
async fn directory_stops(world: &mut World) {
    // The URL and the certificate stay: the caller still names the host by
    // the record it keeps.
    let (process, output) = world
        .tasks
        .directory
        .as_mut()
        .and_then(|directory| directory.process.take())
        .expect("a directory service must run");
    let status = blocking(move || process.interrupt()).await;
    drop(output);
    assert!(status.success(), "the directory service exited with {status}");
}

#[given(expr = "a reader gets {int} record(s) from the directory")]
#[then(expr = "a reader gets {int} record(s) from the directory")]
async fn reader_synchronizes(world: &mut World, count: usize) {
    let reader = world.tasks.root().join("reader");
    let mut command = world.tasks.directory_command();
    command.arg("sync").arg("--data-dir").arg(reader);
    let output = run(command).await;
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains(&format!("\"records\":{count}")), "{output:?}");
}

#[when("the owner tells the node to use the directory that it uses now")]
async fn change_to_same_directory(world: &mut World) {
    let url = world.tasks.directory().url.clone();
    world.tasks.change_directory(url).await;
}

#[when(expr = "the owner tells the node to use the directory {string}")]
async fn change_to_other_directory(world: &mut World, url: String) {
    world.tasks.change_directory(url).await;
}

#[then("the host node applies the change")]
fn change_applied(world: &mut World) {
    let response = world.tasks.admin.as_ref().expect("the owner must have asked");
    assert!(matches!(response, AdminResponse::Applied), "{response:?}");
}

#[then("the host node refuses the change as invalid")]
fn change_refused(world: &mut World) {
    let response = world.tasks.admin.as_ref().expect("the owner must have asked");
    assert!(
        matches!(response, AdminResponse::Error { code } if code == "invalid"),
        "{response:?}"
    );
}
