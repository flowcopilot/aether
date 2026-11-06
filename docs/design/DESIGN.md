---
name: Aether Documentation
description: Audioflow-inspired color and typography for Aether documentation.
colors:
  dark-accent-low: "#2a1a46"
  dark-accent: "#9a6fd6"
  dark-accent-high: "#e3d6f7"
  dark-white: "#ffffff"
  dark-gray-1: "#ece9f5"
  dark-gray-2: "#c9c3dc"
  dark-gray-3: "#9890b3"
  dark-gray-4: "#5f5880"
  dark-gray-5: "#3b3457"
  dark-gray-6: "#261f3d"
  dark-black: "#19132b"
  light-accent-low: "#e7defa"
  light-accent: "#5b2e91"
  light-accent-high: "#2e1452"
  light-white: "#1d1638"
  light-gray-1: "#2b2447"
  light-gray-2: "#463e66"
  light-gray-3: "#6b6585"
  light-gray-4: "#9e98b5"
  light-gray-5: "#d9d4ea"
  light-gray-6: "#ebe8f5"
  light-gray-7: "#f6f4fc"
  light-black: "#fbfaff"
typography:
  display:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "clamp(2.5rem, 5vw, 3.5rem)"
    lineHeight: 1.1
    letterSpacing: "-0.025em"
  headline:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "2.1875rem"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "-0.025em"
  route-title:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: 650
    lineHeight: 1.25
    letterSpacing: "-0.02em"
  body:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "1.0625rem"
    lineHeight: 1.75
  label:
    fontFamily: "Bricolage Grotesque Variable, system-ui, sans-serif"
    fontSize: "0.875rem"
  code:
    fontFamily: "IBM Plex Mono, ui-monospace, monospace"
    fontSize: "0.875rem"
  inline-code:
    fontFamily: "IBM Plex Mono, ui-monospace, monospace"
    fontSize: "0.8125rem"
rounded:
  navigation: "0.25rem"
  control: "0.5rem"
spacing:
  compact: "0.5rem"
  route-gap: "0.75rem"
  standard: "1rem"
  panel: "1.5rem"
  route-block: "1.75rem"
  route-column: "2rem"
  route-after: "3rem"
components:
  route-row:
    typography: "{typography.route-title}"
    padding: "1.75rem 0"
  sidebar-link:
    typography: "{typography.label}"
    rounded: "{rounded.navigation}"
    padding: "0.3em 0.5rem"
  search-trigger:
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    height: "2.5rem"
  pagination:
    rounded: "{rounded.control}"
    padding: "1rem"
  inline-code:
    typography: "{typography.inline-code}"
    padding: "0.125rem 0.375rem"
---

# Design System: Aether Documentation

## Overview

**Creative North Star: "The Inviting Reference Library"**

Aether documentation borrows Audioflow’s lavender and plum palette and self-hosted typography. Expressive headings, readable prose, fine separators, and familiar Starlight navigation make technical material inviting.

This system applies only to the Astro documentation in `docs/`. The root `DESIGN.md` continues to govern the node and hub. Plain language and clear human and agent entry routes remain product commitments.

**Key Characteristics:**

- Lavender and plum in paired light and dark themes.
- Expressive sans serif with a precise monospace companion.
- A stable reading column between navigation and local contents.
- Human and agent route rows divided by fine rules.

## Colors

Light mode pairs lavender paper with plum ink; dark mode pairs deep plum surfaces with pale lavender accents. The frontmatter contains exact source values. Its theme prefixes identify variants of Starlight color names, not simultaneous assignments.

### Primary

Accent supplies focus outlines. Starlight assigns accent to links and active navigation in light mode, and accent-high to those roles in dark mode. Selection uses accent-low behind accent-high text. Route-title hover uses accent-high in both modes.

### Neutral

Starlight’s black token is the page background and white is the strongest heading color; their visual lightness reverses with the theme. Gray 2 carries body text, Gray 3 secondary navigation, Gray 5 control borders, and Gray 6 hairlines. The header uses Gray 7 in light mode and Gray 6 in dark mode. Inline code uses Gray 6 in light mode and Gray 5 in dark mode.

**The Semantic Theme Rule.** Reference Starlight’s semantic color properties instead of binding components to one theme’s hex values.

## Typography

Bricolage Grotesque Variable carries headings and prose, with system-ui and sans-serif fallbacks. IBM Plex Mono carries technical text, with ui-monospace and monospace fallbacks. Fontsource files are bundled and served from the docs origin; mono includes weights 400 and 600.

The frontmatter headline role describes desktop h2. Below Starlight’s 50em breakpoint, h2 is 1.8125rem; h3 moves from 1.5rem to 1.8125rem at that breakpoint. H1 is explicitly 2.5rem through 40rem. H1–h3 use balanced wrapping and `font-stretch: 88%`. Paragraphs have a maximum measure of 72ch. Route descriptions use 1rem, tables 0.9375rem. The site name uses weight 750 and letter spacing -0.02em. These choices supplement Starlight’s inherited type scale.

## Layout

The desktop reading shell has a top bar, left sidebar, central document, and right local table of contents. Search, GitHub, and the theme selector occupy the header. Sidebar groups are **Start here** and **Guides**; developer and protocol material remain available there.

Content width is 48rem. Starlight’s sidebar is 18.75rem; content gutters are 1rem, increasing to 1.5rem at 72rem. Desktop sidebar and expanded search start at 50rem; the right local table of contents starts at 72rem. Smaller screens use Starlight’s menu and local-contents disclosures.

Route rows use two columns (`1fr 1.2fr`), a 0.75rem row gap, 2rem column gap, and 1.75rem vertical padding. The route group has 1.75rem space above and 3rem below. Through 40rem, rows stack into one column with a 0.75rem gap. Fine horizontal rules divide the group and adjacent rows.

## Elevation & Depth

The reading surface and route rows are flat. Tonal changes and fine separators distinguish navigation from the document. Starlight retains soft elevation for menu controls, pagination, and the search dialog; the docs override introduces no new shadow system. The sidecar records the inherited shadow vocabulary.

## Shapes

Reading regions and route rows use straight separators without enclosing cards. Starlight uses the frontmatter navigation radius for sidebar links and shortcut keys, and the control radius for search and pagination.

## Components

### Search

Retain Starlight’s search trigger and Pagefind dialog. Desktop shows a bordered, rounded field-like button with its shortcut; mobile shows the search icon. The trigger and Ctrl/Cmd+K open search. The dialog is centered from 50rem and fills the screen below it. Its input retains Pagefind geometry and theme-aware colors.

### Navigation

Sidebar groups are native disclosures. Links use muted text; hover and focus increase emphasis. The current page has a text-accent background and inverted text. Retain the local table of contents and native Starlight mobile navigation.

### Human and agent routes

Each row is one semantic link containing a title and explanation. The whole row is actionable. Hover changes the title to accent-high and adds an underline. Preserve the raw `agent.md` reference in the agent route. Rows stack at the documented mobile breakpoint.

### Code and pagination

Inline code uses a quiet tonal background and IBM Plex Mono. Larger examples retain Starlight’s fenced-code treatment. Previous and next links use inherited rounded containers, borders, and a soft shadow, with the page title emphasized.

### Focus and motion

Keyboard focus uses a 2px accent outline with 4px offset. Prose links have a 0.2em underline offset. Sidebar carets use Starlight’s 0.2s ease-in-out transform transition. The docs reduced motion rule disables animation and transitions and restores automatic scrolling.

## Do's and Don'ts

### Do:

- Do apply this palette and type only to the Astro documentation.
- Do use semantic Starlight color properties so both themes remain legible.
- Do preserve search, sidebar navigation, local contents, and visible keyboard focus.
- Do preserve human and agent setup routes, including the raw agent Markdown entry.
- Do respect the existing reduced motion override.

### Don't:

- Don’t change the node or hub identity from this scoped record.
- Don’t turn the route rows into decorative cards or add competing custom navigation.
- Don’t replace self-hosted fonts with an external font stylesheet.

<!-- Evidence: docs/astro.config.mjs; docs/src/styles/docs.css; docs/index.md;
     target/docs-site; installed Starlight styles; desktop/dark/mobile captures
     at /tmp/aether-docs-{desktop,dark,mobile}.png. -->
