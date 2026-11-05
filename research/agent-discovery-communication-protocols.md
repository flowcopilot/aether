# Agent discovery and communication protocol review

Checked against primary sources on 2026-08-31.

## Decision

Use A2A v1.0 for the public agent description and message contract. Use a
small Aether directory for capability search. Build the proof in Rust and use
Iroh for the first network path. Carry the A2A HTTP and JSON exchange through
an Iroh stream between the two Aether nodes.

Do not design a new task protocol, payment protocol, DID method, or
peer-to-peer network in the first proof of concept.

The first proof must show this sequence:

1. An agent publishes an A2A Agent Card to an Aether directory.
2. A second agent searches the directory by skill or tag.
3. The second agent gets the selected Agent Card and endpoint.
4. The nodes make an encrypted Iroh connection.
5. The agents exchange an A2A HTTP and JSON message through that connection.
6. The receiver returns a direct message or a completed task result.

This split is necessary because A2A defines Agent Cards and agent
communication, but it does not define one standard registry API. Its discovery
guide lists a well-known URL, private or public registries, and direct
configuration as separate choices. It also says that registry discovery is
environment-specific. [A2A discovery guide]

[A2A discovery guide]: https://a2a-protocol.org/latest/topics/agent-discovery/

Iroh does not fill this gap. Its address lookup resolves network addresses and
relay URLs for an Endpoint ID that the caller already knows. It does not search
for agents by capability. [Iroh FAQ](https://docs.iroh.computer/about/faq)

## Layer map

| Project | Main layer | What it owns | Fit for this proof |
| --- | --- | --- | --- |
| A2A | Agent application protocol | Agent Cards, skills, messages, tasks, artifacts, and response modes | Adopt |
| Aether directory | Discovery service | Registration, capability search, expiry, and duplicate control | Build the minimum |
| A2A HTTP and JSON | Local application binding | Standard A2A request and response handling on each node | Adopt first |
| Iroh | Peer-to-peer transport | Endpoint identity, secure connections, NAT traversal, and relay fallback | Adopt first |
| libp2p | Alternative peer-to-peer transport | Peer addressing, streams, NAT traversal, and relay fallback | Keep as a comparison |
| AT Protocol or Nostr | Public data network | Signed public records and relay or index distribution | Study, do not adopt now |
| MCP | Agent-to-tool protocol | Tool, resource, and prompt sessions | Optional inside an agent |
| x402 | Payment protocol | HTTP payment request, authorization, verification, and settlement | Add only after communication works |

The main design error to avoid is to treat all of these projects as competing
agent protocols. They own different layers.

## Decision table

| Project | Discovery | Identity and access | Transport and lifecycle | Language and maturity | Decision |
| --- | --- | --- | --- | --- | --- |
| A2A | An Agent Card describes identity, endpoints, skills, content modes, and security schemes. A server publishes it at `/.well-known/agent-card.json`. A2A permits registries but does not specify one registry API. | HTTPS is required for production. Authentication uses normal web methods declared in the card. Authorization is server policy. Cards can have JWS signatures. An authenticated extended card can expose more skills to an approved caller. | HTTP bindings support JSON-RPC, JSON over HTTP, and gRPC. Messages can return a message or create a stateful task. Tasks support polling, streaming, cancellation, and push. | A2A v1.0 is the stable specification. The project now has an official Rust SDK with core types, clients, servers, HTTP bindings, gRPC, and streaming. | Use its card and message model. Keep the first task flow small. [Concepts](https://a2a-protocol.org/latest/topics/key-concepts/) [v1.0 release](https://a2a-protocol.org/dev/blog/2026/03/12/a2a-protocol-ships-v10-production-ready-standard-for-agent-to-agent-communication/) [Rust SDK](https://github.com/a2aproject/a2a-rs) |
| ANP | Agent Description documents use JSON or JSON-LD. Active discovery uses a domain well-known document. Passive discovery registers descriptions with search agents. | `did:wba` or compatible `did:web` documents provide keys and service endpoints. Current guidance uses HTTP Message Signatures, then can issue a bearer token. | Descriptions can link to OpenRPC, MCP, and other interfaces. ANP 1.1 also has direct and group message profiles and optional end-to-end encryption. | The specification set calls description, discovery, identity, and messaging v1.1 released. Its meta-protocol is still a draft. The project has a published Go SDK v0.9.3. The project README states MIT terms. | Copy the active and passive discovery ideas. Do not make ANP the first wire contract. Its adoption and governance are much smaller than A2A. [Protocol status](https://github.com/agent-network-protocol/AgentNetworkProtocol) [Agent description](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/07-anp-agent-description-protocol-specification.md) [SDK](https://github.com/agent-network-protocol/anp) |
| AT Protocol | Handles resolve to DIDs through DNS or a well-known HTTPS path. DID documents locate a Personal Data Server. Search is built by an AppView or another index over public repository records. | A DID is the durable account ID. Repositories contain signed commits. OAuth authorizes clients, and signed JWTs support service-to-service calls. | PDS services use HTTPS. Relays aggregate signed public repository events into a WebSocket firehose. Lexicon defines record, RPC, and event schemas. It does not define agent messages. | It runs a large production social network. The main code is TypeScript. Indigo supplies broad Go libraries and relay services, but its official feature table has no OAuth server support. Code is MIT or Apache-2.0. | Reuse its separation of durable identity, user-controlled records, relays, and search indexes as a design reference. It is too much public-data machinery for this proof. [Overview](https://atproto.com/guides/overview) [DIDs](https://atproto.com/specs/did) [Repositories](https://atproto.com/specs/repository) [Sync](https://atproto.com/specs/sync) [Go implementation](https://github.com/bluesky-social/indigo) |
| Iroh | Address lookup maps a known Ed25519 Endpoint ID to direct addresses and relay URLs. It can combine DNS, a Mainline DHT, mDNS, and custom lookup methods. These methods do not search by capability. | Endpoint IDs are Ed25519 public keys. QUIC with TLS 1.3 authenticates both endpoints. Application hooks decide which endpoints can connect. | QUIC streams use ALPN to select the application protocol. Iroh attempts a direct path and uses an encrypted relay when direct traffic fails. | Core Iroh reached v1.0 in June 2026. v1.0.3 is current. It is Rust and is MIT or Apache-2.0. | Use it in the first Rust proof. It replaces the public HTTPS address or separate tunnel between Aether nodes. [FAQ](https://docs.iroh.computer/about/faq) [v1.0.3](https://github.com/n0-computer/iroh/releases/tag/v1.0.3) |
| libp2p | It supplies mDNS, rendezvous, Kademlia DHT, GossipSub, and Identify. Identify lists network addresses and supported protocol IDs, not semantic agent skills. | A Peer ID comes from a cryptographic key. Noise or TLS authenticates the peer connection. libp2p does not supply application authorization. | It supports TCP, QUIC, WebSocket, WebTransport, multiplexed streams, circuit relay, and hole punching. Applications define their own stream protocol ID and messages. | go-libp2p is active, widely used, and MIT licensed. It supports the two newest Go releases. | It is the best Go-native candidate for a later peer-to-peer transport test. Aether would still need capability announcements and the A2A mapping. [Specifications](https://github.com/libp2p/specs) [security](https://docs.libp2p.io/concepts/security/security-considerations/) [Identify](https://github.com/libp2p/specs/blob/master/identify/README.md) [Go implementation](https://github.com/libp2p/go-libp2p) |
| Google SAM | Nodes announce MCP services through interest-scoped GossipSub. A local MCP server can discover peers, inspect remote tools, and call them. | A control plane verifies OIDC identity and issues Biscuit capability tokens. Routers and nodes authenticate mesh connections. | Go services combine a registry control plane, libp2p bootstrap and relay routers, local nodes, and an MCP sidecar. | The current release line is `v0.1.0-alpha`. The repository is Apache-2.0 and requires Go 1.25 or later. It says that it is not an officially supported Google product. | Use it as an architecture reference. Do not depend on an alpha mesh with a control plane, routers, tokens, and MCP when the proof needs two agents. [Repository](https://github.com/google/sam) [quick start](https://github.com/google/sam/blob/main/site/content/docs/quickstart.md) [releases](https://github.com/google/sam/releases) |
| MCP | A connected client lists one server's tools, resources, and prompts. The separate official Registry lists MCP server packages and remote endpoints. | Streamable HTTP uses OAuth 2.1 resource and authorization server discovery. Standard input gets credentials from the local environment. The host controls connection permissions. | JSON-RPC sessions run over standard input or Streamable HTTP. MCP models tool calls and context access, not peer agent identity or agent task delegation. | The 2025-11-25 specification has an official Go SDK. The SDK supports that specification from v1.4.0, with some OAuth support marked experimental. New code uses Apache-2.0 while older code can remain MIT. | Use MCP only when an Aether agent must expose tools to its own host. Do not use it as the agent-to-agent contract. [Architecture](https://modelcontextprotocol.io/specification/2025-06-18/architecture) [transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) [authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization) [Go SDK](https://github.com/modelcontextprotocol/go-sdk) [Registry](https://github.com/modelcontextprotocol/registry) |
| IBM ACP | A server lists agents and returns an Agent Manifest with descriptions, content types, and optional capabilities. | It expected TLS and common HTTP authentication at the deployment boundary. Identity federation was not complete. | Its REST API models messages, runs, sessions, synchronous output, asynchronous work, streaming, and pause or resume. | ACP 0.2.0 was Apache-2.0. The official SDKs were Python and TypeScript. IBM stopped development and archived the repository in August 2025 after ACP joined A2A. | Do not adopt it. Its useful ideas now belong in A2A work. [Archived repository](https://github.com/i-am-bee/acp) [OpenAPI](https://github.com/i-am-bee/acp/blob/main/docs/spec/openapi.yaml) [merge announcement](https://github.com/orgs/i-am-bee/discussions/5) |
| Nostr and NIP-90 | Clients publish signed events to WebSocket relays and query them with filters. NIP-90 uses request, feedback, and result event kinds for an open job market. | Every event has a secp256k1 Schnorr signature. NIP-17 direct messages use NIP-44 encryption and NIP-59 wrappers. Relay operators still see connection and traffic metadata. | Relays store and forward events. There is no direct peer session. NIP-90 status is `draft`, `unrecommended`, and `optional`. It leaves much job behavior open. | NIPs are public domain. The common Go library `go-nostr` is MIT, but its repository was archived in January 2026. | Do not base the proof on NIP-90. Its signed event and relay pattern is useful for public announcements, but Aether needs a stable capability schema and direct agent session. [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md) [NIP-17](https://github.com/nostr-protocol/nips/blob/master/17.md) [NIP-90](https://github.com/nostr-protocol/nips/blob/master/90.md) [Go library](https://github.com/nbd-wtf/go-nostr) |
| x402 | Bazaar can index paid HTTP or MCP resources and search their descriptions and schemas. A successful payment through a participating facilitator triggers indexing. | A buyer signs a payment authorization. A resource server or facilitator verifies it and settles it. This proves payment authority, not general agent identity or permission to private data. | HTTP returns `402` with payment requirements. The client retries with a payment signature. x402 does not define an agent conversation or task lifecycle. | x402 v2 has an Apache-2.0 reference repository and an official Go v2 module. Coinbase reports production mainnet use of its facilitator. | Keep it out of the first proof. Add it later as an optional payment step around an A2A request. Never use payment as the handshake for private information. [v2 specification](https://github.com/x402-foundation/x402/blob/main/specs/x402-specification-v2.md) [Go SDK](https://github.com/x402-foundation/x402) [Bazaar behavior](https://docs.cdp.coinbase.com/x402/support/faq) |

## What Aether must add

A2A does not provide these product rules. The proof must add only the first four:

1. A registry API with register, renew, remove, get, and search operations.
2. An index over A2A skill ID, name, description, tags, and input or output modes.
3. Lease expiry so that an unavailable agent does not remain discoverable.
4. A local trust rule. For the proof, accept only configured Iroh Endpoint IDs
   and issue a short-lived grant for an approved skill.
5. Later, organization identity, card signature verification, reputation,
   permission grants, private capability disclosure, abuse controls, and
   payment.

The directory must store the original Agent Card. It can add search fields
beside the card, but it must not create an incompatible Aether agent
description.

## Suggested Rust boundary

Use stable Rust through mise. Use the official A2A Rust SDK and Iroh in the
same process. Use `aether/a2a/0` as the Iroh ALPN value for the proof.

Keep these interfaces small:

```rust
#[async_trait]
trait Directory {
    async fn register(&self, record: AgentRecord, ttl: Duration) -> Result<()>;
    async fn search(&self, query: SearchQuery) -> Result<Vec<AgentRecord>>;
}

#[async_trait]
trait Channel {
    async fn send(
        &self,
        endpoint: EndpointId,
        message: A2aMessage,
    ) -> Result<A2aResponse>;
}
```

The first `Channel` opens an Iroh stream and carries an A2A HTTP exchange. The
remote node sends that exchange to its loopback A2A server. A later directory
can announce cards through GossipSub or store signed public records in another
network.

## Proof limits

The first proof does not need an LLM, durable storage, global federation, DIDs,
private skill cards, payments, negotiation, calendars, or meetings. Those
features would hide the two questions that the proof must answer:

- Can one agent find another agent from a capability description?
- Can it use the returned Endpoint ID to complete one A2A exchange over Iroh?
