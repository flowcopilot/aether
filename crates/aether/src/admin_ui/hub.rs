//! Invite and connect by name through the hub (`server/`), instead of two
//! copied codes. The host offers its agent to a name, the other person
//! accepts on their page, and the two workers here do the rest: the host's
//! signs the access for the endpoint that accepted, and the caller's keeps
//! the invite it receives.

use aether_core::time::now;
use std::{
    collections::BTreeMap,
    sync::{Arc, PoisonError},
    time::Duration,
};

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    icon::icon,
    router::{
        HeaderValue,
        content::Form,
        error::{bad_request, internal_server_error},
        header, page,
        response::IntoResponse,
        route,
    },
    view::{View, ViewExt, attributes, component, view},
};

use super::{
    Page, PageContext,
    caller::caller_screen,
    host::{Notice, host_screen, sign_invite},
    page_data_dir, require_admin_session,
};
use crate::hub_client::protocol::{DEFAULT_ACCESS_DAYS, OFFER_DAYS};
use aether_store::{ApprovalKind, ApprovalSide, HubApproval, NodeStore};

use crate::{
    admin::SharedAdministration,
    admin_components::{ALERT, copy_button, field},
    components::{
        button::{ButtonSize, ButtonVariant, button},
        input::input,
    },
    hub_client::{
        Account, AgentKind, Block, DEFAULT_HUB_URL, HubClient, HubError, Invite,
        Origin, OwnerLink, Role, Status, valid_full_name, valid_name_part,
    },
    hub_membership::{APPROVAL_SECONDS, accept, hub_message, keep_hub_name, offer},
    hub_sync::{CallerStep, HostStep, caller_step, host_step},
};

/// How long a page waits for the hub before it shows the page without it.
const PAGE_WAIT: Duration = Duration::from_secs(4);
/// How long a worker waits after the hub fails before it asks again.
const RETRY_WAIT: Duration = Duration::from_secs(5);
/// How long a worker's long poll asks the hub to wait for a change.
const POLL_WAIT: Duration = Duration::from_secs(25);

/// The hub's answer for one page, or why there is none.
enum HubState {
    Ready {
        account: Option<Account>,
        invites: Vec<Invite>,
        blocks: Vec<Block>,
        owner_links: Vec<OwnerLink>,
    },
    Unreachable,
}

async fn load(hub: &HubClient) -> HubState {
    let read = async {
        let account = hub.account().await?;
        let invites = match account {
            Some(_) => hub.invites(None, Duration::ZERO).await?.invites,
            None => Vec::new(),
        };
        // A block or link list that does not load does not hide the rest.
        let (blocks, owner_links) = match &account {
            Some(_) => (
                hub.blocks().await.unwrap_or_default(),
                hub.owner_links().await.unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        Ok::<_, HubError>(HubState::Ready { account, invites, blocks, owner_links })
    };
    tokio::time::timeout(PAGE_WAIT, read)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(HubState::Unreachable)
}

/// The hub part of a page: this node's name, and the invites of one side.
/// `role` is the side this page shows: `Sender` for the host, `Receiver`
/// for the caller.
#[component]
pub(super) async fn hub_section(
    cx: &Cx,
    hub: HubClient,
    role: Role,
    #[default] error: String,
    /// The host has no relay, so only the local network can reach it.
    #[default]
    relay_missing: bool,
    /// The way to connect with a code instead, at the end of the section.
    #[default]
    child: topcoat::view::Child<'_>,
) -> TopcoatResult<impl View> {
    let Page(page) = app_context::<Page>(cx);
    let hosting = page.as_ref().is_some_and(|page| page.host_ui);
    let state = load(&hub).await;
    if let (HubState::Ready { account: Some(account), .. }, Some(data_dir)) =
        (&state, page_data_dir(cx))
    {
        keep_hub_name(&data_dir, &account.handle);
        if crate::settings::setting::<crate::hub_client::settings::AgentKind>(&data_dir)
            .ok()
            .flatten()
            .is_none()
        {
            let _ =
                crate::hub_client::settings::store_agent_kind(&data_dir, account.kind);
        }
    }
    let (unreachable, account, invites, blocks, owner_links) = match state {
        HubState::Ready { account, invites, blocks, owner_links } => (
            false,
            account,
            invites
                .into_iter()
                .filter(|invite| {
                    invite.role == role && invite.status != Status::Unknown
                })
                .collect::<Vec<_>>(),
            blocks,
            owner_links,
        ),
        HubState::Unreachable => (true, None, Vec::new(), Vec::new(), Vec::new()),
    };
    // The ids of the sent invites that this page approved; the worker signs
    // no other.
    let approved: std::collections::BTreeSet<String> =
        page_data_dir(cx)
            .and_then(|data_dir| NodeStore::in_dir(&data_dir).ok())
            .map(|store| {
                invites
                    .iter()
                    .filter(|invite| invite.role == Role::Sender)
                    .filter(|invite| {
                        store.hub_approval(&invite.id).ok().flatten().is_some_and(
                            |approval| approval.side == ApprovalSide::Sender,
                        )
                    })
                    .map(|invite| invite.id.clone())
                    .collect()
            })
            .unwrap_or_default();
    // The page reloads while something moves without the person: the other
    // side's answer, or the two nodes finishing the connection. A request
    // can wait for days, so it does not reload the page.
    let waiting = invites.iter().any(|invite| {
        invite.status != Status::Requested
            && (role == Role::Sender || invite.status != Status::Pending)
    });
    let service =
        account.as_ref().is_some_and(|account| account.kind == AgentKind::Service);
    let queue = page_data_dir(cx)
        .map(|data_dir| super::listing::queue_text(&data_dir))
        .unwrap_or_default();
    let sender = role == Role::Sender;
    let requests =
        invites.iter().filter(|invite| invite.status == Status::Requested).count();
    Ok(view! {
        <div class="flex flex-col gap-5" data-hub-waiting=(waiting.then_some(""))>
            if sender && service {
                <section id="invite" aria-label="Access requests" class="flex flex-col gap-4 rounded-xl border border-border bg-background p-6 shadow-sm">
                    <div class="flex flex-col gap-1.5">
                        <h2 class="flex items-center gap-2 text-lg leading-tight font-semibold">
                            "Access requests"
                            if requests > 0 {
                                <span class="flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-semibold text-primary-foreground">(requests)</span>
                            }
                        </h2>
                        <p class="text-[13px] text-muted-foreground">"This is a service agent."</p>
                        <p class="text-[13px] leading-5 text-muted-foreground">
                            "People who found your listing ask for access here. An approved person joins the group requests. " (&queue)
                        </p>
                    </div>
                    if !error.is_empty() {
                        error_line(text: error.clone())
                    }
                    if invites.is_empty() {
                        <p class="border-t border-border pt-4 text-sm text-muted-foreground">"No request waits."</p>
                    } else {
                        <ul class="flex flex-col divide-y divide-border border-t border-border" aria-label="Requests">
                            for invite in &invites {
                                if invite.status == Status::Requested {
                                    request_row(invite: invite.clone())
                                } else {
                                    invite_row(
                                        invite: invite.clone(),
                                        approved: approved.contains(&invite.id)
                                    )
                                }
                            }
                        </ul>
                    }
                    if !blocks.is_empty() {
                        blocked_list(blocks: blocks.clone())
                    }
                    (child)
                </section>
                owner_claims(links: owner_links.clone())
            } else if sender {
                <section id="invite" aria-labelledby="invite-title" class="flex flex-col gap-5 rounded-xl border border-border bg-background p-6 shadow-sm">
                    <div class="flex flex-col gap-1.5">
                        <h2 id="invite-title" class="text-lg leading-tight font-semibold">"Invite someone"</h2>
                        <p class="text-sm text-muted-foreground">
                            "Type their name on Aether. They accept on their page, and the two computers connect by themselves."
                        </p>
                    </div>
                    if relay_missing {
                        super::host::relay_notice()
                    }
                    if unreachable {
                        <p class="text-sm text-muted-foreground" role="status">
                            "The hub cannot be reached now. You can still use a connect code below."
                        </p>
                    } else {
                        match &account {
                            None => name_form(error: error.clone(), hosting: hosting),
                            Some(account) => {
                                <p class="sr-only">"Your name on Aether is " (&account.handle) "."</p>
                                invite_form(error: error.clone())
                                if !invites.is_empty() {
                                    <ul class="flex flex-col divide-y divide-border border-t border-border" aria-label="Invites">
                                        for invite in &invites {
                                            invite_row(
                                                invite: invite.clone(),
                                                approved: approved.contains(&invite.id)
                                            )
                                        }
                                    </ul>
                                }
                                search_form(searchable: account.searchable)
                            }
                        }
                    }
                    (child)
                </section>
                if !unreachable && account.is_some() {
                    service_agents(links: owner_links.clone())
                }
            } else {
                if unreachable {
                    <p class="flex items-center gap-2 rounded-lg border border-border bg-background px-4 py-3 text-sm" role="status">
                        "The hub cannot be reached now. You can still use a connect code below."
                    </p>
                } else {
                    match &account {
                        None => {
                            <div class="rounded-xl border border-border bg-background p-6">
                                name_form(error: error.clone(), hosting: hosting)
                            </div>
                        }
                        Some(account) => {
                            <div class="flex flex-wrap items-center justify-between gap-4 rounded-xl border border-border bg-background px-5 py-4">
                                <div class="flex flex-col gap-1">
                                    <p class="text-[13px] text-muted-foreground">"Your name on Aether is"</p>
                                    <p class="font-mono text-xl font-medium tracking-wide">(&account.handle)</p>
                                </div>
                                copy_button(text: account.handle.clone())
                            </div>
                            if !error.is_empty() {
                                error_line(text: error.clone())
                            }
                            if !invites.is_empty() {
                                <section aria-labelledby="invites-title" class="flex flex-col gap-3">
                                    <h2 id="invites-title" class="text-xl font-semibold tracking-tight">"Invites"</h2>
                                    <ul class="flex flex-col divide-y divide-border rounded-xl border border-border bg-background px-5" aria-label="Invites">
                                        for invite in &invites {
                                            invite_row(
                                                invite: invite.clone(),
                                                approved: approved.contains(&invite.id)
                                            )
                                        }
                                    </ul>
                                </section>
                            } else {
                                <div class="rounded-xl border border-dashed border-border p-6">
                                    <p class="text-sm text-muted-foreground">
                                        "Tell your friend this name. Their invite appears here."
                                    </p>
                                </div>
                            }
                            if !blocks.is_empty() {
                                blocked_list(blocks: blocks.clone())
                            }
                            search_form(searchable: account.searchable)
                            // A node with a host and a caller page shows its
                            // service agents on the host page only.
                            if !hosting {
                                service_agents(links: owner_links.clone())
                            }
                        }
                    }
                }
                (child)
            }
        </div>
    }
    // The page is deep: on the heap, its view does not fill the stack.
    .boxed())
}

/// A refusal under a form, in the destructive color, with its icon.
#[component]
pub(super) async fn error_line(text: String) -> TopcoatResult<impl View> {
    Ok(view! {
        <p class="flex items-start gap-1.5 text-sm text-destructive" role="alert">
            <span class="mt-0.5 shrink-0">icon(data: ALERT)</span>
            (text)
        </p>
    })
}

/// The service agents that this personal agent claims as its own, with a
/// form to claim one.
#[component]
async fn service_agents(links: Vec<OwnerLink>) -> TopcoatResult<impl View> {
    let links: Vec<OwnerLink> =
        links.into_iter().filter(|link| link.role == "personal").collect();
    Ok(view! {
        <section class="flex flex-col gap-3 rounded-xl border border-border bg-background px-6 py-5" aria-label="Your service agents">
            <h2 class="text-[15px] font-semibold">"Your service agents"</h2>
            <p class="text-[13px] leading-5 text-muted-foreground">
                "When the service agent confirms, its public listing shows your full name. Everyone can see when your service agent is online. If it runs on the same computer as this agent, people can guess when you are online."
            </p>
            if !links.is_empty() {
                <ul class="flex flex-col divide-y divide-border border-y border-border">
                    for link in &links {
                        <li class="flex flex-wrap items-center gap-3 py-2.5">
                            <span class=(if link.confirmed { "mr-auto text-sm text-foreground" } else { "mr-auto text-sm text-muted-foreground" })>
                                (&link.service)
                                (if link.confirmed { " (linked)" } else { " (waits for the service agent to confirm)" })
                            </span>
                            <form method="post" action="/hub/owner/remove">
                                <input type="hidden" name="id" value=(&link.id)>
                                button(
                                    variant: ButtonVariant::Outline,
                                    size: ButtonSize::Sm,
                                    attrs: attributes! { type="submit" },
                                    "Remove"
                                )
                            </form>
                        </li>
                    }
                </ul>
            }
            <form method="post" action="/hub/owner/claim" class="flex items-end gap-2">
                <div class="min-w-0 flex-1">
                field(
                    name: "Full name of your service agent",
                    control_id: "owner-service",
                    input(attrs: attributes! {
                        id="owner-service" name="service" required="" maxlength="37"
                        pattern="[a-z0-9][a-z0-9._-]{0,31}#[0-9]{4}" autocomplete="off" spellcheck="false"
                        placeholder="law#0042"
                    })
                )
                </div>
                button(variant: ButtonVariant::Outline, attrs: attributes! { type="submit" }, "Claim")
            </form>
        </section>
    })
}

/// The owner of this service agent, and the claims that wait for it.
#[component]
async fn owner_claims(links: Vec<OwnerLink>) -> TopcoatResult<impl View> {
    let links: Vec<OwnerLink> =
        links.into_iter().filter(|link| link.role == "service").collect();
    Ok(view! {
        <section class="flex flex-col gap-3 rounded-xl border border-border bg-background px-6 py-5" aria-label="Owner">
            <h2 class="text-[15px] font-semibold">"Owner"</h2>
            <p class="text-[13px] leading-5 text-muted-foreground">
                "Confirm only a claim from your own personal agent. Its full name then shows on your public listing."
            </p>
            if links.is_empty() {
                <p class="border-t border-border pt-3 text-sm text-muted-foreground">"No claim waits."</p>
            } else {
                <ul class="flex flex-col divide-y divide-border border-t border-border">
                    for link in &links {
                        <li class="flex flex-col gap-2 pt-3">
                            <p class="text-sm">
                                <strong>(&link.personal)</strong>
                                (if link.confirmed { " is the owner of this agent." } else { " claims this agent." })
                            </p>
                            <p class="font-mono text-xs break-all text-muted-foreground">
                                "Key " (crate::admin_components::grouped(&link.personal_endpoint.clone().unwrap_or_default()))
                            </p>
                            <div class="flex flex-wrap gap-2">
                                if !link.confirmed {
                                    <form method="post" action="/hub/owner/confirm">
                                        <input type="hidden" name="id" value=(&link.id)>
                                        button(size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Confirm")
                                    </form>
                                }
                                <form method="post" action="/hub/owner/remove">
                                    <input type="hidden" name="id" value=(&link.id)>
                                    button(
                                        variant: ButtonVariant::Outline,
                                        size: ButtonSize::Sm,
                                        attrs: attributes! { type="submit" },
                                        "Remove"
                                    )
                                </form>
                            </div>
                        </li>
                    }
                </ul>
            }
        </section>
    })
}

/// The endpoints that this personal agent blocked, with **Unblock**.
#[component]
async fn blocked_list(blocks: Vec<Block>) -> TopcoatResult<impl View> {
    Ok(view! {
        <section class="flex flex-col gap-2" aria-label="Blocked">
            <h3 class="text-sm font-medium">"Blocked"</h3>
            <ul class="flex flex-col divide-y divide-border border-y border-border">
                for block in &blocks {
                    <li class="flex flex-wrap items-center gap-3 py-2.5">
                        <span class="mr-auto text-sm">
                            (block.handle.clone().unwrap_or_else(|| "deleted account".to_owned()))
                        </span>
                        <form method="post" action="/hub/unblock">
                            <input type="hidden" name="endpoint" value=(&block.endpoint)>
                            button(
                                variant: ButtonVariant::Outline,
                                size: ButtonSize::Sm,
                                attrs: attributes! { type="submit" },
                                "Unblock"
                            )
                        </form>
                    </li>
                }
            </ul>
        </section>
    })
}

/// The form for the first name. A host chooses the type of its agent; a
/// page without a host has only a personal agent.
#[component]
async fn name_form(error: String, hosting: bool) -> TopcoatResult<impl View> {
    Ok(view! {
        <form method="post" action="/hub/name" class="flex flex-col gap-3">
            field(
                name: "Choose your name on Aether",
                control_id: "name",
                hint: "Lowercase letters, digits, dot, dash or underscore. The hub adds four digits, such as bob#0042. Give your friends the full name.",
                input(attrs: attributes! {
                    id="name" name="name" required="" maxlength="32"
                    pattern="[a-z0-9][a-z0-9._-]*" autocomplete="off" spellcheck="false"
                    placeholder="bob"
                })
            )
            if hosting {
                <fieldset class="flex flex-col gap-2 text-sm">
                    <legend class="mb-2 font-medium">"Type of agent"</legend>
                    <label class="flex cursor-pointer items-start gap-2.5 rounded-lg border border-border p-3 has-[:checked]:border-primary has-[:checked]:bg-primary/5">
                        <input type="radio" name="kind" value="personal" checked="" class="mt-0.5 accent-[var(--primary)]">
                        <span class="flex flex-col gap-0.5">
                            <span class="text-[13px] leading-5">"Personal agent: it stands for you. Make only one for each person. Friends you invite send it tasks, and it answers at once."</span>
                        </span>
                    </label>
                    <label class="flex cursor-pointer items-start gap-2.5 rounded-lg border border-border p-3 has-[:checked]:border-primary has-[:checked]:bg-primary/5">
                        <input type="radio" name="kind" value="service" class="mt-0.5 accent-[var(--primary)]">
                        <span class="flex flex-col gap-0.5">
                            <span class="text-[13px] leading-5">"Service agent: it offers a service to people you do not know. You list it and approve each request on this page."</span>
                        </span>
                    </label>
                    <p class="text-xs text-muted-foreground">"The type does not change later."</p>
                </fieldset>
            }
            if !error.is_empty() {
                error_line(text: error.clone())
            }
            button(attrs: attributes! { type="submit" class="w-full" }, "Save name")
        </form>
    })
}

/// Name search for a personal agent: off by default.
#[component]
async fn search_form(searchable: bool) -> TopcoatResult<impl View> {
    Ok(view! {
        <form method="post" action="/hub/searchable" class="flex items-start gap-3 border-t border-border pt-4 text-[13px] leading-5">
            <label class="flex flex-1 items-start gap-2.5">
                <input type="checkbox" name="searchable" value="on" checked=(searchable.then_some("")) class="mt-0.5 accent-[var(--primary)]">
                <span>"Let people find me by name search. When this is off, people need your full name, such as bob#0042."</span>
            </label>
            button(
                variant: ButtonVariant::Outline,
                size: ButtonSize::Sm,
                attrs: attributes! { type="submit" },
                "Save"
            )
        </form>
    })
}

#[component]
async fn invite_form(error: String) -> TopcoatResult<impl View> {
    Ok(view! {
        <form method="post" action="/hub/invite" class="flex flex-col gap-4">
            <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
                field(
                    name: "Their name on Aether",
                    control_id: "to",
                    input(attrs: attributes! {
                        id="to" name="to" required="" maxlength="37"
                        pattern="[a-z0-9][a-z0-9._-]{0,31}#[0-9]{4}" autocomplete="off" spellcheck="false"
                        placeholder="bob#0042"
                    })
                )
                field(
                    name: "Access for",
                    control_id: "hub-days",
                    <select
                        id="hub-days"
                        name="days"
                        class="h-9 rounded-lg border border-border bg-background px-3 text-sm shadow-xs"
                    >
                        for days in OFFER_DAYS {
                            <option value=(days) selected=((days == DEFAULT_ACCESS_DAYS).then_some(""))>
                                (if days == 1 { "1 day".to_owned() } else { format!("{days} days") })
                            </option>
                        }
                    </select>
                )
            </div>
            if !error.is_empty() {
                error_line(text: error.clone())
            }
            button(attrs: attributes! { type="submit" class="w-full" }, "Invite")
        </form>
    })
}

/// A request that waits for the owner of this service agent. Approve has
/// only the days: the person always joins the group requests.
#[component]
async fn request_row(invite: Invite) -> TopcoatResult<impl View> {
    let endpoint = invite.receiver_endpoint.clone().unwrap_or_default();
    let selected = if OFFER_DAYS.contains(&invite.days) {
        invite.days
    } else {
        DEFAULT_ACCESS_DAYS
    };
    Ok(view! {
        <li class="flex flex-col gap-2.5 py-3.5">
            <p class="text-sm">
                <strong>(&invite.to)</strong>
                " asks for access for "
                (if invite.days == 1 { "1 day".to_owned() } else { format!("{} days", invite.days) })
                "."
            </p>
            <p class="font-mono text-xs break-all text-muted-foreground">"Key " (crate::admin_components::grouped(&endpoint))</p>
            <div class="flex flex-wrap items-center gap-2">
                <form method="post" action="/hub/approve" class="flex items-center gap-2">
                    <input type="hidden" name="id" value=(&invite.id)>
                    <select
                        name="days"
                        aria-label=(format!("Access for {}", invite.to))
                        class="h-8 rounded-lg border border-border bg-background px-2 text-sm"
                    >
                        for days in OFFER_DAYS {
                            <option value=(days) selected=((days == selected).then_some(""))>
                                (if days == 1 { "1 day".to_owned() } else { format!("{days} days") })
                            </option>
                        }
                    </select>
                    button(size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Approve")
                </form>
                <form method="post" action="/hub/remove">
                    <input type="hidden" name="id" value=(&invite.id)>
                    button(
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="submit" },
                        "Decline"
                    )
                </form>
                <form method="post" action="/hub/block-request" title="Block hides this key's requests from you. It sees the same as before.">
                    <input type="hidden" name="id" value=(&invite.id)>
                    button(
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="submit" },
                        "Block"
                    )
                </form>
            </div>
        </li>
    })
}

#[component]
async fn invite_row(invite: Invite, approved: bool) -> TopcoatResult<impl View> {
    let days = if invite.days == 1 {
        "1 day".to_owned()
    } else {
        format!("{} days", invite.days)
    };
    let text = match (invite.role, invite.status) {
        (Role::Sender, _) if !approved => {
            "This invite was made before an update. Cancel it and invite again."
                .to_owned()
        }
        (Role::Sender, Status::Pending) => {
            format!("Waiting for {} to accept", invite.to)
        }
        (Role::Sender, _) => format!("Connecting {}…", invite.to),
        (Role::Receiver, Status::Pending) => {
            format!("{} wants to share their agent with you for {days}", invite.from)
        }
        (Role::Receiver, Status::Requested) => {
            format!("Waiting for {} to approve", invite.from)
        }
        (Role::Receiver, _) => format!("Connecting to {}…", invite.from),
    };
    let can_accept = invite.role == Role::Receiver && invite.status == Status::Pending;
    let remove_label = match (invite.role, invite.status) {
        (Role::Sender, _) | (Role::Receiver, Status::Requested) => "Cancel",
        (Role::Receiver, _) => "Decline",
    };
    Ok(view! {
        <li class="flex flex-wrap items-center gap-3 py-3">
            if matches!(invite.status, Status::Accepted | Status::Delivered) {
                <span class="size-3.5 shrink-0 animate-spin rounded-full border-2 border-border border-t-primary" aria-hidden="true"></span>
            }
            <span class="mr-auto text-sm">(text)</span>
            if can_accept {
                <form method="post" action="/hub/accept">
                    <input type="hidden" name="id" value=(&invite.id)>
                    button(size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Accept")
                </form>
            }
            if invite.role == Role::Receiver && invite.status == Status::Pending {
                <form method="post" action="/hub/block" title="Block hides this person's invites from you. They see the same as after a decline.">
                    <input type="hidden" name="id" value=(&invite.id)>
                    button(
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="submit" },
                        "Block"
                    )
                </form>
            }
            if matches!(invite.status, Status::Pending | Status::Requested)
                || (invite.role == Role::Sender && !approved)
            {
                <form method="post" action="/hub/remove">
                    <input type="hidden" name="id" value=(&invite.id)>
                    button(
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="submit" },
                        (remove_label)
                    )
                </form>
            }
        </li>
    })
}

/// The hub of this page's side, and the page to show after a hub form.
fn page_hub(cx: &Cx) -> TopcoatResult<HubClient> {
    current_hub(cx).ok_or_else(|| bad_request("this page has no hub").into())
}

/// The hub of this page's side now, if there is one.
fn current_hub(cx: &Cx) -> Option<HubClient> {
    let Page(page) = app_context::<Page>(cx);
    page.as_ref().and_then(|page| page.current_hub())
}

#[component]
async fn screen(
    cx: &Cx,
    #[default] error: String,
    #[default] setting_error: String,
) -> TopcoatResult<impl View> {
    let Page(page) = app_context::<Page>(cx);
    let hosting = page.as_ref().is_some_and(|page| page.host_ui);
    Ok(view! {
        if hosting {
            host_screen(notice: Notice::hub(error.clone(), setting_error.clone()))
        } else {
            caller_screen(hub_error: error.clone(), hub_setting_error: setting_error.clone())
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NameForm {
    name: String,
    #[serde(default)]
    kind: Option<String>,
}

/// Claims this node's name on the hub.
#[page(POST "/hub/name")]
pub(super) async fn claim_name(
    cx: &Cx,
    Form(form): Form<NameForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let Page(page) = app_context::<Page>(cx);
    let data_dir = page_data_dir(cx);
    let name = form.name.trim();
    // A page without a host has only a personal agent.
    let host_ui = page.as_ref().is_some_and(|page| page.host_ui);
    let chosen = match (host_ui, form.kind.as_deref()) {
        (false, _) | (true, None) => Some(AgentKind::Personal),
        (true, Some(kind)) => AgentKind::parse(kind),
    };
    let stored = data_dir.as_deref().and_then(|dir| {
        crate::settings::setting::<crate::hub_client::settings::AgentKind>(dir)
            .ok()
            .flatten()
    });
    let error = match chosen {
        _ if !valid_name_part(name) => {
            "Use lowercase letters, digits, dots, dashes or underscores, up to 32."
        }
        None => "Choose a personal agent or a service agent.",
        Some(kind) if stored.is_some_and(|stored| stored != kind) => {
            "This node already has an agent of the other type. The type does not change."
        }
        Some(kind) => {
            // The hub sets the type once: only the first claim names it.
            let first = matches!(hub.account().await, Ok(None));
            match hub.claim(name, first.then_some(kind)).await {
                Ok(account) => {
                    if let Some(data_dir) = &data_dir {
                        keep_hub_name(data_dir, &account.handle);
                        let _ = crate::hub_client::settings::store_agent_kind(
                            data_dir,
                            account.kind,
                        );
                    }
                    ""
                }
                Err(error) => hub_message(error),
            }
        }
    };
    Ok(view! { screen(error: error.to_owned()) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchForm {
    #[serde(default)]
    searchable: Option<String>,
}

/// Turns name search on or off for a personal agent.
#[page(POST "/hub/searchable")]
pub(super) async fn set_search(
    cx: &Cx,
    Form(form): Form<SearchForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let searchable = form.searchable.as_deref() == Some("on");
    let error = match hub.set_searchable(searchable).await {
        Ok(_) => "",
        Err(HubError::Invalid) => "Only a personal agent can be found by name search.",
        Err(error) => hub_message(error),
    };
    Ok(view! { screen(error: error.to_owned()) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InviteForm {
    to: String,
    days: u64,
}

/// Offers this node's agent to a name.
#[page(POST "/hub/invite")]
pub(super) async fn invite_by_name(
    cx: &Cx,
    Form(form): Form<InviteForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(bad_request("this node cannot invite").into());
    };
    let hub = page.current_hub().ok_or_else(|| bad_request("this node has no hub"))?;
    let to = form.to.trim();
    let error = if !valid_full_name(to) {
        "Use their full name on Aether, with the four digits, such as bob#0042."
    } else if !OFFER_DAYS.contains(&form.days) {
        "Choose how long they can use your agent."
    } else {
        offer(&hub, &page.data_dir, to, form.days).await.err().unwrap_or_default()
    };
    Ok(view! { screen(error: error.to_owned()) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InviteIdForm {
    id: String,
}

/// Accepts an invite. The host's node then sends the access by itself.
#[page(POST "/hub/accept")]
pub(super) async fn accept_invite(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(bad_request("this node cannot accept invites").into());
    };
    if !page.caller {
        return Err(bad_request("this node cannot accept invites").into());
    }
    let hub = page_hub(cx)?;
    let error = match accept(&hub, &page.data_dir, &form.id).await {
        Ok(_) => "",
        Err(error) => hub_message(error),
    };
    Ok(view! { caller_screen(hub_error: error.to_owned()) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApproveForm {
    id: String,
    days: u64,
}

/// Approves an access request to this service agent. The worker then signs
/// the access, in the group requests, for the key shown on the page.
#[page(POST "/hub/approve")]
pub(super) async fn approve_request(
    cx: &Cx,
    Form(form): Form<ApproveForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(bad_request("this node has no access requests").into());
    };
    let hub = page.current_hub().ok_or_else(|| bad_request("this node has no hub"))?;
    let error = if OFFER_DAYS.contains(&form.days) {
        approve(&hub, &page.data_dir, &form.id, form.days)
            .await
            .err()
            .map(hub_message)
            .unwrap_or_default()
    } else {
        "Choose how long they can use your agent."
    };
    Ok(view! { screen(error: error.to_owned()) })
}

/// The open request `id` to this service agent, with the endpoint that
/// asked, from the hub's list.
async fn open_request(hub: &HubClient, id: &str) -> Result<(Invite, String), HubError> {
    let invite = hub
        .invites(None, Duration::ZERO)
        .await?
        .invites
        .into_iter()
        .find(|invite| {
            invite.id == id
                && invite.role == Role::Sender
                && invite.origin == Origin::Request
        })
        .ok_or(HubError::Forbidden)?;
    let endpoint = invite
        .receiver_endpoint
        .clone()
        .filter(|endpoint| endpoint.parse::<iroh::EndpointId>().is_ok())
        .filter(|_| valid_full_name(&invite.to))
        .ok_or(HubError::Forbidden)?;
    Ok((invite, endpoint))
}

/// Approves the request `id` for `days` days: first the local approval,
/// pinned to the endpoint on the page, then the answer to the hub.
async fn approve(
    hub: &HubClient,
    data_dir: &std::path::Path,
    id: &str,
    days: u64,
) -> Result<(), HubError> {
    let (invite, endpoint) = open_request(hub, id).await?;
    if invite.status != Status::Requested {
        return Err(HubError::Forbidden);
    }
    let store = NodeStore::in_dir(data_dir).map_err(|_| HubError::Unexpected)?;
    if store.hub_blocked(&endpoint).unwrap_or(true) {
        return Err(HubError::Forbidden);
    }
    let now = now();
    store
        .put_hub_approval(&HubApproval {
            invite_id: invite.id.clone(),
            side: ApprovalSide::Sender,
            kind: ApprovalKind::Request,
            peer_name: invite.to.clone(),
            peer_endpoint: Some(endpoint),
            days: u32::try_from(days).map_err(|_| HubError::Invalid)?,
            access_group: crate::access_groups::REQUESTS_GROUP.to_owned(),
            bundle: None,
            created_at: now,
            expires_at: now.saturating_add(APPROVAL_SECONDS),
            listing: None,
        })
        .map_err(|_| HubError::Unexpected)?;
    if let Err(error) = hub.approve(id, days).await {
        let _ = store.remove_hub_approval(id);
        return Err(error);
    }
    Ok(())
}

/// Blocks the key that asked this service agent for access. The node keeps
/// the block too, so it never signs for that key.
#[page(POST "/hub/block-request")]
pub(super) async fn block_request(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(bad_request("this node has no access requests").into());
    };
    let hub = page.current_hub().ok_or_else(|| bad_request("this node has no hub"))?;
    let error = block_requester(&hub, page, &form.id)
        .await
        .err()
        .map(hub_message)
        .unwrap_or_default();
    Ok(view! { screen(error: error.to_owned()) })
}

async fn block_requester(
    hub: &HubClient,
    page: &PageContext,
    id: &str,
) -> Result<(), HubError> {
    let (invite, endpoint) = open_request(hub, id).await?;
    {
        let _sign = page.hub_sign().lock().unwrap_or_else(PoisonError::into_inner);
        let store =
            NodeStore::in_dir(&page.data_dir).map_err(|_| HubError::Unexpected)?;
        store
            .put_hub_block(&endpoint, &invite.to, now())
            .map_err(|_| HubError::Unexpected)?;
        let _ = store.remove_hub_approval(id);
    }
    hub.block(&endpoint).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerClaimForm {
    service: String,
}

/// The kind of this page's agent on the hub, and its endpoint key.
async fn page_agent(
    cx: &Cx,
    hub: &HubClient,
) -> TopcoatResult<(AgentKind, iroh::SecretKey)> {
    let kind = match hub.account().await {
        Ok(Some(account)) => account.kind,
        _ => return Err(bad_request("this node has no name on the hub").into()),
    };
    let data_dir =
        page_data_dir(cx).ok_or_else(|| bad_request("this page has no node"))?;
    let key =
        crate::data_dir::DataDir::new(&data_dir).endpoint_key().map_err(|_| {
            internal_server_error(std::io::Error::other("the endpoint key cannot load"))
        })?;
    Ok((kind, key))
}

/// A personal agent claims a service agent as its own.
#[page(POST "/hub/owner/claim")]
pub(super) async fn claim_owner(
    cx: &Cx,
    Form(form): Form<OwnerClaimForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let (kind, key) = page_agent(cx, &hub).await?;
    if kind != AgentKind::Personal {
        return Err(bad_request("only a personal agent claims a service agent").into());
    }
    let service = form.service.trim();
    let error = if valid_full_name(service) {
        let expires_at = u64::try_from(now())
            .unwrap_or_default()
            .saturating_add(crate::listing::OWNER_LINK_LIFETIME.as_secs());
        let signature = crate::listing::sign_owner_claim(&key, service, expires_at);
        match hub.claim_owner_link(service, expires_at, &signature).await {
            Ok(_) => "",
            Err(HubError::NotFound) => "No service agent has this name on Aether.",
            Err(error) => hub_message(error),
        }
    } else {
        "Use the full name of the service agent, with the four digits, such as law#0042."
    };
    Ok(view! { screen(error: error.to_owned()) })
}

/// The service agent confirms the claim of its owner's personal agent.
#[page(POST "/hub/owner/confirm")]
pub(super) async fn confirm_owner(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let (kind, key) = page_agent(cx, &hub).await?;
    if kind != AgentKind::Service {
        return Err(bad_request("only a service agent confirms an owner").into());
    }
    let error = confirm_claim(&hub, &key, &form.id).await.err().unwrap_or_default();
    Ok(view! { screen(error: error.to_owned()) })
}

/// Signs the confirm of the open claim `id` with the endpoint `key`.
async fn confirm_claim(
    hub: &HubClient,
    key: &iroh::SecretKey,
    id: &str,
) -> Result<(), &'static str> {
    let links = hub.owner_links().await.map_err(hub_message)?;
    let (personal, expires_at) = links
        .into_iter()
        .find(|link| link.id == id && link.role == "service" && !link.confirmed)
        .and_then(|link| {
            link.personal_endpoint
                .filter(|endpoint| endpoint.parse::<iroh::EndpointId>().is_ok())
                .map(|endpoint| (endpoint, link.expires_at))
        })
        .ok_or("This claim changed. Look at the list again.")?;
    let signature = crate::listing::sign_owner_confirm(key, &personal, expires_at);
    hub.confirm_owner_link(id, &signature).await.map(drop).map_err(
        |error| match error {
            HubError::Limit => "This agent has an owner. Remove that link first.",
            error => hub_message(error),
        },
    )
}

/// Either side removes an owner link or claim.
#[page(POST "/hub/owner/remove")]
pub(super) async fn remove_owner(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let error = hub
        .delete_owner_link(&form.id)
        .await
        .err()
        .map(hub_message)
        .unwrap_or_default();
    Ok(view! { screen(error: error.to_owned()) })
}

/// Blocks the sender of a received invite.
#[page(POST "/hub/block")]
pub(super) async fn block_invite(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let error =
        hub.block_invite(&form.id).await.err().map(hub_message).unwrap_or_default();
    Ok(view! { screen(error: error.to_owned()) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnblockForm {
    endpoint: String,
}

/// Removes a block.
#[page(POST "/hub/unblock")]
pub(super) async fn unblock(
    cx: &Cx,
    Form(form): Form<UnblockForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let error =
        hub.unblock(&form.endpoint).await.err().map(hub_message).unwrap_or_default();
    if error.is_empty()
        && let Some(store) =
            page_data_dir(cx).and_then(|data_dir| NodeStore::in_dir(&data_dir).ok())
    {
        let _ = store.remove_hub_block(&form.endpoint);
    }
    Ok(view! { screen(error: error.to_owned()) })
}

/// Declines or cancels an invite.
#[page(POST "/hub/remove")]
pub(super) async fn remove_invite(
    cx: &Cx,
    Form(form): Form<InviteIdForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let hub = page_hub(cx)?;
    let error = hub.remove(&form.id).await.err().map(hub_message).unwrap_or_default();
    Ok(view! { screen(error: error.to_owned()) })
}

/// Which hub names go through, as a row of the Connection card, and a form
/// to change it. The form is open while there is no hub or when it refused
/// the last input.
#[component]
pub(super) async fn hub_setting(
    url: Option<String>,
    #[default] error: String,
) -> TopcoatResult<impl View> {
    let open = url.is_none() || !error.is_empty();
    let value = url.clone().unwrap_or_default();
    Ok(view! {
        <div id="hub" class="py-3">
            <details class="group" open=(open.then_some(""))>
                <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
                    <span class="flex min-w-0 flex-col gap-0.5">
                        match &url {
                            Some(url) => {
                                <span class="text-[13px] text-muted-foreground">"Names go through"</span>
                                <code class="font-mono text-[13px] break-all text-foreground">(url)</code>
                            }
                            None => <span class="text-[13px] text-muted-foreground">"No hub: connect with a code."</span>,
                        }
                    </span>
                    <span class="shrink-0 text-[13px] font-medium text-primary underline underline-offset-4">
                        (if url.is_some() { "Change hub" } else { "Set a hub" })
                    </span>
                </summary>
                <form method="post" action="/hub/url" class="mt-3 flex flex-col gap-2">
                    field(
                        name: "Hub URL",
                        control_id: "hub_url",
                        hint: format!("The public hub is {DEFAULT_HUB_URL}. Leave it empty to turn names off."),
                        input(attrs: attributes! {
                            id="hub_url" name="hub_url" type="url" value=(&value)
                            placeholder=(DEFAULT_HUB_URL) class="font-mono"
                            spellcheck="false" autocomplete="off"
                            aria-invalid=((!error.is_empty()).then_some("true"))
                        })
                    )
                    if !error.is_empty() {
                        <p class="flex items-center gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (&error)</p>
                    }
                    button(variant: ButtonVariant::Outline, size: ButtonSize::Sm, attrs: attributes! { type="submit" class="w-fit" }, "Use this hub")
                </form>
            </details>
        </div>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HubUrlForm {
    hub_url: String,
}

/// Changes the hub of this page and keeps it for the next start. An empty
/// URL turns the hub off; the public hub is kept as "no setting", so it
/// follows the default.
#[page(POST "/hub/url")]
pub(super) async fn hub_url(
    cx: &Cx,
    Form(form): Form<HubUrlForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let Page(page) = app_context::<Page>(cx);
    let Some(page) = page.as_ref() else {
        return Err(bad_request("this page has no hub").into());
    };
    let data_dir = page.data_dir.clone();
    let url = form.hub_url.trim().trim_end_matches('/');
    let chosen = crate::hub_client::hub_setting(url).ok().and_then(|stored| {
        if url.is_empty() {
            Some((stored, None))
        } else {
            HubClient::for_data_dir(url, &data_dir).ok().map(|hub| (stored, Some(hub)))
        }
    });
    let setting_error = match chosen {
        Some((stored, hub)) => {
            crate::hub_client::set_hub_url(&data_dir, &stored)
                .map_err(internal_server_error)?;
            page.set_hub(hub);
            String::new()
        }
        None => "Use an https:// address, such as the public hub.".to_owned(),
    };
    Ok(view! { screen(setting_error: setting_error) })
}

/// Whether each contact on the hub is online, by endpoint ID, for the
/// presence dots. Empty without a hub or an answer.
#[route(GET "/hub/presence")]
pub(super) async fn presence(cx: &Cx) -> TopcoatResult<impl IntoResponse> {
    require_admin_session(cx).await?;
    let presence: BTreeMap<String, bool> = match current_hub(cx) {
        Some(hub) => tokio::time::timeout(PAGE_WAIT, hub.contacts())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(|contact| (contact.endpoint, contact.online))
            .collect(),
        None => BTreeMap::new(),
    };
    let body = serde_json::to_string(&presence).map_err(internal_server_error)?;
    Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))], body))
}

/// Follows the invites on the hub that `current` names, and gives `act`
/// each list. The hub can change or go away while this runs: a new hub
/// starts a new list, and a failed request waits [`RETRY_WAIT`].
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
            // No name yet, or no answer: ask again later.
            version = None;
            tokio::time::sleep(RETRY_WAIT).await;
        }
    }
}

/// The host's worker: when someone accepts, sign their access for the
/// endpoint that accepted and send the invite line.
#[allow(clippy::redundant_pub_crate)]
pub(crate) async fn host_worker(
    administration: SharedAdministration,
    page: Arc<PageContext>,
) {
    let current = {
        let page = Arc::clone(&page);
        move || page.current_hub()
    };
    follow(current, |hub, invites| {
        let administration = administration.clone();
        let page = Arc::clone(&page);
        async move {
            let Ok(store) = NodeStore::in_dir(&page.data_dir) else {
                return;
            };
            let _ = store.clear_expired_hub_approvals(now());
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

/// What the host worker does with one hub row after the store part.
enum HostAction {
    Skip,
    Refuse,
    Failed,
    Deliver(String),
}

/// The store part of the host worker for one row: read the approval and
/// the block, then sign and pin. It holds `hub_sign`, so a block from the
/// page lands before the block check or after the pin. A grant whose line
/// cannot be pinned is revoked: the next pass would sign a second one.
fn host_action(
    administration: &SharedAdministration,
    page: &PageContext,
    store: &NodeStore,
    invite: &Invite,
    own_name: Option<&str>,
) -> HostAction {
    let _sign = page.hub_sign().lock().unwrap_or_else(PoisonError::into_inner);
    let approval = store.hub_approval(&invite.id).ok().flatten();
    // A block that cannot be read counts as a block.
    let blocked = invite
        .receiver_endpoint
        .as_deref()
        .is_some_and(|endpoint| store.hub_blocked(endpoint).unwrap_or(true));
    match host_step(invite, approval.as_ref(), own_name, blocked) {
        HostStep::Skip => HostAction::Skip,
        HostStep::Refuse => HostAction::Refuse,
        HostStep::Deliver(line) => HostAction::Deliver(line),
        HostStep::Sign { friend_name, endpoint, days, access_group } => {
            let (Some(own_name), Some(approval)) = (own_name, approval.as_ref()) else {
                return HostAction::Skip;
            };
            let Ok((line, grant_id)) = sign_invite(
                administration,
                page,
                own_name,
                &friend_name,
                &endpoint,
                days,
                &access_group,
            ) else {
                return HostAction::Failed;
            };
            if store.pin_hub_approval(approval, &endpoint, &line).is_err() {
                let _ = administration
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .revoke(&grant_id, aether_store::AuditActor::Ui);
                return HostAction::Failed;
            }
            HostAction::Deliver(line)
        }
    }
}

/// The caller's worker: keep each delivered invite line that is for this
/// computer, then remove the invite from the hub.
#[allow(clippy::redundant_pub_crate)]
pub(crate) async fn caller_worker(page: Arc<PageContext>) {
    let current = {
        let page = Arc::clone(&page);
        move || page.current_hub()
    };
    follow(current, |hub, invites| {
        let page = Arc::clone(&page);
        async move {
            let Ok(store) = NodeStore::in_dir(&page.data_dir) else {
                return;
            };
            for invite in invites {
                let approval = store.hub_approval(&invite.id).ok().flatten();
                match caller_step(&invite, approval.as_ref()) {
                    CallerStep::Skip => continue,
                    CallerStep::Import(bundle) => {
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
                    CallerStep::Refuse(reason) => {
                        log_reason("hub_invite_refused", &invite, reason);
                    }
                }
                // A line that this node did not keep cannot become valid,
                // so it is removed too.
                let _ = store.remove_hub_approval(&invite.id);
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
