use std::path::PathBuf;

use crate::{
    direct::{self, DirectError},
    directory_client, managed_client,
    transport::{self, Frame, TransportError},
};

#[derive(Debug, Clone)]
pub struct RequestInput {
    pub data_dir: PathBuf,
    pub scope: String,
    pub text: String,
    pub endpoint_id: Option<String>,
    pub address: Option<String>,
    pub directory: Option<String>,
    pub directory_certificate: Option<PathBuf>,
    pub agent_id: Option<String>,
    pub agent_verifying_key: Option<PathBuf>,
    pub managed_control: Option<String>,
    pub managed_control_certificate: Option<PathBuf>,
    pub relay_url: Option<String>,
    pub authorization: Option<PathBuf>,
    pub idempotency_key: Option<String>,
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
    let data_dir = &input.data_dir;
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
            .map_err(|_| RequestError::AuthorizationInvalid)?,
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
