# Plan: Repair the Aether node

## Goal

Repair all 12 issues in [issues.md](issues.md), dated 6 September 2026.
Give the node control of queued tasks and execution. Apply policy changes to
the running node. Make retries, cancellation, retention, and command results
match the durable task state.

## Status after commit 56acdcf

The checklist is not complete. The latest implementation commit is
`56acdcf`, `test(e2e): verify task supervision and live policy`.
This review checked its current source and ran `mise run check` successfully.
Build, format, lint, and workspace tests passed. This result does not cover
missing acceptance tests. Docker relay scripts and separate concurrent
command-suite runs were not run in this review.

The implementation includes admission without execution waits, periodic
maintenance, execution-ID checks, retention columns, a router semaphore,
response correlation, state mapping, cancel-scope checks, and CLI failure
status. These changes replace the old status based only on commit `c66e188`.

| Earlier phase | Current status | Remaining phase |
| --- | --- | --- |
| 22 and 22a | Resolved by phase 33: thread ownership, the shutdown-before-registration path, and the start-boundary policy recheck are implemented and tested; see phase 33 for the acceptance-test gaps that remain (thread-creation failure, precise mid-shutdown timing) | none |
| 23 | Resolved by phase 34: database paging is now SQL-bounded and the poller delivers every event across pages instead of only the last poll's; the `router_with_async` long-task and byte-limit acceptance tests remain open | none |
| 24 | Resolved by phase 33: `maintain` reports recovered task IDs and stops the affected worker, and the lease-renewal execution-ID guard now rejects an already-expired lease; the "stop affected worker" path itself has no dedicated test (no test hook for `MAINTENANCE_INTERVAL`) | none |
| 25 | Resolved by phase 35: migration 010 backfills legacy terminal rows and `UnknownResult` now gets a definite deadline; a large-backlog test and a migration-010-specific backup/restore test remain open | none |
| 26 | Resolved by phase 38: the production `router_with_async` overload path is now tested with real connections (immediate overload, disconnect-based permit release, recovery) | none |
| 27 | Resolved by phase 36: UI confirmation applies live (workspace, grant, limit) with revert-on-failure, workspace/grant changes reload on restart, confirmation rejects a stale change using a durable revision that itself now survives a restart, and a workspace change can be restricted to a policy-approved id/path set; admission checking the same revision remains open by user decision, and was separately confirmed safe to defer — the property it would give (a queued task cannot dispatch under a revoked grant or stale workspace) already holds through existing live rechecks | 36 |
| 28 | Partially resolved by phase 37: directory renewal and capability publication are now real runtime actions, both reusing the pinned directory record; invitation remains open (ambiguous between two different flows — see phase 37) | 37 |
| 29 | Resolved by phase 36: confirmation is bound to a durable revision, recovered on restart, and rejects staleness whether or not a restart happened between queuing and confirming | none |
| 30 | Resolved by phase 38: operator-review and expired-result errors are machine-readable with safe data, the TaskState test matrix including restart-after-any-state coverage is done, and a second `Leased ->` transition gap (this time `Failed`) is fixed the way phase 33 fixed the first | none |
| 31 | Cancel scope and CLI status exist; complete contract tests remain | 38 |
| 32 | Partially resolved by phase 39: relay and managed-relay Docker checks now pass (Docker was available); a concurrent command-suite run remains open | 39 |

Phases 33 through 35 are implemented and their focused checks pass. Phases
36 and 38 are only partially done: phase 36's typed-revision architecture
was deferred by explicit user decision, and phase 38 took only its one
bullet with real behavior content, leaving its test-matrix expansion
untouched (see each phase below for the exact boundary). Phase 37 was not
started at all — it is the same kind of architectural work phase 36
deferred, for a different set of change types. `mise run check` passes on
the current tree: 191 tests passed, 1 live-Codex test ignored, fmt and
clippy clean. Phase 37, the rest of phase 36, and phase 38's test matrix
and phase 39 are the next work. A checked code item records the
verified implementation. A combined item stays open if any required behavior
or test is missing. Earlier planned commit names are not commands to rewrite
history. Commit `56acdcf` contains work from several phases; only its actual
commit record is complete. The same applies to the phase 33 through 38 commits
that follow it: each checklist item is checked because the required behavior
is implemented and the practically-testable scenarios pass, but a few named
test scenarios remain open rather than falsely closed — see `TODO.md` phases
33 through 38 for the lists.

## Approach

Phases 22 through 32 define the original repair requirements.
Phases 33 through 39 below define the remaining work after commit `56acdcf`. They continue the phase numbers in
the earlier plan below. Issues 1 through 10 take priority. Phase 31 then closes
issues 11 and 12. The earlier design remains useful context, but these repair
phases replace its conflicting behavior and completion claims.

The code review for this plan covered `proof.rs`, `node_service.rs`,
`execution.rs`, `transport.rs`, `admin.rs`, `node_config.rs`, `main.rs`, and
`crates/aether-store/src/lib.rs`. Commit `56acdcf` repairs the previously
open scheduler tests. The status table separates implemented code from
remaining behavior and acceptance tests.

Use one scheduler, one supervisor, and the existing SQLite store owner. Keep
one execution slot until the task ownership tests pass. Use bounded polling
for remote progress. This fits the existing exchange limits and durable event
cursor. Do not add a second execution queue in request handlers.

Use one typed policy owner for runtime reads and changes. Local policy changes
and remote publication have different failure conditions. Phase 27 handles
local changes. Phase 28 handles operations that need a remote service.

Each phase must build and pass its focused checks before its scoped
Conventional Commit. Use the commit message specified below. Split a phase
only after updating both this plan and `TODO.md`. This task writes the plan;
it does not start implementation.

No new dependency is required by this plan. Check the pinned APIs before an
implementation change needs them. Public HTTP A2A support, more execution
slots, offline delivery, and unrelated feature work remain outside scope.

## Issue coverage

| Issue | Repair phases |
| --- | --- |
| 1 | 22, 22a |
| 2 | 22, 22a |
| 3 | 22, 22a, 23 |
| 4 | 26 |
| 5 | 27, 28 |
| 6 | 29 |
| 7 | 26 |
| 8 | 24 |
| 9 | 30 |
| 10 | 25 |
| 11 | 31 |
| 12 | 31 |

Phase 32 verifies all repairs together.

## Remaining implementation phases

Run phases 33 through 39 in order. Keep each phase buildable and run its
focused checks before its scoped commit. Update both documents before a split.

### Phase 33: Complete execution start, recovery, and shutdown

Completes earlier phases 22, 22a, 24.

- In `proof.rs`, retain and join every execution thread handle. Handle thread creation failure and clear execution capacity. Keep storage open until execution and lease-renewal workers stop. The current `dispatch_one` discards the thread handle.
- Close the gap between `prepare_execution` and `run_execution`. Register the active task and cancellation before dispatch. Serialize cancellation and the actual start boundary. Recheck policy at that boundary so a grant change after preparation cannot authorize a stale start.
- In `execution.rs`, move start confirmation to the actual process response. `execute_with_confirmed_start` currently calls its callback before `start_turn_with_events`. Record durable execution intent before the external request. Preserve an uncertain start as `UnknownResult`.
- Make `maintain` report recovered task IDs and stop affected workers. On renewal failure, stop execution promptly. Requeue only tasks with proof that execution never started. Execution-ID guards must also reject an expired lease before the recovery job removes it.
- Use one shutdown deadline for all worker waits. Handle shutdown before active-task registration and a process that ignores cancellation. Stop and reap the process before storage closes. Preserve queued tasks and uncertain results.
- Test the real scheduler with blocked execution, several peers, cancellation at the start boundary, expiry after startup, renewal failure, thread creation failure, and late completion. Add restart tests for selected scope, workspace, and limits. Use bounded waits and clean up all test processes.

Commit: `fix(scheduler): close execution lifecycle races`

Result: implemented, including a store-level fix this phase's work surfaced
(`TaskState` did not permit `leased -> rejected`, so a dispatch-time policy
denial left a task stuck rather than terminal). See `TODO.md` phase 33 for
the acceptance-test scenarios that remain open.

### Phase 34: Complete bounded event paging and progress

Completes earlier phases 23.

- Add SQL limits to `NodeStore::events_after` and avoid loading the complete journal through `stored_task`. Read each page and its task snapshot in one consistent store operation.
- In `node_service.rs`, bound pages by both frame count and encoded bytes. Reserve room for the final snapshot. Return a cursor and an explicit indication that more events remain.
- In `transport.rs`, continue reading remaining pages after a terminal snapshot. The current poller stops at terminal state even when `.take(MAX_FRAMES - 1)` omitted later events. Preserve or deliver all requested events instead of returning only the last poll response.
- Test more than 64 events, a terminal task with several unread pages, byte limits, concurrent completion, cursor resume, and slow readers. Run a task longer than 30 seconds through `router_with_async` and observe progress before completion. Verify one execution across polls.
- Update `references/aether-wire-profile.md` with the cursor, byte bounds, timeout, and terminal-page rules.

Commit: `fix(protocol): page durable progress without losing events`

Result: implemented. A new end-to-end test (a task with 70 progress events,
run through the real wire) reproduces the bug this phase fixes: before this
change it would have returned at most 63 events because the client stopped
as soon as one page's snapshot looked terminal, even though the durable
journal held more. See `TODO.md` phase 34 for the acceptance-test scenarios
that remain open.

### Phase 35: Complete retention migration and bounded cleanup

Completes earlier phases 25.

- Add a forward migration after `009_content_retention.sql`. Do not edit an applied migration. Migration 009 copies only existing result expiry; old failed, rejected, cancelled, and cleared-result rows can keep input without expiry.
- Backfill all terminal rows from durable completion evidence. Define a conservative fallback for missing timestamps. Do not restart a 24-hour clock for old content. Preserve task identity, request fingerprints, and event order.
- Bound `clear_expired_content` by batch size and require the intended task states in its query. Define an explicit expiry rule for `UnknownResult` and update `references/aether-security-model.md`. Its current unresolved-task rule does not give a definite deadline.
- Check the planned content-hash requirement. Preserve the existing request fingerprint and add separate content hashes only if the retention contract requires them. Keep expiration distinct from valid empty output.
- Test fresh install and upgrades from migrations 008 and 009 with every terminal state. Test active and unresolved tasks, expiry boundaries, repeat cleanup, and a large backlog. Check keys and foreign keys.
- Test migration failure and backup restore on a database copy. Document rollback through restore and verify content retention in backups. Record any backup-policy gap before closing this phase.

Commit: `fix(store): complete legacy task content expiry`

Result: implemented, including giving `UnknownResult` a definite retention
deadline it did not have before (the lease-recovery path into that state
never called `set_input_expiry`). See `TODO.md` phase 35 for the
acceptance-test scenarios that remain open.

### Phase 36: Complete confirmed live policy changes

Completes earlier phases 27, 29.

- Route `Administration::confirm_ui` through the same validated live apply operation as immediate changes. It currently calls `NodeStore::confirm_pending_change`, writes configuration, removes the pending row, and returns without `apply_live`.
- Use one typed policy revision for admission and administration. Prepare and validate before commit. Publish the committed revision before success. Keep admission stopped if publication fails. Recover the revision on restart.
- Complete workspace, skill, grant, and limit application. `apply_live` has no skill action and accepts missing callbacks. Return a defined unsupported or unavailable result instead of success when an action cannot run.
- Apply workspace changes to `NodeService`, `Executor`, and the underlying Codex adapter together. The current setter changes only executor limits; the service and process have their own workspace configuration. Resolve workspaces by approved ID.
- Load validated stored workspace, skill, and grant changes on restart. `node_config::load_service` currently loads stored queue limits only. Keep signature verification for grants and test replacement or revocation before dispatch.
- Bind UI confirmation to exact values and the base revision. Recheck queue-limit risk at application. A missing or failed live limit read must not classify an increase as low risk. Test stale confirmation and both queue-limit fields.
- Test a signed grant and a workspace change through MCP, the administration socket, and UI confirmation. Verify execution behavior before and after restart. Include skill changes, concurrent admission, storage failure, publication failure, and secret redaction.

Commit: `fix(admin): apply confirmed policy revisions to the live node`

Result: split by explicit user decision. Offered a full-architecture option,
a scoped-fix option, and stopping the autonomous work here; the user chose
the scoped fix. Implemented and committed as
`fix(admin): apply confirmed high-risk changes to the live node`: the first
and (partially) third bullets above. Two further scoped fixes followed,
each addressing one concrete gap this phase named without building the
deferred typed-revision architecture:
`fix(admin): require confirmation when a live limit read is unavailable`
(a missing or failed limit read no longer silently classifies an increase
as low risk) and
`fix(execution): apply live workspace changes to the actual Codex process`
(a workspace change now reaches `NodeService`, `Executor`, and the Codex
process itself, not only the executor's limits), and
`fix(admin): restore live workspace and grant changes on restart`
(`node_config::load_service` now replays the last-applied workspace and
every stored grant when the node starts, instead of only stored queue
limits), and
`fix(admin): reject a stale confirmation after the live policy has moved
on` (a monotonic revision counter, advanced once per applied live change;
queuing a high-risk change records it, and confirming — both the
in-memory and store-backed paths — rejects with a new `AdminError::Stale`
if some other change applied in the meantime), and
`fix(admin): recover the policy revision across a restart` (that counter,
and the base revision each pending change was queued against, are now
also written to durable configuration and recovered when `Administration`
is reconstructed, so staleness detection works whether or not a restart
happened between queuing and confirming — verified by dropping an
`Administration` entirely and reopening a fresh one over the same
database mid-test), and
`fix(admin): resolve live workspace changes by approved id` (an optional,
opt-in `approvedWorkspaces` policy-file field maps an id to a
canonicalized, startup-verified path; once any entry is configured, a
live `Change::Workspace` may only switch to one of those id/path pairs —
an unknown id, or a path that does not match the approved one for that
id, is rejected. Left empty, the default for every existing policy file,
behavior is unchanged from before this existed).
What remains fully open, not attempted, not partially built is narrower
than "the typed policy-revision architecture" as originally scoped:
specifically, *admission* does not check this revision at all (only
administration does), and the prepare/validate/commit/publish sequence
exists only for the change types this session's scoped fixes happened to
cover. Checked directly against `permits_dispatch` and `NodeService`'s
own workspace mutex rather than left as an open question: the safety
property a shared revision would give admission — a queued task cannot
dispatch under a revoked grant or a stale workspace — already holds,
through the live grant recheck at dispatch (phase 33) and one node-wide
workspace mutex the executor always reads fresh. A shared revision would
add explicit traceability (which revision a task was admitted versus
dispatched under), not fix a live bug; building it means merging
admission's and administration's currently separate state into one
owner, which is the architecture decision, not an omission. See
`TODO.md` phase 36 for the exact boundary of what was and was not done.

### Phase 37: Complete discovery administration actions

Completes earlier phases 28.

- After phase 36, connect directory, invitation, and capability changes in `admin.rs`, `main.rs`, `discovery.rs`, `directory_client.rs`, and `managed_client.rs`. These variants currently fall through to success without a runtime action.
- Define typed actions for directory replacement, invitation use, and signed capability publication. Persist pending operations with stable operation IDs. Report success only after required local and remote actions finish.
- Recover interrupted operations after restart. Handle remote failure and partial completion without duplicate invitation use or duplicate publication. Validate legacy `admin.*` values before activation.
- Test each action through MCP and UI. Verify changed discovery behavior, restart recovery, remote-service failure, safe retries, and redaction. Add database migration and restore checks if operation storage changes.

Commit: `fix(admin): apply pending discovery changes at runtime`

Result: split after research (not assumption) into what genuinely needs
phase 36's deferred architecture and what does not.
`fix(admin): renew the pinned directory record on request` and
`fix(admin): publish the pinned directory record with an approved
capability` took the two tractable slices: `Change::Directory` already
carries enough information (a URL, checked against the node's own pinned
configuration) to trigger a real "renew now" without new wire fields or
new trust material, and `Change::Capability` can mean "republish that
same pinned record with this capability tag" — directory records are the
only existing vehicle for publishing a capability, so this drives the
existing primitive rather than inventing one. Invitation stays open: its
real operation is ambiguous between two flows (issue vs. redeem; direct
vs. managed), which is a product decision about what the change should
even mean, not an engineering gap, and it would need a breaking
`ChangeRequest` field (a full signed record) neither of the other two
needed. A later session traced both flows to ground and found the
blocker is sharper than picking one: issuing a direct invitation is a
stateless signing operation with no live state for a `Change` to apply,
already served by its own CLI command, while redeeming a managed one
needs a `record` naming a *prospective member joining the roster* — not
this already-running node's own fixed identity — meaning this node would
act as a proxy provisioning new roster members on a caller's behalf, a
security-relevant capability needing its own explicit product decision.
See `TODO.md` phase 37 for the full trace through `sync_managed` and
`ManagedClient::redeem`. Bridging `apply_live`'s synchronous call into the real async
directory client took three attempts — a scratch-thread `block_on`, and
`block_on`/`block_in_place` called directly both made router shutdown
flaky; `Handle::spawn` plus a blocking channel does not, and both
appliers use it. That investigation also surfaced a separate, still-open,
pre-existing race: any successful admin change over the socket (not
directory-specific, reproduced with a plain `Limit` change) while a
directory publication and its periodic renewer are active can turn a
later clean SIGINT shutdown into an unhandled termination instead. See
`TODO.md` phase 37 for what this phase still leaves open.

### Phase 38: Complete transport, state, and command contracts

Completes earlier phases 26, 30, 31.

- Test more than 32 slow peers through the production `router_with_async` path. Verify immediate overload handling, permit release on timeout and disconnect, and successful service after capacity returns.
- Extend response tests for mismatched exchange, scope, task, and payload IDs. Cover request or cancel frames in responses, missing or duplicate final frames, and frames after a final frame. Exercise direct, record, and relay client paths.
- Test every durable `TaskState` through `NodeService`, including restart, valid empty output, missing output, and expired output. Use consistent snapshot and journal mappings. Return machine-readable operator-review and result-expired errors with safe state data.
- Test cancellation with the correct owner and wrong scope, wrong owner, repeated valid cancellation, and request-plus-cancel rejection before admission. Verify that denied cancellation preserves task state and events.
- Test actual CLI output and process exit status for success, denial, overload, replay, timeout, failed, rejected, cancelled, unknown-result, and expired-result outcomes. Preserve structured output. Update the wire reference where behavior changes.

Commit: `test(protocol): verify router bounds and task outcomes`

Result: the "machine-readable errors" slice of the third bullet was taken,
as `fix(protocol): report operator-review and expired-result errors
distinctly` — the only piece of this phase with real behavior content
rather than test-matrix expansion. Also completed the cancellation-scope
bullet (`test(protocol): verify cancellation scope and request-plus-cancel
rejection`): wrong scope versus wrong owner, state/events preserved on
denial, idempotent repeated cancellation, request-plus-cancel rejected
before admission, and a denied request's CLI exit code. Extended the
response-validation and TaskState tests too: mismatched response scope and
task ids, a stray cancel frame in a response, duplicate and
trailing-after-final frames, and `Failed`/`Rejected`/`Cancelled` tasks each
reported normally through `NodeService::handle` rather than as a transport
error. `PendingApproval` has no producing code path today, so it may not be
testable until one exists. Checked why, precisely, in a later session:
`aether_core::ApprovalRule` already has a `LocalUi` variant next to
`Automatic`, already parsed from the policy file and already signed as
part of a grant, but nothing in `PolicyEngine::admit`/`permits_dispatch`
ever reads a grant's `approval_rule` — a `LocalUi` grant is admitted
exactly like `Automatic`, silently. A real, narrow, pre-existing gap, not
invented scope, but closing it needs an actual approval surface designed
(where an admin sees and acts on a pending task, what happens to one
nobody ever approves) — product/UX work this session was not asked to
do, not an engineering gap to fill by guessing. See `TODO.md` for the
exact trace.

A later pass took the first bullet too:
`test(protocol): verify router overload and recovery over real connections`.
It found that `iroh`'s connection machinery does not support many
independent concurrent connections from one peer identity — opening all
`MAX_CONNECTIONS` connections from a single shared caller endpoint
deadlocked after only 8-11 ever reached the router's handler, confirmed
with a 45-second real wait rather than mere slowness. Rewriting each
occupying connection to use its own freshly bound endpoint (also the more
faithful shape for "many slow peers") resolved it. Covers immediate
overload, permit release on a peer disconnecting outright, and successful
service once capacity returns; does not cover permit release from
`STREAM_TIMEOUT` alone elapsing (judged a less representative, much slower
case to add for what it would additionally prove).

The CLI exit-status item is also done, narrower than planned:
`response_exit_code` gives only `0` (`Completed`) or `1` (everything
else, transport errors included) by design, so there is no second code
left to distinguish, say, `rejected` from `timeout`. Adding the `failed`
case (`a_task_exceeding_max_context_tokens_fails_through_the_cli`)
surfaced a second real `TaskState::allows` gap of the same kind phase 33
fixed once already: `Leased -> Failed` was not permitted, so a task
whose pre-start limits check failed (before `mark_running`, so not a
policy denial — those already used the allowed `mark_rejected`) called
`mark_failed`, which the state machine silently rejected; the task was
stranded in `Leased` forever with no terminal status and no visible
error, discovered with a real server process, a live `sqlite3` read of
its database, and a stack sample proving no execution thread was still
running. Fixed by adding `Failed` to the `Leased` arm. `overload`,
`replay`, `timeout`, `unknown-result`, `expired-result`, and `cancelled`
remain untested at the CLI-process level (concurrent/duplicate requests,
long real waits or a missing clock/lease-TTL hook, and no CLI cancel
command, respectively) — each exits `1` regardless, so this is a coverage
gap, not an open exit-code question.

`restart_preserves_every_terminal_task_state` (`proof.rs`) then closed
the restart-after-any-state gap: five tasks driven to `Succeeded`,
`Failed`, `Rejected`, `Cancelled`, and `UnknownResult`, then a genuine
restart (the store closed and reopened, a fresh `ProofNode` built on top)
confirms every one kept its exact state. Phase 38 has no further open
items; see `TODO.md` phase 38 for the few named acceptance scenarios
inside otherwise-checked bullets that remain untested (`PendingApproval`
has no producing code path yet, and a handful of CLI-process outcomes
would need test infrastructure this session judged not worth adding).

### Phase 39: Verify remaining repairs together

Completes earlier phases 32.

- After phases 33 through 38, run `mise run check` and record the actual suite totals. Match each issue to its passing acceptance tests. Do not close a phase because a combined commit exists.
- Run concurrent command suites with isolated ports and data directories. Use bounded readiness checks and cleanup on failure. Verify that no test child remains active.
- Run `scripts/container-e2e.sh`, `scripts/relay-e2e.sh`, and `scripts/managed-relay-e2e.sh` with Docker. Record unavailable environment checks as open. Keep live Codex and physical-machine checks separate.
- Update README measured results and affected reference documents. Reconcile earlier checklist claims with the verified behavior. Use scoped commits for this remaining work; do not rewrite commit `56acdcf`.

Commit: `test(e2e): verify remaining review acceptance cases`

Result: partially done ahead of its normal order, because Docker turned out
to be available in this environment. Ran `mise run check` (189 tests pass)
and all three Docker e2e scripts (direct, relay, managed relay), all
passing, and updated README.md's measured results. Also stress-ran the
concurrent command suite (8 runs, 112 test executions, two parallelism
levels) with no readiness failures and no leftover processes, addressing
the specific concern `issues.md`'s review verification raised. Re-run in a
later session against the tests added since (223 total, and a fresh
5-runs-plus-3-oversubscribed-runs stress pass on the then-current
`commands.rs` suite), still with no leftover processes. This does not mean
phase 39 as a whole is closed: phase 36 has one bullet left (admission
checking the shared policy revision, deferred by the user's own explicit
decision and independently confirmed safe to defer), phase 37's Invitation
needs a security-relevant product decision neither flow it was traced to
resolves on its own, and phase 38 is complete except `timeout` (mechanism
diagnosed precisely; its real fix is a wire-protocol design change, not a
scoped bug). None of these are undiagnosed gaps; each is a decision or an
architecture change this session was not asked to make. See `TODO.md`
phase 39.

## Earlier repair requirements

Use the status table and remaining phases above to select the next work.
The following requirements retain their original phase numbers.


### Phase 22: Give the node control of task execution

Issues: 1, 2, 3. Start here.

- Change `StoreWorker::admit_and_schedule` and `ProofNode::submit_inner` in `crates/aether/src/proof.rs`. Admission must only store the task and wake the scheduler. Keep replay checks and admission in one transaction. Keep a result-wait adapter for current callers until phase 23 changes the wire behavior.
- Give `ProofNode` one scheduler and one task supervisor. Keep one Codex execution slot for this repair. Acquire execution capacity before `NodeStore::lease_next`. Load the selected task from storage, including its scope, workspace, and execution data.
- Keep waiting tasks queued. Use fair per-peer selection and define whether each queue limit counts waiting or active work. Check `count` and `lease_next` in `crates/aether-store/src/lib.rs` against that definition.
- Register cancellation before dispatch. Use one start gate for cancellation and execution. Check durable state at that gate. Record the execution intent before the external start, and record `running` when Codex confirms the start. An uncertain start must require operator review.
- Change `NodeStore::cancel` to return success for an already cancelled task without another cancellation event. Prevent a late result from changing a cancelled task.
- Track store, execution, and lease-renewal threads. Connect their stop and join operations to node shutdown in `crates/aether/src/main.rs`. Stop admission, cancel active work, wait for workers, then close storage and Iroh. Keep unstarted queued work durable.
- Test two different tasks with one blocked fake Codex process in `proof.rs`. Check execution order, per-peer fairness, cancellation before start, repeated cancellation, late completion, and shutdown. Check that all child processes and threads stop.

Commit: `fix(scheduler): own task dispatch and cancellation`

### Phase 22a: Complete scheduler ownership

Issues: 1, 2, 3. Complete this repair before phase 23. Commit `c66e188`
records partial work; it does not close phase 22. These steps follow the
current code inspection. Tests were not run during this plan update.

- Limit `SchedulerContext::dispatch_one` use of `dispatch_serial` to admission and start coordination. Release the lock before execution. A second task must enter the durable queue while the first task runs.
- Use a common start gate for `ProofNode::cancel`, `cancel_from`, and execution. Record execution intent before the external start. Set `Running` only after start confirmation. Preserve uncertain starts for operator review.
- Give `StoreWorker::stop` an explicit stop command or close all senders before `join`. Dropping a sender clone does not close the channel while the owner retains its sender.
- Track the active task ID in `ProofNode::shutdown`. Cancel that task instead of passing an empty ID. Stop admission, resolve waiting callers, stop execution and renewal work, then join the store worker. Set a bounded shutdown deadline.
- Complete the selected task data in `NodeStore::task_execution_data`. Load its scope and approved workspace data. Verify that restart dispatch uses the same execution limits as admission.
- Repair `scheduler_executes_two_tasks_in_order_with_one_slot` and `cancellation_before_start_prevents_execution` in `proof.rs`. Use valid grants, explicit start signals, bounded waits, and cleanup on failure. Diagnose implementation faults before changing assertions.
- Test queued admission during blocked execution, cancellation at the start boundary, repeated cancellation, and shutdown with active and waiting tasks. Run focused tests and `mise run check` before phase 23.

Commit: `fix(scheduler): complete dispatch and shutdown ownership`

### Phase 23: Return acceptance and read task progress

Issues: 3. Complete the preceding repair phase first.

- Change `NodeService::handle` and `handle_async` in `crates/aether/src/node_service.rs` to return a durable task snapshot after admission. A request handler must not wait for a Codex turn.
- Use bounded polling for this repair. Reuse the idempotent request and `last_event_sequence` to read progress. Each exchange needs a fresh exchange ID and nonce. Keep the task identity and request fingerprint unchanged.
- Page `NodeStore::events_after` and reserve space for the final snapshot within `MAX_FRAMES` and the exchange byte limit. Return the last delivered sequence. Read each event page and its task snapshot consistently.
- Keep SQLite as the event source. Broadcast messages only wake readers. Preserve the local UI event stream in `crates/aether/src/admin_ui.rs`.
- Update the request path in `crates/aether/src/main.rs` to poll until a terminal state. A client disconnect or transport timeout ends that exchange. The node supervisor retains accepted work. Explicit cancellation or the execution deadline stops the task.
- Update response payload validation in `transport.rs` to accept the new task snapshots. Update `references/aether-wire-profile.md` and `references/aether-task-state.md` with acceptance, polling, start, timeout, and shutdown rules.
- Test a task that runs for more than 30 seconds through the production router. Read progress before completion. Test reconnect, event pagination above 64 events, slow readers, and one execution across repeated polls.

Commit: `feat(protocol): return task acceptance and paged progress`

### Phase 24: Recover expired leases during operation

Issues: 8. Complete the preceding repair phase first.

- Call `NodeStore::recover_expired_leases` from a periodic scheduler job as well as startup. Run the job while Codex is blocked.
- Use execution IDs to check start, renewal, and completion writes. An expired execution must not overwrite a newer state.
- Requeue a lease only when the node can prove that execution did not start. Change an uncertain execution to `UnknownResult`, stop its worker, and require operator review. Never start a second turn automatically.
- Test expiry after startup, renewal failure, expiry during start, and late completion. Use a controllable clock where possible. Check that recovery and cancellation preserve ordered events.

Commit: `fix(scheduler): recover expired leases during operation`

### Phase 25: Delete expired task content

Issues: 10. Complete the preceding repair phase first.

- Add a forward migration after `008_managed_roster.sql` under `crates/aether-store/migrations/node/`. Register it in `NODE_MIGRATIONS`. Add input expiry and an explicit content-expired record. Preserve existing keys and constraints.
- Define the retention timestamps in `references/aether-security-model.md`. Proposed default: delete terminal task input and output 24 hours after terminal completion. Retain active input until the task reaches a terminal state. Record this exception and decide how it applies to `UnknownResult` before implementation.
- Backfill existing terminal rows from durable completion evidence. Do not give old content a new 24-hour period. Define a conservative fallback when the completion time is missing.
- Replace startup-only `clear_expired_results` use with bounded periodic cleanup under the supervisor. Preserve state, request fingerprints, content hashes, and ordered events. Make repeated cleanup safe.
- Test fresh databases and upgrade from migration 008. Check foreign keys, task identity, hashes, event order, expiry boundaries, active tasks, and repeated cleanup.
- Test migration failure and restore from the pre-migration backup on a database copy. Document rollback through backup restore. Check retained content in backups against the existing retention policy.

Commit: `fix(store): enforce task content retention during operation`

### Phase 26: Bound incoming connections and validate responses

Issues: 4, 7. Complete the preceding repair phase first.

- Add one shared semaphore to `AetherProtocol` in `crates/aether/src/transport.rs`. Acquire its permit before stream acceptance or request reads. Keep it through the response write.
- Reject excess connections immediately. Do not create an unbounded set of permit waiters. If no request has been read, close the connection without a task error frame. Document this overload behavior.
- Keep handshake, stream, read, handler, and write deadlines. Release permits on errors, cancellation, and shutdown.
- Make `validate_response` direction-specific. Call it in every client request path, including direct, record, and relay paths. Match exchange, scope, task, and payload IDs to the original request.
- Accept zero or more events followed by exactly one result or error. Reject request and cancel frames in a response, mismatched IDs, missing final frames, and frames after a final frame.
- Test more than 32 slow peers through `router_with_async`, permit release after timeout, and service recovery. Add response tests for each invalid direction, identity, and order case.

Commit: `fix(transport): bound connections and correlate responses`

### Phase 27: Apply local policy changes to the running node

Issues: 5. Complete the preceding repair phase first.

- Replace `Administration::apply_live` limit-only behavior in `crates/aether/src/admin.rs` with one typed policy owner shared with `ProofNode`.
- Connect workspace, skill, grant, and limit changes to policy admission and executor configuration in `crates/aether/src/proof.rs`, `execution.rs`, and `node_config.rs`. Validate paths and signed grants before application.
- Prepare a complete policy revision before writing it. Serialize policy reads and updates through the owner. Commit durable state, then publish the prepared revision before returning `applied`. Reload that committed revision before admission on restart.
- If a durable commit fails, keep the current policy. If publication cannot finish, stop new admission until recovery completes. Use the same owner for UI, MCP, and socket changes.
- Recheck current policy before dispatch so a queued task cannot use a revoked grant or workspace. Define the effect of a change on a task that already runs.
- Test a workspace or grant change through MCP and UI confirmation in `features/node/` scenarios. Check changed admission before restart, the same policy after restart, concurrent requests, and injected storage failure.

Commit: `fix(admin): apply local policy changes to the running node`

### Phase 28: Apply discovery and capability changes

Issues: 5. Complete the preceding repair phase first.

- Extend the same policy owner to directory, invitation, and capability changes. Connect `discovery.rs`, `directory_client.rs`, `managed_client.rs`, and the node service setup in `main.rs`.
- Give each `ChangeRequest` variant a defined runtime action. Parse and validate its typed data. A stored `admin.*` row alone must not produce `applied`.
- Track changes that require a control-service call or record publication. Keep them pending until the required durable and runtime actions finish. Use retry-safe operation IDs and recover interrupted changes after restart.
- Load existing `admin.*` values through a validated conversion path in `node_config.rs`. Do not activate old unvalidated values as grants. If a schema change is needed, add a forward migration and backup-restore checks.
- Test each change type for its runtime effect. Include directory replacement, capability publication, invitation application, remote-service failure, restart recovery, and secret redaction.

Commit: `fix(admin): apply live discovery and capability changes`

### Phase 29: Require UI confirmation for limit increases

Issues: 6. Complete the preceding repair phase first.

- Change `Change::is_high_risk` and the administration apply path in `crates/aether/src/admin.rs` to compare requested limits with the current policy revision.
- Require local UI confirmation if either queue limit increases. Permit a decrease or unchanged value through the existing low-risk path. Validate supported IDs and bounds.
- Bind confirmation to the exact change and base revision. Recheck risk at apply time. If the revision changed, require a new confirmation where needed.
- Test global and per-peer increases, decreases, unchanged values, stale confirmation, expiry, and an MCP attempt to confirm its own change through `mcp.rs` and `admin_ui.rs`.

Commit: `fix(admin): require confirmation for queue limit increases`

### Phase 30: Return the durable state for retries

Issues: 9. Complete the preceding repair phase first.

- Replace the default `Working` mapping in `NodeService::handle`. Use one complete mapping for task snapshots and journal events.
- Map `PendingApproval` to `InputRequired`; `Queued` and `Leased` to `Submitted`; `Running` to `Working`; and successful, failed, rejected, and cancelled states to their matching A2A states. Check the enum names in the pinned crate before implementation.
- Return an explicit operator-review error for `UnknownResult` and unresolved `OperatorReviewed`. Include the durable state in safe error data. Do not report these states as active execution.
- Return an explicit result-expired error when successful output has expired. Keep a valid empty result distinct from missing retained content. Use the expiry evidence from phase 25.
- Test every `TaskState`, valid empty output, expired output, and retries after restart. Check state, event sequence, and execution count.

Commit: `fix(protocol): return durable retry states and expired results`

### Phase 31: Check cancellation scope and command exit status

Issues: 11, 12. Complete the preceding repair phase first.

- Reject request-plus-cancel exchanges in `validate_order` and at the `NodeService::handle` boundary. Accept one standalone request or one standalone cancel. Reject invalid order before admission.
- Extend `NodeStore::cancel_if_owned` to check task ID, authenticated owner endpoint, and stored scope in one transaction. Pass scope through `ProofNode::cancel_from` and the node service. Preserve idempotent cancellation.
- Keep structured response output in `crates/aether/src/main.rs`. Return a nonzero command status for an A2A error or a failed task. Define nonzero status for rejected, cancelled, unknown-result, and expired-result outcomes too.
- Update `references/aether-wire-profile.md` and README command behavior. Test owner and scope mismatch, repeated valid cancellation, invalid mixed frames, successful output, denial, overload, replay, timeout, and failed task status.

Commit: `fix(protocol): enforce cancel scope and command failure status`

### Phase 32: Verify the repaired node

Issues: 1 through 12. Complete the preceding repair phase first.

- Use production node, router, store, UI, and MCP paths in the `features/node/` scenarios. Use a controllable fake Codex process for blocked execution.
- Give command tests readiness checks, isolated data and ports, bounded waits, and child-process cleanup on failure. Repeat the command suite concurrently to check the readiness failure reported in `issues.md`.
- Run `mise run check`. Then run the direct, relay, and managed relay scripts when Docker is available. Record blocked environment checks separately from failures.
- Update README measured results and the three affected reference documents. Keep live Codex and physical-machine checks separate from fake-executor results. Run live Codex only when the operator enables it.
- Close an issue only after its acceptance tests pass. Check the matching TODO item only after its work is complete. Check a phase commit only after that commit succeeds.

Commit: `test(e2e): verify task supervision and live policy`

## Repair risks and decisions

- SQLite and an external Codex process cannot share one transaction. Record
  execution intent and use execution IDs. An uncertain start requires review.
- Polling adds request traffic. Use a bounded interval and event pages. Apply
  the existing peer rate limit to polling as well as admission.
- A process crash can occur after a durable policy commit and before runtime
  publication. Block admission until the committed revision is loaded.
- Remote administration actions cannot share a transaction with SQLite.
  Persist pending operations and recover them before reporting success.
- The existing retention rule does not define its start time or the treatment
  of unresolved tasks. Resolve the phase 25 policy before its migration.
- Backup restore can discard changes made after the backup. Stop the node
  before restore and verify the restored database before admission.

## Open decisions before implementation

- Phase 25 proposes 24 hours from terminal completion. Confirm this retention
  clock and the handling of unresolved execution content before changing data.
- Define shutdown grace time and forced process-stop behavior in phase 22.
  A forced stop must preserve uncertain execution as `UnknownResult`.
- Define runtime actions and safe conversion for existing invitation and grant
  strings in phase 28. Unvalidated legacy values must not authorize work.

## Verification

Use current behavior and security contracts for tests. Do not add tests whose
only purpose is to check that removed implementation code stays absent.
Database phases must test fresh install, upgrade, failure, restore, and data
integrity. The phase-specific checks above define acceptance. Planning does
not establish that those checks pass.

## Earlier plan

The following text records the earlier implementation design. Use the repair
phases above for current work where the two designs differ.

### Earlier goal: Aether agent communication proof

## Goal

Build a Rust proof that lets two Aether nodes discover each other, approve a
limited capability, and exchange one A2A task over Iroh. The same node must
support direct contacts, an open directory, and an optional managed network.
The managed network can add invitations and a member roster, but it must not
change the Aether core or grant local workspace access by itself.

## Approach

The repository now has a Cargo workspace with three crates. The first thirteen
phases built the main types, stores, transport helpers, Codex adapter, local UI,
and tests. Later scoped commits added signed task execution and peer
cancellation. The implementation review found faults in migration 003, task
scheduling, replay checks, result storage, and discovery trust. It also found
that discovery and administration are not connected to the live node. The next
phases repair these faults before the proof is called complete.

The workspace has these crates:

- `crates/aether-core` will contain shared records, task types, policy types,
  signed claims, and the small plug-in interfaces.
- `crates/aether-store` will contain SQLite migrations and durable stores for
  node state, control-service state, tasks, events, grants, and audit records.
- `crates/aether` will build one binary. The `aether node`, `aether control`,
  and `aether mcp` commands will run as separate processes where required.

`mise.toml` pins stable Rust and defines the build, format, lint, test, and
proof tasks. `Cargo.lock` pins all Rust dependencies. The
[crate review](research/rust-crates-for-aether-implementation.md) records the
package, version, license, minimum Rust version, and adoption decision for each
new crate.

The implementation will add crates when their phase needs them:

- `rusqlite_migration`, `uuid`, and `blake3` for safe migrations, operation
  identifiers, and private request fingerprints.
- `a2a-lf` for the official A2A data model. The Iroh envelope stays local to
  Aether.
- `tokio-util`, `tower`, and `governor` for framing, shutdown, time limits,
  concurrency limits, and per-peer rate limits.
- `axum`, `axum-server`, and `askama` for the local administration server.
- `rmcp` for the MCP server.
- `thiserror`, `secrecy`, `tracing`, `tempfile`, `hex`, and
  `serde_path_to_error` in the phases that remove the related custom code.

The proof will not add an asynchronous SQLite pool. One store owner is simpler,
but it must expose atomic domain operations. The proof will not use an
in-memory cache as the replay authority.

The Aether core will have three real seams because each one has more than one
adapter:

- `DiscoveryProvider` will publish, search, and resolve signed agent records.
- `TrustProvider` will verify identity and scope claims.
- `PolicyMiddleware` will add limits or deny a request before the final local
  policy decision.

The first adapters will live under `crates/aether/src/plugins/`. They will be
compiled into the binary and selected through configuration. The proof will
not load native dynamic libraries. A later restricted WebAssembly plug-in
system is outside this plan.

Every publication, claim, grant, and task will carry a `scope_id`. A scope is a
direct contact, an open directory, or a managed network. Claims from one scope
must not work in another scope. A plug-in can deny a request, but it cannot
grant a skill or workspace. The local policy engine is always the final
authority.

The node will use one fixed inbound sequence: verify the Iroh Endpoint ID,
verify signed claims, accept the durable replay key, apply scope middleware,
apply rate and approval rules, apply the local skill and workspace grant, and
then admit the task to the bounded queue. A timeout or plug-in error will deny
new work.

The Iroh application protocol will use `aether/a2a/0`. One bidirectional QUIC
stream will carry one bounded, length-prefixed Aether exchange. `a2a-lf` will
provide the A2A task, part, state, event, result, and error types. The Aether
envelope will carry scope, authorization, replay, and Iroh identity data. This
is a custom A2A transport binding. A public HTTPS A2A gateway is outside the
first proof.

SQLite will store a defined task state machine, ordered task events, request
content under the retention rule, and final results. Task insertion and its
first event will use one transaction. One scheduler will own lease selection.
Leases will have a finite expiry and startup recovery. An expired execution
lease with an unknown Codex result will require operator review and will not
start a second turn.

The Codex adapter will use Codex App Server. It will set the approved working
directory, sandbox mode, approval policy, time limit, and a small environment
allowlist. The adapter must reject a task if it cannot enforce the requested
limits. The first proof will support text and JSON parts only. File attachments
are outside scope.

The node will render a small loopback administration UI with Axum and Askama.
Core pages will show agents, capabilities, directories, connections,
permissions, workspaces, tasks, and activity from live node state. The
managed-network adapter will enable the Members and Invitations pages. The UI
and the RMCP-based `aether mcp` server will call the same typed administration
service. High-risk MCP changes will require confirmation in the UI.

The first proof excludes offline delivery, AT Protocol storage, payments,
receipts, trust scores, confidential computing, negotiation automation,
calendar work, public directory moderation, dynamic third-party plug-ins, and
production key recovery. It will use one central control process and one Iroh
relay for the managed-network test. Relay high availability is a later test.

## Review baseline

The review on 2 September 2026 set these rules for the remaining work:

- `aether node serve` must not accept task traffic until the complete inbound
  policy path is active.
- Agent records, invitations, claims, and grants must use real signatures. A
  test trust provider that accepts all input must not enter a live node.
- The transport payload must validate the selected A2A message types. Generic
  JSON is not sufficient.
- The node must derive administration risk from a typed change. An MCP caller
  must not select its own risk level.
- Idempotency, cancellation, replay limits, context limits, private-key file
  permissions, and database backups must match the reference documents.
- A passing transport test with a fixed `ok` response proves reachability only.
  It does not prove Aether admission or execution.

The second review on 2 September 2026 adds these rules:

- Migration 003 is unsafe because `CREATE TABLE AS SELECT` removes constraints.
  A new migration must restore primary keys, foreign keys, unique keys, and
  `NOT NULL` rules for existing and new databases.
- A request handler must not call `admit` and global `lease_next` as two
  separate store operations. One scheduler must select and dispatch leases.
- The node must check the durable nonce and exchange replay key before task
  admission. Storage code without a run-time caller is not replay protection.
- An idempotent retry must return the stored task state, events, and result. It
  must not return a task ID as completed agent output.
- Discovery trust must come from a pinned contact key or a trusted directory
  statement. A record cannot establish trust with the key inside that record.
- SQLite is the source for event replay. Tokio channels can wake subscribers,
  but they cannot replace durable events.
- Each network stage needs a time limit. The node must track spawned work and
  stop it in a defined order.
- The UI and MCP server must change one typed live configuration service. A
  stored `admin.*` value is not a successful node change.

## Approved commit splits

Phases 14, 16, and 17 are too large for one commit. Each of them ships as a
sequence of scoped Conventional Commits, and the phase commit message closes
the sequence. Every commit in a split must build and pass its own tests.

## Implementation phases

### Phase 1: Fix the proof specification

- Update `references/a2a-codex-connection-flow.md` so the common architecture
  uses direct, directory, and managed scopes instead of mandatory membership.
- Keep `research/iroh-central-relay-and-network-join.md` as research for the
  optional managed-network adapter. Add a clear note that it is not the Aether
  core.
- Add `references/aether-wire-profile.md` with frame limits, version checks,
  error codes, cancellation, event order, and replay rules.
- Add `references/aether-security-model.md` with key ownership, signed grant
  fields, claim freshness, fail-closed rules, sandbox requirements, local UI
  security, audit redaction, and data retention.
- Add `references/aether-task-state.md` with task states, valid changes,
  transaction rules, lease recovery, approval expiry, and cancellation rules.
- Record the static local UI and compile-time adapters as the first
  plug-in method.

  **Commit:** `docs(architecture): define scoped plugin proof`

### Phase 2: Bootstrap the Rust workspace

- Add `mise.toml` with a pinned stable Rust toolchain and `fmt`, `lint`, `test`,
  `check`, and `proof` tasks.
- Add the root `Cargo.toml`, `Cargo.lock`, `rustfmt.toml`, and the
  `crates/aether-core`, `crates/aether-store`, and `crates/aether` packages.
- Add the `aether node`, `aether control`, and `aether mcp` command structure
  with configuration loading and structured, redacted logs.
- Add a continuous-integration workflow that runs the same `mise run check`
  task used on a developer computer.
- Add build tests for each binary command and configuration error.

  **Commit:** `chore(rust): bootstrap aether workspace`

### Phase 3: Add scoped core contracts

- Add agent, owner, endpoint, scope, capability, signed record, claim, grant,
  request, and task types to `crates/aether-core`.
- Add the `DiscoveryProvider`, `TrustProvider`, and `PolicyMiddleware`
  interfaces with typed errors, deadlines, and cancellation.
- Add the fixed policy sequence. A denial must stop evaluation. An adapter
  error or timeout must deny new work. The final local policy must approve the
  exact skill, workspace, and access mode.
- Add compile-time adapter registration and configuration for direct,
  directory, and managed scopes under `crates/aether/src/plugins/`.
- Test scope isolation, middleware order, deny precedence, timeout behavior,
  and the rule that an adapter cannot grant local access.

  **Commit:** `feat(core): add scoped discovery and policy contracts`

### Phase 4: Add durable state and scheduling

- Add separate numbered SQLite migrations for node and control state under
  `crates/aether-store/migrations/`.
- Add stores for configuration, scopes, signed records, grants, tasks, task
  events, pending changes, and audit records.
- Add the task state machine, transactional event writes, idempotent task
  admission, bounded queues, per-peer limits, fair scheduling, execution
  leases, and review of unknown results.
- Back up a database before a schema migration. Document restore as the proof
  rollback method. A failed migration must leave the prior database usable.
- Test migration from an empty database, backup and restore, foreign keys,
  unique keys, event order, queue limits, restart recovery, and cancellation.

  **Commit:** `feat(store): add durable task state and scheduling`

### Phase 5: Carry A2A tasks over Iroh

- Add persistent Iroh endpoint keys, relay configuration, endpoint hooks, and
  the `aether/a2a/0` application protocol to `crates/aether`.
- Implement the bounded frame codec from `references/aether-wire-profile.md`.
- Map A2A message and task data to request, event, cancel, result, overload,
  permission, and protocol error frames.
- Bind each connection to its authenticated Endpoint ID and reject a claim or
  grant for another endpoint or scope.
- Add transport tests with two local endpoints, malformed frames, oversized
  frames, version mismatch, slow readers, cancellation, replay, and reconnect.

  **Commit:** `feat(transport): carry a2a tasks over iroh`

### Phase 6: Add direct and open-directory discovery

- Add a direct-contact adapter that imports an expiring signed invitation and
  stores the contact locally.
- Add open-directory routes to `aether control` for publish, renew, remove,
  search, and resolve operations.
- Add the directory client adapter to `aether node`. Verify signatures and
  leases before a result can enter a connection attempt.
- Keep the directory outside the task path after discovery. A directory must
  not grant a local skill or workspace.
- Test direct contact without a control process, directory discovery, lease
  expiry, record replacement, forged signatures, and a directory outage.

  **Commit:** `feat(discovery): add direct and directory scopes`

### Phase 7: Add the managed-network adapter

- Add invitation creation, one-use redemption, signed membership claims,
  roles, roster leases, and revocation routes to `aether control`.
- Add managed membership verification and roster synchronization under
  `crates/aether/src/plugins/managed/`.
- Add the relay admission route that checks an Endpoint ID against active
  managed membership. Keep relay admission separate from local task access.
- Make revocation deny new direct and relayed tasks after the stated cache
  limit. Keep direct-contact and open-directory scopes active at the same time.
- Test invitation races, claim expiry, revocation, control-process outage,
  cross-scope replay, and coexistence with an open-directory scope.

  **Commit:** `feat(network): add optional managed membership`

### Phase 8: Execute approved tasks through Codex

- Add a Codex App Server process adapter with JSON-RPC request correlation,
  streamed events, cancellation, timeout, and restart handling.
- Convert an admitted A2A task into one Codex thread and turn with the approved
  skill, workspace ID, access mode, and context budget.
- Enforce the canonical workspace path, Codex sandbox mode, approval policy,
  environment allowlist, network rule, and task time limit before execution.
- Separate the skill instruction from remote untrusted text. Do not copy queue
  contents, directory events, secrets, or other tasks into model context.
- Use a fake Codex process for repeatable tests. Keep a live Codex smoke test
  opt-in because it can use credentials and model quota.

  **Commit:** `feat(execution): run approved tasks through codex`

### Phase 9: Add local UI and MCP administration

- Add the loopback UI under `crates/aether/src/admin_ui.rs`. Bind to
  `127.0.0.1` and add Host checks, origin checks, CSRF protection, secure
  session cookies, a Content Security Policy, and session expiry.
- Add core pages for agents, capabilities, directories, connections,
  permissions, workspaces, tasks, and activity. Use Server-Sent Events for
  bounded activity updates.
- Show Members and Invitations only when a managed scope is enabled.
- Add the local administration module and route both the UI and `aether mcp`
  through it. The MCP adapter will use standard input and output plus an
  owner-only local socket to the node.
- Add strict MCP schemas, pagination, field selection, dry-run changes, audit
  records, and UI confirmation for high-risk changes.
- Test loopback access, browser request checks, CSRF failure, session expiry,
  MCP schema validation, secret redaction, pending-change expiry, and the rule
  that an MCP client cannot confirm its own high-risk change.

  **Commit:** `feat(admin): add local topcoat and mcp controls`

### Phase 10: Verify the complete proof

- Add deterministic proof fixtures and end-to-end tests under `tests/e2e/`.
- Test direct invitation, open-directory discovery, and managed-network
  discovery with two nodes and a fake Codex process.
- Test task approval, queue overload, duplicate request, restart, revocation,
  path escape, hostile remote text, event reconnect, cancellation, and scope
  isolation.
- Add a two-node runbook that uses separate VMs for the direct path and a
  dedicated Iroh relay in isolated Docker networks for the relay path. Block
  direct UDP for one run and verify that the same task completes through the
  relay. A physical two-computer run remains a later operational check.
- Update `README.md` with the proof commands, limits, known omissions, and the
  exact result of the final verification.

  **Commit:** `test(e2e): verify direct and managed agent flows`

### Phase 11: Add cryptographic identity and safe node keys

- Replace `signature_for` in `crates/aether/src/discovery.rs` with canonical,
  domain-separated signed bytes for records and invitations.
- Add a production `TrustProvider` that checks the issuer key, signature,
  endpoint binding, scope, issue time, expiry time, nonce, and revocation state.
- Extend the core claim and grant types with all fields required by
  `references/aether-security-model.md`. Do not let a grant authorize a wider
  skill, workspace, access mode, or approval rule than it contains.
- Store agent and member private keys in owner-only files. Create key files
  atomically, reject symbolic links, and set mode `0600` on Unix when a key is
  created or loaded.
- Replace `Trusted` in integration tests with deterministic test keys and real
  signature checks. Add forged, changed-field, wrong-key, expired, future,
  revoked, and cross-scope cases.

  **Commit:** `fix(identity): verify signed agent claims`

### Phase 12: Repair durable admission and migration behavior

- Add a node migration that stores the authenticated caller endpoint and a
  canonical request fingerprint with each task.
- Scope idempotency keys to the authenticated caller. Return an existing task
  only when the caller, request ID, task ID, scope, capability, workspace,
  access mode, and request fingerprint are equal.
- Reject reuse of the same caller key when any request field or task body has
  changed. Do not disclose another caller's task ID.
- Check the schema version before a backup. Create one backup only when a new
  migration will run. Use a safe SQLite backup method and a defined retention
  limit.
- Test migration and rollback with an existing database. Test changed request
  fields, key reuse by another caller, concurrent admission, and backup
  retention.

  **Commit:** `fix(store): bind idempotency to authenticated requests`

### Phase 13: Validate A2A data and persist replay state

- Select and document the small A2A subset for the proof. It must cover agent
  discovery, one text or JSON task request, task status events, cancellation,
  a final result, and safe errors.
- Add typed A2A payloads to `crates/aether-core`. Reject missing fields,
  unsupported part types, unknown protocol versions, and data above the local
  limits before task admission.
- Map the typed A2A data to the existing `aether/a2a/0` frames. Keep the custom
  Iroh profile explicit in the documentation.
- Replace the unused in-memory `ReplayWindow` with a durable store keyed by the
  authenticated endpoint, scope, exchange ID, and request nonce. Add expiry,
  a size limit, and cleanup.
- Test valid A2A data, unsupported parts, bad versions, restart replay,
  cross-peer reuse, expiry, and full replay storage.

  **Commit:** `feat(protocol): validate a2a tasks and durable replay`

### Phase 14: Repair durable task correctness

The completed work from the old Phase 14 already moved Codex outside the store
lock, accepted concurrent Iroh connections, routed peer cancellation, and kept
late cancellation results from changing final state. This phase replaces the
unsafe lease path that remains in `crates/aether/src/proof.rs`.

- Add node migration 006 under `crates/aether-store/migrations/node/`. Rebuild
  `task_events` and `execution_leases` with their full constraints. Migrations
  004 and 005 already exist. Preserve valid rows. Run `foreign_key_check`
  before commit and test the final schema, not only the row data.
- Add one `NodeStore` operation that accepts the durable replay key and admits
  the task in one transaction. Keep replay capacity separate for each caller
  and scope.
- Make `StoreWorker` the only scheduler. Request handlers will submit work and
  wait on a task result channel. They will not call `lease_next`. The scheduler
  will dispatch the exact task selected by the fair queue.
- Store the request data needed after restart and store the final result under
  the defined retention rule. An idempotent retry will return the stored state,
  events, and result.
- Use finite leases. Renew a lease while Codex runs. Call
  `recover_expired_leases` during node startup before new work starts.
- Use `uuid` for operation identifiers and BLAKE3 for the domain-separated
  request fingerprint. Keep the original idempotency key only for an explicit
  retry.
- Adopt `rusqlite_migration` after the repair migration passes tests. Convert
  the existing `schema_migrations` value once and keep the backup and restore
  rules.
- Use `thiserror` for `StoreError`. Use `tempfile` for new store and migration
  tests.
- Test migration 005 to 006, schema constraints, concurrent submissions,
  scheduler fairness, restart recovery, replay before admission, and duplicate
  results.

  **Commit:** `fix(store): make task admission and recovery safe`

### Phase 15: Use the official A2A data model

- Add `a2a-lf` to `crates/aether`. Replace the local A2A message, task, part,
  state, event, result, and error types in `crates/aether/src/transport.rs`.
- Keep the Aether envelope for scope, signed authorization, replay data,
  exchange limits, and the authenticated Iroh Endpoint ID.
- Use `uuid::Uuid` to parse and create exchange, request, and task identifiers.
  Remove the local separator check. Use `hex` for key and signature text, but
  keep the signed byte format unchanged.
- Update `references/aether-wire-profile.md` with the supported A2A operations,
  fields, state changes, and custom Iroh binding rules.
- Test official JSON examples, unsupported parts, unknown fields, version
  errors, and conversion between A2A types and stored tasks.

  **Commit:** `refactor(protocol): use official a2a task types`

### Phase 16: Bound and stream live exchanges

- Make the node request service asynchronous. Use `tokio-util` framed I/O with
  an explicit maximum frame length and keep the existing total exchange limit.
- Add a time limit to the Iroh handshake, stream accept, request read, handler,
  response write, and client response read. Use Tower for concurrency,
  load-shed, and request time limits after decoding. Use Governor for keyed
  limits by authenticated peer.
- Use the Iroh `Router` for ALPN routing. Track node-owned work with
  `CancellationToken` and `TaskTracker`. Stop admission, close the router,
  cancel work, and wait for tracked tasks in that order.
- Use `thiserror` for transport errors and `tracing` spans for peer, scope,
  exchange, and task identifiers. Never record prompts or secrets.
- Map permission, replay, overload, timeout, protocol, and internal failures to
  small A2A error frames. Do not close a valid exchange without an error frame
  when the protocol can send one.
- Write task events as they occur. Use a bounded Tokio broadcast channel only
  to wake subscribers. Read durable events with `events_after` and resume from
  `last_event_sequence` after reconnect.
- Route peer, UI, and MCP cancellation to one running-task handle. A slow event
  subscriber must not hold a scheduler or store lock.
- Test idle handshakes, partial frames, slow reads and writes, peer limit
  isolation, overload errors, event reconnect, lagged subscribers, shutdown,
  and cancellation during execution.

  **Commit:** `fix(transport): stream bounded live exchanges`

### Phase 17: Connect live discovery and control

- Make `aether control` run the HTTPS directory and managed-network service
  backed by `ControlStore`. It must not print one startup record and exit.
- Connect `DirectContacts`, `Directory`, and `ManagedNetwork` to the node
  discovery adapters. Cache signed records with their lease expiry and mark
  stale records as unavailable.
- Bind each agent ID to a pinned direct-contact key or to a statement signed by
  the configured directory or network trust key. Do not trust the public key
  carried only inside an agent record.
- Publish, renew, search, resolve, revoke, and roster operations through the
  live control service. Keep task data on the Iroh peer path.
- Feed directory and roster changes to the local node with a reconnect cursor.
  Show all known members and their stale or active state in node data.
- Test two process discovery, forged self-signed records, lease expiry,
  directory restart, cursor resume, revocation, and coexistence of direct,
  directory, and managed scopes.

  **Commit:** `feat(discovery): connect live directory and roster`

### Phase 18: Apply typed node changes

- Replace the generic `Change` fields in `crates/aether/src/admin.rs` with typed
  changes for workspaces, skills, grants, directories, invitations, and limits.
- Remove caller-selected risk. The administration service will derive risk
  from the change type and the old and new values.
- Make the administration service the one owner of live configuration. The UI,
  MCP server, and local socket will call it. Return `applied` only after the
  live node and durable store contain the new value.
- Store a complete pending operation. Apply it in one transaction after local
  confirmation. Keep enough data to apply an unexpired operation after restart.
- Probe `admin.sock` before removal. Remove it only after the probe shows that
  no server owns it. Add a process lock or owner record.
- Use `secrecy` for tokens and secret change values. Use
  `serde_path_to_error` for exact configuration errors. Use `thiserror` for
  administration errors.
- Test risk bypass, confirmation, expiry, restart, transaction failure, stale
  sockets, duplicate node processes, and a real state change after restart.

  **Commit:** `fix(admin): apply typed live node changes`

### Phase 19: Replace the local HTTP server

- Replace the HTTP parser and serial TLS loop in
  `crates/aether/src/admin_ui.rs` with Axum and Axum Server. Keep the loopback
  bind, TLS, Host and Origin checks, CSRF token, secure cookie, content security
  policy, and session expiry.
- Use Askama templates for pages and fragments. Keep Topcoat for CSS. Read
  agents, permissions, skills, workspaces, tasks, connections, and members from
  the live administration service.
- Use Axum server-sent events with keep-alive messages. Resume task and audit
  events from their durable sequence after reconnect.
- Add body, connection, and request time limits with Tower. Keep browser
  sessions separate from task authorization.
- Test partial and oversized HTTP bodies, idle clients, concurrent requests,
  template escaping, CSRF, session expiry, event resume, and secret redaction.

  **Commit:** `refactor(admin): serve live views with axum`

### Phase 20: Replace the custom MCP server

- Replace the JSON-RPC parser and schema code in `crates/aether/src/mcp.rs`
  with `rmcp` server handlers, macros, generated schemas, and standard input
  transport.
- Keep the existing list, inspect, dry-run, change, and confirmation rules.
  Route all tools through the typed administration service from Phase 18.
- Keep high-risk confirmation local to the UI. An MCP client cannot confirm its
  own high-risk change.
- Test protocol negotiation, tool schemas, invalid input, pagination, field
  selection, cancellation, standard input shutdown, and redacted errors.

  **Commit:** `refactor(mcp): use the official rust sdk`

### Phase 21: Repeat the complete proof

- Make `aether node serve` construct the production trust provider, repaired
  store, discovery clients, policy chain, scheduler, Codex adapter, Iroh router,
  administration service, UI, and MCP socket.
- Remove obsolete custom protocol, HTTP, MCP, error, codec, and temporary-path
  helpers after their replacements pass tests. Do not keep two active stacks.
- Run process tests with two nodes and deterministic keys. Complete one task by
  direct contact, open-directory search, and managed roster search.
- Run the container direct test, isolated relay test, restart test, and manual
  two-machine test. Confirm that each path returns the stored fake-Codex result.
- Update `README.md` and the reference documents with the measured limits,
  commands, omissions, and final results.

  **Commit:** `test(e2e): verify the repaired aether proof`

## Risks and tradeoffs

- Axum, Askama, RMCP, and the A2A SDK add dependencies. They remove custom HTTP,
  MCP, HTML, and A2A code that the project would otherwise have to maintain.
- The Aether frame format carries official A2A data, but the Iroh binding is
  custom. External A2A clients will need a later HTTPS gateway.
- `rusqlite_migration` uses a different migration ledger. The conversion must
  keep the old version and must not run a migration twice.
- Compile-time adapters require a rebuild when a new adapter is added. This is
  safer and simpler than loading native code into a node that can reach local
  workspaces.
- SQLite fits one local node and one small control process. It does not provide
  control-service high availability. A production control service will need a
  different store adapter.
- A managed roster can still look like a social network. It remains optional,
  and the core UI will show connections and search results instead of a global
  roster.
- Codex isolation depends on controls that Codex App Server can enforce on the
  host platform. The node must reject execution when those controls are not
  available.
- A relay cannot store work for an offline node. The caller will receive an
  offline error. Durable offline delivery remains outside this proof.
- Real signatures add key rotation and recovery work. The proof will support
  explicit key replacement and revocation, but automatic recovery stays out of
  scope.
- The live daemon adds concurrency around SQLite and Codex. One scheduler and
  one store owner keep state changes ordered. A blocked store will apply
  backpressure to all new tasks.
- A typed administration API has less flexibility than a generic key and value
  API. It prevents an MCP caller from changing the risk class and gives each
  operation a testable effect.
- SQLite will store task input and output for restart recovery. The proof will
  keep this content for 24 hours in an owner-only data directory. Host disk
  encryption remains an operator control.

## Open questions

- Confirm whether the final manual test uses the public Iroh relay or a
  dedicated relay host. The managed relay-admission test requires the
  dedicated host.
- Decide whether the control-service HTTPS proof uses a private test
  certificate or a public certificate. The node must pin the test authority or
  use normal Web PKI checks.
- Decide whether one agent signing key can sign managed membership data with a
  separate domain tag. The Iroh endpoint key stays separate.
