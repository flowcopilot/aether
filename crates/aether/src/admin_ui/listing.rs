//! The public listing of a service agent on the hub. The listing reuses the
//! text of the Agent Card; this section shows what people see, and lists the
//! agent or stops the listing.

use aether_core::time::now_seconds;
use std::path::PathBuf;

use topcoat::{
    Result as TopcoatResult,
    view::{View, component, view},
};

use super::shared;
use crate::{
    admin_components::post_button,
    components::button::{ButtonSize, ButtonVariant},
};

/// The two buttons of the listing section, from their hidden `action` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ListingAction {
    List,
    Stop,
}

impl ListingAction {
    /// The field value; `Deserialize` reads the same names back.
    const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Stop => "stop",
        }
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
    let queue = shared::queue_text(&data_dir);
    let status = match days_left {
        Some(days) => format!(
            "Listed. The listing ends in {} unless this page renews it.",
            crate::admin_components::days_label(days)
        ),
        None => "Not listed.".to_owned(),
    };
    Ok(view! {
        <section id="listing" class="flex flex-col gap-4 pt-2" aria-labelledby="listing-title">
            <div class="flex flex-col gap-1.5">
                crate::admin_components::section_heading(id: "listing-title", "Public listing")
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
                    post_button(
                        action: "/listing/publish",
                        fields: vec![("action", ListingAction::List.as_str().to_owned())],
                        size: ButtonSize::Sm,
                        disabled: name.is_none(),
                        (if days_left.is_some() { "List again with the current card" } else { "List this agent" })
                    )
                    if days_left.is_some() {
                        post_button(
                            action: "/listing/publish",
                            fields: vec![("action", ListingAction::Stop.as_str().to_owned())],
                            variant: ButtonVariant::Outline,
                            size: ButtonSize::Sm,
                            "Stop listing"
                        )
                    }
                </div>
            </div>
            if !error.is_empty() {
                crate::admin_components::error_line(text: error.clone())
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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::ListingAction;
    use crate::admin_ui::test_support::{
        app, get_with, host_ui, hub_host_ui, post, service_page, session,
    };
    use aether_store::NodeStore;

    #[test]
    fn a_listing_action_value_reads_back_as_its_action() {
        for action in [ListingAction::List, ListingAction::Stop] {
            let value = serde_json::Value::from(action.as_str());
            assert_eq!(
                serde_json::from_value::<ListingAction>(value).ok(),
                Some(action)
            );
        }
        assert!(serde_json::from_value::<ListingAction>("pause".into()).is_err());
    }

    #[tokio::test]
    async fn a_service_agent_lists_its_card_text() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        let home = get_with(page.clone(), "/", &cookie).await;
        assert!(home.body.contains("Public listing"), "{}", home.body);
        assert!(home.body.contains("People see this"));
        assert!(home.body.contains("Not listed."));
        assert!(home.body.contains("1 waiting task in all and 1 for each person"));
        assert!(home.body.contains("each computer can see the public IP address"));

        let listed =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=list").await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert!(listed.body.contains("Stop listing"));
        let endpoint = crate::direct::endpoint_id(&data_dir).expect("endpoint");
        let held = hub.state().listings.get(&endpoint).cloned().expect("listed");
        let text = held["listing"].as_str().expect("text");
        let expires_at = held["expiresAt"].as_u64().expect("expiry");
        let now = aether_core::time::unix_seconds(std::time::SystemTime::now())
            .expect("the clock must be after 1970");
        crate::listing::verify(
            text,
            held["signature"].as_str().expect("signature"),
            &endpoint,
            expires_at,
            now,
        )
        .expect("the held listing must verify");
        assert!(expires_at > now + 29 * 24 * 60 * 60);
        let listing = crate::listing::parse(text, "law#0001").expect("listing");
        assert_eq!(
            listing.description,
            crate::agent_card::settings(&data_dir).description
        );
        let state = crate::settings::setting::<crate::listing::settings::ListingState>(
            &data_dir,
        )
        .ok()
        .flatten()
        .expect("state");
        assert_eq!(state.listing, text);

        let stopped =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=stop").await;
        assert!(stopped.body.contains("Not listed."), "{}", stopped.body);
        assert!(!hub.state().listings.contains_key(&endpoint));
        assert_eq!(
            crate::settings::setting::<crate::listing::settings::ListingState>(
                &data_dir
            )
            .ok()
            .flatten(),
            None
        );
    }

    #[tokio::test]
    async fn the_queue_text_shows_the_limits_in_force() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        NodeStore::in_dir(&data_dir)
            .expect("store must open")
            .apply_change(aether_store::ChangeKind::Limit, "queue_global", "3")
            .expect("limit must store");
        let home = get_with(page, "/", &cookie).await;
        assert!(
            home.body.contains("3 waiting tasks in all and 1 for each person"),
            "{}",
            home.body
        );
    }

    #[tokio::test]
    async fn a_personal_agent_cannot_list_itself() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_host_ui(directory.path(), &hub);
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=personal").await;
        assert!(
            !get_with(page.clone(), "/", &cookie).await.body.contains("Public listing")
        );
        let refused =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=list").await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(hub.state().listings.is_empty());
    }

    #[tokio::test]
    async fn a_post_without_any_session_cannot_change_the_listing() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        for action in ["action=list", "action=stop"] {
            let response =
                post(application.clone(), "/listing/publish", None, action).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
            assert!(response.set_cookie.is_none(), "a refused POST starts no session");
        }
    }
}
