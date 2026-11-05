# Plan: Simplify the node after the October 2026 review

## Goal

A read-only review on 25 October 2026 found dead code, duplicate code,
parallel code paths and wasted work in the Rust node and the Hermes plugin.
This plan removes them. The result is less code and fewer store opens,
connections and polls.

The review also found items that need a product decision. This plan does
not include them (see "Not in this plan").

## Rules for all phases

- **The wire format, the signed bytes and the database schema do not
  change.** Phase 11 sets `journal_mode=WAL`; that is the only change to
  the database file.
- **`aether-core` keeps zero dependencies.**
- **`unsafe_code = "forbid"` and the workspace Clippy lints stay as they
  are.** Do not add `allow` attributes to get past a lint.
- **One phase is one commit.** Each commit passes `mise run check`. The
  commit message uses Conventional Commits with a scope, and has no
  attribution lines.
- **Tests:** when you remove code, remove the tests that only test that
  code. Do not add tests that only assert that removed code is absent.
  When code that only tests use moves, move it into `#[cfg(test)]` or into
  `crates/aether/tests/`.
- If a step proves to be wrong when you read the code (for example, a
  "test-only" item has a production caller), do not do that step. Record
  the reason in TODO.md.

## Phases

### Phase 1: Remove the policy middleware and the plugin system

`plugins::middleware` has no production caller, and only aether-core tests
call `PolicyEngine::add_middleware`. Thus the middleware list is always
empty.

- Delete `crates/aether/src/plugins/` and its `mod` line.
- In `aether-core`: delete `PolicyMiddleware`, `MiddlewareDecision`,
  `add_middleware`, the middleware loop in `admit_authenticated` and the
  three `Denial::Middleware*` variants, with their tests.
- If no production code calls `cancel()` on `aether_core::Cancellation`,
  and `Deadline` is always `Deadline::after(1s)` in production, remove the
  two parameters from `admit`, `admit_verified` and the `TrustProvider`
  methods. If the change spreads too far, skip this step and record it.

### Phase 2: Remove store and fixture code that only tests use

In `crates/aether-store/src/lib.rs` and `pact.rs`, grep each item. Delete
the item and its tests, or move it into a `#[cfg(test)]` helper when other
tests need it: `accept_replay_key`, `add_audit`, `add_pending_change`,
`pending_change`, `remove_pending_change`, `put_grant`,
`clear_expired_results`, `is_open`, `expire_access_request_drafts`,
`backup` (keep `backup_connection`), `PactStore::close_context`.

Delete `fixture::create` (no callers).

### Phase 3: One transport router

Production uses `router_with_async` with `NodeService::handle_async`, which
only wraps the sync `handle` in `spawn_blocking`. `handler_service` already
does that.

- Keep one `router(endpoint, handler)` that uses `handler_service`.
- Delete `handle_async`, `router_with_async`, `serve_once_with`,
  `serve_with`, `serve_with_limit`, `router_with`, the private
  `serve_connection`/`accept_connection` chain, `request_once_from` and
  `bind`. Change the tests to use `router`.

### Phase 4: Move the in-memory directory into the tests

`discovery::Directory` and `DirectContacts` (`discovery.rs:35-124`) are
used only by `tests/e2e.rs` and the discovery tests. Move them into the
test code.

### Phase 5: One MCP administration backend

Production builds only `McpServer::socket`.

- Delete `AdministrationBackend::Local`, `McpServer::new`, `Default`, the
  `Local` arms and the unused `serve_stdio` and `serve_stdio_socket`.
- Change the `Local` tests to start a real socket with
  `admin_socket::start`, as the admin_socket tests do.
- Delete `Request::DirectPeers`/`Response::DirectPeers` from the admin
  socket. The client maps `DirectGrants` to peers.
- Move `offer`, `accept` and `keep_hub_name` out of `admin_ui/hub.rs` into
  a domain module (for example `hub_membership.rs`), because MCP calls
  them.

### Phase 6: `Administration` always has a store

Production always uses `shared_with_store`. Make the field
`store: NodeStore`. Delete `Administration::new`, `shared()`, the in-memory
`pending` map, `pending_base_revision`, the `audit` vector, `confirm_ui`
(keep the store version) and the `if let Some(store)` branches. Tests use
a store in a temporary directory.

### Phase 7: `Change` wraps `ChangeRequest`

- `struct Change { request: ChangeRequest, expires_at: SystemTime }`.
  Put `id`, `kind` and `value` on `ChangeRequest` one time. Parse a
  pending row back through one function.
- Delete the `Skill` variant (it always fails) and the `Invitation`
  variant (it writes `admin.invitation.<id>`, and nothing reads that key)
  from `ChangeRequest`, the MCP schema and the admin socket.

### Phase 8: One `LiveNode` handle for `Administration`

Replace the 11 `Option<Arc<dyn Fn…>>` fields of `Administration` and their
type aliases and setters with `live: Option<Arc<dyn LiveNode>>`.
`NodeService` (with the endpoint, the directory publication, the relay
switch and the outbox waker that `main.rs` needs) implements the trait.
Delete the one-line `NodeService` forwarders that no other code needs.
Merge the duplicate async-to-sync bridges in `main.rs:1007-1044`.

### Phase 9: One admission path in `proof.rs`

- Merge `admit_inner`, `admit_verified` and `submit_inner` into one
  `admit(input, identity, replay_key)` that ends in `enqueue`.
- Delete `submit`, `submit_with_replay`, `Submission::Completed`, the
  `pending_results` waiter map, `complete_waiter` and `SUBMIT_WAIT`. Tests
  wait by polling `state()`/`events()` in a test helper.
- Delete `AdmitOutcome` and return `aether_store::Admission`.

### Phase 10: One owner for the live workspace path

The path is in `NodeService`, `ProofNode`, `Executor` and `Agent`. Keep
the `Executor` value as the source. `ProofNode` and `NodeService` read it.
Remove the second canonicalization in `NodeService::apply_workspace`.

### Phase 11: Open the node store cheaply

- One `fn open_connection(path, migrations)` for `NodeStore`,
  `ControlStore` and `PactStore`. It sets `busy_timeout`,
  `foreign_keys=ON`, `journal_mode=WAL` and `synchronous=NORMAL`.
- Run the backup, the migration and the foreign-key check one time for
  each path for each process (`OnceLock`/`Mutex<HashSet<PathBuf>>`). Later
  opens only check that `user_version` is the latest.
- Add `NodeStore::in_dir(&Path)` and use it in place of
  `NodeStore::open(x.join("node.sqlite"))` and the private wrappers
  (`direct::open_store`, `access_groups::store`, `admin_ui::hub::node_store`,
  `outbox::open`).
- Replace the per-key getter and setter pairs in `node_config.rs:371-510`
  with one generic get/set pair over a key enum. The key strings stay the
  same.

### Phase 12: One self-signed certificate helper and one HTTPS hardening

- Move the self-signed certificate code from `control_service.rs:600` and
  `admin_ui.rs:1001` into `key_file.rs` as
  `self_signed_cert(dir, cert_name, key_name, sans)`.
- Give the control service the same handshake timeout, connection limit,
  request timeout and body limit as the admin UI, through one shared
  helper.
- In the admin UI, replace the list of 35 forwarded paths with
  `fallback_service`, if the 404 behaviour stays the same.

### Phase 13: Remove small duplicate helpers

- `impl From<&aether_core::Claim> for transport::A2aClaim`; use it at the
  six hand-written sites.
- Call `node_config::grant_json` in `fixture.rs` and `tests/commands.rs`.
- Use `aether_core::time::unix_seconds` at the inline "now" sites.
- Admin UI: use `admin_components::grouped` in `admin_ui/hub.rs`; one
  `ago(seconds)` in `admin_components` for `caller.rs` and `host.rs` (keep
  the `host.rs` wording, with "yesterday").
- One relay URL parser and one `apply_relay` for `mcp.rs`, `admin_ui` and
  `main.rs`. One `set_hub_url` for `mcp.rs` and `admin_ui/hub.rs`.
- Derive serde on `discovery::Record`/`Invitation` and
  `aether_store::ManagedMember` and delete `WireRecord`, `WireInvitation`
  and `WireManagedMember`. The JSON must stay byte-compatible: check the
  field names, the time encoding and the hex encoding.

### Phase 14: Shared test helpers

- `crates/aether/tests/common/mod.rs` with `ServerProcess`, `read_event`
  and `aether_command`, used by `commands.rs`, `pact.rs` and `e2e.rs`.
- `pub const FAKE_CODEX_SCRIPT` in `aether::fixture`.
- `TestHub::add_listing` for the hand-signed listings in the admin UI,
  MCP, access request and hub sync tests.

### Phase 15: Less polling and fewer store opens on the hot paths

- Admin SSE `/events`: query with `id > ? LIMIT n`, send the whole batch,
  and wait on the node event broadcast (or a notify) instead of a 1 s
  sleep.
- Scheduler: raise the `SCHEDULER_POLL` fallback from 50 ms to 1 s.
- PACT gateway: wait on `subscribe_events()` filtered by task id instead
  of a 200 ms poll; run `PactStore` calls in `spawn_blocking`.
- Outbox: one store for each round; load the contact bundles one time
  into a map; run the store part in `spawn_blocking`.
- MCP: open the store one time in `wait_for` and in search/list tools;
  keep one `HubClient` in `McpServer` and in the outbox loop, and rebuild
  it only when the hub URL changes.
- `node_service.rs`: consume the frame payload instead of cloning it.

### Phase 16: Reuse the QUIC connection while a task is polled

- Server: `serve_connected` accepts streams in a loop until the peer
  closes, with the existing per-peer limiter.
- Client: `request_with_polling_on` and the outbox loop keep one
  connection and open one stream for each poll. If the stream fails,
  connect again one time.

### Phase 17: Do not copy the Codex binary for each task

Resolve the binary one time in the constructor. Try `std::fs::hard_link`
first, and copy only when the hard link fails.

### Phase 18: One long-lived MCP process in the Hermes plugin

Keep one `aether mcp serve` subprocess for each `Settings`, with a lock,
increasing request ids, and a restart on EOF.

## Not in this plan (product decisions)

- The overlap of the Rust directory and managed control with the hub.
- One address resolver for each contact source.
- The agent adapter as a tagged object in the policy file.
- One encoding for signed messages, shared with the hub.
- Hermes `schemas.py` read from `tools/list`.
