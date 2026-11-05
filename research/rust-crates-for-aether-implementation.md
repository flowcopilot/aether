# Rust crates for the Aether implementation

Review date: 2026-09-02

## Result

Aether can remove much hand-written protocol and server code. The first group is `uuid`, `blake3`, `tokio-util`, `tower`, and `governor`. These crates help with identifiers, private fingerprints, task control, time limits, concurrency limits, and peer rate limits.

The second group replaces large custom surfaces. It is `a2a-lf`, `rmcp`, `axum`, `axum-server`, and `askama`. The maintenance group is `rusqlite_migration`, `thiserror`, `secrecy`, `tracing`, `tempfile`, `hex`, and `serde_path_to_error`.

Do not replace durable replay data with an in-memory cache. Do not change the signed data format now. Do not add an asynchronous SQLite pool before the task admission and lease operation is atomic.

Rust 1.98 can use all crates in the adopt list. Each crate publishes an MSRV that is not higher than Rust 1.98. `blake3` does not publish an MSRV, but Aether already builds it through Iroh.

## Critical implementation findings

Fix these problems before a larger crate migration.

1. Migration 003 removes database constraints. It uses `CREATE TABLE ... AS SELECT` for `task_events` and `execution_leases` ([migration 003](../crates/aether-store/migrations/node/003_task_identity.sql)). SQLite states that this form creates no primary key and no constraints ([SQLite `CREATE TABLE AS`](https://www.sqlite.org/lang_createtable.html#createtabas)). A local schema check confirmed that both migrated tables have no constraints. Replace both statements with complete table definitions. Add a migration test that checks the required primary keys, foreign keys, and `NOT NULL` constraints.

2. Replay protection is not active. `request_nonce` and `last_event_sequence` have no run-time consumer. `accept_replay_key` is only called by store tests ([store replay code](../crates/aether-store/src/lib.rs)). The node accepts a nonempty nonce but does not record it ([transport validation](../crates/aether/src/transport.rs)). Call the durable replay operation before task admission. Make its capacity limit separate for each caller and scope. The present SQL uses a global row count, so one peer can remove another peer's entries.

3. The command client uses fixed task, request, nonce, idempotency, and exchange identifiers ([request command](../crates/aether/src/main.rs)). Repeated requests can collide. Generate a new UUID for each new operation. Keep an idempotency key only when the caller explicitly retries the same operation.

4. The current `StoreWorker` can lease the wrong task. Two callers can interleave admission and `lease_next`. Each caller can then lease the other task. Both calls can fail and leave leases at `i64::MAX`. Put admission and lease claim in one store transaction. Give each lease a finite expiry. Call `recover_expired_leases` at startup.

5. An idempotent duplicate returns the stored task ID as completed result text. Task output is not stored ([node service result mapping](../crates/aether/src/node_service.rs)). Store the result or return the existing task state. Do not present a task ID as agent output.

6. Discovery is not connected to node operation. `aether control` prints `startup_log` and exits. Signed discovery records include their own public key. This proves record integrity, but it does not establish trust in that key ([discovery code](../crates/aether/src/discovery.rs)). Pin an invited key or verify a trusted directory statement before membership becomes trusted.

7. The administration surface is not a live configuration surface. Changes only write `admin.<kind>.<id>` keys. They do not update the live node. The UI agent, task, and connection values are placeholders. Define one typed configuration service. The UI and MCP server must call that service.

8. The HTTP server reads one 8 KiB buffer and serves TLS connections in sequence ([admin UI server](../crates/aether/src/admin_ui.rs)). It does not provide a complete HTTP parser. A slow local client can stop other administration requests. Replace this server before the UI gets more functions.

9. The MCP server implements JSON-RPC and MCP messages by hand ([MCP server](../crates/aether/src/mcp.rs)). This makes protocol version changes and schema checks repetitive. Replace it with the official SDK.

10. The request fingerprint includes the complete remote task text. This stores private input twice and calls it a fingerprint. Encode the exact identity fields, add a domain tag, and store a BLAKE3 digest.

11. Task events do not form a live subscription. The service waits for execution to end. It then writes the collected events. `events_after` exists in the store, but the network does not use it. The administration event response is one snapshot, not a continuous stream. Keep SQLite as the durable source. Use a bounded Tokio broadcast channel only as a wake-up signal. Resume from the last stored sequence after a lost connection.

12. Network stage time limits are incomplete. Connection accept and connect have time limits. The handshake, bidirectional stream accept, request read, response read, and write operations do not. A peer can hold a connection permit without progress ([transport server](../crates/aether/src/transport.rs)). Add a time limit to each stage. Add a per-peer request limit. Return a stable overloaded error when the protocol state permits it.

13. Spawned connection tasks are not tracked. The node has no coordinated stop operation for those tasks. Use one cancellation token and one task tracker for node-owned work. Stop admission first. Then close the router and wait for tracked tasks.

14. Transport error frames exist, but the server does not send them for handler failures. A denied or overloaded request usually becomes a closed stream. Map internal failures to small protocol errors. Do not send internal text to a peer.

15. The administration socket removes an existing socket path without a live-server check ([administration socket](../crates/aether/src/admin_socket.rs)). A second process can remove the path of the first process. Probe the socket before removal. Use a process lock or an owner record. A crate is not necessary for this fix.

## Crate decisions

| Area | Crate and current release | License | Published MSRV | Decision |
|---|---|---:|---:|---|
| A2A data model | [`a2a-lf` 0.3.0](https://docs.rs/a2a-lf/0.3.0/a2a/) | Apache-2.0 | 1.85 | Adopt the core types. Defer its HTTP server and client. |
| MCP | [`rmcp` 3.2.0](https://docs.rs/rmcp/3.2.0/rmcp/) | Apache-2.0 | 1.88 | Adopt. Use the small server, macro, schema, and standard input feature set. |
| SQLite migrations | [`rusqlite_migration` 2.6.0](https://docs.rs/rusqlite_migration/2.6.0/rusqlite_migration/) | Apache-2.0 | 1.95 | Adopt after repair of migration 003. |
| HTTP server | [`axum` 0.8.9](https://docs.rs/axum/0.8.9/axum/) | MIT | 1.80 | Adopt. |
| TLS server | [`axum-server` 0.8.0](https://docs.rs/axum-server/0.8.0/axum_server/) | MIT | 1.82 | Adopt with `tls-rustls-no-provider`. |
| HTML templates | [`askama` 0.16.0](https://docs.rs/askama/0.16.0/askama/) | MIT OR Apache-2.0 | 1.83 | Adopt. Replace manual HTML and escaping. |
| Framing and task control | [`tokio-util` 0.7.19](https://docs.rs/tokio-util/0.7.19/tokio_util/) | MIT | 1.71 | Adopt as a direct dependency with `codec` and `rt`. It is already transitive through Iroh. |
| Service limits | [`tower` 0.5.3](https://docs.rs/tower/0.5.3/tower/) | MIT | 1.64 | Adopt after the request handler becomes asynchronous. Use concurrency, load-shed, and time-limit layers. |
| Peer rate limits | [`governor` 0.10.4](https://docs.rs/governor/0.10.4/governor/) | MIT | 1.85 | Adopt for keyed limits by authenticated peer. Keep queue limits in SQLite. |
| Errors | [`thiserror` 2.0.20](https://docs.rs/thiserror/2.0.20/thiserror/) | MIT OR Apache-2.0 | 1.71 | Adopt as a direct dependency. It is already transitive. |
| Secrets in memory | [`secrecy` 0.10.3](https://docs.rs/secrecy/0.10.3/secrecy/) | MIT OR Apache-2.0 | 1.60 | Adopt for secret strings and tokens. |
| Fingerprints | [`blake3` 1.8.7](https://docs.rs/blake3/1.8.7/blake3/) | CC0-1.0 OR Apache-2.0 variants | Not published | Adopt as a direct dependency. It is already transitive through Iroh. |
| Operation identifiers | [`uuid` 1.26.0](https://docs.rs/uuid/1.26.0/uuid/) | MIT OR Apache-2.0 | 1.85 | Adopt with `v7` and `serde`. It is already transitive. |
| Structured logs | [`tracing` 0.1.44](https://docs.rs/tracing/0.1.44/tracing/) | MIT | 1.65 | Adopt with `tracing-subscriber`. Use spans for peer, scope, exchange, and task identifiers. |
| Test paths | [`tempfile` 3.27.0](https://docs.rs/tempfile/3.27.0/tempfile/) | MIT OR Apache-2.0 | 1.63 | Adopt as a development dependency. Replace manual temporary path creation. |
| Hex data | [`hex` 0.4.3](https://docs.rs/hex/0.4.3/hex/) | MIT OR Apache-2.0 | Not published | Adopt. Replace the repeated key and signature codecs. |
| Configuration errors | [`serde_path_to_error` 0.1.20](https://docs.rs/serde_path_to_error/0.1.20/serde_path_to_error/) | MIT OR Apache-2.0 | 1.61 | Adopt. Report the exact invalid configuration field. |

The A2A Rust repository has current releases and supports Rust 1.85 or later ([official A2A Rust SDK](https://github.com/a2aproject/a2a-rs), [release list](https://github.com/a2aproject/a2a-rs/releases)). The official RMCP repository reports full conformance for the 2025-11-25 and 2026-07-28 protocol suites ([RMCP roadmap](https://github.com/modelcontextprotocol/rust-sdk/blob/main/ROADMAP.md)). These are strong maintenance signals.

## Signed data and key files

Keep `ed25519-dalek` and the present versioned binary signature input. The signed objects contain fixed fields, strings, and integers. A small internal encoder is sufficient. Move the duplicate length-prefix code from `identity.rs` and `discovery.rs` into one module. Add fixed test vectors that another language can use.

Defer [`serde_json_canonicalizer` 0.3.2](https://docs.rs/serde_json_canonicalizer/0.3.2/serde_json_canonicalizer/). It implements RFC 8785, but a change to JSON canonical form changes the signed protocol. Its MIT license is suitable, but it does not publish an MSRV. Use it only if Aether makes signed JSON a public, language-neutral format. Add a new signature version if that change occurs.

Keep file-based node keys for the proof of concept. The endpoint key path uses exclusive creation, `O_NOFOLLOW`, and Unix mode 0600. Extract this logic into one secret-file module. Use it for the administrator TLS key too. Write the certificate and key as one recoverable operation.

Defer [`keyring` 4.2.0](https://docs.rs/keyring/4.2.0/keyring/) for node identity keys. It is active, dual licensed, and has MSRV 1.88. However, an operating-system key store can be unavailable to a headless service. It also does not define Aether file recovery.

Use `secrecy` for configuration tokens and administrator change values. It prevents normal `Debug` output and requires an explicit secret exposure operation ([`secrecy` documentation](https://docs.rs/secrecy/0.10.3/secrecy/)). It does not set file permissions and does not lock memory. Keep file and log controls.

## SQLite

Adopt `rusqlite_migration`. Version 2.6.0 uses `rusqlite` 0.40 and matches Aether's `rusqlite` 0.40.2. It uses `PRAGMA user_version`, checks a migration list, and applies migrations in a transaction ([migration API](https://docs.rs/rusqlite_migration/2.6.0/rusqlite_migration/struct.Migrations.html)). Plan a one-time conversion from the current `schema_migrations` table.

Keep the current `rusqlite` online backup code. Migration management does not replace backup or restore tests. Keep Aether's semantic idempotency checks too. A crate cannot decide whether two agent requests have the same meaning.

Defer asynchronous SQLite crates until the store has atomic domain operations:

- [`tokio-rusqlite` 0.7.0](https://docs.rs/tokio-rusqlite/0.7.0/tokio_rusqlite/) uses `rusqlite` 0.37. It is not compatible with Aether's current SQLite dependency. Its design also uses a background thread and messages, which is close to the present `StoreWorker`.
- [`deadpool-sqlite` 0.14.0](https://docs.rs/deadpool-sqlite/0.14.0/deadpool_sqlite/) now uses `rusqlite` 0.40 and has MSRV 1.95. It is compatible. Defer it because a pool does not fix the admission and lease transaction race. Reconsider it when concurrent database reads are necessary.

Do not use [`moka` 0.12.16](https://docs.rs/moka/0.12.16/moka/) as the replay authority. Moka provides bounded in-memory caches and expiry. Replay state must survive a node restart. Moka is already transitive and can be a read cache later.

## A2A data and Iroh transport

Adopt the `a2a-lf` core crate for messages, tasks, parts, states, artifacts, events, errors, and wire-compatible Serde forms. Remove the local copies of these types from `transport.rs`. Keep an Aether envelope for scope, authorization, replay, and Iroh connection identity.

Defer `a2a-server-lf` and `a2a-client-lf`. They provide HTTP, REST, JSON-RPC, gRPC, and event-stream bindings ([A2A Rust workspace](https://github.com/a2aproject/a2a-rs#workspace)). Aether uses an Iroh binding. The A2A specification permits a custom binding, but that binding must keep the core operations and semantics and must document its rules ([A2A custom bindings](https://github.com/a2aproject/A2A/blob/main/docs/topics/custom-protocol-bindings.md)). The current request and cancel subset must not claim full A2A conformance.

Use [`LengthDelimitedCodec`](https://docs.rs/tokio-util/0.7.19/tokio_util/codec/length_delimited/struct.LengthDelimitedCodec.html) with an explicit `max_frame_length`. It uses a four-byte big-endian length by default. This matches the present frame prefix. The limit is checked before a full frame allocation. Keep the total exchange byte limit and frame-count limit in the Aether state machine.

Use Iroh's existing [`Router`](https://docs.rs/iroh/1.1.0/iroh/protocol/struct.Router.html) for the protocol accept loop. It routes by ALPN and provides a clean shutdown operation. Use `tokio-util` [`CancellationToken`](https://docs.rs/tokio-util/0.7.19/tokio_util/sync/struct.CancellationToken.html) and [`TaskTracker`](https://docs.rs/tokio-util/0.7.19/tokio_util/task/task_tracker/struct.TaskTracker.html) for node-wide shutdown and for work that is not owned by the Iroh router.

## Administration HTTP and TLS

Replace the raw HTTP parser and serial TLS loop with Axum. Axum provides typed routing, extractors, body handling, and Tower middleware ([Axum documentation](https://docs.rs/axum/0.8.9/axum/)). Use request body limits, time limits, and a connection limit. Keep the present loopback bind, host check, origin check, CSRF token, secure cookie, and local confirmation rules. Axum does not supply those Aether policy rules.

Use `axum-server` for the Rustls accept loop and graceful shutdown. Its `tls-rustls-no-provider` feature lets Aether keep its explicit Rustls crypto provider ([Axum Server features](https://docs.rs/crate/axum-server/0.8.0/features)). Keep `rustls` and `rcgen` for TLS and local certificate generation.

Use Askama for server-side pages and fragments. Keep Topcoat as the CSS layer. Leptos is not necessary for this small local interface. Askama removes manual HTML assembly and provides automatic escaping.

Use Axum server-sent events for live views. The event body must remain a stream. Add keep-alive messages. Use the SQLite event sequence for recovery after a reconnect ([Axum server-sent events](https://docs.rs/axum/0.8.9/axum/response/sse/)).

## MCP

Replace the manual MCP code with `rmcp`. Use features `server`, `macros`, `schemars`, and `transport-io`. RMCP provides typed MCP messages, generated tool schemas, standard input transport, protocol negotiation, and server handlers ([RMCP documentation](https://docs.rs/rmcp/3.2.0/rmcp/)). Its current feature list permits this small server set ([RMCP features](https://docs.rs/crate/rmcp/3.2.0/features)).

Keep Aether's administration service behind RMCP. The tool handler must call the same typed service as the local UI. High-risk changes must still require local confirmation. RMCP handles protocol rules, not Aether authorization.

## Configuration, errors, and redaction

Keep Serde `deny_unknown_fields`. Replace string-based enum parsers with typed Serde enums. Use small newtypes for byte counts, token counts, durations, endpoint IDs, and scope IDs. Keep cross-field checks in `NodeConfig::validate`, because these checks include trust and file-system rules.

Defer [`garde` 0.23.0](https://docs.rs/garde/0.23.0/garde/). It is active, dual licensed, and has MSRV 1.87. Its derive checks can help when the configuration model becomes larger. At present, typed Serde values and one explicit validation function are easier to audit.

Adopt `thiserror` for `StoreError`, `TransportError`, `AdminError`, and `ExecutionError`. The crate derives the standard `Error` implementation without changing a public API ([`thiserror` documentation](https://docs.rs/thiserror/2.0.20/thiserror/)). Preserve error sources inside the node. Map them to small public errors at the network, UI, and MCP boundaries.

No error crate supplies secret redaction. Do not put task text, grants, tokens, keys, or administrator change values in error variants. Use `secrecy` for secret fields. Log stable identifiers and error classes only.

Use `tracing` for node logs. Replace manual JSON `println!` calls with typed fields and spans. Do not record prompts, secrets, or full grants. Use `serde_path_to_error` for configuration input. Use `hex` for key and signature text.

Tests create many paths under the operating-system temporary directory. Some names only contain the process identifier. Parallel tests can use the same path. Use `tempfile::TempDir` and keep its guard for the full test. This also removes repeated cleanup code.

## Implementation order

1. Repair migration 003. Add schema constraint checks.
2. Make replay acceptance active. Make admission and lease claim atomic. Add finite lease recovery.
3. Add stage time limits, keyed peer limits, and typed remote errors. Track all node tasks.
4. Add `uuid` and `blake3`. Remove fixed IDs and plaintext fingerprints.
5. Add `a2a-lf`. Keep the Aether Iroh envelope and write custom binding documentation.
6. Add `tokio-util` framing and cancellation. Use the Iroh router.
7. Add durable event subscriptions. Use SQLite for replay and Tokio only for wake-up.
8. Add Axum, Axum Server, and Askama. Preserve all local administration security checks.
9. Add RMCP. Route UI and MCP changes through one live configuration service.
10. Add `thiserror`, `secrecy`, `tracing`, `tempfile`, `hex`, and `serde_path_to_error` as part of the related changes. Do not do a separate repository-wide rewrite.

## Verification

`mise run check` passed on 2026-09-02. It ran formatting, Clippy, build checks, and tests. The result was 89 passed tests and one ignored live Codex test. This run did not prove live peer discovery, a physical relay, administrator live reconfiguration, or lease recovery.
