# Block Buzz: agent discovery, delivery, and wake behavior

Checked against the Block Buzz source at commit
[`bc006f6`](https://github.com/block/buzz/commit/bc006f67087b049e2f9c4d8a2f26faceff628225)
on 2026-08-31. Buzz is an active Rust project under the Apache-2.0 license.
Its own architecture document calls the product a self-hosted workspace for
people and agents, not a general agent protocol.
([architecture](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L3-L17))

## Result

Buzz gives a useful model for a live agent inbox. It does not give a complete
offline mailbox or a cold-start service.

There are two different wake levels:

1. If `buzz-acp` is active, it keeps a WebSocket connection to the relay. An
   incoming event goes to the harness without HTTP polling. With `--lazy-pool`,
   the harness can delay the ACP and LLM subprocesses. The first accepted event
   starts the pool. This reduces idle model cost, but the harness and network
   connection must stay active.
   ([harness options](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/README.md#L122-L130))
2. If the complete harness is stopped, Buzz does not start it when a relay event
   arrives. The relay stores persistent events, but it has no control channel to
   start remote compute. A process manager, container platform, operating-system
   service, or another active daemon must start `buzz-acp`.

This distinction is central for Aether. A WebSocket removes request polling. It
does not remove the need for one active process.

## Architecture and protocol ownership

Buzz uses Nostr NIP-01 as both its event format and its client-to-relay wire
protocol. Clients publish signed events and open filtered subscriptions over
WebSocket. The relay verifies, stores, and fans out events. Postgres holds the
event log and relational state. Redis carries presence and cross-process
fan-out. The relay is the single source of truth. Buzz has no peer-to-peer event
exchange, gossip, or relay replication.
([system architecture](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L21-L71))

The layers are:

| Layer | Buzz component | Function |
| --- | --- | --- |
| Data substrate | Nostr events plus Postgres | Signed, durable workspace record |
| Live transport | Nostr WebSocket subscriptions | Publish, query, and live fan-out |
| Agent bridge | `buzz-acp` | Converts relay mentions to ACP prompts |
| Local agent session | ACP over standard input and output | Starts and controls Goose, Codex, Claude Code, or another ACP agent |
| Tool access | MCP | Gives tools to the local agent session |
| Automation | Buzz workflows | Runs event, webhook, or schedule triggers |

These layers are not interchangeable. Nostr is the data and relay substrate.
ACP is the local host-to-agent protocol. MCP is the agent-to-tool protocol.
Buzz does not use A2A for remote agent messages or task state.

## Discovery

Buzz discovery is community-local and access-controlled. The desktop first
gets relay-signed channel membership events for channels that the viewer can
see. It uses bot members as candidates. It then reads each candidate's signed
kind `10100` directory profile, kind `0` identity profile, and owner-signed
kind `30177` policy.
([directory source](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/desktop/src-tauri/src/commands/agent_discovery/relay_directory.rs#L127-L205))

This is roster discovery, not open capability discovery. It answers, "Which
agents are members of channels that I can use?" It does not define a public
registry, a global search network, or A2A skill matching. The agent profile can
contain a `capabilities` array, but the current directory path uses channel
membership as its candidate source.

The `buzz-acp` harness also performs channel discovery. At start, it asks the
relay REST API for accessible channels and subscribes to them. This is discovery
of message scopes for one known agent. It is not discovery of another agent.
([harness flow](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/README.md#L251-L260))

## Identity, authentication, and permission

Each person and agent has a Nostr secp256k1 keypair. The event author signs each
event. WebSocket clients answer a NIP-42 challenge. HTTP clients use NIP-98.
The relay checks authentication before it accepts events or subscriptions.
([connection lifecycle](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L179-L208))

NIP-AA lets an agent present an owner-signed NIP-OA credential during NIP-42
authentication. The relay can then give the agent virtual membership if the
owner is a current member. This does not create a permanent agent membership
row.
([NIP-AA](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/docs/nips/NIP-AA.md#L11-L32))

Channel membership is the content access gate. The relay checks access before
it registers a private-channel subscription. The harness adds a second inbound
gate with `owner-only`, `allowlist`, `anyone`, and `nobody` modes. `owner-only`
is the default.
([relay access check](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L311-L326),
[harness author gate](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/README.md#L132-L162))

## Messaging and inbox behavior

A normal agent wake message is a channel event with the agent public key in a
`p` mention tag. The relay stores a persistent event before it sends the event
to matching live subscriptions. A Nostr `REQ` also returns stored matching
events before the connection changes to live delivery.
([wire messages](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L146-L161),
[historical query](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L324-L326))

Inside the harness, accepted events enter an in-memory queue for each channel.
Only one prompt can run for a channel. The harness can batch queued events into
one ACP `session/prompt`. It detects a failed agent subprocess and starts a new
one. The architecture document is explicit that `buzz-acp` does not persist its
state.
([harness architecture](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L644-L674))

Thus, the relay is a durable event store, but it is not a reliable per-agent
mailbox. Buzz does not define a durable delivery cursor, claim, receipt, lease,
or acknowledgement for each mentioned agent. A stored message can exist even
when no harness has accepted it.

The current README says that startup replays all unprocessed mentions from the
last run. The current source does not support that broad statement. It sets the
first subscription watermark to the current startup time, with a five-second
skew allowance. The source also says that events before startup are not
delivered again. Therefore, reconnect catch-up protects an interruption in one
active harness process. It does not provide general catch-up after an arbitrary
offline period.
([startup watermark](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/src/lib.rs#L1989-L2017),
[subscription rule](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/src/relay.rs#L3240-L3283),
[source comment](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/src/relay.rs#L600-L611))

## Scheduling and MCP

Buzz has two time-based mechanisms. An optional harness heartbeat prompts an
idle agent to inspect feed actions and mentions. It is disabled by default. A
heartbeat is skipped if all agents are busy. This is periodic agent work, not a
delivery guarantee.
([heartbeat behavior](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-acp/README.md#L183-L217))

The workflow engine supports event, webhook, and schedule triggers. Its cron
loop checks schedules every 60 seconds. This can start a workflow inside an
active relay service, but it does not start a stopped remote agent harness.
([workflow scheduler](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/ARCHITECTURE.md#L509-L554))

MCP stays local to agent execution. Buzz merges pack-level and persona-level MCP
server definitions and sends them in the ACP `NewSessionRequest`. The defined
transports are standard input and output and Streamable HTTP. MCP does not carry
the relay message and does not supply agent discovery.
([MCP configuration](https://github.com/block/buzz/blob/bc006f67087b049e2f9c4d8a2f26faceff628225/crates/buzz-persona/PERSONA_PACK_SPEC.md#L426-L486))

## Comparison with Aether, A2A, and Iroh

| Need | Buzz | Proposed Aether design |
| --- | --- | --- |
| Capability discovery | Community channel roster plus Buzz profiles | Aether directory indexes A2A Agent Cards and skills |
| Application messages | Buzz-specific signed event kinds | A2A messages, tasks, status, and artifacts |
| Network transport | Client-server WebSocket through one Buzz relay | Iroh encrypted QUIC connection to a known Endpoint ID |
| Durable data | Relay Postgres event log | Not supplied by A2A or Iroh |
| Live wake | Active `buzz-acp` subscription | Active Aether daemon on an Iroh stream or another push channel |
| Cold wake | External process or platform must start the harness | Still needs an external supervisor or mailbox notification |
| Local tools | MCP through ACP | MCP can stay inside each agent host |

A2A is a better public contract for Aether because its Agent Card describes
skills, supported interfaces, security, and update features. A2A supports task
polling, streams, and HTTP webhooks, but it does not define one registry API or
a machine wake service.
([A2A discovery](https://a2a-protocol.org/latest/topics/agent-discovery/),
[A2A update delivery](https://a2a-protocol.org/dev/specification/#35-task-update-delivery-mechanisms))

Iroh is a transport, not a mailbox. Its address lookup finds connection data
for a specific Endpoint ID. Its relays are stateless and do not store
application data. Therefore, an Iroh relay cannot hold an A2A message until a
stopped Aether node starts.
([Iroh address lookup](https://docs.iroh.computer/about/faq#what-is-address-lookup-in-iroh-and-which-one-should-i-enable),
[Iroh relays](https://docs.iroh.computer/concepts/relays#why-relays-are-better))

## Recommendation for the proof of concept

Keep the first proof small, but make the wake boundary explicit:

1. Use the Aether directory for A2A Agent Card and skill search.
2. Use A2A for the message and task model.
3. Use Iroh for a direct connection to a known Aether node.
4. Run one small Aether daemon for each online node. It owns the Iroh endpoint
   and starts or calls the local agent when work arrives. This is the Buzz
   `buzz-acp` pattern without the Buzz workspace protocol.
5. Do not claim offline delivery in this proof. Return a clear `offline` result
   if the target daemon cannot connect.

For the next step, add a durable mailbox service with a per-recipient sequence,
acknowledgement, retry state, expiry, and idempotency key. Notify the resident
daemon through a live stream. If no daemon is active, use an external process
manager or platform notification to start it. Store the full A2A envelope, not
a second Aether task model.

Buzz gives the correct lesson: keep the expensive model process lazy, but keep a
small network process active. Its relay event log and WebSocket fan-out are good
design references. Its community roster, Buzz event kinds, and ACP session
bridge should not replace A2A Agent Cards, A2A task semantics, or the Aether
directory.
