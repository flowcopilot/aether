---
name: Aether
description: Share your personal AI agent with people you trust.
colors:
  canvas: "oklch(0.955 0.005 165)"
  background: "oklch(0.99 0.002 165)"
  foreground: "oklch(0.22 0.008 165)"
  muted: "oklch(0.925 0.007 165)"
  muted-foreground: "oklch(0.46 0.012 165)"
  primary: "oklch(0.43 0.07 185)"
  primary-foreground: "oklch(0.99 0.004 185)"
  success: "oklch(0.46 0.09 160)"
  warning: "oklch(0.47 0.09 80)"
  destructive: "oklch(0.48 0.15 30)"
  destructive-foreground: "oklch(0.99 0.012 30)"
  border: "oklch(0.88 0.008 165)"
  ring: "oklch(0.43 0.07 185)"
  dark-canvas: "oklch(0.16 0.008 165)"
  dark-background: "oklch(0.2 0.009 165)"
  dark-foreground: "oklch(0.94 0.005 165)"
  dark-muted: "oklch(0.27 0.01 165)"
  dark-muted-foreground: "oklch(0.72 0.012 165)"
  dark-primary: "oklch(0.78 0.08 185)"
  dark-border: "oklch(0.32 0.01 165)"
  hub-sky: "#030508"
  hub-ink: "#d6e2ee"
  spectrum-cyan: "#5ad1ff"
  spectrum-violet: "#9b7bff"
  spectrum-pink: "#ff6bd5"
  spectrum-amber: "#ffb86b"
  spectrum-green: "#7effa8"
typography:
  headline:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "30px"
    fontWeight: 600
    lineHeight: "36px"
    letterSpacing: "-0.025em"
  title:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "18px"
    fontWeight: 600
    lineHeight: "28px"
  body:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: "20px"
    fontFeature: "tnum"
  lead:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: "24px"
  section:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: "28px"
    letterSpacing: "-0.025em"
  card-title:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "15px"
    fontWeight: 600
    lineHeight: "20px"
  caption:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "20px"
  count:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "11px"
    fontWeight: 600
    lineHeight: "18px"
  label:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: "16px"
  code:
    fontFamily: "ui-monospace, SF Mono, Menlo, Consolas, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "20px"
  key:
    fontFamily: "ui-monospace, SF Mono, Menlo, Consolas, monospace"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "28px"
    letterSpacing: "0.025em"
rounded:
  md: "6px"
  lg: "8px"
  xl: "12px"
  full: "9999px"
spacing:
  "1": "4px"
  "1.5": "6px"
  "2": "8px"
  "3": "12px"
  "4": "16px"
  "5": "20px"
  "6": "24px"
  "8": "32px"
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.lg}"
    padding: "0 16px"
    height: "36px"
    typography: "{typography.body}"
  button-outline:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.lg}"
    padding: "0 16px"
    height: "36px"
  button-destructive:
    backgroundColor: "{colors.destructive}"
    textColor: "{colors.destructive-foreground}"
    rounded: "{rounded.lg}"
    padding: "0 16px"
    height: "36px"
  input:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.lg}"
    padding: "0 12px"
    height: "36px"
  copy-field:
    backgroundColor: "{colors.muted}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.lg}"
    padding: "0 12px"
    height: "36px"
    typography: "{typography.code}"
  copy-field-terminal:
    backgroundColor: "{colors.foreground}"
    textColor: "{colors.background}"
    rounded: "{rounded.lg}"
    padding: "0 12px"
    height: "36px"
    typography: "{typography.code}"
  card:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.xl}"
    padding: "24px"
  badge:
    textColor: "{colors.foreground}"
    rounded: "{rounded.md}"
    padding: "2px 8px"
    typography: "{typography.label}"
  step-number:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.full}"
    size: "28px"
---

# Design System: Aether

## Overview

**Creative North Star: "Signal in the Dark"**

Everything is quiet and still until a signed permission makes a link. The system reserves its colour and energy for that moment: a person who can use your agent, an invite that has just been made, an agent on the hub that is available now. Everything else stays neutral so that the link is what you see.

Aether has two registers. **The node UI** is the base system: a light, cool-neutral tool served on loopback, with system typefaces, hairline borders, small shadows, and one deep teal accent. It is plain and exact; nothing competes with the person and the action. **The hub page** is the public register: it shows the agents in the network as a living map, each agent drawn as a sigil from its key. The idea of the map is a commitment. Its current style (a dark night sky, an italic serif headline, a mono face) is one execution and is not mandatory.

The node UI is light today. Its dark tokens are part of the system but nothing turns them on yet.

**Key Characteristics:**

- Neutral surfaces; colour marks a status, the main action, or (on the hub) an available agent.
- One accent (deep teal) for the main action, links, focus, and step numbers.
- System typefaces only in the node UI, so it works offline.
- Hairline borders and low shadows; surfaces are close in value.
- Keys are shown in mono, in groups of four, so a person can read them aloud.
- Agents are drawn as sigils made from their 32-byte key.

## Colors

A cool neutral with a slight green cast, one deep teal accent, and a spectrum kept for permitted connections.

### Primary

- **Deep Teal** (`primary`): the main action on a screen (Create invite, Connect), links, the focus ring, the numbered step circles. One filled teal button for each panel at most. In dark mode it lifts to `dark-primary`.

### Tertiary

- **The Spectrum** (`spectrum-cyan` → `spectrum-violet` → `spectrum-pink` → `spectrum-amber` → `spectrum-green`): the colour of a connection. On the hub it lights the sigils of agents that are available now and the hub mark. It never decorates a surface, a heading, or a button. On a light ground, use deeper versions of the same hues so that the arcs stay visible.

### Neutral

- **Canvas** (`canvas`): the page ground behind raised surfaces.
- **Paper** (`background`): cards, panels, inputs, and outline buttons.
- **Ink** (`foreground`): text and icons. Also the ground of the terminal copy field, inverted.
- **Mist** (`muted`): quiet fills: code chips, the plain copy field.
- **Slate** (`muted-foreground`): secondary text, captions, hints, placeholders, column heads.
- **Hairline** (`border`): borders and row rules.
- **Night Sky** (`hub-sky`) and **Starlight** (`hub-ink`): the ground and text of the current hub page.

### Semantic

- **Success** (`success`): a finished state: an answered task, a reachable relay.
- **Warning** (`warning`): a state that needs time: a running task, an unreachable relay.
- **Destructive** (`destructive`): revoke and errors. A filled red button appears only at the moment of confirmation.

### Named Rules

**The Derived States Rule.** Hover and press have no tokens of their own. Derive them from the fill or the foreground at reduced opacity (`primary` at 90% and 80%; `foreground` at 5%, 10%, and 15%), so that both colour schemes work without overrides.

**The One Accent Rule.** Teal is the only accent in the node UI. Do not add a second brand colour.

**The Status Colour Rule.** In the node UI, colour means a status: success for answered or reachable, warning for running or unreachable, destructive for revoke and errors. People are initials in tinted circles. The spectrum and the sigils stay on the hub's network map. (Decided 2026-10-22: variant A on the "Colour rule — choose" page of the Aether Paper file. Variant B, which drew people as spectrum sigils and made statuses grey, was rejected.)

## Typography

**Body Font:** the system sans stack (`ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto`)
**Mono Font:** the system mono stack (`ui-monospace, SF Mono, Menlo, Consolas`)

**Character:** the platform's own typeface, set tight at headline size and with tabular figures everywhere. It reads as a well-made system tool, not as a brand.

### Hierarchy

- **Headline** (600, 30px, 36px, tight tracking, balanced wrap): one page title, such as "Who can use your agent".
- **Section** (600, 20px, tight tracking): a section inside a page, such as "Public listing" or "Sent tasks".
- **Title** (600, 18px): numbered step titles and the main card of an aside ("Invite someone", "Access requests").
- **Card title** (600, 15px): the titles of the smaller aside cards ("Connection", "Owner", "Your service agents") and of access groups.
- **Lead** (400, 16px, pretty wrap, max one prose measure): the one-line purpose under a page title, in `muted-foreground`.
- **Body** (400, 14px): rows, controls, alerts. Tabular figures are on for the whole body.
- **Caption** (400, 13px): secondary lines in cards and rows: a row's label in the Connection card, an agent's description, a notice under a card title.
- **Label** (500, 12px): field labels on copy fields, badges, hints.
- **Count** (600, 11px): the number in a tab or card count badge, on a full pill.
- **Code** (mono, 13px): invite strings, commands, paths.
- **Key** (mono, 15px, 28px line height, wide word spacing): endpoint keys, two lines of eight groups of four.

### Named Rules

**The Offline Type Rule.** The node UI uses only system typefaces. It is served on loopback and must work offline, so it loads no web font. (The Paper mock-ups use IBM Plex Sans and Plex Mono; they are a close stand-in, not the shipped stack.)

**The Read-Aloud Rule.** Show a key or a code in groups of four characters, so that two people can compare it by voice.

## Layout

The node UI has one column, centred, with a maximum width of 1152px (`max-w-6xl`) and 16px side padding (32px from the small breakpoint up). The header is a single bar, at least 56px high, with a hairline under it: the product name, a slash, the section, a flexible space, then status and links. The host screen puts the people table on the left and the "Invite someone" panel on the right. The caller screen is one narrow column of numbered steps.

Spacing uses the 4px Tailwind scale. Related items sit 6–8px apart (a label and its control), groups 12–20px apart (fields in a form, sections in a card), and the page sections 24–32px apart. Tables scroll sideways in their own container instead of widening the page. Controls in a row share one height (36px), so an input and a button sit flush.

The current hub page is a full-viewport stage: the map fills the screen, and the headline, counts, and search sit at the bottom centre. Below 620px the headline shrinks and the legend is hidden.

## Elevation & Depth

The system is close to flat. Depth comes mostly from the step between `canvas` and `background`, and from hairline borders. Two small shadows support this: one for controls and one for raised surfaces. In dark mode the shadows become darker and denser, and the surfaces separate by value.

### Shadow Vocabulary

- **Control** (`0 1px 2px oklch(0.25 0.02 165 / 10%)`): buttons with a fill or border, and inputs. A ghost button has none.
- **Raised** (`0 1px 2px oklch(0.25 0.02 165 / 7%), 0 8px 24px -6px oklch(0.25 0.02 165 / 12%)`): cards and panels.

### Named Rules

**The Hairline First Rule.** Separate with a 1px `border` line or a value step before you add a shadow. Use only the two shadows above.

## Shapes

The corners are soft but not round: 6px on badges and small buttons, 8px on buttons, inputs, and copy fields, 12px on cards, panels, and the key block. Circles are only for things that are a point or a person: avatars, step numbers, status dots, and sigils. Every button and badge has a border, even a transparent one, so that the outline variant has the same size as the others.

**The Sigil.** An agent's face is drawn from its 32-byte endpoint key: a hairline ring, 64 ticks from the nibbles, and 16 arcs in four lanes from the four-hex groups. The same key always gives the same sigil. It is grey when the agent is alone or offline, and in the spectrum when it is connected or available.

## Components

### Buttons

Plain and exact; the label says the action.

- **Shape:** gently rounded (8px; 6px for the small size).
- **Primary:** deep teal fill, near-white text, 36px high, 16px side padding, control shadow. Use it for the one main action.
- **Outline:** paper fill, hairline border, ink text. Use it for Copy, Revoke at rest, and secondary actions.
- **Secondary and Ghost:** an ink tint at 5% (secondary) or no fill until hover (ghost).
- **Destructive:** red fill, used only for the confirming click of a destructive action.
- **Hover / Press:** the fill at 90% and 80%, or an ink tint of 5% and 10%. **Focus:** a 2px `ring` with a 2px offset. **Disabled:** 50% opacity, no pointer events.

### Badges

- **Style:** 12px medium text in a 6px-rounded pill with 2px × 8px padding. Variants: primary fill, ink tint, outline, destructive.
- **Use:** the state of the last task (Answered, Running) and short tags.

### Cards / Containers

- **Corner Style:** 12px.
- **Background:** `background` on the `canvas` ground.
- **Shadow Strategy:** the Raised shadow.
- **Border:** a hairline.
- **Internal Padding:** 24px at the sides and 24px at the top and bottom. Sections stack with a gap of 20px.

### Inputs / Fields

- **Style:** paper fill, hairline border, 8px radius, 36px high, 12px side padding, control shadow, `muted-foreground` placeholder.
- **Focus:** a 2px `ring` with a 2px offset.
- **Field:** a label (14px medium) above the control with a 6px gap, and an optional 12px hint under it.

### Copy Field

The way to hand a value to another person: a read-only value and an outline Copy button on one line. The plain tone is a `muted` box with the value in mono. The terminal tone is an inverted ink box, with a terminal icon on its label. After a copy, the button label changes to "Copied" for 1.6 seconds. Without script, the value can still be selected by hand.

### Key Code

An endpoint key in mono, in two lines of eight groups of four, in a 12px-rounded block with a hairline border, and a Copy button beside it.

### Numbered Step

A 28px teal circle with the number, then a title and its content. Use it only where the order matters (the caller's connect flow).

### Navigation

The header bar is the only navigation: "Aether" in 16px semibold, then one tab for each screen of the node's mode ("Who can use your agent", "Agents you use", "Advanced"). The current tab has a 2px teal line under it and ink text; the others are slate. A tab carries a count pill when something waits on it: teal for access requests to confirm, warning for changes to confirm. On the right: how the node is reached (an 8px dot and short text) and the node's name on Aether in a small mono pill. Pages that are not tabs (PACT consent) keep the plain "Aether / section" bar. Links in text are teal and underlined with a 3px offset; a link styled as a button has no underline.

### Page structure

The host page has two columns on wide screens: the people and the public listing on the left, and an aside of cards on the right ("Invite someone" or "Access requests", then "Owner" or "Your service agents", then "Connection"). The relay and the hub are rows of the **Connection** card: a caption, the value in mono, and a teal "Change" link that opens the form in place. Below a hairline, the agent's card and the access groups sit side by side. The "Agents you use" page is one column in this order: requests to confirm, your name and invites, agents you can use, the MCP commands, sent tasks, then "Connect another agent". Advanced has a section list on the left on wide screens.

### Refusals

A refusal under a form is destructive text with an alert icon, and the field gets a destructive border (`aria-invalid`). A result after an action (card saved, invite created) is a success-tinted box with a check; a saved setting that needs a restart is a warning-tinted box.

### Status and Presence Dots

An 8px circle and a short label. Success colour when the state is good, warning colour when it is not. A presence dot is hidden until the hub reports a state.

### Network Map (hub)

The hub page shows the listed service agents as sigils on a slow orbit, with their name and number under them. Available agents use the spectrum and a slow pulse; offline agents are grey and dim. Search dims the agents that do not match. Hover or tap shows a card with the agent, its state, its description, its tags, its owner, and "ask for access to `name#0000`". A hidden list repeats the same agents for screen readers. With reduced motion, the map does not move.

## Do's and Don'ts

### Do:

- **Do** use only the tokens in `crates/aether/styles.css`; components refer to tokens, never to raw colours.
- **Do** keep one filled teal button for each panel.
- **Do** derive hover and press states from opacity, as in the Derived States Rule.
- **Do** show keys and codes in mono, in groups of four.
- **Do** give every value that a person must pass on a Copy button and a selectable fallback.
- **Do** draw an agent from its key, and keep the same sigil for the same key.
- **Do** stop all motion when `prefers-reduced-motion` is set.

### Don't:

- **Don't** load a web font or an external asset in the node UI.
- **Don't** add a second accent colour to the node UI.
- **Don't** use the spectrum as decoration on surfaces, headings, or buttons.
- **Don't** use a filled red button before the confirming step of a destructive action.
- **Don't** add shadows other than Control and Raised.
- **Don't** set `role="alert"` on a notice that renders with the page.
