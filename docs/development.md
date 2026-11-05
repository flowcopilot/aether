# Development and proof checks

Install the pinned toolchain and run the local proof checks:

```sh
mise install
mise run check
mise run features
```

Each product feature is a Gherkin file in `features/` and is also a test.
See `features/README.md`.

With the fake Codex executor, `mise run check`, `cargo test -p aether --lib`,
`cargo test -p aether --test features`, and `cargo test -p aether-store` pass. The request CLI returns a
nonzero exit status for permission denial, protocol errors, failed tasks,
cancelled work, unknown results, and expired retained output.

The node tests use test agents on the `execution::Agent` seam, not Codex.
The Codex adapter tests (`agent::codex`) use fake `codex app-server` scripts,
and the connected adapter tests (`agent::connected` and
the "Connected agents" feature) use a test agent on the agent
socket. `mise run openclaw-plugin-test` runs the OpenClaw plugin tests with
Node alone. `mise run hermes-plugin-test` runs the Hermes plugin tests. Its
adapter tests need Hermes Agent on `PYTHONPATH`; without it they are skipped:

```sh
PYTHONPATH=<hermes-agent checkout> <hermes venv>/bin/python \
  -m unittest discover -s integrations/hermes-plugin/tests
```

Two tests use the real Codex binary. They run only with
`AETHER_RUN_LIVE_CODEX=1`. The first needs only the `codex` binary. The
second also needs a configured Codex account and model quota:

```sh
AETHER_RUN_LIVE_CODEX=1 cargo test -p aether --lib live_app_server_lists_the_agent_mcp_servers -- --ignored
AETHER_RUN_LIVE_CODEX=1 AETHER_LIVE_FORBIDDEN_FILE=$HOME/.profile \
  cargo test -p aether --lib live_task_folder_denies_a_home_file -- --ignored
```

## Model checks

`specs/` holds TLA+ models of the node protocols. `mise run tla` checks
each `specs/<Name>.cfg` with TLC; `mise run check` runs it too. The script
downloads a pinned `tla2tools.jar` into `target/tla/` and checks its hash.
Check one model with `sh scripts/tla.sh ExecutionLease`. The models do not
read the code: when you change a function that a model names, change the
model in the same commit. [specs/README.md](https://github.com/flowcopilot/aether/blob/main/specs/README.md) lists the
models and what each one checks.

The proof binary has three process modes:

```sh
cargo run -p aether -- node
cargo run -p aether -- node endpoint --data-dir ./.aether/client
cargo run -p aether -- node fixture --data-dir ./.aether/fixture
cargo run -p aether -- control --port 8443
cargo run -p aether -- mcp
cargo run -p aether -- mcp serve
cargo run -p aether -- node admin --data-dir ./.aether/node --port 3000
cargo run -p aether -- mcp serve --socket /path/to/node/admin.sock
```

Start the UI with a live node by adding `--admin-port`. The node writes an
`admin_listening` event with its local HTTPS URL.

```sh
AETHER_CODEX_BIN=/tmp/aether-proof/fake-codex \
  cargo run -p aether -- node serve \
  --policy /tmp/aether-proof/policy.json \
  --data-dir /tmp/aether-proof/server --port 4242 --admin-port 0
```

## More proof commands

Use `aether --help` for command help. Use `aether __usage_spec__` to print the
machine-readable Usage specification.

An imported JSON policy can list trusted issuer public
keys and signed grants; the node's own issuer key is always trusted. `node
request` requires a JSON authorization file, except for a direct contact
imported from an invite. `node
fixture` creates short-lived test files and a fake Codex App Server under its
data directory. Do not use fixture credentials for a real node.

A policy can use one `scope` object or a `scopes` list. Each grant must name a
scope in that list. One running node can admit direct, directory, and managed
tasks when its policy lists those scopes. A managed control connection requires
exactly one managed scope.

Run this local proof in two terminals. The first command creates the signed
test files. The server prints its endpoint ID when it is ready. Create a direct
invitation from the server data directory and import it into the client data
directory before you send the task.

```sh
cargo run -p aether -- node fixture --data-dir /tmp/aether-proof
AETHER_CODEX_BIN=/tmp/aether-proof/fake-codex \
  cargo run -p aether -- node serve \
  --policy /tmp/aether-proof/policy.json \
  --data-dir /tmp/aether-proof/server --port 4242

cargo run -p aether -- node invite \
  --data-dir /tmp/aether-proof/server --agent-id server --expires-in 600 \
  > /tmp/aether-direct-invitation.json
```

```sh
cargo run -p aether -- node direct \
  --data-dir /tmp/aether-proof/client \
  import /tmp/aether-direct-invitation.json

cargo run -p aether -- node request \
  --data-dir /tmp/aether-proof/client \
  --agent-id server --address 127.0.0.1:4242 \
  --scope direct --text review \
  --authorization /tmp/aether-proof/authorization.json
```

Test the local HTTPS directory in two terminals. The control process creates
its certificate at `./.aether/control/control-cert.der`.

```sh
cargo run -p aether -- control --port 8443
```

```sh
cargo run -p aether -- control test-record > /tmp/aether-record.json
jq -r .signingKey /tmp/aether-record.json > /tmp/aether-directory-key.hex

cargo run -p aether -- node directory \
  --directory https://127.0.0.1:8443 \
  --directory-certificate ./.aether/control/control-cert.der \
  --agent-verifying-key /tmp/aether-directory-key.hex \
  publish /tmp/aether-record.json

cargo run -p aether -- node directory \
  --directory https://127.0.0.1:8443 \
  --directory-certificate ./.aether/control/control-cert.der \
  --agent-verifying-key /tmp/aether-directory-key.hex \
  resolve test-agent
```

Use the same command with `renew /tmp/aether-record.json`,
`search test`, or `revoke /tmp/aether-record.json`. `control test-record`
creates public test data. Do not use its signing key for a real node. A
directory client pins both the HTTPS certificate and the record signing key.

Use `sync` to save ordered directory changes in a node data directory. The
command resumes from its durable cursor. A revocation keeps the record as stale
local data and prevents its use for a new task.

```sh
cargo run -p aether -- node directory \
  --directory https://127.0.0.1:8443 \
  --directory-certificate ./.aether/control/control-cert.der \
  --agent-verifying-key /tmp/aether-directory-key.hex \
  sync --data-dir ./.aether/client
```

Test a managed invitation with the same control service. First create an
invitation and save its `token` output in a private file.

```sh
cargo run -p aether -- control invite --scope managed --expires-in 600
```

Create a signed member record, then redeem the token over the HTTPS control
service. The token is read from a file so it does not appear in the command
arguments.

```sh
cargo run -p aether -- node record \
  --data-dir ./.aether/managed-client \
  --agent-id managed-client \
  --address 127.0.0.1:9443 \
  --scope managed --capability review --expires-in 600 > /tmp/aether-member.json

cargo run -p aether -- node managed \
  --control https://127.0.0.1:8443 \
  --control-certificate ./.aether/control/control-cert.der \
  redeem --token-file /tmp/aether-token.txt /tmp/aether-member.json

cargo run -p aether -- node managed \
  --control https://127.0.0.1:8443 \
  --control-certificate ./.aether/control/control-cert.der \
  members managed

cargo run -p aether -- node managed \
  --control https://127.0.0.1:8443 \
  --control-certificate ./.aether/control/control-cert.der \
  sync managed --data-dir ./.aether/managed-client
```

Managed task requests use the roster member endpoint through the relay. Set
`AETHER_RELAY_URL`, then use `node request --managed-control <url>
--managed-control-certificate <certificate> --agent-id <member>` with the
managed scope and normal authorization file. A request uses cached active
roster data only when the pinned control service is unavailable.

Create a record for a running node with its data directory and bound Iroh
address. The command creates a separate owner-only `discovery.key` file and
prints only the signed public record.

```sh
cargo run -p aether -- node record \
  --data-dir /tmp/aether-proof/server \
  --agent-id server --address 127.0.0.1:4242 \
  --scope directory --capability review --expires-in 3600 \
  > /tmp/aether-live-record.json
```

Publish this file with `node directory publish`. Give a caller the
`signingKey` value from the record as its pinned agent verification key.

Build the container images and run the direct and relay checks:

```sh
docker build -f Dockerfile.e2e -t aether-e2e:local .
docker build -f Dockerfile.relay -t iroh-relay:local .
sh scripts/container-e2e.sh
sh scripts/relay-e2e.sh
sh scripts/managed-relay-e2e.sh
```

The current proof verifies scoped policy, SQLite task recovery, direct and
directory discovery, managed membership, bounded A2A frames over local Iroh
endpoints, and the local execution boundary. The local Iroh checks need UDP
socket access.

The direct container test starts one server and one client container. The relay
test puts them on separate Docker networks. Only the relay container joins both
networks, and the client uses a relay-only endpoint address. Each test creates
short-lived signed fixtures and asserts one `fake Codex result` response.

The OpenClaw test runs the host node, OpenClaw with the `aether` plugin and a
mock model in one container, and sends one task from a client node. It checks
that the plugin loads, that the answer is the mock answer, that the `aether`
agent gets no `aether__` tool, and that the main agent gets only the seven
caller tools of `aether mcp serve --caller-only`. Set `OPENROUTER_API_KEY` and
`E2E_MODEL` (for example `openrouter/nvidia/nemotron-3.5-lightning:free`, a
free model) to use a real model; the key goes
to the container only as an environment variable:

```sh
docker build -f Dockerfile.e2e -t aether-e2e:local .
docker build -f integrations/openclaw-plugin/e2e/Dockerfile \
  -t aether-openclaw-e2e:local integrations/openclaw-plugin
sh scripts/openclaw-e2e.sh
```

The managed relay test also runs the pinned HTTPS control service. It redeems
one managed invitation for the caller and one for the server. The client and
server stay on separate Docker networks. They use only the control service and
the relay across those networks.

## Measured results, 14 September 2026

| Check | Command | Result |
| --- | --- | --- |
| Local checks | `mise run check` | Format, lints, build, and 191 tests pass. One live Codex test stays ignored. |
| Direct container path | `sh scripts/container-e2e.sh` | One task returns `fake Codex result` between two containers. |
| Relay path | `sh scripts/relay-e2e.sh` | The same task returns `fake Codex result` with the client and the server on separate Docker networks. |
| Managed relay path | `sh scripts/managed-relay-e2e.sh` | Two redeemed managed invitations, one pinned HTTPS roster, and the same `fake Codex result`. |

The Docker-based rows above were not rerun on 17 September 2026 (nothing
changed that would affect them); only the local checks were re-measured:
`mise run check` still passes — format, lints, build, and 223 tests, one
live Codex test still ignored. The growth from 191 covers work done after
14 September: approved-workspace-ID resolution and a durable policy
revision, and, at the CLI-process level specifically, `replay`
(an idempotent retry reusing a request's own key), `cancelled` (an
administration cancellation reaching a request in flight), `expired-result`
(a real 24-hour retention window fast-forwarded by calling the same
cleanup function the server itself uses, with an explicit far-future
cutoff instead of waiting), `unknown-result` (a real lease recovered the
same way, immediately, instead of waiting on `LEASE_TTL` and
`MAINTENANCE_INTERVAL`), and `overload` (a genuine three-request race made
deterministic once the scheduler's single execution slot was confirmed in
code, not assumed).

These results record checks from September 2026. They do not establish
the test coverage or behavior of the current version.

Limits in the code: 1 MiB per frame, 4 MiB per exchange, 64 frames per
exchange, 32 concurrent Iroh connections, 8 requests per second for each
authenticated peer, a 20-second handshake limit, a 10-second stream limit, and
a 30-second handler limit. The local UI accepts 32 connections, an 8 KiB form
body, a 5-second TLS handshake, and a 10-second request. The administration
socket accepts a 64 KiB message.

The [two-VM direct-path runbook](https://github.com/flowcopilot/aether/blob/main/references/aether-vm-runbook.md) gives a
separate-VM test. It does not replace a physical two-computer test.

The proof includes a local HTTPS directory service. It does not include a
public directory deployment, offline delivery, or a physical two-computer run.
