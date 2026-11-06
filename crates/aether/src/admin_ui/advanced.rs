//! The Advanced page: the node's counts, task cancellation, live activity,
//! the managed roster, and the changes that wait for the operator's
//! confirmation. `node serve` and `node admin` link to it from their own
//! screen; a UI with neither a host nor a caller shows it at `/`.

use std::{
    path::PathBuf,
    sync::{Arc, PoisonError},
};

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    router::{
        content::Form,
        error::{bad_request, internal_server_error},
        page,
    },
    view::{View, ViewExt, attributes, class, component, view},
};

use super::{
    AdminConfirmations, Managed, Page, ensure_admin_session,
    layout::{Tab, node_header, page_data_dir},
};
use crate::{
    admin::{AdminStoreAccess, SharedAdministration},
    admin_components::page_title,
    components::{
        alert::{AlertVariant, alert, alert_description, alert_title},
        button::{ButtonSize, ButtonVariant, button, button_variants},
        input::input,
        label::label,
    },
    policy::{PolicySettings, policy_settings},
};
use aether_store::{ChangeKind, ManagedMember};

#[page("/advanced")]
pub(super) async fn advanced() -> TopcoatResult<impl View> {
    Ok(view! { dashboard() })
}

/// The Advanced page's content, shared by `/advanced` and by `confirm` and
/// `cancel`, which re-render it in place instead of redirecting.
#[component]
pub(super) async fn dashboard(
    cx: &Cx,
    #[default] policy_saved: bool,
) -> TopcoatResult<impl View> {
    ensure_admin_session(cx).await?;
    let Page(page) = app_context::<Page>(cx);
    let data_dir = page_data_dir(cx);
    let policy = data_dir
        .map(|path| policy_settings(&path))
        .transpose()
        .map_err(internal_server_error)?
        .flatten();

    let administration = app_context::<SharedAdministration>(cx);
    let (store_access, pending, configured, summary, members) = {
        let administration =
            administration.lock().unwrap_or_else(PoisonError::into_inner);
        (
            administration.store_access(),
            administration.pending(),
            administration.configured_change_counts(),
            administration.node_summary(),
            administration.managed_members(),
        )
    };
    let tokens = app_context::<Arc<AdminConfirmations>>(cx).reset(pending)?;
    let Managed(managed) = *app_context::<Managed>(cx);

    let applied = |kind| configured.get(&kind).copied().unwrap_or(0);
    let capabilities = applied(ChangeKind::Capability);
    let directories = applied(ChangeKind::Directory);
    let grants = applied(ChangeKind::Grant);
    let workspaces = applied(ChangeKind::Workspace);

    Ok(view! {
        node_header(current: Tab::Advanced)
        <div class="mx-auto flex max-w-6xl gap-14 px-4 py-10 sm:px-8">
            <nav aria-label="Control sections" class="navigation-bar sticky top-6 hidden w-44 shrink-0 flex-col gap-0.5 self-start pt-24 lg:flex">
                <a class=(SECTION_LINK) href="#pending">"Pending changes"</a>
                <a class=(SECTION_LINK) href="#overview">"Overview"</a>
                <a class=(SECTION_LINK) href="#permissions">"Permissions"</a>
                <a class=(SECTION_LINK) href="#policy">"Task policy"</a>
                <a class=(SECTION_LINK) href="#tasks">"Tasks"</a>
                <a class=(SECTION_LINK) href="#activity">"Activity"</a>
                if managed {
                    <a class=(SECTION_LINK) href="#members">"Members"</a>
                }
            </nav>
        <main id="main" class="flex min-w-0 max-w-3xl flex-1 flex-col gap-10">
            page_title(
                title: "Local control",
                "Review local permissions, tasks and pending changes."
            )

            if matches!(store_access, AdminStoreAccess::Unavailable) {
                alert(variant: AlertVariant::Destructive, attrs: attributes! { role="alert" },
                    alert_title("Administration store is unavailable")
                    alert_description("This node cannot read its local administration database. Fix the data directory and reload.")
                )
            }

            <section id="pending" class="flex scroll-mt-6 flex-col gap-3">
                <h2 class="text-xl font-semibold tracking-tight">"Pending changes"</h2>
                if matches!(store_access, AdminStoreAccess::Unavailable) {
                    <p class="text-muted-foreground">"Pending changes cannot be loaded."</p>
                } else if tokens.is_empty() {
                    <p class="text-muted-foreground">"No changes need confirmation."</p>
                } else {
                    <ul class="flex flex-col gap-2">
                        for entry in &tokens {
                            <li class="flex flex-wrap items-center justify-between gap-4 rounded-xl border border-warning/40 bg-warning/5 px-5 py-4">
                                <div class="flex flex-col gap-0.5">
                                    <p class="font-semibold">"Local change"</p>
                                    <p class="text-sm text-muted-foreground">"Review and confirm this change."</p>
                                </div>
                                <form method="post" action="/confirm">
                                    <input type="hidden" name="id" value=(entry)>
                                    button(
                                        variant: ButtonVariant::Primary,
                                        attrs: attributes! { type="submit" },
                                        "Confirm change"
                                    )
                                </form>
                            </li>
                        }
                    </ul>
                }
            </section>

            <section id="overview" class="flex scroll-mt-6 flex-col gap-3">
            <h2 class="text-xl font-semibold tracking-tight">"Overview"</h2>
            if matches!(store_access, AdminStoreAccess::Unavailable) {
                <p class="text-muted-foreground">"Overview counts cannot be loaded."</p>
            } else {
                <dl class="grid grid-cols-2 gap-px overflow-hidden rounded-xl border border-border bg-border sm:grid-cols-3">
                    count(id: "agents", name: "Agents", value: summary.agents, unit: "known agents")
                    count(id: "capabilities", name: "Capabilities", value: capabilities, unit: "configured capabilities")
                    count(id: "directories", name: "Directories", value: directories, unit: "configured directories")
                    count(id: "connections", name: "Connections", value: summary.active_connections, unit: "active connections")
                    count(id: "grant-count", name: "Permissions", value: grants, unit: "configured grants")
                    count(id: "workspaces", name: "Workspaces", value: workspaces, unit: "configured workspaces")
                </dl>
            }
            </section>

            <section id="permissions" class="flex scroll-mt-6 flex-col gap-3">
                <h2 class="text-xl font-semibold tracking-tight">"Permissions"</h2>
                if page.as_ref().is_some_and(|page| page.host_ui) {
                    <p class="text-muted-foreground">"Invite a person to send tasks to your agent. Access groups control who can send tasks."</p>
                    <div class="flex flex-wrap gap-2">
                        <a href="/#invite" class=(class!(button_variants(ButtonVariant::Primary, ButtonSize::Md), "no-underline"))>"Invite someone"</a>
                        <a href="/#groups" class=(class!(button_variants(ButtonVariant::Outline, ButtonSize::Md), "no-underline"))>"Manage access groups"</a>
                    </div>
                } else {
                    <p class="text-muted-foreground">"Start your node with a sharing page to manage who can send tasks."</p>
                }
            </section>

            <section id="policy" class="flex scroll-mt-6 flex-col gap-3">
                <h2 class="text-xl font-semibold tracking-tight">"Task policy"</h2>
                if let Some(policy) = policy {
                    <p class="text-muted-foreground">"Set the folder for new task folders and the task limits. Restart the node to use saved changes."</p>
                    if policy_saved {
                        <p role="status" class="rounded-lg border border-warning/40 bg-warning/5 px-3.5 py-2.5 text-sm text-warning">"Policy saved. Restart the node to use these settings."</p>
                    }
                    <form method="post" action="/policy" class="flex flex-col gap-4 rounded-xl border border-border bg-background p-5">
                        <div class="flex flex-col gap-1.5">
                            label(attrs: attributes! { for="policy_workspace" }, "Task folder root")
                            input(attrs: attributes! { id="policy_workspace" name="workspace" required="" value=(policy.workspace.display().to_string()) })
                            <p class="text-sm text-muted-foreground">"This folder must exist. Each task gets a new empty folder inside it."</p>
                        </div>
                        <div class="grid gap-4 sm:grid-cols-2">
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="max_context_tokens" }, "Max context tokens")
                                input(attrs: attributes! { id="max_context_tokens" name="max_context_tokens" type="number" min="1" required="" value=(policy.max_context_tokens.to_string()) })
                            </div>
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="max_seconds" }, "Max task time (seconds)")
                                input(attrs: attributes! { id="max_seconds" name="max_seconds" type="number" min="1" required="" value=(policy.max_seconds.to_string()) })
                            </div>
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="queue_global" }, "Max waiting tasks")
                                input(attrs: attributes! { id="queue_global" name="queue_global" type="number" min="1" required="" value=(policy.queue_global.to_string()) })
                            </div>
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="queue_per_peer" }, "Max waiting tasks per person")
                                input(attrs: attributes! { id="queue_per_peer" name="queue_per_peer" type="number" min="1" required="" value=(policy.queue_per_peer.to_string()) })
                            </div>
                        </div>
                        button(variant: ButtonVariant::Primary, attrs: attributes! { type="submit" class="w-fit" }, "Save task policy")
                    </form>
                } else {
                    <p class="text-muted-foreground">"Start the node once to create its task policy."</p>
                }
            </section>

            <section id="tasks" class="flex scroll-mt-6 flex-col gap-3">
                <h2 class="text-xl font-semibold tracking-tight">"Tasks"</h2>
                if matches!(store_access, AdminStoreAccess::Unavailable) {
                    <p class="text-muted-foreground">"Task counts cannot be loaded."</p>
                } else {
                    <p class="text-muted-foreground">
                        (summary.tasks) " stored tasks. " (summary.queued_tasks) " tasks are waiting."
                    </p>
                }
                <form method="post" action="/cancel" class="flex flex-wrap items-end gap-2">
                    <div class="flex min-w-64 flex-1 flex-col gap-1.5">
                        label(attrs: attributes! { for="task_id" }, "Task ID")
                        input(attrs: attributes! { id="task_id" name="task_id" required="" class="font-mono" })
                    </div>
                    button(variant: ButtonVariant::Outline, attrs: attributes! { type="submit" }, "Cancel task")
                </form>
            </section>

            <section id="activity" class="flex scroll-mt-6 flex-col gap-2">
                <h2 class="text-xl font-semibold tracking-tight">"Activity"</h2>
                <p id="activity-status" role="status" class="rounded-lg bg-muted px-3.5 py-2.5 font-mono text-[13px] text-foreground">
                    "The local control service is ready."
                </p>
            </section>

            if managed {
                <section id="members" class="flex flex-col gap-2">
                    <h2 class="text-xl font-semibold tracking-tight">"Members"</h2>
                    if matches!(store_access, AdminStoreAccess::Unavailable) {
                        <p class="text-muted-foreground">"Members cannot be loaded."</p>
                    } else if members.is_empty() {
                        <p class="text-muted-foreground">"No members are listed."</p>
                    } else {
                        <ul class="flex flex-col gap-2">
                            for member in &members {
                                <li>
                                    <p>
                                        (&member.agent_id) " in " (&member.scope_id) ": "
                                        (managed_member_state(member)) "."
                                    </p>
                                    <p class="text-sm text-muted-foreground">"Endpoint " (&member.endpoint_id) "."</p>
                                </li>
                            }
                        </ul>
                    }
                </section>
                <section id="invitations" class="flex flex-col gap-2">
                    <h2 class="text-xl font-semibold tracking-tight">"Invitations"</h2>
                    <p class="text-muted-foreground">"Manage invitations with the control command."</p>
                </section>
            }
        </main>
        </div>
    }
    // The page is deep: on the heap, its view does not fill the stack.
    .boxed())
}

/// The look of a link in the Advanced page's section list.
const SECTION_LINK: &str = "rounded-md px-2.5 py-1.5 text-sm text-muted-foreground no-underline hover:bg-foreground/5 hover:text-foreground";

/// One configured count, with its heading kept as the anchor target.
#[component]
async fn count(
    id: &'static str,
    name: &'static str,
    value: usize,
    unit: &'static str,
) -> TopcoatResult<impl View> {
    Ok(view! {
        <div id=(id) class="flex flex-col gap-1 bg-background px-4 py-3.5">
            <dt class="text-[13px] text-muted-foreground">(name)</dt>
            <dd class="text-[15px] font-semibold">(format!("{value} {unit}."))</dd>
        </div>
    })
}

fn managed_member_state(member: &ManagedMember) -> &'static str {
    if member.revoked_at.is_some() {
        return "revoked";
    }
    let now = aether_core::time::now();
    if member.expires_at <= now { "expired" } else { "active" }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfirmForm {
    id: String,
}

#[page(POST "/confirm")]
pub(super) async fn confirm(
    cx: &Cx,
    Form(form): Form<ConfirmForm>,
) -> TopcoatResult<impl View> {
    super::require_admin_session(cx).await?;
    let Some(id) = app_context::<Arc<AdminConfirmations>>(cx).resolve(&form.id) else {
        return Err(bad_request("this change is no longer available").into());
    };
    let result = app_context::<SharedAdministration>(cx)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .confirm_ui(&id);
    if result.is_err() {
        return Err(bad_request("this change is no longer available").into());
    }
    Ok(view! { dashboard() })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CancelForm {
    task_id: String,
}

#[page(POST "/cancel")]
pub(super) async fn cancel(
    cx: &Cx,
    Form(form): Form<CancelForm>,
) -> TopcoatResult<impl View> {
    super::require_admin_session(cx).await?;
    let result = app_context::<SharedAdministration>(cx)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .cancel_task(&form.task_id);
    if result.is_err() {
        return Err(bad_request("the task cannot be cancelled").into());
    }
    Ok(view! { dashboard() })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PolicyForm {
    workspace: String,
    max_context_tokens: u32,
    max_seconds: u64,
    queue_global: usize,
    queue_per_peer: usize,
}

#[page(POST "/policy")]
pub(super) async fn save_policy(
    cx: &Cx,
    Form(form): Form<PolicyForm>,
) -> TopcoatResult<impl View> {
    super::require_admin_session(cx).await?;
    page_data_dir(cx).ok_or_else(|| bad_request("this page has no node data"))?;
    let settings = PolicySettings {
        workspace: PathBuf::from(form.workspace),
        max_context_tokens: form.max_context_tokens,
        max_seconds: form.max_seconds,
        queue_global: form.queue_global,
        queue_per_peer: form.queue_per_peer,
    };
    app_context::<SharedAdministration>(cx)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .save_policy_settings(&settings, aether_store::AuditActor::Ui)
        .map_err(|_| {
            bad_request("use an existing absolute folder and limits above zero")
        })?;
    Ok(view! { dashboard(policy_saved: true) })
}
