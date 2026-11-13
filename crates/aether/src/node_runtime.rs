use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use aether_core::ScopeKind;

use crate::{
    admin::SharedAdministration,
    admin_ui::{PageContext, RelaySwitcher, UiOptions},
    directory_publication::DirectoryPublication,
    hub_client::NodeHub,
    node_serve_context::NodeServeContext,
    node_service::RunningNode,
    policy::AgentAdapter,
    transport,
};

#[derive(Debug, Clone)]
pub struct NodeServeOptions {
    pub policy: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
    pub admin_port: Option<u16>,
    pub pact_listen: Option<String>,
    pub managed_control: Option<String>,
    pub managed_control_certificate: Option<PathBuf>,
    pub adapter: Option<crate::policy::AgentAdapter>,
    pub codex_bin: PathBuf,
    pub relay_url: Option<String>,
    pub hub_url: Option<String>,
    pub publication: PublicationFlags,
    pub invite_name: Option<String>,
}

#[derive(Debug)]
pub enum NodeRunError {
    Usage(String),
    Failed(String),
    Transport(transport::TransportError),
}

impl NodeRunError {
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::from(2),
            Self::Failed(_) | Self::Transport(_) => ExitCode::from(1),
        }
    }
}

pub struct BackgroundWorkers {
    directory_renewer: Option<tokio::task::JoinHandle<()>>,
    managed_renewer: Option<tokio::task::JoinHandle<()>>,
    hub_host: tokio::task::JoinHandle<()>,
    hub_caller: Option<tokio::task::JoinHandle<()>>,
    listing: tokio::task::JoinHandle<()>,
}

impl BackgroundWorkers {
    pub async fn shutdown(self) {
        if let Some(handle) = self.directory_renewer {
            stop(handle, "directory_renewer_join_failed").await;
        }
        if let Some(handle) = self.managed_renewer {
            stop(handle, "managed_renewer_join_failed").await;
        }
        for handle in [Some(self.hub_host), self.hub_caller, Some(self.listing)]
            .into_iter()
            .flatten()
        {
            stop(handle, "hub_worker_join_failed").await;
        }
    }
}

/// Prints the error log line `event`, with `error` when there is one.
fn log_error(event: &str, error: Option<&dyn std::fmt::Display>) {
    let line = match error {
        Some(error) => serde_json::json!({
            "level": "error",
            "event": event,
            "error": error.to_string(),
        }),
        None => serde_json::json!({ "level": "error", "event": event }),
    };
    eprintln!("{line}");
}

/// Aborts the task of `handle` and waits for it; a failure other than the
/// cancellation is logged as `event`.
async fn stop(handle: tokio::task::JoinHandle<()>, event: &str) {
    handle.abort();
    if let Err(error) = handle.await
        && !error.is_cancelled()
    {
        log_error(event, Some(&error));
    }
}

pub fn start_background_workers(
    administration: &SharedAdministration,
    ctx: &Arc<NodeServeContext>,
    directory_publication: Option<Arc<DirectoryPublication>>,
    endpoint_id: String,
    managed_sync: Option<(Arc<crate::managed_client::ManagedClient>, String, String)>,
    data_dir: &Path,
) -> BackgroundWorkers {
    let directory_renewer = directory_publication.map(|publication| {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(
                    publication.ttl() / 2,
                ))
                .await;
                if let Err(error) = publication.renew(&endpoint_id).await {
                    log_error("directory_renew_failed", Some(&error));
                }
            }
        })
    });
    let managed_renewer = managed_sync.map(|(client, control, scope_id)| {
        let data_dir = data_dir.to_path_buf();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if crate::managed_client::sync_managed(
                    &data_dir, &control, &scope_id, &client,
                )
                .await
                .is_err()
                {
                    log_error("managed_sync_failed", None);
                }
            }
        })
    });
    let hub_host = tokio::spawn(crate::hub_workers::host_worker(
        Arc::clone(administration),
        Arc::clone(ctx),
    ));
    let hub_caller = ctx
        .caller
        .then(|| tokio::spawn(crate::hub_workers::caller_worker(Arc::clone(ctx))));
    let listing = tokio::spawn(crate::listing::listing_renewal_worker(Arc::clone(ctx)));
    BackgroundWorkers {
        directory_renewer,
        managed_renewer,
        hub_host,
        hub_caller,
        listing,
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run(options: NodeServeOptions) -> Result<(), NodeRunError> {
    if options
        .invite_name
        .as_deref()
        .is_some_and(|name| !crate::direct::valid_name(name))
    {
        return Err(NodeRunError::Usage(
            "--invite-name must be a valid direct contact name (1 to 64 letters, digits, dots, underscores or dashes, or a hub full name with #)".to_owned(),
        ));
    }
    let service = Arc::new(
        crate::node_builder::load_or_create_service(
            &options.policy,
            &options.data_dir,
            options.codex_bin.clone(),
            options.adapter,
        )
        .map_err(|error| NodeRunError::Usage(error.to_string()))?,
    );
    match service.agent_adapter() {
        AgentAdapter::Codex => {
            let codex_home =
                crate::data_dir::DataDir::new(&options.data_dir).codex_home();
            if !crate::data_dir::codex_login(&codex_home).exists() {
                eprintln!(
                    "aether: Codex is not logged in for this node, so tasks will fail. Run: CODEX_HOME={} codex login --device-auth",
                    codex_home.display()
                );
            }
        }
        AgentAdapter::Connected => eprintln!(
            "aether: tasks go to the agent that connects to {}",
            crate::data_dir::DataDir::new(&options.data_dir).agent_socket().display()
        ),
    }
    let hub = crate::hub_client::configured_hub_client(
        options.hub_url.as_deref(),
        &options.data_dir,
    )
    .map_err(|error| NodeRunError::Usage(error.to_string()))?;
    let pact = match (options.pact_listen.as_deref(), service.pact_config()) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(NodeRunError::Usage(
                "--pact-listen needs a pact section in the policy".to_owned(),
            ));
        }
        (Some(address), Some(config)) => {
            let listener =
                tokio::net::TcpListener::bind(address).await.map_err(|error| {
                    NodeRunError::Failed(format!(
                        "the PACT gateway cannot listen: {error}"
                    ))
                })?;
            let node: Arc<dyn crate::pact::gateway::PactNode> = service.clone();
            match crate::pact::server::start(config, &options.data_dir, node) {
                Ok(started) => Some((
                    listener,
                    started,
                    config.public_url.clone(),
                    config.max_seconds,
                )),
                Err(error) => {
                    return Err(NodeRunError::Failed(format!(
                        "the PACT gateway cannot start: {error}"
                    )));
                }
            }
        }
    };
    let managed_scope = service.has_scope_kind(ScopeKind::Managed);
    let managed_sync = match options
        .managed_control
        .as_deref()
        .zip(options.managed_control_certificate.as_deref())
    {
        None => None,
        Some((control, certificate_path)) => {
            let Some(scope_id) =
                service.scope_id_for_kind(ScopeKind::Managed).map(str::to_owned)
            else {
                return Err(NodeRunError::Usage(
                    "managed control requires exactly one managed node scope"
                        .to_owned(),
                ));
            };
            let certificate = std::fs::read(certificate_path).map_err(|_| {
                NodeRunError::Usage(
                    "managed control certificate cannot be read".to_owned(),
                )
            })?;
            let client = crate::managed_client::ManagedClient::with_pinned_certificate(
                control,
                &certificate,
            )
            .map_err(|_| {
                NodeRunError::Usage("managed control connection is invalid".to_owned())
            })?;
            if crate::managed_client::sync_managed(
                &options.data_dir,
                control,
                &scope_id,
                &client,
            )
            .await
            .is_err()
            {
                return Err(NodeRunError::Failed(
                    "managed roster synchronization failed".to_owned(),
                ));
            }
            Some((client, control.to_owned(), scope_id))
        }
    };
    let administration = aether_store::NodeStore::in_dir(&options.data_dir)
        .map(crate::admin::shared_with_store)
        .map_err(|error| {
            NodeRunError::Failed(format!("administration failed: {error}"))
        })?;
    let socket = crate::data_dir::DataDir::new(&options.data_dir).admin_socket();
    let live =
        Arc::new(RunningNode::new(Arc::clone(&service), options.data_dir.clone()));
    if let Ok(mut administration) = administration.lock() {
        administration.set_live(live.clone());
    } else {
        return Err(NodeRunError::Failed(
            "administration failed: service is unavailable".to_owned(),
        ));
    }
    crate::admin_socket::start(&socket, administration.clone()).map_err(|error| {
        NodeRunError::Failed(format!("administration failed: {error}"))
    })?;
    let relay_url = options
        .relay_url
        .clone()
        .or_else(|| {
            crate::settings::setting::<crate::transport::relay::Relay>(
                &options.data_dir,
            )
            .ok()
            .flatten()
        })
        .filter(|relay| relay != crate::transport::relay::RELAY_OFF);
    let publication_scope =
        options.publication.scope.as_deref().unwrap_or_else(|| service.scope_id());
    if options.publication.directory.is_some()
        && service.scope_kind_for(publication_scope) != Some(ScopeKind::Directory)
    {
        return Err(NodeRunError::Usage(
            "directory publication requires a directory node scope".to_owned(),
        ));
    }
    let publication = options.publication.complete()?;
    let endpoint =
        transport::bind_node(&options.data_dir, options.port, relay_url.as_deref())
            .await
            .map_err(NodeRunError::Transport)?;
    let bound_port = endpoint
        .addr()
        .ip_addrs()
        .next()
        .map_or(options.port, std::net::SocketAddr::port);
    let started = async {
        store_node_settings(&options, relay_url.as_ref())?;
        live.attach_endpoint(endpoint.clone(), tokio::runtime::Handle::current());
        publish(
            publication,
            &options.data_dir,
            endpoint.id().to_string(),
            bound_port,
            publication_scope,
        )
        .await
    }
    .await;
    let directory_publication = match started {
        Ok(directory_publication) => directory_publication,
        Err(error) => {
            endpoint.close().await;
            return Err(error);
        }
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
        live.attach_directory(Arc::clone(publication));
    }
    let outbox_wake = Arc::new(tokio::sync::Notify::new());
    live.attach_outbox(Arc::clone(&outbox_wake));
    let outbox = tokio::spawn(crate::outbox::run(
        options.data_dir.clone(),
        Arc::new(crate::outbox::RelayCourier::shared(endpoint.clone())),
        options.hub_url.clone(),
        outbox_wake,
    ));
    let relay_switcher: RelaySwitcher = Arc::new({
        let endpoint = endpoint.clone();
        move |previous, relay| {
            let endpoint = endpoint.clone();
            Box::pin(async move {
                transport::switch_relay(
                    &endpoint,
                    previous.as_deref(),
                    relay.as_deref(),
                )
                .await
            })
        }
    });
    let node_hub =
        Arc::new(NodeHub::new(options.data_dir.clone(), options.hub_url.clone()));
    if let Some(client) = hub {
        let _ = node_hub.prime(client);
    }
    let hub_sign = Arc::new(std::sync::Mutex::new(()));
    let worker_ctx = Arc::new(NodeServeContext {
        data_dir: options.data_dir.clone(),
        hub: Arc::clone(&node_hub),
        hub_sign: Arc::clone(&hub_sign),
        caller: true,
    });
    let page = Arc::new(
        PageContext::host(
            options.data_dir.clone(),
            options.invite_name.clone(),
            relay_url.clone(),
            Arc::clone(&relay_switcher),
            Arc::clone(&node_hub),
            Arc::clone(&hub_sign),
        )
        .with_caller(true)
        .with_task_folder_mode(service.task_folder_mode()),
    );
    let background = start_background_workers(
        &administration,
        &worker_ctx,
        directory_publication.as_ref().map(|(publication, _)| Arc::clone(publication)),
        endpoint.id().to_string(),
        managed_sync
            .map(|(client, control, scope_id)| (Arc::new(client), control, scope_id)),
        &options.data_dir,
    );
    let admin_server = options.admin_port.map(|admin_port| {
        let handle = axum_server::Handle::new();
        let data_dir = options.data_dir.clone();
        let administration = administration.clone();
        let shutdown = handle.clone();
        let ui_options = UiOptions {
            managed: managed_scope,
            page: Some(page),
            pact: pact.as_ref().map(|(_, started, _, _)| crate::admin_ui::PactUi {
                consent: Arc::clone(&started.consent),
                link: Arc::clone(&started.link),
            }),
        };
        let server = tokio::spawn(async move {
            crate::admin_ui::serve_tls_with_scope_and_handle(
                admin_port,
                &data_dir,
                administration,
                ui_options,
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
        tokio::spawn(crate::pact::server::serve(
            listener,
            started.gateway,
            max_seconds,
            async move { shutdown.notified().await },
        ))
    });
    let service_for_shutdown = Arc::clone(&service);
    let router = transport::router(endpoint.clone(), move |peer, frames| {
        service.a2a().handle(peer, frames)
    });
    let mut result = match shutdown_signal().await {
        Ok(()) => router
            .shutdown()
            .await
            .map_err(|error| transport::TransportError::Network(error.to_string())),
        Err(error) => Err(transport::TransportError::Network(error.to_string())),
    };
    if let Some(mut server) = pact_server {
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
                result = Err(transport::TransportError::Network(error.to_string()));
            }
            Err(_) if result.is_ok() => {
                result = Err(transport::TransportError::Network(
                    "administration server stopped".to_owned(),
                ));
            }
            _ => {}
        }
    }
    background.shutdown().await;
    stop(outbox, "outbox_join_failed").await;
    service_for_shutdown.shutdown();
    endpoint.close().await;
    result.map_err(NodeRunError::Transport)?;
    println!("{}", serde_json::json!({ "event": "node_completed" }));
    Ok(())
}

/// Keeps the relay this node uses and the name its connect codes carry.
fn store_node_settings(
    options: &NodeServeOptions,
    relay_url: Option<&String>,
) -> Result<(), NodeRunError> {
    crate::settings::store_setting::<crate::transport::relay::ActiveRelay>(
        &options.data_dir,
        &relay_url.cloned(),
    )
    .and_then(|()| {
        crate::settings::store_setting::<crate::direct::HostName>(
            &options.data_dir,
            &options.invite_name.clone().unwrap_or_else(|| {
                crate::direct::default_agent_name(
                    &crate::direct::endpoint_id(&options.data_dir).unwrap_or_default(),
                )
            }),
        )
    })
    .map_err(|_| NodeRunError::Failed("node settings cannot be stored".to_owned()))
}

/// The flags of a directory record publication. A node publishes its record
/// when every flag is set; setting only some of them is an error.
#[derive(Debug, Clone, Default)]
pub struct PublicationFlags {
    pub directory: Option<String>,
    pub directory_certificate: Option<PathBuf>,
    pub agent_id: Option<String>,
    pub address: Option<String>,
    pub scope: Option<String>,
    pub capability: Option<String>,
    pub directory_record_ttl: Option<u64>,
}

/// The settings of a directory record publication: [`PublicationFlags`]
/// with every flag set.
struct Publication<'a> {
    directory: &'a str,
    certificate: &'a Path,
    agent_id: &'a str,
    address: &'a str,
    scope: &'a str,
    capability: &'a str,
    ttl: u64,
}

fn publication_unavailable() -> NodeRunError {
    NodeRunError::Failed("directory publication is unavailable".to_owned())
}

impl PublicationFlags {
    /// The publication these flags ask for: `None` when no flag is set.
    fn complete(&self) -> Result<Option<Publication<'_>>, NodeRunError> {
        match (
            self.directory.as_deref(),
            self.directory_certificate.as_deref(),
            self.agent_id.as_deref(),
            self.address.as_deref(),
            self.scope.as_deref(),
            self.capability.as_deref(),
            self.directory_record_ttl,
        ) {
            (
                Some(directory),
                Some(certificate),
                Some(agent_id),
                Some(address),
                Some(scope),
                Some(capability),
                Some(ttl),
            ) => Ok(Some(Publication {
                directory,
                certificate,
                agent_id,
                address,
                scope,
                capability,
                ttl,
            })),
            (None, None, None, None, None, None, None) => Ok(None),
            _ => Err(publication_unavailable()),
        }
    }
}

/// Publishes the record of `publication`, when there is one, and returns it
/// with the publication that renews it.
async fn publish(
    publication: Option<Publication<'_>>,
    data_dir: &Path,
    endpoint_id: String,
    bound_port: u16,
    configured_scope: &str,
) -> Result<
    Option<(DirectoryPublication, crate::discovery::wire::WireRecord)>,
    NodeRunError,
> {
    let Some(publication) = publication else {
        return Ok(None);
    };
    publish_record(publication, data_dir, endpoint_id, bound_port, configured_scope)
        .await
        .map(Some)
        .ok_or_else(publication_unavailable)
}

async fn publish_record(
    publication: Publication<'_>,
    data_dir: &Path,
    endpoint_id: String,
    bound_port: u16,
    configured_scope: &str,
) -> Option<(DirectoryPublication, crate::discovery::wire::WireRecord)> {
    if !crate::directory_publication::valid_publication_fields(
        publication.agent_id,
        publication.address,
        publication.scope,
        publication.capability,
        publication.ttl,
        configured_scope,
    ) {
        return None;
    }
    let mut address = publication.address.parse::<std::net::SocketAddr>().ok()?;
    if address.port() == 0 {
        address.set_port(bound_port);
    }
    let certificate = std::fs::read(publication.certificate).ok()?;
    let directory_publication = DirectoryPublication::new(
        publication.directory.to_owned(),
        &certificate,
        data_dir,
        crate::directory_publication::PublicationRecord {
            agent_id: publication.agent_id.to_owned(),
            address: address.to_string(),
            scope: publication.scope.to_owned(),
            capability: publication.capability.to_owned(),
            ttl: publication.ttl,
        },
    )
    .ok()?;
    let record = directory_publication.renew(&endpoint_id).await.ok()?;
    Some((directory_publication, record))
}

/// Makes `ring` the process-wide rustls crypto provider, once.
///
/// Without a default, reqwest builds its TLS client on `aws-lc-rs`, whose
/// first `RAND_bytes` call (the `ClientHello` random) seeds a jitter-entropy
/// source that costs about 100 ms of CPU. `ring` draws from the operating
/// system's generator, as `iroh` and the control service already do. A
/// default that is already installed stays.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Resolves when the process is asked to stop: Ctrl-C everywhere, and on
/// unix also SIGTERM, which `docker stop` and systemd send. An error means
/// a signal handler could not be installed.
///
/// # Errors
/// Returns the I/O error from installing the signal handler.
pub async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::sync::Mutex;

    use super::{PublicationFlags, publish, start_background_workers};
    use crate::{
        admin::test_support::shared_in, hub_client::NodeHub,
        node_serve_context::NodeServeContext,
    };

    #[tokio::test]
    async fn hub_sync_and_listing_renewal_start_without_the_admin_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        let ctx = Arc::new(NodeServeContext {
            data_dir: directory.path().to_path_buf(),
            hub: Arc::new(NodeHub::new(directory.path().to_path_buf(), None)),
            hub_sign: Arc::new(Mutex::new(())),
            caller: false,
        });
        let workers = start_background_workers(
            &administration,
            &ctx,
            None,
            "endpoint".to_owned(),
            None,
            directory.path(),
        );
        workers.shutdown().await;
    }

    #[tokio::test]
    async fn live_publication_rejects_a_scope_outside_the_node_policy() {
        let flags = PublicationFlags {
            directory: Some("https://directory.test".to_owned()),
            directory_certificate: Some("certificate.der".into()),
            agent_id: Some("agent".to_owned()),
            address: Some("127.0.0.1:1".to_owned()),
            scope: Some("other".to_owned()),
            capability: Some("review".to_owned()),
            directory_record_ttl: Some(60),
        };
        let publication = flags.complete().expect("every flag is set");
        assert!(
            publish(
                publication,
                std::path::Path::new("node"),
                "endpoint".to_owned(),
                1,
                "directory"
            )
            .await
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_ends_the_wait_for_a_shutdown_signal() {
        let signal = super::shutdown_signal();
        tokio::pin!(signal);
        // The first poll installs the handler; only then is SIGTERM safe to
        // send to this process.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut signal)
                .await
                .is_err()
        );
        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .expect("kill must run");
        assert!(status.success());
        tokio::time::timeout(std::time::Duration::from_secs(5), signal)
            .await
            .expect("SIGTERM must end the wait")
            .expect("the handler must install");
    }

    #[test]
    fn publication_flags_must_be_all_set_or_all_unset() {
        assert!(matches!(PublicationFlags::default().complete(), Ok(None)));
        let some = PublicationFlags {
            directory: Some("https://directory.test".to_owned()),
            ..PublicationFlags::default()
        };
        assert!(some.complete().is_err());
    }
}
