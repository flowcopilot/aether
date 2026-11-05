# TLA+ models

Each `<Name>.tla` models one protocol of the node, and `<Name>.cfg` sets its
small constants and the properties that TLC checks. Run all of them with
`mise run tla`, or one with `sh scripts/tla.sh <Name>`. Each action in a
model names the function that it models. When you change that function,
change the model in the same commit.

| Model | Code | Checked properties |
|---|---|---|
| `ExecutionLease` | `NodeStore` lease functions, `SchedulerContext` in `proof.rs`, `Agent::cancel` | A lease row exists only while a task is leased or running; a turn starts at most once for a task; a live turn's task is running, cancelled, or `unknown_result`; every cancel reaches the turn (liveness) |
| `Outbox` | `outbox.rs`, `sent_tasks` store functions, host admission | The host keeps one task for each sent task; an answer comes only from the host; a task that the host runs never shows as failed after a replay; an attempt whose lease another process claimed records nothing; an ended task does not change; each task ends (liveness) |
| `HubInvite` | `hub_sync.rs`, `hub_membership.rs`, `admin_ui/hub.rs` workers, `server/lib/aether_hub` | A grant only for an approved row and its first endpoint; no grant for a blocked endpoint; one live grant for each approval, also when the store fails; a kept line belongs to the approval it was signed for; the caller keeps only lines that it accepted; an honest hub delivers an accepted invite (liveness) |
| `PactConsent` | `pact/`, `aether-store/src/pact.rs`, the dispatch recheck in `proof.rs` | One token for each device code; no token from a denied code; no task without a token; a failed approval leaves no grant; no task starts after its grant is revoked; an approved flow gets a token, and an admitted task ends (liveness) |

## Properties that are not checked

These properties are in the models but not in the `.cfg` files. The
comment above each one says why.

| Model | Property | Why |
|---|---|---|
| `HubInvite` | `GrantToTypedName` | By design, the hub resolves the invite name, so an attacker hub chooses it. |
| `HubInvite` | `KeptLineFromNamedHost` | By design, for an invite the caller takes the sender endpoint from the hub. A request checks the listing key. |
| `Outbox` | `OneAttempt` | Two processes can work on one task when an attempt takes longer than the 90 s `LEASE`. `NoStaleRecord` checks that the late attempt changes nothing. |
| `PactConsent` | `StartCovered` | A grant can expire between the dispatch recheck and `mark_running`. Expiry has no lock; `StartNotRevoked` checks revocation. |
