---------------------------- MODULE PactConsent ----------------------------
(***************************************************************************)
(* The PACT gateway: device flow, the owner's consent, revocation, and the *)
(* path from a delegated turn to a running Aether task.                    *)
(*                                                                         *)
(* Rust code that this spec models:                                        *)
(*   crates/aether/src/pact/gateway.rs  device_authorization, step_up,     *)
(*       create_device_authorization, token, redeem, issue_tokens,         *)
(*       begin_send, verify_delegation_header, run_task                    *)
(*   crates/aether/src/pact/consent.rs  PactConsent::approve, deny         *)
(*   crates/aether/src/pact/tokens.rs   verify_delegation                  *)
(*   crates/aether-store/src/pact.rs    approve_device, deny_device,       *)
(*       poll_device, use_refresh_token (DeviceStatus)                     *)
(*   crates/aether/src/admin.rs         Administration::revoke_ui          *)
(*   crates/aether/src/proof.rs         ProofNode::admit, revoke_grant,    *)
(*       SchedulerContext::prepare_leased, run_execution (the recheck in   *)
(*       on_confirmed_start, then mark_running)                            *)
(*   crates/aether-core/src/lib.rs      admit_verified, permits_dispatch   *)
(*                                                                         *)
(* One personal agent (PA) and one Brand. Each device code `c` can get     *)
(* clicks <<c, k>> from the owner. Each click signs its own Aether grant,  *)
(* so a grant id is a pair <<code, click>>. Time is abstract: each expiry  *)
(* is a flag that an `Expire*` action sets at any moment.                  *)
(*                                                                         *)
(* The live policy matches a task to a grant by caller, scope, capability, *)
(* workspace and access mode, not by grant id. All grants here are for    *)
(* the same PA and Brand, so a task is covered when ANY live grant that    *)
(* has not expired exists (`Covered`).                                     *)
(*                                                                         *)
(* Abstracted away: JWT signatures and the PA identity checks (a forged or *)
(* foreign token is the same as an invalid one), other callers (a poll by  *)
(* another PA gets `Unknown` and changes nothing), contexts and message    *)
(* reservation and retries, receipts, slow_down, queue limits, the access  *)
(* groups (they do not apply to the pact scope), the single-execution      *)
(* scheduler slot, lease expiry (see ExecutionLease), cancellation, and a  *)
(* crash between the two steps of `approve`. An access token has no        *)
(* expiry of its own here: an expired token gets 401 like a token for a    *)
(* revoked or expired grant, which the model covers. Only the first code   *)
(* gets a racing second click and refresh tokens (see `G` and `Tokens`).   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Codes,       \* device codes (pact_device_authorizations rows)
    Clicks,      \* concurrent approve clicks on one code
    MaxPerCode,  \* access tokens per code: 1 from the device code, the rest from refresh
    Msgs         \* turns (message:send requests)

\* To keep the state space small, only the first code gets a second
\* (racing) click and refresh tokens. The other codes get one click and
\* only the token from the device code.
FirstCode == CHOOSE c \in Codes : TRUE
FirstClick == CHOOSE k \in Clicks : TRUE
G == ({FirstCode} \X Clicks) \cup (Codes \X {FirstClick})          \* Aether grant ids
Tokens == (Codes \X {1}) \cup ({FirstCode} \X (1..MaxPerCode))  \* <<c, 1>>: from the device code
NoGrant == <<"none", 0>>
NoToken == <<"none", 0>>

CodeStates == {"none", "pending", "approved", "denied", "consumed"}
ClickStates == {"idle", "applied", "committed", "failed"}
TurnStates == {"idle", "auth_required", "invalid_token", "verified", "refused", "admitted"}
TaskStates == {"none", "queued", "leased", "checked", "running", "done", "rejected"}

VARIABLES
    code,          \* DeviceStatus of each code ("none": not created yet)
    codeExpired,   \* expires_at <= now
    click,         \* where each approve click is
    live,          \* the live policy holds the Aether grant
    gExpired,      \* the grant's expires_at <= now
    storedGrant,   \* pact_grants row (and grant_id) for each code
    issued,        \* history: access tokens issued by redeeming the device code
    refresh,       \* unused refresh tokens for the grant of each code
    tokIssued,     \* the PA holds this access token
    turn,          \* how the gateway answered each turn
    turnTok,       \* the delegation token that the turn carried
    task,          \* the Aether task of each turn (task id derives from the message)
    startedRevoked \* history: mark_running ran after the owner revoked every live grant

vars == <<code, codeExpired, click, live, gExpired, storedGrant, issued,
          refresh, tokIssued, turn, turnTok, task, startedRevoked>>

Covered == \E g \in G : live[g] /\ ~gExpired[g]

\* A recheck holds dispatch_serial from the policy recheck through
\* mark_running (run_execution, on_confirmed_start). ProofNode::revoke_grant
\* takes dispatch_serial too, so no revocation lands in that window.
DispatchHeld == \E m \in Msgs : task[m] = "checked"

TokenValid(t) ==
    LET g == storedGrant[t[1]] IN
    /\ tokIssued[t]
    /\ g # NoGrant
    /\ ~gExpired[g]
    /\ live[g]

TypeOK ==
    /\ code \in [Codes -> CodeStates]
    /\ codeExpired \in [Codes -> BOOLEAN]
    /\ click \in [G -> ClickStates]
    /\ live \in [G -> BOOLEAN]
    /\ gExpired \in [G -> BOOLEAN]
    /\ storedGrant \in [Codes -> G \cup {NoGrant}]
    /\ issued \in [Codes -> Nat]
    /\ refresh \in [Codes -> Nat]
    /\ tokIssued \in [Tokens -> BOOLEAN]
    /\ turn \in [Msgs -> TurnStates]
    /\ turnTok \in [Msgs -> Tokens \cup {NoToken}]
    /\ task \in [Msgs -> TaskStates]
    /\ startedRevoked \in [Msgs -> BOOLEAN]

Init ==
    /\ code = [c \in Codes |-> "none"]
    /\ codeExpired = [c \in Codes |-> FALSE]
    /\ click = [g \in G |-> "idle"]
    /\ live = [g \in G |-> FALSE]
    /\ gExpired = [g \in G |-> FALSE]
    /\ storedGrant = [c \in Codes |-> NoGrant]
    /\ issued = [c \in Codes |-> 0]
    /\ refresh = [c \in Codes |-> 0]
    /\ tokIssued = [t \in Tokens |-> FALSE]
    /\ turn = [m \in Msgs |-> "idle"]
    /\ turnTok = [m \in Msgs |-> NoToken]
    /\ task = [m \in Msgs |-> "none"]
    /\ startedRevoked = [m \in Msgs |-> FALSE]

-----------------------------------------------------------------------------
(* Device flow and consent *)

\* Gateway::device_authorization and Gateway::step_up, through
\* create_device_authorization: a new row in status `pending`.
RequestCode(c) ==
    /\ code[c] = "none"
    /\ code' = [code EXCEPT ![c] = "pending"]
    /\ UNCHANGED <<codeExpired, click, live, gExpired, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* Time passes the code's expires_at (DEVICE_CODE_SECONDS).
ExpireCode(c) ==
    /\ code[c] # "none"
    /\ ~codeExpired[c]
    /\ codeExpired' = [codeExpired EXCEPT ![c] = TRUE]
    /\ UNCHANGED <<code, click, live, gExpired, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* PactConsent::approve, first half: pending_device finds the code pending
\* and not expired, sign_grant, then administration.apply_ui puts the grant
\* in the live policy. Two clicks on the same code can both get here.
ApplyGrant(g) ==
    LET c == g[1] IN
    /\ click[g] = "idle"
    /\ code[c] = "pending"
    /\ ~codeExpired[c]
    /\ click' = [click EXCEPT ![g] = "applied"]
    /\ live' = [live EXCEPT ![g] = TRUE]
    /\ UNCHANGED <<code, codeExpired, gExpired, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* PactConsent::approve, second half: PactStore::approve_device moves
\* `pending` to `approved` and stores the PactGrant in one transaction, only
\* while the code is pending and not expired. Otherwise approve calls
\* revoke_ui (-> ProofNode::revoke_grant, under dispatch_serial) and takes
\* the live grant back.
CommitGrant(g) ==
    LET c == g[1] IN
    /\ click[g] = "applied"
    /\ (code[c] = "pending" /\ ~codeExpired[c]) \/ ~DispatchHeld
    /\ IF code[c] = "pending" /\ ~codeExpired[c]
       THEN /\ code' = [code EXCEPT ![c] = "approved"]
            /\ storedGrant' = [storedGrant EXCEPT ![c] = g]
            /\ click' = [click EXCEPT ![g] = "committed"]
            /\ UNCHANGED live
       ELSE /\ click' = [click EXCEPT ![g] = "failed"]
            /\ live' = [live EXCEPT ![g] = FALSE]
            /\ UNCHANGED <<code, storedGrant>>
    /\ UNCHANGED <<codeExpired, gExpired, issued, refresh, tokIssued,
                   turn, turnTok, task, startedRevoked>>

\* PactConsent::deny (also approve with an empty choice):
\* PactStore::deny_device, only from `pending`.
Deny(c) ==
    /\ code[c] = "pending"
    /\ code' = [code EXCEPT ![c] = "denied"]
    /\ UNCHANGED <<codeExpired, click, live, gExpired, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* Administration::revoke_ui -> ProofNode::revoke_grant: the live policy
\* drops the grant at any time, also while a turn or a task is in flight,
\* but never between a recheck and its mark_running: revoke_grant holds
\* dispatch_serial. It does not cancel a task that runs.
Revoke(g) ==
    /\ live[g]
    /\ ~DispatchHeld
    /\ live' = [live EXCEPT ![g] = FALSE]
    /\ UNCHANGED <<code, codeExpired, click, gExpired, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* Time passes the grant's expires_at (GRANT_SECONDS). An access token's
\* exp is at most the grant's, so this also ends its tokens.
ExpireGrant(g) ==
    /\ click[g] # "idle"
    /\ ~gExpired[g]
    /\ gExpired' = [gExpired EXCEPT ![g] = TRUE]
    /\ UNCHANGED <<code, codeExpired, click, live, storedGrant, issued,
                   refresh, tokIssued, turn, turnTok, task, startedRevoked>>

\* Gateway::token with grant_type=device_code: PactStore::poll_device
\* (an expired code answers `Expired` and changes nothing; an approved code
\* is consumed by the first poll that sees it), then the `bound` check in
\* Gateway::redeem (has_grant, grant not expired), then issue_tokens.
\* `pending`, `denied` and `consumed` answer an error and change nothing.
Redeem(c) ==
    LET g == storedGrant[c] IN
    /\ code[c] = "approved"
    /\ ~codeExpired[c]
    /\ code' = [code EXCEPT ![c] = "consumed"]
    /\ IF live[g] /\ ~gExpired[g]
       THEN /\ issued' = [issued EXCEPT ![c] = @ + 1]
            /\ tokIssued' = [tokIssued EXCEPT ![<<c, 1>>] = TRUE]
            /\ refresh' = [refresh EXCEPT ![c] = @ + 1]
       ELSE UNCHANGED <<issued, tokIssued, refresh>>
    /\ UNCHANGED <<codeExpired, click, live, gExpired, storedGrant,
                   turn, turnTok, task, startedRevoked>>

\* Gateway::token with grant_type=refresh_token: use_refresh_token spends
\* the token only while the grant has not expired; then the `bound` check
\* and issue_tokens, which gives a new access and refresh token.
UseRefresh(c, n) ==
    LET g == storedGrant[c]
        t == <<c, n>>
    IN
    /\ n > 1
    /\ t \in Tokens
    /\ refresh[c] > 0
    /\ ~tokIssued[t]
    /\ ~gExpired[g]
    /\ IF live[g]
       THEN /\ tokIssued' = [tokIssued EXCEPT ![t] = TRUE]
            /\ UNCHANGED refresh
       ELSE /\ refresh' = [refresh EXCEPT ![c] = @ - 1]
            /\ UNCHANGED tokIssued
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, turn, turnTok, task, startedRevoked>>

-----------------------------------------------------------------------------
(* Turns and tasks *)

\* Gateway::begin_send. No delegation header: step_up answers
\* TASK_STATE_AUTH_REQUIRED and no task. A header that fails
\* verify_delegation_header (signature, exp, stored grant, grant expiry,
\* has_grant) answers 401 invalid_token. Otherwise the turn is verified.
SendTurn(m, t) ==
    /\ turn[m] = "idle"
    /\ IF t = NoToken THEN TRUE ELSE tokIssued[t]
    /\ turnTok' = [turnTok EXCEPT ![m] = t]
    /\ turn' = [turn EXCEPT ![m] =
                  IF t = NoToken THEN "auth_required"
                  ELSE IF TokenValid(t) THEN "verified"
                  ELSE "invalid_token"]
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, task, startedRevoked>>

\* Gateway::run_task -> NodeService::admit_verified -> ProofNode::admit:
\* PolicyEngine::admit_verified under the policy lock, then enqueue. A
\* revocation can land between begin_send and this step.
Admit(m) ==
    /\ turn[m] = "verified"
    /\ IF Covered
       THEN /\ turn' = [turn EXCEPT ![m] = "admitted"]
            /\ task' = [task EXCEPT ![m] = "queued"]
       ELSE /\ turn' = [turn EXCEPT ![m] = "refused"]
            /\ UNCHANGED task
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, turnTok, startedRevoked>>

\* NodeStore::lease_next, then SchedulerContext::prepare_leased:
\* permits_dispatch, or mark_rejected.
Lease(m) ==
    /\ task[m] = "queued"
    /\ task' = [task EXCEPT ![m] = IF Covered THEN "leased" ELSE "rejected"]
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, turn, turnTok, startedRevoked>>

\* run_execution, on_confirmed_start: under dispatch_serial and the policy
\* lock, permits_dispatch against the live grants, or mark_rejected. The
\* policy lock is released after this step; dispatch_serial is held until
\* `Start` (see DispatchHeld). Time is not under any lock.
Recheck(m) ==
    /\ task[m] = "leased"
    /\ task' = [task EXCEPT ![m] = IF Covered THEN "checked" ELSE "rejected"]
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, turn, turnTok, startedRevoked>>

\* run_execution, on_confirmed_start continued: NodeStore::mark_running,
\* still under dispatch_serial, then the agent process starts. It does not
\* read the policy again.
Start(m) ==
    /\ task[m] = "checked"
    /\ task' = [task EXCEPT ![m] = "running"]
    /\ startedRevoked' = [startedRevoked EXCEPT ![m] = @ \/ ~\E g \in G : live[g]]
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, turn, turnTok>>

\* mark_succeeded / mark_failed. Revocation does not stop a running task.
Finish(m) ==
    /\ task[m] = "running"
    /\ task' = [task EXCEPT ![m] = "done"]
    /\ UNCHANGED <<code, codeExpired, click, live, gExpired, storedGrant,
                   issued, refresh, tokIssued, turn, turnTok, startedRevoked>>

Next ==
    \/ \E c \in Codes : RequestCode(c) \/ ExpireCode(c) \/ Deny(c) \/ Redeem(c)
    \/ \E c \in Codes, n \in 1..MaxPerCode : UseRefresh(c, n)
    \/ \E g \in G : ApplyGrant(g) \/ CommitGrant(g) \/ Revoke(g) \/ ExpireGrant(g)
    \/ \E m \in Msgs : \/ \E t \in Tokens \cup {NoToken} : SendTurn(m, t)
                       \/ Admit(m) \/ Lease(m) \/ Recheck(m) \/ Start(m) \/ Finish(m)

\* The owner's click completes, the PA keeps polling its code, and the
\* scheduler keeps working.
Fairness ==
    /\ \A g \in G : WF_vars(CommitGrant(g))
    /\ \A c \in Codes : WF_vars(Redeem(c))
    /\ \A m \in Msgs : WF_vars(Admit(m)) /\ WF_vars(Lease(m))
                       /\ WF_vars(Recheck(m)) /\ WF_vars(Start(m)) /\ WF_vars(Finish(m))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety *)

\* A device code gives at most one access token: poll_device consumes it.
OneTokenPerCode == \A c \in Codes : issued[c] <= 1

\* A denied code never gives a token.
DeniedGivesNoToken == \A c \in Codes : code[c] = "denied" => issued[c] = 0

\* A token comes only from a code that the owner approved (now consumed).
TokenOnlyFromApproval ==
    \A t \in Tokens : tokIssued[t] =>
        /\ code[t[1]] = "consumed"
        /\ storedGrant[t[1]] # NoGrant
        /\ click[storedGrant[t[1]]] = "committed"

\* At most one click commits per code, and the stored grant is that click.
OneCommitPerCode ==
    \A g \in G : click[g] = "committed" <=> storedGrant[g[1]] = g

\* Every live Aether grant from the consent page has its PactGrant row, or
\* its approve is still in progress: a failed approve leaves no live grant.
NoOrphanGrant ==
    \A g \in G : live[g] => click[g] = "applied" \/ storedGrant[g[1]] = g

\* A turn without a delegation token never creates a task: it gets
\* TASK_STATE_AUTH_REQUIRED. An invalid token gets 401 and no task either.
NoTokenNoTask ==
    \A m \in Msgs :
        /\ turnTok[m] = NoToken /\ turn[m] # "idle" => turn[m] = "auth_required"
        /\ task[m] # "none" => turn[m] = "admitted" /\ turnTok[m] # NoToken

\* A task reaches `checked` (the confirmed-start recheck passed) or later
\* only through `Recheck`, which requires a live, unexpired grant at that
\* moment under the policy lock. After every covering grant is revoked or
\* expired, no further task passes the recheck: `Recheck` rejects it.
StartsOnlyAfterRecheck ==
    \A m \in Msgs : task[m] \in {"checked", "running", "done"} =>
        turn[m] = "admitted"

\* No task starts after a revocation removed the grant that its recheck
\* used: when mark_running runs, a grant that the owner did not revoke is
\* still in the live policy. Before ProofNode::revoke_grant took
\* dispatch_serial, TLC found Admit, Lease, Recheck, Revoke, Start.
\*
\* Grant EXPIRY is not covered: permits_dispatch reads the clock in the
\* recheck, and nothing stops the clock before mark_running, so a grant
\* can pass its expires_at between the two (Admit, Lease, Recheck,
\* ExpireGrant, Start). The gap is as short as the recheck-to-start path,
\* at the end of a 30-day grant.
StartNotRevoked == \A m \in Msgs : ~startedRevoked[m]

-----------------------------------------------------------------------------
(* Liveness *)

\* An approved code gives the PA a token, unless the code or the grant
\* expires or the owner revokes the grant first.
ApprovedGetsToken ==
    \A c \in Codes :
        code[c] = "approved" ~>
            \/ tokIssued[<<c, 1>>]
            \/ codeExpired[c]
            \/ storedGrant[c] # NoGrant
               /\ (~live[storedGrant[c]] \/ gExpired[storedGrant[c]])

\* An admitted task always ends: it runs to the end or the recheck rejects it.
AdmittedTaskEnds ==
    \A m \in Msgs : task[m] = "queued" ~> task[m] \in {"done", "rejected"}

=============================================================================
