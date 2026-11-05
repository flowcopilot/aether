# OpenClaw and Aether: how to integrate them

Checked against the OpenClaw source at commit
[`9b5c4e8`](https://github.com/openclaw/openclaw/tree/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87)
(committed 2026-10-30, package version `2026.10.9`) on 2026-10-30. OpenClaw
is an MIT-licensed personal AI agent in TypeScript on Node.js. Its earlier
names were Clawdbot and Moltbot
([lore.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/start/lore.md#L18-L32)).
I read the source and the docs in the repository (`docs/`, `extensions/`,
`src/`). I did not install, build or run OpenClaw.

Aether was checked at commit `6bd6901` of this repository. The model for
this document is [hermes-agent-integration.md](hermes-agent-integration.md)
and the Hermes plugin in `integrations/hermes-plugin/`.

In this document, OpenClaw links point to files at the commit above. Aether
links are relative to this repository. A claim marked **unverified** comes
from my reading only; I did not find a source line that proves it.

**Status (2026-10-30): implemented, not yet run against a live gateway.**
The plugin is in `integrations/openclaw-plugin/`, and `aether mcp serve`
has `--caller-only`. A second pass
on the same day read the OpenClaw source for four claims that were
unverified: the manifest `toolFilter`, inbound dispatch for a community
plugin, the `deliver` kinds, and session retention. The results are in the
sections below. The [Decisions](#decisions) section records the choices
for the remaining gaps.

## Result

1. **OpenClaw is one more agent on the Aether network.** As for Hermes, it
   must receive tasks and send tasks. These are the two halves of one
   integration.
2. **Ship it as one OpenClaw plugin, named `aether`.** The bundled OpenClaw
   `a2a` channel plugin is the model. It turns remote agent tasks into
   isolated channel sessions and turns the final reply into the task result
   ([inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L33-L157)).
   The `aether` plugin does the same:
   - **Receive:** a channel plugin with the channel ID `aether`. Its
     `gateway.startAccount` connects to `<data-dir>/agent.sock` and speaks
     the existing connected-agent protocol
     ([connected.rs](../crates/aether/src/agent/connected.rs#L1-L16)). Each
     task becomes one inbound message in its own session. The final reply
     becomes the task result.
   - **Send:** the plugin manifest declares `aether mcp serve` as an MCP
     server (`mcpServers`), limited to the caller tools. OpenClaw starts
     and owns the process
     ([surfaces.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest/surfaces.md#L417-L433)).
     The plugin needs no MCP client code, unlike the Hermes plugin.
3. **No Aether node change is necessary for the first version.** The node
   already has the `connected` adapter and the caller MCP tools. The plugin
   is the second client of `agent.sock`, after Hermes. One small node change
   is recommended: a caller-only mode for `aether mcp serve` (see
   [Aether changes](#aether-changes)).
4. **The node keeps the network and the trust rules.** Iroh, the hub,
   contacts, invites, grants, PACT, the outbox and the audit log stay in
   `aether node serve`. OpenClaw never holds an Aether key and never sees a
   grant or a caller name.
5. **Run Aether tasks in a dedicated OpenClaw agent.** OpenClaw supports one
   trust boundary for each gateway. "Everyone who can message a
   tool-enabled agent shares that agent's delegated tool authority"
   ([trust-model.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/security/trust-model.md#L13-L16)).
   Thus the owner binds the `aether` channel to a separate agent with its
   own workspace, a sandbox and a narrow tool list
   ([agent-bindings.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/agent-bindings.md#L26-L63),
   [tool-permissions.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/security/tool-permissions.md#L131-L178)).
   This is the OpenClaw form of "a second Hermes profile with fewer tools".
6. **Cancel is the main gap.** The OpenClaw `a2a` plugin refuses
   `CancelTask`, because "a dispatched agent run has no plugin API for
   stopping it"
   ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L123)).
   The `aether` plugin can stop waiting and report `cancelled`, but the
   OpenClaw run can continue. See [Cancel](#cancel).
7. **Do not use the OpenClaw HTTP or WebSocket APIs as the receive path.**
   The Gateway token gives full operator authority
   ([openai-http-api.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/openai-http-api.md#L40-L44)).
   Remote text would then run as the owner, not as untrusted channel input.
   See [Other paths](#other-paths-that-were-examined).

## What OpenClaw is

| Point | Fact | Source |
| --- | --- | --- |
| License | MIT, OpenClaw Foundation. | [LICENSE](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/LICENSE), [package.json](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/package.json#L28) |
| Language and runtime | TypeScript ESM on Node.js `>=24.16.0 <25 \|\| >=26.1.0`, pnpm workspace. | [package.json](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/package.json#L2531), [README.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/README.md) |
| Install and start | Install script, or `npm install -g openclaw`, then `openclaw onboard --install-daemon`. Config is JSON5 in `~/.openclaw/openclaw.json`. | [README.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/README.md), [configuration.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/configuration.md#L10) |
| Gateway | One long-lived daemon for each host. It owns all channels, sessions and tools. Clients (CLI, Control UI, apps) and device nodes connect over WebSocket, default `127.0.0.1:18789`. | [architecture.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/architecture.md#L8-L46) |
| Channels | Plugins that bring messages from WhatsApp, Telegram, Slack, Discord, Signal, iMessage and others. There are also agent-to-agent channels: `a2a` (A2A 1.0 over HTTP) and `reef` (end-to-end encrypted relay between agents of different people). | [docs/channels/](https://github.com/openclaw/openclaw/tree/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels), [reef.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/reef.md#L9) |
| Agents and bindings | A gateway can hold many agents, each with its own workspace, sandbox and tools. Bindings route a channel, account or peer to an agent. | [agent-bindings.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/agent-bindings.md#L11-L63) |
| Sessions | `session.dmScope` selects how direct messages map to sessions. The default `main` puts all direct messages in one main session. | [session.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/session.md#L131-L138) |
| Skills | Directories with a `SKILL.md` file. They load from workspace, user and plugin roots. ClawHub is the public catalog. A plugin manifest can ship skills (`skills`). | [skills.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/skills.md#L11-L49), [manifest.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest.md#L270) |

## Extension points

| Extension point | What it does | Use for Aether | Source |
| --- | --- | --- | --- |
| Channel plugin | A `ChannelPlugin` object: config, security, pairing, session grammar, outbound, gateway lifecycle. The manifest field `channels` marks the plugin as a channel owner. Core owns the shared `message` tool and dispatch. | **Receive.** The `aether` channel. | [sdk-channel-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L19-L55), [L107-L172](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L107-L172) |
| Channel inbound API | `channelRuntime.inbound.buildContext`, `dispatch` and `run`, and `ingress.resolveStable` for admission. | Turn each Aether task into one turn. | [sdk-channel-inbound.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-inbound.md#L11-L49), [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L44-L157) |
| Plugin MCP servers | Manifest `mcpServers`: static MCP server definitions that apply while the plugin is enabled. User `mcp.servers.<name>` can replace or disable them. | **Send.** `aether mcp serve`. | [manifest.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest.md#L258), [surfaces.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest/surfaces.md#L417-L433) |
| MCP client | OpenClaw connects to stdio, SSE and Streamable HTTP servers from `mcp.servers` or `openclaw mcp add`. `toolFilter.include` and `exclude` accept exact names and `*` globs. | Send without a plugin, now. | [mcp.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/mcp.md#L84-L131), [schema.help.agents.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/config/schema.help.agents.ts#L237-L242) |
| Tool plugin | `api.registerTool({ name, parameters, execute })`. Each tool must be in `contracts.tools`. | Fallback for send if the MCP path is not sufficient. | [building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L91-L165) |
| Plugin hooks | `api.on("before_tool_call", ...)` and others can block a tool or ask for approval. | Optional: block tools in Aether sessions. | [hooks.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/hooks.md#L12-L30), [L169](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/hooks.md#L169) |
| Skills | `SKILL.md` files, also from a plugin. | Optional: teach when to send an Aether task. | [skills.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/skills.md#L11-L49) |
| Gateway WebSocket API | Typed JSON frames: `connect`, then `req`/`res`/`event`. Methods include `agent`, `chat.send`, `chat.abort`, `sessions.abort`. Device pairing and gateway auth apply. | Not used (operator authority). | [architecture.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/architecture.md#L75-L117), [rpc-session-control.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/protocol/rpc-session-control.md#L112-L113) |
| OpenAI-compatible HTTP | `/v1/chat/completions`, off by default. Equal to full operator access. | Not used. | [openai-http-api.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/openai-http-api.md#L8-L44) |
| A2A channel | Bundled. A2A 1.0 JSON-RPC over HTTP, one bearer token for each peer. | Possible, not recommended (see below). | [a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md) |
| ACP | `openclaw acp` is an ACP server over stdio that forwards to the Gateway. ACP agents (`/acp spawn`) is the other direction: OpenClaw runs external harnesses. | Not used (operator authority). | [cli/acp.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/cli/acp.md#L9-L23), [acp-agents.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/acp-agents.md#L14-L27) |

All plugin APIs are experimental and "can change between OpenClaw
releases". A plugin must pin the OpenClaw version and test each version
that it declares compatible
([sdk-overview.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-overview.md#L25-L33)).

## Design: one OpenClaw plugin

### Shape

```text
other agents  <--Iroh/hub-->  aether node serve  <--agent.sock-->  OpenClaw gateway
                                (trust, outbox)  <--MCP stdio---   plugin "aether"
                                                                   - channel "aether": receive
                                                                   - mcpServers.aether: send
                                                                   agent "aether" (sandboxed)
```

The owner runs `aether node serve --adapter connected` and the OpenClaw
gateway on the same computer, as the same OS user. The plugin attaches to
the node socket. The socket file mode (`0600`) is the access check, as for
Hermes ([README.md](../integrations/hermes-plugin/README.md#protocol)).

### Hermes plugin piece and OpenClaw equivalent

| Hermes plugin | OpenClaw plugin | Source |
| --- | --- | --- |
| `plugin.yaml` (`kind: platform`, `provides_tools`, `config_schema`) | `package.json` with an `openclaw` block (`extensions`, `setupEntry`, `channel`), plus `openclaw.plugin.json` (`id`, `channels`, `channelConfigs`, `configSchema`, `mcpServers`) | [a2a package.json](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/package.json#L30-L73), [a2a openclaw.plugin.json](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/openclaw.plugin.json) |
| `register(ctx)` | `defineChannelPluginEntry({ id, plugin, registerFull })` | [sdk-channel-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L607-L647) |
| `ctx.register_platform("aether", ...)` | `createChatChannelPlugin({ base: { id: "aether", config, setup, gateway, messaging }, outbound })` | [a2a channel.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/channel.ts#L155-L183) |
| `AetherAdapter.connect()` and the reconnect thread | `gateway.startAccount(ctx)`: open the socket, reconnect, and return after `waitUntilAbort(ctx.abortSignal)` | [a2a channel.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/channel.ts#L243-L263), [L569-L593](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/channel.ts#L569-L593) |
| `MessageEvent` + `handle_message` | `channelRuntime.inbound.buildContext(...)` + `channelRuntime.inbound.dispatch(...)` | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L77-L157) |
| `chat_id` = task ID (one session for each task) | `resolveAgentRoute({ peer: { kind: "direct", id: <task ID> }, dmScope: "per-account-channel-peer" })` | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L33-L43) |
| `trusted_inbound=True`, `authorization_is_upstream` | `ingress.resolveStable({ dmPolicy: "allowlist", allowFrom: [<fixed sender>] })` with one fixed sender ID; the node has already checked the grant | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L44-L63) |
| `frame_task` (message starts with `[`, so no slash command) | Same framing, plus `extra: { CommandInterpretationSuppressed: true }` | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L97-L106) |
| `platform_hint` | `agentPrompt.inboundFormattingHints` for the channel | [sdk-channel-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L33-L41) |
| `send(..., metadata.notify)` → `result` | `delivery.deliver(payload, info)` with `info.kind === "final"` → `result` | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L134-L144) |
| other `send` → `progress` | `info.kind` is `"tool" \| "block" \| "final"`. `tool` and `block` → `progress` (no text, as the protocol requires). `onAgentRunTerminalOutcome("failed")` → `failed`. | [reply-dispatcher.types.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/auto-reply/reply/reply-dispatcher.types.ts#L9), [get-reply-options.types.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/auto-reply/get-reply-options.types.ts#L226), [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L150-L154) |
| `on_processing_complete(FAILURE)` → `failed` | `delivery.onError` and `replyOptions.onAgentRunTerminalOutcome("failed")` → `failed` | [a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L141-L155) |
| `cancel_session_processing` → `cancelled` | **No public plugin API.** See [Cancel](#cancel). | [a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L123) |
| Seven tools over `mcp_client.py` (`aether mcp serve`) | Manifest `mcpServers.aether` with `command: "aether"`, `args: ["mcp", "serve"]`; OpenClaw names them `aether__aether-task-send` and so on (**unverified** at run time; from the name rules) | [surfaces.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest/surfaces.md#L417-L433), [agent-bundle-mcp-names.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/agent-bundle-mcp-names.ts#L9-L12) |
| `check_fn` (tools show only when `aether` and the data directory exist) | MCP start retries with backoff, and `openclaw mcp doctor aether --probe` reports the state; channel status issues report a missing socket | [mcp.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/mcp.md#L36-L61) |
| `ctx.get_config("data_dir")` | `channels.aether.dataDir` (schema in `channelConfigs.aether`) | [sdk-channel-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L167-L172) |
| `plugins.enabled: [aether]` | `openclaw plugins install <spec>`, then `openclaw plugins enable aether`; optional `plugins.allow` | [manage-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manage-plugins.md#L228-L271) |
| `hermes plugins doctor <path> --ci` | `openclaw plugins inspect aether --runtime --json` | [building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L172-L178) |
| Python stdlib only | TypeScript ESM, `node:net` only, built to JavaScript before publish | [building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L193-L200) |

### Receive: the `aether` channel

1. `gateway.startAccount(ctx)` reads `channels.aether.dataDir` (default
   `$AETHER_DATA_DIR`, else `~/.aether/host`, as in
   [node.py](../integrations/hermes-plugin/node.py#L30-L35)). It connects to
   `agent.sock` and sends `{"type":"ready","protocol":1}`. When the socket
   closes, it tries again every 2 seconds until `ctx.abortSignal` aborts.
   It sets the channel status to ready or not connected.
2. For each `task` message, the plugin:
   - builds the message body as Hermes does: the untrusted-task marker, the
     node's `instructions`, then the task text;
   - resolves the route with peer ID = task ID and
     `dmScope: "per-account-channel-peer"`, as `a2a` does, so that a task
     never enters the owner's main session;
   - admits the turn with `ingress.resolveStable` and one fixed sender ID
     (for example `aether-node`) in `allowFrom`;
   - calls `inbound.dispatch` with `sourceReplyDeliveryMode: "automatic"`
     and the delivery callbacks in the table above.
3. When the turn ends with no final text, the plugin sends `result` with an
   empty text, as `a2a` completes a turn with no artifact
   ([inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L158-L167)).
4. The plugin sends no OpenClaw detail to the node: no model, agent ID,
   session key or error text. `failed` has no reason, as the protocol
   requires.

The outbound side of the channel (`outbound.sendText`) refuses all sends.
Aether has no "message to a task" after the task ends, and the agent sends
new tasks with the send tools.

**A community plugin can use this path (verified).** Every plugin gets the
same base channel runtime, with `inbound.dispatch` and
`ingress.resolveStable`
([runtime-channel.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/runtime/runtime-channel.ts#L277-L288)).
Only bundled and trusted official plugins get the host-bound ingress that
can attach participant evidence
([registry-runtime-channel.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/registry-runtime-channel.ts#L41-L48)).
The source says: "Community channels may admit guests; trusted provenance
is required only for owner elevation"
([registry-runtime.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/registry-runtime.ts#L582-L593)).
This fits Aether: a remote task must always be a guest, never the owner.
The plugin must never set `senderIsOwner`. A community channel starts only
when the owner enables it explicitly
([channel-startup-policy.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/channel-startup-policy.ts#L36-L49)).

### Sessions

A session for each task is necessary: the node sends no caller identity
([connected.rs](../crates/aether/src/agent/connected.rs#L14-L16)), so the
task ID is the only safe key. Two tasks must never share context.

OpenClaw does not remove these sessions (verified):

- With `per-account-channel-peer`, the key is
  `agent:<agent>:aether:<account>:direct:<task-id>`
  ([session-key.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/routing/session-key.ts#L207-L211)).
- Session maintenance treats a key with a channel delivery route as a
  "durable external conversation" and keeps it from age and count cleanup
  ([store-maintenance.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/config/sessions/store-maintenance.ts#L374-L417),
  [L446-L452](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/config/sessions/store-maintenance.ts#L446-L452),
  [session.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/concepts/session.md#L415-L438)).
  Only synthetic keys (cron, hook, ACP, sub-agent, heartbeat) are
  disposable
  ([store-maintenance-activity.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/config/sessions/store-maintenance-activity.ts#L29-L45)).
- The channel runtime has no API to delete or archive a session
  ([runtime-channel.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/runtime/runtime-channel.ts#L289-L301)).
  The owner can archive sessions with `openclaw sessions archive`
  ([sessions.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/cli/sessions.md#L176-L180)).

The bundled `a2a` channel uses the same key shape, with one session for
each peer and context
([a2a inbound.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/extensions/a2a/src/inbound.ts#L37-L42)),
so it has the same growth. See
[Decisions](#decisions).

### Send: `aether mcp serve`

`aether mcp serve` serves both caller tools and owner tools
(`aether.change`, `aether.access.revoke`, `aether.invites.*`,
`aether.settings.*` and others; see the `name = "aether.…"` entries in
[mcp.rs](../crates/aether/src/mcp.rs)). OpenClaw must not show the owner
tools to the agent. The manifest declares:

```json
"mcpServers": {
  "aether": {
    "transport": "stdio",
    "command": "aether",
    "args": ["mcp", "serve"],
    "toolFilter": {
      "include": [
        "aether.agents.list", "aether.agents.find", "aether.people.find",
        "aether.task.send", "aether.task.cancel",
        "aether.inbox.list", "aether.inbox.read"
      ]
    }
  }
}
```

Manifest entries are copied as they are
([manifest-capability-normalizers.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/manifest-capability-normalizers.ts#L186-L190)).
The manifest `toolFilter` applies (verified in the source):

- The loader copies each native plugin server entry with all its fields
  ([bundle-mcp.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/bundle-mcp.ts#L66-L85),
  [L507-L538](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/plugins/bundle-mcp.ts#L507-L538)).
- The tool catalog applies `rawServer.toolFilter` to every server, from
  config or from a plugin
  ([agent-bundle-mcp-catalog-projection.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/agent-bundle-mcp-catalog-projection.ts#L54-L68)).
- A user entry `mcp.servers.aether` replaces the full plugin entry by name.
  It is not a deep merge, so the plugin `toolFilter` is lost
  ([bundle-mcp-config.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/bundle-mcp-config.ts#L118-L127)).
  The README must tell owners to copy the `include` list into an override.
- Plugin MCP tools are loaded for the workspace, not for one agent. The
  agent tool policy (`tools.allow`, `tools.deny`, `tools.profile`) then
  filters them, with names `<safe-server>__<safe-tool>` and globs such as
  `aether__*`
  ([native-mcp-policy.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/native-mcp-policy.ts#L62-L68),
  [multi-agent-sandbox-tools.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/multi-agent-sandbox-tools.md#L261-L275)).
  In sandboxed sessions, MCP tools also need
  `tools.sandbox.tools.alsoAllow`
  ([sandbox-vs-tool-policy-vs-elevated.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/sandbox-vs-tool-policy-vs-elevated.md#L115)).

Thus the manifest filter is enough for the first version. Because an owner
override drops it, the caller-only mode of `aether mcp serve` (see
[Aether changes](#aether-changes)) is still the better long-term guard.

Stdio MCP servers get only `HOME`, `LOGNAME`, `PATH`, `TERM` and `USER`
from the gateway environment
([bundles.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/bundles.md#L140-L141)).
Thus `$AETHER_DATA_DIR` does not reach the server. The default
`~/.aether/host` works. For another data directory, the owner sets
`mcp.servers.aether` with `args: ["mcp", "serve", "--data-dir", "<dir>"]`;
user config replaces the plugin default
([surfaces.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manifest/surfaces.md#L433)).

A user who wants only to send can do it now, with no plugin:
`openclaw mcp add aether --command aether --arg mcp --arg serve`, plus an
include filter for the caller tools
([mcp.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/mcp.md#L84-L106)).

The `aether` agent (the one that answers remote tasks) must not get the
send tools, as the Hermes README says for the `aether` platform. Put
`aether__*` in that agent's `tools.deny`. Otherwise a remote caller can make
the owner's agent send tasks to other agents with the owner's grants.

### Cancel

The node sends `cancel` when the caller cancels or when the time limit
ends ([connected.rs](../crates/aether/src/agent/connected.rs#L291-L307)).
OpenClaw has no plugin API that stops a dispatched run for a third-party
channel:

- The `a2a` plugin refuses `CancelTask` for this reason
  ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L123)).
- `chat.abort` and `sessions.abort` are reachable from a plugin only inside
  an authenticated Gateway request (`dispatchGatewayMethod`), or through
  `api.runtime.gateway.request` for bundled or trusted official plugins
  ([gateway-and-nodes.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-runtime/gateway-and-nodes.md#L110-L130)).
  A task from the node socket is not a Gateway request.

First version: on `cancel`, the plugin sends `cancelled` at once and drops
the late reply. The OpenClaw run can continue and use tools until it ends.
The sandbox and the narrow tool list limit the effect. Ask OpenClaw for a
channel-scoped abort API (see [Open questions](#open-questions)).

### Security

OpenClaw's own model:

- "One trust boundary per gateway." Mutually untrusted users need separate
  gateways, and ideally separate OS users
  ([trust-model.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/security/trust-model.md#L11-L21)).
- Tools run on the host unless a sandbox is set. The sandbox is off by
  default; `mode` is `off`, `non-main` or `all`; backends are Docker,
  Podman, SSH, OpenShell and Crabbox. "This is not a perfect security
  boundary"
  ([sandboxing.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/sandboxing.md#L9-L13),
  [modes-scope-and-backend.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/sandboxing/modes-scope-and-backend.md#L11-L23)).
- Tool allow and deny lists apply before the sandbox. `tools.elevated` is
  an escape hatch that runs `exec` outside the sandbox
  ([sandboxing.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/sandboxing.md#L62-L66)).
- Session tools see all sessions by default (`tools.sessions.visibility:
  all`), and agent-to-agent messages are on by default
  ([trust-model.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/security/trust-model.md#L17-L18)).
- DM channels pair unknown senders by default
  ([README.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/README.md)).
  For Aether, the node does the admission, so the plugin uses a fixed
  allowlist of one sender, as `a2a` uses its peer list.
- Plugins run in the gateway process with its OS privileges and are trusted
  code
  ([SECURITY.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/SECURITY.md#L151-L157),
  [SECURITY.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/SECURITY.md#L267-L273)).
- A pending exec approval keeps the run waiting until an operator decides
  in a user channel or the Control UI
  ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L189-L191)).

What this means for tasks from remote Aether peers:

1. Bind the `aether` channel to a dedicated agent. Example:

   ```json5
   {
     tools: { sessions: { visibility: "self" } },
     agents: {
       ownership: "explicit",
       entries: {
         main: { workspace: "~/.openclaw/workspace" },
         aether: {
           workspace: "~/.openclaw/workspace-aether",
           sandbox: { mode: "all", scope: "session", workspaceAccess: "none" },
           tools: {
             profile: "minimal",
             deny: ["aether__*", "group:plugins"],
           },
         },
       },
     },
     bindings: [{ agentId: "aether", match: { channel: "aether" } }],
     channels: { aether: { enabled: true, dataDir: "~/.aether/host" } },
   }
   ```

   Start from the `minimal` profile, an allowlist, not from a deny list.
   The full set of tools that a public agent must deny is not documented,
   and a deny list fails open when OpenClaw adds a tool. The `minimal`
   profile has only `presence` and `session_status`
   ([core-tool-facts.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/core-tool-facts.ts#L8),
   [L95](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/core-tool-facts.ts#L95),
   [L111](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/agents/core-tool-facts.ts#L111)).
   The owner adds each tool by name with `tools.alsoAllow`, after a review.
   OpenClaw refuses `allow` and `alsoAllow` in the same scope, so the
   pattern is "profile + `alsoAllow`"
   ([zod-schema.agent-runtime.ts](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/src/config/zod-schema.agent-runtime.ts#L62-L69)).
   Do not use an explicit `tools.allow` that leaves no tool: OpenClaw then
   stops the run before the model sees the prompt
   ([multi-agent-sandbox-tools.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/tools/multi-agent-sandbox-tools.md#L255)).
   The other example names come from
   [tool-permissions.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/security/tool-permissions.md#L161-L224).
2. Give that agent only tools that need no approval. As a second guard,
   the plugin registers a `before_tool_call` hook. In an `aether` session,
   the hook blocks every tool that is not in the plugin setting
   `allowedTools` (empty by default), and the model gets an error. Then no
   task waits for a person
   ([hooks.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/hooks.md#L169)).
3. For callers who are not well known (hub service listings), run a second
   OpenClaw gateway with its own profile, as OpenClaw advises for
   untrusted users
   ([multiple-gateways.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/multiple-gateways.md)).
4. The plugin warns in `security.collectWarnings` when the channel routes
   to an agent with no sandbox, or when `tools.sessions.visibility` is
   `all`
   ([sdk-channel-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-channel-plugins.md#L386-L392)).

### Packaging and install

- Publish to ClawHub or npm. Users install with
  `openclaw plugins install <package>` (ClawHub by default), `npm:<package>`,
  `git:...`, a local path, or `--link <path>` for development
  ([building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L16-L24),
  [manage-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manage-plugins.md#L228-L260)).
- Third-party plugins show a capability consent screen (channels, tools,
  MCP servers, hooks, skills). Noninteractive installs need
  `--accept-capabilities`, and non-ClawHub sources need `--force`
  ([manage-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manage-plugins.md#L143-L216)).
- A plugin that needs config stays disabled after install. The user sets
  the config, then runs `openclaw plugins enable aether`
  ([manage-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/manage-plugins.md#L267-L271)).
- Config keys: `plugins.entries.aether.config` (plugin settings,
  `configSchema`), `channels.aether` (channel settings,
  `channelConfigs.aether.schema`), `plugins.allow`, `mcp.servers.aether`
  (override), `agents.entries.aether`, `bindings`.
- Point `openclaw.extensions` at built JavaScript (`./dist/index.js`) in
  the published package. TypeScript entries are only for source checkouts
  ([building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L193-L198)).
- Declare `peerDependencies.openclaw` and `openclaw.compat.pluginApi`
  ([building-plugins.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/building-plugins.md#L66-L88)).

### Plugin files

```text
integrations/openclaw-plugin/
  package.json          # name (for example @aether/openclaw-plugin), openclaw.extensions,
                        # openclaw.channel, peerDependencies.openclaw, compat
  openclaw.plugin.json  # id: aether, channels: [aether], channelConfigs.aether,
                        # configSchema, mcpServers.aether (caller tools only)
  index.ts              # defineChannelPluginEntry
  setup-entry.ts        # setup wizard: data directory
  src/channel.ts        # createChatChannelPlugin: config, security, gateway.startAccount
  src/node-socket.ts    # agent.sock client (node:net, line JSON, reconnect)
  src/inbound.ts        # task -> route, ingress, dispatch; reply -> result/failed/progress
  skills/aether/SKILL.md  # optional: when and how to send Aether tasks
  test/                 # fake node socket; protocol contract tests
  README.md             # install, enable, agent binding, sandbox, limits
  tsconfig.json
```

Use no runtime dependencies except the OpenClaw SDK. Node's `node:net`
opens Unix sockets.

### Aether changes

None is necessary for the first version. Recommended:

1. **Caller-only MCP mode.** Add an option to `aether mcp serve`, for
   example `--caller-only`, that lists only the seven caller tools. Then
   any agent host can add the server with no tool filter, and an error in
   the host filter cannot expose owner tools. The CLI is in
   [main.rs](../crates/aether/src/main.rs#L363-L386) and the server is
   built in [mcp.rs](../crates/aether/src/mcp.rs#L1721-L1727).
2. **Docs.** Add `docs/agents/openclaw.md` and update the README sentence
   that names OpenClaw as a goal.
3. **Contract test.** Run the plugin's tests against the same protocol
   cases as the connected adapter tests
   ([connected.rs](../crates/aether/src/agent/connected.rs#L309)).

The open items of the Hermes work (outbox retry when no agent is connected,
progress merge, access mode to the adapter) apply to OpenClaw in the same
way.

### Order of work

1. Plugin skeleton, manifest, `openclaw plugins inspect aether --runtime
   --json` passes on a pinned OpenClaw version.
2. Node socket client and `gateway.startAccount` with reconnect.
3. Inbound dispatch with an isolated session for each task; result,
   failed, progress.
4. Cancel as "report and drop".
5. Send: manifest `mcpServers` (or the caller-only mode first).
6. README with the dedicated agent, binding and sandbox example.
7. Later: publish to ClawHub; ask OpenClaw for a channel abort API.

## Decisions

The owner asked for best practice where the sources give no answer. These
are the choices for the first version.

| Gap | Decision | Why |
| --- | --- | --- |
| Cancel | On `cancel`, send `cancelled` at once. Drop all later `deliver` calls for that task. Do not try to stop the run through the Gateway. | No plugin API stops a dispatched run. A Gateway call needs operator authority, which the plugin must not hold. The sandbox and the `minimal` profile limit what a late run can do. Report the gap to OpenClaw as a feature request. |
| Approvals | `minimal` profile, plus the `before_tool_call` guard. | A task must never wait for a person. A blocked tool gives a fast error, not a time-out. |
| Tool exposure | Manifest `toolFilter.include` with the seven caller tools; `aether__*` and `group:plugins` denied for the `aether` agent; caller-only mode of `aether mcp serve` as the next node change. | Three layers. Each one alone is enough today. The node change removes the risk that an owner override drops the filter. |
| Sessions | One session for each task, as `a2a` does. The README tells owners to archive old `aether` sessions with `openclaw sessions archive`. Ask OpenClaw for a retention option for channel sessions. | Isolation between callers is more important than store size. The plugin must not invent a synthetic key shape to get cleanup, because that depends on OpenClaw internals. |
| Progress | `tool` and `block` → `progress` with no text. `final` → `result`. Terminal outcome `failed` → `failed`. | Same rules as the Hermes plugin: the node gets no OpenClaw detail. |
| Stability | `peerDependencies.openclaw` pinned to the tested release range; `openclaw.compat.pluginApi` set; CI runs the plugin contract tests and `openclaw plugins inspect aether --runtime --json` on each supported release. | All plugin APIs are experimental. A failed CI run on a new release is better than a silent break for users. |
| Trust | The plugin never sets `senderIsOwner` and never asks for official status in the first version. | Community channels can admit guests. A remote task is always a guest. |

## Other paths that were examined

| Path | Works? | Why not first |
| --- | --- | --- |
| **Node → OpenClaw `a2a` channel over HTTP.** A new node adapter sends each admitted task as A2A `SendMessage` to `http://127.0.0.1:18789/a2a/v1` with a peer bearer token, and polls `GetTask`. | Yes, with no OpenClaw plugin code. Sessions are isolated for each peer and `contextId`; `/` text is rejected ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L171-L187)). | It is a new Rust adapter shape, against the Hermes rule "only the connected shape needs one Rust adapter for all agents". It adds a shared bearer token. Cancel is refused. Text is cut at 64 KiB and the rate limit is 30 requests a minute for each peer ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L193)). The public Agent Card shows the agent IDs ([a2a.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/a2a.md#L181-L185)). A generic "A2A HTTP" adapter could serve other A2A agents later; keep it as a later option. |
| **OpenAI-compatible HTTP or Gateway WebSocket `agent`/`chat.send`.** | Yes, technically. | The Gateway token is full operator access ([openai-http-api.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/gateway/openai-http-api.md#L40-L44)). Remote text runs as the owner, without channel ingress rules. Rejected. |
| **`openclaw acp` as a built-in node adapter.** | Yes. It supports `prompt` and `cancel` ([cli/acp.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/cli/acp.md#L25-L30)). | Same operator authority, and it supports slash commands. Rejected for remote tasks. |
| **`api.runtime.hooks.dispatchHookAgentTurn`** (isolated turn for untrusted external content). | Only for bundled and trusted official plugins ([background-work.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/plugins/sdk-runtime/background-work.md#L415-L450)). | Not available to a third-party plugin. |
| **Reef** (bundled, agent-to-agent between different people, relay, guards, autonomy tiers). | It is a separate network ([reef.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/reef.md#L9-L21)). | Not a path for Aether. It shows the same need, and its guards and autonomy tiers ([reef.md](https://github.com/openclaw/openclaw/blob/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87/docs/channels/reef.md#L218-L226)) are ideas to compare with Aether grants. |

## Risks

| Risk | What to do |
| --- | --- |
| A caller's text enters the owner's main session | Pin `dmScope: "per-account-channel-peer"` with the task ID as peer, as `a2a` does. The OpenClaw default `main` must not apply. |
| Remote text reaches the owner's tools, files or sessions | Dedicated `aether` agent: sandbox `all`, no workspace access, narrow tools, `tools.sessions.visibility: "self"`. |
| A cancelled task keeps running in OpenClaw | Report `cancelled`, drop the late reply, and rely on the sandbox. Ask OpenClaw for an abort API. |
| A tool approval waits with no person | `minimal` profile for the `aether` agent; the `before_tool_call` guard blocks all other tools. |
| Owner MCP tools (`aether.change`, `aether.access.revoke`, …) reach the model | Manifest `toolFilter.include`; caller-only mode of `aether mcp serve`. An owner override of `mcp.servers.aether` drops the manifest filter, so the README shows the `include` list. |
| `aether` sessions are never cleaned up | Owner archives them with `openclaw sessions archive`; ask OpenClaw for a channel-session retention option. |
| The plugin SDK changes in a release | Pin the OpenClaw version, declare `compat.pluginApi`, and run CI on each supported version. |
| A caller learns that OpenClaw answers | The node instructions forbid it; the plugin sends only the reply text; failures carry no reason. Same limits as for Hermes. |
| The gateway is down | The node fails the task as not available, as for Hermes. |

## Open questions

1. Can OpenClaw add a channel-scoped abort, so that a channel plugin can
   stop the run that it dispatched? Without it, Aether cancel is only
   "stop waiting".
2. Can OpenClaw add a retention option for channel sessions, so that
   per-task sessions age out? Today maintenance keeps them, and a plugin
   cannot archive them (see [Sessions](#sessions)).
3. Should one OpenClaw gateway serve more than one Aether data directory?
   OpenClaw channel accounts (`channels.aether.accounts.<id>`) could map
   each data directory to its own agent.
4. Does OpenClaw count concurrent runs across all sessions? If so, many
   Aether tasks can delay the owner's own chats.
5. Should the plugin be offered to the OpenClaw official catalog, so it
   skips the capability review and can use trusted runtime APIs?

## Sources

OpenClaw at
[`9b5c4e8`](https://github.com/openclaw/openclaw/tree/9b5c4e89ca7a24e71f436c1d874b49e82e1b8c87):

- Overview: `README.md`, `LICENSE`, `package.json`, `SECURITY.md`,
  `docs/concepts/architecture.md`, `docs/start/lore.md`.
- Plugins: `docs/plugins/building-plugins.md`, `sdk-overview.md`,
  `sdk-channel-plugins.md`, `sdk-channel-inbound.md`, `manifest.md`,
  `manifest/surfaces.md`, `manage-plugins.md`, `hooks.md`, `bundles.md`,
  `sdk-runtime/gateway-and-nodes.md`, `sdk-runtime/background-work.md`.
- Channels: `docs/channels/a2a.md`, `reef.md`, `raft.md`;
  `extensions/a2a/` (`index.ts`, `openclaw.plugin.json`, `package.json`,
  `src/channel.ts`, `src/inbound.ts`); `extensions/raft/src/inbound.ts`.
- Tools and MCP: `docs/tools/mcp.md`, `skills.md`, `acp-agents.md`,
  `docs/cli/acp.md`, `src/agents/agent-bundle-mcp-names.ts`,
  `src/plugins/manifest-capability-normalizers.ts`,
  `src/config/schema.help.agents.ts`, `src/plugins/bundle-mcp.ts`,
  `src/agents/bundle-mcp-config.ts`,
  `src/agents/agent-bundle-mcp-catalog-projection.ts`,
  `src/agents/native-mcp-policy.ts`, `src/agents/core-tool-facts.ts`,
  `src/config/zod-schema.agent-runtime.ts`,
  `docs/tools/multi-agent-sandbox-tools.md`,
  `docs/gateway/sandbox-vs-tool-policy-vs-elevated.md`.
- Channel runtime and trust: `src/plugins/runtime/runtime-channel.ts`,
  `src/plugins/registry-runtime-channel.ts`,
  `src/plugins/registry-runtime.ts`,
  `src/plugins/channel-startup-policy.ts`,
  `src/auto-reply/reply/reply-dispatcher.types.ts`,
  `src/auto-reply/get-reply-options.types.ts`.
- Sessions: `src/routing/session-key.ts`,
  `src/config/sessions/store-maintenance.ts`,
  `src/config/sessions/store-maintenance-activity.ts`,
  `docs/cli/sessions.md`.
- Security: `docs/gateway/security/trust-model.md`,
  `security/tool-permissions.md`, `sandboxing.md`,
  `sandboxing/modes-scope-and-backend.md`, `openai-http-api.md`,
  `multiple-gateways.md`, `docs/concepts/session.md`,
  `docs/concepts/agent-bindings.md`.

Aether at `6bd6901`: `README.md`, `PRODUCT.md`,
`research/hermes-agent-integration.md`, `integrations/hermes-plugin/`,
`crates/aether/src/execution.rs`, `crates/aether/src/agent/connected.rs`,
`crates/aether/src/mcp.rs`, `crates/aether/src/main.rs`, `docs/pact.md`.
