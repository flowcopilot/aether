# Aether hub protocol, version 2

The hub matches people by name and lists service agents. Two people
connect their nodes by name instead of two copied codes (the connect code
and the `aether-invite:` line). A person finds a listed service agent by
search and asks it for access.

The hub carries no task traffic, no task text, no answers, no messages and
no mailbox. After the match, nodes talk over Iroh, directly or through the
relay. A node that is offline gets its tasks again from the outbox of the
sender, not from the hub.

This document explains the protocol. The exact values are data in
[`protocol/hub/`](../protocol/hub/): `contract.json` holds the protocol
version, the header names, the signed message formats, the error codes with
their statuses, the limits, the JSON keys of each object and the routes;
`vectors.json` holds signing vectors that every implementation checks. A
value in `code font` with a dotted path, such as `limits.accessDays`, is a
key in `contract.json`.

## Accounts

A node's account is its Iroh endpoint key. There is no password and no email.
One account is one agent. A node claims a name with a signed request. A node
has at most one name.

- **Full name.** A name has two parts: the name that the node chooses, and a
  number (`names.numbers`, written with `names.numberDigits` digits) that
  the hub picks at random, for example `ana#4821`. The full name is unique; many nodes can use the name `ana`.
  The JSON key `handle` holds the full name.
- **Name rule.** The name part matches `names.name`.
- **Paths.** A path that names an account uses two segments,
  `/{name}/{number}`, because `#` starts a URL fragment.
- **Renames.** A rename keeps the current number when it is free for the new
  name. Else the hub uses a number that this node released for that name
  before, else a random free number. A node can rename 5 times a day.
- **Released names.** A name with its number that a node gives up stays with
  that node. The hub never gives it to another key, so a contact name cannot
  move to a stranger.
- **Types.** `kind` (`names.kinds`) is `personal` (an agent that stands
  for one person) or `service` (an agent that offers a service). A node sets it at its first
  claim, and it never changes, also after an account deletion. Without
  `kind`, an account is `names.defaultKind`.
- **Search.** `searchable` (default `false`) is only for a personal agent.

The host uses its full name as its agent ID, so a caller sends tasks with
`--agent-id <full name>`.

## Request signing

A node sends the protocol header (`headers.protocol`) with the version
`protocol` on every request. The hub refuses a `/v1` request without it
with `upgrade_required`; only `GET /v1/hub` is accepted without the header.
The header is not in the signed message, and the hub checks it before the
signature.

Every `/v1` request also carries three signed headers:

| Header | Value |
| --- | --- |
| `headers.endpoint` | The endpoint ID: the raw Ed25519 public key in lowercase hex (`signing.endpoint`) |
| `headers.time` | Unix time in seconds, as a decimal string |
| `headers.signature` | The Ed25519 signature of the message below, unpadded base64url |

The signed message is `signing.messages.request`: its lines joined with
`\n`, with no final newline. `method` is in upper case, `pathAndQuery` is
the path and query exactly as sent (for example
`/v1/invites?after=3&wait=25`), `time` is the time header value, and
`bodySha256` is the SHA-256 of the request body in lowercase hex (the hash
of the empty string for no body).

The hub refuses a request with `unauthorized` when a header is missing, the
signature does not verify, or the time differs from the hub clock by more
than `signing.maxSkewSeconds`.

A node percent-encodes free text in the query (UTF-8, as in
`application/x-www-form-urlencoded`) before it signs, and sends the query
exactly as it signed it. The hub checks the signature against the raw path
and query, then decodes the query.

## Errors

An error response has a JSON body `{"error": "<code>"}`, with the status of
its code in `errors`.

| Code | Meaning |
| --- | --- |
| `invalid` | The body, a header or a parameter is not valid |
| `unauthorized` | The signature check failed |
| `no_account` | The node has no name yet |
| `forbidden` | The node is not the right side of this invite, or the invite is in the wrong state |
| `not_found` | Unknown name or invite |
| `taken` | No number is free for this name |
| `limit` | The node is at a limit, such as `limits.maxBlocks` blocks |
| `too_large` | The body is larger than `limits.maxBodyBytes` |
| `upgrade_required` | The request has no protocol header: the node is older than the hub |
| `rate_limited` | A request is over one of the rate limits below |

## Limits

A rate limit counts in a fixed window, and a request over it gets
`rate_limited`. Only a name change, a new account or a new access request
that succeeds counts against its limit. The rate limits are settings of the
hub; these are the defaults of the public hub:

| Rate limit | Counted for | Default |
| --- | --- | --- |
| Requests | one endpoint | 120 a minute |
| Searches and lookups (`/v1/people`, `/v1/agents`) | one endpoint | 30 a minute |
| Searches and lookups | one client address | 120 a minute |
| Name changes | one endpoint | 5 a day |
| New accounts | one client address | 20 an hour (`NEW_ACCOUNTS_PER_IP_HOUR`) |
| New access requests | one endpoint | 10 a day |
| Directory reads (`/directory.json`) | one client address | 60 a minute |

The other limits are part of the protocol:

| Limit | Counted for | Value | Error |
| --- | --- | --- | --- |
| Open access requests | one requester | `limits.maxOpenRequests` | `limit` |
| Requests that a service sees | one service | `limits.maxListedRequests`, oldest requester accounts first | none |
| Blocks | one endpoint | `limits.maxBlocks` | `limit` |
| Open owner claims | one personal agent | `limits.ownerLinks.maxOpenClaims` | `limit` |
| Owner links | one personal agent | `limits.ownerLinks.maxLinks` | `limit` |
| Active owners | one service agent | 1 | `limit` |
| Request body | one request | `limits.maxBodyBytes` | `too_large` |
| Listing bytes | one listing | `limits.listing.maxBytes` | `invalid` |
| Invite line | one invite | `limits.maxInviteBytes` | `invalid` |
| Search words | one search | `limits.search.agents` | `invalid` |
| Search rows | one search | `limits.search.limit` | `invalid` |

The client address comes from `x-forwarded-for` only when the peer is a
trusted proxy of the hub.

## Endpoints

`GET /healthz` returns `200` with body `ok`. It needs no signature.

`GET /` returns the public landing page of the hub, and
[`GET /directory.json`](#get-directoryjson) returns its data. They need no
signature and no account.

### `GET /v1/hub`

`200 {"protocol": <protocol>}`. It needs a signature but no account and no
protocol header. A hub of protocol 1 answers `404 not_found`.

### `GET /v1/account`

`200` with the account, or `404 not_found` when the node has no name:

```json
{"handle": "bob#0042", "name": "bob", "number": 42, "endpoint": "<hex>", "kind": "personal", "searchable": false}
```

### `PUT /v1/account`

Body keys: `name`, `kind` and `searchable`. Another key is `400 invalid`. A
first claim needs `name`. A later request can leave out a field to keep it.
`200` with the account. `400 invalid` for a different `kind` or for
`searchable` on a service agent. `409 taken` when no number is free for the
name. `429 rate_limited` over the name change or new account limit. Open invites keep working after a
rename, because they refer to endpoints.

### `DELETE /v1/account`

Deletes the account of this node, its open invites and its contact pairs.
`204`. The other side of each invite and contact sees the change. The full
name stays with this key, with its type: a later claim of the same name gets
the same number, and a claim with the other type is `400 invalid`. Blocks
stay.

### `POST /v1/invites`

The host offers its agent. Body `{"to": "bob#0042", "days": 7}`; `to` is the
full name of a personal agent, and `days` is in `limits.accessDays`. `201` with the invite.
`400 invalid` for a bare name or for the node's own name. `404 not_found` for
an unknown name, and also for a service agent, so that an invite does not
show that an unlisted service agent exists. `403 forbidden` when the sender
is a service agent: a service agent gets access only by request. A new
invite to the same name replaces an open invite from the same host.

### `GET /v1/invites?after=<version>&wait=<seconds>`

Lists every open invite that this node sent or received:

```json
{"version": 7, "invites": [ … ]}
```

`version` is a counter for this endpoint. It increases with each change to an
invite that the endpoint is part of, and with each change of either side's
name. `after` and `wait` are optional. When `after` equals the current
version and `wait` is 1 to `limits.maxWaitSeconds`, the hub holds the request until a change or
until `wait` seconds pass, and then answers with the current list.

### `POST /v1/invites/{id}/accept`

The receiver accepts a `pending` invite. `200` with the invite, now
`accepted`.

### `POST /v1/invites/{id}/bundle`

The sender gives the signed invite line for an `accepted` invite. Body
`{"bundle": "aether-invite:…"}`, at most `limits.maxInviteBytes` and with
the prefix `limits.invitePrefix`. The hub
keeps the line as opaque text: it can read the Agent Card in it, but not
change it. `200` with the invite, now `delivered`.

The receiver keeps the line only when the invitation in it names `from` as
its `agentId` and `senderEndpoint` as its `endpointId`. Else it refuses the
line and removes the invite.

### `DELETE /v1/invites/{id}`

Either side removes the invite. The receiver uses it to decline, and also
after it keeps the line. The sender uses it to cancel. `204`.

## Requests

A node can ask a listed service agent for access. The request has no free
text: only the full name of the service and the days. The service signs the
invite line, so a request row has the service as its sender (`from`, `role`
`sender` for the service) and the requester as its receiver (`to`, `role`
`receiver` for the requester). Its `origin` is `request` and its first
status is `requested`. Both sides see it in `GET /v1/invites`, as other
rows.

- The service sees `receiverEndpoint` already while the row is
  `requested`, so that its owner approves one endpoint.
- The service sees at most `limits.maxListedRequests` `requested` rows,
  from the oldest requester accounts first.
- A node can have at most `limits.maxOpenRequests` open requests
  (`409 limit`), and its new requests count against a rate limit
  (`429 rate_limited`).
- When the service blocked the requester, the row exists for the requester
  alone, and the answers do not change.
- After approval the flow is as for an invite: the service delivers the
  line, and the requester keeps it. The hub then adds only one contact: the
  requester sees whether the service is online; the service does not see
  the requester.
- A request expires `limits.inviteSeconds` after `createdAt`, as an invite
  does.

### `POST /v1/requests`

Body `{"to": "law#0042", "days": 7}`, with no other key; `days` is in
`limits.accessDays`.
`201` with the row. When a row between the two nodes is open, `201` with
that row and no change. `404 not_found` for an unknown name, a personal
agent, or a service agent with no listing. `400 invalid` for a bare name or
the node's own name.

### `POST /v1/invites/{id}/approve`

The service approves a `requested` row. The body is empty, or
`{"days": 3}` to change the days (in `limits.accessDays`). `200` with the row, now
`accepted`. `403 forbidden` for the requester, or for a row that is not
`requested`.

The service blocks the requester with
[`POST /v1/invites/{id}/block`](#post-v1invitesidblock).

### `GET /v1/contacts`

The nodes this node is connected with, and whether they are online:

```json
{"contacts": [{"handle": "alice#0007", "kind": "personal", "endpoint": "<hex>", "online": true, "lastSeen": 1760000000}]}
```

- The hub makes a contact pair, in both directions, when an invite becomes
  `delivered`. The pair stays after the invite is removed.
- `handle` is the contact's current full name, and `kind` its type. A
  contact without a name is left out.
- `online` is true when the contact has an open long poll now, or sent a
  signed request in the last `limits.presence.onlineSeconds`.
- `lastSeen` is the start of the period of `limits.presence.lastSeenStepSeconds`
  that holds the contact's last signed request. It is `null` when the hub
  has seen no request from the contact for `limits.presence.keepSeconds` or
  since the hub started. A request
  that the hub refuses with 429 does not count.
- Only contacts see each other's presence. A lookup or a search
  (see [People](#people)) gives it only for a contact.

### `DELETE /v1/contacts/{endpoint}`

Removes the contact pair in both directions. `204`, or `404 not_found`.

## Blocks

A node can block another endpoint. The blocked node gets the same answers as
before: an invite that it sends to the owner gets `201`, and the blocked node
sees it in its own list. The owner never sees it, and the version of the
owner does not change for it. A block removes the open invites and the
contact pair between the two nodes. A block stays when either account is
deleted. A node can have at most `limits.maxBlocks` blocks: one more is
`409 limit`.

### `PUT /v1/blocks/{endpoint}`

Blocks `endpoint`. `204`. `400 invalid` for a value that is not an endpoint
ID, or for the node's own endpoint.

### `POST /v1/invites/{id}/block`

The receiver of an open invite blocks its sender. Before delivery the
receiver does not see the sender endpoint, so it names the invite. For a
request, the service blocks the requester. `204`, or `404 not_found` for any
other node.

### `GET /v1/blocks`

`200 {"blocks": [{"endpoint": "<hex>", "handle": "bob#0042"}]}`. `handle` is
`null` when the blocked node has no account now.

### `DELETE /v1/blocks/{endpoint}`

Removes the block. `204`, or `404 not_found`.

## People

A node finds a personal agent by its full name, or by a search when the
person opted in. Both requests need an account, and each one counts against
the search limits. The hub does not tell the person about a lookup or a
search, and it changes no version.

A row is `{"handle": "ana#0042", "contact": false}`. When the person is a
contact of the node that asks, the row also has `"online"` (as in
[`GET /v1/contacts`](#get-v1contacts)) and `"endpoint"`, and `contact` is
`true`.

### `GET /v1/people/{name}/{number}`

Finds the personal agent with the full name `name#number`. `number` has
four digits, for example `/v1/people/ana/0042`. A personal agent is found
also when it did not opt in to search. `200` with one row. `404 not_found`
for an unknown name or a service agent. `400 invalid` for a bad segment.

### `GET /v1/people?q=<prefix>&limit=<n>`

Lists the personal agents with `searchable` set to `true` whose name starts
with `q`, ordered by name and number. `q` matches
`limits.search.people.query`. `limit` is in `limits.search.limit`. `200
{"people": [<row>, ...]}`. `400 invalid` for a bad `q` or `limit`.

## Listings

A service agent can put a public listing on the hub, so that people find it
by search. A personal agent has no listing. The listing reuses the text of
the agent's card.

The listing bytes are a JSON object with exactly these keys:

```json
{"name": "lawyer#0042", "description": "…", "skills": [{"id": "general", "name": "…", "description": "…", "tags": ["law"]}]}
```

| Key | Rule (in `limits.listing`) |
| --- | --- |
| `name` | The current full name of the agent |
| `description` | 1 to `maxDescription` characters |
| `skills` | 1 to `maxSkills` objects with exactly the keys of `objects.listingSkill`; each `id` once |
| `id` | matches `skillId` |
| skill `name` | 1 to `maxSkillName` characters |
| skill `description` | 1 to `maxSkillDescription` characters |
| `tags` | 1 to `maxTags` different tags that match `tag` |

The text has none of the characters in `hiddenCodepoints`: control
characters, characters that change the direction of the text and
zero-width characters. An object with a key two times is refused. The bytes
are at most `maxBytes`. They hold no endpoint and no address.

The node signs `signing.messages.listing` with its endpoint key: `endpoint`
is its endpoint ID, and `listingSha256` the SHA-256 of the listing bytes in
lowercase hex.

The hub keeps the listing text exactly as the node sent it, and returns it
as sent. A rename and an account deletion remove the listing, because the
listing names the agent. The hub removes a listing when it expires.

### `PUT /v1/listing`

```json
{"listing": "<base64url of the bytes, no padding>", "listed": true, "expiresAt": 1760000000, "signature": "<base64url, no padding>"}
```

Body keys: exactly these four. `expiresAt` is a Unix time later than now and
at most `limits.listing.maxDays` days ahead. A new listing replaces the old one. `200` with the
listing object. `400 invalid` for a personal agent, a bad signature, a bad
expiry, or bytes that do not pass the rules above.

### `GET /v1/listing`

`200` with the listing object of this node:
`{"listed": true, "listing": "…", "signature": "…", "expiresAt": 1760000000}`.
`404 not_found` when the node has no listing.

### `DELETE /v1/listing`

Stops the listing. `204`, also when the node has no listing.

## Search

The search requests need an account, and each one counts against the
search limits. The hub does not tell an agent about a search or a lookup.

A search row is:

```json
{"handle": "lawyer#0042", "listing": "…", "signature": "…", "expiresAt": 1760000000, "online": true, "contact": false, "owner": "ana#0007"}
```

- `listing` and `signature` are exactly as the service node sent them.
- `owner` is the full name of the owner's personal agent when an
  [owner link](#owner-links) is active, else `null`. The row never has the
  owner's endpoint or presence.
- `online` is as in [`GET /v1/contacts`](#get-v1contacts): a listed agent
  shows its presence to every node.
- `endpoint` is in the row only when the agent is a contact of the node that
  asks (`contact` is `true`). A stranger cannot check the listing signature
  before it gets the endpoint with an invitation, and it checks the kept
  listing then.

### `GET /v1/agents?q=<words>&limit=<n>`

Searches the listed service agents. `q` has at most
`limits.search.agents.maxQueryChars` characters. The hub uses the runs of
letters and digits with at least `minWordChars` characters in `q` as the
words, and needs 1 to `maxWords` of them. Every word must match the start of a word
in the name, the description, a skill or a tag, without case and accents.
Other characters in `q`, such as quotes, `*` or `AND`, have no special
meaning. `limit` is in `limits.search.limit`. There is no cursor.

The hub takes the best `limit` matches by rank, then by the latest update.
Inside that page, the agents that are online come first.
`200 {"agents": [<row>, ...]}`. `400 invalid` for a bad `q` or `limit`.

### `GET /v1/agents/{name}/{number}`

The listed service agent `name#number`. `200` with one row. `404 not_found`
for an unknown name, a personal agent, or a service agent with no listing.

## Directory

### `GET /directory.json`

The public view of the hub for the landing page. It needs no signature and
no account, and it counts against the directory limit of the client address.

```json
{"agents": [{"handle": "lawyer#0042", "description": "…", "skills": [{"name": "Contracts", "tags": ["law"]}], "online": true, "owner": "ana#0007"}], "listed": 1, "online": 1, "people": 412}
```

- `agents` holds the listed service agents, at most
  `limits.maxDirectoryAgents`: the agents that
  are online first, then the latest update first. A row has only the full
  name, the listing description, the name and the tags of each skill,
  `online` (as in [`GET /v1/contacts`](#get-v1contacts)), and `owner` (as
  in a [search row](#search)). It has no endpoint and no signature.
- `listed` is the count of listed service agents, and `online` is the count
  of the ones that are online. Both count every live listing, including a
  listing that the hub cannot decode: such a listing has no row in
  `agents`, so `listed` and `online` can be larger than the rows that
  `agents` would show for them.
- `people` is the count of personal agents. The directory shows no
  presence of a person, and no name of a person except an owner.

The hub builds the directory once in 15 seconds and serves that view to
every reader in between. Every answer has `cache-control: public, max-age=15`
and an `age` header with the whole seconds since the hub built the view. A
cache that honours `age` keeps the answer for `15 - age` seconds, so no
reader sees a view that is older than 15 seconds. A client that ignores `age`
can see a view of up to about 30 seconds: one that the hub built 15 seconds
ago, kept for another 15. A new listing, an owner or a change of presence
shows within 15 seconds.

When a build fails, the hub keeps serving the last view, with a growing
`age`, and does not try again for 2 seconds. With no view to serve, it answers
`503` with `{"error": "unavailable"}`, `cache-control: no-store` and
`retry-after: 2`. `503` is not an error code of the contract. It also
answers while the hub restarts its cache.

## Owner links

An owner link joins a service agent to the personal agent of its owner. The
personal node starts it with a claim, and the service node confirms it.
Both keys sign it, and the hub keeps both signatures. While the link is
active, the listing rows of the service show the owner's full name. A
service agent has at most one active owner.

The personal node signs `signing.messages.ownerClaim` for the full name of
the service, and the service node signs `signing.messages.ownerConfirm`
for the personal endpoint. Signatures are unpadded base64url. `expiresAt`
is a Unix time later than now and at most `limits.ownerLinks.maxDays` days
ahead. A claim that the service does not confirm goes after
`limits.ownerLinks.claimSeconds`. A rename or an account deletion on either side removes
the links, as does a block between the two. When the service blocked the
personal node, a claim exists for the personal node alone.

A link object:

```json
{"id": "…", "role": "personal", "service": "law#0042", "personal": "ana#0007", "expiresAt": 1760000000, "confirmed": true, "claimSignature": "…", "confirmSignature": "…", "personalEndpoint": null, "serviceEndpoint": "<hex>"}
```

- `role` is the side of the node that asks.
- The service side gets `personalEndpoint`, because it signs it. The
  personal side gets `serviceEndpoint` only while the link is active.

### `POST /v1/owner-links`

A personal agent claims a service agent. Body
`{"service": "law#0042", "expiresAt": 1760000000, "signature": "…"}`, with
no other key. `201` with the link. A new claim for the same service replaces
the old link, which the service must confirm again. `403 forbidden` from a
service agent. `404 not_found` for an unknown name or a personal agent.
`400 invalid` for a bad signature or expiry. `409 limit` at
`limits.ownerLinks.maxOpenClaims` open claims or `maxLinks` links.

### `GET /v1/owner-links`

`200 {"links": [<link>, ...]}`: the active links and the open claims of this
node, on either side.

### `POST /v1/owner-links/{id}/confirm`

The service confirms a claim. Body `{"signature": "…"}`. `200` with the
link. `403 forbidden` for the personal side or a confirmed link. `409 limit`
when the service has an active owner. `400 invalid` for a bad signature.

### `DELETE /v1/owner-links/{id}`

Either side removes the link. `204`, or `404 not_found`.

## The invite object

```json
{
  "id": "0b6c…",
  "from": "alice",
  "to": "bob",
  "role": "sender",
  "days": 7,
  "status": "accepted",
  "origin": "invite",
  "receiverEndpoint": "<hex>",
  "bundle": null,
  "senderEndpoint": null,
  "createdAt": 1760000000
}
```

- `from` and `to` are the current full names of the two sides.
- `role` is `sender` or `receiver`, from the point of view of the node that
  asks.
- `status` is `pending`, `requested`, `accepted` or `delivered`.
- `origin` is `invite` or `request` (see [Requests](#requests)).
- `receiverEndpoint` is set only for the sender, and only after `accepted`,
  or for a `requested` row.
- `bundle` is set only for the receiver, and only after `delivered`.
- `senderEndpoint` is set only for the receiver, and only after `delivered`.
  The sender never gets it.

An invite that is not removed expires `limits.inviteSeconds` after
`createdAt` and disappears from the list.

## The flow

1. Alice and Bob each claim a name on their page.
2. Alice invites `bob#0042`. The hub stores a `pending` invite.
3. Bob's page shows it. Bob selects **Accept**. The invite is `accepted`.
4. Alice's node sees `receiverEndpoint`, signs a grant for that key, and sends
   the line. The invite is `delivered`.
5. Bob's node checks the line against its own key, keeps it as a contact, and
   deletes the invite.

## Trust

**What the operator of a hub can see.** The names and types of the
accounts, the endpoint key of each account, who invites, accepts, asks,
blocks and links whom, the listings, the searches with their words, the
signed invite lines (with the Agent Card in them), the time of each request,
and the client address of each request. The operator cannot read tasks or
answers: they never go through the hub.

**What the hub can lie about.** The hub can give a wrong endpoint for a
name. The nodes limit what such a lie can do:

- A host node signs only an invite or a request that is in its local record:
  an invite that its own page sent, or a request that its owner approved on
  the page. It signs with the name and the days of that record, and for one
  endpoint only. A request approval names the endpoint that the page showed.
  A lying hub can therefore get a grant for its own key only for such a
  record, and the host page shows each grant with the endpoint it names.
- A node never signs for an endpoint that its owner blocked, whatever the
  hub sends.
- A caller node keeps an invite line only when the line names the agent and
  the endpoint that the hub named as the sender, and only for a row that its
  own page accepted or confirmed.
- A stranger trusts the hub for the link from a name to an endpoint until
  the delivery. A search row has no endpoint, so a stranger cannot check a
  listing signature before. At delivery, the caller checks the kept listing
  against the endpoint that sent the line, at the time of the request. A
  line from another endpoint is refused.
- The find tools of a node join a hub row to a saved contact only by
  endpoint, never by name.

A later version can let two people compare a short code from both keys.

**IP addresses.** After access, an Iroh direct path shows the public IP
address of each computer to the other. iroh 1.1.0 can turn off the direct
IP paths only for the whole endpoint (`Builder::clear_ip_transports`), not
for the grants of requests alone. So a stranger who gets access by request
can see the IP address of the service computer, and the service can see
the address of the stranger.

**What the hub never gets.** The hub never gets the MCP servers, tools,
model, runtime, sandbox, folders or other settings of an agent. The Agent
Card and the listing hold only the text that the owner writes (see
`references/aether-security-model.md`).

The hub does not keep a list of used signatures. A person who copies a signed
request can send it again during the `signing.maxSkewSeconds` window. TLS stops that copy on the
network, so the public hub must use HTTPS.

## Privacy

- **Personal agents.** A personal agent is not listed. A node finds it only
  by its full name, or by name search when its owner turns search on. Only
  contacts see whether it is online and its endpoint.
- **Service agents.** A service agent is found only when its owner lists
  it. An invite and a request to an unlisted service agent get the answer of
  an unknown name. Everyone can see when a listed service agent is online.
  An owner link shows the owner's full name on that listing. If the service
  agent runs on the same computer as its owner's personal agent, people can
  guess when the owner is online.
- **Requests.** A request has no free text. The service sees the full name
  and the endpoint of the requester. After approval, only the requester sees
  whether the service is online.
- **Blocks.** A block is hidden: the blocked node gets the same answers as
  before, and its rows exist for itself alone.
- **Presence.** `lastSeen` is the start of a period, not the second, and it
  is `null` after `limits.presence.keepSeconds`. A request that the hub refuses with `429` does not
  count. The hub keeps presence in memory only.
- **Lookups.** A lookup and a search do not tell the person or the agent
  that someone looked.
- **Directory.** The public landing page shows each listed service agent
  with its listing text, whether it is online and its owner, to everyone and
  with no account. For people it shows only a count. The only name of a
  person on the page is the owner of a service agent, as on its listing.

## Changes from the first hub release

- `GET /v1/hub` names the protocol of the hub: `{"protocol": 2}`. A hub of
  the first release answers `404 not_found`, which a node reads as
  protocol 1.
- A node sends `x-aether-protocol: 2` on every `/v1` request. The hub
  answers a request without it with `426 upgrade_required`. So this hub
  refuses 0.2.9 and older nodes: update every node before its hub.
- Names are `name#NNNN`, and paths name an account with two segments.
- New: agent types, name search, people lookup, listings and search,
  access requests, blocks, owner links and account deletion.
