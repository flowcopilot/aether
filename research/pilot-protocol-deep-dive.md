# Pilot Protocol deep dive for Aether

Checked: 2026-08-31

Status: research only. Pilot Protocol is not part of the active proof. The
project keeps this report for a later transport review.

## Research recommendation before the active architecture decision

Pilot Protocol is a good candidate for a limited Aether transport trial. It is
not ready to replace Iroh in the Aether design.

Pilot supplies these useful parts:

- a local daemon;
- stable node addresses;
- a central registry and member roster;
- network join, invite, and trust flows;
- direct peer traffic through NAT;
- an encrypted relay path when direct traffic fails;
- reliable byte streams that can carry A2A traffic.

Pilot does not supply these required Aether parts:

- current A2A v1.0 behavior;
- Aether skill, workspace, task, and spend authorization;
- an offline store-and-forward mailbox;
- a supported Rust SDK;
- a documented production process for a self-hosted registry and beacon.

The first trial must run Pilot as a separate Go process behind an Aether
transport interface. Do not add Pilot types to the Aether domain model. Keep
Iroh as the fallback until all trial tests in this report pass.

## Source scope

This report uses Pilot release `v1.13.9`, dated 2026-07-29, and current official
source and documentation available on 2026-08-31. The main repository uses Go
1.25.12 and several separately versioned Pilot modules. See the
[release](https://github.com/pilot-protocol/pilotprotocol/releases/tag/v1.13.9),
[module file](https://github.com/pilot-protocol/pilotprotocol/blob/main/go.mod),
and [wire specification](https://github.com/pilot-protocol/docs/blob/main/SPEC.md).

The IETF document is an active individual Internet-Draft. It is not an IETF
standard and it has no IETF endorsement. It describes Pilot as an experimental
overlay below A2A and MCP. It also states that the registry and beacon are
central services. See the
[IETF Datatracker record](https://datatracker.ietf.org/doc/draft-teodor-pilot-protocol/).

## Architecture and wire transport

The data path is:

```text
Aether -> local IPC -> Pilot daemon -> UDP or WSS -> peer Pilot daemon -> Aether
                                  \-> beacon relay, when direct UDP fails

Pilot daemon -> registry: address, directory, trust, and membership control
Pilot daemon -> beacon: endpoint discovery, hole punch, and relay control
```

Pilot is a custom network and transport overlay. It is not QUIC. Its main
transport uses UDP. Its compatibility mode uses WebSocket Secure on TCP port
443 for networks that block UDP. The beacon first helps the peers to find their
public endpoints and to punch through NAT. It relays encrypted frames if the
direct path fails. See the
[protocol architecture](https://github.com/pilot-protocol/docs/blob/main/SPEC.md)
and [beacon source](https://github.com/pilot-protocol/beacon).

A Pilot address has a 16-bit network ID and a 32-bit node ID. A port is 16
bits. The binary packet header is 34 bytes. It contains the protocol version,
flags, protocol type, payload length, source and destination addresses, source
and destination ports, sequence and acknowledgment values, receive window, and
CRC32. The protocol types are reliable stream, datagram, and control. See the
[address code](https://github.com/pilot-protocol/common/blob/main/protocol/address.go),
[header code](https://github.com/pilot-protocol/common/blob/main/protocol/header.go),
and [packet code](https://github.com/pilot-protocol/common/blob/main/protocol/packet.go).

Tunnel frames use these four-byte markers:

| Marker | Purpose |
| --- | --- |
| `PILT` | Plain test frame |
| `PILS` | AES-256-GCM encrypted frame |
| `PILK` | Anonymous X25519 key exchange |
| `PILA` | Ed25519-authenticated X25519 key exchange |
| `PILP` | NAT punch frame |

The authenticated tunnel derives keys with HKDF-SHA256. The encrypted frame
contains the sender node ID, a 12-byte nonce, ciphertext, and a 16-byte tag.
The reliable stream implements SYN, ACK, FIN, and RST state, sequence numbers,
retransmission, selective acknowledgment, flow control, and congestion
control. See the [wire specification](https://github.com/pilot-protocol/docs/blob/main/SPEC.md)
and [tunnel code](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/pkg/daemon/tunnel.go).

## Registry, beacon, relay, and self-hosting

The rendezvous registry is the central control plane. It uses length-prefixed
JSON messages over TCP, normally on port 9000. It stores registrations,
membership, trust links, directory data, and routing state. A JSON store, a
write-ahead log, and leader/follower replication make the registry state
durable. An admin HTTP API and dashboard normally use port 3000. See the
[rendezvous repository](https://github.com/pilot-protocol/rendezvous).

The beacon normally uses UDP port 9001. It supplies STUN-like endpoint
discovery, coordinated hole punch, encrypted relay fallback, and state gossip
between beacons. Its node endpoint map is in memory. Daemons must register
again after a beacon restart. The relay forwards frames. It does not store
them. It drops traffic for an unknown destination. See the
[beacon repository](https://github.com/pilot-protocol/beacon) and
[beacon server](https://github.com/pilot-protocol/beacon/blob/main/server.go).

The source can build separate `rendezvous` and `beacon` binaries. The Pilot
README also shows a local rendezvous command. However, both server repository
READMEs say that they are for source audit and are not self-hosting guides.
Thus, source availability proves that self-hosting is possible at the code
level. It does not prove that production self-hosting is supported. Aether
must test deployment, backup, restore, TLS, upgrades, and failover.

The published default services are run by Vulture Labs at
`34.71.57.205:9000` and `34.71.57.205:9001`. This default is useful for a quick
test, but it does not meet the Aether private control-plane goal. See the
[daemon defaults](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/cmd/daemon/main.go),
[registry README](https://github.com/pilot-protocol/rendezvous), and
[beacon README](https://github.com/pilot-protocol/beacon).

## Private join and member list

Pilot networks have `open`, `token`, and `invite` join rules. The registry
persists the network record, members, roles, tags, policy, join token, and admin
token. Roles are owner, admin, and member. An invitation is valid for 30 days,
and one inbox can hold 100 invitations. See the
[network guide](https://pilotprotocol.network/docs/networks) and
[membership source](https://github.com/pilot-protocol/rendezvous/blob/main/membership/membership.go).

The source has an important condition that the general network description can
hide: an invite-only network requires enterprise mode. Aether must not assume
that a normal private network supports the planned invitation flow. The trial
must create an enterprise invite network or test a token network as a second
choice.

The local daemon IPC has commands to list, join, and leave networks. It also
has commands to list members, invite a node, poll invitations, and respond to
an invitation. Thus, the Aether node UI can show the local member roster. The
registry remains the source of this roster. The documentation states that
some cached directory and member data can lag by up to five minutes. See the
[driver network commands](https://github.com/pilot-protocol/common/blob/main/driver/driver.go)
and [network guide](https://pilotprotocol.network/docs/networks).

Network membership grants connectivity. It does not grant a peer permission
to use a skill, read a directory, change a repository, spend money, or approve
a task. Aether must apply these rules after it authenticates the remote node.

## Identity and trust

Each node has a persistent Ed25519 identity file, normally at
`~/.pilot/identity.json`. The registry maps the public key to the Pilot node
address. A tunnel uses an ephemeral X25519 key. The peer signs that key with
its Ed25519 identity. Application frames use AES-256-GCM after key derivation.
See the [identity code](https://github.com/pilot-protocol/common/blob/main/crypto/identity.go)
and [security guide](https://pilotprotocol.network/docs/security).

Two nodes can establish bilateral trust through signed registry messages. A
shared network can also grant reachability. The registry is still a trusted
party for address, public-key, membership, and route data. The relay can see
traffic metadata and ciphertext, but it cannot read an encrypted application
payload. See the [trust guide](https://pilotprotocol.network/docs/trust) and
[IETF security considerations](https://datatracker.ietf.org/doc/draft-teodor-pilot-protocol/).

The default local IPC socket is owner-only mode `0600`. On Linux, the daemon
also checks that a client has the same user ID. This is a useful local boundary,
but every same-user process can still call the daemon. Aether must protect its
own approval tokens and must not treat IPC access as user approval. See the
[IPC server code](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/pkg/daemon/ipc.go).

## Reliable streams and A2A carriage

The Go driver supplies `Dial`, `Listen`, `Read`, and `Write` operations for
reliable streams. Aether can reserve one Pilot port and carry HTTP bytes over
one stream. Pilot supplies transport reliability. A2A must still supply Agent
Cards, messages, tasks, status, artifacts, streaming rules, and application
authentication. See the [Go driver](https://github.com/pilot-protocol/common/blob/main/driver/driver.go)
and [connection code](https://github.com/pilot-protocol/common/blob/main/driver/conn.go).

Pilot published an A2A example in February 2026. It uses
`/.well-known/agent.json`, one top-level Agent Card `url`, `pilot://` URLs, and
old `tasks/send`, `tasks/get`, and `tasks/cancel` JSON-RPC methods. This is an
old design example, not a current A2A integration. See the
[Pilot A2A article](https://pilotprotocol.network/blog/a2a-agent-cards-over-pilot-tunnels).

Current A2A requires the standard Agent Card path
`/.well-known/agent-card.json`. The card declares an ordered
`supportedInterfaces` list with a URL, binding, and protocol version. Current
operations and schemas also differ from the old Pilot example. See the
[A2A v1.0.1 release](https://github.com/a2aproject/A2A/releases/tag/v1.0.1),
[current A2A specification](https://github.com/a2aproject/A2A/blob/main/docs/specification.md)
and [current A2A schema](https://github.com/a2aproject/A2A/blob/main/specification/a2a.proto).

For the trial, Aether must build a small bridge that sends current A2A HTTP
traffic on a fixed Pilot stream port. It must not publish `pilot://` as if it
were a standard A2A interface unless Aether defines and tests a custom A2A
binding URI. A current A2A client and server must pass the same conformance
checks through the bridge as they pass over normal HTTP.
See the [A2A custom binding rules](https://a2a-protocol.org/latest/topics/custom-protocol-bindings/).

## Local daemon API and Rust integration

The daemon uses a Unix socket. The default is `$XDG_RUNTIME_DIR/pilot.sock` on
Linux when that variable exists. The fallback and macOS default are
`/tmp/pilot.sock`. An IPC frame is a four-byte big-endian length, a one-byte
command, and a payload. Server-push frames share the same connection. See the
[IPC source](https://github.com/pilot-protocol/common/blob/main/driver/ipc.go).

The IPC protocol has no request ID. The official Go client permits only one
request and reply at a time on one socket. It also drops a late reply after a
timeout so that the next call does not receive the wrong result. The source
states that full correction needs a daemon protocol change. This is a
stability risk for a new Rust client.

Pilot lists official Node.js, Python, and Swift SDKs. It does not list a Rust
SDK. See the [official SDK list](https://github.com/pilot-protocol/pilotprotocol#programmatic-access-sdks).

Use this integration order:

1. Run the unmodified Pilot daemon as a separate process.
2. Use `pilotctl --json` for setup, trust, network, and member operations.
3. Use a small Go sidecar with the official driver for long-lived stream I/O.
4. Define one narrow Aether RPC between Rust and the Go sidecar.
5. Implement native Rust IPC only if the trial proves that process overhead is
   a problem and Pilot publishes an IPC version contract.

This order keeps the first proof small and avoids a new Rust implementation of
an unversioned, correlated-request-limited IPC protocol.

## Offline inbox behavior

The data exchange service listens on Pilot port 1001. It stores received text,
JSON, and binary messages under `~/.pilot/inbox`. It stores received files
under `~/.pilot/received`. The default local limits are 10,000 inbox files,
256 MiB for the inbox, and 2 GiB for received files. See the
[data exchange repository](https://github.com/pilot-protocol/dataexchange) and
[service source](https://github.com/pilot-protocol/dataexchange/blob/main/service.go).

This inbox is durable only after the receiver daemon accepts the live stream.
The registry and beacon do not queue application messages for an offline node.
The relay has no persistent mailbox. Thus:

- If Codex is stopped but the local Pilot daemon is running, the daemon can
  receive and store a message. Aether can wake or poll the local inbox later.
- If the computer or Pilot daemon is offline, the sender cannot leave a
  message for later Pilot delivery.
- `send-message --wait` waits for a reply. It does not add store-and-forward
  behavior.

Pilot solves the local receiver loop. It does not solve network-wide offline
delivery. Aether still needs a durable relay mailbox, retry queue, or another
replicated message log if offline delivery is a product requirement.

## Updates, telemetry, prompts, and skill injection

The current documentation and source contain a default conflict. The CLI code
says automatic binary update is off when `~/.pilot/auto-update.json` is absent.
However, the fresh installer writes `{"enabled": true}`, installs the updater
service, and starts it. Therefore, the effective fresh-install default is
automatic update on. When the updater is on, application adapters also update
unless `PILOT_APP_UPDATE_OPT_OUT=true` is set. See the
[installer source](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/install.sh),
[update code](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/cmd/pilotctl/updates.go),
and [application update guide](https://pilotprotocol.network/docs/app-store#app-update-opt-out).

Telemetry, broadcast, and review consent values default to true when a key is
absent. App-store telemetry sends signed events with the node ID, time, action,
and app details to a Pilot HTTPS service. Explicit review submission uses the
telemetry endpoint. Automatic review prompts also require feature flags that
are currently off. The review consent value is on, but this alone does not turn
the automatic prompt flags on. See the
[consent code](https://github.com/pilot-protocol/common/blob/main/consent/consent.go),
[telemetry client](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/pkg/telemetry/client.go),
and [review code](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/cmd/pilotctl/review.go).

On a fresh install, skill injection is in `auto` mode. The daemon fetches
content from the public `TeoSlayer/pilot-skills` repository every 15 minutes
and writes into detected agent tool directories. It can add managed files and
marker blocks to files such as `CLAUDE.md` and `AGENTS.md`. This is a prompt
supply-chain boundary, even though the injector does not execute the text.
See the [consent guide](https://pilotprotocol.network/docs/consent) and
[skill command source](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/cmd/pilotctl/skills.go).

Before the Aether trial, create `~/.pilot/config.json` with these values:

```json
{
  "consent": {
    "telemetry": false,
    "broadcasts": false,
    "reviews": false
  },
  "skill_inject": {
    "mode": "disabled"
  }
}
```

Also create `~/.pilot/auto-update.json` with `{"enabled": false}` before the
installer runs, or do not use the installer. Run
`pilotctl skills disable all` to remove content that Pilot already wrote.
Use `pilotctl update disable` to stop binary updates. Pin the tested version.
Do not enable application updates. These settings do not stop core peer
transport.

## Security and threat limits

Pilot has useful cryptography, but the secure controls are not all on by
default in `v1.13.9`:

- daemon registry TLS defaults to false;
- daemon strict data-plane trust defaults to false;
- rendezvous strict directory authorization defaults to false;
- rendezvous strict registration authentication defaults to false;
- beacon punch-token enforcement defaults to false;
- a `--no-encrypt` test option exists;
- trust auto-approval can remove the human trust decision.

The official security guide calls the strict controls opt-in compatibility
controls. Aether must enable registry TLS, strict data-plane trust, strict
directory authorization, strict registration authentication, and punch tokens
together. It must reject a startup that uses `--no-encrypt` or trust
auto-approval. See the
[daemon flags](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/cmd/daemon/main.go),
[rendezvous flags](https://github.com/pilot-protocol/rendezvous/blob/main/cmd/rendezvous/main.go),
[beacon flags](https://github.com/pilot-protocol/beacon/blob/main/cmd/beacon/main.go),
and [security guide](https://pilotprotocol.network/docs/security).

Further limits are:

- The registry can observe identity, membership, directory, and route
  metadata. It is also a central availability and policy dependency.
- The beacon relay can observe endpoints, timing, sizes, and node identifiers.
- A shared network or bilateral trust controls connectivity, not the meaning
  or safety of an A2A request.
- Broadcast authorization is checked at the sender. A receiver gets an
  ordinary datagram and must validate its application payload.
- Skill injection can change agent instructions from a remote source.
- A custom transport has less independent review and deployment history than
  QUIC and TLS.
- The Internet-Draft itself notes registry trust, metadata exposure, replay,
  nonce, and IPC boundaries as security concerns.

## AGPL-3.0 engineering impact

The Pilot daemon, registry, beacon, common module, and data exchange module use
AGPL-3.0-or-later. See the
[core license](https://github.com/pilot-protocol/pilotprotocol/blob/v1.13.9/LICENSE),
[registry license](https://github.com/pilot-protocol/rendezvous/blob/main/LICENSE),
and [beacon license](https://github.com/pilot-protocol/beacon/blob/main/LICENSE).

These are engineering facts, not legal advice:

- Aether must record the exact Pilot version, source location, notices, and
  build inputs for every Pilot binary that it distributes.
- If the team modifies an AGPL Pilot component and users interact with that
  modified program over a network, the AGPL text has a corresponding-source
  requirement for those users.
- Running an unmodified published daemon as a separate process reduces source
  coupling. It does not remove the need for a license review.
- The team must not vendor or modify Pilot source before it defines the source
  publication and compliance process.
- Legal counsel must decide how the license applies to the planned package and
  service. This report does not make that legal decision.

For the proof, use an unmodified pinned binary as a separate process. Publish
the version and source link in the proof notes.

## Maturity and maintenance

Pilot is active and young. The core repository began in February 2026. Release
`v1.13.9` was published on 2026-07-29. Its release notes include fixes for a
key-exchange rate limiter, stream and registry timer leaks, resume behavior,
and webhook authentication. The code has a large test set. These are good
maintenance signals, but they also show that network and lifecycle defects are
still being corrected. See the
[release notes](https://github.com/pilot-protocol/pilotprotocol/releases/tag/v1.13.9)
and [core repository](https://github.com/pilot-protocol/pilotprotocol).

Most visible core work comes from two maintainers. The implementation is split
across many separately released Go modules. Private enterprise networks are
described as early access. The self-hosted server repositories do not supply a
production guide. The wire protocol, IPC, and current A2A bridge are not stable
standards. These facts make maintenance and supplier risk higher than for Iroh.

Do not use the Pilot website node-count claim as proof of maturity. This review
did not find a first-party, reproducible API snapshot that can verify that
number.

## Small Aether trial

Use two Aether nodes, one Linux server, and two different Internet connections.
Pin all Pilot components to the reviewed versions. Keep a packet capture and
structured logs for each test.

| Test | Pass condition | Fail condition |
| --- | --- | --- |
| Clean install | No Pilot file changes an agent instruction file. Updates, telemetry, reviews, broadcasts, and skill injection stay off. | Any opt-out changes, any remote skill fetch occurs, or any agent file changes. |
| Self-host control plane | A private registry and beacon start with TLS and all strict controls. Backup and restore keep addresses, trust, and membership. | The node must use the public service, a strict control cannot work, or restore loses required state. |
| Private join | An owner invites node B to an enterprise invite network. Node B accepts. Both local nodes list the same complete roster. | Invitation needs an undocumented manual service, member data is wrong, or a non-member can list the roster. |
| Trust and policy | An approved member can connect only to the reserved Aether port. A revoked or unknown node cannot connect. | A denied node reaches the Aether service, or revocation is not effective within the declared limit. |
| Direct NAT path | Two home-network nodes establish a direct encrypted stream and exchange 100 messages and a 100 MiB file. | Data corrupts, the stream stalls, or reconnect loses the tested operation. |
| Relay path | Forced symmetric-NAT or blocked direct UDP causes encrypted relay fallback with no application change. | The operation fails, leaks plaintext, or requires a public Aether endpoint. |
| TCP 443 path | A UDP-blocked node uses WSS compatibility mode and completes the same A2A request. | The mode cannot use the private control plane or changes A2A behavior. |
| Current A2A | A current A2A client gets `/.well-known/agent-card.json`, selects a declared interface, sends a task, streams status, and receives an artifact through the Pilot bridge. | The bridge needs the old Pilot article schema or fails current A2A conformance checks. |
| Local inbox | With Codex stopped and Pilot running, a message is stored locally and Aether handles it after restart. | The message is lost or Pilot starts Codex without an Aether approval rule. |
| Offline node | With the receiver daemon stopped, the test shows a clear send failure and no later delivery. | The product reports delivery or implies that Pilot queued the message. |
| Restart | Daemon, node, registry, and beacon restarts keep identity and required registry state. The beacon rebuilds ephemeral endpoint state. | Identity changes, membership disappears, or route recovery needs manual repair. |
| IPC load | The Go sidecar runs concurrent member, trust, and stream work for one hour without reply mix-up or deadlock. | A late reply reaches the wrong call, a socket blocks, or a stream is assigned to the wrong peer. |
| License record | The proof package contains the exact binary version, source URL, license, notices, and modification status. | A binary has no matching source record or contains an unreviewed modification. |

## Trial outcome rule

Pilot passes only if all tests pass without a change to the Pilot wire protocol.
If only the Rust SDK test is weak, keep the Go sidecar. If self-hosting,
security defaults, member listing, or current A2A fails, stop the Pilot path and
use Iroh with an Aether-owned join and directory service.
