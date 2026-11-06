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
    hub_client::HubClient,
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
    pub directory: Option<String>,
    pub directory_certificate: Option<PathBuf>,
    pub agent_id: Option<String>,
    pub address: Option<String>,
    pub scope: Option<String>,
    pub capability: Option<String>,
    pub directory_record_ttl: Option<u64>,
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
    hub_host: Option<tokio::task::JoinHandle<()>>,
    hub_caller: Option<tokio::task::JoinHandle<()>>,
    listing: Option<tokio::task::JoinHandle<()>>,
}

impl BackgroundWorkers {
    pub async fn shutdown(self) {
        if let Some(handle) = self.directory_renewer {
            handle.abort();
            if let Err(error) = handle.await
                && !error.is_cancelled()
            {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "error",
                        "event": "directory_renewer_join_failed",
                        "error": error.to_string(),
                    })
                );
            }
        }
        if let Some(handle) = self.managed_renewer {
            handle.abort();
            if let Err(error) = handle.await
                && !error.is_cancelled()
            {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "error",
                        "event": "managed_renewer_join_failed",
                        "error": error.to_string(),
                    })
                );
            }
        }
        for handle in
            [self.hub_host, self.hub_caller, self.listing].into_iter().flatten()
        {
            handle.abort();
            if let Err(error) = handle.await
                && !error.is_cancelled()
            {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "level": "error",
                        "event": "hub_worker_join_failed",
                        "error": error.to_string(),
                    })
                );
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn hub_sync_started(&self) -> bool {
        self.hub_host.is_some()
    }

    #[cfg(test)]
    #[must_use]
    pub const fn listing_renewal_started(&self) -> bool {
        self.listing.is_some()
    }
}

pub fn hub_client(
    hub_url: Option<&str>,
    data_dir: &Path,
) -> Result<Option<HubClient>, NodeRunError> {
    crate::hub_client::configured_hub_client(hub_url, data_dir)
        .map_err(|error| NodeRunError::Usage(error.to_string()))
}

pub fn start_background_workers(
    administration: &SharedAdministration,
    page: &Arc<PageContext>,
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
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "level": "error",
                            "event": "directory_renew_failed",
                            "error": error.to_string(),
                        })
                    );
                }
            }
        })
    });
    let managed_renewer = managed_sync.map(|(client, control, scope_id)| {
        let data_dir = data_dir.to_path_buf();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if sync_managed(&data_dir, &control, &scope_id, &client).await.is_err()
                {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "level": "error",
                            "event": "managed_sync_failed",
                        })
                    );
                }
            }
        })
    });
    let hub_host = tokio::spawn(crate::admin_ui::hub::host_worker(
        Arc::clone(administration),
        Arc::clone(page),
    ));
    let hub_caller = page
        .caller
        .then(|| tokio::spawn(crate::admin_ui::hub::caller_worker(Arc::clone(page))));
    let listing =
        tokio::spawn(crate::admin_ui::listing::listing_worker(Arc::clone(page)));
    BackgroundWorkers {
        directory_renewer,
        managed_renewer,
        hub_host: Some(hub_host),
        hub_caller,
        listing: Some(listing),
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run(options: NodeServeOptions) -> Result<(), NodeRunError> {
    if options
        .invite_name
        .as_deref()
        .is_some_and(|name| !crate::hub_client::valid_name_part(name))
    {
        return Err(NodeRunError::Usage(
            "--invite-name must be 1 to 32 lowercase letters, digits, dots, underscores or dashes, starting with a letter or digit".to_owned(),
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
    let hub = hub_client(options.hub_url.as_deref(), &options.data_dir)?;
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
            if sync_managed(&options.data_dir, control, &scope_id, &client)
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
        .filter(|relay| relay != "off");
    let endpoint =
        transport::bind_node(&options.data_dir, options.port, relay_url.as_deref())
            .await
            .map_err(NodeRunError::Transport)?;
    if crate::settings::store_setting::<crate::transport::relay::ActiveRelay>(
        &options.data_dir,
        &relay_url,
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
    .is_err()
    {
        endpoint.close().await;
        return Err(NodeRunError::Failed("node settings cannot be stored".to_owned()));
    }
    live.attach_endpoint(endpoint.clone(), tokio::runtime::Handle::current());
    let bound_port = endpoint
        .addr()
        .ip_addrs()
        .next()
        .map_or(options.port, std::net::SocketAddr::port);
    let publication_scope =
        options.scope.as_deref().unwrap_or_else(|| service.scope_id());
    if options.directory.is_some()
        && service.scope_kind_for(publication_scope) != Some(ScopeKind::Directory)
    {
        endpoint.close().await;
        return Err(NodeRunError::Usage(
            "directory publication requires a directory node scope".to_owned(),
        ));
    }
    let Ok(directory_publication) = start_directory_publication(
        &options,
        endpoint.id().to_string(),
        bound_port,
        publication_scope,
    )
    .await
    else {
        endpoint.close().await;
        return Err(NodeRunError::Failed(
            "directory publication is unavailable".to_owned(),
        ));
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
    let page = crate::invites::host_page_context(
        options.data_dir.clone(),
        options.invite_name.clone(),
        relay_url.clone(),
        Arc::clone(&relay_switcher),
        options.hub_url.clone(),
        hub.clone(),
        service.task_folder_mode(),
    );
    let background = start_background_workers(
        &administration,
        &page,
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
        service.handle(peer, frames)
    });
    let mut result = match tokio::signal::ctrl_c().await {
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
    outbox.abort();
    if let Err(error) = outbox.await
        && !error.is_cancelled()
    {
        eprintln!(
            "{}",
            serde_json::json!({
                "level": "error",
                "event": "outbox_join_failed",
                "error": error.to_string(),
            })
        );
    }
    service_for_shutdown.shutdown();
    endpoint.close().await;
    result.map_err(NodeRunError::Transport)?;
    println!("{}", serde_json::json!({ "event": "node_completed" }));
    Ok(())
}

#[allow(clippy::result_unit_err)]
pub async fn start_directory_publication(
    options: &NodeServeOptions,
    endpoint_id: String,
    bound_port: u16,
    configured_scope: &str,
) -> Result<Option<(DirectoryPublication, crate::discovery::wire::WireRecord)>, ()> {
    let (
        Some(directory),
        Some(certificate),
        Some(agent_id),
        Some(address),
        Some(scope),
        Some(capability),
        Some(ttl),
    ) = (
        options.directory.as_ref(),
        options.directory_certificate.as_ref(),
        options.agent_id.as_ref(),
        options.address.as_ref(),
        options.scope.as_ref(),
        options.capability.as_ref(),
        options.directory_record_ttl,
    )
    else {
        if options.directory.is_none()
            && options.directory_certificate.is_none()
            && options.agent_id.is_none()
            && options.address.is_none()
            && options.scope.is_none()
            && options.capability.is_none()
            && options.directory_record_ttl.is_none()
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
    let publication = DirectoryPublication::new(
        directory.clone(),
        &certificate,
        &options.data_dir,
        crate::directory_publication::PublicationRecord {
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

#[allow(clippy::result_unit_err)]
pub async fn sync_managed(
    data_dir: &Path,
    control: &str,
    scope_id: &str,
    client: &crate::managed_client::ManagedClient,
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

#[must_use]
pub fn managed_member_active(
    member: &aether_store::ManagedMember,
    agent_id: &str,
) -> bool {
    member.agent_id == agent_id && member.is_active(aether_core::time::now())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::start_background_workers;
    use crate::{admin::test_support::shared_in, invites::host_page_context};

    #[tokio::test]
    async fn hub_sync_and_listing_renewal_start_without_the_admin_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        let switch: crate::admin_ui::RelaySwitcher =
            Arc::new(|_, _| Box::pin(async { Ok(()) }));
        let page = host_page_context(
            directory.path().to_path_buf(),
            None,
            None,
            switch,
            None,
            None,
            false,
        );
        let workers = start_background_workers(
            &administration,
            &page,
            None,
            "endpoint".to_owned(),
            None,
            directory.path(),
        );
        assert!(workers.hub_sync_started());
        assert!(workers.listing_renewal_started());
    }
}
