# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

**Primary, now: a person who shares their personal AI agent with people they trust.** They run an Aether node on their own macOS or Linux computer, with Codex or Hermes Agent behind it. They invite friends or colleagues by name (`ana#0042`), decide who can use the agent, see what each person asked last, and revoke access when they want.

**The caller** is the other side of the same relationship: a person who received an invite, pastes the `aether-invite:` line, and sends tasks to the host's agent from the web UI, the CLI (`aether node request`), or their own agent through the Aether MCP server. A caller does not need to run an agent.

**Service-agent owners** put a public listing on the hub so that people can find the agent by search and ask it for access.

**Later, not now: organizations** that publish agents, capabilities, and approved knowledge to other organizations for discovery, negotiation, meeting coordination, and lead creation (`brief.md`). Design work serves the personal case first. The organization network is roadmap.

## Product Purpose

Aether lets you share your personal AI agent with people you trust, without giving them your computer, your files, or your accounts. Success today: two people connect in the direct scope without files or signing commands, the caller gets plain-text answers, and the host stays in control of who has access.

The long-term purpose is a network where agents discover each other, request work and knowledge from each other, and build trust from the record of those transactions.

## Positioning

- **Private by design.** The hub matches names and lists service agents. It carries no task text, no messages, and no mailbox; after the match, nodes talk peer-to-peer over Iroh. With Codex, each task runs in a new empty folder without network access, and the node removes the folder when the task ends. `~/.codex`, `~/.ssh`, the host's MCP servers, and memories stay out of reach.
- **Permission makes the link.** A connection exists only through a signed invite. One action revokes it, and the next request gets `permission denied`. Callers do not see which agent answers.
- **Any personal agent.** Aether is agent-neutral: Codex today, Hermes Agent through the connected adapter, outside personal agents through the PACT gateway, with Claude Code and OpenClaw as stated goals.
- **Trust from receipts (roadmap, not built).** A vouch comes from a signed payment receipt, not from a free-standing review, so reputation costs real money to fake. Personalized PageRank runs over the vouch graph, and agents query the scores at run time.

## Operating Context

- **The node web UI** is served by `aether node serve` on a local HTTPS URL with a self-signed certificate. Pages: **Sharing** (Invite someone, access groups, permissions), **Agents you can use** (the connect code, connected agents), **Hub**, **Advanced** (task policy, relay), plus listing and PACT pages. It is an operational tool.
- **The hub landing page** (`server/priv/static/index.html`) is public. It shows the listed service agents, whether they are online, their owners, and only a count of people. It has search.
- **The connect flow:** the caller copies the connect code from **Agents you can use**; the host pastes it into **Invite someone**, enters a name, and selects **Create invite**; the host sends the `aether-invite:…` line; the caller pastes it and selects **Connect**.
- **Other entry points:** the `aether` CLI, the Aether MCP server for Codex, and the Hermes plugin.
- **Relay:** a new node uses `https://aether-relay.tudorandrei.xyz` by default; the host can set another relay or use only the local network.

## Capabilities and Constraints

- The node UI is Rust (axum + Topcoat components, Tailwind CSS from `crates/aether/styles.css`). The hub is Elixir (Plug, Bandit, Ecto/SQLite), and its landing page is one static HTML file that the router reads at compile time.
- **The node UI must work offline.** It is served on loopback, so it uses system typefaces and no external assets.
- Product UI is English.
- Terms to keep: node, host, caller, hub, relay, invite, connect code, access group, listing, service agent, task, task policy, scope (`direct`), adapter, PACT.
- Version 0.2.9 and older cannot connect to a hub on the new protocol.
- **Undecided:** the organization-to-organization network, the market layer (order book), confidential execution, payments, and the trust score. None of them is a shipped capability.

## Brand Commitments

- Name: **Aether**. The hub page is titled "Aether Hub".
- **Copy is plain and simple:** short sentences, one idea each, direct address ("Your agent gets no access. You can close this page."). Match the README and `docs/`.
- The film `promo/signal/` gives a colour meaning that exists as a brand idea: grey for an agent alone, red for a wall (NAT, firewall, refused request), the spectrum for a connection that only a signed permission produces. Each agent's sigil is drawn from its 32-byte endpoint key. These are a reference, not yet a binding rule for the product UI.

## Evidence on Hand

- `promo/signal/out/aether-signal.mp4` and `promo/signal/out/stills/`: the 78-second film, with its score in `promo/signal/public/score.wav`.
- `README.md`, `docs/usage.md`, `docs/configuration.md`, `docs/pact.md`, `docs/development.md`: real commands, flows, and measured results.
- `research/`: protocol research.
- **Absent:** customers, testimonials, usage numbers, case studies, and press. Future work must not invent any of these.

## Product Principles

1. **The owner stays in control.** Every surface makes clear who has access, what they can reach, and how to take it back.
2. **Nothing leaves without consent.** Show what is shared before it is shared ("Nothing is sent until you select Confirm"). Never imply that the hub or the network sees task content.
3. **State only what ships.** Roadmap ideas (organizations, market, trust scores) do not appear as current features.
4. **Plain words over protocol words.** Show the person and the action first; keys, scopes, and protocol detail come second.
5. **Agent-neutral.** No surface assumes one agent vendor.
