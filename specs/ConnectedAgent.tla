--------------------------- MODULE ConnectedAgent ---------------------------
(***************************************************************************)
(* The agent socket: a connected agent (the Hermes and OpenClaw plugins,   *)
(* the Grok Bot bridge) attaches to the node and answers the turns that    *)
(* the node sends it.                                                      *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/agent/connected.rs   Shared::serve (attach, the     *)
(*       replace path, the read loop, the end of a connection),            *)
(*       ConnectedAgent::connection, run_turn, start_turn_with_events,     *)
(*       cancel, Turns::close                                              *)
(*   crates/aether/src/agent.rs             EarlyCancels (cancel, take,    *)
(*       end)                                                              *)
(*   protocol/agent-socket/README.md        the rules of both sides        *)
(*   integrations/openclaw-plugin/src/task-runner.ts   the honest agent    *)
(*                                                                         *)
(* A connection is a number: the `generation` that `serve` gives it. Each  *)
(* connection has its own `pending` turns (`Turns`: the open task IDs and   *)
(* the `closed` flag). A turn is one call of                               *)
(* `start_turn_with_events`; each task has one thread that can run         *)
(* MaxTurns turns, one after the other. Turns of different tasks can run   *)
(* at the same time, which is more than the scheduler does (it runs one    *)
(* execution at a time).                                                   *)
(*                                                                         *)
(* The agent is not trusted to follow the protocol. When `honest` is       *)
(* FALSE, an agent can answer any task ID that the node ever sent to an    *)
(* agent, at any time and more than once, can ignore a cancel, can never   *)
(* answer, and can close only its sending side. When `honest` is TRUE,     *)
(* the agent follows task-runner.ts. `limit` says whether the time limit   *)
(* of a turn (`ctx.deadline`) ever ends. Both are chosen in Init and do    *)
(* not change, so one TLC run checks all four cases.                       *)
(*                                                                         *)
(* Abstracted away: the socket file and its mode, the `ready` check and    *)
(* the protocol number, bad and long lines (a long line is an agent        *)
(* close), `progress` messages (they end nothing), the answer text, and    *)
(* the 256-entry bound of each EarlyCancels list. A write and its receipt  *)
(* by the agent are one step, and so are an agent message and its read by  *)
(* `serve`. `generation.fetch_add` and `connection.replace` are one step.  *)
(* `run_turn` holds the `pending` mutex from the insert until the `task`   *)
(* message is written, so the insert and the write are one step.           *)
(* A turn reads only the first final reply in its channel, so the channel  *)
(* keeps only that one. The variables of a turn are reset when it ends.    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Tasks,      \* task IDs
    MaxConns,   \* how many connections attach
    MaxTurns    \* how many turns one task ID gets (the node gives it one)

Conns == 1..MaxConns
NoConn == 0

Stages == {"unused", "drain", "shut", "live", "eof", "done"}
Pcs == {"idle", "got", "wait", "remove", "tcancel", "ending"}
Results == {"none", "answered", "failed", "cancelled", "timeout", "gone",
            "protocol"}
ReplyKinds == {"result", "failed", "cancelled"}

\* `from` is the connection whose agent sent the reply, or NoConn for a
\* `Reply::Cancelled` that the replace path made and for a dropped sender.
Reply == [kind : ReplyKinds \cup {"none", "dropped"}, from : 0..MaxConns]
NoReply == [kind |-> "none", from |-> NoConn]

VARIABLES
    honest,     \* whether the agents follow the protocol (fixed in Init)
    limit,      \* whether a turn's time limit ever ends (fixed in Init)
    cur,        \* Shared.connection: the attached connection, or NoConn
    gen,        \* Shared.generation
    st,         \* where the `serve` thread of each connection is
    repl,       \* the connection that a new connection replaces
    nodeShut,   \* the node called `shutdown` on the connection
    agentEnd,   \* "open", "half" (the agent closed its sending side), "closed"
    pending,    \* Connection.pending: the open task IDs (Turns.open)
    closed,     \* Turns.closed: the connection was replaced or ended
    drain,      \* the turns that a new connection took from the replaced one
    open,       \* honest agent: its open tasks (TaskRunner #open)
    cpend,      \* honest agent: `cancelled` messages on their way to the node
    known,      \* task IDs that the node ever wrote to an agent
    lock,       \* who holds the `cancels` mutex across two steps
    cancelled,  \* EarlyCancels.cancelled
    ended,      \* EarlyCancels.ended
    pc,         \* where the turn thread of each task is
    tc,         \* the connection that the turn took in `connection()`
    chan,       \* the first final reply in the turn's channel
    res,        \* the result of the turn, until it ends
    turns,      \* how many turns the task started
    ends,       \* ghost: how many turns of the task ended
    told,       \* ghost: the agent got a `cancel` after this turn's `task`
    sent        \* ghost: the node wrote the `task` message of this turn

vars == <<honest, limit, cur, gen, st, repl, nodeShut, agentEnd, pending,
          closed, drain, open, cpend, known, lock, cancelled, ended, pc, tc,
          chan, res, turns, ends, told, sent>>

connVars == <<cur, gen, st, repl, nodeShut, agentEnd>>
agentVars == <<open, cpend>>
turnVars == <<pc, tc, res, turns, ends>>
fixed == <<honest, limit>>
ghosts == <<told, sent>>

Free == "free"

TypeOK ==
    /\ honest \in BOOLEAN
    /\ limit \in BOOLEAN
    /\ cur \in 0..MaxConns
    /\ gen \in 0..MaxConns
    /\ st \in [Conns -> Stages]
    /\ repl \in [Conns -> 0..MaxConns]
    /\ nodeShut \in [Conns -> BOOLEAN]
    /\ agentEnd \in [Conns -> {"open", "half", "closed"}]
    /\ pending \in [Conns -> SUBSET Tasks]
    /\ closed \in [Conns -> BOOLEAN]
    /\ drain \in [Conns -> SUBSET Tasks]
    /\ open \in [Conns -> SUBSET Tasks]
    /\ cpend \in [Conns -> SUBSET Tasks]
    /\ known \subseteq Tasks
    /\ lock \in Tasks \cup {Free}
    /\ cancelled \subseteq Tasks
    /\ ended \subseteq Tasks
    /\ pc \in [Tasks -> Pcs]
    /\ tc \in [Tasks -> 0..MaxConns]
    /\ chan \in [Tasks -> Reply]
    /\ res \in [Tasks -> Results]
    /\ turns \in [Tasks -> 0..MaxTurns]
    /\ ends \in [Tasks -> 0..MaxTurns]
    /\ told \in [Tasks -> BOOLEAN]
    /\ sent \in [Tasks -> BOOLEAN]

Init ==
    /\ honest \in BOOLEAN
    /\ limit \in BOOLEAN
    /\ cur = NoConn
    /\ gen = 0
    /\ st = [c \in Conns |-> "unused"]
    /\ repl = [c \in Conns |-> NoConn]
    /\ nodeShut = [c \in Conns |-> FALSE]
    /\ agentEnd = [c \in Conns |-> "open"]
    /\ pending = [c \in Conns |-> {}]
    /\ closed = [c \in Conns |-> FALSE]
    /\ drain = [c \in Conns |-> {}]
    /\ open = [c \in Conns |-> {}]
    /\ cpend = [c \in Conns |-> {}]
    /\ known = {}
    /\ lock = Free
    /\ cancelled = {}
    /\ ended = {}
    /\ pc = [t \in Tasks |-> "idle"]
    /\ tc = [t \in Tasks |-> NoConn]
    /\ chan = [t \in Tasks |-> NoReply]
    /\ res = [t \in Tasks |-> "none"]
    /\ turns = [t \in Tasks |-> 0]
    /\ ends = [t \in Tasks |-> 0]
    /\ told = [t \in Tasks |-> FALSE]
    /\ sent = [t \in Tasks |-> FALSE]

-----------------------------------------------------------------------------
(* Helpers.                                                                *)

\* `write_json_line` succeeds: the node did not shut the stream down and
\* the agent still reads.
Writable(c) == ~nodeShut[c] /\ agentEnd[c] # "closed"

\* `read_message` in the read loop of `serve` returns None.
EofVisible(c) == nodeShut[c] \/ agentEnd[c] # "open"

\* The read loop of `serve`: `pending.get(&id)` and `sender.send(reply)`.
\* The node ignores an answer for a task that is not open on this
\* connection. A later reply is never read, so only the first is kept.
Deliver(c, t, kind) ==
    chan' = IF t \in pending[c] /\ chan[t].kind = "none"
            THEN [chan EXCEPT ![t] = [kind |-> kind, from |-> c]]
            ELSE chan

\* The agent of connection c gets `cancel` for the tasks in S. The honest
\* agent (TaskRunner.handle) answers `cancelled` at once for each open one
\* and ignores the others.
AgentGetsCancel(c, S) ==
    IF honest
    THEN /\ open' = [open EXCEPT ![c] = @ \ S]
         /\ cpend' = [cpend EXCEPT ![c] = @ \cup (S \cap open[c])]
    ELSE UNCHANGED agentVars

-----------------------------------------------------------------------------
(* Connections: `Shared::serve`.                                           *)

\* An agent connects and says `ready`: `serve` takes the next generation
\* and puts the connection in `Shared.connection`. Under the `connection`
\* mutex it first closes the turns of the connection that it replaces
\* (`Turns::close`): each open turn gets `Reply::Cancelled`, and no turn
\* starts on that connection after this.
Attach ==
    /\ gen < MaxConns
    /\ LET c == gen + 1 IN
       /\ gen' = c
       /\ cur' = c
       /\ repl' = [repl EXCEPT ![c] = cur]
       /\ st' = [st EXCEPT ![c] = IF cur = NoConn THEN "live" ELSE "drain"]
       /\ IF cur = NoConn
          THEN UNCHANGED <<pending, closed, drain, chan>>
          ELSE /\ closed' = [closed EXCEPT ![cur] = TRUE]
               /\ drain' = [drain EXCEPT ![c] = pending[cur]]
               /\ pending' = [pending EXCEPT ![cur] = {}]
               /\ chan' = [t \in Tasks |->
                             IF t \in pending[cur] /\ chan[t].kind = "none"
                             THEN [kind |-> "cancelled", from |-> NoConn]
                             ELSE chan[t]]
    /\ UNCHANGED <<fixed, nodeShut, agentEnd, agentVars, known, lock,
                   cancelled, ended, turnVars, ghosts>>

\* `serve`, the replace path, part 1: write `cancel` to the replaced
\* connection for each turn that was open on it.
Drain(c) ==
    /\ st[c] = "drain"
    /\ IF Writable(repl[c]) THEN AgentGetsCancel(repl[c], drain[c])
       ELSE UNCHANGED agentVars
    /\ drain' = [drain EXCEPT ![c] = {}]
    /\ st' = [st EXCEPT ![c] = "shut"]
    /\ UNCHANGED <<fixed, cur, gen, repl, nodeShut, agentEnd, pending, closed,
                   known, lock, cancelled, ended, turnVars, chan, ghosts>>

\* `serve`, the replace path, part 2: `replaced.stream.shutdown`. The read
\* loop of the new connection starts. The honest agent of the replaced
\* connection sees the close and forgets its open tasks.
ShutOld(c) ==
    /\ st[c] = "shut"
    /\ nodeShut' = [nodeShut EXCEPT ![repl[c]] = TRUE]
    /\ open' = [open EXCEPT ![repl[c]] = {}]
    /\ repl' = [repl EXCEPT ![c] = NoConn]
    /\ st' = [st EXCEPT ![c] = "live"]
    /\ UNCHANGED <<fixed, cur, gen, agentEnd, pending, closed, drain, cpend,
                   known, lock, cancelled, ended, turnVars, chan, ghosts>>

\* The agent process stops or closes the connection. An agent that does
\* not follow the protocol can close only its sending side. The honest
\* agent forgets its open tasks (TaskRunner.close).
AgentClose(c, how) ==
    /\ st[c] # "unused"
    /\ \/ agentEnd[c] = "open" /\ how \in {"half", "closed"}
       \/ agentEnd[c] = "half" /\ how = "closed"
    /\ how = "half" => ~honest
    /\ agentEnd' = [agentEnd EXCEPT ![c] = how]
    /\ open' = [open EXCEPT ![c] = {}]
    /\ UNCHANGED <<fixed, cur, gen, st, repl, nodeShut, pending, closed, drain,
                   cpend, known, lock, cancelled, ended, turnVars, chan,
                   ghosts>>

\* `serve`: the read loop ends. No later message of the agent is read.
SeeEof(c) ==
    /\ st[c] = "live"
    /\ EofVisible(c)
    /\ st' = [st EXCEPT ![c] = "eof"]
    /\ cpend' = [cpend EXCEPT ![c] = {}]
    /\ UNCHANGED <<fixed, cur, gen, repl, nodeShut, agentEnd, pending, closed,
                   drain, open, known, lock, cancelled, ended, turnVars, chan,
                   ghosts>>

\* `serve`, after the loop, under the `connection` mutex: `Turns::close`,
\* then clear `Shared.connection` when it still holds this generation.
\* Each sender is dropped, so a turn that waits gets `Disconnected`, and
\* no turn starts on the connection after this.
Detach(c) ==
    /\ st[c] = "eof"
    /\ chan' = [t \in Tasks |->
                  IF t \in pending[c] /\ chan[t].kind = "none"
                  THEN [NoReply EXCEPT !.kind = "dropped"] ELSE chan[t]]
    /\ pending' = [pending EXCEPT ![c] = {}]
    /\ closed' = [closed EXCEPT ![c] = TRUE]
    /\ cur' = IF cur = c THEN NoConn ELSE cur
    /\ st' = [st EXCEPT ![c] = "done"]
    /\ UNCHANGED <<fixed, gen, repl, nodeShut, agentEnd, drain, agentVars,
                   known, lock, cancelled, ended, turnVars, ghosts>>

-----------------------------------------------------------------------------
(* The agent.                                                              *)

\* The honest agent ends an open task with `result` or `failed`, once.
\* An agent that does not follow the protocol sends any final answer for
\* any task ID that it knows, at any time. `serve` reads the message.
AgentSays(c, t, kind) ==
    /\ st[c] = "live"
    /\ agentEnd[c] = "open"
    /\ ~nodeShut[c]
    /\ IF honest
       THEN /\ t \in open[c]
            /\ kind \in {"result", "failed"}
            /\ open' = [open EXCEPT ![c] = @ \ {t}]
            /\ UNCHANGED cpend
       ELSE /\ t \in known
            /\ UNCHANGED agentVars
    /\ Deliver(c, t, kind)
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, known, lock,
                   cancelled, ended, turnVars, ghosts>>

\* The `cancelled` that the honest agent sent for a cancel arrives.
CancelledArrives(c, t) ==
    /\ st[c] = "live"
    /\ t \in cpend[c]
    /\ cpend' = [cpend EXCEPT ![c] = @ \ {t}]
    /\ Deliver(c, t, "cancelled")
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, open, known, lock,
                   cancelled, ended, turnVars, ghosts>>

-----------------------------------------------------------------------------
(* One turn: `start_turn_with_events` and `run_turn`.                      *)

\* `run_turn`, under the `cancels` mutex: `cancels.take`, then
\* `connection()`. The mutex stays held until `Insert`.
Begin(t) ==
    /\ pc[t] = "idle"
    /\ turns[t] < MaxTurns
    /\ lock = Free
    /\ turns' = [turns EXCEPT ![t] = @ + 1]
    /\ CASE t \in cancelled ->
              /\ cancelled' = cancelled \ {t}
              /\ res' = [res EXCEPT ![t] = "cancelled"]
              /\ pc' = [pc EXCEPT ![t] = "ending"]
              /\ UNCHANGED <<tc, lock>>
         [] t \notin cancelled /\ cur = NoConn ->
              /\ res' = [res EXCEPT ![t] = "gone"]
              /\ pc' = [pc EXCEPT ![t] = "ending"]
              /\ UNCHANGED <<tc, lock, cancelled>>
         [] OTHER ->
              /\ tc' = [tc EXCEPT ![t] = cur]
              /\ pc' = [pc EXCEPT ![t] = "got"]
              /\ lock' = t
              /\ UNCHANGED <<res, cancelled>>
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, agentVars, known,
                   ended, ends, chan, ghosts>>

\* `run_turn`, under the `pending` mutex of the connection that the turn
\* took: a closed connection takes no turn, and a task ID that is already
\* open is a protocol error. Otherwise insert the task, release the
\* `cancels` mutex and write the `task` message. The honest agent starts a
\* turn for it, unless a task with this ID is open (TaskRunner #start).
Insert(t) ==
    /\ pc[t] = "got"
    /\ lock' = Free
    /\ LET c == tc[t] IN
       CASE closed[c] ->
              /\ res' = [res EXCEPT ![t] = "gone"]
              /\ pc' = [pc EXCEPT ![t] = "ending"]
              /\ UNCHANGED <<pending, known, open, sent>>
         [] ~closed[c] /\ t \in pending[c] ->
              /\ res' = [res EXCEPT ![t] = "protocol"]
              /\ pc' = [pc EXCEPT ![t] = "ending"]
              /\ UNCHANGED <<pending, known, open, sent>>
         [] OTHER ->
              /\ pending' = [pending EXCEPT ![c] = @ \cup {t}]
              /\ IF Writable(c)
                 THEN /\ known' = known \cup {t}
                      /\ open' = IF honest /\ agentEnd[c] = "open"
                                 THEN [open EXCEPT ![c] = @ \cup {t}] ELSE open
                      /\ sent' = [sent EXCEPT ![t] = TRUE]
                      /\ pc' = [pc EXCEPT ![t] = "wait"]
                      /\ UNCHANGED res
                 ELSE /\ res' = [res EXCEPT ![t] = "gone"]
                      /\ pc' = [pc EXCEPT ![t] = "remove"]
                      /\ UNCHANGED <<known, open, sent>>
    /\ UNCHANGED <<fixed, connVars, closed, drain, cpend, cancelled, ended, tc,
                   chan, turns, ends, told>>

\* `run_turn`: `receiver.recv_timeout` gives a final reply, or
\* `Disconnected` after `Turns::close` in `Detach`.
Receive(t) ==
    /\ pc[t] = "wait"
    /\ chan[t].kind # "none"
    /\ res' = [res EXCEPT ![t] =
                 CASE chan[t].kind = "result" -> "answered"
                   [] chan[t].kind = "failed" -> "failed"
                   [] chan[t].kind = "cancelled" -> "cancelled"
                   [] OTHER -> "gone"]
    /\ pc' = [pc EXCEPT ![t] = "remove"]
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, agentVars, known,
                   lock, cancelled, ended, tc, chan, turns, ends, ghosts>>

\* `run_turn`: the time limit of the turn ends.
Timeout(t) ==
    /\ pc[t] = "wait"
    /\ limit
    /\ res' = [res EXCEPT ![t] = "timeout"]
    /\ pc' = [pc EXCEPT ![t] = "remove"]
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, agentVars, known,
                   lock, cancelled, ended, tc, chan, turns, ends, ghosts>>

\* `run_turn`: `pending.remove`.
Remove(t) ==
    /\ pc[t] = "remove"
    /\ pending' = [pending EXCEPT ![tc[t]] = @ \ {t}]
    /\ pc' = [pc EXCEPT ![t] = IF res[t] = "timeout" THEN "tcancel" ELSE "ending"]
    /\ UNCHANGED <<fixed, connVars, closed, drain, agentVars, known, lock,
                   cancelled, ended, tc, chan, res, turns, ends, ghosts>>

\* `run_turn`, after a timeout: write `cancel`. A failed write replaces
\* the timeout with `Unavailable`.
TimeoutCancel(t) ==
    /\ pc[t] = "tcancel"
    /\ IF Writable(tc[t])
       THEN AgentGetsCancel(tc[t], {t}) /\ UNCHANGED res
       ELSE res' = [res EXCEPT ![t] = "gone"] /\ UNCHANGED agentVars
    /\ pc' = [pc EXCEPT ![t] = "ending"]
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, known, lock,
                   cancelled, ended, tc, chan, turns, ends, ghosts>>

\* `start_turn_with_events`: `cancels.end`, which also drops a cancel that
\* was kept while the turn ran. The turn is over.
End(t) ==
    /\ pc[t] = "ending"
    /\ lock = Free
    /\ ended' = ended \cup {t}
    /\ cancelled' = cancelled \ {t}
    /\ ends' = [ends EXCEPT ![t] = @ + 1]
    /\ pc' = [pc EXCEPT ![t] = "idle"]
    /\ tc' = [tc EXCEPT ![t] = NoConn]
    /\ chan' = [chan EXCEPT ![t] = NoReply]
    /\ res' = [res EXCEPT ![t] = "none"]
    /\ told' = [told EXCEPT ![t] = FALSE]
    /\ sent' = [sent EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, agentVars, known,
                   lock, turns>>

-----------------------------------------------------------------------------
(* `Agent::cancel`. The node calls it at any time: for a task whose turn   *)
(* did not start, runs, or ended (ProofNode::cancel, the lease renewal     *)
(* thread, shutdown).                                                      *)

\* The task is open on the attached connection, so `cancel` writes it.
\* `cancel` reads this under the `pending` mutex, so after the `task`
\* message of the turn was written.
CancelWrites(t) == cur # NoConn /\ t \in pending[cur]

Cancel(t) ==
    /\ lock = Free
    /\ IF CancelWrites(t)
       THEN /\ IF Writable(cur) THEN AgentGetsCancel(cur, {t})
               ELSE UNCHANGED agentVars
            /\ told' = [told EXCEPT ![t] = @ \/ (Writable(cur) /\ pc[t] = "wait")]
            /\ UNCHANGED cancelled
       ELSE \* `EarlyCancels::cancel`: keep it, unless the turn ended.
            /\ cancelled' = IF t \in ended THEN cancelled ELSE cancelled \cup {t}
            /\ UNCHANGED <<agentVars, told>>
    /\ UNCHANGED <<fixed, connVars, pending, closed, drain, known, lock, ended,
                   turnVars, chan, sent>>

-----------------------------------------------------------------------------

Next ==
    \/ Attach
    \/ \E c \in Conns :
          \/ Drain(c) \/ ShutOld(c) \/ SeeEof(c) \/ Detach(c)
          \/ \E how \in {"half", "closed"} : AgentClose(c, how)
          \/ \E t \in Tasks :
                \/ CancelledArrives(c, t)
                \/ \E kind \in ReplyKinds : AgentSays(c, t, kind)
    \/ \E t \in Tasks :
          \/ Begin(t) \/ Insert(t) \/ Receive(t) \/ Timeout(t)
          \/ Remove(t) \/ TimeoutCancel(t) \/ End(t) \/ Cancel(t)

\* The node's threads keep running, and a message that the agent sent
\* arrives. The agent's own answer gets no fairness: a turn may take until
\* the time limit, and the limit ends only when `limit` is TRUE.
Fairness ==
    /\ \A t \in Tasks :
          /\ WF_vars(Insert(t)) /\ WF_vars(Receive(t))
          /\ WF_vars(Timeout(t)) /\ WF_vars(Remove(t))
          /\ WF_vars(TimeoutCancel(t)) /\ WF_vars(End(t))
    /\ \A c \in Conns :
          /\ WF_vars(Drain(c)) /\ WF_vars(ShutOld(c)) /\ WF_vars(SeeEof(c))
          /\ WF_vars(Detach(c))
          /\ \A t \in Tasks : WF_vars(CancelledArrives(c, t))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety.                                                                 *)

\* An open task ID belongs to a turn that runs on that connection: no
\* sender stays in a `pending` map after its turn, so the protocol error
\* of `Insert` never happens. A closed connection has no open task.
PendingIsOpenTurn ==
    \A c \in Conns, t \in Tasks :
        t \in pending[c]
           => pc[t] \in {"wait", "remove"} /\ tc[t] = c /\ ~closed[c]

NoProtocolError == \A t \in Tasks : res[t] # "protocol"

\* An answer comes only from the agent of the connection that the turn is
\* open on, whatever task IDs the other agents send.
AnswerFromHolder ==
    \A t \in Tasks :
        res[t] = "answered" => chan[t].kind = "result" /\ chan[t].from = tc[t]

\* A result enters the channel of a turn only after the node sent the
\* turn's task to the connection of that agent, also when the agent does
\* not follow the protocol and knows the task ID from an earlier turn.
AnswerAfterSent ==
    [][\A t \in Tasks :
         chan[t].kind = "none" /\ chan'[t].kind = "result" => sent[t]]_vars

\* A `task` goes only to the attached connection: `serve` closes a
\* connection for turns before it replaces it.
TaskOnlyToAttached ==
    [][\A t \in Tasks : ~sent[t] /\ sent'[t] => cur = tc[t]]_vars

\* The result of an agent that was replaced never enters the channel of a
\* turn.
AnswerWhileAttached ==
    [][\A t \in Tasks : chan[t].kind = "none" /\ chan'[t].kind = "result"
                           => chan'[t].from = cur]_vars

\* A `cancel` never reaches the agent before its `task`: when `cancel`
\* finds the task open on the attached connection, the task was written.
NoLostCancel ==
    \A t \in Tasks : CancelWrites(t) /\ Writable(cur) => sent[t]

\* Each turn ends once.
TurnEndsOnce ==
    \A t \in Tasks : ends[t] = IF pc[t] = "idle" THEN turns[t] ELSE turns[t] - 1

\* A turn that starts while no agent is attached sends nothing and fails.
NoAgentNoTask ==
    [][\A t \in Tasks :
         pc[t] = "idle" /\ pc'[t] # "idle" /\ cur = NoConn
            => pc'[t] = "ending" /\ res'[t] \in {"gone", "cancelled"}]_vars

\* A cancel that came before the turn started cancels it at the start,
\* and nothing is sent to the agent.
EarlyCancelHonored ==
    [][\A t \in Tasks :
         pc[t] = "idle" /\ pc'[t] # "idle" /\ t \in cancelled
            => pc'[t] = "ending" /\ res'[t] = "cancelled"
               /\ t \notin cancelled']_vars

\* A cancel that comes after `cancels.end` of a turn is not kept.
LateCancelDropped ==
    [][\A t \in Tasks : t \in ended /\ t \notin cancelled
                           => t \notin cancelled']_vars

\* No cancel is kept after a turn of its task ended, so a later turn with
\* the same task ID is not cancelled at its start.
NoStaleCancel ==
    \A t \in Tasks : pc[t] = "idle" /\ turns[t] > 0 => t \notin cancelled

\* A cancel for a turn that the attached agent has is written to it.
CancelToldToHolder ==
    [][\A t \in Tasks :
         Cancel(t) /\ pc[t] = "wait" /\ tc[t] = cur /\ Writable(cur)
            => told'[t]]_vars

\* A turn never waits on a connection that is over with nothing in its
\* channel, also when the agent closed only its sending side.
NoOrphanWait ==
    \A t \in Tasks :
        ~(pc[t] = "wait" /\ st[tc[t]] = "done" /\ chan[t].kind = "none")

-----------------------------------------------------------------------------
(* Liveness.                                                               *)

\* Every turn ends when its time limit ends, whatever the agent does.
TurnEnds == \A t \in Tasks : (limit /\ pc[t] # "idle") ~> pc[t] = "idle"

\* Without the time limit: when the connection of a turn is closed (the
\* node replaced it, or the agent closed it), the turn stops waiting.
LostAgentFailsTurn ==
    \A t \in Tasks, c \in Conns :
        (~limit /\ pc[t] = "wait" /\ tc[t] = c
           /\ (nodeShut[c] \/ agentEnd[c] = "closed"))
        ~> pc[t] # "wait"

\* Without the time limit: when an honest agent got a cancel after the
\* task, the turn stops waiting.
CancelStopsHonestTurn ==
    \A t \in Tasks :
        (honest /\ ~limit /\ pc[t] = "wait" /\ told[t]) ~> pc[t] # "wait"

=============================================================================
