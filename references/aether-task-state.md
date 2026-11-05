# Aether task state

Status: proof specification

## States and valid changes

| State | Valid next states |
| --- | --- |
| `pending_approval` | `queued`, `rejected`, `cancelled` |
| `queued` | `leased`, `cancelled`, `rejected` |
| `leased` | `running`, `unknown_result`, `queued`, `rejected`, `cancelled` |
| `running` | `succeeded`, `failed`, `unknown_result`, `cancelled` |
| `unknown_result` | `operator_reviewed` |
| `operator_reviewed` | `succeeded`, `failed`, `cancelled` |
| `succeeded` | none |
| `failed` | none |
| `rejected` | none |
| `cancelled` | none |

Only `queued` tasks can receive a lease. Only one unexpired lease exists for a
task. A scheduler uses fair per-peer selection and configured global and
per-peer queue limits.

## Transactions and idempotency

Task admission inserts the task and its first ordered event in one SQLite
transaction. The database uses foreign keys and unique task ID and idempotency
key constraints. Each state change inserts its event in the same transaction.
Event sequence numbers increase by one for each task and never change.

The same authenticated caller and idempotency key return the existing task.
The same key with changed request data is rejected. Queue overload and rate
limits reject admission before a task row is created.

## Leases, restart, and cancellation

A lease records its owner, issue time, expiry time, and execution correlation
ID. Only the execution holding the current, unexpired lease for a task may
write its next state: a lease renewal, and a subsequent success, failure, or
unknown-result write, are all rejected once the lease's recorded expiry has
passed, even before the periodic recovery job removes the row. This closes
the gap where a delayed renewal could otherwise keep resurrecting a lease the
recovery job had not reclaimed yet.

The node also rechecks policy immediately before a leased task's process is
told to start, using the same live grants a fresh admission would use. A
grant revoked after a task was leased cannot authorize a stale start; the
task moves to `rejected` instead, the same terminal state a policy denial at
admission time produces.

A worker records `running` only once this recheck passes, and only right
before it starts Codex. On restart, the node reclaims an expired lease only
when it proves no Codex turn started: a merely `leased` task returns to
`queued`, while a `running` task moves to `unknown_result` because its
process may have actually run.

If an execution lease expires and the Codex result is unknown, or if the
process was told to start and then failed before its outcome could be
confirmed (a timeout, a lost connection, a lease renewal failure), the node
writes `unknown_result`, emits an event, and requires operator review. It
must not start a second turn. Operator review can set a final observed
result or mark the task failed or cancelled. A renewal failure also signals
the process to stop immediately rather than waiting for it to finish or time
out on its own.

Cancellation is idempotent. It removes a queued task from scheduling, asks a
running adapter to stop, and writes one ordered cancellation event. A final
adapter result after cancellation is recorded for audit but cannot restart the
task. Peer cancellation must match the authenticated caller, task ID, and scope.

## Acceptance, polling, and retention

After admission the node returns the current durable snapshot. Progress reads
reuse the idempotency key and task identity with a new exchange ID, nonce, and
event cursor. Terminal task input and output expire 24 hours after terminal
completion. Active task input remains until the task reaches a terminal state.
Expired content keeps state, fingerprints, hashes, and ordered events. A
`content_expired` event records removal of retained text.

## Sender outbox

The node that sends a task keeps it in `sent_tasks` until it ends:

| State | Meaning |
| --- | --- |
| `waiting` | The host does not have the task yet, or refused it for a time (full queue, not reachable). |
| `working` | The host has the task and works on it. |
| `answered` | The host answered. The answer is kept for the inbox. |
| `failed` | The host refused or failed the task, with the reason. |
| `not_delivered` | The deadline passed, or the access to the host ended. |

- Every attempt sends the same task ID, request ID, and idempotency key, with
  a new exchange ID and nonce. The host keeps one task for the caller endpoint
  and the idempotency key.
- A process claims a due task with a lease of 90 s in an `IMMEDIATE`
  transaction. One endpoint has at most one claimed task at a time.
- A full host queue is tried again after a back-off from 5 s to 5 minutes.
  A host that cannot be reached is tried again from 30 s to 30 minutes.
- While tasks wait, the node reads the presence of its contacts from its hub
  every 60 s. A host that comes online gets its tasks at once. A host that the
  hub shows offline gets one attempt, then one every 30 minutes.
- The deadline is the earlier of 7 days and the end of the access.
- An ended task stays for 7 days (`RETENTION`), then the node deletes it.

## Migration and recovery

The store makes a timestamped database backup before each schema migration.
Restore the backup file to the database path while the node is stopped, then
start the prior binary. A migration runs in a transaction. If it fails, the
node keeps the previous database usable and reports the failure without
starting task scheduling.
