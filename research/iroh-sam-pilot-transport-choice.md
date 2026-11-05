# Iroh, SAM, and Pilot Protocol for Aether

Checked: 2026-08-31

Status: research only. The active architecture now uses Iroh and defers Pilot
Protocol and SAM. See the
[Aether connection architecture](../references/a2a-codex-connection-flow.md).

## Research recommendation before the active architecture decision

Use Pilot Protocol for the first transport trial. Keep Iroh as the fallback.
Do not use SAM for this trial.

Pilot now supplies most of the network functions that Aether needs for the
first proof. It has a local daemon, a central registry and beacon, private
networks, invitations, member lists, peer trust, NAT traversal, and encrypted
relay fallback. Its own guide also puts A2A above the Pilot tunnel. This is a
closer fit than the older Aether research described.

Do not make Pilot part of the Aether application contract. Run its Go daemon as
a separate process. Put it behind an Aether transport interface. A2A must still
define Agent Cards, tasks, status, and artifacts. Aether must still define
workspace permission, skill permission, approval, and network-visible profile
rules.

Adopt Pilot for the proof only if a short test confirms these points:

1. A self-hosted registry and beacon support an invite-only private network.
2. The local node can list all members through a stable machine interface.
3. An A2A request and result can cross one Pilot reliable stream without a
   Pilot-specific message format.
4. The local daemon can run with automatic updates, skill injection, and
   optional telemetry disabled.
5. A license review accepts the AGPL-3.0 daemon and the planned distribution
   method.

If one of these checks fails, use Iroh. The Iroh design is smaller and has less
project risk, but Aether must then build the join service, roster, and network
policy link.

## Why the older files selected SAM

The older SAM review tests an enterprise deployment. It
values OIDC, Biscuit capabilities, Kubernetes, and a self-hosted control plane.
For that scope, SAM is still a strong later option.

The current [Aether connection design](../references/a2a-codex-connection-flow.md)
tests a small partner network between two local nodes. It uses A2A and a local
permission UI. These are different test conditions.

There is also a new SAM fact. SAM removed its A2A service on 2026-06-23. The
current service types are MCP and inference. Thus, Aether would need a bridge or
a new SAM service type before it could keep the chosen A2A contract. See the
[current service type definition](https://github.com/google/sam/blob/main/api/sam.proto#L102-L106)
and the [A2A removal commit](https://github.com/google/sam/commit/e0a4d86c948ef1d54094cf52b6d5706c6c31b167).

## Current project state

| Project | Current release | Language and license | Maintenance signal |
| --- | --- | --- | --- |
| Iroh | `v1.1.0`, 2026-08-25 | Rust, MIT or Apache-2.0 | Stable 1.x project. The repository started in 2022 and has several active maintainers. |
| SAM | `v0.1.0-alpha.7`, 2026-08-16 | Go 1.25, Apache-2.0 | Active but alpha. The repository states that it is not an officially supported Google product. |
| Pilot | `v1.13.9`, 2026-07-29 | Go 1.25, AGPL-3.0 | Active but young. Most core work comes from two named maintainers. Enterprise networks are early access. |

Sources: [Iroh release](https://github.com/n0-computer/iroh/releases/tag/v1.1.0),
[Iroh repository](https://github.com/n0-computer/iroh),
[SAM releases](https://github.com/google/sam/releases),
[SAM repository and support notice](https://github.com/google/sam),
[Pilot release](https://github.com/pilot-protocol/pilotprotocol/releases/tag/v1.13.9),
and [Pilot repository and license](https://github.com/pilot-protocol/pilotprotocol).

Release numbers do not prove reliability. They only show the current release
state. Iroh has the lowest maintenance risk of these three. SAM has the highest
release-stage risk. Pilot has a working release line, but its small maintainer
group and central coordination services add supplier risk.

## Transport and NAT behavior

### Iroh

Iroh is an embedded Rust library for authenticated QUIC connections. An
Endpoint ID is the public part of an Ed25519 key pair. An application selects
its protocol with ALPN and then uses QUIC streams or datagrams. See the
[Iroh endpoint model](https://docs.iroh.computer/concepts/endpoints).

Iroh first uses a relay to exchange network data for NAT traversal. It then
tries a direct path. If the direct path fails, the relay continues to forward
end-to-end encrypted traffic. The relay does not store application data. See
[Iroh relay behavior](https://docs.iroh.computer/concepts/relays).

### SAM

SAM is a full libp2p mesh. A control plane enrolls nodes. Routers supply
bootstrap, GossipSub routing, relay, and forwarding. A local `sam-node` supplies
mesh access and an MCP sidecar. See the
[SAM architecture](https://github.com/google/sam#architecture-components).

This gives more than a transport library. It also adds its own identity,
service, and policy model.

### Pilot Protocol

Pilot uses its own UDP overlay. It has virtual addresses, datagrams, reliable
streams, Ed25519 identity, X25519 key exchange, and AES-256-GCM tunnels. A
registry and beacon coordinate address lookup, STUN, hole punching, and relay
fallback. See the
[Pilot repository architecture](https://github.com/pilot-protocol/pilotprotocol#architecture)
and [security model](https://pilotprotocol.network/docs/security).

Pilot also has a compatibility mode. It uses outbound WSS on TCP port 443 when
UDP is not available. See the
[Pilot start guide](https://pilotprotocol.network/docs/getting-started).

## Membership and discovery

### Iroh

Iroh address lookup starts with a known Endpoint ID. It maps that ID to relay
and direct addresses. DNS and Pkarr records are signed. Optional local and DHT
lookup methods exist. Iroh does not enumerate an Aether network or search for
an agent skill. See
[Iroh address lookup](https://docs.iroh.computer/concepts/address-lookup).

Thus, the current Iroh plan needs an Aether join service, membership registry,
skill directory, and Members page.

### SAM

SAM enrolls a node through its control plane. Nodes advertise MCP and inference
services through the mesh. The catalog is also a protected service. It must
have an explicit permission. See the
[SAM policy reference](https://github.com/google/sam/blob/main/site/content/docs/development/policy.md).

This is service discovery. It is not A2A Agent Card discovery. SAM does not
define the Aether member profile or its visibility rules.

### Pilot Protocol

Pilot private networks support open, invite-only, and token-gated join rules.
The registry stores membership. `pilotctl network members` returns a member
list. This is the closest existing match for the planned Aether join flow and
Members page. See the
[Pilot network guide](https://pilotprotocol.network/docs/networks).

Pilot network membership grants connectivity. It does not grant permission to
run a skill, read a workspace, spend money, or disclose data. Aether must keep
those checks in its local policy engine.

## Authorization

Iroh authenticates the remote Endpoint ID during the QUIC handshake. Endpoint
hooks can reject a connection before application data moves. Aether must supply
all member, skill, workspace, and task rules. See
[Iroh endpoint hooks](https://docs.iroh.computer/connecting/endpoint-hooks).

SAM has the strongest built-in enterprise authorization. The control plane
checks OIDC identity and issues a Biscuit capability. The destination node
checks the capability and local restrictions. Access is denied by default. See
the [SAM policy reference](https://github.com/google/sam/blob/main/site/content/docs/development/policy.md).

Pilot uses signed bilateral handshakes or shared network membership to control
peer connectivity. Its enterprise mode adds network roles and port rules. The
Pilot documentation states that applications remain responsible for task,
tool, data, spend, and approval rules. See the
[Pilot trust guide](https://pilotprotocol.network/docs/trust) and
[Pilot network guide](https://pilotprotocol.network/docs/networks).

## A2A fit

Iroh is neutral. Aether can carry A2A HTTP and JSON data over one ALPN-selected
QUIC stream. This has a good Rust fit because the node can embed Iroh and the
official A2A Rust SDK in one process.

SAM is MCP-first. It no longer has an A2A service type. SAM can remain a later
enterprise transport, but it needs an A2A adapter.

Pilot is also below A2A. Its reliable streams can carry A2A data. Pilot has a
first-party guide for Agent Cards and A2A JSON-RPC over its tunnels. See
[A2A over Pilot](https://pilotprotocol.network/blog/a2a-agent-cards-over-pilot-tunnels).

The Pilot page does not make Pilot an A2A implementation. Aether must still use
the A2A specification and SDK for message and task rules.

## Deployment weight

Iroh needs the embedded crate and one relay for the small proof. A production
test should have two relays. Aether must add its own central join and directory
service.

SAM can use its public testnet for a small test. Its documented private
production setup has an OIDC provider, control plane, PostgreSQL, router,
console, DNS synchronization, nodes, and Kubernetes or Helm. See the
[SAM production guide](https://github.com/google/sam/blob/main/site/content/docs/user/kubernetes-deployment.md)
and [Helm values](https://github.com/google/sam/blob/main/charts/sam-mesh/values.yaml).

Pilot needs a local daemon on each member computer. A self-hosted network also
needs a registry and beacon. The same system already supplies the network
roster and trust workflow. Aether can call the daemon through its Unix socket
or through `pilotctl`. The official SDK table does not list Rust, so a Rust
node needs a process adapter. See the
[Pilot programmatic access table](https://github.com/pilot-protocol/pilotprotocol#programmatic-access-sdks).

## Use each option at the correct boundary

| Test condition | Choice |
| --- | --- |
| Fast proof of friend join, roster, NAT traversal, and A2A traffic | Pilot, subject to the five checks in this report |
| Small Rust transport with the lowest dependency risk | Iroh |
| Enterprise mesh with OIDC, Biscuit policy, Kubernetes, and MCP services | SAM as a later adapter |

The important boundary is simple. A2A is the agent contract. Aether owns agent
profiles and local authorization. The selected transport only moves encrypted
bytes and supplies network reachability.
