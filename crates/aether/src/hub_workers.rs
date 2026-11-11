use std::{
    future::Future,
    path::Path,
    sync::{Arc, PoisonError},
    time::Duration,
};

use aether_core::time::now;
use aether_store::{AuditActor, HubApproval};

use crate::{
    admin::SharedAdministration,
    hub_client::{HubAccess, HubClient, Invite},
    hub_sync::{HostStep, caller_step, host_step},
    invites::{DirectInviteSigning, sign_direct_invite},
    node_serve_context::NodeServeContext,
};

const RETRY_WAIT: Duration = Duration::from_secs(5);
const POLL_WAIT: Duration = Duration::from_secs(25);

/// Follows the invites of the hub that `ctx` uses and runs `act` on each
/// list. A new hub URL starts again from the full list.
async fn follow<F, Fut>(ctx: &NodeServeContext, mut act: F)
where
    F: FnMut(HubClient, Vec<Invite>) -> Fut,
    Fut: Future<Output = ()>,
{
    // The hub URL that is followed, and the version of its last list.
    let mut followed: Option<(String, u64)> = None;
    loop {
        match ctx.hub.access() {
            HubAccess::Unavailable => {
                println!(
                    "{}",
                    serde_json::json!({ "event": "hub_access_unavailable" })
                );
                followed = None;
                tokio::time::sleep(RETRY_WAIT).await;
            }
            HubAccess::Off => {
                followed = None;
                tokio::time::sleep(RETRY_WAIT).await;
            }
            HubAccess::Ready(hub) => {
                let url = hub.url();
                let version = followed
                    .as_ref()
                    .filter(|(followed, _)| *followed == url)
                    .map(|&(_, version)| version);
                if let Ok(list) = hub.invites(version, POLL_WAIT).await {
                    followed = Some((url, list.version));
                    act(*hub, list.invites).await;
                } else {
                    followed = None;
                    tokio::time::sleep(RETRY_WAIT).await;
                }
            }
        }
    }
}

/// The local approval of `invite` and whether the owner blocked the
/// endpoint that it names.
fn host_inputs(
    store: &crate::hub_store::HubStore,
    invite: &Invite,
) -> Result<(Option<HubApproval>, bool), aether_store::StoreError> {
    let approval = store.approval(&invite.id)?;
    let blocked = match invite.receiver_endpoint.as_deref() {
        Some(endpoint) => store.is_blocked(endpoint)?,
        None => false,
    };
    Ok((approval, blocked))
}

/// Signs the grant of `step` and pins `approval` to the line. `None` when
/// either fails. When the line cannot be pinned, the grant is revoked again,
/// and the person leaves `access_group` if the signing put them in it.
#[allow(clippy::too_many_arguments)]
fn sign_and_pin(
    administration: &SharedAdministration,
    data_dir: &Path,
    store: &crate::hub_store::HubStore,
    own_name: &str,
    approval: &HubApproval,
    friend_name: &str,
    endpoint: &str,
    days: u64,
    access_group: &str,
) -> Option<String> {
    let was_member =
        crate::access_groups::is_member(data_dir, access_group, friend_name, endpoint)
            .ok()?;
    let signing = DirectInviteSigning::for_data_dir(data_dir);
    let (line, grant_id) = sign_direct_invite(
        administration,
        &signing,
        own_name,
        friend_name,
        endpoint,
        days,
        access_group,
        AuditActor::Ui,
    )
    .ok()?;
    if store.pin_approval(approval, endpoint, &line).is_err() {
        let _ = administration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revoke(&grant_id, AuditActor::Ui);
        if !was_member {
            let _ = crate::access_groups::leave(
                data_dir,
                access_group,
                friend_name,
                endpoint,
            );
        }
        return None;
    }
    Some(line)
}

/// What the host sends to the hub for one row.
enum HostSend {
    /// Remove the row.
    Refuse,
    /// Send this line.
    Line(String),
}

/// What the host sends for `invite`, signing a grant first when the row
/// needs one. `None` when there is nothing to send now, or when this node
/// failed (logged here).
fn host_send(
    administration: &SharedAdministration,
    ctx: &NodeServeContext,
    store: &crate::hub_store::HubStore,
    invite: &Invite,
    own_name: Option<&str>,
) -> Option<HostSend> {
    // The page signs under the same lock, so a row is signed only once.
    let _sign = ctx.hub_sign.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok((approval, blocked)) = host_inputs(store, invite) else {
        log("hub_invite_failed", invite);
        return None;
    };
    match host_step(invite, approval.as_ref(), own_name, blocked) {
        HostStep::Skip => None,
        HostStep::Refuse => Some(HostSend::Refuse),
        HostStep::Deliver(line) => Some(HostSend::Line(line)),
        HostStep::Sign {
            own_name,
            approval,
            friend_name,
            endpoint,
            days,
            access_group,
        } => {
            let line = sign_and_pin(
                administration,
                &ctx.data_dir,
                store,
                own_name,
                approval,
                &friend_name,
                &endpoint,
                days,
                &access_group,
            );
            if line.is_none() {
                log("hub_invite_failed", invite);
            }
            line.map(HostSend::Line)
        }
    }
}

pub async fn host_worker(
    administration: SharedAdministration,
    ctx: Arc<NodeServeContext>,
) {
    follow(&Arc::clone(&ctx), move |hub, invites| {
        let administration = administration.clone();
        let ctx = Arc::clone(&ctx);
        async move {
            let Ok(store) = crate::hub_store::HubStore::open(&ctx.data_dir) else {
                println!("{}", serde_json::json!({ "event": "hub_store_unavailable" }));
                return;
            };
            if store.clear_expired_approvals(now()).is_err() {
                println!("{}", serde_json::json!({ "event": "hub_store_unavailable" }));
                return;
            }
            let own_name = crate::hub_client::settings::hub_name(&ctx.data_dir);
            for invite in invites {
                match host_send(
                    &administration,
                    &ctx,
                    &store,
                    &invite,
                    own_name.as_deref(),
                ) {
                    None => {}
                    Some(HostSend::Refuse) => {
                        log("hub_invite_refused", &invite);
                        let _ = hub.remove(&invite.id).await;
                    }
                    Some(HostSend::Line(line)) => {
                        match hub.deliver(&invite.id, &line).await {
                            Ok(_) => log("hub_invite_delivered", &invite),
                            Err(_) => log("hub_invite_failed", &invite),
                        }
                    }
                }
            }
        }
    })
    .await;
}

pub async fn caller_worker(ctx: Arc<NodeServeContext>) {
    follow(&Arc::clone(&ctx), move |hub, invites| {
        let ctx = Arc::clone(&ctx);
        async move {
            let Ok(store) = crate::hub_store::HubStore::open(&ctx.data_dir) else {
                println!("{}", serde_json::json!({ "event": "hub_store_unavailable" }));
                return;
            };
            for invite in invites {
                let Ok(approval) = store.approval(&invite.id) else {
                    continue;
                };
                match caller_step(&invite, approval.as_ref()) {
                    crate::hub_sync::CallerStep::Skip => continue,
                    crate::hub_sync::CallerStep::Import(bundle) => {
                        match crate::direct::import_bundle(&ctx.data_dir, &bundle) {
                            Ok(_) => log("hub_invite_kept", &invite),
                            Err(error) => {
                                log_reason(
                                    "hub_invite_refused",
                                    &invite,
                                    &error.to_string(),
                                );
                            }
                        }
                    }
                    crate::hub_sync::CallerStep::Refuse(reason) => {
                        log_reason("hub_invite_refused", &invite, reason);
                    }
                }
                let _ = store.remove_approval(&invite.id);
                if hub.remove(&invite.id).await.is_err() {
                    log("hub_invite_failed", &invite);
                }
            }
        }
    })
    .await;
}

fn log(event: &str, invite: &Invite) {
    println!(
        "{}",
        serde_json::json!({ "event": event, "from": invite.from, "to": invite.to })
    );
}

fn log_reason(event: &str, invite: &Invite, reason: &str) {
    println!(
        "{}",
        serde_json::json!({
            "event": event,
            "from": invite.from,
            "to": invite.to,
            "reason": reason,
        })
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aether_store::{ApprovalKind, ApprovalSide, NodeStore};

    use super::sign_and_pin;
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
    fn a_line_that_cannot_be_pinned_leaves_no_grant_and_no_new_member() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path();
        let administration = shared_in(data_dir);
        let node = Arc::new(FakeNode::default().with_workspace("/start"));
        administration.lock().expect("administration must lock").set_live(node);
        let store =
            crate::hub_store::HubStore::open(data_dir).expect("store must open");
        let group = crate::access_groups::REQUESTS_GROUP;
        // This approval was never kept, so no line can be pinned to it.
        let approval = crate::hub_membership::new_approval(
            "0123456789abcdef0123456789abcdef",
            ApprovalSide::Sender,
            ApprovalKind::Request,
            "ana#0001",
            7,
            group,
            aether_core::time::now(),
        );
        let sign = |endpoint: &str| {
            sign_and_pin(
                &administration,
                data_dir,
                &store,
                "law#0042",
                &approval,
                "ana#0001",
                endpoint,
                7,
                group,
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
        assert_eq!(sign(&endpoint), None);
        assert_eq!(grants(), Vec::new());
        assert!(members(data_dir, group).is_empty());

        // A person who was in the group before stays in it.
        NodeStore::in_dir(data_dir)
            .expect("store must open")
            .add_access_group_member(group, "ana#0001", &endpoint)
            .expect("member must add");
        assert_eq!(sign(&endpoint), None);
        assert_eq!(grants(), Vec::new());
        assert_eq!(members(data_dir, group), ["ana#0001"]);
    }
}
