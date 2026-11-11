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
    listing::{ListingAction, listing_section},
    shared::{self, Notice},
};
use crate::access_groups::{self, Action};
use crate::hub_client::{HubClient, Role};
use crate::invites::{DirectInviteSigning, sign_direct_invite};
use crate::{
    admin::SharedAdministration,
    admin_components::{
        ALERT, CHECK, CHEVRON, CopyTone, ErrorIcon, Tone, ago, copy_field, days_label,
        days_select, error_line, field, page_title, post_button, presence_dot,
        section_heading, state_badge,
    },
    components::{
        alert::{AlertVariant, alert, alert_description, alert_title},
        avatar::{AvatarSize, avatar, avatar_fallback},
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
                section_heading(id: "groups-title", "Incoming access groups")
                <p class="text-sm text-muted-foreground">"A person must have direct access and belong to at least one group to send tasks. New invites join friends. People whose access request you approve join requests."</p>
            </div>
            if !error.is_empty() {
                error_line(text: error.clone(), placement: ErrorIcon::Centered)
            }
            <form method="post" action="/groups" class="flex gap-2">
                <input type="hidden" name="action" value=(GroupAction::Create.as_str()) />
                input(attrs: attributes! { name="group" aria-label="New group name" placeholder="Group name" required="" maxlength="64" })
                button(variant: ButtonVariant::Outline, attrs: attributes! { type="submit" }, "Create group")
            </form>
            for group in &groups {
                <div class="flex flex-col gap-2 rounded-xl border border-border bg-background px-5 py-4">
                    <div class="flex items-baseline justify-between gap-3">
                        <h3 class="text-[15px] font-semibold">(&group.name)</h3>
                        if group.name != crate::access_groups::FRIENDS_GROUP && group.name != crate::access_groups::REQUESTS_GROUP {
                            <form method="post" action="/groups">
                                <input type="hidden" name="action" value=(GroupAction::Delete.as_str()) />
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
                                        <input type="hidden" name="action" value=(GroupAction::Remove.as_str()) />
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
                            <input type="hidden" name="action" value=(GroupAction::Add.as_str()) />
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

/// The change a group form asks for, from its hidden `action` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum GroupAction {
    Create,
    Add,
    Remove,
    Delete,
}

impl GroupAction {
    /// The field value; `Deserialize` reads the same names back.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Delete => "delete",
        }
    }
}

impl From<GroupAction> for Action {
    fn from(action: GroupAction) -> Self {
        match action {
            GroupAction::Create => Self::Create,
            GroupAction::Add => Self::Add,
            GroupAction::Remove => Self::Remove,
            GroupAction::Delete => Self::Delete,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GroupForm {
    action: GroupAction,
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
        form.action.into(),
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
    Ok(view! {
        shared::url_setting(
            id: "relay",
            action: "/relay",
            name: "Relay URL",
            control: "relay_url",
            hint: "An iroh relay, such as one you run. Leave it empty to use only your local network.",
            placeholder: "https://relay.example.com",
            value: url.clone(),
            error: error,
            missing: url.is_empty(),
            link: if url.is_empty() { "Set a relay" } else { "Change relay" },
            submit: "Use this relay",
            <span class="text-[13px] text-muted-foreground">"Relay"</span>
            if url.is_empty() {
                <span class="text-[13px] text-muted-foreground">"No relay yet: only your local network can reach it."</span>
            } else {
                <span class="sr-only">"People reach it through "</span>
                <code class="font-mono text-[13px] break-all text-foreground">(&url)</code>
            }
        )
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
                section_heading(id: "card-title", "Your agent's card")
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
                error_line(text: error.clone(), placement: ErrorIcon::Top)
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
                                        post_button(
                                            action: "/revoke",
                                            fields: vec![("grant_id", friend.grant_id.clone()), ("confirm", "yes".to_owned())],
                                            variant: ButtonVariant::Destructive,
                                            "Revoke"
                                        )
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
                                    post_button(
                                        action: "/revoke",
                                        fields: vec![("grant_id", friend.grant_id.clone())],
                                        variant: ButtonVariant::Outline,
                                        aria_label: format!("Revoke {}", friend.name),
                                        "Revoke"
                                    )
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
                        state_badge(state: task_state(task.state))
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

/// The word and the badge tone of a task state.
const fn task_state(state: TaskState) -> (&'static str, Tone) {
    match state {
        TaskState::PendingApproval => ("Waits for approval", Tone::Warning),
        TaskState::Queued | TaskState::Leased => ("Waiting", Tone::Warning),
        TaskState::Running => ("Running", Tone::Warning),
        TaskState::Succeeded => ("Answered", Tone::Success),
        TaskState::Failed | TaskState::UnknownResult | TaskState::OperatorReviewed => {
            ("Failed", Tone::Destructive)
        }
        TaskState::Rejected => ("Refused", Tone::Destructive),
        TaskState::Cancelled => ("Cancelled", Tone::Muted),
    }
}

fn access_ends(days_left: u64) -> String {
    match days_left {
        0 => "today".to_owned(),
        days => format!("in {}", days_label(days)),
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
                    days_select(
                        id: "days",
                        class: "h-9 rounded-lg border border-border bg-background px-3 text-sm shadow-xs",
                        choices: crate::direct::DIRECT_OFFER_DAYS.to_vec(),
                        selected: crate::direct::DEFAULT_DIRECT_ACCESS_DAYS,
                    )
                )
            </div>
            if !error.is_empty() {
                error_line(text: error.clone(), placement: ErrorIcon::Top)
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
    action: ListingAction,
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
    let result = match (form.action, page.current_hub()) {
        (_, None) => Err(crate::listing::HubPublishError::Hub(
            crate::hub_client::HubError::Unreachable,
        )),
        (ListingAction::List, Some(hub)) => {
            crate::listing::publish_to_hub(&page.data_dir, &hub, now_seconds())
                .await
                .map(drop)
        }
        (ListingAction::Stop, Some(hub)) => {
            crate::listing::stop_hub_listing(&page.data_dir, &hub).await
        }
    };
    let error = result
        .err()
        .map(crate::listing::hub_publish_message)
        .unwrap_or_default()
        .to_owned();
    Ok(view! { host_screen(notice: Notice::Listing(error)) })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;

    use super::GroupAction;
    use crate::admin::test_support::shared_in;
    use crate::admin_ui::{
        UiOptions, build_admin_ui,
        test_support::{
            PORT, app, form_value, get, get_with, host_ui, host_ui_with_mode,
            input_value, post, session, session_cookie_pair, test_administration,
            test_host, test_host_page,
        },
    };
    use aether_store::{NodeStore, QueueLimits, TaskInput};

    #[test]
    fn a_group_action_value_reads_back_as_its_action() {
        for action in [
            GroupAction::Create,
            GroupAction::Add,
            GroupAction::Remove,
            GroupAction::Delete,
        ] {
            let value = serde_json::Value::from(action.as_str());
            assert_eq!(serde_json::from_value::<GroupAction>(value).ok(), Some(action));
        }
        assert!(serde_json::from_value::<GroupAction>("rename".into()).is_err());
    }

    #[tokio::test]
    async fn the_owner_edits_the_agent_card_on_the_host_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let cookie = session(&application).await;
        let home = get_with(application.clone(), "/", &cookie).await;
        assert!(home.body.contains("Your agent's card"));
        assert!(home.body.contains("It is never public."));
        let saved = post(
            application.clone(),
            "/card",
            Some(&cookie),
            "description=Legal+help.&skill_name=Legal+questions&skill_description=Answers+one+question.&tags=law%2C+contracts",
        )
        .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        assert!(saved.body.contains("Card version 2."));
        let card = crate::agent_card::settings(directory.path());
        assert_eq!(card.tags, ["law", "contracts"]);

        let refused = post(
            application.clone(),
            "/card",
            Some(&cookie),
            "description=&skill_name=Legal+questions&skill_description=Answers.&tags=law",
        )
        .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(refused.body.contains("role=\"alert\""));
        assert_eq!(crate::agent_card::settings(directory.path()), card);
    }

    #[tokio::test]
    async fn the_host_sets_the_relay_from_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let switches = Arc::new(std::sync::Mutex::new(Vec::new()));
        let options = UiOptions {
            page: Some(Arc::new(test_host(directory.path(), Arc::clone(&switches)))),
            ..UiOptions::default()
        };
        let application =
            app(build_admin_ui(shared_in(directory.path()), options, PORT));
        let cookie = session(&application).await;
        let relay = |value: &str| {
            let application = application.clone();
            let cookie = cookie.clone();
            let form = form_value("relay_url", value);
            async move { post(application, "/relay", Some(&cookie), &form).await }
        };

        let response = relay("https://relay.example.com/").await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body.contains("Reachable through relay.example.com"));
        assert_eq!(
            crate::settings::setting::<crate::transport::relay::Relay>(
                directory.path()
            )
            .ok()
            .flatten()
            .as_deref(),
            Some("https://relay.example.com/")
        );

        // A relay that does not answer leaves the node where it was.
        let response = relay("https://down.test").await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("The relay did not answer."));
        assert!(response.body.contains("Reachable through relay.example.com"));

        let response = relay("not a url").await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This is not a relay URL."));

        // Empty means the local network only, and nothing to start with.
        let response = relay("").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("No relay yet"));
        assert_eq!(
            crate::settings::setting::<crate::transport::relay::Relay>(
                directory.path()
            )
            .ok()
            .flatten(),
            None
        );

        assert_eq!(
            switches.lock().expect("switches must lock").as_slice(),
            [
                (
                    Some("https://relay.test".to_owned()),
                    Some("https://relay.example.com/".to_owned())
                ),
                (
                    Some("https://relay.example.com/".to_owned()),
                    Some("https://down.test".to_owned())
                ),
                (Some("https://relay.example.com/".to_owned()), None),
            ]
        );
    }

    #[tokio::test]
    async fn an_invitation_applies_a_grant_and_the_host_sees_the_agent_answer() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Invite someone"));
        // Each field's label names its control.
        assert!(home.body.contains("for=\"endpoint_id\""));
        assert!(!home.body.contains("r#for"));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let friend = iroh::SecretKey::generate().public().to_string();
        // A code copied from the caller's page keeps its groups of four.
        let grouped = crate::admin_components::grouped(&friend);
        let response = post(
            application.clone(),
            "/invite",
            Some(cookie),
            &format!("friend_name=ana&{}&days=7", form_value("endpoint_id", &grouped)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let line = input_value(&response.body, "invite-line");
        assert_eq!(
            input_value(&response.body, "invite-command"),
            format!("aether node direct import '{line}'")
        );
        let bundle =
            crate::direct::InvitationBundle::decode(&line).expect("bundle must decode");
        assert_eq!(bundle.format, crate::direct::BUNDLE_FORMAT_V2);
        assert_eq!(bundle.relay_url.as_deref(), Some("https://relay.test"));
        let invitation =
            bundle.invitation.to_invitation().expect("invitation must verify");
        assert_eq!(invitation.agent_id, "tudor");
        let card = crate::agent_card::validate(
            bundle.agent_card.as_deref().expect("a v2 line has a card").as_bytes(),
            &invitation.agent_id,
            &invitation.endpoint_id,
        )
        .expect("the card must be valid");
        assert_eq!(card.name, invitation.agent_id);
        assert_eq!(bundle.authorization.caller_agent_id, "ana");
        assert_eq!(bundle.authorization.capability_id, crate::execution::GENERAL_SKILL);
        assert_eq!(bundle.authorization.claim.subject_endpoint_id, friend);
        let issuer = crate::data_dir::DataDir::new(directory.path())
            .issuer_key()
            .expect("issuer key must load");
        {
            let applied = node.granted();
            let grant = crate::identity::decode_grant(
                applied.first().expect("the grant must reach the node"),
            )
            .expect("grant must parse");
            assert_eq!(grant.signing_key, issuer.verifying_key().to_bytes());
            assert_eq!(grant.subject_endpoint_id.as_str(), friend);
            assert_eq!(
                grant.capabilities,
                std::collections::BTreeSet::from([aether_core::CapabilityId::new(
                    crate::execution::GENERAL_SKILL
                )])
            );
        }
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        assert_eq!(
            store
                .applied_changes(aether_store::ChangeKind::Grant)
                .expect("grants must read")
                .len(),
            1
        );
        store
            .put_scope(crate::direct::SCOPE, aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("greeting"),
                        id: aether_core::RequestId::new("greeting-request"),
                        caller_agent_id: aether_core::AgentId::new("ana"),
                        caller_endpoint_id: aether_core::EndpointId::new(friend),
                        scope_id: aether_core::ScopeId::new(crate::direct::SCOPE),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "greeting-key".to_owned(),
                    request_fingerprint: "greeting-fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "Greet Bob".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must admit");
        store.lease_next("worker", "execution", i64::MAX).expect("task must lease");
        store.mark_running("greeting", "execution").expect("task must run");
        store
            .mark_succeeded("greeting", "execution", "Hello, Bob. <ready>", i64::MAX)
            .expect("task must finish");
        let response = get(application, "/").await;
        assert!(response.body.contains("Read answer"));
        assert!(response.body.contains("Greet Bob"));
        assert!(response.body.contains("Hello, Bob. &lt;ready&gt;"));
    }

    #[tokio::test]
    async fn a_revoked_friend_leaves_the_list_and_the_revocation_is_stored() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Nobody has access."));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let friend = iroh::SecretKey::generate().public().to_string();
        let invited = post(
            application.clone(),
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={friend}&days=7"),
        )
        .await;
        assert!(
            invited.body.contains("aria-label=\"Revoke ana\""),
            "ana must be listed"
        );
        assert!(invited.body.contains("No tasks yet"));
        let grant_id = crate::identity::decode_grant(
            node.granted().first().expect("grant must exist"),
        )
        .expect("grant must parse")
        .id;
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        // The first click only asks.
        let asked = post(
            application.clone(),
            "/revoke",
            Some(&cookie),
            &format!("grant_id={}", grant_id.as_str()),
        )
        .await;
        assert_eq!(asked.status, StatusCode::OK);
        assert!(asked.body.contains("Revoke ana's access?"), "{}", asked.body);
        assert_eq!(store.grant_revoked(grant_id.as_str()), Ok(false));
        let response = post(
            application,
            "/revoke",
            Some(&cookie),
            &format!("grant_id={}&confirm=yes", grant_id.as_str()),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Nobody has access."));
        assert_eq!(store.grant_revoked(grant_id.as_str()), Ok(true));
        assert!(
            store
                .applied_changes(aether_store::ChangeKind::Grant)
                .expect("grants")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_host_changes_the_agent_workspace() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("value=\"/start\""));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let project = directory.path().join("project");
        std::fs::create_dir_all(&project).expect("project must create");
        let project = project.to_str().expect("path must be UTF-8");
        let response = post(
            application.clone(),
            "/workspace",
            Some(&cookie),
            &form_value("path", project),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains(project), "the page must show the new path");
        let missing = directory.path().join("missing");
        let response = post(
            application,
            "/workspace",
            Some(&cookie),
            &form_value("path", missing.to_str().expect("path must be UTF-8")),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn task_folder_page_refuses_folder_changes() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui_with_mode(directory.path(), true);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Each task runs in a new empty folder"));
        assert!(!home.body.contains("Change folder"));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let response = post(
            application,
            "/workspace",
            Some(&cookie),
            &form_value("path", directory.path().to_str().expect("path must be UTF-8")),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn host_page_creates_and_deletes_an_incoming_group() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let created = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            "action=create&group=work",
        )
        .await;
        assert_eq!(created.status, StatusCode::OK);
        assert!(created.body.contains("work"));
        let endpoint = iroh::SecretKey::generate().public().to_string();
        let invited = post(
            application.clone(),
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={endpoint}&days=7"),
        )
        .await;
        assert_eq!(invited.status, StatusCode::OK);
        let added = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            &format!("action=add&group=work&endpoint_id={endpoint}"),
        )
        .await;
        assert_eq!(added.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .any(|group| group.name == "work"
                    && group.members.iter().any(|member| member.agent_id == "ana"))
        );
        let removed = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            &format!("action=remove&group=work&agent=ana&endpoint_id={endpoint}"),
        )
        .await;
        assert_eq!(removed.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .any(|group| group.name == "work" && group.members.is_empty())
        );
        let deleted =
            post(application, "/groups", Some(&cookie), "action=delete&group=work")
                .await;
        assert_eq!(deleted.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .all(|group| group.name != "work")
        );
    }

    #[tokio::test]
    async fn an_invitation_needs_the_session_and_a_real_endpoint_id() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let friend = iroh::SecretKey::generate().public().to_string();
        let form = format!("friend_name=ana&endpoint_id={friend}&days=7");
        let response = post(application.clone(), "/invite", None, &form).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let response = post(
            application,
            "/invite",
            Some(cookie),
            "friend_name=ana&endpoint_id=not-a-key&days=7",
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This is not a connect code."));
        assert!(node.granted().is_empty());
    }

    #[tokio::test]
    async fn the_card_names_the_hub_name_when_the_node_has_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (administration, _) = test_administration(directory.path());
        let options = UiOptions {
            page: Some(Arc::new(test_host_page(
                directory.path().to_path_buf(),
                Some("server".to_owned()),
                None,
                Arc::new(|_, _| Box::pin(async { Ok(()) })),
                None,
            ))),
            ..UiOptions::default()
        };
        crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            directory.path(),
            &Some("tudor#0042".to_owned()),
        )
        .expect("hub name must store");
        let host = app(build_admin_ui(administration, options, PORT));
        let cookie = session(&host).await;
        let friend = iroh::SecretKey::generate().public().to_string();
        let response = post(
            host,
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={friend}&days=7"),
        )
        .await;
        let line = input_value(&response.body, "invite-line");
        let bundle = crate::direct::InvitationBundle::decode(&line).expect("bundle");
        let card: serde_json::Value = serde_json::from_str(
            bundle.agent_card.as_deref().expect("a v2 line has a card"),
        )
        .expect("the card is JSON");
        assert_eq!(card["name"], "tudor#0042");
        assert_eq!(bundle.invitation.agent_id, "tudor#0042");
    }

    #[test]
    fn the_longest_card_keeps_the_invite_line_under_the_hub_limit() {
        let directory = tempfile::tempdir().expect("directory must create");
        crate::agent_card::save_settings(
            directory.path(),
            &crate::agent_card::CardInput {
                description: "d".repeat(crate::agent_card::MAX_DESCRIPTION),
                skill_name: "n".repeat(crate::agent_card::MAX_SKILL_NAME),
                skill_description: "s".repeat(crate::agent_card::MAX_SKILL_DESCRIPTION),
                tags: (0..crate::agent_card::MAX_TAGS)
                    .map(|index| {
                        format!("{index}{}", "t".repeat(crate::agent_card::MAX_TAG - 1))
                    })
                    .collect(),
            },
        )
        .expect("the longest card must save");
        let (administration, _) = test_administration(directory.path());
        let host =
            test_host(directory.path(), Arc::new(std::sync::Mutex::new(Vec::new())));
        let longest = format!("{}#0001", "a".repeat(32));
        let (line, _) = crate::invites::sign_direct_invite(
            &administration,
            &crate::invites::DirectInviteSigning::for_data_dir_with_relay(
                &host.data_dir,
                host.relay_url(),
            ),
            &longest,
            &longest,
            &iroh::SecretKey::generate().public().to_string(),
            30,
            "friends",
            aether_store::AuditActor::Ui,
        )
        .expect("the invite must sign");
        assert!(line.len() + "bundle=".len() + 2 <= 8 * 1024, "{}", line.len());
    }
}
