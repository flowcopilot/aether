# Plan: Simplify the admin web UI

## Goal

Build the design approved on 25.09.2026 (canvas "Aether portal UX", host
screen and caller first-visit screen) into the admin web UI. Each mode shows
one screen with one job: `node serve --admin-port` shows who can use the
agent and how to invite someone; `node admin` shows how to connect to a
friend's agent. The caller UI does not send tasks: talking to an agent is the
job of agents (MCP, CLI), not of this page. An invite is one copyable string,
and the host can also copy it as a one-line CLI command.

## Approach

- **Invite string.** `aether-invite:` followed by the unpadded base64url of
  the `aether-invitation/v1` bundle JSON. `InvitationBundle::encode` and
  `InvitationBundle::decode` live in `direct.rs`; decode also accepts the raw
  JSON, so old bundles still import.
- **One import path.** `direct::import_bundle(data_dir, &bundle)` checks the
  caller endpoint, imports the invitation and stores the bundle as
  `caller.contact.<name>`. The UI and the CLI both call it.
- **CLI.** `node direct import <invitation>` takes a file (invitation JSON or
  bundle JSON) or an `aether-invite:` string. `node request --scope direct
  --agent-id <name>` takes the authorization and relay URL from the stored
  bundle when `--authorization` and `--address` are not given. `--data-dir`
  becomes optional for `node admin`, `node direct` and `node request`, with
  one default: `$AETHER_DATA_DIR`, else `~/.aether/node`. This lets the host
  copy one command without knowing the caller's paths.
- **No task sender in the UI.** Remove `/send`, "Sent tasks", `Sender`,
  `SendFuture`, `SentTask`, `SentState` and the sender in `CallerContext`.
- **Components.** Install topcoat registry components `table`, `badge`,
  `select`, `alert` and `avatar`. App-specific composites go in a new
  `admin_components.rs`: `page_header`, `copy_field` (read-only value plus a
  copy button), `step` (numbered step), `grouped_code` (a key in groups of
  four), `last_task` (badge for a task state and age). Theme tokens in
  `styles.css` take the canvas palette (cool neutral ground, teal primary).
  No web fonts: the page is loopback-only and must work offline.
- **Copy buttons.** A small script in the existing `/activity.js` route
  (renamed `/admin.js`) copies `data-copy` targets with the Clipboard API.
  The value stays in a read-only input, so it can also be selected by hand.
- **Host screen.** Header with relay status. Title "Who can use your agent",
  the workspace path with a disclosure form to change it, a people table
  (person, last task, access ends, revoke) and an "Invite someone" panel
  (connect code, name, 1/7/30 days). Revoke is two-step without script:
  `POST /revoke` without `confirm` re-renders the row as a confirmation;
  with `confirm=yes` it revokes. A notice links to Advanced when changes wait
  for confirmation. Last task per person comes from a new store query over
  `tasks` (state and `created_at` by `caller_endpoint_id`, direct scope);
  the task text is not shown.
- **Caller screen.** Title "Connect to a friend's agent", step 1 the connect
  code in groups of four with copy, step 2 paste the invite. When contacts
  exist, "Agents you can use" lists them with expiry and the CLI command to
  send a task.
- **Advanced page.** `/advanced` gets the counts, tasks cancel, activity,
  managed members and pending changes that the home page shows today.
  `/confirm` and `/cancel` re-render it.

Out of scope: a CLI for the connect code (`node endpoint` already prints
it), MCP changes, dark-mode toggle.

## Implementation Phases

### Phase 1: One-line invite and CLI

- `InvitationBundle::encode`/`decode`, `direct::import_bundle`,
  `direct::contact_bundle`; `base64` as a direct dependency
- `node direct import` accepts a file or an invite string; `node request`
  uses the stored bundle; shared default data directory
- Tests: encode/decode round trip and raw JSON; import refuses another
  endpoint; command test: import an invite string, then request without
  `--authorization`
  **Commit:** `feat(direct): import a one-line invite and send with the stored authorization`

### Phase 2: Remove the task sender from the web UI

- Remove `/send`, sent tasks and the sender types; the UI import uses
  `import_bundle` and accepts the invite string
- Tests updated: caller imports an invite string
  **Commit:** `refactor(admin-ui): leave sending tasks to agents and the CLI`

### Phase 3: Theme and reusable components

- `topcoat ui add table badge select alert avatar`; theme tokens
- `admin_components.rs` with `page_header`, `copy_field`, `step`,
  `grouped_code`, `last_task`; copy script
  **Commit:** `feat(admin-ui): add the theme and reusable components for the new screens`

### Phase 4: Host screen and Advanced page

- Store query for the last task per caller; `Administration::last_tasks`
- Host screen, two-step revoke, `/advanced`
- Tests: two-step revoke; invite shows the string and the command; the
  advanced page confirms a change; home links to pending changes
  **Commit:** `feat(admin-ui): show who can use the agent on one screen`

### Phase 5: Caller screen

- First-visit steps, grouped code, agents list with the CLI command
- Tests: first visit shows the grouped code; after import the agent is listed
  **Commit:** `feat(admin-ui): connect to a friend's agent in two steps`

### Phase 6: Documentation

- README "Set up with the web UI" updated
  **Commit:** `docs(readme): describe the simplified web UI and the one-line invite`

## Risks & Tradeoffs

- Changing the `node admin` default data directory from `./.aether/node` to
  `~/.aether/node` changes where a bare `node admin` stores data. The README
  always passes `--data-dir`, and a fixed home path is what makes the copied
  command work.
- The invite string is about 2 KB; the UI body limit is 8 KB.
- The Clipboard API needs a secure context; the UI is HTTPS on loopback.
