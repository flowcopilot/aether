# Aether implementation review issues

Review date: 6 September 2026

The happy path works. The safe multi-task daemon does not work correctly yet.
This review found 12 confirmed issues.

The `Impact` value describes the effect on the current implementation. The
`Repair risk` value describes the risk of the required code change.

## 1. Give lease selection to one scheduler

- **Category:** Correctness
- **Impact:** High
- **Effort:** Large
- **Repair risk:** High
- **Confidence:** High
- **Evidence:** `crates/aether/src/proof.rs:101` and
  `crates/aether-store/src/lib.rs:1570`

Request handlers also act as the scheduler. An old queued task can get a lease
from a new request. New tasks also leave the queue before Codex has capacity.
Queue limits and fair order do not control the work.

Let request handlers only admit tasks. Add one scheduler that owns lease
selection and execution dispatch. Mark a task as running only when Codex starts.

## 2. Stop cancelled waiting tasks before execution

- **Category:** Security and correctness
- **Impact:** High
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/proof.rs:370`,
  `crates/aether/src/execution.rs:346`, and
  `crates/aether-store/src/lib.rs:1017`

A task can be cancelled while it waits for the executor, but it can start
later. Cancellation is also not idempotent.

Check the durable task state before Codex starts. Register cancellation before
dispatch. A second cancellation of a cancelled task must return success.

## 3. Own task work through timeout and shutdown

- **Category:** Correctness
- **Impact:** High
- **Effort:** Large
- **Repair risk:** High
- **Confidence:** High
- **Evidence:** `crates/aether/src/node_service.rs:109`,
  `crates/aether/src/node_service.rs:176`, and
  `crates/aether/src/transport.rs:978`

The request waits for Codex to finish. Events are sent only after completion.
The 30-second transport timeout does not stop the blocking task. Shutdown also
does not own the execution threads.

Give the node one task supervisor, a cancellation token, and tracked execution
tasks. Stop admission, cancel work, wait for tasks, and then close storage and
Iroh.

## 4. Apply the connection limit before request reads

- **Category:** Security and resource control
- **Impact:** High
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/transport.rs:813`,
  `crates/aether/src/transport.rs:848`, and
  `crates/aether/src/transport.rs:923`

The production Iroh router does not apply the 32-connection limit before it
reads requests. Many slow peers can create tasks and hold memory before Tower
applies its limit.

Put one shared semaphore in `AetherProtocol`. Acquire a permit before stream
acceptance and keep it through the response write.

## 5. Apply administration changes to the live node

- **Category:** Correctness and product behavior
- **Impact:** High
- **Effort:** Large
- **Repair risk:** High
- **Confidence:** High
- **Evidence:** `crates/aether/src/admin.rs:226`,
  `crates/aether-store/src/lib.rs:676`, and
  `crates/aether/src/node_config.rs:157`

Confirmed workspace, skill, grant, directory, invitation, and capability
changes only write `admin.*` values. They do not change the running node. The
node only loads stored queue limits after restart.

Add one typed live policy service. Return `applied` only after the runtime
policy and the durable state both contain the change.

## 6. Require confirmation for queue-limit increases

- **Category:** Security
- **Impact:** High
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/admin.rs:355` and
  `references/aether-security-model.md:59`

Every queue-limit change is low risk. An MCP caller can increase a limit without
UI confirmation. This conflicts with the security model.

Compare the old and new limits. Require local confirmation when the new limit
is higher.

## 7. Validate each client response against its request

- **Category:** Security and protocol correctness
- **Impact:** High
- **Effort:** Small
- **Repair risk:** Low
- **Confidence:** High
- **Evidence:** `crates/aether/src/transport.rs:1131` and
  `crates/aether/src/transport.rs:1240`

The client does not compare a response with its request. It can accept frames
for another exchange, scope, or task. It can also accept a request or cancel
frame as a response.

Add a direction-specific response validator. Require matching exchange, scope,
and task values. Permit only event frames followed by one result or error.

## 8. Recover expired leases while the node runs

- **Category:** Correctness
- **Impact:** High
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/proof.rs:176` and
  `crates/aether-store/src/lib.rs:1048`

Expired leases are recovered only during startup. A lease that expires after
startup can stay blocked until the next restart.

Run periodic lease recovery from the scheduler. Do not start a second turn for
a task with an unknown result.

## 9. Return the correct state for an idempotent retry

- **Category:** Protocol correctness
- **Impact:** Medium
- **Effort:** Small
- **Repair risk:** Low
- **Confidence:** High
- **Evidence:** `crates/aether/src/node_service.rs:125` and
  `crates/aether-store/src/lib.rs:999`

An idempotent retry reports failed and unknown-result tasks as `Working`. An
expired successful result becomes `Completed` with an empty artifact.

Map each durable state to an explicit A2A state. Return an explicit error when
the retained result has expired.

## 10. Apply the 24-hour content-retention rule

- **Category:** Security and privacy
- **Impact:** High
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/proof.rs:176`,
  `crates/aether-store/src/lib.rs:999`, and `PLAN.md:570`

Cleanup runs only at startup. It clears only final results. It never clears
remote task text.

Add an input-expiry value. Run periodic cleanup for terminal tasks. Keep the
task state, hashes, and ordered event evidence.

## 11. Enforce cancel-frame scope and order rules

- **Category:** Protocol correctness
- **Impact:** Medium
- **Effort:** Medium
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/transport.rs:390`,
  `crates/aether/src/node_service.rs:76`, and
  `crates/aether-store/src/lib.rs:1025`

Frame validation permits `request` followed by `cancel`, but the handler ignores
the second frame. A standalone cancel also does not compare the frame scope with
the task scope.

Reject request-plus-cancel or process the cancel before admission. Bind a
standalone cancel to the task ID, owner endpoint, and stored scope in one
transaction.

## 12. Return a failing CLI status for protocol errors

- **Category:** CLI correctness
- **Impact:** Medium
- **Effort:** Small
- **Repair risk:** Medium
- **Confidence:** High
- **Evidence:** `crates/aether/src/main.rs:980`

The request CLI exits with status 0 when the final frame is an A2A error.
Scripts cannot distinguish success from denial, overload, replay, or timeout.

Keep the structured response output. Return a nonzero status for an A2A error
or a failed task.

## Test coverage gaps

The 162 tests pass, but they do not test the real scheduler with two different
tasks and one blocked Codex process. Add current-contract tests for:

- two different tasks with one blocked executor;
- cancellation before Codex starts;
- a lease that expires after startup;
- more than 32 slow peers through the production router;
- a task that runs for more than 30 seconds;
- one real live policy change through MCP and the UI;
- failed, unknown, and expired-result retries;
- response correlation and cancel-scope validation.

The completed items in `TODO.md` do not match these findings. The scheduler,
live events, connection bound, shutdown, and live administration items must not
stay marked as complete.

## Recommended direction

First, add one node-owned scheduler and task supervisor. Request handlers must
only admit tasks. The scheduler must own leases, execution capacity,
cancellation, recovery, cleanup, events, and shutdown.

Then make submission asynchronous. Return acceptance quickly. Send status from
the durable event journal through streaming or polling. This will make the
pub/sub design real.

After that, replace `admin.*` configuration rows with one typed live policy
service. UI and MCP results must say `applied` only after the runtime policy and
durable state both change.

## Review verification

- `mise run check` passed once. It reported 162 passed tests and one ignored
  live Codex test.
- A parallel command-test run had a readiness failure and left an Aether test
  process active.
- A later isolated Iroh test could not bind sockets in the restricted review
  environment. It did not test application behavior.
- Container, relay, physical-machine, live-Codex, dependency-advisory, and
  license checks were not run in this review.
- `brief.md` stayed unchanged and untracked.

The recommended next plan covers issues 1 through 10 first.
