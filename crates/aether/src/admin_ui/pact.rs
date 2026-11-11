//! The PACT consent page (spec §5.3).
//!
//! A personal agent shows its user the link to this page. The node owner,
//! who holds the admin session, sees which agent asks for which Brand and
//! scope, and approves or denies. Approval adds an ordinary grant, which the
//! owner can revoke like any other.

use std::sync::Arc;

use serde::Deserialize;
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    icon::icon,
    router::{content::Form, error::bad_request, page, parse_query_params},
    view::{View, attributes, component, view},
};

use super::{Pact, ensure_admin_session, require_admin_session};
use crate::{
    admin::SharedAdministration,
    admin_components::{ALERT, CHECK, CLOCK, CROSS, app_header, page_title},
    components::{
        button::{ButtonVariant, button},
        card::{
            card, card_content, card_description, card_footer, card_header, card_title,
        },
        input::input,
        label::label,
    },
    pact::consent::{ConsentError, PactConsent},
};

#[derive(Deserialize, Default)]
struct ConsentQuery {
    #[serde(default)]
    user_code: Option<String>,
}

/// The link a personal agent shows, with or without the code in it.
#[page("/pact/consent")]
pub(super) async fn consent_page(cx: &Cx) -> TopcoatResult<impl View> {
    ensure_admin_session(cx).await?;
    let query: ConsentQuery = parse_query_params(cx).unwrap_or_default();
    Ok(view! { consent_screen(user_code: query.user_code.unwrap_or_default()) })
}

/// The two submit buttons of the consent form.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Decision {
    Allow,
    Deny,
}

impl Decision {
    /// The button value; `Deserialize` reads the same names back.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionForm {
    user_code: String,
    decision: Decision,
    /// Present when its checkbox stays checked. A Brand has one scope.
    #[serde(default)]
    scope: Option<String>,
}

#[page(POST "/pact/consent")]
pub(super) async fn consent_decision(
    cx: &Cx,
    Form(form): Form<DecisionForm>,
) -> TopcoatResult<impl View> {
    require_admin_session(cx).await?;
    let consent = pact_consent(cx)?;
    let outcome = match form.decision {
        Decision::Allow => {
            let chosen = form.scope.into_iter().collect::<Vec<_>>();
            match consent.approve(
                &form.user_code,
                &chosen,
                app_context::<SharedAdministration>(cx),
            ) {
                Ok(granted) if granted.is_empty() => Outcome::Denied,
                Ok(_) => Outcome::Approved,
                Err(ConsentError::NotPending) => Outcome::Expired,
                Err(_) => Outcome::Failed,
            }
        }
        Decision::Deny => match consent.deny(&form.user_code) {
            Ok(()) => Outcome::Denied,
            Err(_) => Outcome::Expired,
        },
    };
    Ok(view! { decided(outcome: outcome) })
}

fn pact_consent(cx: &Cx) -> TopcoatResult<Arc<PactConsent>> {
    let Pact(consent) = app_context::<Pact>(cx);
    consent.clone().ok_or_else(|| bad_request("this node does not offer PACT").into())
}

#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Approved,
    Denied,
    Expired,
    Failed,
}

#[component]
async fn consent_screen(cx: &Cx, user_code: String) -> TopcoatResult<impl View> {
    let consent = pact_consent(cx)?;
    let request =
        (!user_code.is_empty()).then(|| consent.pending(&user_code)).flatten();
    Ok(view! {
        app_header(section: "Personal agents")
        <main id="main" class="mx-auto flex max-w-2xl flex-col gap-8 px-4 py-10 sm:px-8">
            page_title(
                title: "Let a personal agent work for you",
                "Only approve a request that you started yourself, from your own agent."
            )
            if let Some(request) = request {
                card(
                    card_header(
                        card_title((format!("{} asks to use {}", request.pa_origin, request.brand_name)))
                        card_description(
                            "The agent identifies itself as "
                            <code class="rounded-md bg-muted px-1.5 py-0.5 break-all">(&request.pa_issuer)</code>
                            ". Code "
                            <code class="rounded-md bg-muted px-1.5 py-0.5">(&request.user_code)</code>
                            "."
                        )
                    )
                    <form method="post" action="/pact/consent">
                        card_content(
                            <input type="hidden" name="user_code" value=(&request.user_code)>
                            <fieldset class="flex flex-col gap-2">
                                <legend class="mb-2 font-medium">"It may:"</legend>
                                for (id, description) in &request.scopes {
                                    <label class="flex items-start gap-2">
                                        <input type="checkbox" name="scope" value=(id) checked="">
                                        <span>(description)</span>
                                    </label>
                                }
                            </fieldset>
                            <p class="mt-4 text-sm text-muted-foreground">
                                "The access lasts 30 days. You can revoke it at any time from your sharing page."
                            </p>
                        )
                        card_footer(
                            <div class="flex gap-2">
                                button(
                                    variant: ButtonVariant::Primary,
                                    attrs: attributes! { type="submit" name="decision" value=(Decision::Allow.as_str()) },
                                    "Allow"
                                )
                                button(
                                    variant: ButtonVariant::Outline,
                                    attrs: attributes! { type="submit" name="decision" value=(Decision::Deny.as_str()) },
                                    "Deny"
                                )
                            </div>
                        )
                    </form>
                )
            } else {
                card(
                    card_header(
                        card_title("Enter the code from your agent")
                        if !user_code.is_empty() {
                            card_description("This code expired, or it was already used. Start again from your agent.")
                        }
                    )
                    <form method="get" action="/pact/consent">
                        card_content(
                            <div class="flex flex-col gap-1.5">
                                label(attrs: attributes! { for="user_code" }, "Code")
                                input(attrs: attributes! { id="user_code" name="user_code" required="" autocomplete="off" class="font-mono tracking-widest uppercase" })
                            </div>
                        )
                        card_footer(
                            button(variant: ButtonVariant::Primary, attrs: attributes! { type="submit" }, "Continue")
                        )
                    </form>
                )
            }
        </main>
    })
}

#[component]
async fn decided(outcome: Outcome) -> TopcoatResult<impl View> {
    let (title, text) = match outcome {
        Outcome::Approved => (
            "Access approved",
            "Go back to your agent. It can now work for you. Revoke the access from your sharing page.",
        ),
        Outcome::Denied => {
            ("Access denied", "Your agent gets no access. You can close this page.")
        }
        Outcome::Expired => (
            "This request expired",
            "It was already decided, or it is too old. Start again from your agent.",
        ),
        Outcome::Failed => (
            "The node refused the access",
            "Nothing was approved. Look at the node's log, then start again from your agent.",
        ),
    };
    let (mark, tone) = match outcome {
        Outcome::Approved => (CHECK, "bg-success/10 text-success"),
        Outcome::Denied => (CROSS, "bg-muted text-muted-foreground"),
        Outcome::Expired => (CLOCK, "bg-warning/10 text-warning"),
        Outcome::Failed => (ALERT, "bg-destructive/10 text-destructive"),
    };
    Ok(view! {
        app_header(section: "Personal agents", <a href="/">"Back to sharing"</a>)
        <main id="main" class="mx-auto flex max-w-2xl flex-col gap-8 px-4 py-10 sm:px-8">
            <div class=(if matches!(outcome, Outcome::Failed) {
                "flex flex-col gap-3 rounded-xl border border-destructive/40 bg-background p-7"
            } else {
                "flex flex-col gap-3 rounded-xl border border-border bg-background p-7"
            })>
                <span class=(format!("flex size-9 items-center justify-center rounded-full text-lg {tone}")) aria-hidden="true">icon(data: mark)</span>
                page_title(title: title, (text))
            </div>
        </main>
    })
}

#[cfg(test)]
mod tests {
    use super::Decision;

    #[test]
    fn a_button_value_reads_back_as_its_decision() {
        for decision in [Decision::Allow, Decision::Deny] {
            let value = serde_json::Value::from(decision.as_str());
            assert_eq!(serde_json::from_value::<Decision>(value).ok(), Some(decision));
        }
        assert!(serde_json::from_value::<Decision>("Allow".into()).is_err());
    }
}
