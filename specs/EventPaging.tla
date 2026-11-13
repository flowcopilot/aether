---------------------------- MODULE EventPaging ----------------------------
(***************************************************************************)
(* How a caller reads the progress and the answer of one task: the event  *)
(* journal of the host, one page of it for each poll, and the polling     *)
(* loop of the caller.                                                     *)
(*                                                                         *)
(* Host side:                                                              *)
(*   crates/aether-store/src/tasks.rs   `event` (the next sequence is      *)
(*       MAX(sequence)+1), `change`, `append_event`, `mark_succeeded` on a *)
(*       cancelled task, `clear_expired_content`, `events_page`,           *)
(*       `task_progress`, `content_is_expired`                             *)
(*   crates/aether/src/a2a_transport.rs `A2aTransport::handle`: the page   *)
(*       loop with `MAX_FRAMES - 1` and `EVENT_PAGE_BYTE_BUDGET`, `cursor`,*)
(*       `more_events`, and the operator-review and expired-result errors  *)
(*   crates/aether/src/proof/scheduler.rs  the event callback of           *)
(*       `run_execution`, which calls `append_event`                       *)
(* Caller side:                                                            *)
(*   crates/aether/src/transport/client.rs  `request_with_polling_on`,     *)
(*       `response_last_event_sequence`, `response_more_events_remain`,    *)
(*       `is_terminal_task_status` (`aether node request` and the other    *)
(*       `request_once*` callers)                                          *)
(*   crates/aether/src/outbox.rs  `follow`, `outcome`, and the cursor that *)
(*       `attempt_in` keeps in `sent_tasks.last_event_sequence`            *)
(*   crates/aether/src/direct.rs  `resend_frame`                           *)
(* The contract: references/aether-wire-profile.md, "Event paging".        *)
(*                                                                         *)
(* One task and one caller. `mode` is chosen in Init and does not change,  *)
(* so one TLC run checks both polling loops: "cli" is                      *)
(* `request_with_polling_on`, which keeps every event frame; "outbox" is   *)
(* `follow`, which keeps only the cursor and the answer. A `follow` whose  *)
(* window closes keeps the cursor in `sent_tasks`, and the next attempt    *)
(* polls on from it, so the attempts of one task are one loop here.        *)
(*                                                                         *)
(* `task_progress` reads the state and the page in one store call, so the  *)
(* host read is one step. At most one exchange is in flight: each loop     *)
(* waits for its answer before it polls again.                             *)
(*                                                                         *)
(* Abstracted away: admission, replay keys and the grant (see Outbox and   *)
(* Admission), the scheduler and the lease (see ExecutionLease; the task   *)
(* here is admitted, runs, and ends in one step each), `pending_approval`, *)
(* `leased`, `rejected` and `operator_reviewed` (they page like `queued`,  *)
(* `failed` and `unknown_result`), the answer text, the same frame that    *)
(* `Link::exchange` sends again (see Outbox), and the gap between          *)
(* `task_progress` and `content_is_expired`, which are two store calls. A  *)
(* caller that sends a `lastEventSequence` past the end of the journal     *)
(* gets an empty page with that cursor back; the callers here send only    *)
(* the cursor that they got. The size of an event is a small number: real  *)
(* event frames hold only a state and are far smaller than the budget.     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MaxEvents,  \* bound on the events in the journal
    PageLimit,  \* MAX_FRAMES - 1: the event frames of one page
    Budget,     \* EVENT_PAGE_BYTE_BUDGET
    Sizes,      \* the encoded sizes that a progress event can have
    MaxLost     \* bound on lost exchanges

Active   == {"queued", "running"}
Terminal == {"succeeded", "failed", "cancelled"}
States   == Active \cup Terminal \cup {"unknown_result"}

\* `a2a_status`, as far as the callers tell the values apart.
StatusOf(s) ==
    CASE s \in Active     -> "working"
      [] s = "succeeded"  -> "completed"
      [] OTHER            -> "failed"   \* failed, canceled

Kinds == States \cup {"progress", "result", "content_expired"}

None == [kind |-> "none"]

VARIABLES
    mode,     \* "cli" or "outbox" (fixed in Init)
    state,    \* tasks.state
    expired,  \* tasks.content_expired_at IS NOT NULL
    events,   \* task_events rows of the task: [seq, kind, size]
    termSeq,  \* ghost: the sequence of the event that ended the task, or 0
    ran,      \* ghost: an agent turn started, so late events can come
    req,      \* the request frame in flight, or None
    rep,      \* the answer in flight, or None
    c,        \* the caller: [phase, cursor, got]
    lost      \* how many exchanges were lost

vars == <<mode, state, expired, events, termSeq, ran, req, rep, c, lost>>

End == Cardinality(events)
SizeOf(s) == (CHOOSE e \in events : e.seq = s).size

Phases == {"idle", "sent", "done", "error", "aborted"}

TypeOK ==
    /\ mode \in {"cli", "outbox"}
    /\ state \in States
    /\ expired \in BOOLEAN
    /\ events \subseteq [seq : 1..MaxEvents, kind : Kinds, size : Nat]
    /\ termSeq \in 0..MaxEvents
    /\ ran \in BOOLEAN
    /\ req \in {None} \cup [kind : {"req"}, after : 0..MaxEvents]
    /\ rep.kind \in {"none", "page", "error"}
    /\ c.phase \in Phases
    /\ c.cursor \in 0..MaxEvents
    /\ c.got \in Seq(1..MaxEvents)
    /\ lost \in 0..MaxLost

Init ==
    /\ mode \in {"cli", "outbox"}
    \* `admit`: the task row and its first event in one transaction.
    /\ state = "queued"
    /\ expired = FALSE
    /\ events = {[seq |-> 1, kind |-> "queued", size |-> 1]}
    /\ termSeq = 0
    /\ ran = FALSE
    /\ req = None
    /\ rep = None
    /\ c = [phase |-> "idle", cursor |-> 0, got |-> <<>>]
    /\ lost = 0

-----------------------------------------------------------------------------
(* The host's store. Each action is one SQLite transaction.                *)

\* `event`: COALESCE(MAX(sequence),0)+1.
NextSeq == IF events = {} THEN 1
           ELSE (CHOOSE s \in {e.seq : e \in events} :
                    \A e \in events : e.seq <= s) + 1

AddEvent(kind, size) ==
    /\ End < MaxEvents
    /\ events' = events \cup {[seq |-> NextSeq, kind |-> kind, size |-> size]}

\* `change` -> `enter`: the new state and its event in one transaction.
Enter(s) ==
    /\ AddEvent(s, 1)
    /\ state' = s
    /\ termSeq' = IF s \in Terminal THEN NextSeq ELSE termSeq

\* `mark_running`.
Start ==
    /\ state = "queued"
    /\ Enter("running")
    /\ ran' = TRUE
    /\ UNCHANGED <<mode, expired, req, rep, c, lost>>

\* The event callback of `run_execution` -> `append_event`: the agent
\* reports progress. `append_event` checks only that the task exists, and
\* the callback holds back only a `Completed` event of a cancelled task, so
\* a progress event can also come after a cancel.
Progress ==
    /\ state = "running" \/ (state = "cancelled" /\ ran)
    /\ \E size \in Sizes : AddEvent("progress", size)
    /\ UNCHANGED <<mode, state, expired, termSeq, ran, req, rep, c, lost>>

\* `mark_succeeded`, `mark_failed`, `mark_unknown_result`.
Finish ==
    /\ state = "running"
    /\ \E s \in {"succeeded", "failed", "unknown_result"} : Enter(s)
    /\ UNCHANGED <<mode, expired, ran, req, rep, c, lost>>

\* `cancel` / `cancel_if_owned`.
Cancel ==
    /\ state \in Active
    /\ Enter("cancelled")
    /\ UNCHANGED <<mode, expired, ran, req, rep, c, lost>>

\* `mark_succeeded` on a cancelled task: the late answer is one more event.
LateResult ==
    /\ state = "cancelled" /\ ran
    /\ AddEvent("result", 1)
    /\ UNCHANGED <<mode, state, expired, termSeq, ran, req, rep, c, lost>>

\* `clear_expired_content`: 24 hours after the task ended.
ContentExpires ==
    /\ state \in Terminal
    /\ ~expired
    /\ AddEvent("content_expired", 1)
    /\ expired' = TRUE
    /\ UNCHANGED <<mode, state, termSeq, ran, req, rep, c, lost>>

-----------------------------------------------------------------------------
(* One page: `A2aTransport::handle` after admission.                       *)

RECURSIVE Bytes(_, _)
\* The encoded size of the k events after `after`.
Bytes(after, k) == IF k = 0 THEN 0 ELSE Bytes(after, k - 1) + SizeOf(after + k)

\* `events_page`: at most PageLimit events after `after`.
Fetched(after) ==
    LET rest == IF End > after THEN End - after ELSE 0
    IN IF rest > PageLimit THEN PageLimit ELSE rest

\* The page loop: the events that fit in the byte budget.
Sent(after) ==
    CHOOSE k \in 0..Fetched(after) :
        /\ Bytes(after, k) <= Budget
        /\ k = Fetched(after) \/ Bytes(after, k + 1) > Budget

\* `end` and `term` are ghosts: the journal end and `termSeq` at the read.
Page(after) ==
    LET n == Sent(after)
    IN [kind   |-> "page",
        after  |-> after,
        seqs   |-> [i \in 1..n |-> after + i],
        cursor |-> after + n,
        \* `has_more` of `events_page`, or the byte budget stopped the loop.
        more   |-> (End - after > PageLimit) \/ n < Fetched(after),
        status |-> StatusOf(state),
        end    |-> End,
        term   |-> termSeq]

Error == [kind |-> "error"]

\* `handle`: `task_progress` reads the state and the page together. A task
\* that waits for the operator, and an answer that expired, get an error
\* frame and no events.
HostRead ==
    /\ req # None
    /\ rep' = IF state = "unknown_result" \/ (state = "succeeded" /\ expired)
              THEN Error ELSE Page(req.after)
    /\ req' = None
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran, c, lost>>

-----------------------------------------------------------------------------
(* The caller.                                                             *)

\* One turn of either loop: a frame with `lastEventSequence` set to the
\* cursor (`request_with_polling_on`; `resend_frame` in `follow`).
Poll ==
    /\ c.phase = "idle"
    /\ req' = [kind |-> "req", after |-> c.cursor]
    /\ c' = [c EXCEPT !.phase = "sent"]
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran, rep, lost>>

Max(a, b) == IF a > b THEN a ELSE b

Ended(status) == status \in {"completed", "failed"}

\* `request_with_polling_on`: keep the event frames, and stop on an error,
\* or on an ended task whose page has no `aetherMoreEvents`.
CliReceive ==
    /\ mode = "cli"
    /\ c.phase = "sent"
    /\ rep # None
    /\ IF rep.kind = "error"
       THEN c' = [c EXCEPT !.phase = "error"]
       ELSE c' = [phase  |-> IF Ended(rep.status) /\ ~rep.more
                             THEN "done" ELSE "idle",
                  cursor |-> Max(c.cursor, rep.cursor),
                  got    |-> c.got \o rep.seqs]
    /\ rep' = None
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran, req, lost>>

\* `follow` with `outcome`: stop on an error, or on a state that is not
\* Working whose page has no `aetherMoreEvents`. It keeps no event frames.
\* When the window closes first, `still_working` keeps the task active.
OutboxReceive ==
    /\ mode = "outbox"
    /\ c.phase = "sent"
    /\ rep # None
    /\ IF rep.kind = "error"
       THEN c' = [c EXCEPT !.phase = "error"]
       ELSE c' = [c EXCEPT !.phase = IF Ended(rep.status) /\ ~rep.more
                                     THEN "done" ELSE "idle",
                           !.cursor = Max(c.cursor, rep.cursor)]
    /\ rep' = None
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran, req, lost>>

\* A shed exchange is not an action: the per-peer rate limit of the host
\* answers "node overloaded" with `retryAfterMs`, before the host admits
\* anything or reads the journal. `request_with_polling_on` and `follow`
\* send the frame again with the same cursor for up to
\* `OVERLOAD_RETRY_WINDOW`, with every wait cut to the end of that window
\* and of the poll deadline or attempt window. No state that this model
\* tracks changes (the journal, the cursor, `got`), so it is a stutter step
\* of `Poll`. When the window ends, the caller gets the error, which
\* `CliReceive` and `OutboxReceive` take as an "error" reply. A full queue
\* sends the same message without `retryAfterMs` and is not sent again.

\* The frame or its answer is lost, or the exchange times out. The cli loop
\* returns the error and drops what it collected. The outbox keeps the
\* cursor in `sent_tasks` (`record_sent_attempt`), and the next attempt
\* sends it again.
Lose ==
    /\ c.phase = "sent"
    /\ lost < MaxLost
    /\ lost' = lost + 1
    /\ req' = None
    /\ rep' = None
    /\ c' = [c EXCEPT !.phase = IF mode = "cli" THEN "aborted" ELSE "idle"]
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran>>

\* `request_with_polling_on`: TASK_POLL_DEADLINE passed.
CliTimeout ==
    /\ mode = "cli"
    /\ c.phase = "idle"
    /\ c' = [c EXCEPT !.phase = "aborted"]
    /\ UNCHANGED <<mode, state, expired, events, termSeq, ran, req, rep, lost>>

-----------------------------------------------------------------------------

Next ==
    \/ Start \/ Progress \/ Finish \/ Cancel \/ LateResult \/ ContentExpires
    \/ HostRead
    \/ Poll \/ CliReceive \/ OutboxReceive \/ Lose \/ CliTimeout

\* The caller keeps polling and the host answers. The task itself gets no
\* fairness: an agent may run for a long time.
Fairness ==
    /\ WF_vars(Poll)
    /\ WF_vars(HostRead)
    /\ WF_vars(CliReceive)
    /\ WF_vars(OutboxReceive)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* The journal.                                                            *)

\* The sequences are 1, 2, 3, ... without a gap, and each is used once.
Dense ==
    /\ {e.seq : e \in events} = 1..End
    /\ \A e1, e2 \in events : e1.seq = e2.seq => e1 = e2

\* An event never changes and never goes away.
JournalNeverChanges == [][events \subseteq events']_vars

(* One page.                                                               *)

\* A page is the ordered run of events right after the caller's cursor,
\* and its cursor is the last event in it.
PageIsRun ==
    rep.kind = "page" =>
        /\ \A i \in 1..Len(rep.seqs) : rep.seqs[i] = rep.after + i
        /\ rep.cursor = rep.after + Len(rep.seqs)
        /\ Len(rep.seqs) <= PageLimit
        /\ rep.cursor <= rep.end

\* A page says "no more events" only when it reached the end of the
\* journal as of its read, and "more events" only when it did not.
MoreIsHonest ==
    rep.kind = "page" => (rep.more <=> rep.cursor < rep.end)

\* The state and the page are from one instant: a page that shows an ended
\* task was read when the event that ended it was in the journal, and a
\* page that shows a working task was read before that.
SnapshotAtOneInstant ==
    rep.kind = "page" =>
        (Ended(rep.status) <=> (rep.term > 0 /\ rep.term <= rep.end))

\* A page moves the cursor when events remain. It holds only while every
\* event fits in the byte budget alone (EventPaging.cfg: Sizes within
\* 1..Budget). With Sizes = {1, 3} and Budget = 2, TLC finds: Start,
\* Progress(3), Poll, HostRead (events 1 and 2 fit), CliReceive, Poll,
\* HostRead: event 3 is larger than the budget, so the loop in `handle`
\* breaks before the first event, and the page has no events, the same
\* cursor, and `aetherMoreEvents`. Each later poll gets the same page. A
\* real event frame is some hundred bytes and the budget is 4 MB less
\* 64 KB, so the code does not reach this.
PageProgress ==
    rep.kind = "page" /\ rep.end > rep.after => rep.cursor > rep.after

(* The caller.                                                             *)

\* The cursor never moves back, and never passes the end of the journal.
CursorNeverBack == [][c'.cursor >= c.cursor]_vars
CursorInJournal == c.cursor <= End

\* The cli loop has each event up to its cursor once, in order.
CliInOrder ==
    mode = "cli" =>
        /\ Len(c.got) = c.cursor
        /\ \A i \in 1..Len(c.got) : c.got[i] = i

\* A cli loop that stops with an answer has every event up to the one that
\* ended the task. Events that come after its last page (a late result of a
\* cancelled task, `content_expired`) are not part of this.
CliComplete ==
    mode = "cli" /\ c.phase = "done" => termSeq > 0 /\ c.cursor >= termSeq

\* The cursor that the outbox keeps for an ended task covers the event
\* that ended it: `follow` polls on while a page has `aetherMoreEvents`,
\* as the wire profile asks, also when the status looks terminal.
OutboxReadsToEnd ==
    mode = "outbox" /\ c.phase = "done" => termSeq > 0 /\ c.cursor >= termSeq

\* An ended task lets the caller stop: with its last page, with an error
\* frame, or (cli) with a lost exchange or the poll deadline.
EndedTaskStops ==
    (state \in Terminal \cup {"unknown_result"})
        ~> (c.phase \in {"done", "error", "aborted"} \/ lost = MaxLost)

=============================================================================
