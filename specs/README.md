# TLA+ models

Each `<Name>.tla` models one protocol of the node, and `<Name>.cfg` sets its
small constants and the properties that TLC checks. Run all of them with
`mise run tla`, or one with `sh scripts/tla.sh <Name>`. Each action in a
model names the function that it models. When you change that function,
change the model in the same commit.

| Model | Code | Checked properties |
|---|---|---|
| `DirectInvite` | `invites.rs`, `join_and_apply` in `access_groups.rs`, `import_bundle` and the contact functions in `direct.rs`, `PolicyEngine::admit`, `check_group` | A call that failed leaves no grant; the host admits a request only from an endpoint that the owner invited, with a claim signed for that endpoint, so a copied line gives nothing; the caller keeps only an authorization for its own endpoint; one contact name for one endpoint; a valid contact's name stays with its endpoint; each invite call ends (liveness) |
| `Admission` | `a2a_transport.rs`, `proof/admission.rs`, `PolicyEngine::admit`, `admit_with_replay` and `cancel_if_owned` in `aether-store/src/tasks.rs` | A task's owner is the authenticated endpoint, which had a live grant and a group membership; one task for each endpoint and idempotency key; a refused, replayed or changed frame changes nothing; the queue stays within its limits; no answer shows another endpoint's task; only the owner cancels; an honest caller with access gets its task in (liveness) |
| `ExecutionLease` | `NodeStore` lease functions, `SchedulerContext` in `proof/scheduler.rs`, `Agent::cancel` and `EarlyCancels` in `agent.rs` | A lease row exists only while a task is leased or running; a turn starts at most once for a task; a live turn's task is running, cancelled, or `unknown_result`; a cancel that an earlier run left does not stop a turn; every cancel reaches the turn (liveness) |
| `ConnectedAgent` | `agent/connected.rs`, `EarlyCancels` in `agent.rs`, `protocol/agent-socket` | An open task ID belongs to a running turn on that connection; an answer comes only from the agent of the turn's connection; a turn ends once; no agent, no task sent; a cancel before the start cancels the turn, and a cancel after the end is not kept; every turn ends with its time limit, and a turn on a closed or replaced connection or with a cancelled honest agent stops (liveness) |
| `EventPaging` | `events_page`, `task_progress` and the event writers in `aether-store/src/tasks.rs`, `A2aTransport::handle`, `request_with_polling_on`, `follow` in `outbox.rs` | Event sequences have no gap and never change; a page is the ordered run after the caller's cursor; a page says "no more events" only at the end of the journal; the state and the page are from one instant; the cursor never moves back; a `node request` that stops with an answer has every event up to the one that ended the task; an ended task lets the caller stop (liveness) |
| `Outbox` | `outbox.rs`, `sent_tasks` store functions, host admission | The host keeps one task for each sent task; an answer comes only from the host; a task that the host runs never shows as failed after a replay; an attempt whose lease another process claimed records nothing; an ended task does not change; each task ends (liveness) |
| `HubInvite` | `hub_sync.rs`, `hub_membership.rs`, `admin_ui/hub.rs` workers, `server/lib/aether_hub` | A grant only for an approved row and its first endpoint; no grant for a blocked endpoint; one live grant for each approval, also when the store fails; a kept line belongs to the approval it was signed for; the caller keeps only lines that it accepted; an honest hub delivers an accepted invite (liveness) |
| `ManagedRoster` | `managed_client.rs`, `directory_client.rs`, `control_service.rs`, `aether-store/src/{managed,directory,control}.rs`, `RosterClaims::verify` in `identity.rs` | A token makes one member; a roster row comes from a join event; a claim admits work only for a member that joined; with one control URL the roster is exactly the events up to the cursor and a revoke stays; a cursor never moves back or skips; the caller uses only an unexpired record of the trusted key and its scope, also with an attacker directory; a revoke reaches a node that keeps syncing, and directory events reach it unless the log is poisoned (liveness) |
| `PendingChange` | `admin.rs` (`Administration`), `aether-store/src/admin.rs`, `admin_socket.rs`, the start replay in `node_builder.rs` | A high-risk grant or a capability is live or stored only after a UI action for that id and value; a workspace or limit is live only from a UI action or a saved policy; the node and the store agree after each request unless a store write failed; a revoked grant stays out, also after a restart; a stale confirmation is refused while each revision was recorded; an expired pending row never leaves; each request ends (liveness) |
| `PactConsent` | `pact/`, `aether-store/src/pact.rs`, the dispatch recheck in `proof.rs` | One token for each device code; no token from a denied code; no task without a token; a failed approval leaves no grant; no task starts after its grant is revoked; an approved flow gets a token, and an admitted task ends (liveness) |

## Properties that are not checked

These properties are in the models but not in the `.cfg` files, because
TLC finds them false. The comment above each one gives the trace. "By
design" marks a property that the code does not intend to keep. The others
describe what the code does today.

| Model | Property | Why |
|---|---|---|
| `DirectInvite` | `ReturnedLineWorks` | Two calls of `sign_direct_invite` for one person share no lock: one fails and takes the person out of the group, the other returns its line. The grant is live but `check_group` refuses every request. |
| `DirectInvite` | `NoStrayMember` | In the same race, two failed calls can leave the person in the group. Without a grant the person gets no task in. |
| `Admission` | `NoTaskIdClash` | `tasks.id` is unique for the whole table, so a caller that reuses another endpoint's task ID blocks it. The random UUID keeps callers apart. |
| `Admission` | `NoSharedQuota` | The per-peer limit counts by agent name, so two endpoints that the owner invited under one name share it. |
| `Admission` | `NoReplayBeforeExpiry` | At `REPLAY_LIMIT` the oldest replay entry goes before its TTL. The idempotency key still gives one task. |
| `ConnectedAgent` | `NoLostCancel` | `run_turn` puts the task in `pending` before it writes `task`, so a cancel can reach the agent first. The second cancel from the lease renewal stops the turn. |
| `ConnectedAgent` | `NoOrphanWait` | An agent that closes only its sending side, in a race with the start of a turn, leaves the turn waiting until its time limit. |
| `ConnectedAgent` | `NoStaleCancel` | A cancel between `pending.remove` and `cancels.end` is kept. The node starts one turn for a task ID. |
| `ConnectedAgent` | `TaskOnlyToAttached`, `AnswerWhileAttached` | `serve` can replace the connection after a turn took it. The answer is still from the agent that got the task. |
| `ConnectedAgent` | `AnswerAfterSent` | By design, the agent is a process of the owner. It holds when a task ID gets one turn. |
| `EventPaging` | `OutboxReadsToEnd` | `follow` stops on the task status and does not read `aetherMoreEvents`. The outbox keeps only the answer, so only its stored cursor is short. |
| `HubInvite` | `GrantToTypedName` | By design, the hub resolves the invite name, so an attacker hub chooses it. |
| `HubInvite` | `KeptLineFromNamedHost` | By design, for an invite the caller takes the sender endpoint from the hub. A request checks the listing key. |
| `ManagedRoster` | `RevokeStays`, `EveryKeyCatchesUp` | The roster cursor is keyed by control URL but the roster is not: a second spelling of the URL replays the join over a revoke, or stops at the revoke for ever. |
| `ManagedRoster` | `NoAdmitAfterServiceRevoke` | By design, a revoke reaches the node at its next sync. |
| `ManagedRoster` | `ResolveIsForAgent` | `request_via_directory` does not compare the resolved record's agent ID with the one asked for. |
| `ManagedRoster` | `RevokedStaysOut` | Publish and revoke need only the signed record, which is public. |
| `ManagedRoster` | `DirSyncCatchesUp` | An expired or other-key record in the event log fails every later page. |
| `Outbox` | `OneAttempt` | Two processes can work on one task when an attempt takes longer than the 90 s `LEASE`. `NoStaleRecord` checks that the late attempt changes nothing. |
| `PactConsent` | `StartCovered` | A grant can expire between the dispatch recheck and `mark_running`. Expiry has no lock; `StartNotRevoked` checks revocation. |
| `PendingChange` | `LiveFromUi` | The MCP `UpdatePolicy` request saves a workspace and a higher limit with no confirmation; they apply at the next start. |
| `PendingChange` | `StaleRefused` | A stop between `apply_live` and `record_applied_change` loses the revision. |
| `PendingChange` | `StaleSeesRevoke` | By design, a revoke and a policy save do not change the revision. |
| `PendingChange` | `NoMcpRevival` | By design, a low-risk grant applies without confirmation, also for a revoked ID. |
| `PendingChange` | `LiveMatchesStoreAlways` | A failed `set_current_workspace` or `revert_change` leaves the node and the store different. |
| `PendingChange` | `Audited` | The audit record is written after the node has the change. |
