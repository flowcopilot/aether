use std::{path::Path, sync::Arc};

use crate::admin::SharedAdministration;

use crate::{
    admin_ui::{PageContext, RelaySwitcher},
    directory_publication::DirectoryPublication,
};

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
            let _ = handle.await;
        }
        if let Some(handle) = self.managed_renewer {
            handle.abort();
            let _ = handle.await;
        }
        for handle in
            [self.hub_host, self.hub_caller, self.listing].into_iter().flatten()
        {
            handle.abort();
            let _ = handle.await;
        }
    }
}

pub struct HubWorkers {
    pub page: Arc<PageContext>,
}

#[allow(clippy::too_many_arguments)]
pub fn start_background_workers(
    administration: &SharedAdministration,
    data_dir: &Path,
    hub: Option<crate::hub_client::HubClient>,
    invite_name: Option<String>,
    relay_url: Option<String>,
    switch_relay: Option<RelaySwitcher>,
    directory_publication: Option<Arc<DirectoryPublication>>,
    endpoint_id: String,
    managed_sync: Option<(Arc<crate::managed_client::ManagedClient>, String, String)>,
) -> BackgroundWorkers {
    let page = switch_relay.map(|switch_relay| {
        crate::invites::host_page_context(
            data_dir.to_path_buf(),
            invite_name,
            relay_url,
            switch_relay,
            hub,
        )
    });
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
    let (hub_host, hub_caller, listing) = page.map_or((None, None, None), |page| {
        let hub_host = tokio::spawn(crate::admin_ui::hub::host_worker(
            Arc::clone(administration),
            Arc::clone(&page),
        ));
        let hub_caller = page.caller.then(|| {
            tokio::spawn(crate::admin_ui::hub::caller_worker(Arc::clone(&page)))
        });
        let listing =
            tokio::spawn(crate::admin_ui::listing::listing_worker(Arc::clone(&page)));
        (Some(hub_host), hub_caller, Some(listing))
    });
    BackgroundWorkers {
        directory_renewer,
        managed_renewer,
        hub_host,
        hub_caller,
        listing,
    }
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
