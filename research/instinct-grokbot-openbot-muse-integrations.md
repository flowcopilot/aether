# Instinct, Grok Bot, OpenBot and Meta Muse: Aether integration research

**Date: 2026-11-06. Research only; no integration was implemented or live-tested.**
Aether was read at `5e28bc7f74d64ceca3c5222de55c82f34c1b1e52`.
External documentation and repository `main` links are moving snapshots checked
on this date; pin external revisions before implementation. “Verified” below
means documented by the product owner, not independently exercised. “Not found”
describes this public-documentation search, not proof that a private API cannot exist.

## Findings

**Follow-up: how existing projects integrate (2026-11-06).** Concrete
implementations below make Muse provider experiments more actionable than the
initial public-API survey suggested. A vendor submit/result API is not the only
way: an agent can fetch assignments through a connector and post results back.
These are implementers' documented designs and test claims, not Aether live tests.

- [OurAgents.chat / Grok Bot](https://ouragents.chat/for-agents/grok-bot)
  connects through MCP and wakes a dispatcher with an authenticated routine
  webhook. The dispatcher routes an address notice to its owning Bot, which
  fetches messages and replies through tools. Saved cursors and idempotency keys
  handle repeated delivery. Scheduled polling is the fallback.
- [Codex–Grok task bridge](https://github.com/aipmer/codex-grok-task-bridge)
  exposes a durable task queue over MCP: callers create/read work; Grok claims
  it, renews a lease, reports progress and completes it. Its documented manual
  cloud-browser case is stronger evidence than its proposed unattended Routine,
  which still needs in-app validation. Its claim that public webhook waking is
  unavailable conflicts with current xAI docs and OurAgents' setup; do not copy
  that limitation as a current universal fact.
- [OurAgents.chat / Muse](https://ouragents.chat/for-agents/muse)
  uses a Muse-generated Python custom connector with Meta-managed OAuth, saved
  skills and a recurring receive task (example: every 15 minutes). It explicitly
  says wake callbacks are unavailable in its setup and token refresh in scheduled
  runs remains unconfirmed. This is a concrete bidirectional messaging recipe,
  with latency and reliability limits, rather than a hosted-Muse invocation API.
- [Paperclip's personal Muse receiver](https://github.com/paperclipai/paperclip/blob/master/doc/muse-personal-agent.md)
  uses scoped OAuth pairing, Vault-owned credentials, a local Python client and
  a managed detector hook to fetch assigned work and submit native results. It
  remains disabled by default pending real-service qualification. Its synthetic
  workflow tests do not establish live Muse reliability; stopping remote work
  is best effort and unknown effects block overlapping dispatch.
- [Grok desktop MCP](https://github.com/rthomas24/grok-bot-mcp)
  drives the signed-in macOS app through Accessibility/JXA. It reads chats,
  types messages and infers reply completion from quiet time or a question;
  operations serialize on one window. This accesses the hosted agent, but app
  changes can break it and stopping an MCP wait does not prove stopping Bot work.
- [Nightly OpenBot's integration architecture](https://github.com/nightly-labs/openbot/blob/main/docs/architecture/integrations.md)
  already describes HTTP MCP apps, OAuth sign-in/refresh and a local authenticated
  proxy that keeps upstream tokens out of the agent. Its Linear catalog entry
  is a concrete model for an Aether caller app. This still does not establish
  an inbound third-party run/cancel API.
- [WorkerKit's Instinct guide](https://workerkit.ai/instinct/setup)
  describes report delivery to the assistant's email address and explicitly
  calls this delivery rather than a connector. [Pear](https://pearmcp.com/guides/instinct)
  also marks Instinct connectivity unverified. These implementer reports leave
  Instinct as the weakest confirmed programmable target.
- [Meta's Muse Connector Platform](https://platform.muse.ai/)
  offers connector submission for distribution. This is a separate official
  route alongside custom connectors, not an inbound agent-execution API.

**Revised Aether recommendation:** prototype an authenticated task mailbox that
the hosted agent can claim/read/complete through remote MCP or a custom API
connector. Keep the local mailbox bridge attached to `agent.sock`; use Grok
routine wakes and Muse scheduled reception initially. Add explicit leases,
deadlines and late-result rejection. Validate cancellation against the real
agent: revoking a task's ability to post results cannot stop already-running
external actions. Current strict connected-adapter stop requirements remain an
acceptance gate. HTTP MCP transport/auth and executor claim/result tools would
be new work; the existing caller-only MCP tools do not supply this executor role.

- **Grok Bot has the clearest hosted-agent caller path:** its own docs explicitly
  support custom MCP servers. It also has authenticated routine webhooks, but
  these acknowledge dispatch rather than return a finished answer. A complete
  provider adapter still needs a reliable result channel and cancellation.
- **OpenBot is unresolved by name alone.** The likely working interpretation is
  `nightly-labs/openbot` / `openbot.run`, a persistent personal-agent desktop
  workspace. Confidence is medium, not enough to choose an implementation
  silently. CopilotKit/OpenBot, regnull/openbot and meetopenbot/openbot are
  materially different alternatives with different contracts.
- **Muse documents custom connectors built from APIs/CLIs**, making outbound
  Aether access plausible. No public hosted-Muse invocation/cancel contract was
  found. Muse Code has documented MCP and headless execution, but is a separate
  integration target.
- **Instinct is correctly identified, but technically opaque:** no public
  developer API, arbitrary MCP configuration, A2A/PACT endpoint or plugin SDK
  was found. Vendor contact is the practical next step.

## Aether contract and direction

Here **caller** means the user's outside agent sends work to Aether agents;
**provider** means admitted Aether tasks are answered by that user's outside
agent. These are separate capabilities.

The [README](../README.md), [socket protocol](../protocol/agent-socket/README.md),
[PACT guide](../docs/pact.md) and [Hermes plugin](../integrations/hermes-plugin/README.md)
establish three useful seams:

1. **Local caller MCP:** `aether mcp serve --caller-only` serves stdio. Its seven
   tools list/find agents, find people, send/cancel tasks and list/read the inbox.
   It omits owner administration, but still exercises the owner's existing
   caller authority. `task.send` waits up to 20 seconds, then can return a task ID
   for later reads; prompts are limited to 16 KiB and answer chunks to 32 KiB.
   Preserve `untrusted.answer` as untrusted content. This is not already a public
   HTTP MCP endpoint ([router](../crates/aether/src/mcp/mod.rs),
   [caller implementation](../crates/aether/src/mcp/tools/caller.rs)).
2. **Local provider socket:** run the node with `--adapter connected`; an agent
   bridge attaches to mode-0600 `<data-dir>/agent.sock`, sends `ready` with the
   protocol version, consumes `task`/`cancel`, and emits progress and one terminal
   result. A new agent connection replaces the old one and fails its open tasks.
   Cancellation must stop the turn immediately and suppress late answers;
   disconnect must stop and forget all active turns. Use the supplied contract
   and transcript tests, not a new ad hoc wire protocol.
3. **Public caller PACT:** Aether is a PACT Provider using A2A 1.0 over HTTPS,
   identity JWT/JWKS and OAuth device delegation approved in the owner's local
   admin UI. This is an alternative for an outside agent capable of implementing
   the entire flow. A generic A2A client or MCP client alone does not establish
   PACT compatibility. The current guide describes execution through Codex;
   do not assume every PACT/connected-adapter combination is supported without
   checking implementation. Its reported conformance has explicit limitations.

Hermes shows both directions in one plugin, a session per incoming task and a
separate restricted profile when sharing tools. Its warning to disable Aether
send tools for incoming Aether sessions applies equally to these candidates:
otherwise remote prompts can spend the owner's downstream grants or form loops.

## Instinct — Spear Street Technology / Noah Shinn

**Identity: high confidence.** The [official privacy policy](https://instinct.com/privacy-policy)
names Spear Street Technology, Inc. d/b/a Instinct and its autonomous personal
assistant. The [product site](https://instinct.com/) describes text/call interaction
and phone/computer use. [Noah Shinn's own LinkedIn profile](https://www.linkedin.com/in/noah-shinn)
associates him with Instinct; his [personal website](https://noahshinn.com/) was last
updated in 2025 and does not itself establish the current founder role. The
company/product identification is firmer than the biographical attribution.

**Verified surface:** consumer messaging, calling and acting across services.
**Not verified:** public REST/SDK, user-supplied MCP, developer plugins, A2A,
PACT, authenticated external task submission, session IDs, result callbacks or
programmatic cancellation. Searches combined the company/product name with API,
developer, integrations, MCP and A2A; public product and legal pages yielded no
technical integration contract.

**Caller proposal:** ask Instinct whether it can install a custom API/CLI/MCP
connector, or act as a PACT personal agent. Supply the Aether contract and a
read-only demonstration task. Browser access to a page is a manual feasibility
experiment, not evidence of durable protocol support.

**Provider proposal:** defer until Instinct offers an authorized ingress tied to
the owner's agent, a correlated final result and a stop operation. An SMS relay
could send text but would leave sender identity, conversation contamination,
completion, duplicates and cancellation unresolved. The [terms](https://instinct.com/terms)
also restrict account sharing; have the vendor clarify delegated third-party
task execution rather than repurposing a consumer login.

**Questions:** supported developer/partner program? Arbitrary connector
installation? Cross-vendor agent messaging? Dedicated sessions and restricted
tools for third-party callers? Background-task IDs, callbacks, stop and retention?

## xAI Grok Bot

**Identity: high confidence.** This is the persistent hosted Bot product under
the official xAI documentation domain, whose current pages brand the organization
SpaceXSI. It is distinct from Grok chat, the `@grok` social account and Grok
Build/CLI. The [overview](https://docs.x.ai/grok-bot/overview) describes persistent
named Bots and cloud computers.

**Verified caller surface:** [computer/apps documentation](https://docs.x.ai/grok-bot/computer-and-apps)
allows a custom MCP server by URL or by a command run on the cloud computer.
Connections and command servers are account-wide; all Bots share files, cookies
and command-line credentials. Separate Bot names/screens are not isolation.

**Caller implementation proposal:** first try caller-only stdio MCP with a
dedicated Aether node/data directory on the Bot computer, if installation,
process lifetime and network access permit it. This deployment is untested.
Alternatively build an authenticated, narrowly scoped HTTPS MCP facade beside
the owner's node. Do not publish its stdio/admin socket. Use separate credentials,
agent allowlists, quotas and revocation at the facade, because “caller-only”
does not restrict access to a particular saved contact. Enterprise
[network controls](https://docs.x.ai/grok-bot/security) treat remote MCP traffic
separately; the “Apply to MCP servers” switch is relevant to deployment.

**Verified provider ingress, incomplete lifecycle:** [routine webhooks](https://docs.x.ai/grok-bot/skills-routines-and-automations)
accept authenticated POSTs with optional JSON. HTTP 200 means accepted/start,
not completed; the documented result is in chat. Routines have rate limits and
can fail without automatic retry. A connected-socket bridge could dispatch a
routine carrying an opaque Aether task ID, then receive a purpose-built callback
tool invocation. **That callback workflow is a proposal, not a documented Bot
task/result API.** Require correlation, authentication, replay prevention and a
durable terminal outcome; never return webhook 200 as the agent's answer.

**Cancellation gap:** [chat documentation](https://docs.x.ai/grok-bot/chat-and-collaboration)
describes a human “Stop now” message and says it cannot undo completed actions.
No documented external task-scoped stop/result API was found. Pausing a routine
is not proof of stopping its current run. Until a stop mechanism is demonstrated,
the webhook experiment cannot claim full Aether provider-protocol compliance.

The [xAI model API](https://docs.x.ai/overview) and
[API remote MCP tools](https://docs.x.ai/developers/tools/remote-mcp) can power a
new harness. They do not grant access to the user's existing hosted Bot, memory,
cloud browser, connected accounts or subscription session. No public Grok Bot
A2A/PACT or general third-party runtime-plugin SDK was established.

## OpenBot — resolve the repository before building

**Working candidate: `nightly-labs/openbot`, medium identity confidence.** Its
[official site](https://openbot.run/) positions persistent teammates as a local
alternative to Instinct and offers Grok Bot import. Its
[upstream README](https://github.com/nightly-labs/openbot/blob/main/README.md)
describes independent sessions, workspaces, queues, cancellation and multiple
agent providers, with source under PolyForm Noncommercial. The same README warns
of full filesystem/network access without per-action approvals. Importing Grok
Bot instructions/memories is migration, not invocation of the hosted Bot.

**Caller:** the project's [provider architecture](https://github.com/nightly-labs/openbot/blob/main/docs/architecture/providers.md)
documents external plugin registration in provider adapters and a local MCP
bridge for ACP/Pi/Muse. Test a custom Aether MCP configuration against a chosen
provider first; the exact installable plugin manifest and per-agent grant path
still need source-level verification. ACP here means Agent Client Protocol,
not A2A or PACT.

**Provider proposal:** add an OpenBot-side channel/runtime bridge to the existing
Aether socket, invoking the *OpenBot-managed agent* through its scheduler. Map
each task to separate conversation/run state, collect final output and wire its
cancel to actual provider interruption. Reusing its underlying Codex/Claude/ACP
executable directly would create a different agent/session. Queue cancellation
and UI Stop controls do not prove that all tools/background descendants stop.
No stable public external submit/result/cancel SDK was established in this pass;
internal APIs require a pinned-source investigation. Workspace separation alone
does not constrain filesystem authority; prove confinement for the selected mode.

**Other real candidates:**

- [CopilotKit/OpenBot](https://github.com/CopilotKit/OpenBot) is an MIT self-hosted
  template bringing AG-UI agents into governed coworkers. It documents custom
  MCP grants and per-person containers, with optional isolated Bot containers.
  Caller integration likely needs an HTTP MCP facade; provider integration needs
  its run lifecycle, not an assumption that AG-UI equals A2A. Fresh-clone
  single-user mode admits requests as an administrator: enable authentication
  before exposure. This is a strong alternative identity, not the same project.
- [regnull/openbot](https://github.com/regnull/openbot) explicitly documents
  stdio/remote MCP, Python tools, an HTTP actor/inbox API, signed webhooks, SSE,
  thread IDs and `/runs/{id}/cancel`. This has the clearest documented two-way
  prototype if it is the intended product: caller-only MCP outbound; a local
  socket-to-actor bridge inbound, with task/thread/run correlation. Its README
  warns shell tools are unsandboxed and recommends a low-privilege deployment.
  Require `OPENBOT_API_KEY`, loopback binding, webhook signature checking and
  deduplication. Prove cancel also stops tool work and bot handoffs.
- [meetopenbot/openbot](https://github.com/meetopenbot/openbot) documents a local
  event server with publish/SSE/state endpoints and channel/thread/agent/run
  identifiers. Its README says newer work is not published there beyond the
  MIT release v0.5.3; [the linked site](https://www.getopenbot.com/) now markets a
  chief-of-staff service. Do not assume archived server APIs describe that
  hosted service or provide its stop/result guarantees.

Exclude the unrelated smartphone-robot OpenBot project from this integration.
Ask for a URL/repository or installed app publisher before committing to one
candidate; identity remains an explicit implementation gate.

## Meta Muse

**Identity: high confidence.** Meta's
[launch announcement](https://about.fb.com/news/2026/09/introducing-muse-personal-ai-agent/)
identifies the consumer personal agent and its dedicated Secure VM. It is
distinct from Muse-family models and Muse Code.

**Verified extensibility:** Meta's [security architecture](https://research.meta.ai/blog/security-and-safety-for-ai-agents-our-approach-with-muse)
says Muse can create custom connectors for services with APIs or CLIs. Its
runtime is isolated from credential storage and privileged connectors; Sentinel
controls connector actions and egress. Internal Unix sockets and harness names
in that architecture are not a public external-agent SDK.

**Caller proposal:** provide a small authenticated Aether HTTPS connector or
test installing a caller CLI in Muse's VM. These are plausible uses of documented
custom-connector capability, not verified Aether deployments. Egress permissions,
credentials, persistence across VM changes and unattended execution need testing.
A custom PACT client is another proposal, requiring identity issuance/JWKS,
audience configuration and device-flow consent. No native hosted-Muse arbitrary
MCP setting, A2A/PACT support or external invocation/result/cancel contract was
established from Meta's public sources.

**Provider:** defer hosted-Muse sharing until Meta supplies that lifecycle and
a safe third-party session boundary. Human messaging through the app/WhatsApp
does not establish a machine API or authorization for unattended relaying.

**Useful adjacent target, separately named:** [Muse Code's official extension docs](https://dev.meta.ai/docs/muse-code/extending)
document stdio/Streamable HTTP MCP and headless `muse exec` with JSONL output.
That supports a concrete caller experiment and a possible new local provider
adapter. It does not expose a user's hosted Muse VM or personal memory. Likewise
the [Meta Model API announcement](https://research.meta.ai/blog/introducing-muse-spark-meta-model-api)
documents model access, not consumer-agent access. No substitution should be
presented as a completed hosted-Muse integration.

## Practical sequence and acceptance gates

1. Resolve OpenBot's identity; obtain Instinct/Meta partner documentation and ask
   xAI for external run/result/cancel guarantees. Do not spend implementation
   effort on undocumented consumer backend endpoints.
2. Prove **caller only** first: list allowed agents, send a harmless task, retain
   a waiting task ID, resume inbox reads after reconnect, and cancel. Grok Bot
   MCP and the intended self-hosted OpenBot are the first candidates. Muse custom
   connector is exploratory. Keep PACT as an explicitly implemented alternative,
   not a compatibility claim based on generic HTTP/A2A support.
3. For a provider proof, use a dedicated account/profile and node directory;
   preserve the prescribed untrusted-task framing. Treat remote work as a separate
   session with narrow tools, no personal inbox/payment credentials and no
   automatic downstream Aether tools. Test the selected runtime rather than
   inheriting isolation claims from its model vendor.
4. Persist task-to-session/run mappings before dispatch; distinguish acceptance,
   progress and final completion. Deduplicate retries/callbacks without replaying
   side effects. Never let late replies resurrect cancelled tasks. On socket
   loss, stop active work; reconnecting must not silently redispatch it.
5. Replay Aether's transcript cases and test concurrent tasks, duplicate input,
   queue cancellation, tool interruption, human-approval stalls, timeout, process
   death, lost callbacks and disconnection. A provider that only hides late
   output while continuing actions fails the cancellation requirement. Stop
   cannot roll back prior sends or purchases.

These are implementation recommendations, not verified vendor guarantees. No
source code changes or live credentialed experiments were made. Public searches
did not establish full native A2A/PACT support for any of the four named products;
vendor answers and lifecycle tests remain necessary before claiming it.
