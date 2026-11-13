//! The Advanced page: the node's counts, task cancellation, live activity,
//! the managed roster, and the changes that wait for the operator's
//! confirmation. `node serve` and `node admin` link to it from their own
//! screen; a UI with neither a host nor a caller shows it at `/`.

use std::{path::PathBuf, sync::Arc};

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
    AdminConfirmations, Managed, Page, administration, ensure_admin_session,
    layout::{Tab, node_header, page_data_dir},
    shared::IdForm,
};
use crate::{
    admin::AdminStoreAccess,
    admin_components::{page_title, post_button, section_heading},
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

    let (store_access, pending, configured, summary, members) = {
        let administration = administration(cx);
        (
            administration.store_access(),
            administration.pending(),
            administration.configured_change_counts(),
            administration.node_summary(),
            administration.managed_members(),
        )
    };
    let store_unavailable = matches!(store_access, AdminStoreAccess::Unavailable)
        || pending.is_err()
        || configured.is_err()
        || summary.is_err()
        || members.is_err();
    let pending = pending.unwrap_or_default();
    let configured = configured.unwrap_or_default();
    let summary = summary.unwrap_or_default();
    let members = members.unwrap_or_default();
    let tokens = if store_unavailable {
        Vec::new()
    } else {
        app_context::<Arc<AdminConfirmations>>(cx).reset(pending)?
    };
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

            if store_unavailable {
                alert(variant: AlertVariant::Destructive, attrs: attributes! { role="alert" },
                    alert_title("Administration store is unavailable")
                    alert_description("This node cannot read its local administration database. Fix the data directory and reload.")
                )
            }

            <section id="pending" class="flex scroll-mt-6 flex-col gap-3">
                section_heading("Pending changes")
                if store_unavailable {
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
                                post_button(
                                    action: "/confirm",
                                    fields: vec![("id", entry.clone())],
                                    variant: ButtonVariant::Primary,
                                    "Confirm change"
                                )
                            </li>
                        }
                    </ul>
                }
            </section>

            <section id="overview" class="flex scroll-mt-6 flex-col gap-3">
            section_heading("Overview")
            if store_unavailable {
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
                section_heading("Permissions")
                if page.as_ref().is_some_and(|page| page.mode.hosts()) {
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
                section_heading("Task policy")
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
                                input(attrs: attributes! { id="queue_global" name="queue_global" type="number" min="1" required="" value=(policy.queue.get(aether_core::limits::QueueLimitId::Global).to_string()) })
                            </div>
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="queue_per_peer" }, "Max waiting tasks per person")
                                input(attrs: attributes! { id="queue_per_peer" name="queue_per_peer" type="number" min="1" required="" value=(policy.queue.get(aether_core::limits::QueueLimitId::PerPeer).to_string()) })
                            </div>
                            <div class="flex flex-col gap-1.5 sm:col-span-2">
                                label(attrs: attributes! { for="running_tasks" }, "Tasks running at once")
                                input(attrs: attributes! { id="running_tasks" name="running_tasks" type="number" min=(aether_core::limits::QueueLimitId::Running.minimum().to_string()) max=(aether_core::limits::MAX_RUNNING_TASKS.to_string()) required="" value=(policy.queue.get(aether_core::limits::QueueLimitId::Running).to_string()) })
                                <p class="text-sm text-muted-foreground">"For all people together. One runs a task at a time. A higher number uses more of this computer, and other tasks wait until one ends. Lowering it never stops a running task. An agent that takes fewer tasks at once, such as Codex with a shared workspace or a connected agent that takes one, still runs that many."</p>
                            </div>
                        </div>
                        button(variant: ButtonVariant::Primary, attrs: attributes! { type="submit" class="w-fit" }, "Save task policy")
                    </form>
                } else {
                    <p class="text-muted-foreground">"Start the node once to create its task policy."</p>
                }
            </section>

            <section id="tasks" class="flex scroll-mt-6 flex-col gap-3">
                section_heading("Tasks")
                if store_unavailable {
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
                section_heading("Activity")
                <p id="activity-status" role="status" class="rounded-lg bg-muted px-3.5 py-2.5 font-mono text-[13px] text-foreground">
                    "The local control service is ready."
                </p>
            </section>

            if managed {
                <section id="members" class="flex flex-col gap-2">
                    section_heading("Members")
                    if store_unavailable {
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
                    section_heading("Invitations")
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

#[page(POST "/confirm")]
pub(super) async fn confirm(
    cx: &Cx,
    Form(form): Form<IdForm>,
) -> TopcoatResult<impl View> {
    super::require_admin_session(cx).await?;
    let confirmed = app_context::<Arc<AdminConfirmations>>(cx)
        .confirm(&form.id, |id| administration(cx).confirm_ui(id));
    if !matches!(confirmed, Some(Ok(()))) {
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
    let result = administration(cx).cancel_task(&form.task_id);
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
    running_tasks: usize,
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
        queue: crate::policy::QueuePolicyLimits::new(
            form.queue_global,
            form.queue_per_peer,
        )
        .with_running(form.running_tasks),
    };
    administration(cx)
        .save_policy_settings(&settings, aether_store::AuditActor::Ui)
        .map_err(|_| {
            bad_request("use an existing absolute folder and limits in range")
        })?;
    Ok(view! { dashboard(policy_saved: true) })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;

    use super::AdminConfirmations;
    use crate::admin::{
        ChangeRequest, shared_with_store,
        test_support::{FakeNode, shared_in},
    };
    use crate::admin_ui::{
        UiOptions, build_admin_ui,
        test_support::{
            PORT, admin_ui_with_directory_applier, app, get, host_ui, post, session,
            session_cookie_pair,
        },
    };
    use aether_store::{AuditActor, ManagedMember, NodeStore, QueueLimits, TaskInput};

    #[test]
    fn a_failed_confirm_keeps_its_token_and_a_confirmed_one_uses_it_up() {
        let confirmations = AdminConfirmations::new();
        let tokens = confirmations
            .reset(vec![crate::admin::PendingChange {
                id: "workspace".to_owned(),
                kind: "workspace".to_owned(),
            }])
            .expect("tokens must issue");
        let [token] = tokens.as_slice() else { panic!("one token must issue") };
        assert_eq!(confirmations.confirm(token, |_| Err("store")), Some(Err("store")));
        let mut confirmed = Vec::new();
        let result = confirmations.confirm(token, |id| {
            confirmed.push(id.to_owned());
            Ok::<(), &str>(())
        });
        assert_eq!(result, Some(Ok(())));
        assert_eq!(confirmed, ["workspace"]);
        assert_eq!(confirmations.confirm(token, |_| Ok::<(), &str>(())), None);
    }

    fn managed_options() -> UiOptions {
        UiOptions { managed: true, ..UiOptions::default() }
    }

    #[tokio::test]
    async fn a_ui_without_a_running_node_cannot_invite() {
        let directory = tempfile::tempdir().expect("directory must create");
        let admin_ui =
            build_admin_ui(shared_in(directory.path()), UiOptions::default(), PORT);
        let home = get(app(admin_ui), "/").await;
        assert!(!home.body.contains("Invite someone"));
        assert!(home.body.contains("Pending changes"));
    }

    #[tokio::test]
    async fn ui_cancellation_uses_the_live_node_callback() {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        let node = Arc::new(FakeNode::default());
        administration.lock().expect("administration must lock").set_live(node.clone());
        let admin_ui = build_admin_ui(administration, UiOptions::default(), PORT);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let response = post(
            application,
            "/cancel",
            Some(cookie),
            "task_id=00000000-0000-4000-8000-000000000001",
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(node.calls(), ["cancel 00000000-0000-4000-8000-000000000001"]);
    }

    #[tokio::test]
    async fn advanced_page_shows_a_store_error_instead_of_empty_counts() {
        let (directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let db_path = directory.path().join(aether_store::NODE_DATABASE);
        rusqlite::Connection::open(&db_path)
            .expect("database must open")
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("invalid audit row must insert");
        let response = get(app(admin_ui), "/advanced").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Administration store is unavailable"));
        assert!(response.body.contains("Overview counts cannot be loaded"));
        assert!(!response.body.contains("0 known agents"));
    }

    #[tokio::test]
    async fn advanced_page_saves_task_policy_in_the_node_database() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("node");
        crate::node_builder::load_or_create_service(
            &directory.path().join("missing-policy.json"),
            &data_dir,
            "codex".into(),
            None,
        )
        .expect("first node policy must create");
        let (admin_ui, _) = host_ui(&data_dir);
        let application = app(admin_ui);
        let cookie = session(&application).await;
        let mut form =
            reqwest::Url::parse("http://form.test/").expect("form URL must parse");
        form.query_pairs_mut()
            .append_pair(
                "workspace",
                &directory.path().join("tasks").display().to_string(),
            )
            .append_pair("max_context_tokens", "12000")
            .append_pair("max_seconds", "90")
            .append_pair("queue_global", "3")
            .append_pair("queue_per_peer", "2")
            .append_pair("running_tasks", "3");
        let denied = post(
            application.clone(),
            "/policy",
            None,
            form.query().expect("form must encode"),
        )
        .await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        let response = post(
            application.clone(),
            "/policy",
            Some(&cookie),
            form.query().expect("form must encode"),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body.contains("Policy saved"));
        let settings = crate::policy::policy_settings(&data_dir)
            .expect("policy must read")
            .expect("policy must exist");
        assert_eq!(settings.max_context_tokens, 12_000);
        assert_eq!(settings.max_seconds, 90);
        assert_eq!(settings.queue.global, 3);
        assert_eq!(settings.queue.per_peer, 2);
        assert_eq!(settings.queue.running, 3);
    }

    #[tokio::test]
    async fn page_shows_durable_change_counts_without_values() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .apply_change(
                aether_store::ChangeKind::Capability,
                "review",
                "private-credential",
                AuditActor::Mcp,
                1,
            )
            .expect("change must apply");
        store
            .put_scope("scope", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("task"),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("agent"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("scope"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: "fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "private task data".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must admit");
        let admin_ui =
            build_admin_ui(shared_with_store(store), UiOptions::default(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(
            response.body.contains("1 configured capabilities."),
            "{}",
            response.body
        );
        assert!(response.body.contains("1 stored tasks. 1 tasks are waiting."));
        assert!(!response.body.contains("private-credential"));
        assert!(!response.body.contains("private task data"));
    }

    #[tokio::test]
    async fn managed_sections_require_a_managed_scope() {
        let directory = tempfile::tempdir().expect("directory must create");
        let admin_ui =
            build_admin_ui(shared_in(directory.path()), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("id=\"members\""));
        assert!(response.body.contains("id=\"invitations\""));
    }

    #[tokio::test]
    async fn managed_page_shows_cached_member_states() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        for (cursor, agent, endpoint, expires_at) in [
            (1, "active-agent", "active-endpoint", i64::MAX),
            (2, "revoked-agent", "revoked-endpoint", i64::MAX),
            (3, "expired-agent", "expired-endpoint", 1),
        ] {
            store
                .apply_managed_member(
                    "https://control.test",
                    cursor,
                    &ManagedMember {
                        scope_id: "managed".to_owned(),
                        agent_id: agent.to_owned(),
                        endpoint_id: endpoint.to_owned(),
                        role: "member".to_owned(),
                        expires_at,
                        revoked_at: None,
                    },
                )
                .expect("member must store");
        }
        store
            .revoke_managed_member(
                "https://control.test",
                4,
                "managed",
                "revoked-agent",
            )
            .expect("member must revoke");
        let admin_ui =
            build_admin_ui(shared_with_store(store), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("active-agent in managed: active."));
        assert!(response.body.contains("revoked-agent in managed: revoked."));
        assert!(response.body.contains("expired-agent in managed: expired."));
    }

    #[tokio::test]
    async fn member_names_are_escaped_in_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .apply_managed_member(
                "https://control.test",
                1,
                &ManagedMember {
                    scope_id: "managed".to_owned(),
                    agent_id: "<script>alert(1)</script>".to_owned(),
                    endpoint_id: "endpoint&id".to_owned(),
                    role: "member".to_owned(),
                    expires_at: i64::MAX,
                    revoked_at: None,
                },
            )
            .expect("member must store");
        let admin_ui =
            build_admin_ui(shared_with_store(store), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(response.body.contains("endpoint&amp;id"));
        assert!(!response.body.contains("<script>alert(1)</script>"));
    }

    #[tokio::test]
    async fn pending_changes_show_no_secret_value() {
        let (_directory, admin_ui, administration) = admin_ui_with_directory_applier();
        administration
            .lock()
            .expect("administration must lock")
            .submit(
                &{
                    let grant = crate::admin::peer_grant();
                    ChangeRequest::Grant { id: grant.id.as_str().to_owned(), grant }
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must add");
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Confirm change"));
        assert!(!home.body.contains("08080808080808080808080808080808"));
        let token = extract_confirm_token(&home.body)
            .expect("a confirmation token must render");
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("cookie must be set"),
        );
        let response =
            post(application, "/confirm", Some(cookie), &format!("id={token}")).await;
        assert!(response.body.contains("No changes need confirmation."));
        assert_eq!(
            administration
                .lock()
                .expect("administration must unlock")
                .audit()
                .expect("audit"),
            ["pending", "confirmed", "applied"]
        );
    }

    fn extract_confirm_token(body: &str) -> Option<&str> {
        let (_, rest) = body.split_once("name=\"id\" value=\"")?;
        let (token, _) = rest.split_once('"')?;
        Some(token)
    }
}
