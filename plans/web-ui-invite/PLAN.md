# Plan: Invite a friend and send tasks from the admin web UI

## Goal

Today a direct-scope connection between two people needs seven command-line
steps, one of which (`cargo run --example sign_authorization`) needs the
source repository. This plan moves the flow into the local admin web UI:

- **Host** (the node that runs Codex): an "Invite a friend" form. The host
  enters the friend's name and endpoint id. The node signs the friend's
  permission with its own issuer key, applies the grant to the running node,
  and shows one invitation bundle to send to the friend.
- **Caller** (the friend): the UI shows the caller's endpoint id, imports the
  bundle, sends a task through the relay, and shows the result.

The wire format, the signed bytes and the database schema do not change.

## Approach

### Decisions (made on 2026-09-25)

- **Both sides in the UI.** The caller runs the same UI with
  `aether node admin --data-dir <client dir> --port 0`.
- **A node-owned issuer key.** The node creates `issuer.key` (32 bytes,
  owner-only) in its data directory with `key_file::load_or_create`, and
  `load_service` trusts it as the issuer `node`. No `trustedIssuers` edit and
  no `sign_authorization` step are necessary.

### Design

- **One invitation bundle.** A JSON file, `aether-invitation/v1`:
  `{ "format": "aether-invitation/v1", "invitation": <WireInvitation>,
  "authorization": <A2aAuthorization>, "relayUrl": "<url or null>" }`.
  The caller needs only this file. The authorization is bound to the
  caller's endpoint id, so only the caller's key can use it. The bundle is
  not a secret in the cryptographic sense, but the UI shows it once and does
  not store it.
- **Minimal policy.** `trustedIssuers` and `grants` become optional in
  `policy.json` (`#[serde(default)]`), and `load_service` no longer requires
  them to be non-empty. A new node can start with only `scope`, `workspace`
  and the limits, and get its grants from the UI.
- **Grant application.** The UI is already the confirming authority for a
  grant change (`Administration::confirm_ui`). A new
  `Administration::apply_ui(change)` applies a change at once, with the audit
  actor `ui`, through the same `apply_now` path. So the grant is durable
  (`admin.grant.<id>`), replayed at restart, and live at once.
- **Host context.** `node serve` passes a new `HostContext` into the UI:
  data directory, node endpoint id, relay URL (from `--relay-url`), and the
  host's name. The host's name is a UI field, stored once in configuration
  (`admin.host_name`). `node admin` has no host context, so it hides the
  invite form.
- **Caller state.** An imported bundle is stored like `node direct import`
  stores an invitation (signed record `direct:<agent>`). The authorization
  and relay URL go into configuration (`caller.authorization.<agent>`,
  `caller.relay.<agent>`).
- **Sending a task.** A request can take minutes, longer than the UI's
  request time limit. The form starts the request as a background task and
  returns at once. The results are kept in memory and listed in a "Sent
  tasks" section; the page shows the final answer text when it arrives. The
  sender is a function in the app context, so tests can replace the network.
- **Library, not binary.** The invite, import, contact lookup and request
  code moves from `main.rs` into a new module `crates/aether/src/direct.rs`.
  The signing code moves from `examples/sign_authorization.rs` into
  `identity.rs`. The CLI and the example call the library, so the CLI keeps
  its behavior.

### Security

- The UI keeps its current guards: loopback-only `Host`, one admin session,
  same-origin POST (`require_admin_session`).
- Anyone who can use the host's admin UI can grant access. This is already
  true today for grant changes.
- The issuer key is in the data directory, next to `endpoint.key` and
  `discovery.key`, with mode 0600.

- **Revoke (added 2026-09-25).** The host UI lists the friends that have a
  grant and has a "Revoke" button for each. `PolicyEngine` can only add or
  replace a grant today, so this adds `PolicyEngine::remove_grant`,
  `ProofNode::revoke_grant` and `NodeService::revoke_grant`. A revocation
  removes the live grant, deletes the stored `admin.grant.<id>` value (new
  `NodeStore::delete_configuration`), and records `admin.revoked.<id>`, so a
  grant from `policy.json` is skipped at restart too. Queued tasks of the
  revoked friend are rejected at dispatch by the existing live recheck.
- **The agent's home (added 2026-09-25).** Understood as the workspace: the
  directory Codex works in for a remote task. The host UI shows the current
  workspace path and has a form to change it. It uses the existing live
  `Change::Workspace` path (durable, replayed at restart, limited by
  `approvedWorkspaces` when the policy names them), through `apply_ui`.

### Out of scope

- Directory and managed scopes in the UI (direct scope only).
- Choosing a workspace per friend. The grant names the workspace id
  `workspace`, as the fixtures and `sign_authorization` do; the node's one
  workspace path applies.

## Implementation Phases

### Phase 1: Move invite, import, request and signing into the library

- Add `crates/aether/src/direct.rs` with `create_invitation`,
  `import_invitation`, `contact`, and `send_request` (authorization + text →
  frames, through the relay when a relay URL is set). Move the code from
  `run_invite`, `run_direct`, `direct_contact` and the direct arm of
  `NodeCommand::Request` in `main.rs`.
- Add `identity::sign_direct_access(key, input) -> (A2aAuthorization, Grant)`
  from `examples/sign_authorization.rs`. The example calls it.
- `main.rs` calls the new functions. No output or exit code changes.
  **Commit:** `refactor(direct): move invite, import, request and signing into the library`

### Phase 2: Node-owned issuer key and a minimal policy

- `load_service` loads or creates `<data-dir>/issuer.key` and adds its public
  key to the trusted issuers as `node`.
- `trustedIssuers` and `grants` get `#[serde(default)]`; remove the two
  non-empty checks.
- Tests: a policy with no grants and no trusted issuers loads; a grant signed
  by the node key verifies; `issuer.key` is created with mode 0600 and reused.
  **Commit:** `feat(node): trust a node-owned issuer key and allow a policy without grants`

### Phase 3: Host UI — invite a friend

- `Administration::apply_ui(change)`, with audit actor `ui`.
- `HostContext` passed from `node serve` into `build_admin_ui`.
- `POST /invite` (friend name, endpoint id, capability `review`, access
  `read_only`, days 7): sign with `sign_direct_access`, apply the grant with
  `apply_ui`, create the invitation with `direct::create_invitation`, render
  the bundle in a read-only text area.
- Add `/invite` to the axum route table and the request time limit.
- Tests: the form requires the admin session; a bad endpoint id is refused;
  the bundle's invitation and authorization verify; the grant is counted and
  stored.
  **Commit:** `feat(admin-ui): invite a friend and apply the grant from the web UI`

### Phase 4: Caller UI — import an invitation and send tasks

- Show the caller's endpoint id (from `endpoint.key`).
- `POST /import`: parse and verify the bundle, store the contact, the
  authorization and the relay URL.
- `POST /send`: contact + text → background request through the sender in
  the app context; `Sent tasks` section lists status and final answer.
- Add `/import` and `/send` to the axum route table.
- Tests with a fake sender: import stores the contact; send records a
  result; a bundle with a bad signature or another format is refused.
  **Commit:** `feat(admin-ui): import an invitation and send tasks from the web UI`

### Phase 5: Revoke a friend

- `PolicyEngine::remove_grant`, `ProofNode::revoke_grant`,
  `NodeService::revoke_grant`; `NodeStore::delete_configuration`.
- `load_service` skips a grant whose id has `admin.revoked.<id>`.
- Host UI: "Friends" list (grant id, friend name, endpoint id, expiry) with a
  `POST /revoke` button per grant.
- Tests: a revoked friend's next request is denied; a queued task is rejected
  at dispatch; the revocation survives a restart for a stored grant and for a
  `policy.json` grant.
  **Commit:** `feat(admin-ui): revoke a friend's access from the web UI`

### Phase 6: Set the agent's workspace

- Host UI: show the current workspace path; `POST /workspace` changes it
  through `Change::Workspace` and `apply_ui`.
- Tests: the change applies live and is replayed at restart; a path outside
  `approvedWorkspaces` is refused when the policy names approved workspaces;
  a missing directory is refused.
  **Commit:** `feat(admin-ui): set the agent's workspace from the web UI`

### Phase 7: Documentation

- README: a "Set up with the web UI" section with the new steps for host and
  caller.
  **Commit:** `docs(readme): set up a direct connection from the web UI`

## Risks & Tradeoffs

- **The node signs its own grants.** A copy of `issuer.key` lets its holder
  mint grants for this node. Mitigation: mode 0600, same trust level as
  `endpoint.key`; `sign_authorization` and a separate key remain possible.
- **Sent-task results are in memory.** A UI restart loses the list. The task
  itself is on the host and can be queried again later; persisting caller
  results is out of scope.
- **Two processes with one endpoint key.** If the friend runs `node serve`
  and the caller UI on the same data directory, both use the same iroh key.
  Mitigation: the README tells the caller to use a separate client data
  directory, as the CLI steps already do.
- **Public relay is open.** Anyone who knows the URL can relay traffic. Not
  changed by this plan.

## Open Questions

- Default grant lifetime: 7 days (same as `sign_authorization`). Change?
- Capabilities: only `review` in the form for now. Should the form offer
  more?
