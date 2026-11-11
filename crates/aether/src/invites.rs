use std::path::Path;

use aether_store::AuditActor;

use crate::admin::{ApplyOutcome, ChangeRequest, SharedAdministration};

#[derive(Clone)]
pub struct DirectInviteSigning {
    pub data_dir: std::path::PathBuf,
    /// The relay for the line when the node keeps no active relay.
    relay_fallback: Option<String>,
}

impl DirectInviteSigning {
    #[must_use]
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::for_data_dir_with_relay(data_dir, None)
    }

    #[must_use]
    pub fn for_data_dir_with_relay(
        data_dir: &Path,
        relay_fallback: Option<String>,
    ) -> Self {
        Self { data_dir: data_dir.to_path_buf(), relay_fallback }
    }

    fn relay_url(&self) -> Option<String> {
        crate::settings::setting::<crate::transport::relay::ActiveRelay>(&self.data_dir)
            .ok()
            .flatten()
            .or_else(|| self.relay_fallback.clone())
    }
}

#[derive(Debug)]
pub enum InviteError {
    Refused,
    Internal(std::io::Error),
}

pub fn invite_agent_name(data_dir: &Path, invite_name: Option<&str>) -> String {
    if let Some(name) = crate::hub_client::settings::hub_name(data_dir) {
        return name;
    }
    if let Some(name) = invite_name.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    if let Ok(name) = crate::settings::setting::<crate::direct::HostName>(data_dir) {
        return name;
    }
    crate::direct::default_agent_name(
        &crate::direct::endpoint_id(data_dir).unwrap_or_default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sign_direct_invite(
    administration: &SharedAdministration,
    signing: &DirectInviteSigning,
    agent_name: &str,
    friend_name: &str,
    endpoint_id: &str,
    days: u64,
    access_group: &str,
    actor: AuditActor,
) -> Result<(String, String), InviteError> {
    let internal = |error: &dyn std::fmt::Display| {
        InviteError::Internal(std::io::Error::other(error.to_string()))
    };
    let agent_card = crate::direct::create_card(&signing.data_dir, agent_name)
        .map_err(|error| internal(&error))?;
    let (authorization, grant) = crate::direct::sign_invite_access(
        &signing.data_dir,
        friend_name,
        endpoint_id,
        days,
    )
    .map_err(|error| internal(&error))?;
    let grant_id = grant.id.as_str().to_owned();
    let change = ChangeRequest::Grant { id: grant_id.clone(), grant }
        .into_change()
        .map_err(|error| internal(&error))?;
    crate::access_groups::join_and_apply(
        &signing.data_dir,
        access_group,
        friend_name,
        endpoint_id,
        || {
            administration.lock().map_err(|_| InviteError::Refused).and_then(
                |mut admin| {
                    admin
                        .apply(&change, actor)
                        .map_err(|_| InviteError::Refused)
                        .and_then(|outcome| match outcome {
                            ApplyOutcome::Applied => Ok(()),
                            ApplyOutcome::Pending => Err(InviteError::Refused),
                        })
                },
            )
        },
    )
    .map_err(|error| match error {
        crate::access_groups::JoinError::Store(error) => internal(&error),
        crate::access_groups::JoinError::Apply(_) => InviteError::Refused,
    })?;
    crate::direct::encode_invite_after_grant(
        &signing.data_dir,
        agent_name,
        days,
        authorization,
        signing.relay_url(),
        agent_card,
    )
    .map(|line| (line, grant_id))
    .map_err(|error| internal(&error))
}

#[cfg(test)]
mod tests {
    use super::invite_agent_name;

    #[test]
    fn invite_agent_name_matches_the_host_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        crate::settings::store_setting::<crate::direct::HostName>(
            &data_dir,
            &"page-name".to_owned(),
        )
        .expect("host name must store");
        assert_eq!(
            invite_agent_name(&data_dir, Some("flag-name")),
            invite_agent_name(&data_dir, Some("flag-name"))
        );
        assert_eq!(invite_agent_name(&data_dir, None), "page-name");
    }
}
