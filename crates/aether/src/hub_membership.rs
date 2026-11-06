//! The hub steps that the owner page and the MCP tools share: keep this
//! node's hub name, offer this node's agent to a name, and accept an invite.
//! Each step keeps its local approval, which the hub workers need.

use aether_core::time::now;
use std::time::Duration;

use aether_store::{ApprovalKind, ApprovalSide, HubApproval, NodeStore, StoreError};

use crate::hub_client::{HubClient, HubError, Invite, Role, Status};

/// How long the node keeps the approval of an invite: as long as the hub
/// keeps the invite.
pub const APPROVAL_SECONDS: i64 = crate::hub_client::protocol::INVITE_SECONDS;

/// What a hub error means for the person who asked.
pub const fn hub_message(error: HubError) -> &'static str {
    match error {
        HubError::Taken => "No number is free for this name. Choose another name.",
        HubError::NotFound => {
            "Nobody has this name on Aether. Check it with your friend, with the four digits."
        }
        HubError::OldHub => {
            "This hub is older than this node. Ask its operator to update it."
        }
        HubError::OldNode => "This node is older than the hub. Update Aether.",
        HubError::Limit => "You reached a hub limit. Remove an old item first.",
        HubError::Invalid => "The hub refused this. Check the name.",
        HubError::NoAccount => "Choose your name first.",
        HubError::RateLimited => "Too many requests. Wait a minute and try again.",
        HubError::Forbidden => "This invite changed. Look at the list again.",
        _ => "The hub cannot be reached now. Try again, or use a connect code.",
    }
}

/// Keeps `handle`, this node's name as the hub gives it, for the agent name
/// in invites. A name that cannot be an agent name is not kept.
pub fn keep_hub_name(data_dir: &std::path::Path, handle: &str) {
    if crate::hub_client::valid_full_name(handle)
        && crate::settings::setting::<crate::hub_client::settings::HubName>(data_dir)
            .ok()
            .flatten()
            .as_deref()
            != Some(handle)
    {
        let _ = crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            data_dir,
            &Some(handle.to_owned()),
        );
    }
}

/// Offers this node's agent to the full name `to` for `days` days: first
/// the hub invite, then its local approval. The worker signs only an invite
/// that this node sent.
pub async fn offer(
    hub: &HubClient,
    data_dir: &std::path::Path,
    to: &str,
    days: u64,
) -> Result<Invite, &'static str> {
    let invite = hub.invite(to, days).await.map_err(hub_message)?;
    if approve_invite(data_dir, &invite, days).is_err() {
        let _ = hub.remove(&invite.id).await;
        return Err("This node cannot keep the invite. Try again.");
    }
    Ok(invite)
}

/// Keeps the local approval of `invite`, which this node just sent.
fn approve_invite(
    data_dir: &std::path::Path,
    invite: &Invite,
    days: u64,
) -> Result<(), StoreError> {
    let now = now();
    NodeStore::in_dir(data_dir)?.put_hub_approval(&HubApproval {
        invite_id: invite.id.clone(),
        side: ApprovalSide::Sender,
        kind: ApprovalKind::Invite,
        peer_name: invite.to.clone(),
        peer_endpoint: None,
        days: u32::try_from(days).map_err(|_| StoreError::InvalidState)?,
        // New hub invites join the default group, as connect codes do.
        access_group: crate::access_groups::FRIENDS_GROUP.to_owned(),
        bundle: None,
        created_at: now,
        expires_at: now.saturating_add(APPROVAL_SECONDS),
        listing: None,
    })
}

/// Accepts the pending invite `id` that this node received: first the local
/// approval, then the answer to the hub. The caller keeps a line later only
/// from the name that it accepted here.
pub async fn accept(
    hub: &HubClient,
    data_dir: &std::path::Path,
    id: &str,
) -> Result<Invite, HubError> {
    let invite = hub
        .invites(None, Duration::ZERO)
        .await?
        .invites
        .into_iter()
        .find(|invite| invite.id == id)
        .filter(|invite| {
            invite.role == Role::Receiver && invite.status == Status::Pending
        })
        .ok_or(HubError::Forbidden)?;
    let now = now();
    NodeStore::in_dir(data_dir)
        .and_then(|store| {
            store.put_hub_approval(&HubApproval {
                invite_id: invite.id.clone(),
                side: ApprovalSide::Receiver,
                kind: ApprovalKind::Invite,
                peer_name: invite.from.clone(),
                peer_endpoint: None,
                days: u32::try_from(invite.days.clamp(
                    crate::hub_client::protocol::ACCESS_DAYS_MIN,
                    crate::hub_client::protocol::ACCESS_DAYS_MAX,
                ))
                .unwrap_or(u32::MAX),
                access_group: crate::access_groups::FRIENDS_GROUP.to_owned(),
                bundle: None,
                created_at: now,
                expires_at: now.saturating_add(APPROVAL_SECONDS),
                listing: None,
            })
        })
        .map_err(|_| HubError::Unexpected)?;
    hub.accept(id).await
}
