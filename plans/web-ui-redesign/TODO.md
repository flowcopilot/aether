# TODO: Simplify the admin web UI

## Phase 1: One-line invite and CLI

- [x] `InvitationBundle::encode`/`decode`, `direct::import_bundle`, `direct::contact_bundle`
- [x] `node direct import` accepts a file or an invite string
- [x] `node request` uses the stored bundle without `--authorization`/`--address`
- [x] Shared default data directory (`$AETHER_DATA_DIR`, else `~/.aether/node`)
- [x] Tests: round trip, raw JSON, other endpoint refused, command test
- [x] `mise run check` passes
- [x] Commit: `feat(direct): import a one-line invite and send with the stored authorization`

## Phase 2: Remove the task sender from the web UI

- [x] Remove `/send`, sent tasks, `Sender`, `SendFuture`, `SentTask`, `SentState` (and `direct::outcome`, which only the UI used)
- [x] UI import uses `import_bundle` and accepts the invite string
- [x] `mise run check` passes
- [x] Commit: `refactor(admin-ui): leave sending tasks to agents and the CLI`

Phases 3–5 landed as two commits instead of three: the store query on its
own, then one UI commit. The screens came out of one split of `admin_ui.rs`
into `host`, `caller` and `advanced` modules, and the components were unused
(so `clippy -D warnings` failed) until the screens used them.

## Phase 3: Theme and reusable components

- [x] Registry components `table`, `badge`, `alert`, `avatar` (`select` not needed: a styled native `<select>`)
- [x] Theme tokens from the canvas palette
- [x] `admin_components.rs`: `app_header`, `page_title`, `status_dot`, `field`, `copy_button`, `copy_field`, `step`, `key_code`
- [x] Copy script (`/admin.js`, which also keeps the activity stream on the Advanced page)
- [x] Fixed on the way: `attributes! { r#for=… }` rendered a literal `r#for`, so no label named its input; now `for=…`

## Phase 4: Host screen and Advanced page

- [x] Last task per caller from the store (`NodeStore::last_tasks`, `Administration::last_tasks`)
- [x] Host screen: workspace line, people table, invite panel with string and command
- [x] Two-step revoke
- [x] `/advanced` with the old sections; notice for pending changes
- [x] Commit: `feat(store): list each caller's newest task`

## Phase 5: Caller screen

- [x] Steps, grouped connect code, agents list with the CLI command
- [x] Tests
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): one screen per mode, built from reusable components`

## Phase 6: Documentation

- [x] README updated
- [x] Commit: `docs(readme): describe the simplified web UI and the one-line invite`

## Verification

- [x] All tests pass (`mise run check`)
- [x] Manual smoke test through the public relay (agent-browser, 25.09.2026): host invite from the pasted connect code; caller imports the invite line in the UI; `AETHER_DATA_DIR=… aether node request --scope direct --agent-id tudor --text …` answers "fake Codex result"; the host row shows "Answered just now"; revoke asks, then removes access; the next request fails with `permission denied`. The CLI import of the command is covered by `node_serve_executes_a_signed_task_and_denies_another_endpoint`
- [x] Edge cases: invite for another endpoint; malformed invite string; expired invitation (`an_expired_invitation_is_not_imported`); revoke asks before it acts. An expired contact in the list is rendered ("Invite expired") but not tested
- [x] Screens checked in a browser at 1280 px and 390 px; a long folder path now breaks instead of scrolling the page sideways

## Review

- [x] Code reviewed
- [x] PLAN.md updated if the approach changed (see the note on phases 3–5; `--agent-id` on `node serve` turns on directory publication, so the host name comes from `$USER`)
- [x] TODO.md items all checked off
