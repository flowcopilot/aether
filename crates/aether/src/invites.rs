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
    sign_direct_invite_with(
        administration,
        signing,
        agent_name,
        friend_name,
        endpoint_id,
        days,
        access_group,
        actor,
        crate::direct::encode_invite_after_grant,
    )
}

/// [`crate::direct::encode_invite_after_grant`], which tests replace.
type EncodeInvite = fn(
    &Path,
    &str,
    u64,
    crate::transport::A2aAuthorization,
    Option<String>,
    (String, String),
) -> Result<String, crate::direct::DirectError>;

#[allow(clippy::too_many_arguments)]
fn sign_direct_invite_with(
    administration: &SharedAdministration,
    signing: &DirectInviteSigning,
    agent_name: &str,
    friend_name: &str,
    endpoint_id: &str,
    days: u64,
    access_group: &str,
    actor: AuditActor,
    encode: EncodeInvite,
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
    let was_member = crate::access_groups::is_member(
        &signing.data_dir,
        access_group,
        friend_name,
        endpoint_id,
    )
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
    match encode(
        &signing.data_dir,
        agent_name,
        days,
        authorization,
        signing.relay_url(),
        agent_card,
    ) {
        Ok(line) => Ok((line, grant_id)),
        Err(error) => {
            // No invite carries the grant: take it back, and the group
            // membership when this call added it.
            let _ = administration
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .revoke(&grant_id, actor);
            if !was_member {
                let _ = crate::access_groups::leave(
                    &signing.data_dir,
                    access_group,
                    friend_name,
                    endpoint_id,
                );
            }
            Err(internal(&error))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aether_store::{AuditActor, NodeStore};

    use super::{DirectInviteSigning, invite_agent_name, sign_direct_invite_with};
    use crate::admin::test_support::{FakeNode, shared_in};

    fn members(data_dir: &std::path::Path, group: &str) -> Vec<String> {
        NodeStore::in_dir(data_dir)
            .expect("store must open")
            .access_groups()
            .expect("groups must read")
            .into_iter()
            .find(|entry| entry.name == group)
            .map(|entry| {
                entry.members.into_iter().map(|member| member.agent_id).collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn an_invite_that_cannot_be_encoded_leaves_no_grant_and_no_new_member() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path();
        let administration = shared_in(data_dir);
        let node = Arc::new(FakeNode::default().with_workspace("/start"));
        administration.lock().expect("administration must lock").set_live(node);
        let signing = DirectInviteSigning::for_data_dir(data_dir);
        let group = crate::access_groups::REQUESTS_GROUP;
        let sign = |endpoint: &str| {
            sign_direct_invite_with(
                &administration,
                &signing,
                "law",
                "ana",
                endpoint,
                7,
                group,
                AuditActor::Ui,
                |_, _, _, _, _, _| Err(crate::direct::DirectError::Unavailable),
            )
        };
        let grants = || {
            administration
                .lock()
                .expect("administration must lock")
                .grants()
                .expect("grants must read")
        };

        let endpoint = iroh::SecretKey::generate().public().to_string();
        assert!(sign(&endpoint).is_err());
        assert_eq!(grants(), Vec::new());
        assert!(members(data_dir, group).is_empty());

        // A person who was in the group before stays in it.
        NodeStore::in_dir(data_dir)
            .expect("store must open")
            .add_access_group_member(group, "ana", &endpoint)
            .expect("member must add");
        assert!(sign(&endpoint).is_err());
        assert_eq!(grants(), Vec::new());
        assert_eq!(members(data_dir, group), ["ana"]);
    }

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
