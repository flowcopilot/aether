# Iroh central relay and Aether network join

Checked: 2026-08-31

> This document describes the optional Aether managed-network adapter. Direct
> contacts and the open directory are Aether core scope choices and do not need
> membership, a central join API, or this relay design. Managed membership does
> not grant a local skill or workspace.

## Decision

Aether can run a central Iroh relay. The relay does not make an Aether network
by itself. It moves encrypted packets and helps peers make direct connections.
Aether still needs a join service and a capability directory.

For the first proof, put these services in one central deployment, but keep
their jobs separate:

| Service | State | Job |
| --- | --- | --- |
| Aether join API | Stateful | Redeem invitations, register members, and revoke members. |
| Aether directory | Stateful | Map skills and agent cards to Iroh Endpoint IDs. |
| Iroh relay | Stateless | Help with NAT traversal and forward encrypted traffic when a direct path fails. |

Iroh states that relays do not store application data. Clients can use more
than one relay and can fail over when one relay is not available. Iroh
recommends dedicated relays for production use. See the official
[relay model](https://docs.iroh.computer/concepts/relays) and
[dedicated infrastructure guide](https://docs.iroh.computer/deployment/dedicated-infrastructure).

## Fit with the earlier Aether design

The Aether concept,
record-layer review,
SAM review, and
technology brief make these decisions:

- Aether has separate identity and discovery, market, confidential execution,
  and receipt and trust layers.
- A2A and MCP are below the Aether trust layer. A2A handles remote agent
  delegation. MCP can expose local tools.
- Agent profiles, receipts, and vouches must become signed portable records.
  AT Protocol is the selected later record layer.
- A company network uses corporate IAM. A known partner network grows from
  existing business relationships. A public network comes later.
- Discovery is progressive. All members can see a small profile. A peer sees
  more only after the owner grants permission.

The relay and invitation design in this report does not replace those
decisions. It supplies the live network path for a small known-partner proof.
The control service acts as an index over signed records. It must not become
the only owner of those records.

Keep three identities separate. A member DID identifies the person or
organization that owns an installation. An agent DID gives one agent its own
history. An Iroh Endpoint ID identifies the current transport key. Signed
records bind the member to the agent and the agent to the endpoint. The proof
can use `did:key`. An enterprise deployment can use `did:web` linked to IAM. A
later public network can use `did:plc` for key rotation and recovery.

## What the relay does

An Iroh relay has two network jobs:

1. It gives two endpoints a first path so that they can exchange the network
   data that they need for NAT traversal.
2. It continues to forward traffic if the endpoints cannot make a direct path.

The endpoints use QUIC with end-to-end encryption. The relay cannot read A2A
messages or task data. The relay can see metadata such as source and
destination IP addresses, connection times, and the amount of relayed data.
See [Iroh relays](https://docs.iroh.computer/concepts/relays) and
[Iroh security and privacy](https://docs.iroh.computer/deployment/security-privacy).

An Iroh Endpoint ID is the public part of an Ed25519 key pair. The QUIC
connection proves the endpoint identity. The node must store the private key
and load it again after a restart. If it does not, Iroh makes a new identity on
each start. See
[creating an endpoint](https://docs.iroh.computer/connecting/creating-endpoint)
and the [endpoint model](https://docs.iroh.computer/concepts/endpoints).

## What the relay does not do

The relay does not do these jobs:

- It does not tell a user which agents are members of Aether.
- It does not search for an agent by skill.
- It does not store Agent Cards, messages, or offline tasks.
- It does not grant access to a Codex skill or workspace.

Iroh address lookup starts with a known Endpoint ID. It resolves that ID to a
home relay URL or direct addresses. Other software must first give the caller
the Endpoint ID. Iroh says this requirement directly in its
[address lookup documentation](https://docs.iroh.computer/concepts/address-lookup).

This means that a central relay does not solve offline delivery. Aether needs a
durable mailbox or task log for that feature.

## How a person joins

Aether should use an invitation flow. This flow is an Aether design, not an
Iroh protocol.

1. A member creates a single-use invitation in the Aether UI.
2. The invitation contains the Aether join API URL, a network ID, and a random
   token. It has a short expiry time.
3. The new node generates member, agent, and Iroh signing keys. It stores the
   keys in the operating system secret store or in owner-only files.
4. The node derives the member DID, agent DID, and Endpoint ID. It sends the
   public identifiers and invitation token to the join API over HTTPS.
5. The join API sends a random challenge. The node proves all three private
   keys. It signs the member-to-agent and agent-to-endpoint bindings. The join
   API verifies the proof, consumes the token, and adds the member DID to the
   registry.
6. The join API returns the relay URLs, directory URL, network trust key, and a
   short-lived membership credential.
7. The node configures Iroh with the custom relay URLs. It then publishes a
   signed Agent Card and a short directory lease.
8. Other members find the node by skill. The directory returns its Endpoint ID
   and current Iroh dialing data.
9. Iroh makes a direct path when it can. Otherwise, it keeps the encrypted path
   on the central relay.

Iroh accepts a list of custom relay URLs. Its clients can try more than one
relay. A self-hosted relay needs a public IP address, a DNS name, and TLS. See
[use your own relay](https://docs.iroh.computer/add-a-relay).

The invitation must not contain the Iroh private key, Codex credentials, local
paths, or a permanent relay secret.

## Members page

Add a `Members` page to the local Aether UI. Every authenticated member can see
all active members, including members whose nodes are offline. The page shows
a small network-visible profile. It does not show private permissions or local
paths. An administrator can use a separate filter to see revoked members. A
network administrator can also use the page to:

- Create a single-use invitation and copy its link or QR code.
- See a member name, stable DID, current Endpoint ID, join time, visible
  agents, last directory lease, and status.
- Revoke an unused invitation.
- Revoke a member and its relay access.

Show an invitation token only when the UI creates it. Do not store or show the
plain token after use. The existing `Peers` page stays separate. `Members`
controls network membership. `Peers` controls which skills and workspaces one
local node permits a member to use.

## Relay admission and node authorization

The open-source `iroh-relay` server has these access modes:

- Allow all endpoints.
- Allow only listed Endpoint IDs.
- Deny listed Endpoint IDs.
- Ask an HTTP service to approve each Endpoint ID.
- Require a shared bearer token.

The default mode allows all endpoints. Do not use that default for an Aether
relay on the public Internet.

The HTTP mode sends the authenticated Endpoint ID to the access service. This
is a good fit for Aether because the relay can ask the join API if the member is
active. It also gives Aether better member revocation than one shared token.
See the official
[`iroh-relay` configuration source](https://github.com/n0-computer/iroh/blob/main/iroh-relay/src/main.rs)
and the
[relay handshake documentation](https://docs.rs/iroh-relay/latest/iroh_relay/protos/handshake/).

Relay admission is not enough. A revoked node can still try a direct
connection. Each Aether node must also reject an unknown or revoked Endpoint
ID after the QUIC handshake. Iroh endpoint hooks can reject a connection by
remote Endpoint ID and ALPN before application data moves. See
[Iroh endpoint hooks](https://docs.iroh.computer/connecting/endpoint-hooks).

After that check, the Aether policy engine must still check the requested
skill, workspace, approval rule, and task limits. Network membership and agent
permission are different checks.

## Address lookup and capability discovery

Iroh DNS and Pkarr address lookup publish signed records. A record maps an
Endpoint ID to a relay URL or direct addresses. DNS and Pkarr do not publish an
A2A capability index. See
[Iroh address lookup](https://docs.iroh.computer/concepts/address-lookup).

The proof can avoid a separate DNS service. The Aether directory can return a
complete `EndpointAddr` with the Agent Card result. Iroh can dial that address
directly. Later, Aether can run its own DNS and Pkarr service or add a custom
Iroh address lookup provider.

The directory must verify that the agent key signs each Agent Card update and
the current Endpoint ID binding. It must also verify the member-to-agent
ownership record. It must use a short lease so that old dialing data and stale
availability do not remain active.

## First-start requirements

A new Aether node needs these items before it can accept remote work:

- One persistent Iroh secret key and its Endpoint ID.
- One persistent member signing key and stable DID.
- One persistent agent signing key and stable agent DID.
- One valid Aether invitation.
- The join API and directory HTTPS URLs.
- At least one relay URL. Use two for production.
- The Aether protocol ALPN value.
- One enabled skill and one configured workspace.
- A local policy for peer access and human approval.

The node does not need a public IP address, an inbound firewall rule, or its
own TLS certificate. The relay has the public address and certificate.

## Failure and abuse limits

Aether must design for these conditions:

| Condition | Result and required control |
| --- | --- |
| One relay stops | New paths can use another configured relay. Run at least two relays and test the switch time. |
| All relays stop | Some known peers can still connect by a direct address. New NAT traversal and relay fallback can fail. |
| Join API stops | New members cannot join. Existing peer connections do not need the join API. |
| Directory stops | Skill search and new record lookup fail. A caller with current dialing data can still try a connection. |
| Member is revoked | Relay access must fail, and every Aether node must reject the Endpoint ID. |
| Target node is offline | The relay does not queue the task. A separate durable mailbox must hold it. |
| Invitation leaks | A short expiry, one use, and server-side revocation reduce the effect. |
| Member sends too much data | Apply relay byte limits and Aether request, task, time, and cost limits. |

The public Iroh relays have traffic rate limits and no uptime guarantee. The
self-hosted server can limit inbound bytes per client and burst size. Its
current connection-accept limit fields have no effect, according to the
official
[`iroh-relay` server source](https://docs.rs/iroh-relay/latest/src/iroh_relay/server.rs.html#485-519).
Aether must add limits for invitation attempts, directory updates, concurrent
connections, active tasks, and task cost.

There is an open Iroh issue for version `1.0.0-rc.0`. It reports about 30
seconds without inbound relay access after the home relay stops. It also
reports that established relay-only connections stop. This report is not a
protocol guarantee, but it is a release risk that Aether must test with the
selected Iroh version. See
[Iroh issue 4319](https://github.com/n0-computer/iroh/issues/4319).

## Agent Client Protocol is a local protocol

The current Agent Client Protocol at `agentclientprotocol.com` defines the
connection between a coding agent and a trusted client, such as an editor. The
client starts the agent as a local subprocess. They use JSON-RPC over standard
input and output. The agent can send streamed updates and ask the client for
tool permission. See the official
[Agent Client Protocol architecture](https://agentclientprotocol.com/get-started/architecture)
and [protocol overview](https://agentclientprotocol.com/protocol/overview).

This ACP must not replace A2A between two Aether nodes. It does not define the
A2A Agent Card, remote capability discovery, or the A2A task lifecycle. A2A is
made for independent remote agents and long tasks. See the
[A2A specification](https://a2a-protocol.org/latest/specification).

ACP can fit behind one Aether node. Aether can act as the local ACP client for
an ACP-compatible coding agent. It can map an incoming A2A task to an ACP
session and send ACP permission requests to the Aether policy engine and UI.
This gives Aether a common local adapter for coding agents that implement ACP.
ACP uses absolute file paths. Aether must resolve an approved workspace ID to
an absolute path before it starts the local ACP session. A remote peer must
never supply that path.

Codex has a different current interface. Official OpenAI documentation defines
the Codex App Server as a bidirectional JSON-RPC protocol with standard input
and output, WebSocket, and Unix socket transports. It uses Codex methods such
as `thread/start` and `turn/start`. The documentation does not state that it
implements ACP. For the first proof, use the
[Codex App Server](https://learn.chatgpt.com/docs/app-server) directly. Add an
ACP adapter later for other local agents. Do not add an ACP-to-Codex translator
to the first proof.

There is also an older protocol with the same acronym. The IBM-backed Agent
Communication Protocol was for communication between remote agents. Its
official repository says that the work moved into A2A, and the repository is
archived. Do not use it as a separate Aether protocol. See the archived
[Agent Communication Protocol repository](https://github.com/i-am-bee/acp).

## Recommended proof boundary

Use one central host for the first test. Run an Aether join and directory
service plus one Iroh relay on it. Give two users single-use invitation links.
Prove that both nodes register, find each other by skill, and complete one A2A
task. Then stop direct UDP traffic and prove that the same encrypted request
works through the relay.

Add a second relay before a production test. Do not add an offline mailbox to
the first network proof. Test that feature as a separate step because a relay
does not provide persistence.
