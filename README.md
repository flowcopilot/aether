# Aether

Aether lets you share your personal AI agent with people you trust. The goal
is to support any agent that people use as a personal assistant, for example
Codex, Claude Code, OpenClaw, or Hermes Agent. Today, a node runs Codex, or an
agent that connects to the node: Hermes Agent with the plugin in
[integrations/hermes-plugin](integrations/hermes-plugin/README.md), or
OpenClaw with the plugin in
[integrations/openclaw-plugin](integrations/openclaw-plugin/README.md).

You run a node on your computer. People you invite send tasks to your agent,
and it answers in plain text. The people who send tasks do not see which
agent answers. With Codex, each task runs in a new empty folder without
network access, and the node removes the folder when the task ends.

## Install

You need macOS or Linux, [mise](https://mise.jdx.dev), and an agent: the
[Codex CLI](https://github.com/openai/codex) (the default),
[Hermes Agent](integrations/hermes-plugin/README.md) or
[OpenClaw](integrations/openclaw-plugin/README.md).

```sh
git clone https://github.com/flowcopilot/aether.git
cd aether
mise install
mise run install-aether
```

This builds the release binary and copies it to `~/.local/bin/aether`. Make
sure that `~/.local/bin` is on your `PATH`, then check the install:

```sh
aether --help
```

On an Apple Silicon Mac, you can download the release binary instead:

```sh
gh release download -R flowcopilot/aether -p aether-macos-arm64
install -m 755 aether-macos-arm64 ~/.local/bin/aether
```

Version 0.2.9 and older cannot connect to a hub that uses the new protocol.
If the page shows "The hub cannot be reached now", build from source.

## Start your node

```sh
aether node serve
```

The node prints a local HTTPS URL. Open it in your browser and accept the
certificate warning. If the node prints a `codex login` command, run it once
and start the node again.

To answer with Hermes Agent or OpenClaw instead of Codex, start the node
with `aether node serve --adapter connected` and follow the
[Hermes plugin README](integrations/hermes-plugin/README.md) or the
[OpenClaw plugin README](integrations/openclaw-plugin/README.md). The node keeps the
choice for its data directory.

## Connect to another person

1. The caller opens **Agents you can use** and copies the connect code.
2. The host pastes the code into **Invite someone**, enters a name, and
   selects **Create invite**.
3. The host sends the `aether-invite:…` line to the caller.
4. The caller pastes the line and selects **Connect**.

The caller can now send a task:

```sh
aether node request --scope direct --agent-id 'tudor#0042' --text 'Give me three project name ideas'
```

## Use Aether from Codex

This repository's `.codex/config.toml` starts the Aether MCP server. Keep
`aether node serve` running, restart Codex in this folder, and run `/mcp` to
see the Aether tools. Then ask Codex to send a task or manage your invites.

## More documentation

| File | Contents |
| --- | --- |
| [docs/usage.md](docs/usage.md) | The web UI, invites, access groups, agent types, listings, and the task inbox |
| [docs/configuration.md](docs/configuration.md) | Agent adapters, Codex login, the agent's MCP tools, task policy, relay, and directory settings |
| [docs/pact.md](docs/pact.md) | The PACT gateway for outside personal agents |
| [docs/development.md](docs/development.md) | Tests, local proof runs, container checks, measured results, and limits |
| [LOCAL-TEST.md](LOCAL-TEST.md) | Two nodes on one machine, step by step |

## License

Aether is licensed under the [Apache License 2.0](LICENSE).
