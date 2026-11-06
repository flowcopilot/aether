use std::sync::PoisonError;

use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    router::page,
    view::{View, ViewExt, attributes, component, view},
};

use super::{
    ensure_admin_session,
    layout::{Tab, node_header},
};
use crate::{
    admin::{AdminStoreAccess, SharedAdministration},
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
        let administration = app_context::<SharedAdministration>(cx)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (administration.store_access(), administration.audit_entries())
    };
    Ok(view! {
        node_header(current: Tab::Advanced)
        <main id="main" class="mx-auto flex max-w-3xl flex-col gap-8 px-4 py-10 sm:px-8">
            page_title(
                title: "Administration audit",
                "Redacted results of local administration actions."
            )
            if matches!(store_access, AdminStoreAccess::Unavailable) {
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
