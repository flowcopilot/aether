//! Building blocks of the admin pages, composed from the registry components
//! in [`crate::components`]. The registry components are generic; these know
//! the page's vocabulary: a section header, a labelled field, a value to copy,
//! a numbered step, an endpoint key.

use topcoat::{
    Result,
    icon::{IconData, icon},
    view::{Child, View, attributes, component, svg::ViewBox, view},
};

use crate::components::{
    badge::{BadgeVariant, badge},
    button::{ButtonSize, ButtonVariant, button},
    label::label,
};

/// Wraps a 24-unit stroke icon body (the Lucide conventions) in its group.
macro_rules! stroke_icon {
    ($body:literal) => {
        IconData::unescaped_unchecked(
            ViewBox::new(0.0, 0.0, 24.0, 24.0),
            concat!(
                r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
                $body,
                "</g>"
            ),
        )
    };
}

pub const COPY: IconData = stroke_icon!(
    r#"<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#
);
pub const CHECK: IconData = stroke_icon!(r#"<path d="M20 6 9 17l-5-5"/>"#);
pub const CROSS: IconData =
    stroke_icon!(r#"<path d="M6 6l12 12"/><path d="M18 6 6 18"/>"#);
pub const CLOCK: IconData =
    stroke_icon!(r#"<circle cx="12" cy="12" r="8"/><path d="M12 8v4l3 2"/>"#);
pub const CHEVRON: IconData = stroke_icon!(r#"<path d="m9 6 6 6-6 6"/>"#);
pub const ALERT: IconData = stroke_icon!(
    r#"<circle cx="12" cy="12" r="9"/><path d="M12 8v4"/><path d="M12 16h.01"/>"#
);
pub const TERMINAL: IconData =
    stroke_icon!(r#"<path d="m4 17 6-6-6-6"/><path d="M12 19h8"/>"#);

/// The bar on top of every page: the product, the page's section, and the
/// child on the right (status, links).
#[component]
pub async fn app_header(
    #[into] section: String,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <header class="border-b border-border">
            <div class="mx-auto flex min-h-14 max-w-6xl flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2 sm:px-8">
                <a href="/" class="text-base font-semibold tracking-tight text-foreground no-underline">
                    "Aether"
                </a>
                <span class="text-border" aria-hidden="true">"/"</span>
                <span class="text-sm text-muted-foreground">(section)</span>
                <span class="flex-1"></span>
                <div class="flex flex-wrap items-center gap-x-5 gap-y-1 text-sm">(child)</div>
            </div>
        </header>
    })
}

/// A page's heading and its one-line purpose.
#[component]
pub async fn page_title(
    #[into] title: String,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <div class="flex flex-col gap-2">
            <h1 class="text-3xl font-semibold tracking-tight text-balance">(title)</h1>
            <div class="max-w-prose text-base text-pretty text-muted-foreground">(child)</div>
        </div>
    })
}

/// A dot and a short state, such as how the node is reachable.
#[component]
pub async fn status_dot(ok: bool, #[into] text: String) -> Result<impl View> {
    Ok(view! {
        <span class="inline-flex items-center gap-2 text-muted-foreground">
            <span
                class=(if ok { "size-2 rounded-full bg-success" } else { "size-2 rounded-full bg-warning" })
                aria-hidden="true"
            ></span>
            (text)
        </span>
    })
}

/// Whether the node with `endpoint` is online. `/admin.js` fills it in from
/// the hub; without the hub or a script it stays empty.
#[component]
pub async fn presence_dot(#[into] endpoint: String) -> Result<impl View> {
    Ok(view! {
        <span
            class="group inline-flex items-center gap-1.5 text-xs font-normal text-muted-foreground"
            data-presence=(endpoint)
            data-state="unknown"
        >
            <span
                class="size-2 rounded-full bg-muted-foreground/30 group-data-[state=online]:bg-success group-data-[state=unknown]:hidden"
                aria-hidden="true"
            ></span>
            <span data-presence-label=""></span>
        </span>
    })
}

/// A labelled form control: the label, the control (the child), and an
/// optional hint under it.
#[component]
pub async fn field(
    #[into] name: String,
    #[into] control_id: String,
    #[default]
    #[into]
    hint: String,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <div class="flex min-w-0 flex-col gap-1.5">
            label(attrs: attributes! { for=(control_id) }, (name))
            (child)
            if !hint.is_empty() {
                <p class="text-xs text-muted-foreground">(hint)</p>
            }
        </div>
    })
}

/// A button that copies `text` to the clipboard (see `/admin.js`). Without
/// script it does nothing, and the value next to it can still be selected.
#[component]
pub async fn copy_button(
    #[into] text: String,
    #[default(String::from("Copy"))]
    #[into]
    name: String,
) -> Result<impl View> {
    Ok(view! {
        button(
            variant: ButtonVariant::Outline,
            size: ButtonSize::Md,
            attrs: attributes! { type="button" data-copy-text=(text) class="shrink-0" },
            icon(data: COPY)
            <span data-copy-label="">(name)</span>
        )
    })
}

/// How a [`copy_field`] shows its value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyTone {
    /// A value to paste into a page.
    #[default]
    Plain,
    /// A command to paste into a terminal.
    Terminal,
}

/// A value to hand to someone else: shown read-only, with a copy button.
#[component]
pub async fn copy_field(
    #[into] id: String,
    #[into] name: String,
    #[into] value: String,
    #[default] tone: CopyTone,
) -> Result<impl View> {
    let (box_class, button_name) = match tone {
        CopyTone::Plain => (
            "h-9 min-w-0 flex-1 truncate rounded-lg border-0 bg-muted px-3 font-mono text-[13px] text-foreground",
            "Copy",
        ),
        CopyTone::Terminal => (
            "h-9 min-w-0 flex-1 truncate rounded-lg border-0 bg-foreground px-3 font-mono text-[13px] text-background",
            "Copy command",
        ),
    };
    Ok(view! {
        <div class="flex min-w-0 flex-col gap-1.5">
            <label for=(&id) class="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                if tone == CopyTone::Terminal {
                    icon(data: TERMINAL)
                }
                (name)
            </label>
            <div class="flex gap-2">
                <input id=(&id) class=(box_class) readonly="" spellcheck="false" value=(&value)>
                copy_button(text: value.clone(), name: button_name)
            </div>
        </div>
    })
}

/// One numbered step of a short sequence, where the number carries the
/// order the reader must follow.
#[component]
pub async fn step(
    number: u8,
    #[into] title: String,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <li class="grid grid-cols-[1.75rem_minmax(0,1fr)] gap-4">
            <span
                class="flex size-7 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground"
                aria-hidden="true"
            >
                (number)
            </span>
            <div class="flex min-w-0 flex-col gap-3">
                <h2 class="mt-0.5 text-lg font-semibold">(title)</h2>
                (child)
            </div>
        </li>
    })
}

/// The color of a state badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Success,
    Warning,
    Destructive,
    Muted,
}

impl Tone {
    /// The classes of a state badge in this tone.
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Self::Success => "border-success/40 text-success",
            Self::Warning => "border-warning/40 text-warning",
            Self::Destructive => "border-destructive/40 text-destructive",
            Self::Muted => "text-muted-foreground",
        }
    }
}

/// A state as a bordered badge: its word, in its tone.
#[component]
pub async fn state_badge(state: (&'static str, Tone)) -> Result<impl View> {
    let (word, tone) = state;
    Ok(view! {
        badge(variant: BadgeVariant::Outline, attrs: attributes! { class=(tone.class()) }, (word))
    })
}

/// A form that is one submit button: it posts the hidden `fields` to
/// `action`. The child is the button's label. `title` explains the form;
/// `aria_label` names the button where its label alone is unclear.
#[component]
pub async fn post_button(
    action: &'static str,
    fields: Vec<(&'static str, String)>,
    #[default] variant: ButtonVariant,
    #[default] size: ButtonSize,
    #[default]
    #[into]
    title: String,
    #[default]
    #[into]
    aria_label: String,
    #[default] disabled: bool,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action) title=((!title.is_empty()).then_some(&title))>
            for (name, value) in &fields {
                <input type="hidden" name=(*name) value=(value)>
            }
            button(
                variant: variant,
                size: size,
                attrs: attributes! {
                    type="submit"
                    aria-label=((!aria_label.is_empty()).then_some(&aria_label))
                    disabled=(disabled.then_some(""))
                },
                (child)
            )
        </form>
    })
}

/// Where an [`error_line`] sets its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorIcon {
    /// At the top, nudged down to the first line of text.
    #[default]
    Nudged,
    /// At the top, as it is.
    Top,
    /// In the middle of a one-line row.
    Centered,
}

/// A refusal under a form, in the destructive color, with its icon.
#[component]
pub async fn error_line(
    #[into] text: String,
    #[default] placement: ErrorIcon,
) -> Result<impl View> {
    Ok(view! {
        match placement {
            ErrorIcon::Nudged => {
                <p class="flex items-start gap-1.5 text-sm text-destructive" role="alert">
                    <span class="mt-0.5 shrink-0">icon(data: ALERT)</span>
                    (text)
                </p>
            }
            ErrorIcon::Top => {
                <p class="flex items-start gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (text)</p>
            }
            ErrorIcon::Centered => {
                <p class="flex items-center gap-1.5 text-sm text-destructive" role="alert">icon(data: ALERT) (text)</p>
            }
        }
    })
}

/// The heading of a page section. `id` is the target of its
/// `aria-labelledby`, when it has one.
#[component]
pub async fn section_heading(
    #[default]
    #[into]
    id: String,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <h2 id=((!id.is_empty()).then_some(&id)) class="text-xl font-semibold tracking-tight">(child)</h2>
    })
}

/// A count beside a tab or heading. `warning` marks changes that wait for
/// the operator; `aria_label` reads the count out where the number alone is
/// unclear.
#[component]
pub async fn count_badge(
    count: usize,
    #[default] warning: bool,
    #[default]
    #[into]
    aria_label: String,
) -> Result<impl View> {
    Ok(view! {
        <span
            class=(if warning {
                "flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-warning px-1.5 text-[11px] font-semibold text-primary-foreground"
            } else {
                "flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-semibold text-primary-foreground"
            })
            aria-label=((!aria_label.is_empty()).then_some(&aria_label))
        >(count)</span>
    })
}

/// A number of days in words: "1 day", "7 days".
#[must_use]
pub fn days_label(days: u64) -> String {
    if days == 1 { "1 day".to_owned() } else { format!("{days} days") }
}

/// The `days` select of an access form: `choices`, with `selected` chosen.
/// `id` names it for a label; `aria_label` names it where there is none.
#[component]
pub async fn days_select(
    choices: Vec<u64>,
    selected: u64,
    #[into] class: String,
    #[default]
    #[into]
    id: String,
    #[default]
    #[into]
    aria_label: String,
) -> Result<impl View> {
    Ok(view! {
        <select
            id=((!id.is_empty()).then_some(&id))
            name="days"
            aria-label=((!aria_label.is_empty()).then_some(&aria_label))
            class=(class)
        >
            for days in &choices {
                <option value=(days) selected=((*days == selected).then_some(""))>
                    (days_label(*days))
                </option>
            }
        </select>
    })
}

/// How long ago, in the largest whole unit.
#[must_use]
pub fn ago(seconds: i64) -> String {
    match seconds {
        ..60 => "just now".to_owned(),
        60..3_600 => format!("{} min ago", seconds / 60),
        3_600..86_400 => format!("{} h ago", seconds / 3_600),
        _ => match seconds / 86_400 {
            1 => "yesterday".to_owned(),
            days => format!("{days} days ago"),
        },
    }
}

/// `value` in groups of four characters, so a person can read it aloud and
/// compare it with the copy someone else holds.
#[must_use]
pub fn grouped(value: &str) -> String {
    value
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Characters on one line of a [`key_code`]: eight groups of four, so a
/// 64-character endpoint id reads as two even lines.
const KEY_LINE: usize = 32;

/// An endpoint id shown in groups of four, with a button that copies it
/// whole.
#[component]
pub async fn key_code(#[into] id: String, #[into] value: String) -> Result<impl View> {
    let lines = value
        .as_bytes()
        .chunks(KEY_LINE)
        .map(|line| grouped(&String::from_utf8_lossy(line)))
        .collect::<Vec<_>>();
    Ok(view! {
        <div class="flex flex-col gap-3 sm:flex-row sm:items-start">
            <code
                id=(id)
                class="block flex-1 rounded-xl border border-border bg-background px-4 py-3 font-mono text-[15px] leading-7 tracking-wide break-words [word-spacing:0.3em]"
            >
                for line in &lines {
                    <span class="block">(line)</span>
                }
            </code>
            copy_button(text: value.clone())
        </div>
    })
}

#[cfg(test)]
mod tests {
    use super::{ago, grouped};

    #[test]
    fn a_task_age_reads_in_its_largest_unit() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(125), "2 min ago");
        assert_eq!(ago(7_300), "2 h ago");
        assert_eq!(ago(90_000), "yesterday");
        assert_eq!(ago(3 * 86_400), "3 days ago");
    }

    #[test]
    fn a_key_reads_in_groups_of_four() {
        assert_eq!(grouped("0154315dad38"), "0154 315d ad38");
        assert_eq!(grouped("01543"), "0154 3");
        assert_eq!(grouped(""), "");
    }
}
