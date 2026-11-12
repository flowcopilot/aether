use std::{path::PathBuf, process::ExitCode, sync::Arc};

use usage_rs::{Args, Cli, Subcommands};

#[derive(Cli)]
#[usage(bin = "aether", version = env!("CARGO_PKG_VERSION"))]
struct Arguments {
    #[usage(subcommand)]
    command: CliCommand,
}

#[derive(Subcommands)]
enum CliCommand {
    Node(Box<Node>),
    Control(Control),
    Mcp(Mcp),
}

#[derive(Args)]
struct Process {
    #[usage(long)]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct Control {
    #[usage(long)]
    config: Option<PathBuf>,
    #[usage(long)]
    port: Option<u16>,
    #[usage(subcommand)]
    command: Option<ControlCommand>,
}

#[derive(Subcommands)]
enum ControlCommand {
    TestRecord,
    Invite {
        #[usage(long)]
        scope: String,
        #[usage(long)]
        expires_in: u64,
    },
    RevokeMember {
        #[usage(long)]
        scope: String,
        #[usage(long)]
        agent_id: String,
    },
}

#[derive(Args)]
struct Node {
    #[usage(long)]
    config: Option<PathBuf>,
    #[usage(subcommand)]
    command: Option<NodeCommand>,
}

#[derive(Subcommands)]
enum NodeCommand {
    Serve(NodeServe),
    Request(aether::node_request::RequestInput),
    Direct(NodeDirect),
    Invite(NodeInvite),
    Directory(NodeDirectory),
    Managed(NodeManaged),
    Record(NodeRecord),
    Endpoint(NodeEndpoint),
    Fixture(NodeFixture),
    Admin(NodeAdmin),
}

#[derive(Args)]
struct NodeServe {
    /// Old policy file to import on the first start. Later changes use SQLite.
    #[usage(long, default = "~/.aether/policy.json")]
    policy: PathBuf,
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long, default = "0")]
    port: u16,
    #[usage(long, default = "0")]
    admin_port: Option<u16>,
    /// The address the PACT gateway listens on, such as 127.0.0.1:8080.
    /// It needs a `pact` section in the policy and `--admin-port`, which
    /// serves the consent page.
    #[usage(long, requires = "--admin-port")]
    pact_listen: Option<String>,
    #[usage(long, requires = "--managed-control-certificate")]
    managed_control: Option<String>,
    #[usage(long, requires = "--managed-control")]
    managed_control_certificate: Option<PathBuf>,
    /// The agent adapter of this data directory: codex or connected. The
    /// node saves it; later starts keep it.
    #[usage(long)]
    adapter: Option<String>,
    /// The Codex binary of the Codex adapter.
    #[usage(long, env = "AETHER_CODEX_BIN", default = "codex")]
    codex_bin: PathBuf,
    /// An iroh relay to reach the node through.
    #[usage(long, env = "AETHER_RELAY_URL")]
    relay_url: Option<String>,
    /// The Aether hub that lets people invite and connect by name. Defaults
    /// to the one set on the page, else the public hub; `off` turns it off.
    #[usage(long, env = "AETHER_HUB_URL")]
    hub_url: Option<String>,
    #[usage(long)]
    directory: Option<String>,
    #[usage(long)]
    directory_certificate: Option<PathBuf>,
    /// The agent ID in the published directory record. It does not name
    /// invites.
    #[usage(long)]
    agent_id: Option<String>,
    #[usage(long)]
    address: Option<String>,
    #[usage(long)]
    scope: Option<String>,
    #[usage(long)]
    capability: Option<String>,
    #[usage(long)]
    directory_record_ttl: Option<u64>,
    /// The agent name in invites while this node has no name on the hub.
    /// Defaults to agent- and the first 8 characters of the endpoint ID.
    #[usage(long)]
    invite_name: Option<String>,
}

impl TryFrom<NodeServe> for aether::node_runtime::NodeServeOptions {
    type Error = aether::node_runtime::NodeRunError;

    fn try_from(arguments: NodeServe) -> Result<Self, Self::Error> {
        let adapter =
            arguments.adapter.as_deref().map(str::parse).transpose().map_err(|_| {
                aether::node_runtime::NodeRunError::Usage(
                    "--adapter must be codex or connected".to_owned(),
                )
            })?;
        Ok(Self {
            policy: aether::data_dir::expand_home(arguments.policy),
            data_dir: aether::data_dir::resolve_node_data_dir(arguments.data_dir),
            port: arguments.port,
            admin_port: arguments.admin_port,
            pact_listen: arguments.pact_listen,
            managed_control: arguments.managed_control,
            managed_control_certificate: arguments.managed_control_certificate,
            adapter,
            codex_bin: arguments.codex_bin,
            relay_url: arguments.relay_url,
            hub_url: arguments.hub_url,
            publication: aether::node_runtime::PublicationFlags {
                directory: arguments.directory,
                directory_certificate: arguments.directory_certificate,
                agent_id: arguments.agent_id,
                address: arguments.address,
                scope: arguments.scope,
                capability: arguments.capability,
                directory_record_ttl: arguments.directory_record_ttl,
            },
            invite_name: arguments.invite_name,
        })
    }
}

#[derive(Args)]
struct NodeDirect {
    /// Defaults to `$AETHER_DATA_DIR`, else `~/.aether/host`.
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(subcommand)]
    command: aether::direct::cli::Command,
}

#[derive(Args)]
struct NodeInvite {
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long)]
    agent_id: String,
    #[usage(long)]
    expires_in: u64,
}

#[derive(Args)]
struct NodeDirectory {
    #[usage(long)]
    directory: String,
    #[usage(long)]
    directory_certificate: PathBuf,
    #[usage(long)]
    agent_verifying_key: PathBuf,
    #[usage(subcommand)]
    command: aether::directory_client::cli::Command,
}

#[derive(Args)]
struct NodeManaged {
    #[usage(long)]
    control: String,
    #[usage(long)]
    control_certificate: PathBuf,
    #[usage(subcommand)]
    command: aether::managed_client::cli::Command,
}

#[derive(Args)]
struct NodeRecord {
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long)]
    agent_id: String,
    #[usage(long)]
    address: String,
    #[usage(long)]
    scope: String,
    #[usage(long)]
    capability: String,
    #[usage(long)]
    expires_in: u64,
}

#[derive(Args)]
struct NodeEndpoint {
    #[usage(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Args)]
struct NodeFixture {
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long)]
    scope: Option<String>,
}

#[derive(Args)]
struct NodeAdmin {
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long)]
    port: u16,
    /// The Aether hub that lets people invite and connect by name. Defaults
    /// to the one set on the page, else the public hub; `off` turns it off.
    #[usage(long, env = "AETHER_HUB_URL")]
    hub_url: Option<String>,
    #[usage(long)]
    managed_scope: bool,
}

#[derive(Args)]
struct Mcp {
    #[usage(long)]
    config: Option<PathBuf>,
    #[usage(subcommand)]
    command: Option<McpCommand>,
}

#[derive(Subcommands)]
enum McpCommand {
    Serve(McpServe),
}

#[derive(Args)]
struct McpServe {
    /// The node's administration socket. Defaults to the one in the data
    /// directory.
    #[usage(long)]
    socket: Option<PathBuf>,
    /// The data directory with the friends' agents you can use. Defaults to
    /// `$AETHER_DATA_DIR`, else `~/.aether/host`.
    #[usage(long)]
    data_dir: Option<PathBuf>,
    /// List only the tools that send tasks and read answers, for an agent
    /// that must not manage this node.
    #[usage(long)]
    caller_only: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match Arguments::parse().command {
        CliCommand::Node(node) => Box::pin(run_node(*node)).await,
        CliCommand::Control(process) => run_control(process).await,
        CliCommand::Mcp(mcp) => run_mcp(mcp).await,
    }
}

async fn run_control(control: Control) -> ExitCode {
    if let Some(command) = control.command.as_ref() {
        match command {
            ControlCommand::TestRecord => {
                match serde_json::to_string_pretty(
                    &aether::discovery::wire::WireRecord::test_record(),
                ) {
                    Ok(record) => {
                        println!("{record}");
                        return ExitCode::SUCCESS;
                    }
                    Err(error) => {
                        eprintln!("aether: {error}");
                        return ExitCode::from(1);
                    }
                }
            }
            ControlCommand::Invite { scope, expires_in } => {
                return run_control_invite(
                    control.config.as_deref(),
                    scope,
                    *expires_in,
                );
            }
            ControlCommand::RevokeMember { scope, agent_id } => {
                return run_control_member_revocation(
                    control.config.as_deref(),
                    scope,
                    agent_id,
                );
            }
        }
    }
    let config = match control_config(control.config.as_deref()) {
        Ok(config) => config,
        Err(exit_code) => return exit_code,
    };
    println!("{}", aether::startup_log(aether::Command::Control, &config));
    let handle = axum_server::Handle::new();
    let shutdown = handle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
        }
    });
    match aether::control_service::serve(
        &config.data_dir,
        control.port.unwrap_or(8443),
        handle,
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether: control failed: {error}");
            ExitCode::from(1)
        }
    }
}

/// The control configuration at `path`; its error is printed and becomes
/// exit code 2.
fn control_config(path: Option<&std::path::Path>) -> Result<aether::Config, ExitCode> {
    aether::load_config(path, aether::Command::Control).map_err(|error| {
        eprintln!("aether: {error}");
        ExitCode::from(2)
    })
}

fn run_control_invite(
    config_path: Option<&std::path::Path>,
    scope_id: &str,
    expires_in: u64,
) -> ExitCode {
    let config = match control_config(config_path) {
        Ok(config) => config,
        Err(exit_code) => return exit_code,
    };
    let data_dir = aether::data_dir::DataDir::new(config.data_dir);
    if data_dir.create().is_err() {
        eprintln!("aether: control data directory cannot be created");
        return ExitCode::from(1);
    }
    let Ok(service) =
        aether::control_service::ControlService::open(data_dir.control_store())
    else {
        eprintln!("aether: control store cannot be opened");
        return ExitCode::from(1);
    };
    let Ok(token) = service.create_managed_invitation(
        scope_id,
        std::time::Duration::from_secs(expires_in),
    ) else {
        eprintln!("aether: managed invitation cannot be created");
        return ExitCode::from(1);
    };
    println!(
        "{}",
        serde_json::json!({
            "event": "managed_invitation_created",
            "scope": scope_id,
            "expiresIn": expires_in,
            "token": token,
        })
    );
    ExitCode::SUCCESS
}

fn run_control_member_revocation(
    config_path: Option<&std::path::Path>,
    scope_id: &str,
    agent_id: &str,
) -> ExitCode {
    let config = match control_config(config_path) {
        Ok(config) => config,
        Err(exit_code) => return exit_code,
    };
    let Ok(service) = aether::control_service::ControlService::open(
        aether::data_dir::DataDir::new(config.data_dir).control_store(),
    ) else {
        eprintln!("aether: control store cannot be opened");
        return ExitCode::from(1);
    };
    if service.revoke_managed_member(scope_id, agent_id).is_err() {
        eprintln!("aether: managed member cannot be revoked");
        return ExitCode::from(1);
    }
    println!(
        "{}",
        serde_json::json!({
            "event": "managed_member_revoked",
            "scope": scope_id,
            "agent": agent_id,
        })
    );
    ExitCode::SUCCESS
}

fn run_process(command: aether::Command, process: &Process) -> ExitCode {
    match aether::load_config(process.config.as_deref(), command) {
        Ok(config) => {
            println!("{}", aether::startup_log(command, &config));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aether: {error}");
            ExitCode::from(2)
        }
    }
}

async fn run_mcp(mcp: Mcp) -> ExitCode {
    match mcp.command {
        Some(McpCommand::Serve(McpServe { socket, data_dir, caller_only })) => {
            let data_dir = aether::data_dir::resolve_node_data_dir(data_dir);
            let socket = socket.unwrap_or_else(|| {
                aether::data_dir::DataDir::new(&data_dir).admin_socket()
            });
            let hub_flag = std::env::var("AETHER_HUB_URL").ok();
            match aether::mcp::serve_stdio_for(socket, data_dir, hub_flag, caller_only)
                .await
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("aether: mcp failed: {error}");
                    ExitCode::from(1)
                }
            }
        }
        None => run_process(aether::Command::Mcp, &Process { config: mcp.config }),
    }
}

async fn run_admin(arguments: NodeAdmin) -> ExitCode {
    let data_dir = aether::data_dir::resolve_node_data_dir(arguments.data_dir);
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("aether: admin failed: {error}");
        return ExitCode::from(1);
    }
    let store = match aether_store::NodeStore::in_dir(&data_dir) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("aether: admin failed: {error}");
            return ExitCode::from(1);
        }
    };
    let administration = aether::admin::shared_with_store(store);
    let hub = match aether::hub_client::configured_hub_client(
        arguments.hub_url.as_deref(),
        &data_dir,
    ) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("aether: {error}");
            return ExitCode::from(2);
        }
    };
    let socket = aether::data_dir::DataDir::new(&data_dir).admin_socket();
    if let Err(error) = aether::admin_socket::start(&socket, administration.clone()) {
        eprintln!("aether: admin failed: {error}");
        return ExitCode::from(1);
    }
    match aether::admin_ui::serve_tls_with_scope(
        arguments.port,
        &data_dir,
        administration,
        aether::admin_ui::UiOptions {
            managed: arguments.managed_scope,
            // `node admin` runs without a node, so its data directory can be
            // a caller's own: its endpoint key and contacts.
            page: Some(Arc::new(
                aether::admin_ui::PageContext::admin_caller(data_dir.clone())
                    .with_hub(hub),
            )),
            ..Default::default()
        },
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether: admin failed: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run_request(input: aether::node_request::RequestInput) -> ExitCode {
    match aether::node_request::send(input).await {
        Ok(response) => {
            let exit_code = aether::transport::response_exit_code(&response);
            println!(
                "{}",
                serde_json::json!({ "event": "node_completed", "response": response })
            );
            exit_code
        }
        Err(aether::node_request::RequestError::AuthorizationRequired) => {
            eprintln!("aether: request authorization is required");
            ExitCode::from(2)
        }
        Err(aether::node_request::RequestError::AuthorizationInvalid) => {
            eprintln!("aether: request authorization is invalid");
            ExitCode::from(2)
        }
        Err(aether::node_request::RequestError::TaskIdInvalid) => {
            eprintln!("aether: request task id must be a UUID");
            ExitCode::from(2)
        }
        Err(aether::node_request::RequestError::FrameInvalid) => {
            eprintln!("aether: request is invalid");
            ExitCode::from(2)
        }
        Err(aether::node_request::RequestError::Addressing) => {
            eprintln!(
                "aether: specify an endpoint and address, a direct agent and address, or directory settings"
            );
            ExitCode::from(2)
        }
        Err(aether::node_request::RequestError::Transport(error)) => {
            transport_error(&error)
        }
    }
}

async fn run_directory(arguments: NodeDirectory) -> ExitCode {
    aether::directory_client::cli::run(aether::directory_client::cli::Options {
        directory: arguments.directory,
        directory_certificate: arguments.directory_certificate,
        agent_verifying_key: arguments.agent_verifying_key,
        command: arguments.command,
    })
    .await
}

async fn run_managed(arguments: NodeManaged) -> ExitCode {
    aether::managed_client::cli::run(aether::managed_client::cli::Options {
        control: arguments.control,
        control_certificate: arguments.control_certificate,
        command: arguments.command,
    })
    .await
}

async fn run_node(node: Node) -> ExitCode {
    match node.command {
        None => run_process(aether::Command::Node, &Process { config: node.config }),
        Some(NodeCommand::Admin(arguments)) => run_admin(arguments).await,
        Some(NodeCommand::Direct(arguments)) => {
            let data_dir = aether::data_dir::resolve_node_data_dir(arguments.data_dir);
            aether::direct::cli::run(&data_dir, arguments.command)
        }
        Some(NodeCommand::Invite(arguments)) => aether::direct::cli::run_invite(
            &aether::data_dir::resolve_node_data_dir(arguments.data_dir),
            &arguments.agent_id,
            arguments.expires_in,
        ),
        Some(NodeCommand::Directory(arguments)) => run_directory(arguments).await,
        Some(NodeCommand::Managed(arguments)) => run_managed(arguments).await,
        Some(NodeCommand::Record(arguments)) => {
            let data_dir = aether::data_dir::resolve_node_data_dir(arguments.data_dir);
            aether::directory_publication::cli::run_record(
                aether::directory_publication::cli::RecordInput {
                    data_dir: &data_dir,
                    agent_id: arguments.agent_id,
                    address: arguments.address,
                    scope: arguments.scope,
                    capability: arguments.capability,
                    expires_in: arguments.expires_in,
                },
            )
        }
        Some(NodeCommand::Endpoint(arguments)) => {
            let data_dir = aether::data_dir::resolve_node_data_dir(arguments.data_dir);
            let endpoint = match aether::transport::bind_node(&data_dir, 0, None).await
            {
                Ok(endpoint) => endpoint,
                Err(error) => return transport_error(&error),
            };
            println!(
                "{}",
                serde_json::json!({ "endpoint_id": endpoint.id().to_string() })
            );
            endpoint.close().await;
            ExitCode::SUCCESS
        }
        Some(NodeCommand::Fixture(arguments)) => {
            let data_dir = aether::data_dir::resolve_node_data_dir(arguments.data_dir);
            match aether::fixture::create_for_scope(
                &data_dir,
                arguments.scope.as_deref().unwrap_or("direct"),
            )
            .await
            {
                Ok(result) => {
                    println!("{result}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some(NodeCommand::Serve(arguments)) => {
            let result =
                match aether::node_runtime::NodeServeOptions::try_from(arguments) {
                    Ok(options) => aether::node_runtime::run(options).await,
                    Err(error) => Err(error),
                };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    match &error {
                        aether::node_runtime::NodeRunError::Usage(message)
                        | aether::node_runtime::NodeRunError::Failed(message) => {
                            eprintln!("aether: {message}");
                        }
                        aether::node_runtime::NodeRunError::Transport(transport) => {
                            eprintln!("aether: transport failed: {transport:?}");
                        }
                    }
                    error.exit_code()
                }
            }
        }
        Some(NodeCommand::Request(arguments)) => run_request(arguments).await,
    }
}

fn transport_error(error: &aether::transport::TransportError) -> ExitCode {
    eprintln!("aether: transport failed: {error:?}");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use aether::directory_client::{cache_directory_record, cached_directory_record};
    use aether::managed_client::{cached_managed_member, sync_managed};
    use axum_server::Handle;
    use ed25519_dalek::SigningKey;

    #[test]
    fn stale_cached_directory_record_cannot_be_used() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        let wire = aether::discovery::wire::WireRecord::test_record();
        let record = wire.to_record().expect("record must decode");
        cache_directory_record(&data_dir, &record).expect("record must cache");
        let store =
            aether_store::NodeStore::in_dir(&data_dir).expect("store must open");
        assert!(
            store
                .mark_directory_record_stale("test-scope", "test-agent")
                .expect("record must become stale")
        );
        assert!(
            cached_directory_record(
                &data_dir,
                "test-scope",
                "test-agent",
                record.signing_key,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn managed_roster_sync_resumes_and_marks_revoked_members_stale() {
        let directory = tempfile::tempdir().expect("directory must create");
        let root = directory.path().to_path_buf();
        let control_data = root.join("control");
        let node_data = root.join("node");
        let handle = Handle::new();
        let server = tokio::spawn({
            let handle = handle.clone();
            let control_data = control_data.clone();
            async move { aether::control_service::serve(&control_data, 0, handle).await }
        });
        let address = tokio::time::timeout(Duration::from_secs(5), handle.listening())
            .await
            .expect("control must listen")
            .expect("control address must exist");
        let certificate = fs::read(
            aether::data_dir::DataDir::new(&control_data).control_certificate(),
        )
        .expect("certificate must exist");
        let control = format!("https://127.0.0.1:{}", address.port());
        let client = aether::managed_client::ManagedClient::with_pinned_certificate(
            &control,
            &certificate,
        )
        .expect("managed client must open");
        let service = aether::control_service::ControlService::open(
            aether::data_dir::DataDir::new(&control_data).control_store(),
        )
        .expect("control store must open");
        let token = service
            .create_managed_invitation("managed", Duration::from_secs(60))
            .expect("invitation must create");
        let record = aether::discovery::wire::WireRecord::from_record(
            &aether::discovery::signed_record(
                &SigningKey::from_bytes(&[10; 32]),
                "managed-agent".to_owned(),
                "managed-endpoint".to_owned(),
                vec!["127.0.0.1:9443".to_owned()],
                "managed".to_owned(),
                vec!["test".to_owned()],
                SystemTime::now() + Duration::from_secs(60),
            ),
        );
        client.redeem(&token, record).await.expect("member must redeem");

        assert_eq!(
            sync_managed(&node_data, &control, "managed", &client)
                .await
                .expect("first sync must succeed"),
            (1, 1)
        );
        assert_eq!(
            sync_managed(&node_data, &control, "managed", &client)
                .await
                .expect("resumed sync must succeed"),
            (1, 0)
        );
        service
            .revoke_managed_member("managed", "managed-agent")
            .expect("member must revoke");
        assert_eq!(
            sync_managed(&node_data, &control, "managed", &client)
                .await
                .expect("revocation sync must succeed"),
            (2, 1)
        );
        let store =
            aether_store::NodeStore::in_dir(&node_data).expect("node store must open");
        let members = store.managed_members("managed").expect("members must load");
        assert_eq!(members.len(), 1);
        assert!(members[0].revoked_at.is_some());

        handle.graceful_shutdown(Some(Duration::from_secs(1)));
        server.await.expect("server task must finish").expect("server must stop");
        drop(service);
        drop(store);
    }

    #[test]
    fn stale_cached_managed_member_cannot_be_used() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        let mut store =
            aether_store::NodeStore::in_dir(&data_dir).expect("store must open");
        store
            .apply_managed_member(
                "https://control.test",
                1,
                &aether_store::ManagedMember {
                    scope_id: "managed".to_owned(),
                    agent_id: "member-agent".to_owned(),
                    endpoint_id: "member-endpoint".to_owned(),
                    role: "member".to_owned(),
                    expires_at: i64::MAX,
                    revoked_at: None,
                },
            )
            .expect("member must store");
        assert!(cached_managed_member(&data_dir, "managed", "member-agent").is_ok());
        store
            .revoke_managed_member("https://control.test", 2, "managed", "member-agent")
            .expect("member must revoke");
        assert!(cached_managed_member(&data_dir, "managed", "member-agent").is_err());
    }
}
