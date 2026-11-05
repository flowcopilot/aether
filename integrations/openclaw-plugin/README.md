# Aether plugin for OpenClaw

This plugin connects OpenClaw to an Aether node. It does two things.

- It receives tasks from other agents. The `aether` channel runs each task
  in its own OpenClaw session.
- It sends tasks. The plugin adds an MCP server with the seven caller tools.

The plugin does not hold an Aether key. The node keeps the network, the
contacts, the grants and the audit log.

This package is private. It is not published yet.

## Requirements

- OpenClaw `2026.10.9` or later, before `2026.11.0`.
- Node.js `>=24.16.0 <25` or `>=26.1.0`, as OpenClaw needs.
- The `aether` command on your PATH. It must support
  `aether mcp serve --caller-only`. Build and install it with
  `mise run install-aether`.
- A running node that shares the socket with OpenClaw. Run it as the same
  OS user:

  ```bash
  aether node serve --adapter connected
  ```

## Install for development

Build the plugin first. OpenClaw loads the built file in `dist/`.

```bash
cd integrations/openclaw-plugin
npm install
npm run build
openclaw plugins install --link "$PWD"
```

Then set the config (see below) and enable the plugin. A plugin that needs
config stays disabled after install.

```bash
openclaw plugins enable aether
```

## Configure the channel

Set the channel in `openclaw.json`:

```json5
{
  channels: {
    aether: {
      enabled: true,
      dataDir: "~/.aether/host",
      allowedTools: [],
    },
  },
}
```

- `dataDir` holds `agent.sock`. The default is `$AETHER_DATA_DIR`, then
  `~/.aether/host`.
- `allowedTools` lists the tool names that an Aether task may call. It is
  empty by default, so an Aether task calls no tool. OpenClaw names tools
  for the model, for example `aether__<tool>`. Use the names that OpenClaw
  shows for your agent.

## Use a dedicated agent for Aether tasks

Anyone who can send a task to this channel can make its agent act. Give the
channel its own agent, with its own workspace, a sandbox and few tools.
Do not bind it to your main agent.

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

- `profile: "minimal"` starts from almost no tools. Add a tool by name with
  `tools.alsoAllow` after you review it. Do not give the agent the
  `aether__*` tools. They send tasks with the owner's grants.
- The plugin also blocks every tool that is not in `allowedTools`. It
  registers a `before_tool_call` hook for this. A blocked tool gives the
  model an error. The task does not wait for a person.
- `tools.sessions.visibility: "self"` stops the agent from reading other
  sessions.

The plugin warns when the bound agent has no sandbox, or when session
visibility is `all`. These warnings are part of OpenClaw's security checks.

## Caller tools and the `mcp.servers.aether` override

The plugin manifest starts `aether mcp serve --caller-only` and lists only
the seven caller tools:

- `aether.agents.list`, `aether.agents.find`, `aether.people.find`
- `aether.task.send`, `aether.task.cancel`
- `aether.inbox.list`, `aether.inbox.read`

If you set `mcp.servers.aether` in your config, your entry replaces the
plugin entry. The manifest `toolFilter` is then lost. Keep
`--caller-only` in `args`, and copy the seven names into an `include`
list:

```json5
{
  mcp: {
    servers: {
      aether: {
        transport: "stdio",
        command: "aether",
        args: ["mcp", "serve", "--caller-only", "--data-dir", "/path/to/host"],
        toolFilter: {
          include: [
            "aether.agents.list", "aether.agents.find", "aether.people.find",
            "aether.task.send", "aether.task.cancel",
            "aether.inbox.list", "aether.inbox.read",
          ],
        },
      },
    },
  },
}
```

OpenClaw does not pass `AETHER_DATA_DIR` to MCP servers. Use `--data-dir`
when your data directory is not the default.

## Cancel

- When a caller cancels, or the time limit ends, the node sends `cancel`.
  The plugin sends `cancelled` to the node at once. It drops the late reply.
- OpenClaw has no plugin API that stops a running agent turn. The turn can
  keep running until it ends. The sandbox and the tool list limit what it
  can do during that time.

## Sessions

Each Aether task gets its own OpenClaw session. OpenClaw keeps these
sessions. They do not expire by themselves. Archive old ones from time to
time:

```bash
openclaw sessions archive "<session-key>"
```

Run `openclaw sessions --help` to find the keys. Archiving keeps the
transcript and removes the session from the active list.

## Tests and typecheck

```bash
npm test
npm run typecheck
```

From the repository root, `mise run openclaw-plugin-test` runs the tests.
