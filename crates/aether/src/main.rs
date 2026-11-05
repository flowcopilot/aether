use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use hex::FromHex;

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
    Request(NodeRequest),
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
    #[usage(long, default = "~/.aether/host")]
    data_dir: PathBuf,
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

#[derive(Args)]
struct NodeRequest {
    /// Defaults to `$AETHER_DATA_DIR`, else `~/.aether/host`.
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(long)]
    endpoint_id: Option<String>,
    #[usage(long)]
    address: Option<String>,
    #[usage(long)]
    directory: Option<String>,
    #[usage(long)]
    directory_certificate: Option<PathBuf>,
    #[usage(long)]
    agent_id: Option<String>,
    #[usage(long)]
    agent_verifying_key: Option<PathBuf>,
    #[usage(
        long,
        requires("--managed-control-certificate", "--agent-id"),
        conflicts(
            "--endpoint-id",
            "--address",
            "--directory",
            "--directory-certificate",
            "--agent-verifying-key"
        )
    )]
    managed_control: Option<String>,
    #[usage(long, requires = "--managed-control")]
    managed_control_certificate: Option<PathBuf>,
    /// An iroh relay to reach the peer through.
    #[usage(long, env = "AETHER_RELAY_URL")]
    relay_url: Option<String>,
    #[usage(long)]
    scope: String,
    #[usage(long)]
    text: String,
    /// Optional for a direct contact imported from an invite: its bundle
    /// holds the authorization.
    #[usage(long)]
    authorization: Option<PathBuf>,
    /// Lets a caller retry a request idempotently by resubmitting the same
    /// key. A fresh one is generated when this is left unset, matching the
    /// previous, only behavior.
    #[usage(long)]
    idempotency_key: Option<String>,
    /// Lets a caller learn its own task id ahead of the response, so it can
    /// target this request with an administration cancellation. A fresh one
    /// is generated when this is left unset, matching the previous, only
    /// behavior. Must be a UUID, the same requirement the wire protocol
    /// already places on every task id.
    #[usage(long)]
    task_id: Option<String>,
}

#[derive(Args)]
struct NodeDirect {
    /// Defaults to `$AETHER_DATA_DIR`, else `~/.aether/host`.
    #[usage(long)]
    data_dir: Option<PathBuf>,
    #[usage(subcommand)]
    command: NodeDirectCommand,
}

#[derive(Subcommands)]
enum NodeDirectCommand {
    /// Imports an `aether-invite:` line, or a file with an invitation or an
    /// invitation bundle.
    Import {
        invitation: String,
    },
    Resolve {
        agent_id: String,
    },
    /// Forgets a contact: its invitation and its stored access.
    Remove {
        agent_id: String,
    },
}

/// The data directory a command uses when `--data-dir` is not given:
/// `$AETHER_DATA_DIR`, else `~/.aether/host`. `node admin`, `node direct` and
/// `node request` share it, so a copied invite command reaches the same
/// contacts that the caller's web page shows.
fn data_dir_or_default(data_dir: Option<PathBuf>) -> PathBuf {
    aether::data_dir::default_node_data_dir(
        data_dir,
        |name| std::env::var_os(name),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn expand_home(path: PathBuf) -> PathBuf {
    path.strip_prefix("~")
        .ok()
        .and_then(|rest| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))
        })
        .unwrap_or(path)
}

#[derive(Args)]
struct NodeInvite {
    #[usage(long)]
    data_dir: PathBuf,
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
    command: NodeDirectoryCommand,
}

#[derive(Args)]
struct NodeManaged {
    #[usage(long)]
    control: String,
    #[usage(long)]
    control_certificate: PathBuf,
    #[usage(subcommand)]
    command: NodeManagedCommand,
}

#[derive(Subcommands)]
enum NodeManagedCommand {
    Redeem {
        #[usage(long)]
        token_file: PathBuf,
        record: PathBuf,
    },
    Members {
        scope: String,
    },
    Events {
        scope: String,
        #[usage(long)]
        after: Option<i64>,
    },
    Sync {
        scope: String,
        #[usage(long)]
        data_dir: PathBuf,
    },
}

#[derive(Args)]
struct NodeRecord {
    #[usage(long)]
    data_dir: PathBuf,
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

#[derive(Subcommands)]
enum NodeDirectoryCommand {
    Publish {
        record: PathBuf,
    },
    Renew {
        record: PathBuf,
    },
    Resolve {
        agent_id: String,
    },
    Search {
        capability: String,
    },
    Revoke {
        record: PathBuf,
    },
    Sync {
        #[usage(long)]
        data_dir: PathBuf,
    },
}

#[derive(Args)]
struct NodeEndpoint {
    #[usage(long)]
    data_dir: PathBuf,
}

#[derive(Args)]
struct NodeFixture {
    #[usage(long)]
    data_dir: PathBuf,
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
    let config = match aether::load_config(
        control.config.as_deref(),
        aether::Command::Control,
    ) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aether: {error}");
            return ExitCode::from(2);
        }
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

fn run_control_invite(
    config_path: Option<&std::path::Path>,
    scope_id: &str,
    expires_in: u64,
) -> ExitCode {
    let config = match aether::load_config(config_path, aether::Command::Control) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aether: {error}");
            return ExitCode::from(2);
        }
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
    let config = match aether::load_config(config_path, aether::Command::Control) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("aether: {error}");
            return ExitCode::from(2);
        }
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
            let data_dir = data_dir_or_default(data_dir);
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

/// The hub client for `--hub-url`, the page setting or the default hub,
/// signing with the node key in `data_dir`.
fn hub_client(
    hub_url: Option<&str>,
    data_dir: &Path,
) -> Result<Option<aether::hub_client::HubClient>, ExitCode> {
    aether::hub_client::resolve_url(
        hub_url,
        aether::settings::setting::<aether::hub_client::settings::Hub>(data_dir)
            .ok()
            .flatten(),
    )
    .map(|url| aether::hub_client::HubClient::for_data_dir(&url, data_dir))
    .transpose()
    .map_err(|error| {
        eprintln!("aether: {error}");
        ExitCode::from(2)
    })
}

async fn run_admin(arguments: NodeAdmin) -> ExitCode {
    let data_dir = data_dir_or_default(arguments.data_dir);
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
    let hub = match hub_client(arguments.hub_url.as_deref(), &data_dir) {
        Ok(hub) => hub,
        Err(code) => return code,
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

async fn run_node(node: Node) -> ExitCode {
    match node.command {
        None => run_process(aether::Command::Node, &Process { config: node.config }),
        Some(NodeCommand::Admin(arguments)) => run_admin(arguments).await,
        Some(NodeCommand::Direct(arguments)) => run_direct(arguments),
        Some(NodeCommand::Invite(arguments)) => run_invite(&arguments),
        Some(NodeCommand::Directory(arguments)) => run_directory(arguments).await,
        Some(NodeCommand::Managed(arguments)) => run_managed(arguments).await,
        Some(NodeCommand::Record(arguments)) => run_record(arguments),
        Some(NodeCommand::Endpoint(arguments)) => {
            let endpoint =
                match aether::transport::bind_node(arguments.data_dir, 0, None).await {
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
            match aether::fixture::create_for_scope(
                &arguments.data_dir,
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
        Some(NodeCommand::Serve(mut arguments)) => {
            arguments.policy = expand_home(arguments.policy);
            arguments.data_dir = expand_home(arguments.data_dir);
            // Only a hub gives a number, so the flag is a name part.
            if arguments
                .invite_name
                .as_deref()
                .is_some_and(|name| !aether::hub_client::valid_name_part(name))
            {
                eprintln!(
                    "aether: --invite-name must be 1 to 32 lowercase letters, digits, dots, underscores or dashes, starting with a letter or digit"
                );
                return ExitCode::from(2);
            }
            let Ok(adapter) = arguments.adapter.as_deref().map(str::parse).transpose()
            else {
                eprintln!("aether: --adapter must be codex or connected");
                return ExitCode::from(2);
            };
            let service = match aether::node_builder::load_or_create_service(
                &arguments.policy,
                &arguments.data_dir,
                arguments.codex_bin.clone(),
                adapter,
            ) {
                Ok(service) => Arc::new(service),
                Err(error) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(2);
                }
            };
            match service.agent_adapter() {
                aether::policy::AgentAdapter::Codex => {
                    let codex_home =
                        aether::data_dir::DataDir::new(&arguments.data_dir)
                            .codex_home();
                    if !aether::data_dir::codex_login(&codex_home).exists() {
                        eprintln!(
                            "aether: Codex is not logged in for this node, so tasks will fail. Run: CODEX_HOME={} codex login --device-auth",
                            codex_home.display()
                        );
                    }
                }
                aether::policy::AgentAdapter::Connected => eprintln!(
                    "aether: tasks go to the agent that connects to {}",
                    aether::data_dir::DataDir::new(&arguments.data_dir)
                        .agent_socket()
                        .display()
                ),
            }
            let hub =
                match hub_client(arguments.hub_url.as_deref(), &arguments.data_dir) {
                    Ok(hub) => hub,
                    Err(code) => return code,
                };
            // The PACT gateway: started before the admin UI, which serves
            // its consent page and tells it where that page is.
            let pact = match (arguments.pact_listen.as_deref(), service.pact_config()) {
                (None, _) => None,
                (Some(_), None) => {
                    eprintln!(
                        "aether: --pact-listen needs a pact section in the policy"
                    );
                    return ExitCode::from(2);
                }
                (Some(address), Some(config)) => {
                    let listener = match tokio::net::TcpListener::bind(address).await {
                        Ok(listener) => listener,
                        Err(error) => {
                            eprintln!(
                                "aether: the PACT gateway cannot listen: {error}"
                            );
                            return ExitCode::from(1);
                        }
                    };
                    let node: Arc<dyn aether::pact::gateway::PactNode> =
                        service.clone();
                    match aether::pact::server::start(config, &arguments.data_dir, node)
                    {
                        Ok(started) => Some((
                            listener,
                            started,
                            config.public_url.clone(),
                            config.max_seconds,
                        )),
                        Err(error) => {
                            eprintln!("aether: the PACT gateway cannot start: {error}");
                            return ExitCode::from(1);
                        }
                    }
                }
            };
            let managed_scope = service.has_scope_kind(aether_core::ScopeKind::Managed);
            // The usage spec requires the two flags together.
            let managed_sync = match arguments
                .managed_control
                .as_deref()
                .zip(arguments.managed_control_certificate.as_deref())
            {
                None => None,
                Some((control, certificate_path)) => {
                    let Some(scope_id) = service
                        .scope_id_for_kind(aether_core::ScopeKind::Managed)
                        .map(str::to_owned)
                    else {
                        eprintln!(
                            "aether: managed control requires exactly one managed node scope"
                        );
                        return ExitCode::from(2);
                    };
                    let Ok(certificate) = std::fs::read(certificate_path) else {
                        eprintln!("aether: managed control certificate cannot be read");
                        return ExitCode::from(2);
                    };
                    let Ok(client) =
                        aether::managed_client::ManagedClient::with_pinned_certificate(
                            control,
                            &certificate,
                        )
                    else {
                        eprintln!("aether: managed control connection is invalid");
                        return ExitCode::from(2);
                    };
                    if sync_managed(&arguments.data_dir, control, &scope_id, &client)
                        .await
                        .is_err()
                    {
                        eprintln!("aether: managed roster synchronization failed");
                        return ExitCode::from(1);
                    }
                    Some((client, control.to_owned(), scope_id))
                }
            };
            let administration =
                match aether_store::NodeStore::in_dir(&arguments.data_dir) {
                    Ok(store) => aether::admin::shared_with_store(store),
                    Err(error) => {
                        eprintln!("aether: administration failed: {error}");
                        return ExitCode::from(1);
                    }
                };
            let socket =
                aether::data_dir::DataDir::new(&arguments.data_dir).admin_socket();
            let live = Arc::new(aether::node_service::RunningNode::new(
                Arc::clone(&service),
                arguments.data_dir.clone(),
            ));
            if let Ok(mut administration) = administration.lock() {
                administration.set_live(live.clone());
            } else {
                eprintln!("aether: administration failed: service is unavailable");
                return ExitCode::from(1);
            }
            if let Err(error) =
                aether::admin_socket::start(&socket, administration.clone())
            {
                eprintln!("aether: administration failed: {error}");
                return ExitCode::from(1);
            }
            // The flag or environment wins over the owner's saved choice.
            // `off` also lets a headless node use only its local network.
            let relay_url = arguments
                .relay_url
                .clone()
                .or_else(|| {
                    aether::settings::setting::<aether::transport::relay::Relay>(
                        &arguments.data_dir,
                    )
                    .ok()
                    .flatten()
                })
                .filter(|relay| relay != "off");
            let endpoint = match aether::transport::bind_node(
                &arguments.data_dir,
                arguments.port,
                relay_url.as_deref(),
            )
            .await
            {
                Ok(endpoint) => endpoint,
                Err(error) => return transport_error(&error),
            };
            if aether::settings::store_setting::<aether::transport::relay::ActiveRelay>(
                &arguments.data_dir,
                &relay_url,
            )
            .and_then(|()| {
                // The name of connect codes from the admin socket while the
                // node has no name on the hub, as on the host page.
                aether::settings::store_setting::<aether::direct::HostName>(
                    &arguments.data_dir,
                    &arguments.invite_name.clone().unwrap_or_else(|| {
                        aether::direct::default_agent_name(
                            &aether::direct::endpoint_id(&arguments.data_dir)
                                .unwrap_or_default(),
                        )
                    }),
                )
            })
            .is_err()
            {
                endpoint.close().await;
                eprintln!("aether: node settings cannot be stored");
                return ExitCode::from(1);
            }
            live.attach_endpoint(endpoint.clone(), tokio::runtime::Handle::current());
            let bound_port = endpoint
                .addr()
                .ip_addrs()
                .next()
                .map_or(arguments.port, std::net::SocketAddr::port);
            let publication_scope =
                arguments.scope.as_deref().unwrap_or_else(|| service.scope_id());
            if arguments.directory.is_some()
                && service.scope_kind_for(publication_scope)
                    != Some(aether_core::ScopeKind::Directory)
            {
                endpoint.close().await;
                eprintln!(
                    "aether: directory publication requires a directory node scope"
                );
                return ExitCode::from(2);
            }
            let Ok(directory_publication) = start_directory_publication(
                &arguments,
                endpoint.id().to_string(),
                bound_port,
                publication_scope,
            )
            .await
            else {
                endpoint.close().await;
                eprintln!("aether: directory publication is unavailable");
                return ExitCode::from(1);
            };
            println!(
                "{}",
                serde_json::json!({
                    "event": "node_listening",
                    "endpoint_id": endpoint.id().to_string(),
                    "port": bound_port,
                })
            );
            if let Some((_, record)) = &directory_publication {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "directory_record_published",
                        "agent_id": record.agent_id,
                        "signing_key": hex::encode(record.signing_key),
                    })
                );
            }
            let directory_publication = directory_publication
                .map(|(publication, record)| (Arc::new(publication), record));
            if let Some((publication, _)) = &directory_publication {
                // Admin-triggered "renew now" for the already-configured,
                // already-pinned directory record, and its capability
                // publication. `renew` reuses the same pinned client and
                // signed-record construction the periodic renewer below uses.
                live.attach_directory(Arc::clone(publication));
            }
            // The outbox sends the tasks of this node's own agent with the
            // node's endpoint, and keeps their answers for the inbox.
            let outbox_wake = Arc::new(tokio::sync::Notify::new());
            live.attach_outbox(Arc::clone(&outbox_wake));
            let outbox = tokio::spawn(aether::outbox::run(
                arguments.data_dir.clone(),
                Arc::new(aether::outbox::RelayCourier::shared(endpoint.clone())),
                arguments.hub_url.clone(),
                outbox_wake,
            ));
            let relay_switcher: aether::admin_ui::RelaySwitcher = Arc::new({
                let endpoint = endpoint.clone();
                move |previous, relay| {
                    let endpoint = endpoint.clone();
                    Box::pin(async move {
                        aether::transport::switch_relay(
                            &endpoint,
                            previous.as_deref(),
                            relay.as_deref(),
                        )
                        .await
                    })
                }
            });
            let background = aether::node_runtime::start_background_workers(
                &administration,
                &arguments.data_dir,
                hub.clone(),
                arguments.invite_name.clone(),
                relay_url.clone(),
                Some(Arc::clone(&relay_switcher)),
                directory_publication
                    .as_ref()
                    .map(|(publication, _)| Arc::clone(publication)),
                endpoint.id().to_string(),
                managed_sync.map(|(client, control, scope_id)| {
                    (Arc::new(client), control, scope_id)
                }),
            );
            let admin_server = arguments.admin_port.map(|admin_port| {
                let handle = axum_server::Handle::new();
                let data_dir = arguments.data_dir.clone();
                let administration = administration.clone();
                let shutdown = handle.clone();
                let options = aether::admin_ui::UiOptions {
                    managed: managed_scope,
                    page: Some(Arc::new(
                        aether::admin_ui::PageContext::host(
                            data_dir.clone(),
                            arguments.invite_name.clone(),
                            relay_url.clone(),
                            Arc::clone(&relay_switcher),
                        )
                        .with_hub(hub.clone())
                        .with_caller(true)
                        .with_task_folder_mode(service.task_folder_mode()),
                    )),
                    pact: pact.as_ref().map(|(_, started, _, _)| {
                        aether::admin_ui::PactUi {
                            consent: Arc::clone(&started.consent),
                            link: Arc::clone(&started.link),
                        }
                    }),
                };
                let server = tokio::spawn(async move {
                    aether::admin_ui::serve_tls_with_scope_and_handle(
                        admin_port,
                        &data_dir,
                        administration,
                        options,
                        shutdown,
                    )
                    .await
                });
                (handle, server)
            });
            let pact_shutdown = Arc::new(tokio::sync::Notify::new());
            let pact_server = pact.map(|(listener, started, public_url, max_seconds)| {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "pact_listening",
                        "address": listener.local_addr().ok().map(|address| address.to_string()),
                        "public_url": public_url,
                    })
                );
                let shutdown = Arc::clone(&pact_shutdown);
                tokio::spawn(aether::pact::server::serve(
                    listener,
                    started.gateway,
                    max_seconds,
                    async move { shutdown.notified().await },
                ))
            });
            let service_for_shutdown = Arc::clone(&service);
            let router =
                aether::transport::router(endpoint.clone(), move |peer, frames| {
                    service.handle(peer, frames)
                });
            let mut result = match tokio::signal::ctrl_c().await {
                Ok(()) => router.shutdown().await.map_err(|error| {
                    aether::transport::TransportError::Network(error.to_string())
                }),
                Err(error) => {
                    Err(aether::transport::TransportError::Network(error.to_string()))
                }
            };
            if let Some(mut server) = pact_server {
                // A turn in flight may wait for Codex; give it five seconds.
                pact_shutdown.notify_one();
                if tokio::time::timeout(std::time::Duration::from_secs(5), &mut server)
                    .await
                    .is_err()
                {
                    server.abort();
                }
            }
            if let Some((handle, server)) = admin_server {
                handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
                match server.await {
                    Ok(Err(error)) if result.is_ok() => {
                        result = Err(aether::transport::TransportError::Network(
                            error.to_string(),
                        ));
                    }
                    Err(_) if result.is_ok() => {
                        result = Err(aether::transport::TransportError::Network(
                            "administration server stopped".to_owned(),
                        ));
                    }
                    // A clean stop, or a failure after an earlier one.
                    _ => {}
                }
            }
            background.shutdown().await;
            outbox.abort();
            let _ = outbox.await;
            service_for_shutdown.shutdown();
            endpoint.close().await;
            match result {
                Ok(()) => {
                    println!("{}", serde_json::json!({ "event": "node_completed" }));
                    ExitCode::SUCCESS
                }
                Err(error) => transport_error(&error),
            }
        }
        Some(NodeCommand::Request(arguments)) => {
            let data_dir = data_dir_or_default(arguments.data_dir.clone());
            // A direct contact imported from an invite keeps its authorization
            // and relay; the flags, when given, still win.
            let bundle = match (arguments.scope.as_str(), arguments.agent_id.as_deref())
            {
                (aether::direct::SCOPE, Some(agent_id))
                    if arguments.endpoint_id.is_none()
                        && arguments.directory.is_none()
                        && arguments.managed_control.is_none() =>
                {
                    aether::direct::open_store(&data_dir)
                        .and_then(|store| {
                            aether::direct::contact_bundle(&store, agent_id)
                        })
                        .ok()
                }
                _ => None,
            };
            let authorization = match (&arguments.authorization, &bundle) {
                (Some(path), _) => {
                    if let Ok(authorization) = read_authorization(path) {
                        authorization
                    } else {
                        eprintln!("aether: request authorization is invalid");
                        return ExitCode::from(2);
                    }
                }
                (None, Some(bundle)) => bundle.authorization.clone(),
                (None, None) => {
                    eprintln!("aether: request authorization is required");
                    return ExitCode::from(2);
                }
            };
            let relay_url = arguments
                .relay_url
                .clone()
                .or_else(|| bundle.and_then(|bundle| bundle.relay_url));
            let fresh = aether::direct::RequestIds::fresh();
            let task_id = match arguments.task_id.clone() {
                Some(task_id) if uuid::Uuid::parse_str(&task_id).is_ok() => task_id,
                Some(_) => {
                    eprintln!("aether: request task id must be a UUID");
                    return ExitCode::from(2);
                }
                None => fresh.task_id,
            };
            let ids = aether::direct::RequestIds {
                task_id,
                idempotency_key: arguments
                    .idempotency_key
                    .clone()
                    .unwrap_or(fresh.idempotency_key),
            };
            let request = match aether::direct::request_frame(
                authorization,
                &arguments.scope,
                &arguments.text,
                ids,
            ) {
                Ok(request) => request,
                Err(error) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(2);
                }
            };
            let result = if let Some(control) = arguments.managed_control.as_deref() {
                managed_request(&arguments, &data_dir, control, request).await
            } else {
                match (
                    arguments.endpoint_id.as_deref(),
                    arguments.address.as_deref(),
                    arguments.directory.as_deref(),
                    arguments.directory_certificate.as_deref(),
                    arguments.agent_id.as_deref(),
                    arguments.agent_verifying_key.as_deref(),
                ) {
                    (Some(endpoint_id), Some(address), None, None, None, None) => {
                        match relay_url.as_deref() {
                            Some(relay_url) => {
                                aether::transport::request_once_via_relay(
                                    &data_dir,
                                    endpoint_id,
                                    relay_url,
                                    request,
                                )
                                .await
                            }
                            None => {
                                aether::transport::request_once(
                                    &data_dir,
                                    endpoint_id,
                                    address,
                                    request,
                                )
                                .await
                            }
                        }
                    }
                    (None, address, None, None, Some(agent_id), None)
                        if address.is_some() || relay_url.is_some() =>
                    {
                        let contact = if arguments.scope == aether::direct::SCOPE {
                            aether::direct::open_store(&data_dir).and_then(|store| {
                                aether::direct::contact(&store, agent_id)
                            })
                        } else {
                            Err(aether::direct::DirectError::UnknownContact)
                        };
                        let invitation = match contact {
                            Ok(invitation) => invitation,
                            Err(error) => {
                                eprintln!("aether: {error}");
                                return ExitCode::from(2);
                            }
                        };
                        aether::direct::send(
                            &data_dir,
                            &invitation,
                            address,
                            relay_url.as_deref(),
                            request,
                        )
                        .await
                    }
                    (
                        None,
                        None,
                        Some(directory),
                        Some(certificate),
                        Some(agent_id),
                        Some(key),
                    ) => {
                        let Ok(certificate) = std::fs::read(certificate) else {
                            eprintln!("aether: directory certificate cannot be read");
                            return ExitCode::from(2);
                        };
                        let Ok(trusted_key) = read_verifying_key(key) else {
                            eprintln!("aether: agent verification key is invalid");
                            return ExitCode::from(2);
                        };
                        let Ok(directory) = aether::directory_client::DirectoryClient::with_pinned_certificate(
                        directory,
                        &certificate,
                        trusted_key,
                    ) else {
                            eprintln!("aether: directory connection is invalid");
                            return ExitCode::from(2);
                        };
                        let record = match directory.resolve(agent_id).await.and_then(|wire| {
                            wire.to_record().map_err(|_| {
                                aether::directory_client::DirectoryClientError::InvalidRecord
                            })
                        }) {
                            Ok(record) if record.scope_id == arguments.scope => {
                                if cache_directory_record(&data_dir, &record).is_err() {
                                    eprintln!(
                                        "aether: local directory state is unavailable"
                                    );
                                    return ExitCode::from(1);
                                }
                                record
                            }
                            Ok(_) => {
                                eprintln!(
                                    "aether: directory record scope does not match"
                                );
                                return ExitCode::from(2);
                            }
                            Err(
                                aether::directory_client::DirectoryClientError::Request,
                            ) => {
                                if let Ok(record) = cached_directory_record(
                                    &data_dir,
                                    &arguments.scope,
                                    agent_id,
                                    trusted_key,
                                ) {
                                    record
                                } else {
                                    eprintln!(
                                        "aether: directory record cannot be resolved"
                                    );
                                    return ExitCode::from(2);
                                }
                            }
                            Err(_) => {
                                eprintln!(
                                    "aether: directory record cannot be resolved"
                                );
                                return ExitCode::from(2);
                            }
                        };
                        aether::transport::request_once_record(
                            &data_dir, &record, request,
                        )
                        .await
                    }
                    _ => {
                        eprintln!(
                            "aether: specify an endpoint and address, a direct agent and address, or directory settings"
                        );
                        return ExitCode::from(2);
                    }
                }
            };
            match result {
                Ok(response) => {
                    let exit_code = aether::transport::response_exit_code(&response);
                    println!(
                        "{}",
                        serde_json::json!({ "event": "node_completed", "response": response })
                    );
                    exit_code
                }
                Err(error) => transport_error(&error),
            }
        }
    }
}

async fn managed_request(
    arguments: &NodeRequest,
    data_dir: &std::path::Path,
    control: &str,
    request: aether::transport::Frame,
) -> Result<Vec<aether::transport::Frame>, aether::transport::TransportError> {
    // The usage spec rejects the other addressing flags with
    // `--managed-control`, and requires the certificate and the agent id.
    let certificate_path = arguments
        .managed_control_certificate
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    let agent_id = arguments
        .agent_id
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    let certificate = std::fs::read(certificate_path)
        .map_err(|_| aether::transport::TransportError::Address)?;
    let client = aether::managed_client::ManagedClient::with_pinned_certificate(
        control,
        &certificate,
    )
    .map_err(|_| aether::transport::TransportError::Address)?;
    let _ = sync_managed(data_dir, control, &arguments.scope, &client).await;
    let members = match client.members(&arguments.scope).await {
        Ok(members) => members,
        Err(aether::managed_client::ManagedClientError::Request) => {
            vec![cached_managed_member(data_dir, &arguments.scope, agent_id).map_err(
                |()| {
                    aether::transport::TransportError::Network(
                        "roster lookup failed".to_owned(),
                    )
                },
            )?]
        }
        Err(_) => {
            return Err(aether::transport::TransportError::Network(
                "roster lookup failed".to_owned(),
            ));
        }
    };
    let member = members
        .into_iter()
        .find(|member| aether::node_runtime::managed_member_active(member, agent_id))
        .ok_or(aether::transport::TransportError::Address)?;
    let relay_url = arguments
        .relay_url
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    aether::transport::request_once_via_relay(
        data_dir,
        &member.endpoint_id,
        relay_url,
        request,
    )
    .await
}

fn cached_managed_member(
    data_dir: &std::path::Path,
    scope_id: &str,
    agent_id: &str,
) -> Result<aether_store::ManagedMember, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    store
        .managed_members(scope_id)
        .map_err(|_| ())?
        .into_iter()
        .find(|member| aether::node_runtime::managed_member_active(member, agent_id))
        .ok_or(())
}

async fn start_directory_publication(
    arguments: &NodeServe,
    endpoint_id: String,
    bound_port: u16,
    configured_scope: &str,
) -> Result<
    Option<(
        aether::directory_publication::DirectoryPublication,
        aether::discovery::wire::WireRecord,
    )>,
    (),
> {
    let (
        Some(directory),
        Some(certificate),
        Some(agent_id),
        Some(address),
        Some(scope),
        Some(capability),
        Some(ttl),
    ) = (
        arguments.directory.as_ref(),
        arguments.directory_certificate.as_ref(),
        arguments.agent_id.as_ref(),
        arguments.address.as_ref(),
        arguments.scope.as_ref(),
        arguments.capability.as_ref(),
        arguments.directory_record_ttl,
    )
    else {
        if arguments.directory.is_none()
            && arguments.directory_certificate.is_none()
            && arguments.agent_id.is_none()
            && arguments.address.is_none()
            && arguments.scope.is_none()
            && arguments.capability.is_none()
            && arguments.directory_record_ttl.is_none()
        {
            return Ok(None);
        }
        return Err(());
    };
    if ttl < 2
        || agent_id.is_empty()
        || scope.is_empty()
        || capability.is_empty()
        || scope != configured_scope
    {
        return Err(());
    }
    let mut address = address.parse::<std::net::SocketAddr>().map_err(|_| ())?;
    if address.port() == 0 {
        address.set_port(bound_port);
    }
    let certificate = std::fs::read(certificate).map_err(|_| ())?;
    let publication = aether::directory_publication::DirectoryPublication::new(
        directory.clone(),
        &certificate,
        &arguments.data_dir,
        aether::directory_publication::PublicationRecord {
            agent_id: agent_id.clone(),
            address: address.to_string(),
            scope: scope.clone(),
            capability: capability.clone(),
            ttl,
        },
    )
    .map_err(|_| ())?;
    let record = publication.renew(&endpoint_id).await.map_err(|_| ())?;
    Ok(Some((publication, record)))
}

fn run_direct(arguments: NodeDirect) -> ExitCode {
    let data_dir = data_dir_or_default(arguments.data_dir);
    match arguments.command {
        NodeDirectCommand::Import { invitation } => {
            let source = if invitation.starts_with(aether::direct::INVITE_PREFIX) {
                invitation
            } else if let Ok(source) = std::fs::read_to_string(invitation) {
                source
            } else {
                eprintln!("aether: direct invitation cannot be read");
                return ExitCode::from(2);
            };
            // An invite line or a bundle carries the caller's authorization
            // too; a bare invitation file is the host's signed record only.
            let imported = match aether::direct::InvitationBundle::decode(&source) {
                Ok(bundle) => aether::direct::import_bundle(&data_dir, &bundle),
                Err(_) => match serde_json::from_str(&source) {
                    Ok(wire) => aether::direct::import_invitation(&data_dir, &wire),
                    Err(_) => Err(aether::direct::DirectError::Invalid),
                },
            };
            let invitation = match imported {
                Ok(invitation) => invitation,
                Err(
                    error @ (aether::direct::DirectError::Invalid
                    | aether::direct::DirectError::OtherEndpoint
                    | aether::direct::DirectError::NameInUse),
                ) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(2);
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(1);
                }
            };
            println!(
                "{}",
                serde_json::json!({
                    "event": "direct_contact_imported",
                    "agent": invitation.agent_id,
                    "endpoint": invitation.endpoint_id,
                })
            );
            ExitCode::SUCCESS
        }
        NodeDirectCommand::Remove { agent_id } => {
            match aether::direct::remove_contact(&data_dir, &agent_id) {
                Ok(()) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "direct_contact_removed",
                            "agent": agent_id,
                        })
                    );
                    ExitCode::SUCCESS
                }
                Err(error @ aether::direct::DirectError::UnknownContact) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(2)
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(1)
                }
            }
        }
        NodeDirectCommand::Resolve { agent_id } => {
            match aether::direct::open_store(&data_dir)
                .and_then(|store| aether::direct::contact(&store, &agent_id))
            {
                Ok(wire) => {
                    if let Ok(wire) = serde_json::to_string(&wire) {
                        println!("{wire}");
                        ExitCode::SUCCESS
                    } else {
                        eprintln!("aether: direct contact cannot be encoded");
                        ExitCode::from(1)
                    }
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn run_invite(arguments: &NodeInvite) -> ExitCode {
    match aether::direct::create_invitation(
        &arguments.data_dir,
        &arguments.agent_id,
        std::time::Duration::from_secs(arguments.expires_in),
    ) {
        Ok(invitation) => {
            if let Ok(invitation) = serde_json::to_string(&invitation) {
                println!("{invitation}");
                ExitCode::SUCCESS
            } else {
                eprintln!("aether: direct invitation cannot be encoded");
                ExitCode::from(1)
            }
        }
        Err(aether::direct::DirectError::Invalid) => {
            eprintln!("aether: direct invitation is invalid");
            ExitCode::from(2)
        }
        Err(_) => {
            eprintln!("aether: direct invitation is unavailable");
            ExitCode::from(1)
        }
    }
}

fn run_record(arguments: NodeRecord) -> ExitCode {
    if arguments.agent_id.is_empty()
        || arguments.scope.is_empty()
        || arguments.capability.is_empty()
        || arguments.expires_in == 0
        || arguments.address.parse::<std::net::SocketAddr>().is_err()
    {
        eprintln!("aether: node record is invalid");
        return ExitCode::from(2);
    }
    let Ok(endpoint) =
        aether::data_dir::DataDir::new(&arguments.data_dir).endpoint_key()
    else {
        eprintln!("aether: node record is unavailable");
        return ExitCode::from(1);
    };
    let Ok(wire) = aether::directory_publication::signed_node_record(
        &arguments.data_dir,
        &aether::directory_publication::PublicationRecord {
            agent_id: arguments.agent_id,
            address: arguments.address,
            scope: arguments.scope,
            capability: arguments.capability,
            ttl: arguments.expires_in,
        },
        &endpoint.public().to_string(),
    ) else {
        eprintln!("aether: node record is unavailable");
        return ExitCode::from(1);
    };
    if let Ok(wire) = serde_json::to_string(&wire) {
        println!("{wire}");
        ExitCode::SUCCESS
    } else {
        eprintln!("aether: node record cannot be encoded");
        ExitCode::from(1)
    }
}

async fn run_managed(arguments: NodeManaged) -> ExitCode {
    let Ok(certificate) = std::fs::read(&arguments.control_certificate) else {
        eprintln!("aether: control certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(client) = aether::managed_client::ManagedClient::with_pinned_certificate(
        &arguments.control,
        &certificate,
    ) else {
        eprintln!("aether: managed control connection is invalid");
        return ExitCode::from(2);
    };
    let result = match arguments.command {
        NodeManagedCommand::Redeem { token_file, record } => {
            let token = match std::fs::read_to_string(token_file) {
                Ok(token) if !token.trim().is_empty() => token,
                _ => {
                    eprintln!("aether: managed invitation token cannot be read");
                    return ExitCode::from(2);
                }
            };
            if let Ok(record) = read_directory_record(&record) {
                client.redeem(token.trim(), record).await
            } else {
                eprintln!("aether: managed member record is invalid");
                return ExitCode::from(2);
            }
        }
        NodeManagedCommand::Members { scope } => match client.members(&scope).await {
            Ok(members) => match serde_json::to_string(&members) {
                Ok(members) => {
                    println!("{members}");
                    return ExitCode::SUCCESS;
                }
                Err(_) => {
                    Err(aether::managed_client::ManagedClientError::InvalidRecord)
                }
            },
            Err(error) => Err(error),
        },
        NodeManagedCommand::Events { scope, after } => {
            match client.events_after(&scope, after.unwrap_or_default()).await {
                Ok(events) => match serde_json::to_string(&events) {
                    Ok(events) => {
                        println!("{events}");
                        return ExitCode::SUCCESS;
                    }
                    Err(_) => {
                        Err(aether::managed_client::ManagedClientError::InvalidRecord)
                    }
                },
                Err(error) => Err(error),
            }
        }
        NodeManagedCommand::Sync { scope, data_dir } => {
            if let Ok((cursor, changes)) =
                sync_managed(&data_dir, &arguments.control, &scope, &client).await
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "managed_roster_synchronized",
                        "cursor": cursor,
                        "changes": changes,
                    })
                );
                return ExitCode::SUCCESS;
            }
            eprintln!("aether: managed roster synchronization failed");
            return ExitCode::from(1);
        }
    };
    if let Ok(member) = result {
        if let Ok(member) = serde_json::to_string(&member) {
            println!("{member}");
            ExitCode::SUCCESS
        } else {
            eprintln!("aether: managed member cannot be encoded");
            ExitCode::from(1)
        }
    } else {
        eprintln!("aether: managed control request failed");
        ExitCode::from(1)
    }
}

async fn sync_managed(
    data_dir: &std::path::Path,
    control: &str,
    scope_id: &str,
    client: &aether::managed_client::ManagedClient,
) -> Result<(i64, usize), ()> {
    std::fs::create_dir_all(data_dir).map_err(|_| ())?;
    let mut store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let mut cursor = store.managed_cursor(control, scope_id).map_err(|_| ())?;
    let mut changes = 0usize;
    loop {
        let events = client.events_after(scope_id, cursor).await.map_err(|_| ())?;
        if events.is_empty() {
            return Ok((cursor, changes));
        }
        for event in events {
            if event.cursor <= cursor {
                return Err(());
            }
            match event.event {
                aether_store::ManagedEventType::Joined => {
                    let member = event.member.ok_or(())?;
                    store
                        .apply_managed_member(control, event.cursor, &member)
                        .map_err(|_| ())?;
                }
                aether_store::ManagedEventType::Revoked if event.member.is_none() => {
                    store
                        .revoke_managed_member(
                            control,
                            event.cursor,
                            &event.scope_id,
                            &event.agent_id,
                        )
                        .map_err(|_| ())?;
                }
                aether_store::ManagedEventType::Revoked => return Err(()),
            }
            cursor = event.cursor;
            changes = changes.checked_add(1).ok_or(())?;
        }
    }
}

async fn run_directory(arguments: NodeDirectory) -> ExitCode {
    let Ok(certificate) = std::fs::read(&arguments.directory_certificate) else {
        eprintln!("aether: directory certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(verifying_key) = read_verifying_key(&arguments.agent_verifying_key) else {
        eprintln!("aether: agent verification key is invalid");
        return ExitCode::from(2);
    };
    let Ok(client) = aether::directory_client::DirectoryClient::with_pinned_certificate(
        &arguments.directory,
        &certificate,
        verifying_key,
    ) else {
        eprintln!("aether: directory connection is invalid");
        return ExitCode::from(2);
    };
    let result = match arguments.command {
        NodeDirectoryCommand::Publish { record }
        | NodeDirectoryCommand::Renew { record } => {
            if let Ok(record) = read_directory_record(&record) {
                client.publish(&record).await.map(Some)
            } else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            }
        }
        NodeDirectoryCommand::Resolve { agent_id } => {
            client.resolve(&agent_id).await.map(Some)
        }
        NodeDirectoryCommand::Search { capability } => {
            match client.search(&capability).await {
                Ok(records) => match serde_json::to_string(&records) {
                    Ok(records) => {
                        println!("{records}");
                        return ExitCode::SUCCESS;
                    }
                    Err(_) => Err(
                        aether::directory_client::DirectoryClientError::InvalidRecord,
                    ),
                },
                Err(error) => Err(error),
            }
        }
        NodeDirectoryCommand::Revoke { record } => {
            if let Ok(record) = read_directory_record(&record) {
                match client.revoke(&record).await {
                    Ok(()) => {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "directory_record_revoked" })
                        );
                        return ExitCode::SUCCESS;
                    }
                    Err(error) => Err(error),
                }
            } else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            }
        }
        NodeDirectoryCommand::Sync { data_dir } => {
            if let Ok((cursor, records)) =
                sync_directory(&data_dir, &arguments.directory, verifying_key, &client)
                    .await
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "directory_synchronized",
                        "cursor": cursor,
                        "records": records,
                    })
                );
                return ExitCode::SUCCESS;
            }
            eprintln!("aether: directory synchronization failed");
            return ExitCode::from(1);
        }
    };
    match result {
        Ok(Some(record)) => {
            if let Ok(record) = serde_json::to_string(&record) {
                println!("{record}");
                ExitCode::SUCCESS
            } else {
                eprintln!("aether: directory response is invalid");
                ExitCode::from(1)
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("aether: directory operation failed");
            ExitCode::from(1)
        }
    }
}

async fn sync_directory(
    data_dir: &std::path::Path,
    directory: &str,
    trusted_key: [u8; 32],
    client: &aether::directory_client::DirectoryClient,
) -> Result<(i64, usize), ()> {
    std::fs::create_dir_all(data_dir).map_err(|_| ())?;
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let mut cursor = store.directory_cursor(directory, &trusted_key).map_err(|_| ())?;
    let mut records = 0usize;
    loop {
        let changes = client.events_after(cursor).await.map_err(|_| ())?;
        if changes.is_empty() {
            return Ok((cursor, records));
        }
        for change in changes {
            if change.cursor <= cursor {
                return Err(());
            }
            match change.event {
                aether_store::DirectoryEventType::Published
                | aether_store::DirectoryEventType::Renewed => {
                    let record = change.record.ok_or(())?;
                    cache_wire_directory_record(&store, &record)?;
                    records = records.checked_add(1).ok_or(())?;
                }
                aether_store::DirectoryEventType::Revoked => {
                    store
                        .mark_directory_record_stale(&change.scope_id, &change.agent_id)
                        .map_err(|_| ())?;
                }
            }
            cursor = change.cursor;
            store
                .advance_directory_cursor(directory, &trusted_key, cursor)
                .map_err(|_| ())?;
        }
    }
}

fn read_authorization(
    path: &std::path::Path,
) -> Result<aether::transport::A2aAuthorization, ()> {
    let source = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&source).map_err(|_| ())
}

fn read_verifying_key(path: &std::path::Path) -> Result<[u8; 32], ()> {
    let source = std::fs::read_to_string(path).map_err(|_| ())?;
    <[u8; 32]>::from_hex(source.trim()).map_err(|_| ())
}

fn read_directory_record(
    path: &std::path::Path,
) -> Result<aether::discovery::wire::WireRecord, ()> {
    serde_json::from_slice(&std::fs::read(path).map_err(|_| ())?).map_err(|_| ())
}

fn cache_directory_record(
    data_dir: &std::path::Path,
    record: &aether::discovery::Record,
) -> Result<(), ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    cache_wire_directory_record(
        &store,
        &aether::discovery::wire::WireRecord::from_record(record),
    )
}

fn cache_wire_directory_record(
    store: &aether_store::NodeStore,
    record: &aether::discovery::wire::WireRecord,
) -> Result<(), ()> {
    store
        .cache_directory_record(&aether_store::SignedRecord {
            scope_id: record.scope_id.clone(),
            agent_id: record.agent_id.clone(),
            record: serde_json::to_vec(record).map_err(|_| ())?,
            expires_at: i64::try_from(record.expires_at).map_err(|_| ())?,
        })
        .map_err(|_| ())
}

fn cached_directory_record(
    data_dir: &std::path::Path,
    scope_id: &str,
    agent_id: &str,
    trusted_key: [u8; 32],
) -> Result<aether::discovery::Record, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let stored = store.directory_record(scope_id, agent_id).map_err(|_| ())?;
    if stored.scope_id != scope_id || stored.agent_id != agent_id {
        return Err(());
    }
    if stored.expires_at <= aether_core::time::now() {
        return Err(());
    }
    let wire: aether::discovery::wire::WireRecord =
        serde_json::from_slice(&stored.record).map_err(|_| ())?;
    let record = wire.to_record().map_err(|_| ())?;
    if record.signing_key != trusted_key {
        return Err(());
    }
    aether::discovery::validate_record(&record).map_err(|_| ())?;
    Ok(record)
}

fn transport_error(error: &aether::transport::TransportError) -> ExitCode {
    eprintln!("aether: transport failed: {error:?}");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use super::{
        NodeServe, cache_directory_record, cached_directory_record,
        cached_managed_member, start_directory_publication, sync_managed,
    };
    use axum_server::Handle;
    use ed25519_dalek::SigningKey;

    #[tokio::test]
    async fn live_publication_rejects_a_scope_outside_the_node_policy() {
        let arguments = NodeServe {
            policy: PathBuf::from("policy.json"),
            data_dir: PathBuf::from("node"),
            port: 0,
            admin_port: None,
            pact_listen: None,
            managed_control: None,
            managed_control_certificate: None,
            adapter: None,
            codex_bin: PathBuf::from("codex"),
            relay_url: None,
            hub_url: None,
            directory: Some("https://directory.test".to_owned()),
            directory_certificate: Some(PathBuf::from("certificate.der")),
            agent_id: Some("agent".to_owned()),
            address: Some("127.0.0.1:1".to_owned()),
            scope: Some("other".to_owned()),
            capability: Some("review".to_owned()),
            directory_record_ttl: Some(60),
            invite_name: None,
        };
        assert!(
            start_directory_publication(
                &arguments,
                "endpoint".to_owned(),
                1,
                "directory"
            )
            .await
            .is_err()
        );
    }

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
