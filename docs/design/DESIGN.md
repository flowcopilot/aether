# Design: Aether documentation

The documentation uses the design system of the Aether node UI and hub. The
root [`DESIGN.md`](../../DESIGN.md) governs it; this file records only how
those rules map onto Starlight. The token values in
`docs/src/styles/docs.css` are copied from `crates/aether/styles.css`. Change
them there first, then here.

## Tokens

Starlight puts its dark scheme on `:root` and its light scheme on
`[data-theme='light']`. The node UI's `.dark` values go on `:root` and its
light values on `[data-theme='light']`. Starlight's variables refer to the
tokens:

| Starlight | Aether token |
| --- | --- |
| `--sl-color-bg`, `-bg-nav`, `-bg-sidebar` | `canvas` |
| `--sl-color-white`, `-gray-1`, `-text` | `foreground` |
| `--sl-color-gray-3` | `muted-foreground` |
| `--sl-color-gray-5`, `-hairline*` | `border` |
| `--sl-color-gray-6`, `-bg-inline-code` | `muted` |
| `--sl-color-accent*`, `-text-accent` | `primary` |
| `--sl-color-text-invert` | `primary-foreground` |
| blue, purple and green, orange, red | `primary`, `success`, `warning`, `destructive` |
| `--sl-shadow-sm`, `-md` and `-lg` | Control, Raised |

## Type

The system sans and mono stacks only; no web font is loaded. The page title
uses the Headline role (30px, 600, tight tracking), h2 the Section role (20px),
h3 the Title role (18px), and h4 the Card title role (15px). Prose is set at the
16px Lead size, since documentation is read at length. Sidebar links are 14px
Body, group labels 12px Label, and code 13px Code. Tabular figures are on for
the whole page.

## Components

- **Header:** "Aether" in 16px semibold, led by the hub mark (a ring around the
  spectrum disc). The favicon is the same mark on the hub's night sky.
- **Search:** an outline control: paper, hairline, 8px, Control shadow.
- **Sidebar:** the current page reads like the node UI's current tab: ink
  text, a 5% ink tint and a 2px teal line, not a filled block.
- **Router:** the two paths on the home page (for humans, for agents) are cards:
  paper on canvas, a hairline, 12px corners, the Raised shadow and 24px padding.
  Each card is one link. On hover the border tints teal, and the title turns
  teal and is underlined.
- **Code:** inline code is a `muted` chip with a hairline and 6px corners.
  Code blocks have 8px corners, a hairline, and no shadow.
- **Asides:** a box tinted 8% with its status colour, with a hairline and 8px
  corners, like the node UI's notices.
- **Links:** teal, underlined with a 3px offset.
- **Focus:** a 2px `ring` with a 2px offset.

The spectrum appears only in the hub mark, as in the node UI and the hub.
