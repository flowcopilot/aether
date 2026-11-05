------------------------------- MODULE Outbox -------------------------------
(***************************************************************************)
(* The outbox: the tasks that a node sends to other agents.                *)
(*                                                                         *)
(* Models crates/aether/src/outbox.rs (`run_once`, `start_round`,          *)
(* `attempt_in`, `follow`, `is_replay`, `outcome`), the `sent_tasks`       *)
(* functions of crates/aether-store/src/lib.rs (`claim_due_sent_tasks`,    *)
(* `claim_sent_task`, `record_sent_attempt`, `expire_sent_tasks`,          *)
(* `end_sent_tasks_for_endpoint`), `Link::exchange` in                     *)
(* crates/aether/src/transport.rs, and the host side: `NodeService::handle`*)
(* in crates/aether/src/node_service.rs and `admit_with_replay` / `admit`  *)
(* in crates/aether-store/src/lib.rs.                                      *)
(*                                                                         *)
(* All tasks go to one endpoint (one contact). Procs are node processes    *)
(* that share one store: `aether node serve` and `aether mcp serve`        *)
(* (`attempt_without_node`). A process can crash; its lease stays until    *)
(* it expires.                                                             *)
(*                                                                         *)
(* Abstracted away: wall-clock time (a lease is live or expired, a         *)
(* deadline passed or not), backoff and presence (`next_delay`,            *)
(* `wake_sent_tasks`, `defer_sent_tasks`), purge, event sequences, and the *)
(* answer text. The host runs a task to success or failure in one step.    *)
(* A `lease_until` value is a claim number: each claim sets a larger value *)
(* than the expired one that it replaces.                                  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Procs,          \* node processes that share one store
    Tasks,          \* sent tasks, all for one endpoint
    MaxNonce,       \* bound on request frames, so the state space is finite
    MaxClaims,      \* bound on claims of one task, for the same reason
    SlowAttempts    \* TRUE: an attempt can outlive its LEASE (see below)

Active   == {"waiting", "working"}
Terminal == {"answered", "failed", "not_delivered"}
SentStates == Active \cup Terminal

\* What one exchange tells the sender. "unreachable" is a lost or late
\* answer (`outcome` with `Err(_)`).
Outcomes == {"working", "completed", "failed", "busy", "denied", "replay",
             "unreachable"}

\* `outcome`: the sent state for an answer of the host.
StateOf(o) ==
    CASE o = "working"     -> "working"
      [] o = "completed"   -> "answered"
      [] o = "busy"        -> "waiting"
      [] o = "unreachable" -> "waiting"
      [] OTHER             -> "failed"   \* failed, denied, replay rejected

Phases == {"idle", "claimed", "sent", "replied"}

\* The state of the `Link` of one `follow`: no connection, a kept
\* connection, or `one_stream_each` after a kept connection broke.
LinkStates == {"none", "kept", "each"}

VARIABLES
    sent,       \* sent_tasks row: [state, until, live, answer]
    claims,     \* claims[t]: how many claims t had (a history variable)
    late,       \* late[t]: expires_at <= now
    contact,    \* the caller still keeps the contact (its bundle)
    granted,    \* the host still has a grant for the caller
    up,         \* up[p]: process p runs
    att,        \* att[p][t]: the attempt of p for t
    net,        \* frames in flight
    hostTasks,  \* rows of the host's `tasks` table: [t, s]
    seen,       \* the host's replay_entries (request nonces)
    nonce       \* the last nonce used

vars == <<sent, claims, late, contact, granted, up, att, net, hostTasks,
          seen, nonce>>

NoAttempt == [phase |-> "idle", n |-> 0, o |-> "unreachable", link |-> "none",
              canRetry |-> FALSE, replayed |-> FALSE, until |-> 0]

Req(p, t, n)    == [kind |-> "req", p |-> p, t |-> t, n |-> n]
Rep(p, t, n, o) == [kind |-> "rep", p |-> p, t |-> t, n |-> n, o |-> o]

\* Answers to p for t. Only the answer to the current frame is read, so the
\* others are removed when the attempt moves on.
RepsTo(p, t) == {m \in net : m.kind = "rep" /\ m.p = p /\ m.t = t}

HostState(t) == {r.s : r \in {x \in hostTasks : x.t = t}}

TypeOK ==
    /\ sent \in [Tasks -> [state : SentStates, until : 0..MaxClaims,
                           live : BOOLEAN, answer : {"none", "real"}]]
    /\ claims \in [Tasks -> 0..MaxClaims]
    /\ late \in [Tasks -> BOOLEAN]
    /\ contact \in BOOLEAN
    /\ granted \in BOOLEAN
    /\ up \in [Procs -> BOOLEAN]
    /\ att \in [Procs -> [Tasks -> [phase : Phases, n : 0..MaxNonce,
                                    o : Outcomes, link : LinkStates,
                                    canRetry : BOOLEAN, replayed : BOOLEAN,
                                    until : 0..MaxClaims]]]
    /\ net \subseteq ({Req(p, t, n) : p \in Procs, t \in Tasks, n \in 1..MaxNonce}
                \cup {Rep(p, t, n, o) : p \in Procs, t \in Tasks,
                                        n \in 1..MaxNonce, o \in Outcomes})
    /\ hostTasks \subseteq [t : Tasks, s : {"working", "succeeded", "failed"}]
    /\ seen \subseteq 1..MaxNonce
    /\ nonce \in 0..MaxNonce

Init ==
    \* `enqueue` / `insert_sent_task`: waiting, due at once, no lease.
    /\ sent = [t \in Tasks |-> [state |-> "waiting", until |-> 0,
                                live |-> FALSE, answer |-> "none"]]
    /\ claims = [t \in Tasks |-> 0]
    /\ late = [t \in Tasks |-> FALSE]
    /\ contact = TRUE
    /\ granted = TRUE
    /\ up = [p \in Procs |-> TRUE]
    /\ att = [p \in Procs |-> [t \in Tasks |-> NoAttempt]]
    /\ net = {}
    /\ hostTasks = {}
    /\ seen = {}
    /\ nonce = 0

-----------------------------------------------------------------------------
(* The sender's store. Each action is one SQLite transaction.              *)

\* `state IN ('waiting','working') AND (lease_until IS NULL OR
\* lease_until <= now)`.
Claimable(t) ==
    /\ sent[t].state \in Active
    /\ ~sent[t].live
    /\ claims[t] < MaxClaims

SetAtt(p, t, a) == att' = [att EXCEPT ![p][t] = a]

\* Sets a new lease on t for p.
Claim(p, t) ==
    /\ sent' = [sent EXCEPT ![t].until = claims[t] + 1, ![t].live = TRUE]
    /\ claims' = [claims EXCEPT ![t] = @ + 1]
    /\ SetAtt(p, t, [NoAttempt EXCEPT !.phase = "claimed",
                                      !.until = claims[t] + 1])

\* `claim_due_sent_tasks`: an IMMEDIATE transaction; at most one task for the
\* endpoint, and none when another task of the endpoint has a live lease.
ClaimDue(p, t) ==
    /\ up[p]
    /\ Claimable(t)
    /\ \A u \in Tasks \ {t} : ~(sent[u].state \in Active /\ sent[u].live)
    /\ att[p][t].phase = "idle"
    /\ Claim(p, t)
    /\ UNCHANGED <<late, contact, granted, up, net, hostTasks, seen, nonce>>

\* `claim_sent_task` (from `attempt_without_node`): no endpoint check.
ClaimOne(p, t) ==
    /\ up[p]
    /\ Claimable(t)
    /\ att[p][t].phase = "idle"
    /\ Claim(p, t)
    /\ UNCHANGED <<late, contact, granted, up, net, hostTasks, seen, nonce>>

\* `expire_sent_tasks` at the start of a round.
Expire(p, t) ==
    /\ up[p]
    /\ late[t]
    /\ sent[t].state \in Active
    /\ sent' = [sent EXCEPT ![t].state = "not_delivered", ![t].until = 0,
                            ![t].live = FALSE]
    /\ UNCHANGED <<claims, late, contact, granted, up, att, net, hostTasks,
                   seen, nonce>>

\* `end_sent_tasks_for_endpoint`, from `start_round` or from `attempt_in`
\* without a bundle: the contact is gone.
EndForEndpoint(p) ==
    /\ up[p]
    /\ ~contact
    /\ \E t \in Tasks : sent[t].state \in Active
    /\ sent' = [t \in Tasks |->
                  IF sent[t].state \in Active
                  THEN [sent[t] EXCEPT !.state = "not_delivered", !.until = 0,
                                       !.live = FALSE]
                  ELSE sent[t]]
    /\ att' = [att EXCEPT ![p] = [t \in Tasks |->
                  IF att[p][t].phase = "claimed" THEN NoAttempt ELSE att[p][t]]]
    /\ UNCHANGED <<claims, late, contact, granted, up, net, hostTasks, seen,
                   nonce>>

\* `follow` ends: the last answer is not "working" (or the window closed),
\* and it is not a first replay, which `follow` asks again for.
FollowEnds(p, t) ==
    /\ att[p][t].phase = "replied"
    /\ ~(att[p][t].o = "replay" /\ ~att[p][t].replayed)

\* `attempt_in` -> `record_sent_attempt(task_id, lease_until, ..)`: only an
\* active task that still has the lease of this attempt changes; it ends the
\* lease.
Record(p, t) ==
    /\ up[p]
    /\ FollowEnds(p, t)
    /\ IF sent[t].state \in Active /\ sent[t].until = att[p][t].until
       THEN sent' = [sent EXCEPT
                       ![t].state = StateOf(att[p][t].o),
                       ![t].until = 0,
                       ![t].live = FALSE,
                       ![t].answer = IF att[p][t].o = "completed"
                                     THEN "real" ELSE @]
       ELSE UNCHANGED sent
    /\ SetAtt(p, t, NoAttempt)
    /\ net' = net \ RepsTo(p, t)
    /\ UNCHANGED <<claims, late, contact, granted, up, hostTasks, seen, nonce>>

-----------------------------------------------------------------------------
(* One attempt: `follow` and `Link::exchange`.                             *)

\* `follow` builds `resend_frame` with a fresh nonce and exchange ID, the
\* same task ID and idempotency key: the first frame, the next poll after
\* "working", and the one new frame after a first "replay rejected"
\* (`is_replay`, `replayed`). A new `follow` has a new `Link`.
Send(p, t) ==
    /\ up[p]
    /\ contact
    /\ \/ att[p][t].phase = "claimed"
       \/ /\ att[p][t].phase = "replied"
          /\ \/ att[p][t].o = "working"
             \/ att[p][t].o = "replay" /\ ~att[p][t].replayed
    /\ nonce < MaxNonce
    /\ nonce' = nonce + 1
    /\ net' = (net \ RepsTo(p, t)) \cup {Req(p, t, nonce + 1)}
    /\ SetAtt(p, t, [att[p][t] EXCEPT
                       !.phase = "sent",
                       !.n = nonce + 1,
                       !.o = "unreachable",
                       !.canRetry = (att[p][t].link = "kept"),
                       !.replayed = @ \/ (att[p][t].phase = "replied"
                                          /\ att[p][t].o = "replay")])
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, hostTasks, seen>>

\* `Link::exchange`: the kept connection broke, so it connects again, sends
\* `request.clone()` (the same frame, the same nonce), and from then on
\* opens one connection for each stream. So it happens at most once for one
\* `Link`, and never for its first frame.
LinkRetry(p, t) ==
    /\ up[p]
    /\ att[p][t].phase = "sent"
    /\ att[p][t].canRetry
    /\ net' = net \cup {Req(p, t, att[p][t].n)}
    /\ SetAtt(p, t, [att[p][t] EXCEPT !.canRetry = FALSE, !.link = "each"])
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, hostTasks, seen,
                   nonce>>

\* The answer of the current frame arrives. A new connection that answered
\* is kept unless the link is in one-stream-each mode.
Receive(p, t) ==
    /\ up[p]
    /\ att[p][t].phase = "sent"
    /\ \E m \in RepsTo(p, t) :
          /\ m.n = att[p][t].n
          /\ net' = net \ {m}
          /\ SetAtt(p, t, [att[p][t] EXCEPT
                             !.phase = "replied", !.o = m.o, !.canRetry = FALSE,
                             !.link = IF @ = "none" THEN "kept" ELSE @])
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, hostTasks, seen,
                   nonce>>

\* The exchange fails or times out: `outcome` maps `Err(_)` to
\* Waiting/Unreachable, and the link has no kept connection.
TimeOut(p, t) ==
    /\ up[p]
    /\ att[p][t].phase = "sent"
    /\ SetAtt(p, t, [att[p][t] EXCEPT
                       !.phase = "replied", !.o = "unreachable", !.canRetry = FALSE,
                       !.link = IF @ = "each" THEN "each" ELSE "none"])
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, net, hostTasks,
                   seen, nonce>>

-----------------------------------------------------------------------------
(* The host.                                                               *)

\* `NodeService::handle` -> `ProofNode::admit` -> `admit_with_replay`. The
\* policy check comes first; the replay key `exchange_id:nonce` is checked
\* before the idempotency key; an overloaded queue rolls back the whole
\* transaction, replay entry included.
HostReceive ==
    \E m \in net :
      /\ m.kind = "req"
      /\ LET t == m.t
             existing == HostState(t) # {}
             reply(o) == net' = (net \ {m}) \cup {Rep(m.p, t, m.n, o)}
         IN \/ /\ ~granted
               /\ reply("denied")
               /\ UNCHANGED <<hostTasks, seen>>
            \/ /\ granted
               /\ m.n \in seen
               /\ reply("replay")
               /\ UNCHANGED <<hostTasks, seen>>
            \/ /\ granted
               /\ m.n \notin seen
               /\ ~existing
               /\ reply("busy")                   \* StoreError::Overloaded
               /\ UNCHANGED <<hostTasks, seen>>
            \/ /\ granted
               /\ m.n \notin seen
               /\ seen' = seen \cup {m.n}
               /\ IF existing                     \* Admission::Existing
                  THEN /\ hostTasks' = hostTasks
                       /\ \E s \in HostState(t) :
                            reply(CASE s = "working"   -> "working"
                                    [] s = "succeeded" -> "completed"
                                    [] OTHER           -> "failed")
                  ELSE /\ hostTasks' = hostTasks \cup {[t |-> t, s |-> "working"]}
                       /\ reply("working")        \* Admission::Accepted
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, att, nonce>>

\* The host's scheduler runs the task (see ExecutionLease). Dispatch
\* rechecks the grant, so a revoked caller's task fails.
HostFinish ==
    \E r \in hostTasks :
      /\ r.s = "working"
      /\ \E s \in IF granted THEN {"succeeded", "failed"} ELSE {"failed"} :
           hostTasks' = (hostTasks \ {r}) \cup {[r EXCEPT !.s = s]}
    /\ UNCHANGED <<sent, claims, late, contact, granted, up, att, net, seen,
                   nonce>>

-----------------------------------------------------------------------------
(* The environment.                                                        *)

\* A request frame is lost. A lost answer is `TimeOut`.
Drop ==
    \E m \in net :
      /\ m.kind = "req"
      /\ net' = net \ {m}
      /\ UNCHANGED <<sent, claims, late, contact, granted, up, att, hostTasks,
                     seen, nonce>>

\* A process stops; its attempts end, its leases stay in the store.
Crash(p) ==
    /\ up[p]
    /\ up' = [up EXCEPT ![p] = FALSE]
    /\ att' = [att EXCEPT ![p] = [t \in Tasks |-> NoAttempt]]
    /\ net' = net \ {m \in net : m.kind = "rep" /\ m.p = p}
    /\ UNCHANGED <<sent, claims, late, contact, granted, hostTasks, seen, nonce>>

Restart(p) ==
    /\ ~up[p]
    /\ up' = [up EXCEPT ![p] = TRUE]
    /\ UNCHANGED <<sent, claims, late, contact, granted, att, net, hostTasks,
                   seen, nonce>>

\* LEASE (90 s) passes. With SlowAttempts = FALSE an attempt (at most
\* ATTEMPT_WINDOW plus one exchange) always ends before its lease: a lease
\* expires only when no process still works on the task. With
\* SlowAttempts = TRUE a lease can expire at any time.
LeaseExpires(t) ==
    /\ sent[t].live
    /\ SlowAttempts \/ \A p \in Procs : att[p][t].phase = "idle"
    /\ sent' = [sent EXCEPT ![t].live = FALSE]
    /\ UNCHANGED <<claims, late, contact, granted, up, att, net, hostTasks,
                   seen, nonce>>

\* expires_at (MAX_WAIT or the end of the access) passes.
Deadline(t) ==
    /\ ~late[t]
    /\ late' = [late EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<sent, claims, contact, granted, up, att, net, hostTasks,
                   seen, nonce>>

ContactEnds ==
    /\ contact
    /\ contact' = FALSE
    /\ UNCHANGED <<sent, claims, late, granted, up, att, net, hostTasks, seen,
                   nonce>>

HostRevokes ==
    /\ granted
    /\ granted' = FALSE
    /\ UNCHANGED <<sent, claims, late, contact, up, att, net, hostTasks, seen,
                   nonce>>

-----------------------------------------------------------------------------

Next ==
    \/ \E p \in Procs, t \in Tasks :
          \/ ClaimDue(p, t) \/ ClaimOne(p, t) \/ Expire(p, t) \/ Record(p, t)
          \/ Send(p, t) \/ LinkRetry(p, t) \/ Receive(p, t) \/ TimeOut(p, t)
    \/ \E p \in Procs : EndForEndpoint(p) \/ Crash(p) \/ Restart(p)
    \/ \E t \in Tasks : LeaseExpires(t) \/ Deadline(t)
    \/ HostReceive \/ HostFinish \/ Drop \/ ContactEnds \/ HostRevokes

\* The deadline always comes, a stopped process starts again, and a round
\* that can expire a task eventually runs on some process.
Fairness ==
    /\ \A t \in Tasks : WF_vars(Deadline(t))
    /\ \A p \in Procs : WF_vars(Restart(p))
    /\ \A t \in Tasks : SF_vars(\E p \in Procs : Expire(p, t))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Properties.                                                             *)

\* The host keeps one task for each sent task, whatever the retries and the
\* duplicate frames: the idempotency key and the IMMEDIATE transaction.
HostOneTask == \A t \in Tasks : Cardinality({r \in hostTasks : r.t = t}) <= 1

\* An answer is recorded only from a real answer of the host.
AnswerIsReal ==
    \A t \in Tasks :
      sent[t].answer = "real" =>
        /\ sent[t].state = "answered"
        /\ "succeeded" \in HostState(t)

\* NOT IN Outbox.cfg, which sets SlowAttempts = TRUE: at most one process
\* works on one task at a time. It holds only while an attempt ends before
\* its lease (SlowAttempts = FALSE), because a claim only checks
\* `lease_until <= now`. NoStaleRecord below is what holds without that
\* timing assumption.
OneAttempt ==
    \A t \in Tasks : Cardinality({p \in Procs : att[p][t].phase # "idle"}) <= 1

\* A stale attempt never records: when `record_sent_attempt` changes a row,
\* no other process claimed the task after this attempt did. `claims` counts
\* every claim, so `att.until = claims` means "the latest claim".
NoStaleRecord ==
    [][\A p \in Procs, t \in Tasks :
         (Record(p, t) /\ sent'[t] # sent[t]) => att[p][t].until = claims[t]
      ]_vars

\* An ended task never changes again.
TerminalStable ==
    [][\A t \in Tasks : sent[t].state \in Terminal => sent'[t] = sent[t]]_vars

\* Every sent task ends: answered, failed, or not delivered.
EveryTaskEnds == \A t \in Tasks : <>(sent[t].state \in Terminal)

\* The sender records "failed" only when the host has no live task for it,
\* or the host refused it (grant revoked). Before the `is_replay` check in
\* `follow`, this failed: `Link::exchange` sends the same frame (same nonce)
\* again after a kept connection breaks, the host had admitted the first
\* copy and rejects the second as a replay, and `outcome` maps "replay
\* rejected" to Failed.
NoFalseFailure ==
    \A t \in Tasks :
      (sent[t].state = "failed" /\ granted) =>
        HostState(t) \subseteq {"failed"}

=============================================================================
