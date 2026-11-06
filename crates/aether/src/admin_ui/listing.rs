//! The public listing of a service agent on the hub. The listing reuses the
//! text of the Agent Card; this section shows what people see, and lists the
//! agent or stops the listing.

use aether_core::time::now_seconds;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    router::{content::Form, error::bad_request, page},
    view::{View, attributes, component, view},
};

use super::{Page, PageContext, host::host_screen, require_admin_session};
use crate::{
    components::button::{ButtonSize, ButtonVariant, button},
    hub_client::{AgentKind, HubClient, HubError},
    listing::{LISTING_LIFETIME, ListingState, RENEW_BEFORE},
};

/// How often the worker looks at the expiry of the listing.
const RENEW_INTERVAL: Duration = Duration::from_hours(6);

/// Why a listing was not sent.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PublishError {
    NoName,
    Key,
    Store,
    Hub(HubError),
}

/// Signs the current card text for 30 days and sends it to `hub`.
async fn publish(
    data_dir: &Path,
    hub: &HubClient,
    now: u64,
) -> Result<ListingState, PublishError> {
    let name =
        crate::settings::setting::<crate::hub_client::settings::HubName>(data_dir)
            .ok()
            .flatten()
            .ok_or(PublishError::NoName)?;
    let settings = crate::agent_card::settings(data_dir);
    let bytes = crate::listing::listing_bytes(&settings, &name);
    let key = crate::data_dir::DataDir::new(data_dir)
        .endpoint_key()
        .map_err(|_| PublishError::Key)?;
    let expires_at = now.saturating_add(LISTING_LIFETIME.as_secs());
    let signature = crate::listing::sign(&key, &bytes, expires_at);
    let text = URL_SAFE_NO_PAD.encode(&bytes);
    hub.put_listing(&text, &signature, expires_at).await.map_err(PublishError::Hub)?;
    let state = ListingState { listed: true, expires_at, listing: text };
    crate::settings::store_setting::<crate::listing::settings::ListingState>(
        data_dir,
        &Some(state.clone()),
    )
    .map_err(|_| PublishError::Store)?;
    Ok(state)
}

/// Stops the listing on `hub`.
async fn stop(data_dir: &Path, hub: &HubClient) -> Result<(), PublishError> {
    hub.delete_listing().await.map_err(PublishError::Hub)?;
    crate::settings::store_setting::<crate::listing::settings::ListingState>(
        data_dir, &None,
    )
    .map_err(|_| PublishError::Store)
}

const fn publish_message(error: PublishError) -> &'static str {
    match error {
        PublishError::NoName => "Choose your name on Aether first.",
        PublishError::Key | PublishError::Store => {
            "The listing was not saved on this computer. Try again."
        }
        PublishError::Hub(HubError::Invalid) => {
            "The hub refused the listing. Check the text of your agent's card."
        }
        PublishError::Hub(HubError::OldHub) => {
            "This hub is older than this node. Ask its operator to update it."
        }
        PublishError::Hub(HubError::OldNode) => {
            "This node is older than the hub. Update Aether."
        }
        PublishError::Hub(HubError::RateLimited) => {
            "Too many requests. Wait a minute and try again."
        }
        PublishError::Hub(_) => "The hub cannot be reached now. Try again later.",
    }
}

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
        (_, None) => Err(PublishError::Hub(HubError::Unreachable)),
        ("list", Some(hub)) => {
            publish(&page.data_dir, &hub, now_seconds()).await.map(drop)
        }
        ("stop", Some(hub)) => stop(&page.data_dir, &hub).await,
        _ => return Err(bad_request("unknown listing action").into()),
    };
    let error = result.err().map(publish_message).unwrap_or_default().to_owned();
    Ok(view! { host_screen(notice: super::host::Notice::listing(error)) })
}

/// Keeps the listing of a service agent on the hub: when less than 7 days
/// remain, it signs the current card text again.
#[allow(clippy::redundant_pub_crate)]
pub(crate) async fn listing_worker(page: Arc<PageContext>) {
    loop {
        renew(&page).await;
        tokio::time::sleep(RENEW_INTERVAL).await;
    }
}

async fn renew(page: &PageContext) {
    let now = now_seconds();
    let due = crate::settings::setting::<crate::listing::settings::ListingState>(
        &page.data_dir,
    )
    .ok()
    .flatten()
    .is_some_and(|state| {
        state.listed && state.expires_at.saturating_sub(now) < RENEW_BEFORE.as_secs()
    });
    let service = crate::settings::setting::<crate::hub_client::settings::AgentKind>(
        &page.data_dir,
    )
    .ok()
    .flatten()
        == Some(AgentKind::Service);
    let (true, true, Some(hub)) = (due, service, page.current_hub()) else {
        return;
    };
    let event = match publish(&page.data_dir, &hub, now).await {
        Ok(_) => "listing_renewed",
        Err(_) => "listing_renew_failed",
    };
    println!("{}", serde_json::json!({ "event": event }));
}
