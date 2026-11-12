----------------------------- MODULE Admission -----------------------------
(***************************************************************************)
(* Admission: the host side of one wire request, from the endpoint that    *)
(* QUIC authenticated to a row in the `tasks` table.                       *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/transport/server.rs  serve_connected (the endpoint  *)
(*       is `connection.remote_id()`, not a field of the frame)            *)
(*   crates/aether/src/transport/wire.rs    A2aRequest::policy_data        *)
(*   crates/aether/src/a2a_transport.rs     A2aTransport::handle, cancel   *)
(*   crates/aether/src/proof/admission.rs   ProofNode::admit, check_group, *)
(*                                          enqueue                        *)
(*   crates/aether/src/proof/mod.rs         cancel_from, revoke_grant,     *)
(*                                          apply_grant_inner              *)
(*   crates/aether-core/src/lib.rs          PolicyEngine::admit,           *)
(*                                          match_grant                    *)
(*   crates/aether/src/identity.rs          SignedClaims::verify,          *)
(*                                          verify_grant                   *)
(*   crates/aether-store/src/tasks.rs       admit_with_replay,             *)
(*                                          accept_replay_key, admit,      *)
(*                                          own_ids, count,                *)
(*                                          cancel_if_owned                *)
(*   crates/aether-store/src/access_groups.rs  access_group_allows         *)
(*                                                                         *)
(* Every endpoint is an attacker here: it can send any frame under its own *)
(* authenticated endpoint. A frame can name another endpoint, another      *)
(* agent name, the claim of another person, and the task ID, idempotency   *)
(* key and nonce of any other frame, also of its own old frames. `Honest`  *)
(* is one of the people; some properties look at it only.                  *)
(*                                                                         *)
(* A person is a pair <<name, endpoint>>. `access[p]` is the signed access *)
(* of the person: the claim that the caller keeps and the grant in the     *)
(* live policy. Both are signed in one call (`sign_access`) with one       *)
(* expiry. Signatures are perfect: a claim verifies only when the host     *)
(* signed it.                                                              *)
(*                                                                         *)
(* Each request is one atomic step. `enqueue` holds `dispatch_serial`, and *)
(* `admit_with_replay` is one IMMEDIATE transaction. The policy check and  *)
(* the group check come before it without a lock; a revocation between     *)
(* them and the transaction is the same as a revocation after the step.    *)
(*                                                                         *)
(* Abstracted away: frame syntax, the A2A task parts, the rate limiter and *)
(* the connection limit, the shutdown flag, the clock (an expiry is an     *)
(* action), event pages (see EventPaging), the scheduler (a queued task    *)
(* ends in one step; see ExecutionLease), changes of the queue limits, and *)
(* the managed and pact scopes. All requests are in the direct scope: a    *)
(* frame for another scope has no claim that fits, and `policy_data`       *)
(* refuses it. Only a cancel frame carries a scope here. The request ID    *)
(* (`tasks.request_id`, also UNIQUE) is the task ID, as in `direct.rs`.    *)
(* An ID that the node makes (`random_id`) is 128 random bits, so no       *)
(* caller sends it: NodeIds and TaskIds share nothing.                     *)
(* The replay table has one part for each endpoint and scope; with one     *)
(* scope it has one part for each endpoint. All replay entries have the    *)
(* same lifetime (REPLAY_TTL), so the oldest entry expires first, and an   *)
(* expired entry is the same as no entry: `accept_replay_key` deletes the  *)
(* expired entries before it reads. REPLAY_LIMIT has room for every frame  *)
(* that the peer rate limit lets through in REPLAY_TTL; the model does not *)
(* have the rate limit, so here an endpoint can fill its part.             *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Eps,         \* caller endpoints (QUIC keys)
    Names,       \* agent names that the owner gives to callers
    HonestName,  \* the name of the person that the last properties look at
    HonestEp,    \* and the endpoint of that person
    TaskIds,     \* task IDs (UUIDs that the caller chooses)
    NodeIds,     \* task IDs that the node makes for a row
    Keys,        \* idempotency keys
    Data,        \* the rest of a request: its fingerprint
    Nonces,      \* `exchangeId:requestNonce` values
    ReplayLimit, \* REPLAY_LIMIT
    Global,      \* QueueLimits.global
    PerPeer,     \* QueueLimits.per_peer, for one endpoint
    DirectScope, \* the scope of every task
    OtherScope   \* another scope of the node

AllPeople == Names \X Eps
Honest == <<HonestName, HonestEp>>
\* The <<name, endpoint>> pairs that the owner can invite. The owner knows
\* the honest endpoint under one name only. It can give any name to another
\* endpoint, also the name of the honest person.
People == {p \in AllPeople : p[2] = HonestEp => p[1] = HonestName}
Scopes == {DirectScope, OtherScope}

\* The signed access of a person. "revoked": the owner removed the grant
\* from the live policy; the caller still has a claim that verifies.
\* A claim and a grant that passed `expires_at` are the same as none: the
\* claim does not verify, and `match_grant` skips the grant.
AccessStates == {"none", "live", "revoked"}

\* "ended": succeeded, failed, rejected or cancelled (see ExecutionLease).
TaskStates == {"queued", "ended"}
NoTask == [owner |-> "none"]

\* The primary keys of `tasks`.
RowIds == TaskIds \cup NodeIds

\* A `tasks` row. `sent` is the task ID that the caller sent
\* (`caller_task_id`, or `id` when that is NULL). `auth` and `covered` are
\* ghosts: the authenticated endpoint of the frame that made the row, and
\* whether that endpoint had a live grant and a group membership under the
\* name of the row then.
TaskRow == [owner : Eps, agent : Names, key : Keys, data : Data,
            sent : TaskIds, state : TaskStates, auth : Eps,
            covered : BOOLEAN]

\* A request frame. `cep` and `agent` are the caller endpoint and name in
\* the authorization. `claim` is the person that the claim names; a claim
\* that the host did not sign is a claim of a person without access.
Frames == [cep : Eps, agent : Names, claim : AllPeople, id : TaskIds,
           key : Keys, data : Data, nonce : Nonces]

VARIABLES
    access,     \* the claim and the grant of each person
    members,    \* access_group_members, of any group
    tasks,      \* the `tasks` table, by `tasks.id`
    replay,     \* replay_entries of each endpoint, oldest first
    wrongAdmit, \* ghost: a frame that the host must refuse changed something
    leaked,     \* ghost: an answer showed a task of another endpoint
    badCancel   \* ghost: a cancel changed a task of another endpoint or scope

vars == <<access, members, tasks, replay, wrongAdmit, leaked, badCancel>>

Range(s) == {s[i] : i \in 1..Len(s)}

TypeOK ==
    /\ access \in [AllPeople -> AccessStates]
    /\ members \subseteq People
    /\ tasks \in [RowIds -> TaskRow \cup {NoTask}]
    /\ replay \in [Eps -> Seq(Nonces)]
    /\ \A e \in Eps : Len(replay[e]) <= ReplayLimit
    /\ wrongAdmit \in BOOLEAN
    /\ leaked \in BOOLEAN
    /\ badCancel \in BOOLEAN

Init ==
    /\ access = [p \in AllPeople |-> "none"]
    /\ members = {}
    /\ tasks = [id \in RowIds |-> NoTask]
    /\ replay = [e \in Eps |-> <<>>]
    /\ wrongAdmit = FALSE
    /\ leaked = FALSE
    /\ badCancel = FALSE

Rows == {id \in RowIds : tasks[id] # NoTask}
Queued == {id \in Rows : tasks[id].state = "queued"}
\* `count(tx, Some(peer))`: the queued tasks of one ENDPOINT.
QueuedOf(ep) == {id \in Queued : tasks[id].owner = ep}
\* The row of `ep` that the caller sent as the task ID `sid`.
OwnSent(ep, sid) ==
    {id \in Rows : tasks[id].owner = ep /\ tasks[id].sent = sid}

-----------------------------------------------------------------------------
(* The owner and the clock.                                                *)

\* admin_ui/host.rs sign_invite -> `sign_access`, then `apply_grant_inner`
\* -> `replace_grant`: a new claim and a new grant for the person.
Grant(p) ==
    /\ p \in People
    /\ access[p] # "live"
    /\ access' = [access EXCEPT ![p] = "live"]
    /\ UNCHANGED <<members, tasks, replay, wrongAdmit, leaked, badCancel>>

\* Administration::revoke_inner -> ProofNode::revoke_grant ->
\* `remove_grant`. The claim that the caller keeps is not changed.
Revoke(p) ==
    /\ access[p] = "live"
    /\ access' = [access EXCEPT ![p] = "revoked"]
    /\ UNCHANGED <<members, tasks, replay, wrongAdmit, leaked, badCancel>>

\* Time passes `expires_at` of the claim and of the grant.
Expire(p) ==
    /\ access[p] \in {"live", "revoked"}
    /\ access' = [access EXCEPT ![p] = "none"]
    /\ UNCHANGED <<members, tasks, replay, wrongAdmit, leaked, badCancel>>

\* `add_access_group_member` (access_groups.rs `join_and_apply`, the
\* groups page) and `remove_access_group_member`.
Join(p) ==
    /\ p \in People \ members
    /\ members' = members \cup {p}
    /\ UNCHANGED <<access, tasks, replay, wrongAdmit, leaked, badCancel>>

Leave(p) ==
    /\ p \in members
    /\ members' = members \ {p}
    /\ UNCHANGED <<access, tasks, replay, wrongAdmit, leaked, badCancel>>

\* REPLAY_TTL passes for the oldest entry of an endpoint.
ReplayExpires(e) ==
    /\ replay[e] # <<>>
    /\ replay' = [replay EXCEPT ![e] = Tail(@)]
    /\ UNCHANGED <<access, members, tasks, wrongAdmit, leaked, badCancel>>

\* The scheduler takes a task from the queue and ends it (ExecutionLease).
HostRuns(id) ==
    /\ id \in Queued
    /\ tasks' = [tasks EXCEPT ![id].state = "ended"]
    /\ UNCHANGED <<access, members, replay, wrongAdmit, leaked, badCancel>>

-----------------------------------------------------------------------------
(* One request frame.                                                      *)

\* `PolicyEngine::admit` for a frame that `ep` sent, in the order of the
\* code: the endpoint of the request, the subject of the claim, the claim
\* expiry and `SignedClaims::verify`, then `match_grant`, which wants a
\* grant in the live policy for the name and the endpoint of the request.
PolicyAdmits(ep, f) ==
    /\ f.cep = ep                           \* else EndpointMismatch
    /\ f.claim = <<f.agent, ep>>            \* else ScopeMismatch
    /\ access[f.claim] \in {"live", "revoked"}  \* else ClaimExpired, ClaimDenied
    /\ access[<<f.agent, f.cep>>] = "live"  \* else LocalGrantMissing, -Expired

\* `check_group` -> `access_group_allows(agent, endpoint)`.
GroupAllows(ep, f) == <<f.agent, ep>> \in members

\* Ghost: what a frame needs before it may change anything, said without
\* the fields that only the frame claims: the AUTHENTICATED endpoint has a
\* grant in the live policy and a group membership under the name of the
\* frame, and the nonce is not in the replay table of that endpoint.
Covered(name, ep) == access[<<name, ep>>] = "live" /\ <<name, ep>> \in members
MayChange(ep, f) ==
    /\ f.cep = ep
    /\ Covered(f.agent, ep)
    /\ f.nonce \notin Range(replay[ep])

\* `accept_replay_key`: the part of `ep` has room for one more nonce. A
\* full part refuses the frame; no entry goes before its time.
ReplayRoom(ep) == Len(replay[ep]) < ReplayLimit

\* The row of `ep` with this idempotency key
\* (`WHERE caller_endpoint_id=?1 AND idempotency_key=?2`).
KeyRows(ep, key) == {id \in Rows : tasks[id].owner = ep /\ tasks[id].key = key}

\* `stored == *request && fingerprint == input.request_fingerprint`.
SameRequest(id, f) ==
    /\ tasks[id].sent = f.id
    /\ tasks[id].agent = f.agent
    /\ tasks[id].data = f.data

\* `admit`: the queue is full for this frame (StoreError::Overloaded).
Full(ep) ==
    \/ Cardinality(Queued) >= Global
    \/ Cardinality(QueuedOf(ep)) >= PerPeer

\* `own_ids`: the `tasks.id` values that the row of a new frame can get.
\* It is the ID of the frame when no row has it. When a row of another
\* endpoint has it, it is an ID that the node makes. (A row of the same
\* endpoint with this ID refuses the frame before; see Request.)
StoreIds(f) ==
    IF tasks[f.id] = NoTask THEN {f.id}
    ELSE {m \in NodeIds : tasks[m] = NoTask}

\* `A2aTransport::handle` -> `ProofNode::admit` -> `enqueue` ->
\* `admit_with_replay`, and then `task_progress` of the admitted task. A
\* frame that the policy, the group or the replay table refuses changes
\* nothing, so it is no step here. An error of the transaction (a changed
\* request under a known key, a full queue, a task ID that the endpoint
\* used for another key) rolls back the replay entry too, so it is no step
\* either.
Request(ep, f) ==
    /\ PolicyAdmits(ep, f)
    /\ GroupAllows(ep, f)
    /\ f.nonce \notin Range(replay[ep])          \* else DuplicateConflict
    /\ ReplayRoom(ep)                            \* else Overloaded
    /\ replay' = [replay EXCEPT ![ep] = Append(@, f.nonce)]
    /\ IF KeyRows(ep, f.key) # {}
       THEN \* TaskAdmission::Existing; else DuplicateConflict.
            \E id \in KeyRows(ep, f.key) :
               /\ SameRequest(id, f)
               /\ leaked' = (leaked \/ tasks[id].owner # ep)
               /\ wrongAdmit' = (wrongAdmit \/ ~MayChange(ep, f)
                                 \/ tasks[id].sent # f.id
                                 \/ tasks[id].data # f.data)
               /\ UNCHANGED tasks
       ELSE \* TaskAdmission::Accepted; else Overloaded, or
            \* DuplicateConflict for a task ID of another row of `ep`.
            /\ ~Full(ep)
            /\ OwnSent(ep, f.id) = {}
            /\ \E id \in StoreIds(f) :
                  tasks' = [tasks EXCEPT ![id] =
                     [owner |-> ep, agent |-> f.agent, key |-> f.key,
                      data |-> f.data, sent |-> f.id, state |-> "queued",
                      auth |-> ep, covered |-> Covered(f.agent, ep)]]
            /\ wrongAdmit' = (wrongAdmit \/ ~MayChange(ep, f))
            /\ UNCHANGED leaked
    /\ UNCHANGED <<access, members, badCancel>>

\* `A2aTransport::cancel` -> `ProofNode::cancel_from` -> `cancel_if_owned`
\* -> `cancel`. It needs no claim and no grant: the authenticated endpoint
\* and the scope must be those of the row. `sid` is the task ID that the
\* caller sent; the row is the one of `ep` with it, whatever its
\* `tasks.id`. A task that ended stays as it is (`TaskState::allows`), and
\* a cancelled task stays cancelled.
Cancel(ep, sid, scope) ==
    \E id \in OwnSent(ep, sid) :                \* else NotFound, Unauthorized
       /\ scope = DirectScope                    \* else Unauthorized
       /\ tasks[id].state = "queued"
       /\ tasks' = [tasks EXCEPT ![id].state = "ended"]
       /\ badCancel' = (badCancel \/ tasks[id].auth # ep
                        \/ scope # DirectScope)
       /\ UNCHANGED <<access, members, replay, wrongAdmit, leaked>>

\* A first frame of the honest caller: its own name, endpoint and claim,
\* and a key and a task ID that it did not use. Another endpoint can have
\* a row with that task ID.
HonestFrame(f) ==
    /\ f.cep = HonestEp /\ f.agent = HonestName /\ f.claim = Honest
    /\ KeyRows(HonestEp, f.key) = {}
    /\ OwnSent(HonestEp, f.id) = {}

\* The host accepts a first frame of the honest caller.
HonestAdmitted ==
    \E id \in TaskIds, key \in Keys, data \in Data, nonce \in Nonces :
       LET f == [cep |-> HonestEp, agent |-> HonestName, claim |-> Honest,
                 id |-> id, key |-> key, data |-> data, nonce |-> nonce]
       IN HonestFrame(f) /\ Request(HonestEp, f)

Next ==
    \/ \E p \in People : Grant(p) \/ Revoke(p) \/ Expire(p) \/ Join(p) \/ Leave(p)
    \/ \E e \in Eps : ReplayExpires(e)
    \/ \E id \in RowIds : HostRuns(id)
    \* Any endpoint sends any frame. The policy reads only the first three
    \* fields, so they come first: TLC then drops a refused frame early.
    \/ \E e \in Eps, cep \in Eps, agent \in Names, claim \in AllPeople :
          /\ PolicyAdmits(e, [cep |-> cep, agent |-> agent, claim |-> claim])
          /\ \E id \in TaskIds, key \in Keys, data \in Data, nonce \in Nonces :
                Request(e, [cep |-> cep, agent |-> agent, claim |-> claim,
                            id |-> id, key |-> key, data |-> data,
                            nonce |-> nonce])
    \/ \E e \in Eps, id \in TaskIds, s \in Scopes : Cancel(e, id, s)

\* The scheduler keeps working, time passes for the replay entries, and
\* the honest caller sends its task again (see Outbox) with a new nonce
\* for as long as it must.
Fairness ==
    /\ \A id \in RowIds : WF_vars(HostRuns(id))
    /\ \A e \in Eps : WF_vars(ReplayExpires(e))
    /\ SF_vars(HonestAdmitted)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety.                                                                 *)

\* The owner of a row is the endpoint that QUIC authenticated for the frame
\* that made it, whatever endpoint, name or claim the frame carried.
OwnerIsAuthenticated ==
    \A id \in Rows : tasks[id].owner = tasks[id].auth

\* When a row was made, its endpoint had a grant in the live policy that
\* had not expired, and a group membership, both under the name of the row.
AdmittedWasCovered ==
    \A id \in Rows : tasks[id].covered

\* A row names only a person that the owner invited.
RowOfInvitedPerson ==
    \A id \in Rows : <<tasks[id].agent, tasks[id].owner>> \in People

\* One task for an endpoint and an idempotency key.
OneTaskPerKey ==
    \A a, b \in Rows :
        tasks[a].owner = tasks[b].owner /\ tasks[a].key = tasks[b].key => a = b

\* An endpoint has one row for a task ID that it sent, so a cancel names
\* one task.
OneTaskPerSentId ==
    \A a, b \in Rows :
        tasks[a].owner = tasks[b].owner /\ tasks[a].sent = tasks[b].sent => a = b

\* The queue holds at most `global` tasks, and at most `per_peer` tasks of
\* one endpoint.
QueueWithinLimits ==
    /\ Cardinality(Queued) <= Global
    /\ \A e \in Eps : Cardinality(QueuedOf(e)) <= PerPeer

\* A frame changes the tasks or the replay table only when the endpoint
\* that sent it has a live grant and a membership and the nonce is new. A
\* frame that gets its task back (`Existing`) has the task ID and the data
\* of that task: the same key with changed data returns nothing.
OnlyGoodFramesChange == ~wrongAdmit

\* An answer never shows the task of another endpoint: `handle` reads the
\* task ID that the admission returned, not the one in the frame.
NoForeignRead == ~leaked

\* Only the owner endpoint, in the scope of the task, cancels a task.
OnlyOwnerCancels == ~badCancel

\* A row keeps its owner, name, key, data and sent ID for ever: the same key with
\* changed data never changes the task, and no frame takes a task over.
\* Only the state changes, and an ended task does not change.
RowsAreStable ==
    [][\A id \in Rows :
         /\ tasks'[id] # NoTask
         /\ [tasks'[id] EXCEPT !.state = "queued"] = [tasks[id] EXCEPT !.state = "queued"]
         /\ tasks[id].state # "queued" => tasks'[id] = tasks[id]]_vars

\* The node has no ID left to make. Only this small model has that state.
NodeIdsUsedUp == NodeIds \subseteq Rows

\* A frame of `ep` that has everything it needs (access, a new nonce, room
\* in the replay table, a key and a task ID that `ep` did not use, room in
\* the queue) and still fails, because another endpoint has a row with its
\* task ID.
ClashRefused(ep, f) ==
    /\ PolicyAdmits(ep, f) /\ GroupAllows(ep, f)
    /\ f.nonce \notin Range(replay[ep]) /\ ReplayRoom(ep)
    /\ KeyRows(ep, f.key) = {}
    /\ OwnSent(ep, f.id) = {}
    /\ ~Full(ep)
    /\ tasks[f.id] # NoTask
    /\ StoreIds(f) = {}

\* A caller is never refused because of a task ID that another endpoint
\* used. `tasks.id` is the PRIMARY KEY of the whole table, so the row of
\* the second caller gets an ID that the node makes, and the answer and a
\* cancel still use the ID that the caller sent.
\* (The policy admits only a frame with the endpoint of the sender and its
\* claim, and the data of a new frame decides nothing, so those fields have
\* one value here.)
NoTaskIdClash ==
    \/ NodeIdsUsedUp
    \/ \A ep \in Eps, agent \in Names, id \in TaskIds, key \in Keys,
          nonce \in Nonces, data \in Data :
          ~ClashRefused(ep, [cep |-> ep, agent |-> agent,
                             claim |-> <<agent, ep>>, id |-> id, key |-> key,
                             data |-> data, nonce |-> nonce])

\* The honest caller is refused as overloaded only for its own queued
\* tasks or a full global queue: two endpoints that the owner invited
\* under one name do not share a limit.
NoSharedQuota ==
    Full(HonestEp) =>
        \/ Cardinality(Queued) >= Global
        \/ Cardinality({id \in Queued : tasks[id].auth = HonestEp}) >= PerPeer

\* A nonce stays in the replay table until REPLAY_TTL passes, so an
\* endpoint cannot send its own old frame again before that. (Only
\* ReplayExpires makes the table of an endpoint shorter.)
NoReplayBeforeExpiry ==
    [][\A e \in Eps :
         Len(replay'[e]) >= Len(replay[e])
            => Range(replay[e]) \subseteq Range(replay'[e])]_vars

-----------------------------------------------------------------------------
(* Liveness.                                                               *)

HonestCovered == Covered(HonestName, HonestEp)
HonestHasTask == \E id \in Rows : tasks[id].owner = HonestEp

\* An honest caller that keeps its grant and its group membership gets a
\* task in, whatever the other endpoints send: the queue gets room again,
\* and no frame of another endpoint uses up its nonces, its keys or its
\* task IDs.
HonestGetsIn ==
    <>[]HonestCovered => <>(HonestHasTask \/ NodeIdsUsedUp)

=============================================================================
