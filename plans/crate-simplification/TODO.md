# TODO: Replace hand-written code with crates, and fix the audit problems

## Prerequisite

- [x] Commit the 0.2.2 release change (`0b8e536`)

## Phase 1: Escape JSON output

- [x] `main.rs` lines 543, 742, 748, 895, 1107, 1684, 1844, 1865: use `serde_json::json!`
- [x] `main.rs:1107`: embed `response` as JSON, not as a string
- [x] `lib.rs` `startup_log`, `admin_ui.rs:757`, `control_service.rs:493`: use `serde_json::json!`
- [x] Test: `--agent-id` with `"` and a space gives valid JSON with the same id (`directory_discovery_completes_a_signed_node_task`). `\` is covered by the phase 2 unit test: before phase 2, the URL parser read `\` as `/`
- [x] `mise run check` passes
- [x] Commit: `fix(cli): write JSON events with serde_json so values are escaped` (`683bbd5`)

## Phase 2: Percent-encode ids in client URLs

- [x] `DirectoryClient::record_url`: `path_segments_mut()`, reject `.` and `..`
- [x] `ManagedClient::scope_url`: `path_segments_mut()`, reject `.` and `..`
- [x] Unit tests for `?`, `#`, `%`, space, `\`, `.` and `..` in ids
- [x] `mise run check` passes
- [x] Commit: `fix(clients): percent-encode record and scope ids in request URLs` (`bd5c2ac`)

## Phase 3: Create the TLS key owner-only

- [x] `tls_material`: temporary file with `create_new` + `mode(0o600)`, `sync_all`, `rename` (new `key_file::write_owner_only`)
- [x] Delete `restrict_key_permissions` (Unix and non-Unix), in `control_service.rs` and also in `admin_ui.rs`, which had the same problem
- [x] Unix test: key mode is `0o600`
- [x] Test: key exists and certificate is missing → new material is written
- [x] `mise run check` passes
- [x] Commit: `fix(control): create the TLS key file owner-only instead of chmod after write` (`8020c75`)

## Phase 4: Remove dead transport code

- [x] Delete `serve_once`
- [x] Delete `ReplayWindow` and the `Default` impl; keep the `bind` assertions (test renamed `binding_fails_closed`)
- [x] Delete sync `encode`/`decode`, and the `FrameTooLarge` and `TooManyFrames` variants that nothing produces any more
- [x] Move each assertion onto the codec: test-only `encode`/`decode` call `write_frames`/`read_frames`. The oversized length prefix now asserts `BadFrame`, which is what a live stream returns
- [x] `mise run check` passes
- [x] Commit: `refactor(transport): remove unused serve_once, ReplayWindow and sync frame codec` (`696a2fe`)

## Phase 5: Use the hex crate

- [x] `node_config.rs`: delete `decode_hex`, `hex_value`, `decode_array`; use `hex` crate
- [x] Replace `fn hex` in `fixture.rs`, `node_config.rs` tests, `node_service.rs` tests, `tests/commands.rs`, `examples/sign_authorization.rs`
- [x] `admin_ui.rs` `confirmation_token`: `hex::encode`
- [x] `<[u8; 32]>::from_hex` in `transport.rs`, `managed_client.rs`, `control_service.rs`, `main.rs` (`hex` implements `FromHex` for fixed sizes only, not for a generic `N`)
- [x] Add `hex` as a dev-dependency if the integration tests need it — not needed: tests and examples use the package's own dependencies
- [x] `mise run check` passes
- [x] Commit: `refactor: use the hex crate for all hex encoding and decoding` (`fc3720c`)

## Phase 6: Decode hex fields through serde

- [x] Enable `hex` feature `serde`
- [x] `WireRecord`, `WireInvitation`: `#[serde(with = "hex::serde")]` on `signing_key` and `signature`
- [x] `GrantFile`: same on `signing_key`, `nonce`, `signature`
- [x] HTTP status for a bad hex record: 400 before, 422 after (approved). Test `a_record_with_bad_hex_is_rejected_before_the_handler`
- [x] Fix `main.rs` sites that still compiled with the wrong meaning: the published-record event now prints `hex::encode(signing_key)`, and the publication takes the key bytes directly
- [x] `mise run check` passes
- [x] Commit: `refactor: decode hex fields in wire and grant files through hex::serde` (`5d7d5bb`)

## Phase 7: Sync handler service

- [x] `HandlerService` = `BoxCloneSyncService`
- [x] `AetherProtocol.handler`: remove `Arc<Mutex<…>>` and the lock-error path
- [x] `mise run check` passes
- [x] Commit: `refactor(transport): hold the request handler as BoxCloneSyncService` (`7de7182`)

## Phase 8: topcoat TowerService

- [x] Delete `TopcoatService`; use `TowerService::new` wrapped in `MapResponse::new(…, into_axum_response)`
- [x] `mise run check` passes
- [x] Commit: `refactor(admin-ui): serve the topcoat router through TowerService` (`091ef80`)

## Phase 9: CLI declarations

- [x] `AETHER_CODEX_BIN` with `env` + `default = "codex"` (as the flag `--codex-bin`: in usage-rs, `env` works only on a flag)
- [x] `AETHER_RELAY_URL` with `env` on each command that reads it (`--relay-url` on `node serve` and `node request`)
- [x] Remove the `std::env::var` reads at `main.rs` 565, 698, 954, 987, 1172
- [x] `requires` on `--managed-control` / `--managed-control-certificate`; remove the "supplied together" arm. `node request` also declares `conflicts` with the other addressing flags, which replaces the manual check in `managed_request`
- [x] ~~`node request`: `group` in place of the six-value match~~ — not done: the match also selects the mode and gives each branch its values, which a group cannot do (see PLAN.md, phase 9)
- [x] Check the exit code for a `requires` failure is still 2 (checked with the built binary)
- [x] Update the help and spec test — no change needed; it passes
- [x] `mise run check` passes
- [x] Commit: `refactor(cli): declare environment variables and flag rules in the usage spec` (`09109ea`)
- [x] Extra: fix a flaky `admin_socket` test found during this phase (`c1bbc9e`)

## Phase 10: Unix time helpers

- [x] Add `aether_core::time::{unix_seconds, unix_timestamp, from_unix_seconds}` (std only; `unix_timestamp` gives the `i64` the store uses)
- [x] Replace `aether-store` `now`, `control_service` `unix_timestamp`, `main` `unix_timestamp`, `proof` `timestamp_after`, `transport` `time_from_seconds`, `node_config` `time`
- [x] Replace the four `seconds()` test helpers and the inline calls in `node_service.rs` and the `Wire*` conversions (also `admin.rs`, `admin_ui.rs`, `identity.rs`, `managed_client.rs`, `tests/e2e.rs`)
- [x] Do not change `identity.rs` or `discovery.rs` `append_time`
- [x] `mise run check` passes
- [x] Commit: `refactor(core): convert Unix seconds through one pair of helpers` (`9d3b472`)

## Phase 11: Key-file loader

- [x] Add `crates/aether/src/key_file.rs` with `load_or_create`
- [x] `transport::load_or_create_key` uses it; keep `Key`/`Io` mapping
- [x] `discovery::load_or_create_signing_key` uses it; keep `Unavailable`
- [x] `examples/sign_authorization.rs` calls `discovery::load_or_create_signing_key` after `create_dir_all`
- [x] Tests: create then reload, wrong size, symbolic link
- [x] `mise run check` passes
- [x] Commit: `refactor: load and create 32-byte key files through one owner-only loader` (`8ef3341`)

## Phase 12: Pinned HTTPS client

- [x] Add the shared origin check and client builder (`pinned_https.rs`, which also holds the URL builder and the id segment rule from phase 2)
- [x] `DirectoryClient` and `ManagedClient` use it
- [x] `mise run check` passes
- [x] Commit: `refactor(clients): build pinned HTTPS clients through one helper` (`cf2507e`)

## Phase 13: Scope adapter

- [x] Replace `plugins/direct.rs`, `directory.rs`, `managed.rs` with `ScopeAdapter`
- [x] Keep the `match` on `ScopeKind` in `middleware()`
- [x] `mise run check` passes
- [x] Commit: `refactor(plugins): use one scope adapter for every scope kind` (`decb9de`)

## Phase 14: Row mappers

- [x] One mapping function each for `ManagedMember`, `SignedRecord`, `PendingChange`, `TaskEvent`
- [x] Align the SELECT column lists — already the same; the managed-event query reads members at other positions and keeps its own mapping
- [x] `mise run check` passes
- [x] Commit: `refactor(store): map each row type through one function` (`f765aed`)

## Phase 15: TOML config

- [x] Add `toml` as a direct dependency (same major as in `Cargo.lock`)
- [x] `ConfigFile { data_dir }` with `deny_unknown_fields`; keep empty and `\0` checks
- [x] Update the `lib.rs` config tests (comments and quotes parse; unknown key and empty path refused)
- [x] `mise run check` passes
- [x] Commit: `refactor(cli): parse the configuration file with toml` (`cf54599`)

## Phase 16: Enum strings

- [x] `strum` derives on `TaskState`, `AuditActor`, `AuditAction`, `AuditResult`
- [x] `as_str` + `parse` (returns `Option`) by hand for `ScopeKind`, `AccessMode`, `ApprovalRule` in `aether-core`, through one small macro
- [x] Delete the copies in `node_config.rs`, `proof.rs`, `fixture.rs`
- [x] Test: each variant maps to its current string (store and core)
- [x] `mise run check` passes
- [x] Commit: `refactor: derive store enum strings with strum and keep one mapping for core enums` (`82dc5e0`)

## Phase 17: Constant-time bearer

- [x] Add `subtle` as a dev-dependency
- [x] `hermes_bridge.rs:84`: `ct_eq`
- [x] `mise run check` passes
- [x] Commit: `fix(examples): compare the bridge bearer token in constant time` (`4c569e8`)

## Phase 18: Process group

- [x] Add `process-wrap` 10 with `std`
- [x] `AppServerSession::spawn`: `CommandWrap` + `ProcessGroup::leader()` (Unix), `JobObject` (Windows)
- [x] `Drop`: `start_kill` then `wait` on the wrapped child (separate calls, because `kill` skips the wait after an error)
- [x] Test: fake Codex starts a grandchild; after drop the grandchild is gone (checked that it fails without the group)
- [x] `mise run check` passes
- [x] Commit: `fix(execution): stop the whole Codex process group, not only the direct child` (`d8067a5`)

## Phase 19: serde_ignored

- [x] Add `serde_ignored`
- [x] Replace the task, message and status walkers (all walkers; `decode_payload` is now strict)
- [x] Keep the part check and the wrapper-key checks (`only_field`, `validate_parts`)
- [x] `official_a2a_json_uses_supported_parts_and_rejects_unknown_fields` passes (made async in phase 4, assertions unchanged); new test `unknown_fields_fail_at_every_depth`
- [x] `mise run check` passes
- [x] Commit: `refactor(transport): detect unknown A2A fields with serde_ignored` (`093c52b`)

## Phase 20: Test process guard

- [x] Add `ServerProcess` with `interrupt()` and a killing `Drop`
- [x] Replace the 14 `kill -INT` blocks
- [x] Replace the `"endpoint_id":"`, `"port":`, `"url":"` and `"signing_key":"` splits with `serde_json` parsing (`read_event`, `read_node_listening`)
- [x] `mise run check` passes
- [x] Commit: `test(commands): guard server processes and parse JSON output` (`082dbd8`)

## Phase 21: Test crates

- [x] Add `assert_cmd`, `predicates`, `insta` as dev-dependencies
- [x] Convert the short-lived command assertions
- [x] Snapshot the help pages and `__usage_spec__`; commit the `.snap` files (fixed `COLUMNS`, version replaced)
- [x] `mise run check` passes (also with `CI=1`)
- [x] Commit: `test(commands): assert commands with assert_cmd and snapshot help with insta` (`1b3afc9`)

## Phase 22: tokio::process (gated on phase 23)

- [x] Gate approved
- [x] ~~`AppServerSession` on `tokio::process`~~ — not done: no caller has a runtime and `CodexProcess` is synchronous, so it adds code (see PLAN.md, correction for phases 22–23)
- [x] ~~`process-wrap` `tokio1`~~ — not needed; phase 18 fixed the process group with `std`
- [x] Phase 18 grandchild test still passes

## Phase 23: Scheduler (gated)

- [x] Gate approved; sync-versus-async API decided: stays sync (most places that create a `ProofNode` are sync tests with no runtime)
- [x] ~~Replace the scheduler thread and wake channel~~ — kept (see PLAN.md)
- [x] Replace `join_within` and the separate 20 ms `executing` loop with a wait on `JoinHandle::is_finished()`; remove `ProofNode.executing`
- [x] A panic in the execution thread is still caught by `join` and does not stop the node (unchanged). Not changed and not tested here: after such a panic `finish_execution` does not run, so the task waits for lease recovery
- [x] `mise run check` passes; proof and node-service tests pass five runs in a row
- [x] Commit: `refactor(proof): wait for the last execution thread without a watcher thread` (`42ecb6b`)

## Verification

- [x] All existing tests pass (`mise run check`) after each phase. One pre-existing flaky test was found and fixed on the way (`c1bbc9e`)
- [x] New tests written: JSON escaping (phase 1), URL encoding (phase 2), key mode (phase 3), bad-hex status (phase 6), time helpers (phase 10), key loader (phase 11), TOML config (phase 15), enum string contract (phase 16), grandchild kill (phase 18), unknown fields at every depth (phase 19)
- [x] Smoke test: the command tests run the real binary for `node serve` with a directory and with a managed control, then `node request`, and parse every output line as JSON (phase 20). The container proof also printed valid `node_completed` JSON
- [x] Edge cases tested: agent id with `"` (phase 1); record id `..`, `a?b`, `\` (phase 2); stale `control-key.der.tmp` and key present with certificate missing (phase 3); `--codex-bin` default `codex` shown in the help snapshot (phase 21); only one managed-control flag gives exit 2 (checked with the built binary, phase 9)
- [x] No behavior change in phases 4, 5, 7, 8, 10–14, 19–21, with two notes: phase 4 changed only a test's expected error for an oversized length prefix, to the error the live codec already returned; phase 10 maps a time before 1970 in `main.rs` `unix_timestamp` to `i64::MAX` instead of 0, which `SystemTime::now()` cannot produce
- [x] No regressions in the signed bytes: `claim_bytes`, `grant_bytes`, `record_bytes`, `invitation_bytes`, `append_time` and their tests are unchanged since `b5e0429`
- [x] No regressions in the database: no migration changed, no store assertion removed, and the migration tests that open old databases pass
- [x] `mise run container-proof` (container, relay and managed-relay scripts) passes after phase 23

## Review

- [ ] Code reviewed — each phase was checked by me as it was written; an independent review is still open
- [x] PLAN.md updated if approach changed during implementation (phase 9, phases 22–23)
- [x] All phase commits are clean and describe their intent
- [ ] TODO.md items all checked off — all except the independent review
