use topcoat::{
    Result as TopcoatResult,
    context::Cx,
    router::page,
    view::{View, ViewExt, attributes, component, view},
};

use super::{
    administration, ensure_admin_session,
    layout::{Tab, node_header},
};
use crate::{
    admin::AdminStoreAccess,
    admin_components::page_title,
    components::alert::{AlertVariant, alert, alert_description, alert_title},
};

#[page("/audit")]
pub(super) async fn audit() -> TopcoatResult<impl View> {
    Ok(view! { audit_screen() })
}

#[component]
pub(super) async fn audit_screen(cx: &Cx) -> TopcoatResult<impl View> {
    ensure_admin_session(cx).await?;
    let (store_access, entries) = {
        let administration = administration(cx);
        (administration.store_access(), administration.audit_entries())
    };
    let store_unavailable =
        matches!(store_access, AdminStoreAccess::Unavailable) || entries.is_err();
    let entries = entries.unwrap_or_default();
    Ok(view! {
        node_header(current: Tab::Advanced)
        <main id="main" class="mx-auto flex max-w-3xl flex-col gap-8 px-4 py-10 sm:px-8">
            page_title(
                title: "Administration audit",
                "Redacted results of local administration actions."
            )
            if store_unavailable {
                alert(variant: AlertVariant::Destructive, attrs: attributes! { role="alert" },
                    alert_title("Administration store is unavailable")
                    alert_description("This node cannot read its local administration database. Fix the data directory and reload.")
                )
            } else if entries.is_empty() {
                <p class="text-muted-foreground">"No audit records yet."</p>
            } else {
                <ul class="flex flex-col divide-y divide-border rounded-xl border border-border bg-background font-mono text-[13px]" aria-label="Audit records">
                    for entry in &entries {
                        <li class="px-4 py-3">
                            (&entry.actor) " · " (&entry.action) " · " (&entry.result)
                        </li>
                    }
                </ul>
            }
        </main>
    }
    .boxed())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::admin_ui::test_support::{admin_ui_with_directory_applier, app, get};

    #[tokio::test]
    async fn audit_page_shows_a_store_error_instead_of_an_empty_list() {
        let (directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let db_path = directory.path().join(aether_store::NODE_DATABASE);
        rusqlite::Connection::open(&db_path)
            .expect("database must open")
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("invalid audit row must insert");
        let response = get(app(admin_ui), "/audit").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Administration store is unavailable"));
        assert!(!response.body.contains("No audit records yet"));
    }
}
