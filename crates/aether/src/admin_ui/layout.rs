use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    view::{View, component, view},
};

use super::{Page, PageContext, UiMode, administration, shared};
use crate::admin_components::count_badge;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Sharing,
    Agents,
    Advanced,
}

pub(super) fn page_data_dir(cx: &Cx) -> Option<std::path::PathBuf> {
    let Page(page) = app_context::<Page>(cx);
    page.as_ref().map(|page| page.data_dir.clone())
}

/// What waits for the person, as counts on the tabs.
#[derive(Clone, Copy, Default)]
pub(super) struct Waiting {
    /// Access requests that the agent prepared.
    pub drafts: usize,
    /// Changes that wait for confirmation.
    pub pending: usize,
}

impl Waiting {
    fn read(cx: &Cx, page: Option<&PageContext>) -> Self {
        let drafts = page.filter(|page| page.mode.calls()).map_or(0, |page| {
            let now = aether_core::time::now_seconds();
            shared::request_drafts_of(&page.data_dir, now).len()
        });
        let pending = administration(cx).pending().map_or(0, |pending| pending.len());
        Self { drafts, pending }
    }
}

/// The bar on top of a node page. `waiting` saves a second read when the
/// screen already counted what waits.
#[component]
pub(super) async fn node_header(
    cx: &Cx,
    current: Tab,
    #[default] waiting: Option<Waiting>,
) -> TopcoatResult<impl View> {
    let Page(page) = app_context::<Page>(cx);
    let mode = app_context::<Page>(cx).mode();
    let hosts = mode.is_some_and(UiMode::hosts);
    let calls = mode.is_some_and(UiMode::calls);
    let data_dir = page_data_dir(cx);
    let Waiting { drafts, pending } =
        waiting.unwrap_or_else(|| Waiting::read(cx, page.as_deref()));
    let relay_host = page.as_ref().filter(|page| page.mode.hosts()).map(|page| {
        page.relay_url().map(|url| {
            reqwest::Url::parse(&url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or(url)
        })
    });
    let name = data_dir.as_deref().and_then(|dir| {
        crate::settings::setting::<crate::hub_client::settings::HubName>(dir)
            .ok()
            .flatten()
    });
    let tabs = mode.is_some();
    let agents_href = if hosts { "/agents" } else { "/" };
    Ok(view! {
        <header class="border-b border-border">
            <div class="mx-auto flex min-h-14 max-w-6xl flex-wrap items-center gap-x-6 gap-y-2 px-4 sm:px-8">
                <a href="/" class="text-base font-semibold tracking-tight text-foreground no-underline">"Aether"</a>
                if tabs {
                    <nav aria-label="Pages" class="-mb-px flex self-stretch overflow-x-auto">
                        if hosts {
                            node_tab(href: "/", current: current == Tab::Sharing, "Who can use your agent")
                        }
                        if calls {
                            node_tab(
                                href: agents_href,
                                current: current == Tab::Agents,
                                "Agents you use"
                                if drafts > 0 {
                                    count_badge(count: drafts, aria_label: format!("{drafts} requests wait"))
                                }
                            )
                        }
                        node_tab(
                            href: "/advanced",
                            current: current == Tab::Advanced,
                            "Advanced"
                            if pending > 0 {
                                count_badge(count: pending, warning: true, aria_label: format!("{pending} changes wait"))
                            }
                        )
                    </nav>
                }
                <span class="flex-1"></span>
                <div class="flex flex-wrap items-center gap-x-4 gap-y-1 py-2 text-sm">
                    if let Some(relay_host) = &relay_host {
                        match relay_host {
                            Some(reached_through) => crate::admin_components::status_dot(ok: true, text: format!("Reachable through {reached_through}")),
                            None => crate::admin_components::status_dot(ok: false, text: "No relay: only your network can reach it"),
                        }
                    }
                    if let Some(name) = &name {
                        <span class="rounded-md border border-border bg-background px-2.5 py-1 font-mono text-[13px] text-foreground">(name)</span>
                    }
                </div>
            </div>
        </header>
    })
}

#[component]
async fn node_tab(
    href: &'static str,
    current: bool,
    #[default] child: topcoat::view::Child<'_>,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <a
            href=(href)
            aria-current=(current.then_some("page"))
            class=(if current {
                "flex items-center gap-2 border-b-2 border-primary px-3 text-sm font-medium whitespace-nowrap text-foreground no-underline"
            } else {
                "flex items-center gap-2 border-b-2 border-transparent px-3 text-sm whitespace-nowrap text-muted-foreground no-underline hover:text-foreground"
            })
        >
            (child)
        </a>
    })
}
