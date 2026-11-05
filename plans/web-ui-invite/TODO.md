# TODO: Invite a friend and send tasks from the admin web UI

## Phase 1: Move invite, import, request and signing into the library

- [x] Add `crates/aether/src/direct.rs`: `create_invitation`, `import_invitation`, `contact`, `request_frame`, `send`
- [x] Move the code from `run_invite`, `run_direct`, `direct_contact` and the direct arm of `NodeCommand::Request`
- [x] Add `identity::sign_access` and `node_config::grant_json`; `examples/sign_authorization.rs` calls them
- [x] CLI output and exit codes unchanged (command tests pass without change). One difference: an import stores the invitation as normalized JSON, not the raw file bytes; `node direct resolve` prints the same
- [x] `mise run check` passes
- [x] Commit: `refactor(direct): move invite, import, request and signing into the library`

## Phase 2: Node-owned issuer key and a minimal policy

- [x] `load_service` loads or creates `<data-dir>/issuer.key` and trusts it as `node` (a policy that names `node` itself is refused)
- [x] `trustedIssuers` and `grants` optional; non-empty checks removed
- [x] Test: a policy with no grants and no trusted issuers loads
- [x] Test: a grant signed by the node key verifies and applies; one signed by another key does not
- [x] Test: `issuer.key` is reused at restart (mode 0600 is covered by the `key_file` tests)
- [x] `mise run check` passes
- [x] Commit: `feat(node): trust a node-owned issuer key and allow a policy without grants`

## Phase 3: Host UI — invite a friend

- [x] `Administration::apply_ui(change)` with audit actor `ui`
- [x] `HostContext` from `node serve` into `build_admin_ui` (through `UiOptions`); hidden in `node admin`
- [x] `POST /invite` signs, applies the grant, renders the `aether-invitation/v1` bundle
- [x] `/invite` in the axum route table, inside the request time limit
- [x] Test: `/invite` without the admin session is forbidden
- [x] Test: a bad endpoint id is refused, and no grant is applied
- [x] Test: the bundle's invitation verifies; the grant is signed by the node key and stored as `admin.grant.<id>`
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): invite a friend and apply the grant from the web UI`

## Phase 4: Caller UI — import an invitation and send tasks

- [x] Show the caller's endpoint id (only in `node admin`; `node serve` has no caller forms, so the caller and the node never share one endpoint key)
- [x] `POST /import` verifies the bundle and stores it as `caller.contact.<name>` (contact, authorization and relay URL)
- [x] `POST /send` starts a background request; "Sent tasks" shows status and final answer (`direct::outcome`)
- [x] `/import` and `/send` in the axum route table
- [x] Test (fake sender): import lists the contact; send shows the answer; the bundle's relay URL reaches the sender
- [x] Test: a bundle for another endpoint and an unknown format are refused
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): import an invitation and send tasks from the web UI`

## Phase 5: Revoke a friend

- [x] `PolicyEngine::remove_grant`, `ProofNode::revoke_grant`, `NodeService::revoke_grant` (plus `grants()` for the list)
- [x] `NodeStore::delete_configuration`
- [x] `load_service` skips grants recorded as `admin.revoked.<id>`, before it verifies them
- [x] Host UI "Friends" list with `POST /revoke`; `Administration::revoke_ui`
- [x] Test: a revoked friend's next request is denied — a request without a grant is already refused at admission (`LocalGrantMissing`); the new tests check that the grant is gone
- [x] Test: a queued task of the revoked friend is rejected at dispatch (`a_grant_removed_while_queued_rejects_the_task_at_dispatch`)
- [x] Test: the revocation survives a restart (stored grant and `policy.json` grant)
- [x] Test: the UI list drops the friend and the store holds the revocation
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): revoke a friend's access from the web UI`

## Phase 6: Set the agent's workspace

- [x] Host UI shows the current workspace path (`Administration::current_workspace`, read from the running node)
- [x] `POST /workspace` applies `Change::Workspace` through `apply_ui`
- [x] Test: the change reaches the node and the page shows the new path; replay at restart is covered by `stored_workspace_and_grant_changes_are_restored_on_restart`
- [x] Test: a refusal from the node gives 400; the node's own checks (missing directory, `approvedWorkspaces`) are covered by the existing `node_service` and `node_config` tests
- [x] `mise run check` passes
- [x] Commit: `feat(admin-ui): set the agent's workspace from the web UI`

## Phase 7: Documentation

- [x] README section "Set up with the web UI"
- [x] Commit: `docs(readme): set up a direct connection from the web UI`

## Verification

- [x] All existing tests pass; no change to the command tests in phase 1 (no behavior change)
- [x] New tests written for the issuer key, `/invite`, `/import`, `/send`
- [x] Manual smoke test (25.09.2026, through https://aether-relay.tudorandrei.xyz, fake Codex): invite 200, import 200, first task answered "fake Codex result", revoke 200 and the Friends list empty, second task "Failed: permission denied"
- [x] Edge cases tested: wrong endpoint id format; bundle for another caller key; expired invitation (`an_expired_invitation_is_not_imported`); send with no imported contact (`a_caller_cannot_send_to_a_contact_it_has_not_imported`); host UI in `node admin` mode has no invite form
- [x] No regressions in: signed bytes (claim, grant, invitation tests unchanged), existing grant changes through MCP, stored-grant replay at restart

## Review

- [x] Code reviewed
- [x] PLAN.md updated if approach changed during implementation (revoke and workspace phases added)
- [x] All phase commits are clean and describe their intent
- [x] TODO.md items all checked off
