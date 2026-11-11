//! The host screen of `node serve --admin-port`: who can use the agent, the
//! folder it works in, and the form that invites someone.

use std::time::SystemTime;

use aether_core::time::now_seconds;

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    router::{
        StatusCode,
        content::Form,
        error::{bad_request, internal_server_error},
        page,
    },
    view::{View, ViewExt, attributes, component, view},
};

use super::{
    Page, PageContext, admin_page, administration, ensure_admin_session,
    hub::{hub_section, hub_setting},
    layout::{Tab, Waiting, node_header},
    listing::listing_section,
    shared::{self, Notice},
};
use crate::access_groups::{self, Action};
use crate::hub_client::{HubClient, Role};
use crate::invites::{DirectInviteSigning, sign_direct_invite};
use crate::{
    admin::SharedAdministration,
    admin_components::{
        ALERT, CHECK, CHEVRON, CopyTone, ago, copy_field, field, page_title,
        presence_dot,
    },
    components::{
        alert::{AlertVariant, alert, alert_description, alert_title},
        avatar::{AvatarSize, avatar, avatar_fallback},
        badge::{BadgeVariant, badge},
        button::{ButtonSize, ButtonVariant, button},
        input::input,
        table::{table, table_body, table_cell, table_head, table_header, table_row},
    },
};
use aether_store::{LastTask, TaskState};
use topcoat::icon::icon;

/// A new invite, shown once on the page that created it.
#[derive(Clone, Default)]
pub(super) struct NewInvitation {
    friend_name: String,
    line: String,
}

/// One grant in the direct scope, as the host sees it.
struct Friend {
    grant_id: String,
    name: String,
    endpoint_id: String,
    days_left: u64,
    last_task: Option<LastTask>,
}

#[component]
pub(super) async fn host_screen(
    cx: &Cx,
    #[default] invitation: NewInvitation,
    #[default] notice: Notice,
) -> TopcoatResult<impl View> {
    ensure_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(internal_server_error(std::io::Error::other(
            "the host screen needs a node",
        ))
        .into());
    };
    let drafts = if page.mode.calls() {
        let now = aether_core::time::now_seconds();
        super::shared::request_drafts_of(&page.data_dir, now).len()
    } else {
        0
    };
    let relay_url = page.relay_url();
    let (grants, last_tasks, folder, pending) = {
        let administration = administration(cx);
        (
            administration.grants().map_err(|_| {
                internal_server_error(std::io::Error::other("grants unavailable"))
            })?,
            administration.last_tasks(crate::direct::SCOPE).map_err(|_| {
                internal_server_error(std::io::Error::other("tasks unavailable"))
            })?,
            administration.current_workspace().unwrap_or_default(),
            administration
                .pending()
                .map_err(|_| {
                    internal_server_error(std::io::Error::other("pending unavailable"))
                })?
                .len(),
        )
    };
    let now = SystemTime::now();
    let groups = access_groups::list(&page.data_dir)
        .map_err(|error| internal_server_error(std::io::Error::other(error)))?;
    let mut friends = grants
        .into_iter()
        .filter(|grant| grant.scope_id.as_str() == crate::direct::SCOPE)
        .map(|grant| Friend {
            days_left: grant
                .expires_at
                .duration_since(now)
                .map_or(0, |left| left.as_secs().div_ceil(24 * 60 * 60)),
            last_task: last_tasks
                .iter()
                .find(|task| {
                    task.caller_endpoint_id == grant.subject_endpoint_id.as_str()
                })
                .cloned(),
            grant_id: grant.id.as_str().to_owned(),
            name: grant.subject_agent_id.as_str().to_owned(),
            endpoint_id: grant.subject_endpoint_id.as_str().to_owned(),
        })
        .collect::<Vec<_>>();
    friends.sort_by(|left, right| left.name.cmp(&right.name));
    let friend_choices = friends
        .iter()
        .map(|friend| (friend.name.clone(), friend.endpoint_id.clone()))
        .collect::<Vec<_>>();
    let refused = notice.refused();
    let command = format!("aether node direct import '{}'", invitation.line);
    let hub_access = page.hub_access();
    let hub = page.current_hub();
    let service = hub.is_some()
        && crate::settings::setting::<crate::hub_client::settings::AgentKind>(
            &page.data_dir,
        )
        .ok()
        .flatten()
            == Some(crate::hub_client::AgentKind::Service);
    // The code form is the second way when a hub is set; it opens when it
    // is in use.
    let code_open = !invitation.line.is_empty() || !notice.invite_error().is_empty();

    let agent_name = page.agent_name();
    Ok(view! {
        if refused {
            (StatusCode::BAD_REQUEST)
        }
        node_header(current: Tab::Sharing, waiting: Some(Waiting { drafts, pending }))
        <main id="main" class="mx-auto flex max-w-6xl flex-col gap-12 px-4 py-10 sm:px-8">
            <div class="grid items-start gap-10 lg:grid-cols-[minmax(0,1fr)_24rem]">
                <section aria-labelledby="people" class="flex min-w-0 flex-col gap-7">
                    <div id="people">
                        page_title(
                            title: "Who can use your agent",
                            workspace_line(path: folder, error: notice.workspace_error(), task_folder_mode: page.task_folder_mode)
                        )
                    </div>
                    if drafts > 0 {
                        alert(
                            alert_title("Your agent prepared access requests")
                            alert_description(
                                (if drafts == 1 { "1 request waits. ".to_owned() } else { format!("{drafts} requests wait. ") })
                                <a href="/agents#requests">"Confirm or discard them"</a>
                                "."
                            )
                        )
                    }
                    if pending > 0 {
                        alert(
                            variant: AlertVariant::Warning,
                            alert_title("A change waits for your confirmation")
                            alert_description(
                                "An agent asked to change this node. "
                                <a href="/advanced#pending">"Review it in Advanced"</a>
                                "."
                            )
                        )
                    }
                    people(friends: friends, confirm_revoke: notice.confirm_revoke())
                    if service {
                        listing_section(
                            data_dir: page.data_dir.clone(),
                            error: notice.listing_error(),
                        )
                    }
                </section>
                <aside aria-label="Invite and connection" class="flex flex-col gap-5">
                    if matches!(hub_access, super::HubAccess::Unavailable) {
                        alert(variant: AlertVariant::Destructive, attrs: attributes! { role="alert" },
                            alert_title("Hub settings are unavailable")
                            alert_description("This node cannot read its saved hub choice. Fix the data directory and reload.")
                        )
                    }
                    match &hub {
                        Some(hub) => {
                            hub_section(
                                hub: hub.clone(),
                                role: Role::Sender,
                                error: notice.hub_error(),
                                relay_missing: relay_url.is_none(),
                                <details class="group border-t border-border pt-4" open=(code_open.then_some(""))>
                                    <summary class="flex cursor-pointer list-none items-center gap-2 text-sm text-muted-foreground">
                                        <span class="transition-transform group-open:rotate-90">icon(data: CHEVRON)</span>
                                        "Invite with a connect code instead"
                                    </summary>
                                    <div class="flex flex-col gap-5 pt-4">
                                        invite_form(error: notice.invite_error())
                                        if !invitation.line.is_empty() {
                                            invite_result(invitation: invitation.clone(), command: command.clone(), agent_name: agent_name.clone())
                                        }
                                    </div>
                                </details>
                            )
                        }
                        None => {
                            <section id="invite" aria-labelledby="invite-title" class="flex flex-col gap-5 rounded-xl border border-border bg-background p-6 shadow-sm">
                                <div class="flex flex-col gap-1.5">
                                    <h2 id="invite-title" class="text-lg leading-tight font-semibold">"Invite someone"</h2>
                                    <p class="text-sm text-muted-foreground">"Ask them for the connect code on their Aether page. The invite works only on their computer."</p>
                                </div>
                                if relay_url.is_none() {
                                    shared::relay_notice()
                                }
                                invite_form(error: notice.invite_error())
                                if !invitation.line.is_empty() {
                                    invite_result(invitation: invitation.clone(), command: command.clone(), agent_name: agent_name.clone())
                                }
                            </section>
                        }
                    }
                    <section aria-labelledby="connection-title" class="flex flex-col gap-3 rounded-xl border border-border bg-background px-6 py-5">
                        <h2 id="connection-title" class="text-[15px] font-semibold">"Connection"</h2>
                        <div class="flex flex-col divide-y divide-border border-t border-border">
                            relay_line(
                                url: relay_url.clone().unwrap_or_default(),
                                error: notice.relay_error(),
                            )
                            hub_setting(
                                url: hub.as_ref().map(HubClient::url),
                                error: notice.hub_setting_error(),
                            )
                        </div>
                    </section>
                </aside>
            </div>
            <div class="grid items-start gap-10 border-t border-border pt-10 lg:grid-cols-[minmax(0,1fr)_30rem]">
                card_section(
                    settings: crate::agent_card::settings(&page.data_dir),
                    error: notice.card_error(),
                    saved: notice.card_saved(),
                )
                groups_section(groups: groups, friend_choices: friend_choices, error: notice.group_error())
            </div>
        </main>
    }
    // The page is deep: on the heap, its view does not fill the stack.
    .boxed())
}

/// A new invite, shown once: the line for the friend's page and the command
/// for their terminal.
#[component]
async fn invite_result(
    invitation: NewInvitation,
    command: String,
    agent_name: String,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <div class="flex flex-col gap-4 rounded-lg border border-success/40 bg-success/5 p-4" role="status">
            <div class="flex flex-col gap-1">
                <p class="flex items-center gap-2 font-semibold">
                    <span class="text-success">icon(data: CHECK)</span>
                    (&invitation.friend_name) " can now use your agent"
                </p>
                <p class="text-sm text-success">
                    "Send them one of these. They are shown only once."
                </p>
            </div>
            copy_field(
                id: "invite-line",
                name: "For their Aether page",
                value: invitation.line.clone(),
            )
            copy_field(
                id: "invite-command",
                name: "For their terminal",
                value: command,
                tone: CopyTone::Terminal,
            )
            <p class="text-xs text-muted-foreground">
                "Then their agent, or "
                <code class="text-foreground">"aether node request --scope direct --agent-id " (super::caller::shell_quote(&agent_name)) " --text '…'"</code>
                ", sends tasks."
            </p>
        </div>
    })
}

/// The access groups, one card each, with the forms that change them.
#[component]
async fn groups_section(
    groups: Vec<aether_store::AccessGroup>,
    friend_choices: Vec<(String, String)>,
    error: String,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <section id="groups" class="flex flex-col gap-4" aria-labelledby="groups-title">
            <div class="flex flex-col gap-1.5">
                <h2 id="groups-title" class="text-xl font-semibold tracking-tight">"Incoming access groups"</h2>
                <p class="text-sm text-muted-foreground">"A person must have direct access and belong to at least one group to send tasks. New invites join friends. People whose access request you approve join requests."</p>
            </div>
            if !error.is_empty() {
                <p role="alert" class="flex items-center gap-1.5 text-sm text-destructive">icon(data: ALERT) (error.clone())</p>
            }
            <form method="post" action="/groups" class="flex gap-2">
                <input type="hidden" name="action" value="create" />
                input(attrs: attributes! { name="group" aria-label="New group name" placeholder="Group name" required="" maxlength="64" })
                button(variant: ButtonVariant::Outline, attrs: attributes! { type="submit" }, "Create group")
            </form>
            for group in &groups {
                <div class="flex flex-col gap-2 rounded-xl border border-border bg-background px-5 py-4">
                    <div class="flex items-baseline justify-between gap-3">
                        <h3 class="text-[15px] font-semibold">(&group.name)</h3>
                        if group.name != crate::access_groups::FRIENDS_GROUP && group.name != crate::access_groups::REQUESTS_GROUP {
                            <form method="post" action="/groups">
                                <input type="hidden" name="action" value="delete" />
                                <input type="hidden" name="group" value=(&group.name) />
                                <button type="submit" class="text-sm font-medium text-destructive hover:underline">"Delete group"</button>
                            </form>
                        }
                    </div>
                    if group.members.is_empty() {
                        <p class="text-sm text-muted-foreground">"No people in this group."</p>
                    }
                    if !group.members.is_empty() {
                        <ul class="flex flex-col divide-y divide-border border-y border-border">
                            for member in &group.members {
                                <li>
                                    <form method="post" action="/groups" class="flex items-center justify-between gap-2 py-2 text-sm">
                                        <span class="font-mono text-[13px]">(&member.agent_id)</span>
                                        <input type="hidden" name="action" value="remove" />
                                        <input type="hidden" name="group" value=(&group.name) />
                                        <input type="hidden" name="agent" value=(&member.agent_id) />
                                        <input type="hidden" name="endpoint_id" value=(&member.endpoint_id) />
                                        <button type="submit" class="font-medium text-destructive hover:underline">"Remove"</button>
                                    </form>
                                </li>
                            }
                        </ul>
                    }
                    if !friend_choices.is_empty() {
                        <form method="post" action="/groups" class="flex gap-2 pt-1">
                            <input type="hidden" name="action" value="add" />
                            <input type="hidden" name="group" value=(&group.name) />
                            <select name="endpoint_id" aria-label=(format!("Add a person to {}", group.name)) class="h-8 min-w-0 flex-1 rounded-lg border border-border bg-background px-2 text-sm">
                                for (name, endpoint) in &friend_choices {
                                    <option value=(endpoint)> (name) </option>
                                }
                            </select>
                            button(variant: ButtonVariant::Outline, size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Add person")
                        </form>
                    }
                </div>
            }
        </section>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GroupForm {
    action: String,
    group: String,
    agent: Option<String>,
    endpoint_id: Option<String>,
}

#[page(POST "/groups")]
pub(super) async fn change_group(
    cx: &Cx,
    Form(form): Form<GroupForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node has no groups").await?;
    let action = match form.action.as_str() {
        "create" => Action::Create,
        "add" => Action::Add,
        "remove" => Action::Remove,
        "delete" => Action::Delete,
        _ => return Err(bad_request("this group action is invalid").into()),
    };
    let peers = administration(cx)
        .grants()
        .map_err(|_| bad_request("grants are unavailable"))?
        .into_iter()
        .filter(|grant| {
            grant.scope_id.as_str() == crate::direct::SCOPE
                && grant.expires_at > SystemTime::now()
        })
        .map(|grant| {
            (
                grant.subject_agent_id.as_str().to_owned(),
                grant.subject_endpoint_id.as_str().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let group_error = access_groups::change(
        &page.data_dir,
        action,
        &form.group,
        form.agent.as_deref(),
        form.endpoint_id.as_deref(),
        &peers,
    )
    .err()
    .unwrap_or_default()
    .to_owned();
    Ok(view! { host_screen(notice: Notice::Group(group_error)) })
}

/// The relay people reach the agent through, as a row of the Connection
/// card, and a form to change it. The form is open while there is no relay,
/// because an invite needs one to work outside the local network.
#[component]
async fn relay_line(url: String, error: String) -> TopcoatResult<impl View> {
    let open = url.is_empty() || !error.is_empty();
    Ok(view! {
        <div id="relay" class="py-3">
            <details class="group" open=(open.then_some(""))>
                <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
                    <span class="flex min-w-0 flex-col gap-0.5">
                        <span class="text-[13px] text-muted-foreground">"Relay"</span>
                        if url.is_empty() {
                            <span class="text-[13px] text-muted-foreground">"No relay yet: only your local network can reach it."</span>
                        } else {
                            <span class="sr-only">"People reach it through "</span>
                            <code class="font-mono text-[13px] break-all text-foreground">(&url)</code>
                        }
                    </span>
                    <span class="shrink-0 text-[13px] font-medium text-primary underline underline-offset-4">
                        (if url.is_empty() { "Set a relay" } else { "Change relay" })
                    </span>
                </summary>
                <form method="post" action="/relay" class="mt-3 flex flex-col gap-2">
                    field(
                        name: "Relay URL",
                        control_id: "relay_url",
                        hint: "An iroh relay, such as one you run. Leave it empty to use only your local network.",
                        input(attrs: attributes! {
                            id="relay_url" name="relay_url" type="url" value=(&url)
                            placeholder="https://relay.example.com" class="font-mono"
                            spellcheck="false" autocomplete="off"
                            aria-invalid=((!error.is_empty()).then_some("true"))
                        })
                    )
                    if !error.is_empty() {
                        <p class="flex items-center gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (&error)</p>
                    }
                    button(variant: ButtonVariant::Outline, size: ButtonSize::Sm, attrs: attributes! { type="submit" class="w-fit" }, "Use this relay")
                </form>
            </details>
        </div>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RelayForm {
    relay_url: String,
}

/// Moves the running node to another relay, or to none, and keeps the choice
/// for the next start. A relay that does not answer in time is refused and
/// the node keeps the one it had.
#[page(POST "/relay")]
pub(super) async fn relay(
    cx: &Cx,
    Form(form): Form<RelayForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node has no relay to change").await?;
    let relay = form.relay_url.trim();
    let relay = (!relay.is_empty()).then(|| relay.to_owned());
    let valid = relay.as_deref().is_none_or(crate::transport::relay::valid_relay_url);
    let relay_error = if valid {
        match page.set_relay_url(relay).await {
            Ok(()) => String::new(),
            Err(crate::transport::TransportError::Timeout) => {
                "The relay did not answer. The node keeps its current relay.".to_owned()
            }
            Err(_) => "The node cannot use this relay.".to_owned(),
        }
    } else {
        "This is not a relay URL. It starts with https://.".to_owned()
    };
    Ok(view! { host_screen(notice: Notice::Relay(relay_error)) })
}

/// The Agent Card that people get with an invite, and a form to change it.
#[component]
async fn card_section(
    settings: crate::agent_card::CardSettings,
    error: String,
    saved: bool,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <section id="card" class="flex flex-col gap-4" aria-labelledby="card-title">
            <div class="flex flex-col gap-1.5">
                <h2 id="card-title" class="text-xl font-semibold tracking-tight">"Your agent's card"</h2>
                <p class="text-sm text-muted-foreground">
                    "People you invite get this card with the invite. It is never public. Do not name your tools, model, folders or settings."
                </p>
                <p class="text-sm text-muted-foreground">"Card version " (settings.version) "."</p>
            </div>
            <form method="post" action="/card" class="flex flex-col gap-3">
                field(
                    name: "Description",
                    control_id: "card-description",
                    <textarea id="card-description" name="description" required="" maxlength="500" rows="3"
                        class="rounded-lg border border-border bg-background px-3 py-2 text-sm shadow-xs">(&settings.description)</textarea>
                )
                field(
                    name: "Skill name",
                    control_id: "card-skill-name",
                    input(attrs: attributes! { id="card-skill-name" name="skill_name" required="" maxlength="64" value=(&settings.skill_name) })
                )
                field(
                    name: "Skill description",
                    control_id: "card-skill-description",
                    <textarea id="card-skill-description" name="skill_description" required="" maxlength="300" rows="2"
                        class="rounded-lg border border-border bg-background px-3 py-2 text-sm shadow-xs">(&settings.skill_description)</textarea>
                )
                field(
                    name: "Tags",
                    control_id: "card-tags",
                    hint: "Up to 5, with commas: lowercase letters, digits and dashes.",
                    input(attrs: attributes! { id="card-tags" name="tags" required="" maxlength="130" value=(settings.tags.join(", ")) })
                )
                if !error.is_empty() {
                    <p class="flex items-start gap-2 rounded-lg border border-destructive/40 bg-background px-3.5 py-2.5 text-sm text-destructive" role="alert">icon(data: ALERT) (&error)</p>
                } else if saved {
                    <p class="flex items-center gap-2 rounded-lg border border-success/40 bg-success/5 px-3.5 py-2.5 text-sm text-success" role="status">
                        icon(data: CHECK) "The card is saved. New invites carry it."
                    </p>
                }
                button(attrs: attributes! { type="submit" class="w-fit" }, "Save card")
            </form>
        </section>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CardForm {
    description: String,
    skill_name: String,
    skill_description: String,
    tags: String,
}

/// Saves the text of the Agent Card, with the next card version.
#[page(POST "/card")]
pub(super) async fn save_card(
    cx: &Cx,
    Form(form): Form<CardForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node has no agent card").await?;
    let card_input = crate::agent_card::CardInput {
        description: form.description,
        skill_name: form.skill_name,
        skill_description: form.skill_description,
        tags: form
            .tags
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    let notice = match crate::agent_card::save_settings(&page.data_dir, &card_input) {
        Ok(_) => Notice::CardSaved,
        Err(crate::agent_card::CardError::Invalid(reason)) => {
            Notice::Card(format!("The card was not saved: {reason}."))
        }
        Err(error) => return Err(internal_server_error(error).into()),
    };
    Ok(view! { host_screen(notice: notice) })
}

/// Where the agent works, and a form to change it that opens on request.
#[component]
async fn workspace_line(
    path: String,
    error: String,
    task_folder_mode: bool,
) -> TopcoatResult<impl View> {
    let open = !error.is_empty();
    Ok(view! {
        if task_folder_mode {
            <p>"Each task runs in a new empty folder. The node removes the folder when the task ends."</p>
        } else {
            <p>
                "It works in "
                <code class="rounded-md bg-muted px-1.5 py-0.5 text-[0.9em] break-all text-foreground">(&path)</code>
                ". People you invite can read it, not change it."
            </p>
        }
        if !task_folder_mode {
        <details class="mt-2 group" open=(open.then_some(""))>
            <summary class="w-fit cursor-pointer text-sm text-primary underline underline-offset-4">
                "Change folder"
            </summary>
            <form method="post" action="/workspace" class="mt-3 flex max-w-xl flex-wrap items-end gap-2">
                <div class="min-w-64 flex-1">
                    field(
                        name: "Folder",
                        control_id: "path",
                        hint: "An existing folder on this computer.",
                        input(attrs: attributes! { id="path" name="path" required="" value=(&path) class="font-mono" spellcheck="false" })
                    )
                </div>
                button(variant: ButtonVariant::Outline, attrs: attributes! { type="submit" class="mb-5" }, "Use this folder")
            </form>
            if open {
                <p class="flex items-start gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (&error)</p>
            }
        </details>
        }
    })
}

/// The people with access: one row each, with their last task, when the
/// access ends, and a two-step revoke.
#[component]
async fn people(
    friends: Vec<Friend>,
    confirm_revoke: String,
) -> TopcoatResult<impl View> {
    let now = aether_core::time::now();
    Ok(view! {
        if friends.is_empty() {
            <div class="rounded-xl border border-dashed border-border p-6 text-muted-foreground">
                <p class="font-medium text-foreground">"Nobody has access."</p>
                <p class="mt-1 text-sm">"Invite someone, and they appear here with their last task."</p>
            </div>
        } else {
            table(
                attrs: attributes! { aria-label="People with access" },
                table_header(
                    table_row(
                        table_head("Person")
                        table_head("Last task")
                        table_head("Access ends")
                        table_head(<span class="sr-only">"Actions"</span>)
                    )
                )
                table_body(
                    for friend in &friends {
                        if friend.grant_id == confirm_revoke {
                            table_row(
                                attrs: attributes! { class="bg-destructive/5 hover:bg-destructive/5" },
                                table_cell(person(name: friend.name.clone()))
                                table_cell(
                                    attrs: attributes! { colspan="3" class="whitespace-normal" },
                                    <div class="flex flex-wrap items-center justify-end gap-2">
                                        <p class="mr-auto text-sm text-destructive">
                                            "Revoke " (&friend.name) "'s access? A task that is running still finishes. The next one fails."
                                        </p>
                                        <a href="/" class="px-3 py-2 text-sm">"Keep access"</a>
                                        <form method="post" action="/revoke">
                                            <input type="hidden" name="grant_id" value=(&friend.grant_id)>
                                            <input type="hidden" name="confirm" value="yes">
                                            button(
                                                variant: ButtonVariant::Destructive,
                                                attrs: attributes! { type="submit" },
                                                "Revoke"
                                            )
                                        </form>
                                    </div>
                                )
                            )
                        } else {
                            table_row(
                                table_cell(
                                    <span class="flex items-center gap-3">
                                        person(name: friend.name.clone())
                                        presence_dot(endpoint: friend.endpoint_id.clone())
                                    </span>
                                )
                                table_cell(
                                    attrs: attributes! { class="max-w-md whitespace-normal" },
                                    last_task(task: friend.last_task.clone(), now: now)
                                )
                                table_cell((access_ends(friend.days_left)))
                                table_cell(
                                    attrs: attributes! { class="text-right" },
                                    <form method="post" action="/revoke">
                                        <input type="hidden" name="grant_id" value=(&friend.grant_id)>
                                        button(
                                            variant: ButtonVariant::Outline,
                                            attrs: attributes! { type="submit" aria-label=(format!("Revoke {}", friend.name)) },
                                            "Revoke"
                                        )
                                    </form>
                                )
                            )
                        }
                    }
                )
            )
        }
    })
}

/// A person's initial and name.
#[component]
async fn person(name: String) -> TopcoatResult<impl View> {
    let initial = name.chars().next().map_or('?', |first| first.to_ascii_uppercase());
    Ok(view! {
        <span class="flex items-center gap-3 font-medium">
            avatar(size: AvatarSize::Sm, avatar_fallback((initial.to_string())))
            (&name)
        </span>
    })
}

/// The state and retained answer of a person's newest task.
#[component]
async fn last_task(task: Option<LastTask>, now: i64) -> TopcoatResult<impl View> {
    Ok(view! {
        match task {
            None => <span class="text-muted-foreground">"No tasks yet"</span>,
            Some(task) => {
                <div class="flex flex-col gap-2">
                    <span class="flex items-center gap-2">
                        badge(variant: BadgeVariant::Outline, attrs: attributes! { class=(state_tone(task.state)) }, (state_name(task.state)))
                        <span class="text-muted-foreground">(ago(now.saturating_sub(task.created_at)))</span>
                    </span>
                    if let Some(result) = task.result {
                        <details class="text-sm">
                            <summary class="cursor-pointer">"Read answer"</summary>
                            <p class="mt-2 font-medium">"Task"</p>
                            <p class="whitespace-pre-wrap break-words">(task.remote_text)</p>
                            <p class="mt-2 font-medium">"Answer"</p>
                            <pre class="max-h-96 overflow-auto whitespace-pre-wrap break-words font-sans">(result)</pre>
                        </details>
                    }
                </div>
            }
        }
    })
}

const fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::PendingApproval => "Waits for approval",
        TaskState::Queued | TaskState::Leased => "Waiting",
        TaskState::Running => "Running",
        TaskState::Succeeded => "Answered",
        TaskState::Failed | TaskState::UnknownResult | TaskState::OperatorReviewed => {
            "Failed"
        }
        TaskState::Rejected => "Refused",
        TaskState::Cancelled => "Cancelled",
    }
}

const fn state_tone(state: TaskState) -> &'static str {
    match state {
        TaskState::Succeeded => "border-success/40 text-success",
        TaskState::PendingApproval
        | TaskState::Queued
        | TaskState::Leased
        | TaskState::Running => "border-warning/40 text-warning",
        TaskState::Failed
        | TaskState::UnknownResult
        | TaskState::OperatorReviewed
        | TaskState::Rejected => "border-destructive/40 text-destructive",
        TaskState::Cancelled => "text-muted-foreground",
    }
}

fn access_ends(days_left: u64) -> String {
    match days_left {
        0 => "today".to_owned(),
        1 => "in 1 day".to_owned(),
        days => format!("in {days} days"),
    }
}

/// The invite form, with the reason when it refused the last input.
#[component]
async fn invite_form(error: String) -> TopcoatResult<impl View> {
    Ok(view! {
        <form method="post" action="/invite" class="flex flex-col gap-4">
            field(
                name: "Their connect code",
                control_id: "endpoint_id",
                input(attrs: attributes! {
                    id="endpoint_id" name="endpoint_id" required="" autocomplete="off"
                    spellcheck="false" placeholder="0154 315d ad38 …" class="font-mono"
                })
            )
            <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
                field(
                    name: "Their name",
                    control_id: "friend_name",
                    input(attrs: attributes! {
                        id="friend_name" name="friend_name" required="" maxlength="69"
                        pattern="[A-Za-z0-9._-]+(#[0-9]{4})?" title="Letters, digits, dot, dash or underscore, and the four digits of a hub name"
                        placeholder="ana" autocomplete="off"
                    })
                )
                field(
                    name: "Access for",
                    control_id: "days",
                    <select
                        id="days"
                        name="days"
                        class="h-9 rounded-lg border border-border bg-background px-3 text-sm shadow-xs"
                    >
                        for days in crate::direct::DIRECT_OFFER_DAYS {
                            <option value=(days) selected=((days == crate::direct::DEFAULT_DIRECT_ACCESS_DAYS).then_some(""))>
                                (if days == 1 { "1 day".to_owned() } else { format!("{days} days") })
                            </option>
                        }
                    </select>
                )
            </div>
            if !error.is_empty() {
                <p class="flex items-start gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (&error)</p>
            }
            button(attrs: attributes! { type="submit" class="w-full" }, "Create invite")
        </form>
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InviteForm {
    friend_name: String,
    endpoint_id: String,
    days: u64,
}

/// Signs the friend's access with the node's issuer key, applies the grant
/// to the running node, and shows the invite to send.
#[page(POST "/invite")]
pub(super) async fn invite(
    cx: &Cx,
    Form(form): Form<InviteForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node cannot invite").await?;
    // A code copied from the caller's page may keep its groups of four.
    let endpoint_id = form.endpoint_id.split_whitespace().collect::<String>();
    let refusal = if endpoint_id.parse::<iroh::EndpointId>().is_err() {
        Some(
            "This is not a connect code. Ask them to copy it again from their Aether page.",
        )
    } else if !crate::direct::valid_name(&form.friend_name) {
        Some("Use letters, digits, dots, dashes or underscores for their name.")
    } else if !crate::direct::valid_direct_invite_days(form.days) {
        Some("Choose how long they can use your agent.")
    } else {
        None
    };
    let (invitation, notice) = if let Some(refusal) = refusal {
        (NewInvitation::default(), Notice::Invite(refusal.to_owned()))
    } else {
        let line = create_invite(cx, page, &form.friend_name, &endpoint_id, form.days)?;
        (NewInvitation { friend_name: form.friend_name, line }, Notice::default())
    };
    Ok(view! { host_screen(invitation: invitation, notice: notice) })
}

/// Signs the access, applies the grant, and returns the one-line invite.
fn create_invite(
    cx: &Cx,
    page: &PageContext,
    friend_name: &str,
    endpoint_id: &str,
    days: u64,
) -> TopcoatResult<String> {
    sign_direct_invite(
        app_context::<SharedAdministration>(cx),
        &DirectInviteSigning::for_data_dir_with_relay(&page.data_dir, page.relay_url()),
        &page.agent_name(),
        friend_name,
        endpoint_id,
        days,
        crate::access_groups::FRIENDS_GROUP,
        aether_store::AuditActor::Ui,
    )
    .map(|(line, _)| line)
    .map_err(|error| match error {
        crate::invites::InviteError::Refused => {
            bad_request("the node refused the new access").into()
        }
        crate::invites::InviteError::Internal(error) => {
            internal_server_error(error).into()
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevokeForm {
    grant_id: String,
    /// Present on the second click; the first only asks.
    #[serde(default)]
    confirm: Option<String>,
}

/// Revokes a friend's grant after a second click: the running node drops it
/// at once and a later start does not restore it.
#[page(POST "/revoke")]
pub(super) async fn revoke(
    cx: &Cx,
    Form(form): Form<RevokeForm>,
) -> TopcoatResult<impl View> {
    admin_page(cx, "this node cannot revoke").await?;
    let notice = if form.confirm.as_deref() == Some("yes") {
        administration(cx)
            .revoke(&form.grant_id, aether_store::AuditActor::Ui)
            .map_err(|_| bad_request("the access cannot be revoked"))?;
        Notice::default()
    } else {
        Notice::ConfirmRevoke(form.grant_id)
    };
    Ok(view! { host_screen(notice: notice) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceForm {
    path: String,
}

/// Changes the folder the agent works in for remote tasks. The node checks
/// that the folder exists and, when the policy names approved workspaces,
/// that the path is one of them.
#[page(POST "/workspace")]
pub(super) async fn workspace(
    cx: &Cx,
    Form(form): Form<WorkspaceForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node has no workspace to change").await?;
    if page.task_folder_mode {
        return Err(bad_request("task folders do not allow folder changes").into());
    }
    let applied = crate::admin::ChangeRequest::Workspace {
        id: crate::direct::INVITE_WORKSPACE_ID.to_owned(),
        path: form.path.trim().to_owned(),
    }
    .into_change()
    .is_ok_and(|change| {
        administration(cx)
            .apply(&change, aether_store::AuditActor::Ui)
            .map(|_| ())
            .is_ok()
    });
    let notice = if applied {
        Notice::default()
    } else {
        Notice::Workspace("The agent cannot use this folder. It must exist on this computer and be allowed by the policy.".to_owned())
    };
    Ok(view! { host_screen(notice: notice) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListingForm {
    action: String,
}

#[page(POST "/listing/publish")]
pub(super) async fn publish_listing(
    cx: &Cx,
    Form(form): Form<ListingForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node has no listing").await?;
    if crate::settings::setting::<crate::hub_client::settings::AgentKind>(
        &page.data_dir,
    )
    .ok()
    .flatten()
        != Some(crate::hub_client::AgentKind::Service)
    {
        return Err(bad_request("only a service agent has a public listing").into());
    }
    let result = match (form.action.as_str(), page.current_hub()) {
        (_, None) => Err(crate::listing::HubPublishError::Hub(
            crate::hub_client::HubError::Unreachable,
        )),
        ("list", Some(hub)) => {
            crate::listing::publish_to_hub(&page.data_dir, &hub, now_seconds())
                .await
                .map(drop)
        }
        ("stop", Some(hub)) => {
            crate::listing::stop_hub_listing(&page.data_dir, &hub).await
        }
        _ => return Err(bad_request("unknown listing action").into()),
    };
    let error = result
        .err()
        .map(crate::listing::hub_publish_message)
        .unwrap_or_default()
        .to_owned();
    Ok(view! { host_screen(notice: Notice::Listing(error)) })
}
