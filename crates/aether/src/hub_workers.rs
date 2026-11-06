use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use aether_core::time::now;
use aether_store::AuditActor;

use crate::{
    admin::SharedAdministration,
    hub_client::{HubAccess, HubClient, Invite},
    hub_sync::{HostStep, caller_step, host_step},
    invites::{DirectInviteSigning, sign_direct_invite},
    node_serve_context::NodeServeContext,
};

const RETRY_WAIT: Duration = Duration::from_secs(5);
const POLL_WAIT: Duration = Duration::from_secs(25);

enum HubPoll {
    Ready(Box<HubClient>),
    Off,
    Unavailable,
}

fn hub_poll(ctx: &NodeServeContext) -> HubPoll {
    match ctx.hub.access() {
        HubAccess::Ready(client) => HubPoll::Ready(client),
        HubAccess::Off => HubPoll::Off,
        HubAccess::Unavailable => HubPoll::Unavailable,
    }
}

async fn follow<C, F, Fut>(current: C, mut act: F)
where
    C: Fn() -> HubPoll,
    F: FnMut(HubClient, Vec<Invite>) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut version = None;
    let mut followed = String::new();
    loop {
        match current() {
            HubPoll::Unavailable => {
                println!(
                    "{}",
                    serde_json::json!({ "event": "hub_access_unavailable" })
                );
                version = None;
                tokio::time::sleep(RETRY_WAIT).await;
            }
            HubPoll::Off => {
                version = None;
                tokio::time::sleep(RETRY_WAIT).await;
            }
            HubPoll::Ready(hub) => {
                if hub.url() != followed {
                    followed = hub.url();
                    version = None;
                }
                if let Ok(list) = hub.invites(version, POLL_WAIT).await {
                    version = Some(list.version);
                    act(*hub, list.invites).await;
                } else {
                    version = None;
                    tokio::time::sleep(RETRY_WAIT).await;
                }
            }
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
    data_dir: &Path,
    hub_sign: &Mutex<()>,
    store: &crate::hub_store::HubStore,
    invite: &Invite,
    own_name: Option<&str>,
) -> HostAction {
    let _sign = hub_sign.lock().unwrap_or_else(PoisonError::into_inner);
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
            let signing = DirectInviteSigning::for_data_dir(data_dir);
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

pub async fn host_worker(
    administration: SharedAdministration,
    ctx: Arc<NodeServeContext>,
) {
    follow(
        {
            let ctx = Arc::clone(&ctx);
            move || hub_poll(&ctx)
        },
        {
            let ctx = Arc::clone(&ctx);
            move |hub, invites| {
                let administration = administration.clone();
                let ctx = Arc::clone(&ctx);
                async move {
                    let Ok(store) = crate::hub_store::HubStore::open(&ctx.data_dir)
                    else {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "hub_store_unavailable" })
                        );
                        return;
                    };
                    if store.clear_expired_approvals(now()).is_err() {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "hub_store_unavailable" })
                        );
                        return;
                    }
                    let own_name = crate::settings::setting::<
                        crate::hub_client::settings::HubName,
                    >(&ctx.data_dir)
                    .ok()
                    .flatten();
                    for invite in invites {
                        let line = match host_action(
                            &administration,
                            &ctx.data_dir,
                            &ctx.hub_sign,
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
            }
        },
    )
    .await;
}

pub async fn caller_worker(ctx: Arc<NodeServeContext>) {
    follow(
        {
            let ctx = Arc::clone(&ctx);
            move || hub_poll(&ctx)
        },
        {
            let ctx = Arc::clone(&ctx);
            move |hub, invites| {
                let ctx = Arc::clone(&ctx);
                async move {
                    let Ok(store) = crate::hub_store::HubStore::open(&ctx.data_dir)
                    else {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "hub_store_unavailable" })
                        );
                        return;
                    };
                    for invite in invites {
                        let Ok(approval) = store.approval(&invite.id) else {
                            continue;
                        };
                        match caller_step(&invite, approval.as_ref()) {
                            crate::hub_sync::CallerStep::Skip => continue,
                            crate::hub_sync::CallerStep::Import(bundle) => {
                                match crate::direct::import_bundle(
                                    &ctx.data_dir,
                                    &bundle,
                                ) {
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
            }
        },
    )
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
    use std::sync::{Arc, Mutex};

    use super::hub_poll;
    use crate::{
        hub_client::{HubAccess, node_hub::NodeHub},
        node_serve_context::NodeServeContext,
    };

    #[test]
    fn hub_poll_reports_unavailable_when_the_store_cannot_be_read() {
        let directory = tempfile::tempdir().expect("directory");
        aether_store::NodeStore::in_dir(directory.path()).expect("store");
        std::fs::write(directory.path().join("node.sqlite"), b"not sqlite")
            .expect("break store");
        let ctx = NodeServeContext {
            data_dir: directory.path().to_path_buf(),
            hub: Arc::new(NodeHub::new(directory.path().to_path_buf(), None)),
            hub_sign: Arc::new(Mutex::new(())),
            caller: false,
        };
        assert!(matches!(hub_poll(&ctx), super::HubPoll::Unavailable));
        assert!(matches!(ctx.hub.access(), HubAccess::Unavailable));
    }
}
