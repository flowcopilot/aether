use std::{
    future::Future,
    sync::{Arc, PoisonError},
    time::Duration,
};

use aether_core::time::now;
use aether_store::AuditActor;

use crate::{
    admin::SharedAdministration,
    admin_ui::PageContext,
    hub_client::{HubClient, Invite},
    hub_sync::{HostStep, caller_step, host_step},
    invites::{DirectInviteSigning, sign_direct_invite},
};

const RETRY_WAIT: Duration = Duration::from_secs(5);
const POLL_WAIT: Duration = Duration::from_secs(25);

async fn follow<C, F, Fut>(current: C, mut act: F)
where
    C: Fn() -> Option<HubClient>,
    F: FnMut(HubClient, Vec<Invite>) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut version = None;
    let mut followed = String::new();
    loop {
        let Some(hub) = current() else {
            version = None;
            tokio::time::sleep(RETRY_WAIT).await;
            continue;
        };
        if hub.url() != followed {
            followed = hub.url();
            version = None;
        }
        if let Ok(list) = hub.invites(version, POLL_WAIT).await {
            version = Some(list.version);
            act(hub, list.invites).await;
        } else {
            version = None;
            tokio::time::sleep(RETRY_WAIT).await;
        }
    }
}

enum HostAction {
    Skip,
    Refuse,
    Failed,
    Deliver(String),
}

fn host_action(
    administration: &SharedAdministration,
    page: &PageContext,
    store: &crate::hub_store::HubStore,
    invite: &Invite,
    own_name: Option<&str>,
) -> HostAction {
    let _sign = page.hub_sign().lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(approval) = store.approval(&invite.id) else {
        return HostAction::Failed;
    };
    let blocked = if let Some(endpoint) = invite.receiver_endpoint.as_deref() {
        let Ok(blocked) = store.is_blocked(endpoint) else {
            return HostAction::Failed;
        };
        blocked
    } else {
        false
    };
    match host_step(invite, approval.as_ref(), own_name, blocked) {
        HostStep::Skip => HostAction::Skip,
        HostStep::Refuse => HostAction::Refuse,
        HostStep::Deliver(line) => HostAction::Deliver(line),
        HostStep::Sign { friend_name, endpoint, days, access_group } => {
            let (Some(own_name), Some(approval)) = (own_name, approval.as_ref()) else {
                return HostAction::Skip;
            };
            let signing = DirectInviteSigning::from_page(page);
            let Ok((line, grant_id)) = sign_direct_invite(
                administration,
                &signing,
                own_name,
                &friend_name,
                &endpoint,
                days,
                &access_group,
                AuditActor::Ui,
            ) else {
                return HostAction::Failed;
            };
            if store.pin_approval(approval, &endpoint, &line).is_err() {
                let _ = administration
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .revoke(&grant_id, AuditActor::Ui);
                return HostAction::Failed;
            }
            HostAction::Deliver(line)
        }
    }
}

pub async fn host_worker(administration: SharedAdministration, page: Arc<PageContext>) {
    let current = {
        let page = Arc::clone(&page);
        move || page.current_hub()
    };
    follow(current, |hub, invites| {
        let administration = administration.clone();
        let page = Arc::clone(&page);
        async move {
            let Ok(store) = crate::hub_store::HubStore::open(&page.data_dir) else {
                println!("{}", serde_json::json!({ "event": "hub_store_unavailable" }));
                return;
            };
            if store.clear_expired_approvals(now()).is_err() {
                println!("{}", serde_json::json!({ "event": "hub_store_unavailable" }));
                return;
            }
            let own_name = crate::settings::setting::<
                crate::hub_client::settings::HubName,
            >(&page.data_dir)
            .ok()
            .flatten();
            for invite in invites {
                let line = match host_action(
                    &administration,
                    &page,
                    &store,
                    &invite,
                    own_name.as_deref(),
                ) {
                    HostAction::Skip => continue,
                    HostAction::Refuse => {
                        log("hub_invite_refused", &invite);
                        let _ = hub.remove(&invite.id).await;
                        continue;
                    }
                    HostAction::Failed => {
                        log("hub_invite_failed", &invite);
                        continue;
                    }
                    HostAction::Deliver(line) => line,
                };
                match hub.deliver(&invite.id, &line).await {
                    Ok(_) => log("hub_invite_delivered", &invite),
                    Err(_) => log("hub_invite_failed", &invite),
                }
            }
        }
    })
    .await;
}

pub async fn caller_worker(page: Arc<PageContext>) {
    let current = {
        let page = Arc::clone(&page);
        move || page.current_hub()
    };
    follow(current, |hub, invites| {
        let page = Arc::clone(&page);
        async move {
            let Ok(store) = crate::hub_store::HubStore::open(&page.data_dir) else {
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
                        match crate::direct::import_bundle(&page.data_dir, &bundle) {
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
