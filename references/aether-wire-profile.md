# Aether A2A wire profile

Status: proof specification

## Scope

This profile carries A2A JSON data between Aether nodes over Iroh QUIC. Its
ALPN is `aether/a2a/0`. It is an Aether transport profile, not an official A2A
transport binding.

One bidirectional QUIC stream carries one exchange. The sender writes one or
more frames and then finishes its sending side. A peer must not use a stream
for another exchange.

## A2A subset

Aether uses `a2a-lf` for A2A 1.0 task data. The request envelope has an
`a2aVersion` value of `1.0`, an idempotency key, a request nonce, signed
authorization data, and an official A2A `Task`. The A2A task has one submitted
user `Message` in its history. That message has exactly one text `Part`. A
request with any other part, or with more than one part, is refused. A result
has one artifact with one text part. An event carries only a task state.

Event frames use the official A2A `StreamResponse.statusUpdate` form. Result
frames use the official `SendMessageResponse.task` form. Cancel frames use
the official `CancelTaskRequest` form. Error frames use the official JSON-RPC
error object with `code`, `message`, and optional `data` fields.

The proof supports the submitted, working, completed, and canceled A2A task
states. It rejects URL, raw, and data parts. It does not implement an A2A
HTTP, JSON-RPC, gRPC, push-notification, file-part, artifact update, or
agent-card binding.

The receiver rejects unknown fields in the supported subset, wrong A2A
versions, invalid UUID identifiers, empty required values, unsupported parts,
and a task ID that differs from the frame task ID.

## Frame format and limits

Each frame has an unsigned, big-endian 32-bit length followed by that number
of UTF-8 JSON bytes. The length includes only the JSON bytes.

The maximum frame size is 1,048,576 bytes. The maximum exchange size is
4,194,304 bytes. A receiver must reject a larger declared size before it
allocates the frame buffer. A sender must stop after 64 frames in one exchange.

Each JSON frame has these required fields:

```json
{
  "version": 0,
  "kind": "request",
  "exchangeId": "UUID",
  "scopeId": "scope identifier"
}
```

`version` must be integer `0`. `exchangeId`, `taskId`, request nonce, and
signed request ID must be UUID values. `exchangeId` must be unique for the
sender during its replay window. `scopeId` identifies the direct, directory,
or managed scope in which the request is valid.

## Frame kinds

| Kind | Direction | Required extra data |
| --- | --- | --- |
| `request` | caller to receiver | A2A task, idempotency key, nonce, and signed authorization |
| `event` | receiver to caller | A2A task status update |
| `cancel` | either direction | A2A cancel task request |
| `result` | receiver to caller | A2A task response and a final artifact on success |
| `error` | either direction | A2A JSON-RPC error object |

The first frame must be `request`. A receiver can send zero or more `event`
frames, followed by exactly one `result` or `error` frame. A `cancel` frame can
end the exchange, but its receiver must return one final `result` or `error`.

A request handler returns a durable task snapshot after admission. It does not
wait for Codex to finish. The caller polls with fresh exchange IDs and request
nonces while keeping the same task identity and idempotency key. Each poll
carries `lastEventSequence` and receives one page of ordered event frames plus
one final task snapshot frame within the exchange limits. A transport timeout
or client disconnect ends only the current exchange. The node keeps accepted
work until cancellation or the execution deadline stops it.

### Event paging

A page holds at most 63 event frames, reserving the 64th frame for the final
task snapshot. The receiver also bounds a page by encoded size, stopping
before the snapshot frame would risk the 4,194,304-byte exchange limit even
when every individual event frame is small. Either bound can leave events in
the durable journal unread.

The final task snapshot's `metadata` carries two Aether-specific fields:

```json
{
  "aetherEventCursor": 63,
  "aetherMoreEvents": true
}
```

`aetherEventCursor` is the highest event sequence this page actually
delivered (or the caller's own `lastEventSequence` if the page delivered
none). `aetherMoreEvents` is present and `true` only when the durable journal
holds events beyond this page; a page that reached the end of the journal
omits it. A caller keeps polling with `lastEventSequence` set to
`aetherEventCursor` as long as `aetherMoreEvents` is `true`, even when the
snapshot's task status already looks terminal: the task can finish before
every one of its events has been paged out, and a terminal-looking status on
a truncated page is not proof that no events remain. Stopping on status alone
silently drops the undelivered tail. A caller that accumulates every page's
event frames, plus the final page's snapshot frame, sees the complete ordered
journal for one polled task.

## Connection, time, and rate limits

The receiver accepts a maximum of 32 concurrent connections. It sheds a
request above that count and answers with an overload error frame. It accepts
a maximum of 8 requests each second from one authenticated endpoint.

### `retryAfterMs`

A request above the per-peer limit gets the error frame "node overloaded",
with the same code and message as any other overload. That error has one
optional field, `data.retryAfterMs`, a number of milliseconds.

- Only the per-peer rate limit sends `data.retryAfterMs`. An overload for any
  other reason, such as a full handler queue, never carries it: that error
  has no `data`.
- The value is a fixed hint, the time for one token of the limit (125 ms at 8
  requests each second). It is not the exact wait of the limiter, so a client
  that sends again should wait at least that long and back off.
- A host that sends the key has admitted nothing for that request: no task, no
  replay entry, no events. A client may therefore send the same exchange again
  (the same task ID and idempotency key; the reference client uses a new
  exchange ID and request nonce) only when the key is present. A client that
  gets "node overloaded" without it must not send the exchange again in the
  same call; the host rejects it at once by design.
- The reference client sends again for up to 2 seconds, starting at one token
  and doubling to at most 1 second, and no wait passes the end of that window.
  After that it returns the error. An outbox task that gets the error keeps
  waiting and tries again after 2 seconds.

A client or host that does not know the key sees no change, because the
message and code stay the same.

Every network stage has a time limit: 20 seconds for the connection handshake,
10 seconds for stream acceptance, for a frame read, and for a frame write, and
30 seconds for the request handler. A stage above its limit stops and returns a
timeout error frame. An idle peer therefore cannot hold a connection permit.

## Error handling

Malformed frames fail closed. A valid error frame uses an A2A JSON-RPC error
object. The transport maps denied, replayed, overloaded, timed-out, and failed
requests to these error frames.

A task that needs operator review (`UnknownResult` or `OperatorReviewed`) and
a task whose successful output has already expired each get their own error
code, in the -32000 to -32099 range JSON-RPC reserves for implementation-
defined server errors: `-32010` for operator review, carrying the durable
state name (`unknown_result` or `operator_reviewed`) as `data.state`, and
`-32011` for an expired result, with no data. Neither ever includes the
task's input or output; a caller distinguishes these two outcomes by code,
not by matching the error message text.

## Replay and cancellation

The receiver stores accepted idempotency keys with the task ID. The same key
with the same authenticated caller returns the existing task. The same key
with different request data fails closed. The receiver retains exchange IDs
and request nonces for at least the request TTL.

A cancellation request is valid only from the authenticated caller, the local
operator, or an approved local MCP client. Cancellation is idempotent. It does
not erase existing events or audit records. A cancelled task must not start a
new execution turn.
