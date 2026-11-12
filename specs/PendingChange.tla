---------------------------- MODULE PendingChange ----------------------------
(***************************************************************************)
(* Local administration: a change that an MCP client or the owner's UI     *)
(* asks for, the rule that a high-risk change waits for a confirmation in  *)
(* the UI, and how an applied change reaches the running node and the      *)
(* store.                                                                  *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/admin.rs          Administration::submit, apply,    *)
(*       queue_pending, apply_now, finish_live, apply_live,                *)
(*       apply_live_inner, change_requires_confirmation, confirm_ui,       *)
(*       confirm_pending, check_not_stale, revoke, revoke_inner,           *)
(*       save_policy_settings, with_store; Change::is_high_risk            *)
(*   crates/aether-store/src/admin.rs    apply_change, queue_change,       *)
(*       pending_revision, record_applied_change, admin_revision,          *)
(*       revert_change, confirm_pending_change, revoke_applied_grant,      *)
(*       set_current_workspace, grant_revoked                              *)
(*   crates/aether/src/admin_socket.rs   handle: Change, RevokeGrant and   *)
(*       UpdatePolicy always get AuditActor::Mcp; the socket has no        *)
(*       confirm request                                                   *)
(*   crates/aether/src/admin_ui/advanced.rs  the confirm and the policy    *)
(*       form of the UI (AuditActor::Ui)                                   *)
(*   crates/aether/src/policy.rs         save_policy_settings_in_store,    *)
(*       stored_queue_limits                                               *)
(*   crates/aether/src/node_builder.rs   the replay at node start: grants  *)
(*       of the policy file without the revoked ones, then the stored      *)
(*       grants, the stored limit, admin.current.workspace                 *)
(*   crates/aether/src/node_service.rs   impl LiveNode for RunningNode     *)
(*                                                                         *)
(* One process holds the Administration behind a mutex, so one request     *)
(* runs at a time; `op` is that request between its store and node calls.  *)
(* The process is `aether node serve` (mode "serve", with a live node) or  *)
(* `aether node admin` (mode "admin", no live node). It can stop at any    *)
(* step and start again. A store call can fail (StoreFaults).              *)
(*                                                                         *)
(* The MCP client is hostile inside its interface: it sends any socket     *)
(* request in any order. It cannot sign a grant, but it can send again a   *)
(* signed grant that it saw before.                                        *)
(*                                                                         *)
(* Abstracted away: dry runs, field validation, the 300 s expiry as a      *)
(* clock (a pending change expires at any moment), the audit records       *)
(* other than `applied` (an audit record has no value column, so it holds  *)
(* no secret by its type), grant expiry and a stored grant that no longer  *)
(* verifies at start, the approved-workspace set (a refusal is one         *)
(* nondeterministic step), the two limit ids (one limit here), the         *)
(* `admin.<kind>.<id>` values of workspace, capability and directory       *)
(* changes (nothing reads them at start), set_relay, cancel_task, access   *)
(* groups, and the two store writes of save_policy_settings_in_store,      *)
(* which are one step here. A live revoke does not fail.                   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Ids,         \* change ids; a grant id is one of them
    FileIds,     \* ids of grants in the policy file (the owner wrote them)
    MaxOps,      \* bound on requests
    MaxRestarts, \* bound on process stops
    StoreFaults  \* whether a store write can fail

None == "none"
Actors == {"mcp", "ui"}
Kinds == {"grant", "workspace", "limit", "capability", "directory"}
\* "lo": a direct invite grant (direct::grant_is_low_risk_invite). "hi":
\* every other grant.
GVals == {"lo", "hi"}
WVals == {"w1", "w2"}
Limits == 1..3
InitLimit == 2
InitWs == "w1"

\* The value of a change: `val` for a grant or a workspace, `num` for a
\* limit. Capability and directory changes have one value.
ValsOf(kind) ==
    CASE kind = "grant" -> [val : GVals, num : {0}]
      [] kind = "workspace" -> [val : WVals, num : {0}]
      [] kind = "limit" -> [val : {None}, num : Limits]
      [] OTHER -> [val : {None}, num : {0}]

\* Change::is_high_risk.
HighRisk(kind, v) ==
    \/ kind = "grant" /\ v.val = "hi"
    \/ kind \in {"workspace", "capability"}

Pcs == {"idle", "live", "ws_cur", "record", "revert", "rv_live", "rv_record"}

VARIABLES
    mode,        \* "serve": a node runs in the process; "admin": none
    op,          \* the request that holds the Administration mutex
    durGrant,    \* store: admin.grant.<id>
    durLimit,    \* store: admin.limit.<id>, 0 when not set
    curWs,       \* store: admin.current.workspace
    pol,         \* store: workspace and queue limit of the stored policy
    revoked,     \* store: admin.revoked.<id>
    pending,     \* store: pending_changes and admin.pending_revision.<id>
    drev,        \* store: admin.revision
    mrev,        \* Administration::revision, in memory
    liveGrant,   \* node: the grants of the live policy
    liveLimit,   \* node: the queue limit
    liveWs,      \* node: the workspace
    liveCap,     \* node: a capability was published
    audits,      \* store: the `applied` audit records
    nOps, restarts,
    \* Ghosts.
    grOk,        \* grants that a UI action asked for or confirmed
    wsOk,        \* workspaces that a UI action asked for or confirmed
    limOk,       \* the highest limit that a UI action asked for
    capOk,       \* a UI action confirmed a capability
    wsPol,       \* workspaces that MCP saved with UpdatePolicy
    limPol,      \* the highest limit that MCP saved with UpdatePolicy
    dead,        \* grants revoked, with no new apply of the id since
    applies,     \* changes and revokes that reached the node
    lost,        \* a live apply whose revision was not recorded
    auditLost,   \* an apply or revoke whose audit record was not written
    faulted,     \* a store write failed since the process started
    mcpRevived,  \* MCP applied a grant id that the owner had revoked
    staleConfirmed, \* a confirm passed although a change landed after the queue
    otherConfirmed  \* the same, for a revoke or a policy save

store == <<durGrant, durLimit, curWs, pol, revoked, pending, drev, audits>>
node == <<liveGrant, liveLimit, liveWs, liveCap>>
approvals == <<grOk, wsOk, limOk, capOk, wsPol, limPol>>
flags == <<lost, auditLost, faulted, mcpRevived, staleConfirmed, otherConfirmed>>
vars == <<mode, op, store, mrev, node, nOps, restarts, approvals, dead,
          applies, flags>>

NoPending == [kind |-> None, val |-> None, num |-> 0, base |-> 0,
              expired |-> FALSE, landed |-> "no"]
Idle == [pc |-> "idle", actor |-> "ui", kind |-> "grant",
         id |-> CHOOSE i \in Ids : TRUE, val |-> None, num |-> 0,
         prevVal |-> None, prevNum |-> 0]

Max(a, b) == IF a >= b THEN a ELSE b

TypeOK ==
    /\ mode \in {"serve", "admin"}
    /\ op \in [pc : Pcs, actor : Actors, kind : Kinds, id : Ids,
               val : GVals \cup WVals \cup {None}, num : 0..3,
               prevVal : GVals \cup {None}, prevNum : 0..3]
    /\ durGrant \in [Ids -> GVals \cup {None}]
    /\ durLimit \in 0..3
    /\ curWs \in WVals \cup {None}
    /\ pol \in [ws : WVals, limit : Limits]
    /\ revoked \subseteq Ids
    /\ pending \in [Ids -> [kind : Kinds \cup {None},
                            val : GVals \cup WVals \cup {None}, num : 0..3,
                            base : 0..MaxOps, expired : BOOLEAN,
                            landed : {"no", "other", "change"}]]
    /\ drev \in 0..MaxOps /\ mrev \in 0..MaxOps
    /\ liveGrant \in [Ids -> GVals \cup {None}]
    /\ liveLimit \in Limits
    /\ liveWs \in WVals
    /\ liveCap \in BOOLEAN
    /\ audits \in 0..MaxOps /\ applies \in 0..MaxOps
    /\ nOps \in 0..MaxOps /\ restarts \in 0..MaxRestarts
    /\ grOk \subseteq Ids \X GVals
    /\ wsOk \subseteq WVals /\ wsPol \subseteq WVals
    /\ limOk \in Limits /\ limPol \in 0..3
    /\ capOk \in BOOLEAN
    /\ dead \subseteq Ids
    /\ lost \in BOOLEAN /\ auditLost \in BOOLEAN /\ faulted \in BOOLEAN
    /\ mcpRevived \in BOOLEAN /\ staleConfirmed \in BOOLEAN
    /\ otherConfirmed \in BOOLEAN

\* node_builder: the grants that a node start makes live. The policy file
\* grants without the revoked ones, then each stored grant, which replaces
\* a file grant of the same id.
StartGrant(id) ==
    IF durGrant[id] # None THEN durGrant[id]
    ELSE IF id \in FileIds /\ id \notin revoked THEN "hi"
    ELSE None

Init ==
    /\ mode = "serve"
    /\ op = Idle
    /\ durGrant = [id \in Ids |-> None]
    /\ durLimit = 0
    /\ curWs = None
    /\ pol = [ws |-> InitWs, limit |-> InitLimit]
    /\ revoked = {}
    /\ pending = [id \in Ids |-> NoPending]
    /\ drev = 0 /\ mrev = 0
    /\ liveGrant = [id \in Ids |-> IF id \in FileIds THEN "hi" ELSE None]
    /\ liveLimit = InitLimit
    /\ liveWs = InitWs
    /\ liveCap = FALSE
    /\ audits = 0 /\ applies = 0 /\ nOps = 0 /\ restarts = 0
    /\ grOk = FileIds \X {"hi"}
    /\ wsOk = {InitWs} /\ wsPol = {}
    /\ limOk = InitLimit /\ limPol = 0
    /\ capOk = FALSE
    /\ dead = {}
    /\ lost = FALSE /\ auditLost = FALSE /\ faulted = FALSE
    /\ mcpRevived = FALSE /\ staleConfirmed = FALSE /\ otherConfirmed = FALSE

Fault == StoreFaults

\* Something landed: each pending change remembers it (ghost).
Landed(p, what) ==
    [id \in Ids |->
        IF p[id].kind = None \/ p[id].landed = "change" THEN p[id]
        ELSE [p[id] EXCEPT !.landed = what]]

\* What a UI action approves (ghost).
Approve(kind, id, v) ==
    /\ grOk' = IF kind = "grant" THEN grOk \cup {<<id, v.val>>} ELSE grOk
    /\ wsOk' = IF kind = "workspace" THEN wsOk \cup {v.val} ELSE wsOk
    /\ limOk' = IF kind = "limit" THEN Max(limOk, v.num) ELSE limOk
    /\ capOk' = (capOk \/ kind = "capability")
    /\ UNCHANGED <<wsPol, limPol>>

-----------------------------------------------------------------------------
(* A change request: the socket's `Change` (MCP), or a UI action that      *)
(* applies a change (the invite page, the consent page, the hub workers).  *)

\* change_requires_confirmation. A limit from MCP is free only when the
\* live node reads a limit that is not lower.
NeedsConfirmation(actor, kind, v) ==
    /\ actor = "mcp"
    /\ IF kind = "limit"
       THEN ~(mode = "serve" /\ v.num <= liveLimit)
       ELSE HighRisk(kind, v)

\* Administration::apply -> queue_pending -> queue_change. The row's key is
\* the id alone, so a second row for the id is a store error.
Queue(actor, kind, id, v) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ NeedsConfirmation(actor, kind, v)
    /\ nOps' = nOps + 1
    /\ IF pending[id].kind = None
       THEN \/ pending' = [pending EXCEPT ![id] =
                             [kind |-> kind, val |-> v.val, num |-> v.num,
                              base |-> mrev, expired |-> FALSE, landed |-> "no"]]
            \/ Fault /\ UNCHANGED pending
       ELSE UNCHANGED pending
    /\ UNCHANGED <<mode, op, durGrant, durLimit, curWs, pol, revoked, drev,
                   audits, mrev, node, restarts, approvals, dead, applies, flags>>

\* Administration::apply -> apply_now, first half: apply_change keeps the
\* value in the store and returns the value that it replaced.
ApplyNow(actor, kind, id, v) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ ~NeedsConfirmation(actor, kind, v)
    /\ nOps' = nOps + 1
    /\ \/ /\ op' = [pc |-> "live", actor |-> actor, kind |-> kind, id |-> id,
                    val |-> v.val, num |-> v.num,
                    prevVal |-> IF kind = "grant" THEN durGrant[id] ELSE None,
                    prevNum |-> IF kind = "limit" THEN durLimit ELSE 0]
          /\ durGrant' = IF kind = "grant" THEN [durGrant EXCEPT ![id] = v.val]
                         ELSE durGrant
          /\ durLimit' = IF kind = "limit" THEN v.num ELSE durLimit
          /\ dead' = IF kind = "grant" THEN dead \ {id} ELSE dead
          /\ mcpRevived' = (mcpRevived \/ (actor = "mcp" /\ kind = "grant"
                                           /\ id \in revoked))
          /\ IF actor = "ui" THEN Approve(kind, id, v) ELSE UNCHANGED approvals
       \/ /\ Fault
          /\ UNCHANGED <<op, durGrant, durLimit, dead, mcpRevived, approvals>>
    /\ UNCHANGED <<mode, curWs, pol, revoked, pending, drev, audits, mrev, node,
                   restarts, applies, lost, auditLost, faulted, staleConfirmed,
                   otherConfirmed>>

\* Administration::confirm_ui -> confirm_pending: only the UI calls it.
\* check_not_stale compares the revision of the queue with the revision in
\* memory. confirm_pending_change refuses an expired row; else it keeps
\* the value and deletes the row in one transaction.
Confirm(id) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ pending[id].kind # None
    /\ nOps' = nOps + 1
    /\ LET p == pending[id]
           v == [val |-> p.val, num |-> p.num]
       IN IF p.base # mrev \/ p.expired
          THEN UNCHANGED <<op, durGrant, durLimit, pending, dead, approvals,
                           staleConfirmed, otherConfirmed>>
          ELSE \/ /\ op' = [pc |-> "live", actor |-> "ui", kind |-> p.kind,
                            id |-> id, val |-> p.val, num |-> p.num,
                            prevVal |-> IF p.kind = "grant" THEN durGrant[id]
                                        ELSE None,
                            prevNum |-> IF p.kind = "limit" THEN durLimit ELSE 0]
                  /\ durGrant' = IF p.kind = "grant"
                                 THEN [durGrant EXCEPT ![id] = p.val] ELSE durGrant
                  /\ durLimit' = IF p.kind = "limit" THEN p.num ELSE durLimit
                  /\ pending' = [pending EXCEPT ![id] = NoPending]
                  /\ dead' = IF p.kind = "grant" THEN dead \ {id} ELSE dead
                  /\ Approve(p.kind, id, v)
                  /\ staleConfirmed' = (staleConfirmed \/ p.landed = "change")
                  /\ otherConfirmed' = (otherConfirmed \/ p.landed = "other")
               \/ /\ Fault
                  /\ UNCHANGED <<op, durGrant, durLimit, pending, dead,
                                 approvals, staleConfirmed, otherConfirmed>>
    /\ UNCHANGED <<mode, curWs, pol, revoked, drev, audits, mrev, node,
                   restarts, applies, lost, auditLost, faulted, mcpRevived>>

\* The live value is in the node: apply_live adds one to the revision in
\* memory.
LiveDone ==
    /\ mrev' = mrev + 1
    /\ applies' = applies + 1
    /\ pending' = Landed(pending, "change")
    /\ op' = [op EXCEPT !.pc = "record"]

\* finish_live -> apply_live -> apply_live_inner. Without a live node, a
\* grant, a limit and a workspace count as applied; a directory and a
\* capability change are unsupported. The node can refuse a grant (it does
\* not verify), a workspace (not approved), a directory and a capability.
ApplyLive ==
    /\ op.pc = "live"
    /\ LET refuse == /\ op' = [op EXCEPT !.pc = "revert"]
                     /\ UNCHANGED <<mrev, applies, pending, node>>
       IN CASE op.kind = "grant" ->
                 \/ /\ LiveDone
                    /\ liveGrant' = IF mode = "serve"
                                    THEN [liveGrant EXCEPT ![op.id] = op.val]
                                    ELSE liveGrant
                    /\ UNCHANGED <<liveLimit, liveWs, liveCap>>
                 \/ mode = "serve" /\ refuse
            [] op.kind = "limit" ->
                 /\ LiveDone
                 /\ liveLimit' = IF mode = "serve" THEN op.num ELSE liveLimit
                 /\ UNCHANGED <<liveGrant, liveWs, liveCap>>
            [] op.kind = "workspace" ->
                 \/ /\ liveWs' = IF mode = "serve" THEN op.val ELSE liveWs
                    /\ op' = [op EXCEPT !.pc = "ws_cur"]
                    /\ UNCHANGED <<mrev, applies, pending, liveGrant, liveLimit,
                                   liveCap>>
                 \/ mode = "serve" /\ refuse
            [] op.kind = "capability" ->
                 \/ /\ mode = "serve"
                    /\ LiveDone
                    /\ liveCap' = TRUE
                    /\ UNCHANGED <<liveGrant, liveLimit, liveWs>>
                 \/ refuse
            [] OTHER ->
                 \/ mode = "serve" /\ LiveDone /\ UNCHANGED node
                 \/ refuse
    /\ UNCHANGED <<mode, durGrant, durLimit, curWs, pol, revoked, drev, audits,
                   nOps, restarts, approvals, dead, flags>>

\* apply_live_inner, the workspace branch after the node moved:
\* set_current_workspace. When it fails, finish_live reverts the store
\* value, but the node keeps the new workspace.
SetCurrentWorkspace ==
    /\ op.pc = "ws_cur"
    /\ \/ /\ curWs' = op.val
          /\ LiveDone
          /\ UNCHANGED faulted
       \/ /\ Fault
          /\ faulted' = TRUE
          /\ op' = [op EXCEPT !.pc = "revert"]
          /\ UNCHANGED <<curWs, mrev, applies, pending>>
    /\ UNCHANGED <<mode, durGrant, durLimit, pol, revoked, drev, audits, node,
                   nOps, restarts, approvals, dead, lost, auditLost, mcpRevived,
                   staleConfirmed, otherConfirmed>>

\* finish_live, the success branch: record_applied_change writes the new
\* revision and the audit record in one transaction.
Record ==
    /\ op.pc = "record"
    /\ op' = Idle
    /\ \/ /\ drev' = mrev
          /\ audits' = audits + 1
          /\ UNCHANGED <<lost, auditLost, faulted>>
       \/ /\ Fault
          /\ lost' = TRUE /\ auditLost' = TRUE /\ faulted' = TRUE
          /\ UNCHANGED <<drev, audits>>
    /\ UNCHANGED <<mode, durGrant, durLimit, curWs, pol, revoked, pending, mrev,
                   node, nOps, restarts, approvals, dead, applies, mcpRevived,
                   staleConfirmed, otherConfirmed>>

\* finish_live, the failure branch: revert_change puts back the value that
\* the change replaced.
Revert ==
    /\ op.pc = "revert"
    /\ op' = Idle
    /\ \/ /\ durGrant' = IF op.kind = "grant"
                         THEN [durGrant EXCEPT ![op.id] = op.prevVal]
                         ELSE durGrant
          /\ durLimit' = IF op.kind = "limit" THEN op.prevNum ELSE durLimit
          /\ UNCHANGED faulted
       \/ /\ Fault
          /\ faulted' = TRUE
          /\ UNCHANGED <<durGrant, durLimit>>
    /\ UNCHANGED <<mode, curWs, pol, revoked, pending, drev, audits, mrev, node,
                   nOps, restarts, approvals, dead, applies, lost, auditLost,
                   mcpRevived, staleConfirmed, otherConfirmed>>

-----------------------------------------------------------------------------
(* Revoke and the policy settings.                                         *)

\* Administration::revoke -> revoke_inner, first half:
\* revoke_applied_grant removes the stored grant and keeps the mark. A
\* process without a node refuses. The socket's RevokeGrant (MCP) asks
\* first that the live policy has the grant.
Revoke(actor, id) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ mode = "serve"
    /\ actor = "mcp" => liveGrant[id] # None
    /\ nOps' = nOps + 1
    /\ \/ /\ revoked' = revoked \cup {id}
          /\ durGrant' = [durGrant EXCEPT ![id] = None]
          /\ op' = [Idle EXCEPT !.pc = "rv_live", !.actor = actor, !.id = id]
       \/ Fault /\ UNCHANGED <<revoked, durGrant, op>>
    /\ UNCHANGED <<mode, durLimit, curWs, pol, pending, drev, audits, mrev, node,
                   restarts, approvals, dead, applies, flags>>

\* revoke_inner: LiveNode::revoke_grant. It does not change the revision.
RevokeLive ==
    /\ op.pc = "rv_live"
    /\ liveGrant' = [liveGrant EXCEPT ![op.id] = None]
    /\ dead' = dead \cup {op.id}
    /\ applies' = applies + 1
    /\ pending' = Landed(pending, "other")
    /\ op' = [op EXCEPT !.pc = "rv_record"]
    /\ UNCHANGED <<mode, durGrant, durLimit, curWs, pol, revoked, drev, audits,
                   mrev, liveLimit, liveWs, liveCap, nOps, restarts, approvals,
                   flags>>

\* revoke_inner: record_change.
RevokeRecord ==
    /\ op.pc = "rv_record"
    /\ op' = Idle
    /\ \/ audits' = audits + 1 /\ UNCHANGED <<auditLost, faulted>>
       \/ Fault /\ auditLost' = TRUE /\ faulted' = TRUE /\ UNCHANGED audits
    /\ UNCHANGED <<mode, durGrant, durLimit, curWs, pol, revoked, pending, drev,
                   mrev, node, nOps, restarts, approvals, dead, applies, lost,
                   mcpRevived, staleConfirmed, otherConfirmed>>

\* The socket's UpdatePolicy (MCP, the tool aether.settings.update_policy)
\* and the UI's policy form -> Administration::save_policy_settings ->
\* save_policy_settings_in_store. It keeps the workspace and the limit in
\* the stored policy and removes the stored limit of `aether.change`. No
\* actor needs a confirmation. The running node keeps its settings until
\* it starts again.
UpdatePolicy(actor, w, l) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ nOps' = nOps + 1
    /\ \/ /\ pol' = [ws |-> w, limit |-> l]
          /\ durLimit' = 0
          /\ applies' = applies + 1
          /\ pending' = Landed(pending, "other")
          /\ IF actor = "ui"
             THEN /\ wsOk' = wsOk \cup {w} /\ limOk' = Max(limOk, l)
                  /\ UNCHANGED <<wsPol, limPol>>
             ELSE /\ wsPol' = wsPol \cup {w} /\ limPol' = Max(limPol, l)
                  /\ UNCHANGED <<wsOk, limOk>>
          /\ \/ audits' = audits + 1 /\ UNCHANGED <<auditLost, faulted>>
             \/ Fault /\ auditLost' = TRUE /\ faulted' = TRUE /\ UNCHANGED audits
       \/ /\ Fault
          /\ UNCHANGED <<pol, durLimit, applies, pending, wsOk, limOk, wsPol,
                         limPol, audits, auditLost, faulted>>
    /\ UNCHANGED <<mode, op, durGrant, curWs, revoked, drev, mrev, node,
                   restarts, grOk, capOk, dead, lost, mcpRevived,
                   staleConfirmed, otherConfirmed>>

-----------------------------------------------------------------------------
(* The environment.                                                        *)

\* 300 s pass after the queue. No function deletes an expired row.
Expire(id) ==
    /\ pending[id].kind # None /\ ~pending[id].expired
    /\ pending' = [pending EXCEPT ![id].expired = TRUE]
    /\ UNCHANGED <<mode, op, durGrant, durLimit, curWs, pol, revoked, drev,
                   audits, mrev, node, nOps, restarts, approvals, dead, applies,
                   flags>>

\* The process stops at any step and starts again, with or without a
\* node. Administration::with_store reads the stored revision. A node
\* start replays the store (node_builder). A stop between the live apply
\* and record_applied_change loses the new revision and the audit record.
Restart(m) ==
    /\ restarts < MaxRestarts
    /\ restarts' = restarts + 1
    /\ mode' = m
    /\ op' = Idle
    /\ mrev' = drev
    /\ lost' = (lost \/ op.pc = "record")
    /\ auditLost' = (auditLost \/ op.pc \in {"record", "rv_record"})
    /\ faulted' = FALSE
    /\ liveGrant' = [id \in Ids |-> IF m = "serve" THEN StartGrant(id) ELSE None]
    /\ liveLimit' = IF durLimit # 0 THEN durLimit ELSE pol.limit
    /\ liveWs' = IF curWs # None THEN curWs ELSE pol.ws
    /\ liveCap' = FALSE
    /\ UNCHANGED <<store, nOps, approvals, dead, applies, mcpRevived,
                   staleConfirmed, otherConfirmed>>

Next ==
    \/ \E actor \in Actors, kind \in Kinds, id \in Ids :
         \E v \in ValsOf(kind) :
            Queue(actor, kind, id, v) \/ ApplyNow(actor, kind, id, v)
    \/ \E id \in Ids : Confirm(id) \/ Expire(id)
    \/ \E actor \in Actors, id \in Ids : Revoke(actor, id)
    \/ \E actor \in Actors, w \in WVals, l \in Limits : UpdatePolicy(actor, w, l)
    \/ ApplyLive \/ SetCurrentWorkspace \/ Record \/ Revert
    \/ RevokeLive \/ RevokeRecord
    \/ \E m \in {"serve", "admin"} : Restart(m)

\* A request that started goes on to its end.
Fairness ==
    /\ WF_vars(ApplyLive) /\ WF_vars(SetCurrentWorkspace) /\ WF_vars(Record)
    /\ WF_vars(Revert) /\ WF_vars(RevokeLive) /\ WF_vars(RevokeRecord)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety.                                                                 *)

\* A high-risk grant is in the store or in the live policy only when a UI
\* action asked for or confirmed that grant: the same id and value. The
\* socket has no confirm request, and the actor of a socket request is
\* always MCP, so an MCP client cannot lower the risk of its change.
HighRiskGrantNeedsUi ==
    \A id \in Ids :
        (durGrant[id] = "hi" \/ liveGrant[id] = "hi") => <<id, "hi">> \in grOk

\* A capability is published only after a UI confirmation.
CapabilityNeedsUi == liveCap => capOk

\* A pending change is not in force: its value reaches the store and the
\* node only through Confirm. The workspace that a change made live, and
\* the stored limit, come from a UI action, or the limit is not higher
\* than a limit that the node already had.
ChangeNeedsUi ==
    /\ curWs # None => curWs \in wsOk
    /\ durLimit <= Max(limOk, limPol)

\* The live workspace and limit come from a UI action or from a saved
\* policy (UpdatePolicy), from the UI or from MCP.
LiveFromUiOrPolicy ==
    /\ liveWs \in wsOk \cup wsPol
    /\ liveLimit <= Max(limOk, limPol)

\* After each complete request, the node and the store agree, unless a
\* store write failed since the process started.
Agree ==
    /\ \A id \in Ids : liveGrant[id] = StartGrant(id)
    /\ curWs # None => liveWs = curWs
    /\ durLimit # 0 => liveLimit = durLimit
LiveMatchesStore ==
    (mode = "serve" /\ op.pc = "idle" /\ ~faulted) => Agree

\* A revoked grant is not live and not in the store, and a node start does
\* not bring it back, until someone applies a grant with that id again.
RevokedStaysOut ==
    \A id \in dead : liveGrant[id] = None /\ durGrant[id] = None

\* A confirmation is refused when another change was applied after the
\* queue, as long as each applied change recorded its revision.
StaleRefusedWhileRecorded == staleConfirmed => lost

\* Each change that reached the node has one `applied` audit record, as
\* long as each record was written.
AuditedWhileRecorded ==
    /\ audits <= applies
    /\ (op.pc = "idle" /\ ~auditLost) => audits = applies

\* A pending row that expired never changes (but for the ghost `landed`)
\* and never leaves the table:
\* only confirm_pending_change deletes a row, and it refuses this one. The
\* row's key is the id, so no change with this id can be queued again (see
\* Queue).
ExpiredRowStays ==
    [][\A id \in Ids :
         (pending[id].kind # None /\ pending[id].expired)
            => [pending'[id] EXCEPT !.landed = "no"]
                 = [pending[id] EXCEPT !.landed = "no"]]_vars

(***************************************************************************)
(* Properties that TLC finds false. They are not in PendingChange.cfg;     *)
(* each comment gives the counterexample.                                  *)
(***************************************************************************)

\* FALSE: the live workspace and limit come from a UI action only.
\* UpdatePolicy("mcp", w2, 3), Restart("serve"): the node starts in w2
\* with the limit 3. admin_socket.rs gives UpdatePolicy to
\* save_policy_settings with no confirmation, and node_builder reads the
\* stored policy at start. An `aether.change` for the same workspace or
\* limit waits for the UI.
LiveFromUi ==
    /\ liveWs \in wsOk
    /\ liveLimit <= limOk

\* FALSE: a stale confirmation is always refused. MCP queues a change at
\* revision 0; the UI applies another change; the process stops after
\* apply_live and before record_applied_change; it starts again with the
\* stored revision 0, and Confirm passes. The store value of the other
\* change was written first, so a node start makes it live.
StaleRefused == ~staleConfirmed

\* FALSE (by design): a revoke or a policy save after the queue makes the
\* confirmation stale. Neither changes the revision.
StaleSeesRevoke == ~otherConfirmed

\* FALSE (by design): only the owner brings back a grant id that the owner
\* revoked. The UI applies a low-risk grant, the UI revokes it, and MCP
\* sends the same signed grant in a Change: it is low risk, so it applies.
\* The revoked mark only stops a grant of the policy file.
NoMcpRevival == ~mcpRevived

\* FALSE with a store fault: the node and the store agree after each
\* complete request. (1) A workspace change moves the node, then
\* set_current_workspace fails, and revert_change restores the store only.
\* (2) The node refuses a grant and revert_change fails: the store keeps
\* the grant, the caller reads an error, and the next start makes it live.
LiveMatchesStoreAlways == (mode = "serve" /\ op.pc = "idle") => Agree

\* FALSE with a store fault or a stop: each applied change has an audit
\* record. record_applied_change runs after the node has the change.
Audited == op.pc = "idle" => audits = applies

(***************************************************************************)
(* Liveness.                                                               *)
(***************************************************************************)

\* A request that holds the mutex ends: it is recorded or reverted, or the
\* process stops.
RequestEnds == (op.pc # "idle") ~> (op.pc = "idle")

=============================================================================
