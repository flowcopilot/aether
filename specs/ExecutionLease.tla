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
(*   (`dispatch_one` with `RunningTasks::reserve`, `prepare_execution`,    *)
(*   `prepare_leased`, `run_execution` with its lease-renewal thread and   *)
(*   `on_confirmed_start`, `settlement`, `maintain`, `stop_recovered`),    *)
(*   and `ProofNode::cancel` / `ProofNode::shutdown` /                     *)
(*   `ProofNode::set_running_limit` in crates/aether/src/proof/mod.rs.     *)
(* Agent side: `Agent::start_turn_with_events`, `Agent::cancel` and        *)
(*   `Agent::capacity` in crates/aether/src/agent/{codex,connected}.rs,    *)
(*   and `EarlyCancels` in crates/aether/src/agent.rs. An adapter keeps a  *)
(*   cancel for a turn that it did not register yet, and cancels that turn *)
(*   when it registers it. It drops a cancel for a turn that ended, and    *)
(*   `Agent::forget` drops a kept cancel when the execution ends.          *)
(*                                                                         *)
(* One node process runs one scheduler. The scheduler runs up to `cap`     *)
(* executions at once, each on its own execution thread: a worker for each *)
(* execution ID (`Uuid::new_v4`, one for each lease). `cap` is the smaller *)
(* of the owner's running-task limit and the agent's capacity; either can  *)
(* change at any time, so `SetCap` sets it to any value. A lower `cap`     *)
(* stops no execution: it only keeps the scheduler from leasing another    *)
(* task until fewer than `cap` executions are active. `dispatch_serial` is *)
(* one lock for all of them: one execution at a time confirms its start    *)
(* and a cancel waits for that. The process can stop (crash or exit) and   *)
(* start again. Each store call is one atomic step, as each runs in one    *)
(* SQLite transaction on the store worker thread. The clock is a small     *)
(* counter.                                                                *)
(*                                                                         *)
(* Abstracted away: task input and output, events, retention, the order   *)
(* in which `lease_next` picks a queued task, queue limits, and the       *)
(* `event_error` path of `run_execution`, and the bound of 256 task IDs    *)
(* in each `EarlyCancels` list. A slot is taken at the lease: the reserve  *)
(* and `lease_next` are one step.                                          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Tasks,      \* task IDs
    MaxExec,    \* how many execution IDs (`Uuid::new_v4`) the model hands out
    TTL,        \* LEASE_TTL, in clock ticks
    MaxTime,    \* the clock stops here
    MaxRestarts,\* how often the node process may stop and start again
    MaxCap      \* the most executions that `cap` can allow at once

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
\* "start": the run began and waits for `dispatch_serial`. "confirm" and
\* "confirm_check" hold it.
Phases == {"idle", "leased", "prepared", "start", "confirm", "confirm_check",
           "starting", "turn", "finish", "finish_renewal"}
Outcomes == {"none", "ok", "err", "cancelled"}

VARIABLES
    state,      \* tasks.state
    lease,      \* execution_leases row of each task, or NoLease
    now,        \* the clock
    nextExec,   \* the next fresh execution ID
    grant,      \* whether policy and access groups still allow the task
    w,          \* the execution thread (`run_execution`) of each execution
    cap,        \* how many executions the scheduler may have active
    turn,       \* the agent turn of each task
    early,      \* `EarlyCancels.cancelled` of the adapter
    ended,      \* `EarlyCancels.ended` of the adapter
    spurious,   \* ghost: see NoSpuriousCancel
    signals,    \* tasks with an `Agent::cancel` call that is not made yet
    shutdown,   \* the `shutdown` flag
    restarts,   \* how often the process stopped
    starts      \* how often an agent turn started for each task

vars == <<state, lease, now, nextExec, grant, w, cap, turn, early, ended,
          spurious, signals, shutdown, restarts, starts>>

IdleWorker == [phase |-> "idle", task |-> CHOOSE t \in Tasks : TRUE,
               confirmed |-> FALSE, renewFailed |-> FALSE, outcome |-> "none"]
NoTurn == [live |-> FALSE, registered |-> FALSE, interrupted |-> FALSE]

\* The executions that hold a slot of the scheduler.
Busy == {e \in Execs : w[e].phase # "idle"}
\* The executions that hold `dispatch_serial`.
Holders == {e \in Execs : w[e].phase \in {"confirm", "confirm_check"}}
\* The execution of `t` that holds a slot, if there is one.
RunsTask(e, t) == w[e].phase # "idle" /\ w[e].task = t

TypeOK ==
    /\ state \in [Tasks -> States]
    /\ lease \in [Tasks -> [exec : 0..MaxExec, exp : 0..(MaxTime + TTL)]]
    /\ now \in 0..MaxTime
    /\ nextExec \in 1..(MaxExec + 1)
    /\ grant \in [Tasks -> BOOLEAN]
    /\ w \in [Execs -> [phase : Phases, task : Tasks, confirmed : BOOLEAN,
                         renewFailed : BOOLEAN, outcome : Outcomes]]
    /\ cap \in 1..MaxCap
    /\ turn \in [Tasks -> [live : BOOLEAN, registered : BOOLEAN,
                           interrupted : BOOLEAN]]
    /\ early \subseteq Tasks
    /\ ended \subseteq Tasks
    /\ spurious \in BOOLEAN
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
    /\ w = [e \in Execs |-> IdleWorker]
    /\ cap = 1
    /\ turn = [t \in Tasks |-> NoTurn]
    /\ early = {}
    /\ ended = {}
    /\ spurious = FALSE
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

SetW(e, r) == w' = [w EXCEPT ![e] = r]

\* `Agent::cancel`: it interrupts a turn that the adapter registered.
\* Otherwise `EarlyCancels::cancel` keeps it, unless the turn of the task
\* already ended. It touches the turn of `t` and no other.
Interrupt(t) ==
    IF turn[t].live /\ turn[t].registered
    THEN /\ turn' = [turn EXCEPT ![t].interrupted = TRUE]
         /\ UNCHANGED <<early, ended>>
    ELSE IF t \in ended
         THEN UNCHANGED <<turn, early, ended>>
         ELSE /\ early' = early \cup {t}
              /\ UNCHANGED <<turn, ended>>

-----------------------------------------------------------------------------
(* The environment. *)

Tick == now < MaxTime /\ now' = now + 1
        /\ UNCHANGED <<state, lease, nextExec, grant, w, cap, turn, early,
                       ended, spurious, signals, shutdown, restarts, starts>>

\* The owner approves a task that waits for approval.
Approve(t) ==
    /\ state[t] = "pending_approval"
    /\ state' = [state EXCEPT ![t] = "queued"]
    /\ UNCHANGED <<lease, now, nextExec, grant, w, cap, turn, early, ended,
                   spurious, signals, shutdown, restarts, starts>>

\* The owner revokes the grant or removes the caller from its group.
Revoke(t) ==
    /\ grant[t]
    /\ grant' = [grant EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<state, lease, now, nextExec, w, cap, turn, early, ended,
                   spurious, signals, shutdown, restarts, starts>>

\* The owner changes the running-task limit (`ProofNode::set_running_limit`,
\* `set_queue_limit`), or the agent's capacity changes (a connected agent
\* that attaches again). The value is read at the next dispatch. Nothing
\* that runs is touched.
SetCap(n) ==
    /\ n # cap
    /\ cap' = n
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, turn, early, ended,
                   spurious, signals, shutdown, restarts, starts>>

\* `ProofNode::cancel` / `cancel_from`: under `dispatch_serial`, then
\* `canceller.cancel` after the lock. The store call fails on a task that
\* cannot become cancelled, and then no signal follows.
CancelStore(t) ==
    /\ Holders = {}
    /\ \/ state[t] = "cancelled" /\ UNCHANGED <<state, lease>>
       \/ /\ Allows(state[t], "cancelled")
          /\ Apply(Set(t, "cancelled", NoLease))
    /\ signals' = signals \cup {t}
    /\ UNCHANGED <<now, nextExec, grant, w, cap, turn, early, ended, spurious,
                   shutdown, restarts, starts>>

\* `canceller.cancel(task_id)`, made after a cancel or a recovery.
Signal(t) ==
    /\ t \in signals
    /\ signals' = signals \ {t}
    /\ Interrupt(t)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, cap, spurious,
                   shutdown, restarts, starts>>

\* `maintain` -> `recover_expired_leases(now)`, then `canceller.cancel`
\* for each active task that recovery moved to unknown_result. Another
\* running task keeps its lease and runs on.
Recover ==
    LET expired == {t \in Tasks : lease[t] # NoLease /\ lease[t].exp <= now}
        toUnknown == {t \in expired : state[t] = "running"}
    IN /\ expired # {}
       /\ state' = [t \in Tasks |->
                      IF t \in expired /\ state[t] = "leased" THEN "queued"
                      ELSE IF t \in toUnknown THEN "unknown_result"
                      ELSE state[t]]
       /\ lease' = [t \in Tasks |-> IF t \in expired THEN NoLease ELSE lease[t]]
       /\ signals' = signals \cup {t \in toUnknown : \E e \in Execs : RunsTask(e, t)}
       /\ UNCHANGED <<now, nextExec, grant, w, cap, turn, early, ended,
                      spurious, shutdown, restarts, starts>>

\* `ProofNode::shutdown`: set the flag. Its cancel of each active task is
\* one more `CancelStore`, which the model allows at any time.
Shutdown ==
    /\ ~shutdown
    /\ shutdown' = TRUE
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, cap, turn, early,
                   ended, spurious, signals, restarts, starts>>

\* The process stops (an exit after shutdown, or a crash at any time) and
\* starts again. The agent child processes stop with it. On start,
\* `ProofNode::with_scopes` runs `recover_expired_leases(now)`. The limit
\* is read from the stored policy again; any value can come back.
Restart ==
    /\ restarts < MaxRestarts
    /\ restarts' = restarts + 1
    /\ w' = [e \in Execs |-> IdleWorker]
    /\ turn' = [t \in Tasks |-> NoTurn]
    /\ early' = {}
    /\ ended' = {}
    /\ shutdown' = FALSE
    /\ LET expired == {t \in Tasks : lease[t] # NoLease /\ lease[t].exp <= now}
       IN /\ state' = [t \in Tasks |->
                         IF t \in expired /\ state[t] = "leased" THEN "queued"
                         ELSE IF t \in expired /\ state[t] = "running"
                              THEN "unknown_result"
                         ELSE state[t]]
          /\ lease' = [t \in Tasks |-> IF t \in expired THEN NoLease ELSE lease[t]]
    /\ signals' = {}
    /\ UNCHANGED <<now, nextExec, grant, cap, spurious, starts>>

-----------------------------------------------------------------------------
(* The scheduler and the execution threads. *)

\* `dispatch_one`: `RunningTasks::reserve(effective_running_limit)`, then
\* `prepare_execution` -> `lease_next_except`. A slot is free while fewer
\* than `cap` executions are active. The scheduler does not lease a task
\* that an execution of its own still holds (`RunningTasks::tasks_in_flight`).
\* Without that rule, a task whose lease ran out before its renewal thread
\* started goes back to the queue (`Recover`), is leased again, and has two
\* executions: the first one's lost lease then cancels the second one's turn
\* (see NoSpuriousCancel). One slot made that impossible; `cap` > 1 does not.
Lease(t) ==
    /\ ~shutdown
    /\ nextExec <= MaxExec
    /\ Cardinality(Busy) < cap
    /\ \A e \in Execs : ~RunsTask(e, t)
    /\ state[t] = "queued"
    /\ Apply(Set(t, "leased", [exec |-> nextExec, exp |-> now + TTL]))
    /\ SetW(nextExec, [IdleWorker EXCEPT !.phase = "leased", !.task = t])
    /\ nextExec' = nextExec + 1
    /\ UNCHANGED <<now, grant, cap, turn, early, ended, spurious, signals,
                   shutdown, restarts, starts>>

\* `prepare_leased`.
Prepare(e) ==
    /\ w[e].phase = "leased"
    /\ LET t == w[e].task IN
       CASE state[t] = "cancelled" ->
              Apply(Requeue(t)) /\ SetW(e, IdleWorker)
         [] state[t] # "leased" ->
              UNCHANGED <<state, lease>> /\ SetW(e, IdleWorker)
         [] ~grant[t] ->
              Apply(MarkRejected(t, e)) /\ SetW(e, IdleWorker)
         [] OTHER ->
              UNCHANGED <<state, lease>> /\ SetW(e, [w[e] EXCEPT !.phase = "prepared"])
    /\ UNCHANGED <<now, nextExec, grant, cap, turn, early, ended, spurious,
                   signals, shutdown, restarts, starts>>

\* `run_execution`, before the executor runs. The renewal thread starts.
RunStart(e) ==
    /\ w[e].phase = "prepared"
    /\ IF shutdown
       THEN Apply(Requeue(w[e].task)) /\ SetW(e, IdleWorker)
       ELSE UNCHANGED <<state, lease>> /\ SetW(e, [w[e] EXCEPT !.phase = "start"])
    /\ UNCHANGED <<now, nextExec, grant, cap, turn, early, ended, spurious,
                   signals, shutdown, restarts, starts>>

\* `on_confirmed_start` takes `dispatch_serial`. One execution at a time
\* holds it, so one start is confirmed at a time.
Acquire(e) ==
    /\ w[e].phase = "start"
    /\ Holders = {}
    /\ SetW(e, [w[e] EXCEPT !.phase = "confirm"])
    /\ UNCHANGED <<state, lease, now, nextExec, grant, cap, turn, early, ended,
                   spurious, signals, shutdown, restarts, starts>>

\* `on_confirmed_start`, part 1, under `dispatch_serial`: the shutdown
\* check, the policy recheck, and `mark_running`.
ConfirmRunning(e) ==
    /\ w[e].phase = "confirm"
    /\ LET t == w[e].task IN
       CASE shutdown ->
              \* `requeue_leased`, and `settled_at_start`.
              Apply(Requeue(t))
              /\ SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "cancelled"])
         [] ~grant[t] ->
              Apply(MarkRejected(t, e))
              /\ SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "err"])
         [] LeaseIs(t, e) /\ state[t] = "leased" ->
              Apply(Set(t, "running", lease[t]))
              /\ SetW(e, [w[e] EXCEPT !.phase = "confirm_check"])
         [] OTHER ->
              UNCHANGED <<state, lease>>
              /\ SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "err"])
    /\ UNCHANGED <<now, nextExec, grant, cap, turn, early, ended, spurious,
                   signals, shutdown, restarts, starts>>

\* `on_confirmed_start`, part 2: read the state again. On success the
\* executor calls `start_turn`, and the agent turn begins.
ConfirmCheck(e) ==
    /\ w[e].phase = "confirm_check"
    /\ LET t == w[e].task IN
       CASE state[t] = "cancelled" ->
              SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "cancelled"])
              /\ UNCHANGED <<turn, starts>>
         [] state[t] \in {"leased", "running"} ->
              /\ SetW(e, [w[e] EXCEPT !.phase = "starting", !.confirmed = TRUE])
              /\ turn' = [turn EXCEPT ![t] = [live |-> TRUE, registered |-> FALSE,
                                              interrupted |-> FALSE]]
              /\ starts' = [starts EXCEPT ![t] = @ + 1]
         [] OTHER ->
              SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "err"])
              /\ UNCHANGED <<turn, starts>>
    /\ UNCHANGED <<state, lease, now, nextExec, grant, cap, early, ended,
                   spurious, signals, shutdown, restarts>>

\* The adapter registers the turn (Codex `turns.active` after `turn/start`;
\* the connected agent's `pending` map). `EarlyCancels::take`: a cancel
\* that came first ends the turn here as cancelled. Otherwise
\* `Agent::cancel` interrupts the turn from now on.
Register(e) ==
    /\ w[e].phase = "starting"
    /\ LET t == w[e].task IN
       IF t \in early
       THEN /\ SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "cancelled"])
            /\ turn' = [turn EXCEPT ![t] = NoTurn]
            /\ early' = early \ {t}
            /\ ended' = ended \cup {t}
            /\ spurious' = (spurious \/ (state[t] = "running"
                                         /\ ~w[e].renewFailed /\ ~shutdown))
       ELSE /\ SetW(e, [w[e] EXCEPT !.phase = "turn"])
            /\ turn' = [turn EXCEPT ![t].registered = TRUE]
            /\ UNCHANGED <<early, ended, spurious>>
    /\ UNCHANGED <<state, lease, now, nextExec, grant, cap, signals, shutdown,
                   restarts, starts>>

\* The turn fails before the adapter registers it.
StartFails(e) ==
    /\ w[e].phase = "starting"
    /\ SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = "err"])
    /\ turn' = [turn EXCEPT ![w[e].task] = NoTurn]
    /\ ended' = ended \cup {w[e].task}
    /\ UNCHANGED <<state, lease, now, nextExec, grant, cap, early, spurious,
                   signals, shutdown, restarts, starts>>

\* The turn ends. An interrupted turn ends as cancelled.
TurnEnds(e) ==
    /\ w[e].phase = "turn"
    /\ LET t == w[e].task IN
       /\ \E o \in IF turn[t].interrupted THEN {"cancelled"} ELSE {"ok", "err"} :
            SetW(e, [w[e] EXCEPT !.phase = "finish", !.outcome = o])
       /\ turn' = [turn EXCEPT ![t] = NoTurn]
       /\ early' = early \ {t}
       /\ ended' = ended \cup {t}
    /\ UNCHANGED <<state, lease, now, nextExec, grant, cap, spurious, signals,
                   shutdown, restarts, starts>>

\* The renewal thread of `run_execution` -> `renew_lease`. On failure it
\* calls `canceller.cancel` once and stops. A cancel deletes the lease, so
\* it also makes the renewal fail: this is the cancel's second signal.
Renew(e) ==
    /\ w[e].phase \in {"start", "confirm", "confirm_check", "starting", "turn"}
    /\ ~shutdown
    /\ ~w[e].renewFailed
    /\ LET t == w[e].task IN
       IF LeaseIs(t, e) /\ lease[t].exp > now
       THEN /\ lease' = [lease EXCEPT ![t].exp = now + TTL]
            /\ UNCHANGED <<w, turn, early, ended>>
       ELSE /\ SetW(e, [w[e] EXCEPT !.renewFailed = TRUE])
            /\ Interrupt(t)
            /\ UNCHANGED lease
    /\ UNCHANGED <<state, now, nextExec, grant, cap, spurious, signals, shutdown,
                   restarts, starts>>

\* `ProofNode::shutdown`: while an execution is active, it calls
\* `canceller.cancel` for each active task again every 20 ms.
ShutdownSignal(t) ==
    /\ shutdown
    /\ \E e \in Execs : RunsTask(e, t)
    /\ Interrupt(t)
    /\ UNCHANGED <<state, lease, now, nextExec, grant, w, cap, spurious, signals,
                   shutdown, restarts, starts>>

\* `Agent::forget`, which `run_execution` calls after the executor returns
\* and the renewal thread stops: the adapter drops a kept cancel.
Forget(t) == early' = early \ {t}

\* `run_execution` after the executor returns and the renewal thread stops.
Finish(e) ==
    /\ w[e].phase = "finish"
    /\ LET t == w[e].task
       IN IF w[e].renewFailed
          THEN /\ Apply(IF w[e].outcome = "ok" THEN MarkEnd(t, e, "succeeded") ELSE Same)
               /\ SetW(e, [w[e] EXCEPT !.phase = "finish_renewal"])
               /\ Forget(t)
          ELSE /\ Apply(CASE w[e].outcome = "ok" -> MarkEnd(t, e, "succeeded")
                          [] w[e].outcome = "cancelled" -> Same
                          [] OTHER -> IF w[e].confirmed THEN MarkEnd(t, e, "unknown_result")
                                      ELSE MarkEnd(t, e, "failed"))
               /\ SetW(e, IdleWorker)
               /\ Forget(t)
    /\ UNCHANGED <<now, nextExec, grant, cap, turn, ended, spurious, signals,
                   shutdown, restarts, starts>>

\* `run_execution`, the renewal-failed branch after the late result.
FinishRenewal(e) ==
    /\ w[e].phase = "finish_renewal"
    /\ LET t == w[e].task IN
       Apply(IF state[t] = "cancelled" THEN Same
             ELSE IF w[e].confirmed THEN MarkEnd(t, e, "unknown_result")
             ELSE MarkEnd(t, e, "failed"))
    /\ SetW(e, IdleWorker)
    /\ UNCHANGED <<now, nextExec, grant, cap, turn, early, ended, spurious,
                   signals, shutdown, restarts, starts>>

Next ==
    \/ Tick \/ Recover \/ Shutdown \/ Restart
    \/ \E n \in 1..MaxCap : SetCap(n)
    \/ \E t \in Tasks : Approve(t) \/ Revoke(t) \/ CancelStore(t) \/ Signal(t)
                        \/ Lease(t) \/ ShutdownSignal(t)
    \/ \E e \in Execs : Prepare(e) \/ RunStart(e) \/ Acquire(e)
                        \/ ConfirmRunning(e) \/ ConfirmCheck(e) \/ Register(e)
                        \/ StartFails(e) \/ TurnEnds(e) \/ Renew(e) \/ Finish(e)
                        \/ FinishRenewal(e)

Spec == Init /\ [][Next]_vars

\* Each renewal thread and the shutdown loop run while their execution runs,
\* and each `canceller.cancel` that follows a store call is made. The turn
\* itself gets no fairness: an agent may run until `max_seconds`.
FairSpec == Spec /\ (\A e \in Execs : WF_vars(Renew(e)))
                 /\ (\A t \in Tasks : WF_vars(ShutdownSignal(t)))
                 /\ (\A t \in Tasks : WF_vars(Signal(t)))

-----------------------------------------------------------------------------
(* Properties. *)

\* A lease row exists exactly while the task is leased or running.
LeaseIffActive ==
    \A t \in Tasks : (lease[t] # NoLease) <=> (state[t] \in {"leased", "running"})

\* An agent turn starts at most once for a task: no lease expiry, cancel,
\* restart, requeue, or second execution makes the node run a task again.
AtMostOneStart == \A t \in Tasks : starts[t] <= 1

\* While a turn is live, its task is running, or it ended in a state that
\* a person must look at (cancelled, unknown_result). It never shows as
\* queued, leased, or a result that the turn did not give.
TurnMatchesState ==
    \A t \in Tasks :
        turn[t].live => state[t] \in {"running", "cancelled", "unknown_result"}

\* Every cancel reaches the turn: a registered turn whose task is cancelled
\* or unknown_result is interrupted, or it ends. This holds for each task
\* of several that run at once.
\*
\* An `Agent::cancel` can come before the adapter registers the turn
\* (Codex inserts it into `turns.active` only after `turn/start` answers;
\* the connected agent inserts into `pending` in `run_turn`). The adapter
\* keeps it, and the turn ends as cancelled when it registers. No signal
\* is repeated: the cancel itself and one renewal failure are all.
CancelReachesTurn ==
    \A t \in Tasks :
        [](turn[t].live /\ turn[t].registered
             /\ state[t] \in {"cancelled", "unknown_result"}
           => <>(turn[t].interrupted \/ ~turn[t].live))

\* The adapter cancels a turn at its start only for a cancel, a lost lease
\* or a shutdown of this execution. Before `run_execution` called
\* `Agent::forget`, this failed: a renewal that failed before
\* `on_confirmed_start` left a kept cancel, recovery gave the task back to
\* the queue, and the kept cancel ended the turn of the next execution.
\* `settlement` writes nothing for `ExecutionError::Cancelled`, so the task
\* stayed `running` with no turn and ended as `unknown_result`.
NoSpuriousCancel == ~spurious

\* One execution at a time holds `dispatch_serial`, so no two starts are
\* confirmed together, and a cancel (which takes the same lock) never lands
\* between the policy recheck and `mark_running` of any of them.
OneStartAtATime == Cardinality(Holders) <= 1

\* The scheduler never has more executions than the model hands out, and
\* never takes a slot beyond `cap` at the time it takes it. A lower `cap`
\* does not shrink the number of executions: the count only falls when an
\* execution ends.
RunningWithinCap ==
    [][Cardinality(Busy') > Cardinality(Busy) => Cardinality(Busy') <= cap]_vars

\* A change of the limit changes no state, lease, worker or turn: it never
\* stops a task that runs.
CapChangeTouchesNothing ==
    [][cap' # cap => (state' = state /\ lease' = lease /\ w' = w
                      /\ turn' = turn)]_vars

=============================================================================
