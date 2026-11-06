//! The public listing of a service agent on the hub. The listing reuses the
//! text of the Agent Card; this section shows what people see, and lists the
//! agent or stops the listing.

use aether_core::time::now_seconds;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    router::{content::Form, error::bad_request, page},
    view::{View, attributes, component, view},
};

use super::{Page, host, require_admin_session};
use crate::{
    components::button::{ButtonSize, ButtonVariant, button},
    hub_client::{AgentKind, HubError},
    listing::{HubPublishError, publish_to_hub, stop_hub_listing},
};

/// The text about the shared task queue.
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

/// The listing section of a service agent's page.
#[component]
pub(super) async fn listing_section(
    data_dir: PathBuf,
    error: String,
) -> TopcoatResult<impl View> {
    let settings = crate::agent_card::settings(&data_dir);
    let name =
        crate::settings::setting::<crate::hub_client::settings::HubName>(&data_dir)
            .ok()
            .flatten();
    let now = now_seconds();
    let days_left =
        crate::settings::setting::<crate::listing::settings::ListingState>(&data_dir)
            .ok()
            .flatten()
            .filter(|state| state.listed && state.expires_at > now)
            .map(|state| state.expires_at.saturating_sub(now).div_ceil(24 * 60 * 60));
    let queue = queue_text(&data_dir);
    let status = match days_left {
        Some(1) => {
            "Listed. The listing ends in 1 day unless this page renews it.".to_owned()
        }
        Some(days) => {
            format!(
                "Listed. The listing ends in {days} days unless this page renews it."
            )
        }
        None => "Not listed.".to_owned(),
    };
    Ok(view! {
        <section id="listing" class="flex flex-col gap-4 pt-2" aria-labelledby="listing-title">
            <div class="flex flex-col gap-1.5">
                <h2 id="listing-title" class="text-xl font-semibold tracking-tight">"Public listing"</h2>
                <p class="text-sm text-muted-foreground">
                    "People find a listed agent by search on the hub and ask for access. You approve each request on this page."
                </p>
            </div>
            <div class=(if days_left.is_some() {
                "flex flex-wrap items-center justify-between gap-3 rounded-xl border border-success/40 bg-success/5 px-4 py-3"
            } else {
                "flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-background px-4 py-3"
            })>
                <p class="flex items-center gap-2.5 text-sm" role="status">
                    <span class=(if days_left.is_some() { "size-2 shrink-0 rounded-full bg-success" } else { "size-2 shrink-0 rounded-full bg-muted-foreground/40" }) aria-hidden="true"></span>
                    (status)
                </p>
                <div class="flex flex-wrap gap-2">
                    <form method="post" action="/listing/publish">
                        <input type="hidden" name="action" value="list">
                        button(size: ButtonSize::Sm, attrs: attributes! { type="submit" disabled=(name.is_none().then_some("")) },
                            (if days_left.is_some() { "List again with the current card" } else { "List this agent" }))
                    </form>
                    if days_left.is_some() {
                        <form method="post" action="/listing/publish">
                            <input type="hidden" name="action" value="stop">
                            button(variant: ButtonVariant::Outline, size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Stop listing")
                        </form>
                    }
                </div>
            </div>
            if !error.is_empty() {
                super::hub::error_line(text: error.clone())
            }
            <div class="grid gap-6 md:grid-cols-[21rem_minmax(0,1fr)]">
                <div class="flex flex-col gap-2 rounded-xl border border-border bg-background px-4.5 py-4 text-sm" aria-label="People see this">
                    <p class="text-xs font-medium text-muted-foreground">"People see this"</p>
                    <p class="font-mono font-medium">(name.clone().unwrap_or_else(|| "Your name on Aether".to_owned()))</p>
                    <p>(&settings.description)</p>
                    <p class="text-[13px]"><strong>(&settings.skill_name)</strong> ": " (&settings.skill_description)</p>
                    <p class="text-[13px] text-muted-foreground">(settings.tags.join(", "))</p>
                </div>
                <div class="flex flex-col gap-2.5 text-[13px] leading-5 text-muted-foreground">
                    <p>
                        "The listing uses the text of "
                        <a href="#card">"your agent's card"</a>
                        ". When you change the card, list the agent again."
                    </p>
                    <p>"When this agent is listed, people can see when it is online."</p>
                    <p>
                        (queue) " "
                        <a href="/advanced">"Change them in Advanced."</a>
                    </p>
                    <p>"When another agent connects, each computer can see the public IP address of the other."</p>
                </div>
            </div>
        </section>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListingForm {
    action: String,
}

/// Lists this service agent on the hub, or stops the listing.
#[page(POST "/listing/publish")]
pub(super) async fn publish_listing(
    cx: &Cx,
    Form(form): Form<ListingForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(bad_request("this node has no listing").into());
    };
    if crate::settings::setting::<crate::hub_client::settings::AgentKind>(
        &page.data_dir,
    )
    .ok()
    .flatten()
        != Some(AgentKind::Service)
    {
        return Err(bad_request("only a service agent has a public listing").into());
    }
    let result = match (form.action.as_str(), page.current_hub()) {
        (_, None) => Err(HubPublishError::Hub(HubError::Unreachable)),
        ("list", Some(hub)) => {
            publish_to_hub(&page.data_dir, &hub, now_seconds()).await.map(drop)
        }
        ("stop", Some(hub)) => stop_hub_listing(&page.data_dir, &hub).await,
        _ => return Err(bad_request("unknown listing action").into()),
    };
    let error = result
        .err()
        .map(crate::listing::hub_publish_message)
        .unwrap_or_default()
        .to_owned();
    Ok(view! {
        host::host_screen(notice: host::Notice::listing(error))
    })
}
