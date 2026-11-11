//! The caller screen of `node admin`: connect to a friend's agent, then see
//! the agents this computer can use. Sending tasks is the job of the
//! caller's own agent (MCP) or the CLI, so the page shows the commands that
//! connect them instead of a task form.

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    icon::icon,
    router::{
        StatusCode,
        content::Form,
        error::{bad_request, internal_server_error},
        page,
    },
    view::{View, ViewExt, attributes, component, view},
};

use super::{
    Page, admin_page, ensure_admin_session,
    hub::{error_line, hub_failure, hub_section, hub_setting},
    layout::{Tab, node_header},
    shared::{IdForm, Notice},
};
use crate::{
    admin_components::{
        CHEVRON, CopyTone, Tone, copy_field, key_code, page_title, presence_dot,
        state_badge, step,
    },
    components::{
        avatar::{AvatarSize, avatar, avatar_fallback},
        button::{ButtonSize, ButtonVariant, button},
        input::input,
    },
    hub_client::{HubClient, Role},
    hub_membership::accept,
};

/// An agent this computer can use, from its kept invite.
struct Contact {
    name: String,
    endpoint_id: String,
    days_left: Option<u64>,
    /// The checked card of the agent, when its host sent one.
    card: Option<crate::agent_card::CardView>,
}

#[component]
pub(super) async fn caller_screen(
    cx: &Cx,
    #[default] notice: Notice,
) -> TopcoatResult<impl View> {
    ensure_admin_session(cx).await?;
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(internal_server_error(std::io::Error::other(
            "the caller screen needs a data directory",
        ))
        .into());
    };
    if !page.mode.calls() {
        return Err(internal_server_error(std::io::Error::other(
            "the caller screen needs a data directory",
        ))
        .into());
    }
    let endpoint_id =
        crate::direct::endpoint_id(&page.data_dir).map_err(internal_server_error)?;
    let now = aether_core::time::now_seconds();
    let store =
        crate::direct::open_store(&page.data_dir).map_err(internal_server_error)?;
    let mut contacts = crate::direct::contact_bundles(&store)
        .map_err(internal_server_error)?
        .into_iter()
        .map(|bundle| Contact {
            card: crate::direct::contact_card(&store, &bundle),
            days_left: bundle
                .invitation
                .expires_at
                .checked_sub(now)
                .filter(|left| *left > 0)
                .map(|left| left.div_ceil(24 * 60 * 60)),
            endpoint_id: bundle.invitation.endpoint_id,
            name: bundle.invitation.agent_id,
        })
        .collect::<Vec<_>>();
    contacts.sort_by(|left, right| left.name.cmp(&right.name));
    let connected = !contacts.is_empty();
    // The newest tasks that this computer sent, from its outbox.
    let sent = aether_store::NodeStore::in_dir(&page.data_dir)
        .and_then(|store| store.sent_tasks_page(None, 20, None, false));
    let drafts = super::shared::request_drafts_of(&page.data_dir, now);
    let hub = page.current_hub();
    // The MCP server of this binary and this data directory, so the
    // person's own agent uses the same contacts as this page.
    let mcp_command = mcp_command(&page.data_dir);

    let error = notice.import_error();
    let request_error = notice.request_error();
    let code_open = !error.is_empty();
    Ok(view! {
        if notice.refused() {
            (StatusCode::BAD_REQUEST)
        }
        node_header(current: Tab::Agents)
        <main id="main" class="mx-auto flex max-w-3xl flex-col gap-12 px-4 py-10 sm:px-8 sm:py-12">
            if !drafts.is_empty() || !request_error.is_empty() {
                request_drafts(drafts: drafts.clone(), error: request_error.clone())
            }
            if !connected {
                <div id="connect">
                    page_title(
                        title: "Connect to a friend's agent",
                        if hub.is_some() {
                            "Your own agent can send tasks to your friend's agent. Choose a name, give your friend the full name, and accept their invite here."
                        } else {
                            "Your own agent can send tasks to your friend's agent. It takes two messages to set up."
                        }
                    )
                </div>
            }
            match &hub {
                Some(hub) => {
                    hub_section(
                        hub: hub.clone(),
                        role: Role::Receiver,
                        error: notice.hub_error(),
                        if !connected {
                            code_disclosure(endpoint_id: endpoint_id.clone(), error: error.clone(), open: code_open)
                        }
                    )
                }
                None => {
                    if !connected {
                        connect_steps(endpoint_id: endpoint_id.clone(), error: error.clone())
                    }
                }
            }
            if connected {
                <section aria-labelledby="agents" class="flex flex-col gap-4">
                    <div id="agents" class="flex flex-col gap-1.5">
                        <h2 class="text-xl font-semibold tracking-tight">"Agents you can use"</h2>
                        <p class="text-sm text-muted-foreground">"Your own agent reaches them through Aether. From a terminal, the command next to each one sends a task."</p>
                    </div>
                    <ul class="flex flex-col divide-y divide-border rounded-xl border border-border bg-background" aria-label="Agents">
                        for (index, contact) in contacts.iter().enumerate() {
                            <li class="flex flex-col gap-3 px-5 py-4">
                                <div class="flex flex-wrap items-center gap-3">
                                    avatar(
                                        size: AvatarSize::Sm,
                                        avatar_fallback((initial(&contact.name)))
                                    )
                                    <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                                        <span class="flex flex-wrap items-center gap-2.5">
                                            <span class="font-mono text-sm font-medium">(&contact.name)</span>
                                            presence_dot(endpoint: contact.endpoint_id.clone())
                                        </span>
                                        if let Some(card) = &contact.card {
                                            <span class="text-[13px] text-muted-foreground">
                                                (crate::untrusted::single_line(&card.description, 200))
                                                " · Skill: " (crate::untrusted::single_line(&card.skill_name, crate::agent_card::MAX_SKILL_NAME))
                                            </span>
                                        }
                                    </div>
                                    match contact.days_left {
                                        Some(1) => <span class="text-[13px] text-muted-foreground">"Access ends in 1 day"</span>,
                                        Some(days) => <span class="text-[13px] text-muted-foreground">"Access ends in " (days) " days"</span>,
                                        None => <span class="text-[13px] font-medium text-destructive">"Invite expired. Ask for a new one."</span>,
                                    }
                                    <form method="post" action="/contacts/remove">
                                        <input type="hidden" name="agent" value=(&contact.name)>
                                        button(
                                            variant: ButtonVariant::Outline,
                                            size: ButtonSize::Sm,
                                            attrs: attributes! { type="submit" aria-label=(format!("Remove {}", contact.name)) },
                                            "Remove"
                                        )
                                    </form>
                                </div>
                                if contact.days_left.is_some() {
                                    <div class="sm:pl-11">
                                        copy_field(
                                            id: format!("request-{index}"),
                                            name: "Send a task from a terminal",
                                            value: format!(
                                                "aether node request --scope direct --agent-id {} --text 'Give me three ideas for a project name'",
                                                shell_quote(&contact.name)
                                            ),
                                            tone: CopyTone::Terminal,
                                        )
                                    </div>
                                }
                            </li>
                        }
                    </ul>
                </section>
                <section aria-labelledby="own-agent" class="flex flex-col gap-3 rounded-xl border border-border bg-background p-5">
                    <h2 id="own-agent" class="font-semibold">"Let your own agent send tasks"</h2>
                    <p class="text-sm text-muted-foreground">
                        "Run one of these once. Then ask your agent, for example: “Ask "
                        (contacts.first().map(|contact| contact.name.clone()).unwrap_or_default())
                        "'s agent what this project does.” It uses the tools aether.agents.list, aether.task.send and aether.inbox.read."
                    </p>
                    copy_field(
                        id: "mcp-codex",
                        name: "Codex",
                        value: format!("codex mcp add aether -- {mcp_command}"),
                        tone: CopyTone::Terminal,
                    )
                    copy_field(
                        id: "mcp-claude",
                        name: "Claude Code",
                        value: format!("claude mcp add --scope user aether -- {mcp_command}"),
                        tone: CopyTone::Terminal,
                    )
                </section>
            }
            match &sent {
                Ok(sent) if !sent.is_empty() => {
                    sent_tasks(tasks: sent.clone(), now: now)
                }
                Ok(_) => {}
                Err(_) => {
                    <section aria-labelledby="sent" class="flex flex-col gap-4">
                        <h2 id="sent" class="text-xl font-semibold tracking-tight">"Sent tasks"</h2>
                        <p role="alert" class="text-muted-foreground">"Sent tasks cannot be loaded."</p>
                    </section>
                }
            }
            if connected {
                <section id="connect" aria-labelledby="connect-title" class="flex flex-col gap-4 rounded-xl border border-border px-5 py-4">
                    <h2 id="connect-title" class="font-semibold">"Connect another agent"</h2>
                    match &hub {
                        Some(_) => code_disclosure(endpoint_id: endpoint_id.clone(), error: error.clone(), open: code_open),
                        None => connect_steps(endpoint_id: endpoint_id.clone(), error: error.clone()),
                    }
                </section>
            }
            <section aria-label="Hub" class="rounded-xl border border-border bg-background px-5">
                hub_setting(url: hub.as_ref().map(HubClient::url), error: notice.hub_setting_error())
            </section>
        </main>
    }
    // The page is deep: on the heap, its view does not fill the stack.
    .boxed())
}

/// The two steps of connecting with a code, behind a disclosure: the way
/// when names on the hub do not fit.
#[component]
async fn code_disclosure(
    endpoint_id: String,
    error: String,
    open: bool,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <details class="group border-t border-border pt-4" open=(open.then_some(""))>
            <summary class="flex cursor-pointer list-none items-center gap-2 text-sm font-medium">
                <span class="transition-transform group-open:rotate-90">icon(data: CHEVRON)</span>
                "Connect with a code instead"
            </summary>
            <div class="pt-6">
                connect_steps(endpoint_id: endpoint_id, error: error)
            </div>
        </details>
    })
}

/// Send the connect code, then paste the invite that comes back.
#[component]
async fn connect_steps(endpoint_id: String, error: String) -> TopcoatResult<impl View> {
    Ok(view! {
        <ol class="flex flex-col gap-9">
            step(
                number: 1,
                title: "Send your friend this connect code",
                key_code(id: "connect-code", value: endpoint_id)
                <p class="text-sm text-muted-foreground">
                    "It names this computer. It is not a password, so any chat app is fine."
                </p>
            )
            step(
                number: 2,
                title: "Paste the invite they send back",
                <form method="post" action="/import" class="flex flex-col gap-2">
                    <label for="bundle" class="sr-only">"Invite"</label>
                    <div class="flex flex-col gap-2 sm:flex-row">
                        input(attrs: attributes! {
                            id="bundle" name="bundle" required="" autocomplete="off"
                            spellcheck="false" placeholder="aether-invite:…" class="h-10 font-mono"
                            aria-describedby="bundle-help"
                            aria-invalid=((!error.is_empty()).then_some("true"))
                        })
                        button(attrs: attributes! { type="submit" class="h-10 px-5" }, "Connect")
                    </div>
                    if !error.is_empty() {
                        error_line(text: error.clone())
                    }
                    <p id="bundle-help" class="text-sm text-muted-foreground">
                        "If it says the invite is for another computer, send the code above again. It may have been copied wrong."
                    </p>
                </form>
            )
        </ol>
    })
}

/// The access requests that the person's agent prepared. The node sends one
/// only after **Confirm**.
#[component]
async fn request_drafts(
    drafts: Vec<aether_store::AccessRequestDraft>,
    error: String,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <section id="requests" aria-labelledby="requests-title" class="flex flex-col gap-4">
            <div class="flex flex-col gap-1.5">
                <h2 id="requests-title" class="text-xl font-semibold tracking-tight">"Requests your agent wants to send"</h2>
                <p class="text-sm text-muted-foreground">
                    "Your agent found these agents and asks for access to them. Nothing is sent until you select Confirm."
                </p>
            </div>
            if !error.is_empty() {
                error_line(text: error.clone())
            }
            <ul class="flex flex-col gap-3" aria-label="Requests to confirm">
                for draft in &drafts {
                    request_draft(draft: draft.clone())
                }
            </ul>
        </section>
    })
}

#[component]
async fn request_draft(
    draft: aether_store::AccessRequestDraft,
) -> TopcoatResult<impl View> {
    let listing = crate::listing::parse(&draft.listing.text, &draft.target_name).ok();
    let mut choices =
        [crate::hub_client::protocol::OFFER_DAYS.as_slice(), &[u64::from(draft.days)]]
            .concat();
    choices.sort_unstable();
    choices.dedup();
    Ok(view! {
        <li class="flex flex-col gap-3 rounded-xl border border-primary bg-background p-5 shadow-sm">
            <p class="font-mono font-medium">(&draft.target_name)</p>
            if let Some(listing) = &listing {
                <div class="flex flex-col gap-1.5 rounded-lg border border-border bg-canvas/40 p-3.5 text-sm">
                    <p class="text-xs text-muted-foreground">"The agent's owner wrote this. Aether did not check it."</p>
                    <p>(&listing.description)</p>
                    for skill in &listing.skills {
                        <p><strong>(&skill.name)</strong> ": " (&skill.description)</p>
                    }
                </div>
            }
            <p class="text-sm text-muted-foreground">
                "When this agent connects to yours, each computer can see the public IP address of the other."
            </p>
            <div class="flex flex-wrap items-center gap-2">
                <form method="post" action="/requests/confirm" class="flex items-center gap-2">
                    <input type="hidden" name="id" value=(&draft.id)>
                    <select
                        name="days"
                        aria-label=(format!("Access to {} for", draft.target_name))
                        class="h-8 rounded-lg border border-border bg-background px-2 text-sm"
                    >
                        for days in &choices {
                            <option value=(days) selected=((*days == u64::from(draft.days)).then_some(""))>
                                (if *days == 1 { "1 day".to_owned() } else { format!("{days} days") })
                            </option>
                        }
                    </select>
                    button(size: ButtonSize::Sm, attrs: attributes! { type="submit" }, "Confirm")
                </form>
                <form method="post" action="/requests/discard">
                    <input type="hidden" name="id" value=(&draft.id)>
                    button(
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        attrs: attributes! { type="submit" },
                        "Discard"
                    )
                </form>
            </div>
        </li>
    })
}

#[page(POST "/hub/accept")]
pub(super) async fn accept_invite(
    cx: &Cx,
    Form(form): Form<IdForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node cannot accept invites").await?;
    if !page.mode.calls() {
        return Err(bad_request("this node cannot accept invites").into());
    }
    let Some(hub) = page.current_hub() else {
        return Err(bad_request("this node has no hub").into());
    };
    let error = hub_failure(accept(&hub, &page.data_dir, &form.id).await);
    Ok(view! { caller_screen(notice: Notice::Hub(error.to_owned())) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfirmForm {
    id: String,
    days: u64,
}

/// Sends an access request that the person's agent prepared.
#[page(POST "/requests/confirm")]
pub(super) async fn confirm_request(
    cx: &Cx,
    Form(form): Form<ConfirmForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node sends no access requests").await?;
    let error = match page.hub_access() {
        super::HubAccess::Ready(hub) => {
            confirm(&hub, &page.data_dir, &form.id, form.days)
                .await
                .err()
                .unwrap_or_default()
        }
        super::HubAccess::Off => "Set a hub first: requests go through the hub.",
        super::HubAccess::Unavailable => {
            "This node cannot read its hub settings. Fix the data directory and try again."
        }
    };
    Ok(view! { caller_screen(notice: Notice::Request(error.to_owned())) })
}

/// Sends the draft `id` for `days` days: first to the hub, then the local
/// approval with the listing, so that only a line from the endpoint that
/// signed the listing is kept.
async fn confirm(
    hub: &HubClient,
    data_dir: &std::path::Path,
    id: &str,
    days: u64,
) -> Result<(), &'static str> {
    if !(crate::hub_client::protocol::ACCESS_DAYS_MIN
        ..=crate::hub_client::protocol::ACCESS_DAYS_MAX)
        .contains(&days)
    {
        return Err("Choose 1 to 30 days.");
    }
    let now = aether_core::time::now();
    let store = crate::hub_store::HubStore::open(data_dir)
        .map_err(|_| "The requests are unavailable.")?;
    let draft = store
        .access_request_draft(id)
        .map_err(|_| "The requests are unavailable.")?
        .filter(|draft| draft.expires_at > now)
        .ok_or("This request is gone. Ask your agent again.")?;
    if draft.listing.expires_at <= now {
        let _ = store.remove_access_request_draft(id);
        return Err(
            "The listing of this agent has expired. Ask your agent to find it again.",
        );
    }
    let invite = hub.request_access(&draft.target_name, days).await.map_err(
        |error| match error {
            crate::hub_client::HubError::NotFound => "This agent is not listed now.",
            error => crate::hub_membership::hub_message(error),
        },
    )?;
    let approval = aether_store::HubApproval {
        invite_id: invite.id.clone(),
        side: aether_store::ApprovalSide::Receiver,
        kind: aether_store::ApprovalKind::Request,
        peer_name: draft.target_name.clone(),
        peer_endpoint: None,
        days: u32::try_from(days).unwrap_or(30),
        access_group: crate::access_groups::REQUESTS_GROUP.to_owned(),
        bundle: None,
        created_at: now,
        expires_at: now.saturating_add(crate::hub_membership::APPROVAL_SECONDS),
        listing: Some(draft.listing),
    };
    if store.put_approval(&approval).is_err() {
        let _ = hub.remove(&invite.id).await;
        return Err("This node cannot keep the request. Try again.");
    }
    let _ = store.remove_access_request_draft(id);
    Ok(())
}

#[page(POST "/requests/discard")]
pub(super) async fn discard_request(
    cx: &Cx,
    Form(form): Form<IdForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this node sends no access requests").await?;
    aether_store::NodeStore::in_dir(&page.data_dir)
        .and_then(|store| store.remove_access_request_draft(&form.id))
        .map_err(internal_server_error)?;
    Ok(view! { caller_screen() })
}

/// `aether mcp serve` for this binary and `data_dir`, quoted for a shell.
fn mcp_command(data_dir: &std::path::Path) -> String {
    let binary = std::env::current_exe()
        .map_or_else(|_| "aether".to_owned(), |path| path.display().to_string());
    let data_dir = std::path::absolute(data_dir).map_or_else(
        |_| data_dir.display().to_string(),
        |path| path.display().to_string(),
    );
    format!("{} mcp serve --data-dir {}", shell_quote(&binary), shell_quote(&data_dir))
}

/// `value` as one shell word: as it is when it is safe, else in single
/// quotes.
pub(super) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-~+:=".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// The age of a time `then` at `now`, in plain words.
fn age(then: i64, now: u64) -> String {
    crate::admin_components::ago(
        i64::try_from(now).unwrap_or(i64::MAX).saturating_sub(then),
    )
}

/// The word and the badge tone of a sent task's state.
const fn sent_state(state: aether_store::SentState) -> (&'static str, Tone) {
    match state {
        aether_store::SentState::Waiting | aether_store::SentState::Working => {
            ("Waiting", Tone::Warning)
        }
        aether_store::SentState::Answered => ("Answered", Tone::Success),
        aether_store::SentState::Failed => ("Failed", Tone::Destructive),
        aether_store::SentState::NotDelivered => ("Not delivered", Tone::Destructive),
    }
}

/// The tasks that this computer sent: the agent, the state and its reason,
/// the age, and the answer, which another agent wrote.
#[component]
async fn sent_tasks(
    tasks: Vec<aether_store::SentTask>,
    now: u64,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <section aria-labelledby="sent" class="flex flex-col gap-4">
            <h2 id="sent" class="text-xl font-semibold tracking-tight">"Sent tasks"</h2>
            <ul class="flex flex-col divide-y divide-border border-y border-border" aria-label="Sent tasks">
                for task in &tasks {
                    <li class="flex flex-col gap-2 py-3.5 text-sm">
                        <div class="flex flex-wrap items-center gap-3">
                            <span class="w-36 font-mono text-[13px] font-medium">(&task.agent_id)</span>
                            state_badge(state: sent_state(task.state))
                            <span class="ml-auto text-[13px] text-muted-foreground">(age(task.created_at, now))</span>
                        </div>
                        if let Some(reason) = &task.reason {
                            <p class="text-muted-foreground">(crate::untrusted::single_line(reason, 200))</p>
                        }
                        if let Some(answer) = &task.answer {
                            <details>
                                <summary class="cursor-pointer text-primary">"Answer"</summary>
                                <p class="mt-2 rounded-lg bg-muted px-3 py-2.5 whitespace-pre-wrap">(crate::untrusted::multi_line(answer, 64 * 1024))</p>
                            </details>
                        }
                    </li>
                }
            </ul>
        </section>
    })
}

fn initial(name: &str) -> String {
    name.chars().next().map_or('?', |first| first.to_ascii_uppercase()).to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportForm {
    bundle: String,
}

/// Verifies an invite for this computer and keeps it as a contact.
#[page(POST "/import")]
pub(super) async fn import(
    cx: &Cx,
    Form(form): Form<ImportForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this page cannot import invitations").await?;
    let error = match crate::direct::InvitationBundle::decode(&form.bundle) {
        Err(_) => "This is not an Aether invite. Ask for it again and paste all of it.",
        Ok(bundle) => match crate::direct::import_bundle(&page.data_dir, &bundle) {
            Ok(_) => "",
            Err(crate::direct::DirectError::OtherEndpoint) => {
                "This invite is for another computer."
            }
            Err(crate::direct::DirectError::NameInUse) => {
                "You already have another agent with this name. Remove it first."
            }
            Err(crate::direct::DirectError::Unavailable) => {
                return Err(internal_server_error(std::io::Error::other(
                    "the contacts are unavailable",
                ))
                .into());
            }
            Err(_) => "This invite is not valid or has expired. Ask for a new one.",
        },
    };
    Ok(view! { caller_screen(notice: Notice::Import(error.to_owned())) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveContactForm {
    agent: String,
}

/// Forgets a contact: this computer can no longer send it tasks.
#[page(POST "/contacts/remove")]
pub(super) async fn remove_contact(
    cx: &Cx,
    Form(form): Form<RemoveContactForm>,
) -> TopcoatResult<impl View> {
    let page = admin_page(cx, "this page has no contacts").await?;
    match crate::direct::remove_contact(&page.data_dir, &form.agent) {
        Ok(()) | Err(crate::direct::DirectError::UnknownContact) => {}
        Err(error) => return Err(internal_server_error(error).into()),
    }
    Ok(view! { caller_screen() })
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn a_path_with_a_space_or_a_quote_becomes_one_shell_word() {
        assert_eq!(
            shell_quote("/Users/bob/Downloads/aether"),
            "/Users/bob/Downloads/aether"
        );
        assert_eq!(shell_quote("/Users/bob/My Files"), "'/Users/bob/My Files'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
