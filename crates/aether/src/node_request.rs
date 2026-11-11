use std::path::PathBuf;

use crate::{
    direct::{self, DirectError},
    directory_client, managed_client,
    transport::{self, Frame, TransportError},
};

// The flags of `aether node request`. A doc comment here would become the
// command's help.
#[derive(Debug, Clone, usage_rs::Args)]
pub struct RequestInput {
    /// Defaults to `$AETHER_DATA_DIR`, else `~/.aether/host`.
    #[usage(long)]
    pub data_dir: Option<PathBuf>,
    #[usage(long)]
    pub endpoint_id: Option<String>,
    #[usage(long)]
    pub address: Option<String>,
    #[usage(long)]
    pub directory: Option<String>,
    #[usage(long)]
    pub directory_certificate: Option<PathBuf>,
    #[usage(long)]
    pub agent_id: Option<String>,
    #[usage(long)]
    pub agent_verifying_key: Option<PathBuf>,
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
    pub managed_control: Option<String>,
    #[usage(long, requires = "--managed-control")]
    pub managed_control_certificate: Option<PathBuf>,
    /// An iroh relay to reach the peer through.
    #[usage(long, env = "AETHER_RELAY_URL")]
    pub relay_url: Option<String>,
    #[usage(long)]
    pub scope: String,
    #[usage(long)]
    pub text: String,
    /// Optional for a direct contact imported from an invite: its bundle
    /// holds the authorization.
    #[usage(long)]
    pub authorization: Option<PathBuf>,
    /// Lets a caller retry a request idempotently by resubmitting the same
    /// key. A fresh one is generated when this is left unset, matching the
    /// previous, only behavior.
    #[usage(long)]
    pub idempotency_key: Option<String>,
    /// Lets a caller learn its own task id ahead of the response, so it can
    /// target this request with an administration cancellation. A fresh one
    /// is generated when this is left unset, matching the previous, only
    /// behavior. Must be a UUID, the same requirement the wire protocol
    /// already places on every task id.
    #[usage(long)]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RequestError {
    AuthorizationRequired,
    AuthorizationInvalid,
    TaskIdInvalid,
    FrameInvalid,
    Addressing,
    Transport(TransportError),
}

impl From<TransportError> for RequestError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

fn direct_store_unavailable(error: DirectError) -> Option<RequestError> {
    match error {
        DirectError::Unavailable => Some(RequestError::Transport(
            TransportError::Network("direct store is unavailable".to_owned()),
        )),
        _ => None,
    }
}

pub async fn send(input: RequestInput) -> Result<Vec<Frame>, RequestError> {
    let data_dir = &crate::data_dir::resolve_node_data_dir(input.data_dir.clone());
    if input
        .task_id
        .as_ref()
        .is_some_and(|task_id| uuid::Uuid::parse_str(task_id).is_err())
    {
        return Err(RequestError::TaskIdInvalid);
    }
    let bundle = match (input.scope.as_str(), input.agent_id.as_deref()) {
        (direct::SCOPE, Some(agent_id))
            if input.endpoint_id.is_none()
                && input.directory.is_none()
                && input.managed_control.is_none() =>
        {
            match direct::open_store(data_dir) {
                Ok(store) => match direct::contact_bundle(&store, agent_id) {
                    Ok(bundle) => Some(bundle),
                    Err(error) => {
                        if let Some(error) = direct_store_unavailable(error) {
                            return Err(error);
                        }
                        None
                    }
                },
                Err(error) => {
                    if let Some(error) = direct_store_unavailable(error) {
                        return Err(error);
                    }
                    None
                }
            }
        }
        _ => None,
    };
    let authorization = match (&input.authorization, &bundle) {
        (Some(path), _) => directory_client::read_authorization(path)
            .map_err(|()| RequestError::AuthorizationInvalid)?,
        (None, Some(bundle)) => bundle.authorization.clone(),
        (None, None) => return Err(RequestError::AuthorizationRequired),
    };
    let relay_url =
        input.relay_url.clone().or_else(|| bundle.and_then(|bundle| bundle.relay_url));
    let fresh = direct::RequestIds::fresh();
    let task_id = input.task_id.clone().unwrap_or(fresh.task_id);
    let ids = direct::RequestIds {
        task_id,
        idempotency_key: input.idempotency_key.clone().unwrap_or(fresh.idempotency_key),
    };
    let request = direct::request_frame(authorization, &input.scope, &input.text, ids)
        .map_err(|_| RequestError::FrameInvalid)?;
    if let Some(control) = input.managed_control.as_deref() {
        return managed_client::request_task(
            &input,
            data_dir,
            control,
            request,
            relay_url.as_deref(),
        )
        .await
        .map_err(RequestError::Transport);
    }
    match (
        input.endpoint_id.as_deref(),
        input.address.as_deref(),
        input.directory.as_deref(),
        input.directory_certificate.as_deref(),
        input.agent_id.as_deref(),
        input.agent_verifying_key.as_deref(),
    ) {
        (Some(endpoint_id), Some(address), None, None, None, None) => match relay_url
            .as_deref()
        {
            Some(relay_url) => {
                transport::request_once_via_relay(
                    data_dir,
                    endpoint_id,
                    relay_url,
                    request,
                )
                .await
            }
            None => {
                transport::request_once(data_dir, endpoint_id, address, request).await
            }
        }
        .map_err(RequestError::Transport),
        (None, address, None, None, Some(agent_id), None)
            if address.is_some() || relay_url.is_some() =>
        {
            let invitation = if input.scope == direct::SCOPE {
                direct::open_store(data_dir)
                    .and_then(|store| direct::contact(&store, agent_id))
            } else {
                Err(DirectError::UnknownContact)
            }
            .map_err(|_| RequestError::FrameInvalid)?;
            direct::send(data_dir, &invitation, address, relay_url.as_deref(), request)
                .await
                .map_err(RequestError::Transport)
        }
        (None, None, Some(directory), Some(certificate), Some(agent_id), Some(key)) => {
            directory_client::request_via_directory(
                data_dir,
                directory,
                certificate,
                key,
                agent_id,
                &input.scope,
                request,
            )
            .await
            .map_err(RequestError::Transport)
        }
        _ => Err(RequestError::Addressing),
    }
}
