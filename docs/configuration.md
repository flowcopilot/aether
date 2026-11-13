# Node configuration

## Agent adapters

A node gives each admitted task to one agent. The policy field `adapter`
chooses how the node reaches it:

| `adapter` | The agent | Setup |
| --- | --- | --- |
| `codex` (default) | The node starts `codex app-server` for each task. | See "Real Codex" below. |
| `connected` | An agent that runs on its own connects to `<data-dir>/agent.sock`. | For Hermes Agent, see [integrations/hermes-plugin](https://github.com/flowcopilot/aether/blob/main/integrations/hermes-plugin/README.md). For OpenClaw, see [integrations/openclaw-plugin](https://github.com/flowcopilot/aether/blob/main/integrations/openclaw-plugin/README.md). |

Set the adapter once with `aether node serve --adapter codex|connected`. The
node saves it in its policy, and later starts keep it. Each data directory has
one agent; to run two agents, use two data directories.

The caller does not see the adapter: the Agent Card, hub listings, task
states and errors are the same for every agent.

The Codex fields `sandbox`, `allowNetwork` and `environmentAllowlist` apply
only to the Codex adapter. A connected agent runs with its owner's own setup,
so the node refuses a policy that sets these fields together with
`"adapter": "connected"`. A switch to `connected` with `--adapter` removes
them.

A connected agent speaks one JSON object on each line. It sends
`{"type":"ready","protocol":1}` first. Then the node sends `task` (`id`,
`instructions`, `text`) and `cancel` (`id`), and the agent answers with
`progress`, `result` (`text`), `failed` or `cancelled`, each with the task
`id`. The node sends no keys, grants, endpoint IDs or caller names. The
socket file mode is `0600`. When no agent is connected, the node fails each
task as not available. At `maxSeconds`, the node cancels the turn. The
`ready` message can also say `"max_tasks"`: how many tasks the agent takes at
once (see "Tasks that run at once").

## Tasks that run at once

The node runs one task at a time unless its owner says more. The policy field
`runningTasks` is how many tasks run at the same time, for all callers
together. It is 1 for a new node and for a policy file that does not have the
field, so a node that you upgrade behaves as before. It can be 1 to 32: each
running task is a turn of the agent, and for Codex its own `codex app-server`
process, so a higher number needs a computer that can run that many agents.

It is separate from the waiting limits `queueGlobal` and `queuePerPeer`,
which say how many tasks wait. A task that comes while `runningTasks` tasks
run waits in the queue, in the order that the node always used, and counts
against those limits. There is no limit for one caller or one group of running
tasks.

Change it like the waiting limits, with the same checks and audit records:

- On the **Advanced** page, **Task policy**, **Tasks running at once**. Saved
  policy changes take effect after a restart.
- With `aether.settings.update_policy` (`running_tasks`) or with
  `aether.change` (`{"type": "limit", "id": "running_tasks", "limit": 2}`).
  `aether.change` changes the running node at once: a task that waits starts
  as soon as there is room. A higher value waits for your confirmation under
  Pending changes on the Advanced page; a lower value applies at once.

Lowering the limit never stops a task that runs. The node starts no new task
until fewer than the new number of tasks run.

The agent can allow fewer tasks than you do; the node uses the smaller
number:

| Agent | Tasks at once |
| --- | --- |
| Codex, `sandbox` `task-folder` (default) | As many as `runningTasks`. Each task has its own folder and its own Codex process. |
| Codex, `sandbox` `workspace-write` | One. All tasks would write the same folder. |
| Hermes plugin, OpenClaw plugin | Up to 32: each task is its own session, and the plugin says so in `ready`. |
| Grok Bot bridge | One: a Bot claims one task at a time. |
| A connected agent that does not say `max_tasks` | One. |

## Real Codex

A node runs Codex in its own Codex home, `<data-dir>/codex-home`. If the owner
is already logged in to Codex, the node links only the owner's login file.
The owner's own `~/.codex` setup (MCP servers, hooks, plugins, and memories)
stays out of the node's Codex home. The agent's tools are the MCP servers in
`<data-dir>/codex-home/config.toml` (see "Agent tools"). Both Codex processes use the same login file when they refresh
their credentials. If the owner has no Codex login file, `node serve` prints
this command:

```sh
CODEX_HOME=<data-dir>/codex-home codex login --device-auth
```

Do not copy `auth.json` from `~/.codex`. The link lets both processes use one
file when Codex changes the login. The Codex app server can reach its API
outside a task. A task folder has no network access. Set the time and context
limits on the Advanced page. An old policy file can still be imported on the
first start with fields such as:

```json
"environmentAllowlist": ["PATH"],
"maxSeconds": 120,
"maxContextTokens": 20000
```

`environmentAllowlist` names variables to copy from the node's own
environment. It refuses `HOME` and the `CODEX_*` variables that choose a
configuration, credential or state location.

## Agent tools (MCP servers)

The tools of this node's agent are the MCP servers in
`<data-dir>/codex-home/config.toml`. The owner turns a tool on or off in this
file. Aether has no tool setting on a page or in a grant. An example:

```toml
[mcp_servers.calendar]
command = "/usr/local/bin/calendar-mcp"
args = ["--stdio"]
enabled_tools = ["list_events", "create_event"]

[mcp_servers.calendar.env]
CALENDAR_TOKEN = "<token>"

[mcp_servers.calendar.tools.create_event]
approval_mode = "approve"
```

- Edit the file, or use
  `CODEX_HOME=<data-dir>/codex-home codex mcp add|list|remove`. Set
  `enabled = false` in a server table to turn the server off.
- Aether runs every task with the approval policy `"never"`. Codex then
  refuses each MCP tool call that needs an approval. A tool with the
  `readOnlyHint` annotation runs. A tool that can change data runs only with
  `approval_mode = "approve"` for that tool, or
  `default_tools_approval_mode = "approve"` for the server.
- The node declines each request from an MCP server for more input
  (elicitation), because nobody reads a remote task.
- Codex starts with an empty environment. A task has no `PATH` unless the
  policy puts `PATH` in `environmentAllowlist`. Use an absolute `command`, or
  set `PATH` in the server's `env` table.
- MCP servers run outside the task sandbox. They have the file and network
  access of the account that runs the node.
- Every caller that the node admits can use every enabled tool. To give
  some callers fewer tools, run a second agent with its own data directory
  and fewer MCP servers.
- Each task starts a new Codex app server, so a change applies to the next
  task.
- The app server runs with `--strict-config`. An unknown key stops every
  task. Run `codex mcp list` with this `CODEX_HOME` after each change.
- Connectors (`apps`) and plugins stay off for tasks, also when the config
  turns them on.
- Do not add `aether mcp serve` to this file. That server is for your own
  assistant. It administers the node and sends tasks to other agents.
- Put secrets in the server's `env` table, or name allowlisted variables in
  `env_vars`. The node's Codex home is owner-only (mode 700).
- Aether never publishes these servers or their tools: not in an Agent
  Card, a listing, a hub record, or a tool description. The rules here were
  checked with codex-cli 0.162.1.

Add directory settings to publish the live node record and renew it before it
expires. `--address` can use port `0` when `--port 0` selects the port. The
node replaces that port with its bound port before it signs the record.

```sh
  --directory https://127.0.0.1:8443 \
  --directory-certificate ./.aether/control/control-cert.der \
  --agent-id server --address 127.0.0.1:0 \
  --scope directory --capability review --directory-record-ttl 600
```

Add `--managed-scope` to `node serve` or `node admin` to show Members and
Invitations. `node admin` is for local UI checks without a running node. Use
`node serve --admin-port` to administer a live node and to cancel its tasks.

For a managed node policy, add `--managed-control` and
`--managed-control-certificate` to `node serve`. The node completes a pinned
HTTPS roster sync before it accepts work. It refreshes the roster every 30
seconds and denies a revoked or expired member.

The UI listens only on `https://127.0.0.1:<port>/`. It creates an
`admin-cert.der` certificate and an owner-only private key in the node data
directory. Trust the certificate in the local browser before you use the page.

`mcp serve` uses `$AETHER_DATA_DIR`, else `~/.aether/host`. Use `--data-dir`
to select another node and its contacts. Use `--caller-only` for an agent
that must only send tasks and read answers: the server then lists no tool
that manages the node.
