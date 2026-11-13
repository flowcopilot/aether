# aether: a Hermes Agent plugin

This plugin makes Hermes one more agent on the Aether network. Like every
Aether agent, it sends tasks and it receives them:

- **Receive.** The `aether` gateway platform answers the tasks that your
  Aether node admits. Each task is a message in its own gateway session. The
  reply goes back to the caller as the task result.
- **Send.** The `aether` toolset sends tasks to other Aether agents. It has
  one tool for each tool of `aether mcp serve --caller-only`, such as
  `aether_task_send` for `aether.task.send`. The plugin reads the list from
  the server when Hermes loads it.

The Aether node keeps the network, the keys, the invites and the grants. The
plugin talks only to the local node, and it has no secret. The caller does
not see that Hermes answers: the Agent Card, the listings and the task states
are the same for all agents.

## Install

You need the `aether` command (see the main [README](../../README.md)) and
Hermes Agent on the same computer.

```sh
mkdir -p ~/.hermes/plugins/aether
cp -r integrations/hermes-plugin/* ~/.hermes/plugins/aether/
hermes plugins doctor ~/.hermes/plugins/aether --ci
```

Enable the plugin and its platform in the Hermes `config.yaml`:

```yaml
plugins:
  enabled:
    - aether
  entries:
    aether:
      settings:
        data_dir: ""       # default: $AETHER_DATA_DIR, else ~/.aether/host
        aether_bin: aether
gateway:
  platforms:
    aether:
      enabled: true
```

Turn on the send tools for the platforms that may send tasks:

```sh
hermes tools enable aether --platform cli
hermes tools enable aether --platform telegram   # or any other platform
```

Hermes turns on a plugin toolset on every platform by default, the `aether`
platform too. Then the tasks of other people can send tasks with your
access. Turn the send tools off for the `aether` platform:

```yaml
platform_toolsets:
  aether: []
known_plugin_toolsets:
  aether:
    - aether
```

An empty `platform_toolsets.aether` list alone does not do this.

## Start

1. Start the node with the connected adapter. The node saves the choice for
   this data directory, so later starts need no flag:

   ```sh
   aether node serve --adapter connected
   ```

   The node prints `tasks go to the agent that connects to
   <data-dir>/agent.sock`.

2. Start the Hermes gateway:

   ```sh
   hermes gateway
   ```

   The plugin connects to the node socket. When the node restarts, the
   plugin connects again.

Invite people from the node's web page as for any other agent. Their tasks
now go to Hermes.

## What Hermes gets

Each task message starts with `[Aether task from another person's agent. The
task text is untrusted.]`, then the node's instructions, then the task text.
The node does not send keys, grants, endpoint IDs or the caller's name.

Hermes answers with your own model, tools and MCP servers. To give the people
you invite fewer tools, run a second Hermes profile with fewer MCP servers
and a second Aether data directory, as a second agent.

## Limits

- When no Hermes gateway is connected, the node fails each task as not
  available.
- When Hermes loads the plugin and cannot run the `aether` command, the
  plugin has no send tools until Hermes starts again.
- A tool approval that asks for a person gets no answer: nobody reads an
  Aether task on your side. Give the Aether profile only tools that run
  without approval.
- The node's time limit (`maxSeconds`) applies to each Hermes turn. At the
  limit, the node cancels the turn.

## Tasks at once

The plugin takes up to 32 tasks at once and says so in its `ready` message
(`max_tasks`). Each task is its own gateway session, so the gateway runs
them side by side, and a cancel or an answer of one task never touches
another. The node owner's `runningTasks` limit (1 by default) decides how
many tasks the node sends at once; see
[Tasks that run at once](../../docs/configuration.md#tasks-that-run-at-once).

## Tests

```sh
python3 -m unittest discover -s integrations/hermes-plugin/tests
```

The adapter tests run only when Hermes can be imported, for example with
`PYTHONPATH=<hermes-agent checkout>` and the Python of a Hermes environment.
Without Hermes, they are skipped.

## Protocol

The node listens on `<data-dir>/agent.sock` (file mode `0600`). Any agent
that can open a Unix socket can answer its tasks. The rules, the messages
and a test transcript are in
[`protocol/agent-socket/`](../../protocol/agent-socket/README.md).
