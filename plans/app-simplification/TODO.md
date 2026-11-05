# TODO: Simplify the node after the October 2026 review

Each phase: `mise run check` passes, then one commit. Write the commit hash
next to the phase. Write a skipped step and its reason under the phase.

- [x] Phase 1: Remove the policy middleware and the plugin system (3fc62c6)
  - Also removed `Denial::Cancelled`: only the removed cancellation checks
    made it.
  - `Deadline` and `Cancellation` stayed, because `DiscoveryProvider` still
    used them. Phase 4 removed the trait and the two types.
- [x] Phase 2: Remove store and fixture code that only tests use (f33d065)
  - `accept_replay_key`, `put_grant`, `add_audit` and `backup` became
    helpers in the store tests, because tests of other code need them.
- [x] Phase 3: One transport router (ae195d5)
  - Also deleted `serve_incoming` and `async_handler_service`: only the
    removed functions used them. `request_once_from` callers in tests now
    use `request_once` with the caller's data directory.
- [x] Phase 4: Move the in-memory directory into the tests (900db07)
  - `tests/e2e.rs` has the two types with only the methods that it uses.
    The discovery unit tests now call `validate_record` and
    `validate_invitation`; the tests of only the in-memory types are gone.
  - Also removed `DiscoveryProvider`, `Deadline` and `Cancellation` from
    `aether-core`. The trait had no implementation. `SignedRecord`,
    `AgentRecord` and `OwnerId` had no other user, so they are gone too.
- [x] Phase 5: One MCP administration backend (c9b76df)
  - The admin socket protocol change is safe: `aether mcp serve` and
    `aether node serve` are the same binary, and the Hermes plugin uses
    only `agent.sock`, not `admin.sock`.
  - `hub_message` and `APPROVAL_SECONDS` also moved to `hub_membership.rs`,
    because `offer` and `accept` use them.
  - `aether.settings.set_relay` saved the relay without a running node only
    through the `Local` backend. It now saves the relay when no node owns
    the socket, as its description says. Before, it gave an error.
- [x] Phase 6: `Administration` always has a store (2cb14e0)
  - `change` now takes `&Change`: without the in-memory map it does not
    keep the change.
  - Tests use `admin::test_support::shared_in(dir)`, a store in the test
    directory. The audit now comes from the store, so the tests expect the
    store records (`pending`, `confirmed`, `applied`), as production does.
  - The in-memory stale test was the same as the store stale test, so it
    is gone.
- [x] Phase 7: `Change` wraps `ChangeRequest` (c4c436e)
  - `ChangeRequest::from_stored` parses a pending row. The kind and value
    strings did not change.
  - A stored pending row of kind `skill` or `invitation` now fails to
    confirm with `InvalidField("change_type")`, as any unknown kind does.
  - The Hermes plugin and the admin UI did not send these two kinds.
  - `secrecy` had no other user, so it is gone from `aether`.
- [x] Phase 8: One `LiveNode` handle for `Administration` (40fb331)
  - `node_service::RunningNode` implements the trait, not `NodeService`: it
    holds the service, and gets the endpoint, the directory publication and
    the outbox waker when they start. Until then, their changes are
    unsupported, as before. The admin socket still starts first, because it
    is the "already running" check.
  - `DirectoryPublication` and `signed_node_record` moved from `main.rs` to
    `directory_publication.rs`, so that `RunningNode` can renew the record
    and tests can use the real node. Their `()` errors are now
    `PublicationError`.
  - Deleted the `NodeService` forwarders `cancel_local`, `set_queue_limit`,
    `current_queue_limit`, `revoke_grant` and `grants`.
  - Tests use `admin::test_support::FakeNode`. The MCP relay test now checks
    that the change reaches the node; the saving it checked was done by the
    test closure, not by production code.
- [x] Phase 9: One admission path in `proof.rs` (00a3c88)
  - `ProofNode::admit(input, identity, replay_key)` is the one path.
    `Identity::Claim` runs `PolicyEngine::admit`, and `Identity::Verified`
    (PACT) runs `PolicyEngine::admit_verified`. The checks and their order
    did not change: shutdown, task mismatch, scope, policy, group, queue.
  - `SubmissionInput` lost `claim` (now in `Identity`) and `workspace`
    (no code read it). `NodeService::handle` no longer reads its workspace
    copy for each request.
  - The scheduler no longer carries a result for a waiter, so
    `prepare_execution` returns a plain `ProofError` and `finish_execution`
    takes no arguments.
  - The tests poll `state()` through `submit_and_wait` (in the `proof`
    tests and in `tests/e2e.rs`). The store does not keep the denial of a
    task that dispatch rejects, so the dispatch tests now check the
    `rejected` state, not the denial value. The shutdown test admits
    without a waiter, and checks that task a is `cancelled` and task b
    stays `queued` in the store.
- [x] Phase 10: One owner for the live workspace path (df05f81)
  - `Executor` keeps the path in `execution::Workspace`, a shared handle.
    `ProofNode` holds a clone of the handle, and `NodeService` reads it
    through `ProofNode::workspace`. A read does not lock the executor, so
    it does not wait for a running task.
  - `Executor::set_workspace` is the one canonicalization; `ProofNode` and
    `NodeService` no longer canonicalize. `NodeService::new` no longer
    takes a workspace.
  - The Codex agent keeps its own copy: it lives in the cancellation
    handle, apart from the `Executor`. `Executor::set_workspace` still
    sets it.
- [x] Phase 11: Open the node store cheaply (7e1c350)
  - `open_connection` sets `journal_mode=WAL` and `synchronous=NORMAL`.
    The first open of a path in the process makes the backup (if a
    migration is pending), migrates and checks the foreign keys. A later
    open reads `user_version`; if it is not the latest, the open does the
    full check again, so a newer schema still fails.
  - No code or test copies or moves a database file. The migration backup
    already used the SQLite backup API. `restore` used `fs::copy`; it now
    uses the SQLite backup API too, so a `-wal` file cannot outlive it.
    No `open_for_migration` exists, and no command deletes or replaces
    the data directory.
  - `direct::open_store` and `access_groups::store` stay, because they do
    more than open: one creates the data directory, the other fails when
    `node.sqlite` does not exist. They now call `NodeStore::in_dir`.
    `admin_ui::hub::node_store` and `outbox::open` are gone.
  - The settings are not an enum: each key has a different value type, so
    each key is a unit type that implements `node_config::Setting`, with
    `setting::<S>` and `store_setting::<S>` as the one pair. The key
    strings did not change. `store_agent_kind` stays, because it refuses
    to change a stored agent type.
  - The admin storage-failure test made the directory read-only, so that
    SQLite could not make its rollback journal. In WAL mode the `-wal` file
    already exists, so the test now holds the write lock from a second
    connection (`rusqlite` is now a dev-dependency of `aether`).
  - The admin UI printed `admin_listening` before it gave the PACT gateway
    the consent page link. With faster store opens, `tests/pact.rs` sent
    its first turn in that gap and got no consent link. The event now
    comes after the link is set.
- [x] Phase 12: One self-signed certificate helper and one HTTPS hardening (0d98cea)
  - `key_file::self_signed_cert` makes the certificate for the admin UI
    and the control service. The file names, the SANs and the owner-only
    key did not change, so existing data directories keep their
    certificates. Each caller keeps its own rustls configuration: the
    control service still offers `h2` and `http/1.1` through ALPN, and the
    admin UI offers no ALPN.
  - `https::serve` with a `Hardening` value serves both. The admin UI
    limits did not change (5 s handshake, 32 connections, 10 s request,
    8 KiB body). The control service now has a 5 s handshake (was the
    `axum_server` default of 10 s), 1024 connections (was no limit), a
    10 s request time limit (was none) and a 64 KiB body (was the axum
    default of 2 MiB). The largest valid record has 16 addresses of 256
    bytes; a redeem request is a token and one record.
  - The request time limit now also covers the admin `/events` route. It
    limits only the time to the response head; the SSE handler returns
    its head at once, and the stream continues after it.
  - The PACT server does not use `https::serve`: it serves plain HTTP
    behind a TLS proxy, refuses extra requests with `503` (load shed, not
    a dropped connection), times out with `504`, and stops with a
    graceful-shutdown future, not a `Handle`.
  - The admin UI forwards every path to topcoat with `fallback_service`.
    An unknown path still gets `404`. The body is now `not found` (was
    empty), with the admin UI security headers. The host check does not
    apply to a `404`, as before. No test checks the `404` body.
- [x] Phase 13: Remove small duplicate helpers (f68c660, e488498, d50e9dd)
  - `From<&Claim> for A2aClaim` replaces the six hand-written claims.
    `fixture.rs` and `tests/commands.rs` call `node_config::grant_json`.
    Their copies wrote `accessMode` and `approvalRule` as fixed text; each
    of their grants is `read_only` and `automatic`, so the JSON is the same.
  - The inline "now" and `append_time` sites call
    `aether_core::time::unix_seconds` (or `unix_timestamp`). Each keeps its
    old answer for a time before 1970, so the signed bytes do not change.
  - `admin_components::grouped` and `admin_components::ago` serve the host,
    caller and hub pages. The caller page said "1 days ago"; it now says
    "yesterday", as the host page does.
  - `node_config::valid_relay_url` is the one relay URL rule for MCP and
    the admin UI; they already agreed on it. `node_config::apply_relay`
    switches the relay and keeps it as the saved and the active relay, for
    the admin UI and the admin socket (`NodeService::set_relay`). Each
    caller still reads the previous relay itself: the admin UI falls back
    to its relay in memory when no active relay is stored, and the admin
    socket refuses with a store error.
  - `hub_client::hub_setting` is the one hub URL rule, and
    `hub_client::set_hub_url` keeps the hub and forgets the hub name, for
    MCP and the admin UI.
  - The rules differ only for the word `off`. MCP accepts `off` for the
    relay and the hub, and `main.rs` accepts it for `--relay-url`; the
    admin UI refuses it (an empty field means off there). The stricter
    rule would make MCP refuse `off`, but its tool descriptions and the
    `--relay-url` flag document `off`. So `off` stays a word of MCP and the
    command line, outside the shared URL rules, and no input changes its
    answer. `main.rs` does not check the relay URL itself; iroh parses it
    when the node binds.
  - `aether_store::ManagedMember` derives serde (camelCase, unknown fields
    refused, `revokedAt` as `null` when absent), and `WireManagedMember`
    is gone. `aether-store` now depends on `serde`. A unit test in
    `control_service` holds the wire JSON.
  - Not done: `discovery::Record` and `Invitation` keep `WireRecord` and
    `WireInvitation`. `expires_at` is a `SystemTime`, which needs a custom
    serde module for whole seconds. The control service also stores the
    `WireRecord` JSON as the record in its database and works on its
    `u64` expiry; and an expiry that `SystemTime` cannot hold is now
    `ControlError::InvalidRecord` (400), but would become a JSON rejection
    (422).
- [x] Phase 14: Shared test helpers (542c82f)
  - `crates/aether/tests/common/mod.rs` holds `aether_command` (hub off),
    `ServerProcess` (relay off) and `read_event` (with an optional event
    name). `commands.rs` and `pact.rs` use them. `pact.rs` already set the
    hub and the relay to `off`, so the shared command is the same. Its node
    now stops with SIGINT at the end, and the test checks the exit status,
    so each shared item has a user in each test file and no `allow` is
    needed.
  - Not done for `e2e.rs`: it starts no process and has no copy of these
    helpers.
  - `aether::fixture::FAKE_CODEX_SCRIPT` replaces the copy in `pact.rs`, the
    function in `commands.rs` and three identical inline copies there. The
    four different scripts in `commands.rs` and those in `agent/codex.rs`
    stay.
  - `TestHub::add_listing` signs and lists a service agent for the admin UI
    and MCP tests. The `significant_drop_tightening` expectation on the MCP
    tests is gone with the lock it was for.
  - Not done for the access request and hub sync tests: they put no listing
    on a hub. The access request test uses a fake signature with no key,
    and the hub sync test signs a `KeptListing` for the store.
- [x] Phase 15: Less polling and fewer store opens on the hot paths
  (0f84607, 8bd4713, d2acba5, dbe15b4, 57fe8fc, 3bee144)
  - Admin SSE (0f84607): the audit part of the event id is a count of
    records, not a row id, so the query is `ORDER BY id LIMIT ? OFFSET ?`
    and the ids keep their meaning. Task events use `rowid > ? LIMIT ?`.
    The feed wakes on a `watch` that `Administration` marks after each call
    that can write an audit record, on the node event broadcast (a new
    `LiveNode::subscribe_events`), or after a 3 s fallback for records of
    other processes. One change in a broken store only: a batch with an
    unsafe audit row sends no audit events from that row on; before, such a
    row stopped all audit events.
  - Scheduler (8bd4713): admission, finish, cancellation, shutdown and a
    failed thread spawn already woke it. A leased task that does not start
    (cancelled, refused, bad path) now wakes it too. Lease expiry and
    maintenance still run on the loop, so they are at most 1 s late. Not
    done: a wake at start for the tasks that a previous run left queued;
    `peer_cancel_marks_an_owned_queued_task_cancelled` needs such a task to
    stay queued. Those tasks start at the first poll (1 s).
  - PACT gateway (d2acba5): the fallback read is 1 s. `message:send`, the
    device authorization and the token route run their store work in
    `spawn_blocking`; the consent pages of the admin UI are not the gateway
    and do not change.
  - Outbox (dbe15b4): a contact map that cannot be read counts as no
    contacts, as each failed lookup did before, so those tasks end as
    before.
  - MCP (57fe8fc): `HubCache` keeps the client, and with it the hub's
    protocol answer, while the URL stays the same. The admin UI keeps its
    own `HubSlot`.
- [x] Phase 16: Reuse the QUIC connection while a task is polled
  (610024d, 889f89f)
  - Server (610024d): a connection serves its streams one after the other,
    so a peer runs at most one handler for each `MAX_CONNECTIONS` permit.
    Each stream goes through the per-peer limiter and the same limits. The
    first stream must come within `STREAM_TIMEOUT`; after a response the
    handler waits `RESPONSE_LINGER` for the next stream or the close. The
    linger is now 2 s (was 1 s), longer than the outbox poll interval
    (1 s). A client that sends one request and closes ends the handler at
    once, as before; a peer that goes away without a close holds its permit
    up to 2 s, not 1 s.
  - Client (889f89f): `transport::Link` keeps the connection of one
    followed task and opens one stream for each poll. It replaces
    `exchange_on`; `Courier::exchange` takes the link. The first request of
    a link always goes on a new connection, so the submission is never sent
    again by the link. When a stream on a kept connection fails, the link
    connects one time more and sends that poll again (polls repeat the task
    ID and idempotency key). An old host closes the kept connection after
    its 1 s linger, so the second poll waits up to about 1 s one time; from
    then on the link uses a new connection for each exchange and closes it,
    as before. No error is logged on that path.
- [x] Phase 17: Do not copy the Codex binary for each task (4578b81)
  - The binary is found in the constructor, and found again only when that
    file is gone (for example after an update moved it).
  - A hard link is made only when the task cannot change the file: another
    user owns it and only that user can write it. Else the file is copied,
    as before. Reason: the task can write in its folder, and the Codex
    sandbox rules go by path, so a hard link to a file of the node user
    would let a remote task write or chmod the original binary. The code
    itself never writes or chmods the file. For a binary in the user's own
    folder (npm, Homebrew as the user) the copy stays.
  - A file of an earlier try is removed before the copy, so a copy never
    writes through a hard link into its original.
- [x] Phase 18: One long-lived MCP process in the Hermes plugin (e223162)
  - Hermes runs the plugin in one long-lived process (gateway or CLI) and
    calls the tool handlers from more than one thread, so one kept process
    saves a start and an `initialize` for each call. Calls for one command
    and data directory now wait for each other on a lock; before, each call
    had its own process and they ran at the same time. The wait for the
    lock counts in the call's time limit.
  - A kept process that stopped before the request was written is started
    again, and the call goes to the new process. A request that was
    written is not sent again: when the process stops after the write, the
    call fails as before. If the process dies at the same time as the
    write, the call can fail one time.
  - A call that times out kills the process; the next call starts a new
    one. At interpreter exit each process gets 1 s to stop, then it is
    killed; a call that runs then is not waited for longer than 1 s.
  - `Settings.tools_available` keeps the path of the command and finds it
    again only when it was not found or that file is gone. (Hermes also
    keeps each `check_fn` result for 30 s.)
