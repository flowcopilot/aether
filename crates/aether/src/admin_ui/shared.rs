use std::path::Path;

use topcoat::{
    Result as TopcoatResult,
    view::{View, component, view},
};

use crate::components::alert::{alert, alert_description};

pub(super) fn request_drafts_of(
    data_dir: &Path,
    now: u64,
) -> Vec<aether_store::AccessRequestDraft> {
    aether_store::NodeStore::in_dir(data_dir)
        .and_then(|store| {
            store.access_request_drafts(i64::try_from(now).unwrap_or(i64::MAX))
        })
        .unwrap_or_default()
}

pub(super) fn queue_text(data_dir: &Path) -> String {
    match crate::policy::queue_limits(data_dir) {
        Ok(Some(limits)) => {
            let tasks = if limits.global == 1 {
                "1 waiting task".to_owned()
            } else {
                format!("{} waiting tasks", limits.global)
            };
            format!(
                "Your agent keeps {tasks} in all and {} for each person. People who get access by request share these places.",
                limits.per_peer
            )
        }
        Ok(None) => "This node has no task policy yet. Start `aether node serve` once."
            .to_owned(),
        Err(_) => "The task limits of this node cannot be read.".to_owned(),
    }
}

#[component]
pub(super) async fn relay_notice() -> TopcoatResult<impl View> {
    Ok(view! {
        alert(
            alert_description(
                "Only your local network can reach this agent. "
                <a href="#relay">"Set a relay"</a>
                " before you invite someone elsewhere."
            )
        )
    })
}
