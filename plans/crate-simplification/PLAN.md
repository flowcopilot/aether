# Plan: Replace hand-written code with crates, and fix the audit problems

## Goal

A read-only audit on 24 September 2026 found hand-written code that crates
can replace. Most of these crates are already dependencies. The audit also
found three correctness problems, duplicate code and dead code. This plan
does these changes:

- Fix the three correctness problems: JSON output without escapes, record
  URLs built with `format!`, and a TLS key file that is readable for a short
  time.
- Section 1: use more of the crates that are already direct dependencies
  (`hex`, `tower`, `topcoat`, `usage-rs`, `tokio-util`).
- Section 2: use `toml`, `strum` and `subtle`. These crates are already in
  `Cargo.lock` as transitive dependencies.
- Section 3: add `process-wrap`, `serde_ignored`, `assert_cmd`, `predicates`
  and `insta`. Change `execution.rs` and `proof.rs` to tokio primitives.
- Remove the duplicate code and the dead code.

The result is less code. The wire format, the signed bytes and the database
format do not change.

## Approach

### Rules for all phases

- **The wire format, the signed bytes and the database do not change.**
  `identity.rs` `claim_bytes`/`grant_bytes` and `discovery.rs` record bytes
  stay hand-written. The strings in the SQLite `TEXT` columns stay the same.
- **`aether-core` keeps zero dependencies.** Shared helpers that go into
  `aether-core` use `std` only.
- **`unsafe_code = "forbid"` applies to all targets, tests included.** Thus
  `libc::kill` is not permitted. A process group kill must use a safe
  wrapper (`process-wrap`), and the test harness continues to use the
  `kill` command.
- **`panic = "abort"` stays unset.** `proof.rs` must continue to catch a
  panic in an execution thread or task.
- **One phase is one commit**, and each commit passes `mise run check`.

### Prerequisite

The working tree has an uncommitted release change to 0.2.2: `Cargo.toml`,
`Cargo.lock`, `README.md` and `crates/aether/src/mcp.rs`. That change must be
committed, or put away, before phase 1 starts. It is not part of this plan.

### Correctness fixes (phases 1–3)

1. **JSON output.** These places write JSON with `format!` and do not escape
   the values:
   - `main.rs`: lines 543, 742, 748, 895, 1107, 1684, 1844 and 1865.
   - `lib.rs:99` (`startup_log`).
   - `admin_ui.rs:757`.
   - `control_service.rs:493`.

   `record.agent_id` at `main.rs:748` comes from the `--agent-id`
   argument. Replace each `format!` with `serde_json::json!(…)` and print
   it with `Display`. The key order stays the same: `serde_json` without
   the `preserve_order` feature sorts keys, and the tests parse the JSON
   and do not compare the text, so the order does not matter.

   Where the value is already JSON (`response` at `main.rs:1107`), parse it
   into `serde_json::Value` or use `serde_json::value::RawValue`. Do not
   insert it as a string.

2. **Record URLs.** `DirectoryClient::record_url`
   (`directory_client.rs:196`) and `ManagedClient::scope_url`
   (`managed_client.rs:193`) do `join(&format!("v1/records/{agent_id}"))`.
   They reject only `/` and `\0`. Replace this with
   `Url::path_segments_mut()` (from `reqwest::Url`, so no new dependency).
   `path_segments_mut()` percent-encodes each segment. Also reject `.` and
   `..` explicitly. The axum server decodes path parameters, so the server
   side does not change.

3. **TLS key permissions.** `tls_material` (`control_service.rs:607`) calls
   `restrict_key_permissions`. That function writes `control-key.der` with
   the umask permissions and then runs `chmod 0600`. Replace it:
   - Remove a stale `control-key.der.tmp`, if one exists.
   - Create `control-key.der.tmp` with
     `OpenOptions::new().write(true).create_new(true).mode(0o600)`.
   - Write the key, call `sync_all`, then `rename` the file to
     `control-key.der`.

   This also covers the case where `control-key.der` exists but the
   certificate does not. In that case `tls_material` makes new material,
   and `create_new` on the final path would fail.

### Section 1: use crates that are already dependencies (phases 4–9)

4. **Dead code in `transport.rs`.**
   - Delete `serve_once` (line 798), which always returns `PermissionDenied`
     and has no callers.
   - Delete `ReplayWindow` (line 1544). Only a test uses it.
   - Delete the sync `encode`/`decode` (lines 389–439). Only tests use them.
     The production path is `frame_codec()` + `LengthDelimitedCodec`.

   The tests at lines 1718–1818 use `encode` to reach `validate_order` and
   `validate_payloads`. Change these tests to call the validators directly,
   or to write through the codec on a `BytesMut`. The validation coverage
   must stay the same. Do not add tests that only assert that the removed
   code is absent.

5. **`hex` crate.**
   - Replace `decode_hex`, `hex_value` and `decode_array` in
     `node_config.rs:422-452` with `hex::decode` and `<[u8; N]>::from_hex`,
     mapped to `NodeConfigError::Invalid`.
   - Replace the six hand-written `fn hex(bytes)` with `hex::encode`:
     `fixture.rs:210`, `node_config.rs:990` (test), `node_service.rs:1638`
     (test), `tests/commands.rs:2993`, `examples/sign_authorization.rs:226`
     and `admin_ui.rs:285` (`confirmation_token`).
   - Replace `hex::decode(..)?.try_into()` with `<[u8; N]>::from_hex` in
     `transport.rs:237`, `managed_client.rs:65` and `control_service.rs:389,
     426`.

6. **`hex::serde`.** Enable the `serde` feature of `hex`. Put
   `#[serde(with = "hex::serde")]` on the byte fields of `WireRecord`,
   `WireInvitation` (`control_service.rs`) and `GrantFile`
   (`node_config.rs`). The JSON stays the same. After this change, a bad
   hex value becomes a serde error, not `ControlError::InvalidRecord`.
   Before the commit, make sure that the HTTP status for a bad record does
   not change. The axum `Json` rejection gives 422, and today the handler
   gives the status of `InvalidRecord`. If the status changes, keep the
   field as a `String` for that DTO and do not do this phase for it.

7. **`tower::util::BoxCloneSyncService`.** `AetherProtocol.handler` is
   `Arc<Mutex<HandlerService>>` (`transport.rs:913`). The only reason for
   the mutex is that `BoxCloneService` is not `Sync`. Change the
   `HandlerService` type alias (line 73) to `BoxCloneSyncService`, and
   remove the `Mutex` and its lock-error path.

8. **`topcoat::router::tower::TowerService`.** Delete `TopcoatService`
   (`admin_ui.rs:156-179`). Use `TowerService::new(router)` with
   `tower::ServiceExt::map_response(into_axum_response)`. The security
   headers are set in `into_axum_response`, so they do not change.

9. **usage-rs declarations.** usage-derive 6.6.1 supports `env`, `default`,
   `requires` and `group` (verified in its `lib.rs` documentation).
   - Declare `AETHER_CODEX_BIN` (default `codex`) and `AETHER_RELAY_URL`
     on the command structs. Remove the `std::env::var` reads at `main.rs`
     lines 565, 698, 954, 987 and 1172.
   - Declare `requires` on `--managed-control` and
     `--managed-control-certificate`. Remove the
     `"must be supplied together"` arm (`main.rs:629`).
   - Replace the six-value `match` in `node request` (`main.rs:945`) with a
     `group`. That match only exists to reject flag combinations.

   **Correction during implementation:** the six-value `match` does more than
   reject flag combinations. It also selects the request mode and gives each
   branch its values, so a `group` cannot replace it, and it stays. Also, in
   usage-rs `env` works only on a flag, so the two variables became the
   flags `--codex-bin` and `--relay-url`, with the variables as fallbacks.

   Keep the exit codes. The tests assert exit code 2 for a usage error.
   Look at what usage-rs returns for a `requires` failure before you
   change the tests. Update the help snapshot and the spec test
   (`tests/commands.rs:74-157`).

### Duplicate code (phases 10–14)

10. **Unix time helpers in `aether-core`.** There are approximately 35
    `duration_since(UNIX_EPOCH)` calls in 14 files. They handle times before
    the epoch in three different ways: `map_or(0)`, `expect` and an error.

    Add `aether_core::time::{unix_seconds(SystemTime) -> Option<u64>,
    from_unix_seconds(u64) -> Option<SystemTime>}`. Each caller maps `None`
    to its own error. Replace these functions:
    - `aether-store` `now()` (line 1954).
    - `control_service.rs:597` `unix_timestamp`.
    - `main.rs:2026` `unix_timestamp`.
    - `proof.rs:1276` `timestamp_after`.
    - `transport.rs:231` `time_from_seconds`.
    - `node_config.rs:418` `time`.
    - The four `seconds()` test helpers.
    - The inline calls in `node_service.rs` (12) and in the `Wire*`
      conversions.

    `identity.rs` `append_time` and `discovery.rs` `append_time` write
    signed bytes. They must keep `map_or(0)` exactly as it is.

11. **One key-file loader.** `transport::load_or_create_key`
    (`transport.rs:1569-1639`) and `discovery::load_or_create_signing_key`
    (`discovery.rs:198-269`) have the same code: `O_NOFOLLOW` open,
    `create_new` with mode 0600, and a retry for `AlreadyExists`.

    Move the code to a new module `crates/aether/src/key_file.rs`, with
    `load_or_create(path, generate: impl FnOnce() -> [u8; 32]) ->
    Result<[u8; 32], KeyFileError>`. Each caller maps the error to its own
    type. Keep today's mappings: transport gives `Key` for an open error or
    a bad size and `Io` for a write error; discovery gives `Unavailable`
    for all errors.

    `examples/sign_authorization.rs` `load_or_create_issuer_key` (line 190)
    has no `O_NOFOLLOW` and ignores permission errors. Make it call
    `discovery::load_or_create_signing_key` after `create_dir_all`.

12. **One pinned HTTPS client.** `DirectoryClient::with_pinned_certificate`
    and `ManagedClient::with_pinned_certificate` (lines 33–60 in both files)
    have the same origin check and the same `reqwest` builder. Move them to
    one `pinned_https_client(origin, certificate_der) -> Result<(Client,
    Url), PinnedClientError>`. Each client maps the error to its own type.

13. **One scope adapter.** `plugins/direct.rs`, `plugins/directory.rs` and
    `plugins/managed.rs` are identical. Replace them with one
    `plugins::ScopeAdapter`. `middleware()` keeps the `match` on
    `ScopeKind`, so a new kind still needs a decision. A kind that needs its
    own behavior later can get its own type again.

14. **Row-mapping functions in `aether-store`.** The same struct literal
    repeats: `ManagedMember` three times, `SignedRecord` three times,
    `PendingChange` three times and `TaskEvent` two times. Move each to one
    `fn(&Row) -> rusqlite::Result<T>`. The SELECT column lists must be the
    same for each use of a function. Align them where they are different.

### Section 2: transitive crates as direct dependencies (phases 15–17)

15. **`toml` for `load_config`** (`lib.rs:67-95`). Deserialize
    `struct ConfigFile { data_dir: PathBuf }` with `deny_unknown_fields`.
    Keep the empty-path and `\0` checks. The file grammar becomes more
    permissive, because comments and spaces are now valid. The error
    messages change. Update the `lib.rs` tests.

16. **`strum` for the store enums.** Derive `IntoStaticStr` + `EnumString`
    with `serialize_all = "snake_case"` on `TaskState`, `AuditActor`,
    `AuditAction` and `AuditResult` (`aether-store/src/lib.rs:68-306`).
    `TaskState::as_str` is a `const fn` today. Keep it as a thin wrapper if a
    `const` caller exists. If not, remove it. `TaskState::allows` stays
    hand-written.

    The core enums (`ScopeKind`, `AccessMode`, `ApprovalRule`) are in
    `aether-core`, which has no dependencies. For these enums, add one
    `as_str` and one `FromStr` in `aether-core` by hand. Then delete the
    copies in `node_config.rs:344-351, 378-387`, `proof.rs:1334-1350` and
    `fixture.rs:155-161`.

    A test must check that each variant keeps its string. The strings are
    the database contract.

17. **`subtle` for the bearer check.** `examples/hermes_bridge.rs:84`
    compares the bearer token with `!=`. Use `ConstantTimeEq::ct_eq`. Add
    `subtle` as a dev-dependency, because only the example uses it.

### Section 3: new crates (phases 18–23)

18. **`process-wrap` for the Codex process.** `AppServerSession::spawn`
    (`execution.rs:485`) uses `std::process::Command`. `Drop`
    (`execution.rs:702`) calls `child.kill()`, which stops only the direct
    child. Codex starts sandboxed commands as grandchildren, so these can
    continue to run after a timeout, a cancel or a shutdown.

    Add `process-wrap` 10 with the `std` feature. Spawn through
    `CommandWrap` + `ProcessGroup::leader()` on Unix, and `JobObject` on
    Windows. `kill()` on the wrapped child then stops the whole group.

    Add a test: a fake Codex script starts a `sleep` grandchild and writes
    its pid. Drop the session. The test must then find that the
    grandchild is gone.

19. **`serde_ignored` for unknown A2A fields.** The `validate_*_json`
    functions (`transport.rs:578-748`) walk the JSON to reject unknown keys.
    a2a-lf does not use `deny_unknown_fields`. Deserialize `Task`, `Message`
    and `TaskStatus` through `serde_ignored::deserialize`, and fail when a
    key is ignored.

    Keep `validate_part_json` and the checks on the wrapper keys. a2a-lf
    reads `Part`, `StreamResponse` and `SendMessageResponse` through a
    `HashMap<String, Value>`, so `serde_ignored` cannot see keys in them.
    `validate_part_json` also enforces "exactly one content field". The
    test `official_a2a_json_uses_supported_parts_and_rejects_unknown_fields`
    (line 1822) must pass without a change. Expected saving: 80–100 of the
    170 lines.

20. **Test harness, no new crate.** `tests/commands.rs` has 14 copies of the
    `Command::new("kill").args(["-INT", pid])` block and 9 places that split
    output on `"endpoint_id":"`. Add a `ServerProcess` guard:
    - `interrupt()` runs `kill -INT`.
    - `Drop` kills the process and waits for it.

    Thus a test that panics no longer leaves server processes. Parse the
    output lines with `serde_json::from_str::<Value>`. Replace the local
    `hex` and `seconds` functions with the phase 5 and phase 10 helpers.

21. **`assert_cmd`, `predicates` and `insta`** as dev-dependencies. Use
    `assert_cmd::Command` + `predicates` for the commands that run and
    stop. The long-running servers keep the phase 20 guard. Use `insta`
    snapshots for `usage_renders_help_and_a_machine_readable_spec`
    (`tests/commands.rs:74-157`). Commit the `.snap` files.

22. **`tokio::process` for Codex (go/no-go gate).** `AppServerSession` uses
    a reader thread and `mpsc::recv_timeout` (`execution.rs:485-655`).
    Replace them with `tokio::process::Command` + `FramedRead<_,
    LinesCodec>` + `timeout_at` + `kill_on_drop`. Keep the process group
    from phase 18: `process-wrap` has a `tokio1` feature.

    `CodexProcess` is a sync trait, and `proof.rs` calls it from its own
    threads. Thus this phase needs a runtime `Handle` in
    `CodexAppServer`, or it must wait for phase 23. **Gate:** do phase 22
    only if phase 23 is approved. Alone, it only moves the thread into a
    `block_on`.

23. **Scheduler with `CancellationToken` + `TaskTracker` (go/no-go gate).**
    `ProofService` (`proof.rs:194-863`) has these parts today:
    - A scheduler thread, woken by a `sync_channel`.
    - An `executing: AtomicBool` and an `execution_thread` slot.
    - A 20 ms sleep loop at shutdown (line 844).
    - `join_within` (line 1291), which starts a watcher thread.

    Replace them with `tokio_util::sync::CancellationToken` and
    `tokio_util::task::TaskTracker` (the `rt` feature is already on). The
    executor runs in `spawn_blocking`, and a panic arrives as a `JoinError`.
    Shutdown becomes `token.cancel(); tracker.close();
    timeout(deadline, tracker.wait())`.

    The public API of `ProofService` is sync (`submit`, `admit_with_replay`,
    `shutdown`), and the store tests call it without a runtime. Keep the
    API sync and hold a runtime `Handle`, or change the callers. Decide this
    before the phase starts.

    **Gate:** this phase rewrites code that had race conditions, and phases
    22–38 of the root `PLAN.md` fixed and tested that code. Do it only
    after phases 1–21 are complete, and only if you approve it.

**Correction during implementation (phases 22–23):** the plan assumed that
the callers of `ProofNode` have a tokio runtime `Handle`. Most do not: most
places that create a `ProofNode` are synchronous `#[test]` functions
(`proof.rs`, `node_service.rs`, `tests/e2e.rs`, the `node_config.rs` tests). A tokio scheduler would
therefore need a runtime owned by `ProofNode`. `main.rs` calls
`ProofNode::shutdown` from inside its async runtime, where dropping an owned
runtime panics, so shutdown would also need `shutdown_background` or a
separate thread. `CodexProcess` is a synchronous trait, so the executor
would still run on a blocking thread. The result is more code and new ways
to fail, not less code. So:

- Phase 22 is not done. Phase 18 already fixed the real defect (the process
  group). The reader thread and `recv_timeout` stay while `CodexProcess` is
  synchronous.
- Phase 23 is done only in the part that simplifies with `std`: shutdown now
  waits on the execution thread's `JoinHandle::is_finished()`. This removes
  `join_within`, its watcher thread and the second wait loop (`42ecb6b`). The
  scheduler thread, the wake channel and the `executing` flag stay.
- The full tokio rewrite stays possible. It first needs `CodexProcess` to
  become async and the tests to run inside a runtime. That is a separate
  decision.

### Out of scope

- The signed canonical encoding, a change from usage-rs to clap, the admin
  socket framing (commit bca8d12), the poll loop at `transport.rs:1468`, the
  `components/*.rs` registry copies, and the `admin.rs`
  `Change`/`ChangeRequest` duplication.
- `src/env.rs`. varlock generates it, and no crate compiles it. After
  phase 9, `main.rs` reads no environment variable directly. Whether to
  remove the varlock generation is a separate decision.
- The `admit_inner`/`submit_inner` duplication in `proof.rs:992-1138`. If
  phase 23 is done, it touches that code, so do that refactor there.

## Implementation Phases

### Phase 1: Escape JSON output

- Replace the eleven hand-built JSON `format!` strings with
  `serde_json::json!`.
- Add a test: `node serve` with an `--agent-id` that contains `"` and `\`
  writes valid JSON with the same id.
  **Commit:** `fix(cli): write JSON events with serde_json so values are escaped`

### Phase 2: Percent-encode ids in client URLs

- Build `record_url` and `scope_url` with `path_segments_mut()`, and reject
  `.` and `..`.
- Add unit tests for ids that contain `?`, `#`, `%`, a space and `..`.
  **Commit:** `fix(clients): percent-encode record and scope ids in request URLs`

### Phase 3: Create the TLS key owner-only

- Write the key through a `create_new` + `mode(0o600)` temporary file and
  `rename`. Delete `restrict_key_permissions`.
- Add a Unix test: after `tls_material`, the key mode is `0o600`. Also
  test the case where the key exists and the certificate does not.
  **Commit:** `fix(control): create the TLS key file owner-only instead of chmod after write`

### Phase 4: Remove dead transport code

- Delete `serve_once`, `ReplayWindow` and the sync `encode`/`decode`.
- Move the frame-validation tests onto `validate_order`/`validate_payloads`
  or onto the codec.
  **Commit:** `refactor(transport): remove unused serve_once, ReplayWindow and sync frame codec`

### Phase 5: Use the hex crate

- Replace the `node_config.rs` decoder, the six `hex()` copies and the
  `decode(..).try_into()` sites.
  **Commit:** `refactor: use the hex crate for all hex encoding and decoding`

### Phase 6: Decode hex fields through serde

- Enable `hex/serde`. Annotate the `WireRecord`, `WireInvitation` and
  `GrantFile` byte fields. Check the HTTP status for a bad record.
  **Commit:** `refactor: decode hex fields in wire and grant files through hex::serde`

### Phase 7: Sync handler service

- Change `HandlerService` to `BoxCloneSyncService`. Remove the `Mutex`.
  **Commit:** `refactor(transport): hold the request handler as BoxCloneSyncService`

### Phase 8: topcoat TowerService

- Replace `TopcoatService` with `TowerService` + `map_response`.
  **Commit:** `refactor(admin-ui): serve the topcoat router through TowerService`

### Phase 9: CLI declarations

- Declare `env`, `default`, `requires` and `group`. Remove the manual reads
  and checks. Update the help and spec tests.
  **Commit:** `refactor(cli): declare environment variables and flag rules in the usage spec`

### Phase 10: Unix time helpers

- Add `aether_core::time`. Replace the named helpers and the inline
  conversions. Keep `append_time`.
  **Commit:** `refactor(core): convert Unix seconds through one pair of helpers`

### Phase 11: Key-file loader

- Add `key_file.rs`. Use it in `transport` and `discovery`. Make the
  example call `discovery`.
  **Commit:** `refactor: load and create 32-byte key files through one owner-only loader`

### Phase 12: Pinned HTTPS client

- Add the shared builder. Use it in both clients.
  **Commit:** `refactor(clients): build pinned HTTPS clients through one helper`

### Phase 13: Scope adapter

- Replace the three plugin files with `ScopeAdapter`.
  **Commit:** `refactor(plugins): use one scope adapter for every scope kind`

### Phase 14: Row mappers

- Add one mapping function each for `ManagedMember`, `SignedRecord`,
  `PendingChange` and `TaskEvent`.
  **Commit:** `refactor(store): map each row type through one function`

### Phase 15: TOML config

- Add `toml` as a direct dependency. Parse `ConfigFile`. Update the tests.
  **Commit:** `refactor(cli): parse the configuration file with toml`

### Phase 16: Enum strings

- Derive `strum` on the store enums. Add `as_str`/`FromStr` on the core
  enums. Delete the copies. Add the string-contract test.
  **Commit:** `refactor: derive store enum strings with strum and keep one mapping for core enums`

### Phase 17: Constant-time bearer

- Add `subtle` as a dev-dependency. Use `ct_eq` in `hermes_bridge.rs`.
  **Commit:** `fix(examples): compare the bridge bearer token in constant time`

### Phase 18: Process group

- Add `process-wrap` (`std`). Spawn Codex as a group leader. Add the
  grandchild test.
  **Commit:** `fix(execution): stop the whole Codex process group, not only the direct child`

### Phase 19: serde_ignored

- Add `serde_ignored`. Replace the task, message and status walkers.
  **Commit:** `refactor(transport): detect unknown A2A fields with serde_ignored`

### Phase 20: Test process guard

- Add `ServerProcess`. Replace the 14 kill blocks and the 9 string splits.
  **Commit:** `test(commands): guard server processes and parse JSON output`

### Phase 21: Test crates

- Add `assert_cmd`, `predicates` and `insta`. Convert the short commands and
  the help and spec test.
  **Commit:** `test(commands): assert commands with assert_cmd and snapshot help with insta`

### Phase 22: tokio::process (gated on phase 23)

- Move `AppServerSession` to `tokio::process` + `LinesCodec` +
  `process-wrap` `tokio1`.
  **Commit:** `refactor(execution): drive Codex app-server through tokio::process`

### Phase 23: Scheduler (gated)

- Replace the scheduler thread, `executing`, `execution_thread`, the
  sleep loop and `join_within` with `CancellationToken` + `TaskTracker`.
  **Commit:** `refactor(proof): supervise execution with CancellationToken and TaskTracker`

## Risks & Tradeoffs

- **Changed error text and exit codes (phases 6, 9 and 15).** usage-rs,
  serde and toml write their own messages. Mitigation: keep the exit codes,
  and check the HTTP status in phase 6 before the commit.
- **Test validation coverage (phase 4).** The deleted `encode` was the
  entry point for the validator tests. Mitigation: move each assertion,
  and do not delete it.
- **Enum strings are a database contract (phase 16).** A wrong
  `serialize_all` breaks the existing rows. Mitigation: an explicit
  variant-to-string test, and a test that reopens an existing store.
- **Signed bytes (phase 10).** Mitigation: `append_time` stays unchanged,
  and the existing signature tests must pass without a change.
- **`serde_ignored` coverage is partial (phase 19).** Mitigation: keep the
  part and wrapper checks, and keep the unknown-field test unchanged.
- **Windows (phase 18).** `JobObject` is used on Windows, and CI runs Unix
  only. It is compiled but not tested.
- **Phases 22–23 are the largest risk.** They rewrite supervision code that
  took many phases to make correct. The gate exists for this reason. The
  saving is approximately 150 lines.
- **Plugin files (phase 13).** The three identical files can be a
  placeholder for different behavior later. The `match` in `middleware()`
  keeps that option.

## Decisions (24 September 2026)

- The 0.2.2 release change is committed (`0b8e536`), with the README text in
  its own commit (`5e22084`).
- Phases 22–23 are approved.
- Phase 6: a change of the HTTP status for a bad record to 422 is
  acceptable.

## Open Questions

- Phase 23: keep `ProofService` sync with a runtime `Handle`, or make its
  API async? Default: keep it sync, because that changes the fewest callers.
- `src/env.rs`: remove the varlock Rust generation after phase 9?
