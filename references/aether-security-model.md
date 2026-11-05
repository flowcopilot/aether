# Aether security model

Status: proof specification

## Keys and signed records

Each node owns separate persistent member, agent, and Iroh endpoint private
keys. The node keeps private keys in an operating-system secret store or
owner-only files. It never sends a private key to a peer, directory, relay,
log, UI response, MCP response, or audit record.

An Aether signed grant must contain these fields: grant ID, issuer agent ID,
subject agent ID, subject endpoint ID, scope ID, allowed skill IDs, allowed
workspace IDs, access mode, approval rule, issue time, expiry time, nonce, and
signature. A grant is valid only for its exact scope and authenticated endpoint.

Profile, endpoint-binding, membership, and grant claims must include an issue
time, expiry time, scope ID, signing key ID, nonce, and signature. The node
uses a configured clock-skew limit. Expired, future-dated beyond that limit,
unsigned, malformed, unknown-key, revoked, or cross-scope claims fail closed.

## Request admission

The receiving node must use this fixed order:

1. Verify the remote Iroh Endpoint ID.
2. Verify all signed claims.
3. Apply scope middleware.
4. Apply rate and approval rules.
5. Apply the final local skill, workspace, and access-mode grant.
6. Admit the task to the bounded queue.

Any denial ends evaluation. A middleware timeout, panic, or adapter error
denies new work. A discovery, trust, or managed-network adapter can deny a
request. It cannot create or widen a local grant. The local policy engine is
the final authority.

## Execution boundary

Before it starts Codex, the node must resolve an approved workspace ID to a
canonical local path and reject absolute remote paths, traversal, and
symbolic-link escape. It must enforce the configured Codex sandbox mode,
approval policy, time limit, context limit, network rule, and environment
allowlist. If it cannot enforce one requested limit, it must reject execution.

The node must keep skill instructions separate from untrusted remote task text.
Direct invites grant the skill `general`. Its instruction asks the agent to do
the task that the caller sends, and not to name the tools, MCP servers, model,
files, folders or settings of the host computer. A direct grant with the older
skill `review` runs as `general`; the signed grant does not change. A PACT
Brand keeps its own capability as the skill.
It must not put directory events, queued tasks, secrets, credentials, or text
from another task in a model context. A request carries exactly one text
part. The node refuses data, raw, URL, and file parts. A reply carries one
text answer.

## Agent tools

The agent's tools are the MCP servers in `<data-dir>/codex-home/config.toml`.
The owner turns them on or off in that file. Aether has no per-grant tool
limit. Every admitted caller can use every enabled tool. The owner's own
`~/.codex` setup stays out of the node. Tasks run with the approval policy
`"never"`, so an MCP write tool runs only with `approval_mode = "approve"`.
The node declines MCP elicitation requests. MCP servers run outside the task
sandbox, with the rights of the node account.

## Agent Card

The host signs its Agent Card with the invitation key and puts it in the
invite line (`aether-invitation/v2`). The caller checks the signature and the
card fields before it keeps anything. The card holds the endpoint ID, so it is
never public. A hub that carries the line can read the card, but it cannot
change it without the caller seeing a bad signature.

## What a node never publishes

A node never publishes the names of its MCP servers or tools, its model, its
Codex settings, its sandbox mode, or its folder paths. Cards, listings, hub
records, and tool descriptions name only what the owner writes and the
protocol needs.

## Local administration

The local UI binds only to `127.0.0.1`. It checks Host and Origin, uses CSRF
protection, HttpOnly same-site session cookies, session expiry, and a restrictive
Content Security Policy. HTTPS and Secure cookies are required before production.
The MCP server uses standard input and output and reaches the
node through an owner-only local socket.

High-risk MCP changes create a pending change. Only a UI session can confirm
it. An MCP client cannot confirm its own change. High-risk changes include a
new read-write workspace, peer grant, managed invitation, public capability,
or higher limit.

## Audit and retention

Logs, UI and MCP responses, and audit records must redact private keys,
credentials, invitation tokens, authorization values, and full secret values.
Audit records contain the actor type, action, changed field names, safe result,
and timestamp. The proof default retains task content for 24 hours and redacted
audit records for 30 days. Deletion and retention jobs must preserve the event
ordering evidence required for active tasks.

Task content retention starts when a task reaches a terminal state. Terminal
input and output are cleared 24 hours (`CONTENT_RETENTION_SECONDS`) after the
task reached that state — not 24 hours after the cleanup job happens to run.
Active tasks keep their input until they finish. `UnknownResult` has the same
definite 24-hour deadline, starting the moment the node records the outcome
as unknown, whether that happens through lease recovery at restart or through
a confirmed-start execution that failed after the process was told to run.
The deadline does not depend on whether an operator has reviewed the task:
review does not need the original input, only the durable state and events,
and this node does not retain potentially sensitive remote task text
indefinitely on the chance nobody looks at it. Once a task is backfilled with
its deadline, it stays fixed through any further `OperatorReviewed` handling.
The store records `input_expires_at` and `content_expired_at` so repeated
cleanup stays safe without rewriting event order or request fingerprints.
Cleanup itself is bounded to `CONTENT_CLEANUP_BATCH` rows per pass, so a
large backlog cannot hold the store's single writer for an unbounded time; a
periodic caller clears the rest over later passes.

A database upgraded from before a task's terminal state reliably set
`input_expires_at` (migrations up to and including `009_content_retention.sql`)
backfills the missing deadline from that task's own completion evidence — its
last recorded event, or its admission time if none exists — never from the
time the upgrade runs. An old row is therefore immediately eligible for
cleanup rather than getting a fresh 24-hour grace period it was never meant
to have.
