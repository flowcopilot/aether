# T3 Code: how it uses the Agent Client Protocol

Checked against the T3 Code source at commit
[`00cdceb`](https://github.com/pingdotgg/t3code/tree/00cdceb196edf661652dee6efe983c0d40662971)
on 2026-10-15. T3 Code is an MIT-licensed TypeScript application (Effect) that
runs coding agents for a person through a web, desktop or mobile client.

"ACP" in this document is the Agent Client Protocol
([agentclientprotocol.com](https://agentclientprotocol.com)). It is not the IBM
Agent Communication Protocol (merged into A2A) and not the Agentic Commerce
Protocol.

## Result

1. **T3 Code does not run Codex through ACP.** Codex, Claude, Cursor, OpenCode,
   Pi and Muse have native adapters. Codex uses its own App Server package,
   `packages/effect-codex-app-server`. ACP is the generic path for other agents.
   Aether made the same choice for Codex.
2. **A message starts the agent lazily.** A new thread starts no process. The
   first run creates the provider session, then the provider thread, then the
   turn. An idle reaper can stop the process later. The next message starts it
   again and resumes the stored session.
3. **Agent-to-agent messages go through T3's own MCP server, not through ACP.**
   An agent calls `t3_thread_send` or `delegate_task`. The tool writes a
   durable command. The orchestrator then starts the target agent. A finished
   child task wakes the parent thread.
4. **The permission answer copies the Codex sandbox model.** T3 answers ACP
   `session/request_permission` from a policy: reads are allowed, a read-only
   sandbox denies changes, and a workspace-write sandbox allows changes only
   inside canonical workspace roots.
5. **T3 does not trust its own answers as a sandbox.** Its provider guide says
   that an answer from T3 is weaker than the agent's own enforcement.

## Integration shape

The provider guide gives two integration types.
([adding a provider](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/internals/adding-a-provider.md#choose-the-integration-shape))

| Type | Used for | Code |
| --- | --- | --- |
| Native adapter | Codex, Claude, Cursor, OpenCode, Pi, Muse | One package per provider; Codex uses `effect-codex-app-server` |
| Shared ACP adapter | Any agent in the ACP Registry, or a local ACP command | `packages/provider-acp`, on the protocol package `packages/effect-acp` |
| ACP flavor | Agents that need more than the specification (Grok, Antigravity) | A small layer on the shared ACP adapter |

The guide forbids agent-specific checks in the generic adapter: "Never add
agent-id checks to the generic registry adapter." The generic adapter still
contains some workarounds for `codex-acp` and `pi-acp` behaviour, for example
`adapter.ts` line 692 and line 786.

The ACP Registry gives the agent list. T3 installs a registry agent under
`tools/<agent-id>/<version>/`, checks the SHA-256 when the registry gives one,
and starts the binary directly, without a shell. A person can also add a local
command with literal arguments.
([ACP providers](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/user/providers-acp.md#add-a-local-command))

## Protocol versions

T3 sends `protocolVersion: 2` (the ACP v2 preview) in `initialize` and accepts a
v1 answer. It does not trust the version number in the answer. It reads the
shape of the answer, because Antigravity reports version 2 with a v1 response
shape.
([`effect-acp/src/client.ts`](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/packages/effect-acp/src/client.ts#L1128-L1144))

Lesson for Aether: ACP v2 is not final yet. A client must accept both
generations and must decide by response shape.

## Wake behaviour

A thread is durable state. A provider process is not.
([feature lifecycles](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/orchestration-v2/feature-lifecycles.md))

```text
thread.message.send
  -> ConversationMessage(role=user)
  -> Run(status=queued)
  -> enqueue provider command

first run starts
  -> ensure ProviderSession   (spawns the ACP process)
  -> ensure ProviderThread    (session/new, session/load or session/resume)
  -> send provider turn       (session/prompt)
```

The same recovery path runs after a server restart, after the idle reaper
stops a process, and after a crash. T3 keeps the native session reference in
durable state and resumes it at the next run. Tests must use the production
reaper and recovery services, not test-only restart methods.
([data model](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/orchestration-v2/core-graph-and-data-model.md))

T3 does not start a stopped server. The T3 server process must run. This is
the same limit that `research/block-buzz.md` found for `buzz-acp`.

## Agent-to-agent messages

T3 serves an authenticated MCP endpoint at `http://127.0.0.1:<port>/mcp`.
Before it opens a provider session, it issues a bearer credential scoped to the
environment, the parent thread, the provider instance and the provider session.
The credential expires after a maximum lifetime and after an idle period, and
it is revoked when the session is released.
([orchestrator MCP server](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/orchestration-v2/orchestrator-mcp-server.md#transport-and-authentication))

Each agent type gets the endpoint in its own way:

| Agent | How T3 injects the MCP server |
| --- | --- |
| Codex App Server | `-c mcp_servers.t3-code.url=…` and a bearer token in an environment variable |
| Claude Agent SDK | `mcpServers` with an `Authorization` header |
| ACP agents | The `mcpServers` field of `session/new`, `session/load` and `session/fork` |

The relevant tools:

| Tool | What it does |
| --- | --- |
| `t3_thread_send` | Sends a message to a thread. Mode `auto` starts an idle thread, steers an active turn, or queues behind a turn that cannot be steered yet |
| `delegate_task` | Creates a child thread and starts it at once; `mode: "wait"` blocks until the result |
| `task_status`, `task_cancel` | Read or stop a delegated task |
| `t3_thread_wait`, `t3_thread_interrupt` | Wait for or stop a run |

Two safety rules apply to these tools:

- A target thread cannot run with broader runtime or interaction modes than the
  caller.
- Retries are idempotent: stable command and message IDs come from
  `clientRequestId`.

The MCP server only writes commands. It never calls a provider adapter
directly. A child task that finishes writes a durable result transfer, and that
transfer wakes the parent, also after a restart.

ACP has no subagents and no steering. For ACP agents, T3 creates child threads
itself and replaces steering with cancel and restart.

## Permission answers

ACP agents run their own tools under their own sandbox and approval rules. T3
answers their `session/request_permission` requests by the thread's permission
mode.
([ACP permissions](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/user/providers-acp.md#permissions-and-terminals))

The decision function is `acpOperationDisposition` in
[`provider-acp/src/server/clientPolicy.ts`](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/packages/provider-acp/src/server/clientPolicy.ts).
It returns `allow`, `ask` or `deny`:

| Tool kind | Sandbox | Answer |
| --- | --- | --- |
| `read`, `search`, `think` | any known type | allow; an unknown sandbox type denies |
| any other kind | approval required | ask the person |
| any other kind | `readOnly` | deny |
| `edit`, `delete`, `move` | `workspaceWrite` | allow only if every location is inside the workspace or a writable root; otherwise deny |
| `execute` and other kinds | `workspaceWrite` | deny |
| any | `dangerFullAccess`, `externalSandbox` | allow |
| any | unknown type | deny |

The containment check is careful and is worth copying:

- It resolves each path with `realpath` before it compares paths. It does not
  normalize `..` first, because `workspace/link/../file` can leave the
  workspace through a symbolic link.
- For a file that does not exist yet, it resolves the deepest existing parent
  and adds the missing part.
- A broken symbolic link or any other error denies the request.
- A request without locations is denied.

T3 also passes the provider's own option IDs back unchanged. For OpenCode,
which stores "always" grants for the whole project, T3 answers an automatic
approval with "once", so a full-access reply cannot widen a supervised session.
([providers](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/internals/providers.md#process-and-account-isolation))
The same rule is a safe default for any agent that can remember a grant.

The provider guide states the limit: "Do not imitate a missing mode by
answering approvals in T3: T3's check is weaker than the agent's own
enforcement."
([adding a provider](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/internals/adding-a-provider.md#report-only-what-the-provider-does))
An agent can edit a file without asking, so a client answer is a second gate,
not a sandbox.

## Process and account isolation

- Each provider instance has its own sessions, catalogue and credentials. T3
  removes ambient credentials that the instance did not configure, so two
  instances cannot share an account or billing by accident.
- A status check must not open a session, because a session can start MCP
  servers, run hooks or open a login browser. Catalogue inspection never starts
  an ACP process.
- Antigravity gets a separate profile per instance and file-based credential
  storage instead of the shared macOS keychain entry.
- T3 does not guarantee account isolation for registry agents: "ACP does not
  describe account isolation."
  ([providers](https://github.com/pingdotgg/t3code/blob/00cdceb196edf661652dee6efe983c0d40662971/docs/internals/providers.md#process-and-account-isolation))

## Comparison with Aether

| Need | T3 Code | Aether today |
| --- | --- | --- |
| Codex execution | Native App Server adapter | Native App Server (`CodexAppServer`) |
| Other agents | Shared ACP adapter plus the ACP Registry | None |
| Start on message | Durable run queue; process started lazily at the first run; idle reaper | Durable task queue; one `codex app-server` process per task |
| Who sends the message | A local agent through an MCP tool, or the person | A remote peer over Iroh or PACT, after grant and policy checks |
| Resume across turns | Native session reference stored and resumed | Ephemeral thread per task; no resume |
| Permission answers | Policy function on tool kind and canonical paths | Codex sandbox set at thread start; features disabled by flags |
| Cold start | Not supported; the server must run | Not supported; `node serve` must run |
| Version handling | ACP v2 preferred, v1 accepted, decided by response shape | Not applicable |

## Recommendation for Aether

1. **Keep Codex on the App Server path.** T3 Code made the same choice. ACP
   would remove the `--disable` flags and the ephemeral thread that keep a
   remote task away from the owner's Codex setup.
2. **When a second agent is necessary, add one generic ACP executor** next to
   `CodexAppServer`, behind the same `CodexProcess` trait. Do not add
   agent-specific code to it.
3. **Copy T3's permission function, but fail closed for remote work.** A remote
   peer is less trusted than T3's local person. Use these rules: reads allowed,
   `read-only` denies all changes, `workspace-write` allows `edit`, `delete` and
   `move` only inside the canonical workspace, and every `execute` is denied.
   Answer "once", never "always".
4. **Accept that ACP permission answers are not a sandbox.** Run an ACP agent
   only in an operating-system sandbox, or only an agent with its own sandbox
   that the node can set at start. Write this limit in the policy
   documentation.
5. **Take the idempotent "send and start if idle" model for local delegation.**
   It matches the Aether queue: `submit` already wakes the scheduler. T3's
   scoped, short-lived MCP credential is a good model if a node ever lets its
   local agent send tasks to other nodes.
6. **Do not expect a cold start from ACP.** A stopped node needs an operating
   system service (launchd or systemd), as `research/block-buzz.md` also found.
