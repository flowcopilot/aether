--------------------------- MODULE ExecutionLease ---------------------------
(***************************************************************************)
(* The execution lease of a task that this node runs for a caller.        *)
(*                                                                         *)
(* Store side: `NodeStore` in crates/aether-store/src/lib.rs               *)
(*   (`TaskState::allows`, `lease_next`, `mark_running`, `renew_lease`,    *)
(*   `mark_succeeded`, `mark_failed`, `mark_unknown_result`,               *)
(*   `mark_rejected`, `cancel`, `requeue_leased`,                          *)
(*   `recover_expired_leases`).                                            *)
(* Node side: `SchedulerContext` in crates/aether/src/proof/scheduler.rs   *)
(*   (`prepare_execution`, `prepare_leased`, `run_execution` with its      *)
(*   lease-renewal thread and `on_confirmed_start`, `settlement`,          *)
(*   `maintain`), and `ProofNode::cancel` / `ProofNode::shutdown` in       *)
(*   crates/aether/src/proof/mod.rs.                                       *)
(* Agent side: `Agent::start_turn_with_events` and `Agent::cancel` in      *)
(*   crates/aether/src/agent/{codex,connected}.rs, and `EarlyCancels` in   *)
(*   crates/aether/src/agent.rs. An adapter keeps a cancel for a turn that *)
(*   it did not register yet, and cancels that turn when it registers it.  *)
(*   It drops a cancel for a turn that ended, and `Agent::forget` drops a  *)
(*   kept cancel when the execution ends.                                  *)
(*                                                                         *)
(* One node process runs one scheduler, so at most one execution is       *)
(* active. The process can stop (crash or exit) and start again. Each     *)
(* store call is one atomic step, as each runs in one SQLite transaction  *)
(* on the store worker thread. The clock is a small counter.              *)
(*                                                                         *)
(* Abstracted away: task input and output, events, retention, the order   *)
(* in which `lease_next` picks a queued task, queue limits, and the       *)
(* `event_error` path of `run_execution`, and the bound of 256 task IDs    *)
(* in each `EarlyCancels` list.                                            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Tasks,      \* task IDs
    MaxExec,    \* how many execution IDs (`Uuid::new_v4`) the model hands out
    TTL,        \* LEASE_TTL, in clock ticks
    MaxTime,    \* the clock stops here
    MaxRestarts \* how often the node process may stop and start again

States == {"pending_approval", "queued", "leased", "running", "unknown_result",
           "operator_reviewed", "succeeded", "failed", "rejected", "cancelled"}
Terminal == {"succeeded", "failed", "rejected", "cancelled"}

\* `TaskState::allows`.
Allows(from, to) ==
    CASE from = "pending_approval" -> to \in {"queued", "rejected", "cancelled"}
      [] from = "queued" -> to \in {"leased", "rejected", "cancelled"}
      [] from = "leased" -> to \in {"queued", "running", "unknown_result",
                                    "rejected", "failed", "cancelled"}
      [] from = "running" -> to \in {"succeeded", "failed", "unknown_result",
                                     "cancelled"}
      [] from = "unknown_result" -> to = "operator_reviewed"
      [] from = "operator_reviewed" -> to \in {"succeeded", "failed", "cancelled"}
      [] OTHER -> FALSE

Execs == 1..MaxExec
NoLease == [exec |-> 0, exp |-> 0]
Phases == {"idle", "leased", "prepared", "confirm", "confirm_check",
           "starting", "turn", "finish", "finish_renewal"}
Outcomes == {"none", "ok", "err", "cancelled"}

VARIABLES
    state,      \* tasks.state
    lease,      \* execution_leases row of each task, or NoLease
    now,        \* the clock
    nextExec,   \* the next fresh execution ID
    grant,      \* whether policy and access groups still allow the task
    w,          \* the one execution thread (`run_execution`)
    turn,       \* the agent turn that the execution started, and the
                \* adapter's `EarlyCancels`
    signals,    \* tasks with an `Agent::cancel` call that is not made yet
    shutdown,   \* the `shutdown` flag
    restarts,   \* how often the process stopped
    starts      \* how often an agent turn started for each task

vars == <<state, lease, now, nextExec, grant, w, turn, signals, shutdown,
          restarts, starts>>

IdleWorker == [phase |-> "idle", task |-> CHOOSE t \in Tasks : TRUE, exec |-> 0,
               confirmed |-> FALSE, renewFailed |-> FALSE, outcome |-> "none"]
\* `early` and `ended` are the two lists of `EarlyCancels`. `spurious` is a
\* ghost: the adapter cancelled a turn at its start with a cancel that an
\* earlier execution of the task left behind (see NoSpuriousCancel).
NoTurn == [live |-> FALSE, registered |-> FALSE, interrupted |-> FALSE,
           task |-> CHOOSE t \in Tasks : TRUE, early |-> {}, ended |-> {},
           spurious |-> FALSE]

\* The turn is over: `NoTurn`, with the adapter's lists kept.
TurnOver(early, ended, spurious) ==
    [NoTurn EXCEPT !.early = early, !.ended = ended, !.spurious = spurious]

TypeOK ==
    /\ state \in [Tasks -> States]
    /\ lease \in [Tasks -> [exec : 0..MaxExec, exp : 0..(MaxTime + TTL)]]
    /\ now \in 0..MaxTime
    /\ nextExec \in 1..(MaxExec + 1)
    /\ grant \in [Tasks -> BOOLEAN]
    /\ w \in [phase : Phases, task : Tasks, exec : 0..MaxExec,
              confirmed : BOOLEAN, renewFailed : BOOLEAN, outcome : Outcomes]
    /\ turn \in [live : BOOLEAN, registered : BOOLEAN, interrupted : BOOLEAN,
                 task : Tasks, early : SUBSET Tasks, ended : SUBSET Tasks,
                 spurious : BOOLEAN]
    /\ signals \subseteq Tasks
    /\ shutdown \in BOOLEAN
    /\ restarts \in 0..MaxRestarts
    /\ starts \in [Tasks -> Nat]

Init ==
    /\ state = [t \in Tasks |-> "pending_approval"]
    /\ lease = [t \in Tasks |-> NoLease]
    /\ now = 0
    /\ nextExec = 1
    /\ grant = [t \in Tasks |-> TRUE]
    /\ w = IdleWorker
    /\ turn = NoTurn
    /\ signals = {}
    /\ shutdown = FALSE
    /\ restarts = 0
    /\ starts = [t \in Tasks |-> 0]

-----------------------------------------------------------------------------
(* Store operations. Each returns the new [state, lease] of task t. A     *)
(* `change` to a state that `allows` refuses rolls back the transaction.  *)

LeaseIs(t, e) == lease[t].exec = e

Set(t, s, l) == [st |-> [state EXCEPT ![t] = s], ls |-> [lease EXCEPT ![t] = l]]
Same == [st |-> state, ls |-> lease]

\* `mark_succeeded`, `mark_failed`, `mark_unknown_result`: no-op on a
\* cancelled task, else the lease must match.
MarkEnd(t, e, s) ==
    IF state[t] = "cancelled" THEN Same
    ELSE IF LeaseIs(t, e) /\ Allows(state[t], s) THEN Set(t, s, NoLease)
    ELSE Same

\* `mark_rejected`: the lease must match.
MarkRejected(t, e) ==
    IF LeaseIs(t, e) /\ Allows(state[t], "rejected")
    THEN Set(t, "rejected", NoLease) ELSE Same

\* `requeue_leased`: it does not check the execution ID.
Requeue(t) ==
    IF state[t] = "leased" THEN Set(t, "queued", NoLease) ELSE Same

Apply(r) == state' = r.st /\ lease' = r.ls

\* `Agent::cancel`: it interrupts a turn that the adapter registered.
\* Otherwise `EarlyCancels::cancel` keeps it, unless the turn of the task
\* already ended.
Interrupt(t) ==
    turn' = IF turn.live /\ turn.registered /\ turn.task = t
            THEN [turn EXCEPT !.interrupted = TRUE]
            ELSE IF t \in turn.ended THEN turn
            ELSE [turn EXCEPT !.early = @ \cup {t}]

-----------------------------------------------------------------------------
(* The environment. *)

Tick == now < MaxTime /\ now' = now + 1
        /\ UNCHANGED <<state, lease, nextExec, grant, w, turn, signals,
                       shutdown, restarts, starts>>

\* The owner approves a task that waits for approval.
Approve(t) ==
    /\ state[t] = "pending_approval"
    /\ state' = [state EXCEPT ![t] = "queued"]
    /\ UNCHANGED <<lease, now, nextExec, grant, w, turn, signals, shutdown,
                   restarts, starts>>

\* The owner revokes the grant or removes the caller from its group.
Revoke(t) ==
    /\ grant[t]
    /\ grant' = [grant EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<state, lease, now, nextExec, w, turn, signals, shutdown,
                   restarts, starts>>

\* `ProofNode::cancel` / `cancel_from`: under `dispatch_serial`, then
\* `canceller.cancel` after the lock. The store call fails on a task that
\* cannot become cancelled, and then no signal follows.
CancelHeld == w.phase \in {"confirm", "confirm_check"}

CancelStore(t) ==
    /\ ~CancelHeld
    /\ \/ state[t] = "cancelled" /\ UNCHANGED <<state, lease>>
       \/ /\ Allows(state[t], "cancelled")
          /\ Apply(Set(t, "cancelled", NoLease))
    /\ signals' = signals \cup {t}
    /\ UNCHANGED <<now, nextExec, grant, w, turn, shutdown, restarts, starts>>

\* `canceller.cancel(task_id)`, made after a cancel or a recovery.
Signal(t) ==
    /\ t \in signals
    /\ signals' = signals \ {t}
    /\ Interrupt(t)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, shutdown, restarts,
                   starts>>

\* `maintain` -> `recover_expired_leases(now)`, then `canceller.cancel`
\* for the active task if recovery moved it to unknown_result.
RecoverAll ==
    LET expired == {t \in Tasks : lease[t] # NoLease /\ lease[t].exp <= now}
        toUnknown == {t \in expired : state[t] = "running"}
    IN /\ expired # {}
       /\ state' = [t \in Tasks |->
                      IF t \in expired /\ state[t] = "leased" THEN "queued"
                      ELSE IF t \in toUnknown THEN "unknown_result"
                      ELSE state[t]]
       /\ lease' = [t \in Tasks |-> IF t \in expired THEN NoLease ELSE lease[t]]
       /\ signals' = signals \cup
                       {t \in toUnknown : w.phase # "idle" /\ w.task = t}

Recover ==
    /\ RecoverAll
    /\ UNCHANGED <<now, nextExec, grant, w, turn, shutdown, restarts, starts>>

\* `ProofNode::shutdown`: set the flag. Its cancel of the active task is
\* one more `CancelStore`, which the model allows at any time.
Shutdown ==
    /\ ~shutdown
    /\ shutdown' = TRUE
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, turn, signals,
                   restarts, starts>>

\* The process stops (an exit after shutdown, or a crash at any time) and
\* starts again. The agent child process stops with it. On start,
\* `ProofNode::with_scopes` runs `recover_expired_leases(now)`.
Restart ==
    /\ restarts < MaxRestarts
    /\ restarts' = restarts + 1
    /\ w' = IdleWorker
    /\ turn' = NoTurn
    /\ shutdown' = FALSE
    /\ LET expired == {t \in Tasks : lease[t] # NoLease /\ lease[t].exp <= now}
       IN /\ state' = [t \in Tasks |->
                         IF t \in expired /\ state[t] = "leased" THEN "queued"
                         ELSE IF t \in expired /\ state[t] = "running"
                              THEN "unknown_result"
                         ELSE state[t]]
          /\ lease' = [t \in Tasks |-> IF t \in expired THEN NoLease ELSE lease[t]]
    /\ signals' = {}
    /\ UNCHANGED <<now, nextExec, grant, starts>>

-----------------------------------------------------------------------------
(* The execution thread. *)

\* `dispatch_one` -> `prepare_execution` -> `lease_next`.
Lease(t) ==
    /\ w.phase = "idle"
    /\ ~shutdown
    /\ nextExec <= MaxExec
    /\ state[t] = "queued"
    /\ Apply(Set(t, "leased", [exec |-> nextExec, exp |-> now + TTL]))
    /\ w' = [IdleWorker EXCEPT !.phase = "leased", !.task = t, !.exec = nextExec]
    /\ nextExec' = nextExec + 1
    /\ UNCHANGED <<now, grant, turn, signals, shutdown, restarts, starts>>

\* `prepare_leased`.
Prepare ==
    /\ w.phase = "leased"
    /\ LET t == w.task IN
       CASE state[t] = "cancelled" ->
              Apply(Requeue(t)) /\ w' = IdleWorker
         [] state[t] # "leased" ->
              UNCHANGED <<state, lease>> /\ w' = IdleWorker
         [] ~grant[t] ->
              Apply(MarkRejected(t, w.exec)) /\ w' = IdleWorker
         [] OTHER ->
              UNCHANGED <<state, lease>> /\ w' = [w EXCEPT !.phase = "prepared"]
    /\ UNCHANGED <<now, nextExec, grant, turn, signals, shutdown, restarts, starts>>

\* `run_execution`, before the executor runs. The renewal thread starts.
RunStart ==
    /\ w.phase = "prepared"
    /\ IF shutdown
       THEN Apply(Requeue(w.task)) /\ w' = IdleWorker
       ELSE UNCHANGED <<state, lease>> /\ w' = [w EXCEPT !.phase = "confirm"]
    /\ UNCHANGED <<now, nextExec, grant, turn, signals, shutdown, restarts, starts>>

\* `on_confirmed_start`, part 1, under `dispatch_serial`: the shutdown
\* check, the policy recheck, and `mark_running`.
ConfirmRunning ==
    /\ w.phase = "confirm"
    /\ LET t == w.task IN
       CASE shutdown ->
              \* `requeue_leased`, and `settled_at_start`.
              Apply(Requeue(t))
              /\ w' = [w EXCEPT !.phase = "finish", !.outcome = "cancelled"]
         [] ~grant[t] ->
              Apply(MarkRejected(t, w.exec))
              /\ w' = [w EXCEPT !.phase = "finish", !.outcome = "err"]
         [] LeaseIs(t, w.exec) /\ state[t] = "leased" ->
              Apply(Set(t, "running", lease[t]))
              /\ w' = [w EXCEPT !.phase = "confirm_check"]
         [] OTHER ->
              UNCHANGED <<state, lease>>
              /\ w' = [w EXCEPT !.phase = "finish", !.outcome = "err"]
    /\ UNCHANGED <<now, nextExec, grant, turn, signals, shutdown, restarts, starts>>

\* `on_confirmed_start`, part 2: read the state again. On success the
\* executor calls `start_turn`, and the agent turn begins.
ConfirmCheck ==
    /\ w.phase = "confirm_check"
    /\ LET t == w.task IN
       CASE state[t] = "cancelled" ->
              w' = [w EXCEPT !.phase = "finish", !.outcome = "cancelled"]
              /\ UNCHANGED <<turn, starts>>
         [] state[t] \in {"leased", "running"} ->
              /\ w' = [w EXCEPT !.phase = "starting", !.confirmed = TRUE]
              /\ turn' = [turn EXCEPT !.live = TRUE, !.registered = FALSE,
                                       !.interrupted = FALSE, !.task = t]
              /\ starts' = [starts EXCEPT ![t] = @ + 1]
         [] OTHER ->
              w' = [w EXCEPT !.phase = "finish", !.outcome = "err"]
              /\ UNCHANGED <<turn, starts>>
    /\ UNCHANGED <<state, lease, now, nextExec, grant, signals, shutdown, restarts>>

\* The adapter registers the turn (Codex `turns.active` after `turn/start`;
\* the connected agent's `pending` map). `EarlyCancels::take`: a cancel
\* that came first ends the turn here as cancelled. Otherwise
\* `Agent::cancel` interrupts the turn from now on.
Register ==
    /\ w.phase = "starting"
    /\ LET t == w.task IN
       IF t \in turn.early
       THEN /\ w' = [w EXCEPT !.phase = "finish", !.outcome = "cancelled"]
            /\ turn' = TurnOver(turn.early \ {t}, turn.ended \cup {t},
                                turn.spurious \/ (state[t] = "running"
                                                  /\ ~w.renewFailed /\ ~shutdown))
       ELSE /\ w' = [w EXCEPT !.phase = "turn"]
            /\ turn' = [turn EXCEPT !.registered = TRUE]
    /\ UNCHANGED <<state, lease, now, nextExec, grant, signals, shutdown,
                   restarts, starts>>

\* The turn fails before the adapter registers it.
StartFails ==
    /\ w.phase = "starting"
    /\ w' = [w EXCEPT !.phase = "finish", !.outcome = "err"]
    /\ turn' = TurnOver(turn.early, turn.ended \cup {w.task}, turn.spurious)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, signals, shutdown,
                   restarts, starts>>

\* The turn ends. An interrupted turn ends as cancelled.
TurnEnds ==
    /\ w.phase = "turn"
    /\ \E o \in IF turn.interrupted THEN {"cancelled"} ELSE {"ok", "err"} :
         w' = [w EXCEPT !.phase = "finish", !.outcome = o]
    /\ turn' = TurnOver(turn.early, turn.ended \cup {w.task}, turn.spurious)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, signals, shutdown,
                   restarts, starts>>

\* The renewal thread of `run_execution` -> `renew_lease`. On failure it
\* calls `canceller.cancel` once and stops. A cancel deletes the lease, so
\* it also makes the renewal fail: this is the cancel's second signal.
Renew ==
    /\ w.phase \in {"confirm", "confirm_check", "starting", "turn"}
    /\ ~shutdown
    /\ ~w.renewFailed
    /\ LET t == w.task IN
       IF LeaseIs(t, w.exec) /\ lease[t].exp > now
       THEN /\ lease' = [lease EXCEPT ![t].exp = now + TTL]
            /\ UNCHANGED <<w, turn>>
       ELSE /\ w' = [w EXCEPT !.renewFailed = TRUE]
            /\ Interrupt(t)
            /\ UNCHANGED lease
    /\ UNCHANGED <<state, now, nextExec, grant, signals, shutdown, restarts, starts>>

\* `ProofNode::shutdown`: while an execution is active, it calls
\* `canceller.cancel` for it again every 20 ms.
ShutdownSignal ==
    /\ shutdown
    /\ w.phase # "idle"
    /\ Interrupt(w.task)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, signals, shutdown,
                   restarts, starts>>

\* `Agent::forget`, which `run_execution` calls after the executor returns
\* and the renewal thread stops: the adapter drops a kept cancel.
Forget(t) == turn' = [turn EXCEPT !.early = @ \ {t}]

\* `run_execution` after the executor returns and the renewal thread stops.
Finish ==
    /\ w.phase = "finish"
    /\ LET t == w.task
           e == w.exec
       IN IF w.renewFailed
          THEN /\ Apply(IF w.outcome = "ok" THEN MarkEnd(t, e, "succeeded") ELSE Same)
               /\ w' = [w EXCEPT !.phase = "finish_renewal"]
               /\ Forget(t)
          ELSE /\ Apply(CASE w.outcome = "ok" -> MarkEnd(t, e, "succeeded")
                          [] w.outcome = "cancelled" -> Same
                          [] OTHER -> IF w.confirmed THEN MarkEnd(t, e, "unknown_result")
                                      ELSE MarkEnd(t, e, "failed"))
               /\ w' = IdleWorker
               /\ Forget(t)
    /\ UNCHANGED <<now, nextExec, grant, signals, shutdown, restarts, starts>>

\* `run_execution`, the renewal-failed branch after the late result.
FinishRenewal ==
    /\ w.phase = "finish_renewal"
    /\ LET t == w.task IN
       Apply(IF state[t] = "cancelled" THEN Same
             ELSE IF w.confirmed THEN MarkEnd(t, w.exec, "unknown_result")
             ELSE MarkEnd(t, w.exec, "failed"))
    /\ w' = IdleWorker
    /\ UNCHANGED <<now, nextExec, grant, turn, signals, shutdown, restarts, starts>>

Next ==
    \/ Tick \/ Recover \/ Shutdown \/ Restart
    \/ \E t \in Tasks : Approve(t) \/ Revoke(t) \/ CancelStore(t) \/ Signal(t)
                        \/ Lease(t)
    \/ Prepare \/ RunStart \/ ConfirmRunning \/ ConfirmCheck \/ Register
    \/ StartFails \/ TurnEnds \/ Renew \/ ShutdownSignal \/ Finish
    \/ FinishRenewal

Spec == Init /\ [][Next]_vars

\* The renewal thread and the shutdown loop run while the execution runs,
\* and each `canceller.cancel` that follows a store call is made. The turn
\* itself gets no fairness: an agent may run until `max_seconds`.
FairSpec == Spec /\ WF_vars(Renew) /\ WF_vars(ShutdownSignal)
                 /\ \A t \in Tasks : WF_vars(Signal(t))

-----------------------------------------------------------------------------
(* Properties. *)

\* A lease row exists exactly while the task is leased or running.
LeaseIffActive ==
    \A t \in Tasks : (lease[t] # NoLease) <=> (state[t] \in {"leased", "running"})

\* An agent turn starts at most once for a task: no lease expiry, cancel,
\* restart, or requeue makes the node run a task again.
AtMostOneStart == \A t \in Tasks : starts[t] <= 1

\* While a turn is live, its task is running, or it ended in a state that
\* a person must look at (cancelled, unknown_result). It never shows as
\* queued, leased, or a result that the turn did not give.
TurnMatchesState ==
    turn.live => state[turn.task] \in {"running", "cancelled", "unknown_result"}

\* Every cancel reaches the turn: a registered turn whose task is cancelled
\* or unknown_result is interrupted, or it ends.
\*
\* An `Agent::cancel` can come before the adapter registers the turn
\* (Codex inserts it into `turns.active` only after `turn/start` answers;
\* the connected agent inserts into `pending` in `run_turn`). The adapter
\* keeps it, and the turn ends as cancelled when it registers. No signal
\* is repeated: the cancel itself and one renewal failure are all.
CancelReachesTurn ==
    [](turn.live /\ turn.registered
         /\ state[turn.task] \in {"cancelled", "unknown_result"}
       => <>(turn.interrupted \/ ~turn.live))

\* The adapter cancels a turn at its start only for a cancel, a lost lease
\* or a shutdown of this execution. Before `run_execution` called
\* `Agent::forget`, this failed: a renewal that failed before
\* `on_confirmed_start` left a kept cancel, recovery gave the task back to
\* the queue, and the kept cancel ended the turn of the next execution.
\* `settlement` writes nothing for `ExecutionError::Cancelled`, so the task
\* stayed `running` with no turn and ended as `unknown_result`.
NoSpuriousCancel == ~turn.spurious

=============================================================================
