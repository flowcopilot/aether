use std::{path::Path, sync::Arc};

use aether_store::AuditActor;

use crate::{
    admin::{ApplyOutcome, ChangeRequest, SharedAdministration},
    admin_ui::{PageContext, RelaySwitcher},
};

#[derive(Debug)]
pub enum InviteError {
    Refused,
    Internal(std::io::Error),
}

pub fn invite_agent_name(data_dir: &Path, invite_name: Option<&str>) -> String {
    if let Ok(Some(name)) =
        crate::settings::setting::<crate::hub_client::settings::HubName>(data_dir)
    {
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
    page: &PageContext,
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
    let agent_card = crate::direct::create_card(&page.data_dir, agent_name)
        .map_err(|error| internal(&error))?;
    let (authorization, grant) = crate::direct::sign_invite_access(
        &page.data_dir,
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
        &page.data_dir,
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
        &page.data_dir,
        agent_name,
        days,
        authorization,
        page.relay_url(),
        agent_card,
    )
    .map(|line| (line, grant_id))
    .map_err(|error| internal(&error))
}

pub fn host_page_context(
    data_dir: std::path::PathBuf,
    invite_name: Option<String>,
    relay_url: Option<String>,
    switch_relay: RelaySwitcher,
    hub_flag: Option<String>,
    hub: Option<crate::hub_client::HubClient>,
    task_folder_mode: bool,
) -> Arc<PageContext> {
    Arc::new(
        PageContext::host(data_dir, invite_name, relay_url, switch_relay, hub_flag)
            .with_hub(hub)
            .with_caller(true)
            .with_task_folder_mode(task_folder_mode),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{host_page_context, invite_agent_name};

    #[test]
    fn invite_agent_name_matches_the_host_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        crate::settings::store_setting::<crate::direct::HostName>(
            &data_dir,
            &"page-name".to_owned(),
        )
        .expect("host name must store");
        let switch: crate::admin_ui::RelaySwitcher =
            Arc::new(|_, _| Box::pin(async { Ok(()) }));
        let page = host_page_context(
            data_dir.clone(),
            Some("flag-name".to_owned()),
            None,
            switch,
            None,
            None,
            false,
        );
        assert_eq!(page.agent_name(), invite_agent_name(&data_dir, Some("flag-name")));
        let switch: crate::admin_ui::RelaySwitcher =
            Arc::new(|_, _| Box::pin(async { Ok(()) }));
        let page =
            host_page_context(data_dir.clone(), None, None, switch, None, None, false);
        assert_eq!(page.agent_name(), invite_agent_name(&data_dir, None));
    }
}
