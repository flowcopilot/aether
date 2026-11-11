use std::path::Path;

use topcoat::{
    Result as TopcoatResult,
    view::{Child, View, attributes, component, view},
};

use crate::{
    admin_components::{ErrorIcon, error_line, field},
    components::{
        alert::{alert, alert_description},
        button::{ButtonSize, ButtonVariant, button},
        input::input,
    },
};

/// What the last form asks the host or caller screen to show. Each form sets
/// at most one; an empty text shows nothing.
#[derive(Clone, Default)]
pub(super) enum Notice {
    #[default]
    Nothing,
    /// The host's code invite was refused.
    Invite(String),
    Workspace(String),
    Relay(String),
    /// The grant id whose revoke waits for a second click.
    ConfirmRevoke(String),
    /// A hub form of either screen failed.
    Hub(String),
    /// The hub URL was refused.
    HubSetting(String),
    Group(String),
    Card(String),
    CardSaved,
    Listing(String),
    /// The caller's pasted invite was refused.
    Import(String),
    /// A prepared access request could not be sent.
    Request(String),
}

/// One accessor per text variant: its text, or empty for any other notice.
macro_rules! notice_texts {
    ($($name:ident => $variant:ident),* $(,)?) => {
        $(
            pub(super) fn $name(&self) -> String {
                match self {
                    Self::$variant(text) => text.clone(),
                    _ => String::new(),
                }
            }
        )*
    };
}

impl Notice {
    notice_texts! {
        invite_error => Invite,
        workspace_error => Workspace,
        relay_error => Relay,
        confirm_revoke => ConfirmRevoke,
        hub_error => Hub,
        hub_setting_error => HubSetting,
        group_error => Group,
        card_error => Card,
        listing_error => Listing,
        import_error => Import,
        request_error => Request,
    }

    pub(super) const fn card_saved(&self) -> bool {
        matches!(self, Self::CardSaved)
    }

    /// Whether the screen answers `400 Bad Request`: a form refused its
    /// input. A hub error, which is often the hub's own answer, and a hub URL
    /// that was refused still answer `200`, as they always have.
    pub(super) const fn refused(&self) -> bool {
        match self {
            Self::Invite(text)
            | Self::Workspace(text)
            | Self::Relay(text)
            | Self::Group(text)
            | Self::Card(text)
            | Self::Listing(text)
            | Self::Import(text)
            | Self::Request(text) => !text.is_empty(),
            Self::Nothing
            | Self::ConfirmRevoke(_)
            | Self::Hub(_)
            | Self::HubSetting(_)
            | Self::CardSaved => false,
        }
    }
}

/// A form whose one field names what a button acts on.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdForm {
    pub(super) id: String,
}

pub(super) fn request_drafts_of(
    data_dir: &Path,
    now: u64,
) -> Vec<aether_store::AccessRequestDraft> {
    aether_store::NodeStore::in_dir(data_dir)
        .and_then(|store| {
            store.access_request_drafts(i64::try_from(now).unwrap_or(i64::MAX))
        })
        .unwrap_or_default()
}

pub(super) fn queue_text(data_dir: &Path) -> String {
    match crate::policy::queue_limits(data_dir) {
        Ok(Some(limits)) => {
            let tasks = if limits.global == 1 {
                "1 waiting task".to_owned()
            } else {
                format!("{} waiting tasks", limits.global)
            };
            format!(
                "Your agent keeps {tasks} in all and {} for each person. People who get access by request share these places.",
                limits.per_peer
            )
        }
        Ok(None) => "This node has no task policy yet. Start `aether node serve` once."
            .to_owned(),
        Err(_) => "The task limits of this node cannot be read.".to_owned(),
    }
}

#[component]
pub(super) async fn relay_notice() -> TopcoatResult<impl View> {
    Ok(view! {
        alert(
            alert_description(
                "Only your local network can reach this agent. "
                <a href="#relay">"Set a relay"</a>
                " before you invite someone elsewhere."
            )
        )
    })
}

/// A row of the Connection card: what the node uses now (the child), a link
/// that opens the form, and the form that changes the URL. The form is open
/// while no URL is set (`missing`) or when it refused the last input.
#[component]
pub(super) async fn url_setting(
    /// The row's anchor, such as `relay`.
    id: &'static str,
    action: &'static str,
    /// The label of the URL field.
    name: &'static str,
    /// The id and form name of the URL field.
    control: &'static str,
    #[into] hint: String,
    placeholder: &'static str,
    value: String,
    error: String,
    missing: bool,
    /// The text of the link that opens the form.
    link: &'static str,
    /// The text of the submit button.
    submit: &'static str,
    #[default] child: Child<'_>,
) -> TopcoatResult<impl View> {
    let open = missing || !error.is_empty();
    Ok(view! {
        <div id=(id) class="py-3">
            <details class="group" open=(open.then_some(""))>
                <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
                    <span class="flex min-w-0 flex-col gap-0.5">(child)</span>
                    <span class="shrink-0 text-[13px] font-medium text-primary underline underline-offset-4">(link)</span>
                </summary>
                <form method="post" action=(action) class="mt-3 flex flex-col gap-2">
                    field(
                        name: name,
                        control_id: control,
                        hint: hint,
                        input(attrs: attributes! {
                            id=(control) name=(control) type="url" value=(&value)
                            placeholder=(placeholder) class="font-mono"
                            spellcheck="false" autocomplete="off"
                            aria-invalid=((!error.is_empty()).then_some("true"))
                        })
                    )
                    if !error.is_empty() {
                        error_line(text: error.clone(), placement: ErrorIcon::Centered)
                    }
                    button(variant: ButtonVariant::Outline, size: ButtonSize::Sm, attrs: attributes! { type="submit" class="w-fit" }, (submit))
                </form>
            </details>
        </div>
    })
}
