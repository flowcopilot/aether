# Hermes Agent and Aether: how to integrate them

Checked against the Hermes Agent source at commit
[`e6f5742`](https://github.com/NousResearch/hermes-agent/tree/e6f57427c198cfd96854522b7a9159eb007e13c2)
(committed 2026-10-17) on 2026-10-18. Hermes Agent is an MIT-licensed Python
personal agent by Nous Research. I read the source and the docs in the
repository (`website/docs/`). I did not install or run Hermes.

Aether was checked at commit `4eee4fc` of this repository. The existing Hermes
work is in `integrations/hermes-plugin/`, `integrations/hermes-bridge-README.md`
and `crates/aether/examples/hermes_bridge.rs`.

In this document, `H` links point to Hermes files at the commit above. Aether
links are relative to this repository.

**Status (2026-10-18): implemented.** The line numbers below refer to
`4eee4fc`, before the change. What was built:

- `execution::Agent` is the one seam. `Limits` holds only the node limits;
  `agent::codex::CodexLimits` holds the Codex settings.
- `agent::codex` (the old Codex code) and `agent::connected` (the agent
  socket) are the two adapters, and `agent::NodeAgent` selects one.
- The policy field `adapter` (`codex` or `connected`) and the
  `node serve --adapter` flag, which saves the choice.
- `integrations/hermes-plugin/` is the new `aether` plugin: the `aether`
  platform (receive) and seven send tools over `aether mcp serve`.
- The old bridge and its example are removed.

Not built yet: the outbox does not retry a task that fails because no agent is
connected, the node does not merge progress events, and the grant's access
mode does not reach the adapter.

## Result

1. **Hermes is one more agent on the Aether network.** Like every Aether
   agent, it must send tasks to other agents and receive tasks from them.
   Send and receive are not two options. They are the two halves of one
   integration.
2. **Ship it as one Hermes plugin, named `aether`.** The bundled Hermes
   `a2a` plugin is the model. It registers an inbound platform adapter and
   an outbound toolset from one `register(ctx)`
   ([a2a/__init__.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/__init__.py#L91-L97)). The
   `aether` plugin does the same:
   - **Receive:** `ctx.register_platform("aether", ...)`. Aether becomes a
     gateway platform for Hermes, like Telegram or Discord. Each admitted
     Aether task arrives as a message in the owner's Hermes gateway, and the
     reply goes back as the task result.
   - **Send:** `ctx.register_tool(...)` for the caller calls: find agents,
     find people, send a task, read the inbox.
3. **The Aether node keeps the network and the trust rules.** Iroh, the hub,
   contacts, invites, grants, PACT, the outbox and the audit log stay in
   `aether node serve`. The plugin talks only to the local node. Hermes
   never holds an Aether key and never sees a grant.
4. **The node must be agent-neutral, and the sender agent-blind.** Today
   the node hands every admitted task to `codex app-server` through the
   `CodexProcess` trait ([execution.rs](../crates/aether/src/execution.rs#L96-L117)),
   and Codex ideas (sandbox, approval policy, `CODEX_HOME`, token count) are
   in the node core. The change is one `Agent` seam with adapters. Codex is
   one built-in adapter. A local agent connection lets any agent attach, and
   the Hermes plugin is its first client. Nothing on the wire says which
   agent answers (see [Agent-neutral receiver](#agent-neutral-receiver)).
5. **Remove the old plugin and bridge.** The new plugin replaces them. They
   skip contacts, hub names, invites and the outbox, they change a signed
   grant into one shared bearer token, and the plugin cannot read its own
   secret (see [Existing plugin and bridge](#existing-plugin-and-bridge)).
6. **Privacy follows the current product rule.** A personal agent works with
   the MCP servers and setup that its owner runs. To give remote people less,
   the owner runs a second Hermes profile with fewer tools, as a second
   agent. The plugin must give each Aether task its own gateway session, and
   it must not send the Hermes setup (model, tools, MCP names) to callers.

## Design: one Hermes plugin

### Shape

```text
other agents  <--Iroh/hub-->  aether node serve  <--local socket-->  Hermes gateway
                                (trust, outbox)                       plugin "aether"
                                                                       - platform: receive
                                                                       - tools: send
```

The owner runs `aether node serve` and `hermes gateway` on the same
computer. The plugin connects to the node through a Unix socket in the node
data directory, as `aether mcp serve` and the admin UI already do with the
admin socket ([admin_socket.rs](../crates/aether/src/admin_socket.rs)).

### Receive: the `aether` platform adapter

The adapter follows the `a2a` adapter
([adapter.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/adapter.py)):

1. `connect()` opens the node socket and says "this agent is ready"
   ([a2a connect](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/adapter.py#L295)).
2. For each admitted task, the adapter builds a `MessageEvent` with
   `chat_id` = the Aether task or context ID and `user_id` = the caller's
   hub name, and gives it to `handle_message`
   ([a2a inbound](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/adapter.py#L575-L576)).
3. Hermes runs the turn in that chat's session, with the owner's model,
   tools and MCP servers.
4. `send(chat_id, content)` returns the final text to the node as the task
   result ([a2a send](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/adapter.py#L829-L836)).
   Progress events become Aether `progress` events.
5. When the node cancels a task, the adapter stops that turn.

A short platform hint, like the `a2a` hint
([a2a/__init__.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/__init__.py#L12-L22)), tells
the model that the text comes from another person's agent, that it is
untrusted, and that it must not name its tools, model or files.

Hermes lets a plugin platform set `trusted_inbound`, which skips the
gateway allowlist and pairing
([platform_registry.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/gateway/platform_registry.py#L114),
[platform_registry.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/gateway/platform_registry.py#L177)). This fits,
because the node has already checked the caller's grant. If the plugin does
not set it, the owner must also add callers to a Hermes allowlist, which
repeats the Aether access list.

### Send: the `aether` toolset

The tools match the caller tools of `aether mcp serve`
([mcp.rs](../crates/aether/src/mcp.rs#L1326-L1362)):
`aether_agents_find`, `aether_people_find`, `aether_task_send`,
`aether_task_cancel`, `aether_inbox_list`, `aether_inbox_read`.
They call the node, so hub names, contacts, the outbox and offline delivery
work as they do for Codex. `aether_task_send` waits up to 20 seconds and then
returns `waiting`; the answer comes later in the inbox.

Owner tools (invites, access, groups, settings) stay out of the plugin. The
owner uses the Aether admin UI or the owner MCP tools for them.

A user who wants only to send can use `aether mcp serve` as a Hermes MCP
server now, with no plugin
([mcp.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/website/docs/user-guide/features/mcp.md#L482-L506)). The
plugin is still the target, because only a plugin can receive.

### Aether changes

The Hermes plugin is the first agent that is not Codex, but it must not be a
second special case. Codex and Hermes are two agents among many. Some agents
take a plugin (Hermes), some are a process that the node starts (Codex
`app-server`), and others will come in other packages. The node must accept
all of them through one seam, and the sender must not know or care which
agent answers. See [Agent-neutral receiver](#agent-neutral-receiver) for the
full review of the code. In short:

1. **One agent seam in the node.** Replace `CodexProcess` with an
   agent-neutral `Agent` trait. Codex becomes one adapter behind it.
2. **Agent connection protocol.** A local, framed protocol on a Unix socket
   in the data directory, owner-only file mode, like the admin socket:
   - node to agent: `task` (ID, context ID, caller hub name, text,
     instructions), `cancel`;
   - agent to node: `ready`, `progress`, `result`, `failed`.
   The node sends only the text, the caller's name and the privacy
   instructions. It does not send keys, grants or the endpoint ID. Any agent
   that can open a Unix socket can use it. The Hermes plugin is the first
   client.
3. **Choose the agent per data directory**, in the policy file (see below).
   One agent for each data directory fits "a second agent with its own data
   directory" in [configuration.md](../docs/configuration.md).
4. **Offline agent.** When no agent is connected, the node reports the agent
   as not available. The caller's outbox then keeps the task and retries, as
   for an offline node.
5. **Caller calls on the same socket**, or a small client of the existing
   MCP and admin paths, so the plugin needs no MCP subprocess.

## Agent-neutral receiver

This section reviews the Aether code at `4eee4fc` for places that assume the
receiving agent is Codex. Two rules guide it:

- **The node supports many agents.** Codex, Hermes and later agents are
  adapters. Each adapter can come in its own package: built into the node, a
  plugin for the agent, or a separate program.
- **The sender is agent-blind.** Nothing that leaves the node tells the
  caller which agent, model or tools answer. The caller sees the Agent Card,
  task states and the final text, and nothing more.

### What is already agent-blind

- The Agent Card names no model, sandbox or tool
  ([agent_card.rs](../crates/aether/src/agent_card.rs#L4-L5)).
- Hub listings carry only `AgentKind` = Personal or Service
  (`hub_client.rs`).
- Task events on the wire carry only an A2A status and a sequence number.
  `progress` and `approval_requested` both become `Working`
  ([node_service.rs](../crates/aether/src/node_service.rs#L357-L372),
  [node_service.rs](../crates/aether/src/node_service.rs#L417)).
- A failed task carries no reason text, so no executor error leaks.
- PACT receipts name the action `aether.<capability>`
  ([pact/gateway.rs](../crates/aether/src/pact/gateway.rs#L920)).
- The owner MCP tools, the admin socket and `Change` do not edit Codex
  files.

Keep these properties. Add a test for each wire shape that checks that no
adapter field (agent kind, model, thread ID) is in it. This is a current
privacy contract, so a negative test is correct here.

### Where the node is tied to Codex

| Area | Now | Change |
| --- | --- | --- |
| Executor seam | `CodexProcess` with `start_turn`, `start_turn_with_events`, `cancel`, `set_workspace` ([execution.rs](../crates/aether/src/execution.rs#L96-L117)). `Executor<P: CodexProcess>` and `NodeService<_, CodexAppServer>` ([node_config.rs](../crates/aether/src/node_config.rs#L242)). | Trait `Agent` with `run(task, events)` and `cancel(task_id)`. `NodeService` holds a `Box<dyn Agent>` (or an enum of adapters), chosen from the policy file at start. |
| Codex code | JSON-RPC, `thread/start`, `turn/start`, `turn/interrupt`, `final_answer_text`, disabled features, task folder permission profiles, all in `execution.rs`. | Move to `agent/codex.rs`. `execution.rs` keeps only `Task`, `AgentEvent`, the trait and the `Executor` checks. |
| Events | `ExecutionEvent { Progress, ApprovalRequested, Completed }` ([execution.rs](../crates/aether/src/execution.rs#L79)). `ApprovalRequested` is a Codex idea. | Neutral `AgentEvent { Progress, Completed(text), Failed }`. Each adapter handles its own approvals (Codex denies them) and reports them as `Progress` if it wants. |
| Limits | `Limits { sandbox, approval, allow_network }` accepts only Codex values (`read-only`, `workspace-write`, `task-folder`, `never`, …) ([execution.rs](../crates/aether/src/execution.rs#L18-L52)). | Neutral limits stay in the node: workspace, access mode from the grant, context budget. Sandbox, approval policy, network and environment allowlist move to the Codex adapter config. |
| Workspace | `set_workspace` on the trait. The doc comment names `CodexAppServer`. The grant's `access_mode` (ReadOnly/ReadWrite) is never given to the executor ([transport.rs](../crates/aether/src/transport.rs#L116-L122)). | Put the workspace and the access mode in each `Task`. Each adapter maps them: Codex to `sandboxPolicy`; an agent that cannot limit itself must refuse `ReadWrite` grants or report "not supported". |
| Privacy instructions | `developer_instructions` builds the "do not name tools, MCP servers, model" text inside the Codex code ([execution.rs](../crates/aether/src/execution.rs#L391)). | Build the text in the node and put it in `Task.instructions`. Every adapter must give it to its agent (Codex: `developerInstructions`; Hermes: platform hint). |
| Context budget | PACT and `token_count` use Codex's "one token per three characters" ([pact/gateway.rs](../crates/aether/src/pact/gateway.rs#L1223-L1230)). | Count the budget in characters in the node. An adapter can give a better count later. |
| CLI and env | `--codex-bin` / `AETHER_CODEX_BIN` ([main.rs](../crates/aether/src/main.rs#L100-L102)), the Codex login warning ([main.rs](../crates/aether/src/main.rs#L692-L696)). | Agent choice in the policy file. Each adapter has a `check()` that the node calls at start, so the login warning becomes the Codex `check()`. Keep `--codex-bin` as a Codex adapter setting. |
| Codex home | `codex_home`, `prepare_codex_home`, `codex_environment`, the reserved `CODEX_*` variables ([node_config.rs](../crates/aether/src/node_config.rs#L650-L706)). | Move to the Codex adapter. |
| Tests | Fakes named `FakeCodex`, `CountingCodex`, `BlockingCodex` ([tests/e2e.rs](../crates/aether/tests/e2e.rs#L52-L89)) and fake Codex shell scripts in `fixture.rs` and `tests/commands.rs`. The e2e scripts grep `"fake Codex result"`. | Node tests use one `FakeAgent` on the `Agent` trait. The fake Codex scripts test only the Codex adapter. Add one adapter contract test (run, progress, cancel, fail, offline) that each adapter must pass. |
| Docs | README "Today, Aether runs only Codex", `docs/configuration.md` "Real Codex", `docs/development.md`, `references/a2a-codex-connection-flow.md`, `references/aether-security-model.md`. | Describe the node and the seam in neutral words. Put each agent's setup in its own page (`docs/agents/codex.md`, `docs/agents/hermes.md`). |

The policy file then has one agent block for each data directory, for
example:

```toml
[agent]
kind = "codex"            # or "connected"

[agent.codex]             # only for kind = "codex"
binary = "codex"
sandbox = "task-folder"
allow_network = false

[agent.connected]         # only for kind = "connected"
# no settings: the agent opens <data-dir>/agent.sock
```

### Adapter shapes and packages

| Shape | How the node reaches the agent | Package | Examples |
| --- | --- | --- | --- |
| Built-in process | The node starts the agent program and speaks its protocol. | Code in `crates/aether/src/agent/`. | Codex `app-server`. |
| Connected agent | The agent connects to `<data-dir>/agent.sock` and speaks the Aether agent protocol. | Code in the agent's own plugin system, in `integrations/<agent>/`. | Hermes plugin. Later, any agent with plugins. |
| Command bridge (later) | A small program in `integrations/<agent>/` that is a connected agent on one side and drives the agent's CLI or API on the other side. | Separate program. | Agents with no plugin system. |

Only the first shape adds Rust code for each agent. The connected shape
needs one Rust adapter (`ConnectedAgent`) for all agents. This keeps the node
small when more agents come.

The sender side does not change for any shape. A sender agent uses
`aether mcp serve` or a plugin's send tools, and these call the node the same
way for all receivers.

### Leaks to check in each adapter

- **Event timing.** Codex sends many deltas, Hermes may send one final text.
  The count and timing of `Working` frames can show the agent kind. The node
  should merge progress events, for example at most one in a fixed interval.
- **Answer text.** An agent can name itself in its answer. The privacy
  instructions ask it not to. This is a prompt rule, not a hard check.
- **Error text.** Keep failures without reason text on the wire. Write the
  adapter error only to the local audit log.
- **Timeouts.** Use the same node timeouts for all adapters, so a timeout
  does not show the agent kind.

### Plugin files

```text
integrations/hermes-plugin/
  plugin.yaml      # kind: platform, provides_tools, config: data_dir
  __init__.py      # register(ctx): register_platform + register_tools
  adapter.py       # AetherAdapter(BasePlatformAdapter)
  client.py        # node socket client (stdlib only)
  tools.py, schemas.py
  README.md        # install, plugins.enabled, gateway setup
```

Use only the standard library, like `a2a`. Read the data directory from
`ctx.get_config`. There is no secret: the socket file mode is the access
check. Add `check_fn` so the tools show only when the node socket exists.
Run `hermes plugins doctor <path> --ci` in CI
([plugins_cmd.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins_cmd.py#L1047-L1053)).

### Order of work

1. Agent seam: the `Agent` trait, `AgentEvent`, `Task.instructions`, the
   workspace and access mode in `Task`. Move the Codex code to
   `agent/codex.rs` with no change in behavior. Replace the Codex fakes in
   node tests with `FakeAgent`.
2. Policy file `[agent]` block, with `kind = "codex"` as the default, so
   current nodes keep working.
3. Agent connection protocol and `ConnectedAgent`, with the adapter contract
   test.
4. Plugin: platform adapter (receive).
5. Plugin: tools (send).
6. Docs per agent, then remove the old bridge and example.
7. Later: propose the plugin to the Hermes plugin catalog.

## A2A in Hermes

The bundled plugin `plugins/platforms/a2a` implements A2A v1.0 over HTTP
JSON-RPC, with only the standard library
([DESIGN.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/DESIGN.md)).

- **Outbound:** `a2a_discover`, `a2a_call`, `a2a_list`, `a2a_history`,
  `a2a_orchestrate`. `a2a_call` sends `SendMessage` with an optional static
  `Authorization: Bearer` token from `a2a_agents`
  ([tools.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/tools.py#L48-L49),
  [tools.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/tools.py#L95-L128)).
  The tools show only when A2A is configured
  ([tools.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/tools.py#L329-L361)).
- **Inbound:** an Agent Card and JSON-RPC methods. Tasks are "injected into
  your live agent session — the same agent that's talking to you, with full
  memory" ([README.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/README.md#L49-L54),
  [DESIGN.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugins/platforms/a2a/DESIGN.md#L50-L57)).
  Inbound text goes through filters that replace some patterns with
  `[filtered]`.

Fit with Aether:

- **Hermes calling an Aether node over A2A:** not possible today. Aether's
  peer protocol is A2A data over Iroh QUIC (`aether/a2a/0`), not HTTP
  ([aether-wire-profile.md](../references/aether-wire-profile.md)). The HTTP
  path is the PACT gateway. It needs a JWT from an issuer with a JWKS and an
  `x-a2a-user-delegation` token ([docs/pact.md](../docs/pact.md),
  [gateway.rs](../crates/aether/src/pact/gateway.rs#L45-L46)). Hermes sends
  neither.
- **Using the Hermes A2A server as the Aether receive path:** possible, but
  not recommended. It opens a second HTTP server with its own tokens, so it
  goes around the node's grants, hub names and outbox. It puts all A2A tasks
  into the live session, and its filters change the caller's text. The
  `aether` platform adapter uses the same gateway hooks without these
  problems.

A community catalog plugin, `hermes-iroh-interconnect`, sends Hermes tasks to
paired peers over Iroh QUIC. Its own entry says that "tasks from a paired peer
run in your live gateway agent with its full toolset"
([catalog entry](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/plugin-catalog/hermes-iroh-interconnect.yaml)).
It shows demand for the same idea. It has no hub, invites or per-person
grants.

## Existing plugin and bridge

### What they do

`hermes_bridge` is an HTTP server with one route, `POST /delegate`, behind a
bearer token. It builds one A2A request frame from the text, signs nothing
new, copies a fixed `authorization.json`, and sends the frame to one node
with `transport::request_once` or `request_once_via_relay`. It returns the
final state and the artifact text
([hermes_bridge.rs](../crates/aether/examples/hermes_bridge.rs#L76-L167)).
The plugin registers one tool, `aether_delegate_task`, that posts the text to
the bridge ([tools.py](../integrations/hermes-plugin/tools.py)).

So this is Hermes as a caller, through an HTTP hop.

### Does it match current Aether?

| Point | State |
| --- | --- |
| Builds | Yes. `cargo check --example hermes_bridge -p aether` passed on 2026-10-18. |
| Wire format | Matches. The bridge uses `text_request_payload`, `Frame` and `request_once`; `request_once` polls until the task ends ([transport.rs](../crates/aether/src/transport.rs#L1173-L1260)). The ALPN is still `aether/a2a/0`. |
| Hub protocol 2 | Not affected. The bridge does not use the hub. That is also the problem: it cannot use hub names such as `ana#0042`, hub invites, or presence. |
| Contacts and invites | Not used. It needs `--endpoint-id` and `--authorization` by hand. The current flow imports a connect code and sends by agent name ([README.md](../README.md#connect-to-another-person)). |
| Outbox | Not used. An offline host gives an error. `aether.task.send` keeps the task in the outbox and sends it later ([docs/usage.md](../docs/usage.md)). |
| Identity | It binds a new Iroh endpoint from the caller's `--data-dir`. If `aether node serve` runs on the same data directory, two processes use one endpoint key. I did not test this. |
| Security | Anyone with the one bearer token can spend the caller's grant. The bridge speaks plain HTTP and needs a TLS proxy for remote use. |

### Does it match the current Hermes plugin API?

| Point | State |
| --- | --- |
| `plugin.yaml` | Valid. `manifest_version: 2`, `provides_tools`, `python_dependencies` and `config_schema` are known fields ([plugins_manifest.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins_manifest.py#L130-L184)). `python_dependencies` is never installed by Hermes. `requests` is already a Hermes dependency. |
| `register(ctx)` and `ctx.register_tool(name, toolset, schema, handler)` | Valid ([plugins.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins.py#L454-L458)). Hermes calls `handler(args, **kwargs)`, so the `partial(..., ctx)` works ([registry.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/tools/registry.py#L936-L952)). |
| `ctx.get_config` | Exists. It reads `plugins.entries.<id>.settings.<key>` in `config.yaml` ([plugins.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins.py#L264-L274)). |
| `bearer_token: {type: secret}` | **Mismatch.** Hermes stores a `secret` field in `.env` (here as `AETHER_BEARER_TOKEN`), never in `config.yaml`, and refuses to write it there ([plugins_settings.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins_settings.py#L1-L10), [plugins_settings.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins_settings.py#L116-L131)). The handler reads only `config.yaml`. A token set in the Desktop or TUI settings form does not reach the handler. |
| Enabling | **Missing.** User plugins are off until listed in `plugins.enabled` ([plugins.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/website/docs/user-guide/features/plugins.md#L166-L168)). The README does not say this. |
| `hermes plugins doctor <path> --ci` | Exists ([plugins_cmd.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/plugins_cmd.py#L1047-L1053)). |
| `check_fn` | Missing. The tool shows even when the plugin has no token. The bundled A2A plugin hides its tools in that case. |

### Decision: replace

Replace `integrations/hermes-plugin/` with the new `aether` plugin. Remove
`integrations/hermes-bridge-README.md` and
`crates/aether/examples/hermes_bridge.rs`. Also remove the `subtle`
dev-dependency if nothing else uses it.

Reasons:

- The old plugin can only send, and only through an HTTP hop.
- The bridge works around the identity model. It turns a signed, per-person
  grant into a shared HTTP secret.
- The new plugin gets names, contacts, outbox, inbox and agent search from
  the node, and it can receive.

## Notes from the first pass

The first version of this document treated "Hermes runs inside the node in
place of Codex" as a separate direction. That is wrong: Hermes is its own
agent and does not use the Codex server. Some facts from that pass still
help:

- `hermes -z` and `hermes chat --oneshot` load memory and rules, and
  `-z` turns off approvals
  ([oneshot.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/hermes_cli/oneshot.py#L274-L276)). The plugin does not
  use them; the gateway runs the turns.
- Hermes says: "The only security boundary against an adversarial LLM is the
  operating system" ([SECURITY.md](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/SECURITY.md#L58-L119)). Docker with
  `docker_network: false` holds only shell and file tools. This is the same
  for the owner's Telegram or Discord messages today.
- `hermes acp` is a viewer of the gateway daemon
  ([gateway_server.py](https://github.com/NousResearch/hermes-agent/blob/e6f57427c198cfd96854522b7a9159eb007e13c2/acp_adapter/gateway_server.py#L1-L20)). The
  platform adapter is simpler, because the gateway already runs.

## Risks

| Risk | What to do |
| --- | --- |
| A caller's text enters a session with the owner's chat history | One gateway session for each Aether task or context. Never route Aether messages to the owner's home channel. |
| The owner's memory or tools reach a caller | The owner's choice, by product rule. Document "run a second Hermes profile with fewer MCP servers and its own Aether data directory". |
| A caller learns the Hermes setup | Platform hint forbids it. The node never puts Hermes names in the Agent Card or listings. |
| Answers from other agents act as instructions | `aether_task_send` and `aether_inbox_read` return answers as untrusted data, as the MCP tools do. |
| Remote text starts a slash command | Escape a leading `/` in the adapter. |
| Hermes asks for approval with no human present | Aether tasks run with no human step on the remote side. Find how the gateway handles approvals for a platform with no human, and fail the step, not the whole gateway. |
| The gateway is down | The node reports "not available"; the caller's outbox retries. |
| Another local process uses the socket | Owner-only file mode, as for the admin socket. |

## Open questions

1. How does the Hermes gateway handle a tool approval request on a platform
   with no human? Can the adapter answer "deny" for each request?
2. Does `trusted_inbound` on a non-bundled plugin platform need a review or
   a flag from the owner?
3. Can the adapter send progress events, or only the final text? The `a2a`
   adapter ignores non-final `send` calls.
4. How does the gateway map `chat_id` to sessions, and how long does it keep
   them? Can the adapter end a session when the task ends?
5. Should one Hermes gateway serve more than one Aether data directory (two
   agents), or one plugin instance for each profile?
6. Does the Aether audit log need the Hermes session ID, so the owner can
   find the turn?
7. Should Codex also move to the agent connection protocol later, so the
   node has one path for all agents?
8. What does an adapter do with a `ReadWrite` grant when its agent cannot
   limit file access to the workspace? Refuse the grant, or tell the owner
   when they make it?
9. Should the node merge progress events for all agents, so event timing
   does not show the agent kind?
