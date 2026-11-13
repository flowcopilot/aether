--------------------------- MODULE ManagedRoster ---------------------------
(***************************************************************************)
(* The managed and directory trust scopes: how a node learns who is a      *)
(* member from a control service, how it keeps signed directory records,   *)
(* and how the kept roster decides whether a signed claim admits work.     *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/control_service.rs   publish, resolve, revoke,      *)
(*       changes_after, redeem_managed_invitation, revoke_managed_member,  *)
(*       managed_changes_after                                             *)
(*   crates/aether-store/src/control.rs     redeem_managed_invitation,     *)
(*       revoke_managed_member, managed_events_after,                      *)
(*       publish_signed_record, revoke_signed_record                       *)
(*   crates/aether/src/managed_client.rs    events_after, sync_managed     *)
(*   crates/aether-store/src/managed.rs     managed_cursor,                *)
(*       apply_managed_member, revoke_managed_member, advance_cursor,      *)
(*       ManagedMember::is_active                                          *)
(*   crates/aether/src/identity.rs          RosterClaims::verify           *)
(*   crates/aether/src/directory_client.rs  validate, validate_event,      *)
(*       events_after, sync_directory, cached_directory_record,            *)
(*       request_via_directory                                             *)
(*   crates/aether-store/src/directory.rs   cache_directory_record,        *)
(*       mark_directory_record_stale, advance_directory_cursor             *)
(*   crates/aether/src/discovery.rs         validate_record                *)
(*                                                                         *)
(* The two scopes do not share state, so `mode` picks one of them in Init  *)
(* and does not change. One TLC run checks both.                           *)
(*                                                                         *)
(* Trust. The managed control service is TRUSTED: a roster event has no    *)
(* signature, and the node knows the service only by its pinned TLS        *)
(* certificate. A service that lies can add any member, so the model has   *)
(* an honest managed service. The network can lose a reply, and a node     *)
(* process can stop at any step. The directory service is NOT trusted for  *)
(* the content of a record: one key that the caller trusts signs each      *)
(* record. When `dirHonest` is FALSE, each answer of the directory can be  *)
(* any record that the trusted key ever signed, or any record that the     *)
(* attacker made. Signatures are perfect.                                  *)
(*                                                                         *)
(* A node keeps one roster for a scope, but one cursor for each pair of    *)
(* control URL and scope (`cursor_key`). `Keys` are the spellings of the   *)
(* control URL that the owner passes to `--managed-control` or to          *)
(* `aether managed sync`. Each cursor reads the events from the start, so  *)
(* a row of the roster only moves from absent to active to revoked.        *)
(* `OneKey` says that the node used one spelling only.                     *)
(*                                                                         *)
(* Abstracted away: the creation and the expiry of a token (each token    *)
(* exists in Init), the expiry of a member (an expired member only loses  *)
(* its access, in `is_active`), the role, other scopes of                 *)
(* one control service, the `members` list and the route of a caller to a  *)
(* managed host (`request_task`), the claim itself (each pair of agent and *)
(* endpoint has a valid claim of the trusted issuer, the worst case), the  *)
(* grant, addresses and capabilities of a record, `search`, the renewal    *)
(* worker, and wall-clock time (each expiry is a flag that an `Expire*`    *)
(* action sets at any moment). The expiry of a directory record is its     *)
(* serial: a record that the trusted key signed later expires later, and   *)
(* each record of the attacker expires at one time. A directory page holds *)
(* one event here; the real page holds up to 128 (`EVENT_PAGE`), and one   *)
(* malformed event refuses the whole page. The directory cursor has one    *)
(* key here.                                                               *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Agents,     \* agent ids
    Eps,        \* endpoint ids
    Tokens,     \* managed invitation tokens
    Keys,       \* spellings of the control URL (one cursor for each)
    Procs,      \* processes that run `sync_managed` on one data directory
    Page,       \* events in one managed page
    MaxRec,     \* directory records that the trusted key signs
    MaxDirLog   \* bound on directory events

None == "none"
MaxLog == 2 * Cardinality(Tokens)   \* one join and one revoke for each token
Scopes == {"dir", "else"}           \* the scope that the caller asks for, and another

NoRow == [st |-> None, ep |-> None]
Rows == [st : {None, "active", "revoked"}, ep : Eps \cup {None}]
Events == [type : {"joined", "revoked"}, agent : Agents, ep : Eps \cup {None}]

\* A directory record. `key` is the signing key, `sig` says whether the
\* signature fits the fields, and `n` is the serial of a record that the
\* trusted key signed (0 for a record that the attacker made).
Rec == [agent : Agents, scope : Scopes, key : {"trusted", "other"},
        sig : {"good", "bad"}, n : 0..MaxRec]
NoRec == [agent |-> None, scope |-> None, key |-> None, sig |-> None, n |-> 0]
\* Records of the attacker: its own key, or the trusted key with a
\* signature that does not fit.
Evil == {r \in Rec : r.n = 0 /\ ~(r.key = "trusted" /\ r.sig = "good")}
DirEvents == [type : {"put", "revoke"}, agent : Agents, scope : Scopes,
              rec : Rec \cup {NoRec}]

VARIABLES
    mode,        \* "managed" or "directory" (fixed in Init)
    \* --- the managed control service ---
    tok,         \* managed_invitations: "issued" or "redeemed"
    joins,       \* ghost: how many members each token made
    cm,          \* managed_members of the service
    log,         \* managed_events of the scope, in order
    \* --- the node, managed scope ---
    roster,      \* managed_members of the node
    cur,         \* the roster cursor of each control URL
    proc,        \* each sync process: its URL and the page that it holds
    revApplied,  \* ghost: members whose revoke event the node applied
    \* --- the directory ---
    dirHonest,   \* whether the directory follows control_service.rs
    signed,      \* the records that the trusted key signed
    rexp,        \* serials whose expires_at passed
    dstore,      \* signed_records of the directory, by agent
    dlog,        \* directory_events, in order
    high,        \* directory_record_marks.latest_expires_at, by agent and key
    tomb,        \* directory_record_marks.revoked_through, by agent and key
    revoked,     \* ghost: records that a revoke removed from the directory
    cache,       \* the node's cached directory records, by scope and agent
    dcur,        \* the node's directory cursor
    dpc,         \* sync_directory: "idle", or "advance" before the cursor moves
    badUse       \* ghost: which wrong uses of a record happened

mvars == <<tok, joins, cm, log, roster, cur, proc, revApplied>>
dvars == <<dirHonest, signed, rexp, dstore, dlog, high, tomb, revoked, cache,
           dcur, dpc, badUse>>
vars == <<mode, mvars, dvars>>

Idle(k) == [key |-> k, lo |-> 0, hi |-> 0]
SignKeys == {"trusted", "other"}
\* The expiry of a record as a mark: 0 is "no mark".
Exp(r) == r.n + 1
NoEntry == [rec |-> NoRec, stale |-> FALSE]

TypeOK ==
    /\ mode \in {"managed", "directory"}
    /\ tok \in [Tokens -> {"issued", "redeemed"}]
    /\ joins \in [Tokens -> Nat]
    /\ cm \in [Agents -> Rows]
    /\ log \in Seq(Events) /\ Len(log) <= MaxLog
    /\ roster \in [Agents -> Rows]
    /\ cur \in [Keys -> 0..MaxLog]
    /\ proc \in [Procs -> [key : Keys, lo : 0..MaxLog, hi : 0..MaxLog]]
    /\ revApplied \subseteq Agents
    /\ dirHonest \in BOOLEAN
    /\ signed \subseteq Rec
    /\ rexp \subseteq 1..MaxRec
    /\ dstore \in [Agents -> Rec \cup {NoRec}]
    /\ dlog \in Seq(DirEvents) /\ Len(dlog) <= MaxDirLog
    /\ high \in [Agents -> [SignKeys -> 0..(MaxRec + 1)]]
    /\ tomb \in [Agents -> [SignKeys -> 0..(MaxRec + 1)]]
    /\ revoked \subseteq Rec
    /\ cache \in [Scopes -> [Agents -> [rec : Rec \cup {NoRec}, stale : BOOLEAN]]]
    /\ dcur \in 0..MaxDirLog
    /\ dpc \in {"idle", "advance"}
    /\ badUse \subseteq {"untrusted", "resolved_other_agent"}

Init ==
    /\ mode \in {"managed", "directory"}
    /\ tok = [t \in Tokens |-> "issued"]
    /\ joins = [t \in Tokens |-> 0]
    /\ cm = [a \in Agents |-> NoRow]
    /\ log = <<>>
    /\ roster = [a \in Agents |-> NoRow]
    /\ cur = [k \in Keys |-> 0]
    /\ proc = [p \in Procs |-> Idle(CHOOSE k \in Keys : TRUE)]
    /\ revApplied = {}
    /\ dirHonest \in (IF mode = "directory" THEN BOOLEAN ELSE {TRUE})
    /\ signed = {}
    /\ rexp = {}
    /\ dstore = [a \in Agents |-> NoRec]
    /\ dlog = <<>>
    /\ high = [a \in Agents |-> [k \in SignKeys |-> 0]]
    /\ tomb = [a \in Agents |-> [k \in SignKeys |-> 0]]
    /\ revoked = {}
    /\ cache = [s \in Scopes |-> [a \in Agents |-> NoEntry]]
    /\ dcur = 0
    /\ dpc = "idle"
    /\ badUse = {}

-----------------------------------------------------------------------------
(* The managed control service. Each action is one SQLite transaction.     *)

\* ControlService::redeem_managed_invitation -> ControlStore::
\* redeem_managed_invitation: the token must be unused and not expired, and
\* the INSERT fails for an agent id that has a row (PRIMARY KEY, also when
\* the row is revoked) or an endpoint that another member has (UNIQUE).
\* A failed INSERT rolls the token back to unused.
Redeem(t, a, e) ==
    /\ mode = "managed"
    /\ tok[t] = "issued"
    /\ cm[a] = NoRow
    /\ \A b \in Agents : cm[b].ep # e
    /\ tok' = [tok EXCEPT ![t] = "redeemed"]
    /\ joins' = [joins EXCEPT ![t] = @ + 1]
    /\ cm' = [cm EXCEPT ![a] = [st |-> "active", ep |-> e]]
    /\ log' = Append(log, [type |-> "joined", agent |-> a, ep |-> e])
    /\ UNCHANGED <<mode, roster, cur, proc, revApplied, dvars>>

\* ControlStore::revoke_managed_member: only a member that is not revoked.
RevokeMember(a) ==
    /\ mode = "managed"
    /\ cm[a].st = "active"
    /\ cm' = [cm EXCEPT ![a].st = "revoked"]
    /\ log' = Append(log, [type |-> "revoked", agent |-> a, ep |-> None])
    /\ UNCHANGED <<mode, tok, joins, roster, cur, proc, revApplied,
                   dvars>>

-----------------------------------------------------------------------------
(* The node: sync_managed.                                                 *)

Min(x, y) == IF x < y THEN x ELSE y

\* `managed_cursor`, then ManagedClient::events_after: the page after the
\* cursor of this process's control URL. An honest service answers with the
\* next events in order, so `monotonic_cursors` passes.
Fetch(p) ==
    /\ mode = "managed"
    /\ proc[p].lo = proc[p].hi
    /\ LET c == cur[proc[p].key] IN
       /\ c < Len(log)
       /\ proc' = [proc EXCEPT ![p].lo = c, ![p].hi = Min(c + Page, Len(log))]
    /\ UNCHANGED <<mode, tok, joins, cm, log, roster, cur, revApplied,
                   dvars>>

\* One event of the page: apply_managed_member or revoke_managed_member.
\* The cursor and the row change in one transaction. `advance_cursor`
\* refuses a cursor that does not move forward. A join writes the row of
\* its agent unless the row is revoked (ON CONFLICT DO UPDATE ... WHERE
\* revoked_at IS NULL); it fails when another agent has the endpoint
\* (UNIQUE). A revoke keeps the revoked_at of a row that is revoked, and
\* fails with NotFound when the agent has no row. A failure ends the sync.
Apply(p) ==
    /\ mode = "managed"
    /\ proc[p].lo < proc[p].hi
    /\ LET k == proc[p].key
           i == proc[p].lo + 1
           ev == log[i]
           a == ev.agent
           ok == /\ i > cur[k]
                 /\ IF ev.type = "joined"
                    THEN \A b \in Agents \ {a} : roster[b].ep # ev.ep
                    ELSE roster[a] # NoRow
       IN IF ok
          THEN /\ cur' = [cur EXCEPT ![k] = i]
               /\ proc' = [proc EXCEPT ![p].lo = i]
               /\ roster' = [roster EXCEPT ![a] =
                               IF ev.type = "revoked"
                               THEN [@ EXCEPT !.st = "revoked"]
                               ELSE IF @.st = "revoked" THEN @
                               ELSE [st |-> "active", ep |-> ev.ep]]
               /\ revApplied' = IF ev.type = "revoked"
                                THEN revApplied \cup {a} ELSE revApplied
          ELSE /\ proc' = [proc EXCEPT ![p].lo = proc[p].hi]
               /\ UNCHANGED <<cur, roster, revApplied>>
    /\ UNCHANGED <<mode, tok, joins, cm, log, dvars>>

\* The process stops in the middle of a page, or the reply is lost.
Lose(p) ==
    /\ mode = "managed"
    /\ proc[p].lo < proc[p].hi
    /\ proc' = [proc EXCEPT ![p].lo = proc[p].hi]
    /\ UNCHANGED <<mode, tok, joins, cm, log, roster, cur, revApplied,
                   dvars>>

\* The owner starts the node, or runs `aether managed sync`, with another
\* spelling of the control URL. The roster stays; the cursor is another
\* one, and it starts before the first event.
Rekey(p, k) ==
    /\ mode = "managed"
    /\ proc[p].lo = proc[p].hi
    /\ proc[p].key # k
    /\ proc' = [proc EXCEPT ![p] = Idle(k)]
    /\ UNCHANGED <<mode, tok, joins, cm, log, roster, cur, revApplied,
                   dvars>>

\* RosterClaims::verify for a claim of the trusted issuer that names agent
\* `a` at the authenticated endpoint `e`: a roster row of the agent with
\* that endpoint, not revoked.
\* It reads the roster and changes nothing, so it is a predicate here, not
\* an action: the claim admits work in each state where it holds.
\* `RosterClaims` reads through a `KeptStore`, which stays open between
\* calls. Each read starts a new read transaction, so it sees the roster as
\* committed at that moment, as a new open did; the store opens again when
\* the file is replaced or its `user_version` differs, and a store that
\* cannot open gives `ProviderError::Unavailable`, which admits nothing.
\* That is the same predicate: a stutter step, not a change to the model.
Admits(a, e) ==
    roster[a].st = "active" /\ roster[a].ep = e

-----------------------------------------------------------------------------
(* The directory.                                                          *)

Serials == {r.n : r \in signed}

\* The holder of the trusted key signs a record (`signed_record`).
Sign(a, s) ==
    /\ mode = "directory"
    /\ Cardinality(signed) < MaxRec
    /\ signed' = signed \cup {[agent |-> a, scope |-> s, key |-> "trusted",
                               sig |-> "good", n |-> Cardinality(signed) + 1]}
    /\ UNCHANGED <<mode, mvars, dirHonest, rexp, dstore, dlog, high, tomb,
                   revoked, cache, dcur, dpc, badUse>>

\* Time passes the record's expires_at.
ExpireRec(n) ==
    /\ mode = "directory"
    /\ n \in Serials \ rexp
    /\ rexp' = rexp \cup {n}
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, dstore, dlog, high, tomb,
                   revoked, cache, dcur, dpc, badUse>>

\* `validate_record` in the service: the signature fits the record's own
\* key, and the record has not expired. It does not know a trusted key.
ServiceAccepts(r) == r.sig = "good" /\ r.n \notin rexp

\* ControlService::publish -> ControlStore::publish_signed_record. The
\* request is the record alone, so anyone who holds a record can send it:
\* its owner, or a reader of the directory. The service takes a record of
\* any key for an agent id that has no record, and only the same key for
\* an agent id that has one. It refuses a record that does not expire
\* after the last revoke of its agent id and key (`revoked_through`): each
\* record that was public before the revoke is such a record.
DirPublish(r) ==
    /\ mode = "directory" /\ dirHonest
    /\ r \in signed \cup Evil
    /\ ServiceAccepts(r)
    /\ dstore[r.agent] = NoRec \/ dstore[r.agent].key = r.key
    /\ Exp(r) > tomb[r.agent][r.key]
    /\ Len(dlog) < MaxDirLog
    /\ dstore' = [dstore EXCEPT ![r.agent] = r]
    /\ dlog' = Append(dlog, [type |-> "put", agent |-> r.agent,
                             scope |-> r.scope, rec |-> r])
    /\ high' = [high EXCEPT ![r.agent][r.key] =
                   IF Exp(r) > @ THEN Exp(r) ELSE @]
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, tomb, revoked, cache,
                   dcur, dpc, badUse>>

\* The records of the key of agent `a`'s stored record that the directory
\* took for `a`: what a reader of the directory can hold.
Public(a) ==
    {dlog[i].rec : i \in {j \in 1..Len(dlog) :
        /\ dlog[j].type = "put" /\ dlog[j].agent = a
        /\ dlog[j].rec.key = dstore[a].key}}

\* ControlService::revoke -> ControlStore::revoke_signed_record. The
\* request is a record of the same agent and key that `validate_record`
\* accepts; the stored record itself is one, so anyone who read a record
\* can revoke it. `revoked_through` becomes `latest_expires_at`.
DirRevoke(a) ==
    /\ mode = "directory" /\ dirHonest
    /\ dstore[a] # NoRec
    /\ \E w \in signed \cup Evil :
          w.agent = a /\ ServiceAccepts(w) /\ w.key = dstore[a].key
    /\ Len(dlog) < MaxDirLog
    /\ dstore' = [dstore EXCEPT ![a] = NoRec]
    /\ dlog' = Append(dlog, [type |-> "revoke", agent |-> a,
                             scope |-> dstore[a].scope, rec |-> NoRec])
    /\ tomb' = [tomb EXCEPT ![a][dstore[a].key] = high[a][dstore[a].key]]
    /\ revoked' = revoked \cup Public(a)
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, high, cache, dcur,
                   dpc, badUse>>

\* DirectoryClient::validate: the trusted key, a signature that fits, and
\* not expired.
ClientAccepts(r) == r.key = "trusted" /\ r.sig = "good" /\ r.n \notin rexp

\* What a trusted record is, said without the client's checks.
Trusted(r) == r \in signed /\ r.n \notin rexp

\* The next event that the node reads. An attacker directory can send any.
NextDirEvents ==
    IF dirHonest
    THEN (IF dcur < Len(dlog) THEN {dlog[dcur + 1]} ELSE {})
    ELSE (IF dcur < MaxDirLog
          THEN [type : {"put", "revoke"}, agent : Agents, scope : Scopes,
                rec : signed \cup Evil \cup {NoRec}]
          ELSE {})

\* DirectoryClient::validate_event: the names of the event and of its
\* record agree. Else events_after refuses the page.
EventShape(ev) ==
    IF ev.type = "put"
    THEN ev.rec # NoRec /\ ev.rec.agent = ev.agent /\ ev.rec.scope = ev.scope
    ELSE ev.rec = NoRec

\* A record that the node does not keep and does not refuse: another key
\* signed it, or it expired (DirectoryClient::validate checks the key,
\* then validate_record checks the expiry before the signature).
\* events_after takes such a record out of its event.
Unusable(r) == r.key # "trusted" \/ r.n \in rexp

\* sync_directory, first half: events_after refuses a page with an event
\* of a wrong shape or with a signature that does not fit, and then
\* nothing changes. Else cache_directory_record for a record that the node
\* can use, mark_directory_record_stale for a revoke, and nothing for a
\* record that the node cannot use.
DirApply ==
    /\ mode = "directory"
    /\ dpc = "idle"
    /\ \E ev \in NextDirEvents :
        /\ EventShape(ev)
        /\ ev.type = "put" => Unusable(ev.rec) \/ ClientAccepts(ev.rec)
        /\ cache' = [cache EXCEPT ![ev.scope][ev.agent] =
                       IF ev.type = "put"
                       THEN (IF Unusable(ev.rec) THEN @
                             ELSE [rec |-> ev.rec, stale |-> FALSE])
                       ELSE IF @.rec = NoRec THEN @
                       ELSE [@ EXCEPT !.stale = TRUE]]
    /\ dpc' = "advance"
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, dstore, dlog, high,
                   tomb, revoked, dcur, badUse>>

\* sync_directory, second half: advance_directory_cursor. It is another
\* statement than the cache write, not the same transaction.
DirAdvance ==
    /\ mode = "directory"
    /\ dpc = "advance"
    /\ dcur' = dcur + 1
    /\ dpc' = "idle"
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, dstore, dlog, high,
                   tomb, revoked, cache, badUse>>

\* The process stops between the two halves. The next sync reads the same
\* event again.
DirCrash ==
    /\ mode = "directory"
    /\ dpc = "advance"
    /\ dpc' = "idle"
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, dstore, dlog, high,
                   tomb, revoked, cache, dcur, badUse>>

\* What `resolve` answers for the agent id `want`. ControlService::resolve
\* answers 410 for an expired record.
Resolved(want) ==
    IF dirHonest
    THEN (IF dstore[want] # NoRec /\ dstore[want].n \notin rexp
          THEN {dstore[want]} ELSE {})
    ELSE signed \cup Evil

\* request_via_directory, when the directory answers: DirectoryClient::
\* resolve checks the agent id (validate_resolved), the key, the signature
\* and the expiry, and the caller checks the scope. The caller caches the
\* record and sends the task to the record's endpoint.
Resolve(want) ==
    /\ mode = "directory"
    /\ \E r \in Resolved(want) :
        /\ r.agent = want
        /\ ClientAccepts(r)
        /\ r.scope = "dir"
        /\ cache' = [cache EXCEPT ![r.scope][r.agent] =
                       [rec |-> r, stale |-> FALSE]]
        /\ badUse' = badUse
             \cup (IF Trusted(r) /\ r.scope = "dir" THEN {} ELSE {"untrusted"})
             \cup (IF r.agent = want THEN {} ELSE {"resolved_other_agent"})
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, dstore, dlog, high,
                   tomb, revoked, dcur, dpc>>

\* request_via_directory, when the directory cannot be reached:
\* cached_directory_record checks the names, the expiry (a stale record
\* has expires_at 0), the key and the signature.
UseCached(want) ==
    /\ mode = "directory"
    /\ LET c == cache["dir"][want] IN
        /\ c.rec # NoRec /\ ~c.stale
        /\ c.rec.agent = want /\ c.rec.scope = "dir"
        /\ ClientAccepts(c.rec)
        /\ badUse' = badUse
             \cup (IF Trusted(c.rec) /\ c.rec.scope = "dir" THEN {}
                   ELSE {"untrusted"})
    /\ UNCHANGED <<mode, mvars, dirHonest, signed, rexp, dstore, dlog, high,
                   tomb, revoked, cache, dcur, dpc>>

-----------------------------------------------------------------------------

Next ==
    \/ \E t \in Tokens, a \in Agents, e \in Eps : Redeem(t, a, e)
    \/ \E a \in Agents : RevokeMember(a)
    \/ \E p \in Procs : Fetch(p) \/ Apply(p) \/ Lose(p)
                        \/ \E k \in Keys : Rekey(p, k)
    \/ \E a \in Agents, s \in Scopes : Sign(a, s)
    \/ \E n \in 1..MaxRec : ExpireRec(n)
    \/ \E r \in Rec : DirPublish(r)
    \/ \E a \in Agents : DirRevoke(a) \/ Resolve(a) \/ UseCached(a)
    \/ DirApply \/ DirAdvance \/ DirCrash

\* A node keeps syncing: the worker of `aether node serve` runs
\* sync_managed every 30 s. A reply can be lost again and again, but not
\* for ever (strong fairness of Apply and DirAdvance).
Fairness ==
    /\ \A p \in Procs : WF_vars(Fetch(p)) /\ SF_vars(Apply(p))
    /\ WF_vars(DirApply)
    /\ SF_vars(DirAdvance)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety, managed scope.                                                  *)

\* The node used at most one spelling of the control URL.
OneKey == Cardinality({k \in Keys : cur[k] > 0}) <= 1

\* A token makes at most one member.
TokenOnce == \A t \in Tokens : joins[t] <= 1

\* A row of the node's roster comes from a join event of the service for
\* that agent and that endpoint.
RosterFromLog ==
    \A a \in Agents : roster[a] # NoRow =>
        \E i \in 1..Len(log) :
            log[i] = [type |-> "joined", agent |-> a, ep |-> roster[a].ep]

\* A claim admits work only for an agent and an endpoint that joined.
AdmitIsMember ==
    \A a \in Agents, e \in Eps : Admits(a, e) =>
        \E i \in 1..Len(log) : log[i] = [type |-> "joined", agent |-> a, ep |-> e]

\* The roster after the first n events of the service.
Last(a, n) ==
    LET is == {i \in 1..n : log[i].agent = a} IN
    IF is = {} THEN 0 ELSE CHOOSE i \in is : \A j \in is : j <= i
RosterAt(n) ==
    [a \in Agents |->
        IF Last(a, n) = 0 THEN NoRow
        ELSE [st |-> IF log[Last(a, n)].type = "joined" THEN "active" ELSE "revoked",
              ep |-> cm[a].ep]]

\* With one control URL the roster is exactly the events up to the cursor:
\* no event is skipped, applied twice, or applied out of order, whatever
\* the lost replies, the stops, and the processes that sync together.
OneKeyExact ==
    OneKey => \A k \in Keys : cur[k] > 0 => roster = RosterAt(cur[k])

\* A revoke that the node applied stays, and no claim of that member admits
\* work again, whatever the spellings of the control URL that the node
\* used: a second cursor reads the old join again and changes nothing.
RevokeStays ==
    \A a \in revApplied :
        /\ roster[a].st = "revoked"
        /\ \A e \in Eps : ~Admits(a, e)

\* A cursor moves forward by one event, or not at all.
CursorForward ==
    [][\A k \in Keys : cur'[k] = cur[k] \/ cur'[k] = cur[k] + 1]_vars

(***************************************************************************)
(* A managed property that TLC finds false. It is not in                   *)
(* ManagedRoster.cfg; the comment gives the counterexample.                *)
(***************************************************************************)

\* FALSE, by design: no claim admits work after the service revoked the
\* member. The node learns of a revoke at its next sync (every 30 s), so
\* after Redeem(a), a sync and RevokeMember(a), the claim of a admits work
\* until then.
NoAdmitAfterServiceRevoke ==
    \A a \in Agents, e \in Eps : Admits(a, e) => cm[a].st # "revoked"

-----------------------------------------------------------------------------
(* Safety, directory scope. Checked against the attacker directory.        *)

\* The caller uses only a record that the trusted key signed, that has not
\* expired, and that is for the scope that the caller asked for.
UsedRecordIsTrusted == "untrusted" \notin badUse

\* The caller sends its task to the agent that it asked for, also when
\* the directory answers with a valid record of another agent.
ResolveIsForAgent == "resolved_other_agent" \notin badUse

\* The cache holds only records that the trusted key signed, each under
\* its own scope and agent id, so a record from the cache is the record of
\* the agent that the caller asked for.
CacheIsSigned ==
    \A s \in Scopes, a \in Agents :
        cache[s][a].rec # NoRec =>
            /\ cache[s][a].rec \in signed
            /\ cache[s][a].rec.agent = a /\ cache[s][a].rec.scope = s

DirCursorForward == [][dcur' = dcur \/ dcur' = dcur + 1]_vars

\* No record that was public before a revoke comes back. A publish
\* request is the signed record alone, so anyone who read such a record
\* can send it again while it has not expired.
RevokedStaysOut == \A a \in Agents : dstore[a] \notin revoked

-----------------------------------------------------------------------------
(* Liveness.                                                               *)

\* A revoke of the service reaches a node that keeps syncing.
RevokeReaches ==
    \A a \in Agents : cm[a].st = "revoked" ~> roster[a].st = "revoked"

\* A node that stays with one spelling of the control URL reads every
\* event, whatever its other cursors applied before: no process stays
\* for ever with a cursor that is behind.
EveryKeyCatchesUp ==
    \A k \in Keys, p \in Procs :
        []<>(proc[p].key # k \/ cur[k] = Len(log))

\* An honest directory's events reach a node that keeps syncing, also
\* when the log has a record that expired or that another key signed.
DirSyncCatchesUp ==
    dirHonest => \A i \in 1..MaxDirLog : (Len(dlog) >= i) ~> (dcur >= i)

=============================================================================
