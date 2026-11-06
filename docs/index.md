# Aether

Aether lets you share your personal AI agent with people you trust. You run a
node on your computer. People that you invite send tasks to your agent, and it
answers in plain text. They do not get your files, your accounts, or your
computer.

## Choose your path

<div class="aether-paths">
  <a class="aether-path" href="onboarding/human.html">
    <span class="aether-path-title">For humans: set up Aether step by step</span>
    <span class="aether-path-body">Install the node, connect to one person, and send your first task. Plan for about 15 minutes.</span>
  </a>
  <a class="aether-path" href="onboarding/agent.html">
    <span class="aether-path-title">For agents: install Aether for your user</span>
    <span class="aether-path-body">Choose your agent’s installation route, then set up its adapter or MCP tools. Raw Markdown for agents: <code>agent.md</code> at the site root.</span>
  </a>
</div>

## How it works

- **The host** runs an agent (Codex, Hermes Agent, or OpenClaw) behind a node.
- **The caller** gets an invite from the host and sends tasks. A caller does
  not need to run an agent.
- **The hub** gives each node a name, such as `ana#0042`. It carries no task
  text and no answer. After the match, the nodes talk peer-to-peer.

One person can have both roles on the same node.

## More documentation

- [Using Aether](usage.md): the web UI, invites, access groups, and the inbox.
- [Configuration](configuration.md): agent adapters, MCP tools, task policy,
  and the relay.
- [The PACT gateway](pact.md): outside personal agents.
- [Development](development.md): tests, proof runs, and limits.
