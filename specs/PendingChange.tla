---------------------------- MODULE PendingChange ----------------------------
(***************************************************************************)
(* Local administration: a change that an MCP client or the owner's UI     *)
(* asks for, the rule that a high-risk change waits for a confirmation in  *)
(* the UI, and how an applied change reaches the running node and the      *)
(* store.                                                                  *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/admin.rs          Administration::submit, apply,    *)
(*       settle, queue_pending, apply_now, check_live, finish_live,        *)
(*       apply_live, change_requires_confirmation, policy_is_wider,        *)
(*       confirm_ui, confirm_pending, check_not_stale, revoke,             *)
(*       revoke_inner, save_policy_settings, with_store;                   *)
(*       Change::is_high_risk                                              *)
(*   crates/aether-store/src/admin.rs    apply_change, keep_change,        *)
(*       queue_change, delete_expired, pending_revision, admin_revision,   *)
(*       revert_change, confirm_pending_change, revoke_applied_grant,      *)
(*       record_change, fail_change, grant_revoked                         *)
(*   crates/aether/src/admin_socket.rs   handle: Change, RevokeGrant and   *)
(*       UpdatePolicy always get AuditActor::Mcp; the socket has no        *)
(*       confirm request                                                   *)
(*   crates/aether/src/admin_ui/advanced.rs  the confirm and the policy    *)
(*       form of the UI (AuditActor::Ui)                                   *)
(*   crates/aether/src/policy.rs         save_policy_settings_in_store,    *)
(*       stored_queue_limits, stored_running_limit                         *)
(*   crates/aether/src/node_builder.rs   the replay at node start: grants  *)
(*       of the policy file without the revoked ones, then the stored      *)
(*       grants, the stored limits, admin.current.workspace                *)
(*   crates/aether/src/node_service.rs   impl LiveNode for RunningNode     *)
(*   crates/aether/src/proof/mod.rs      ProofNode::set_queue_limit,       *)
(*       set_running_limit                                                 *)
(*                                                                         *)
(* There are two limits (`QueueLimitId`): "queue", which stands for the    *)
(* two limits of the waiting tasks (they take the same path and behave     *)
(* alike), and "running", the number of tasks that run at once. Each has   *)
(* its own stored value, live value and value in the stored policy. A      *)
(* limit change names its limit; the policy settings carry both.           *)
(*                                                                         *)
(* One process holds the Administration behind a mutex, so one request     *)
(* runs at a time; `op` is that request between its store and node calls.  *)
(* The process is `aether node serve` (mode "serve", with a live node) or  *)
(* `aether node admin` (mode "admin", no live node). It can stop at any    *)
(* step and start again. A store call can fail (StoreFaults).              *)
(*                                                                         *)
(* A change is one store transaction (the value, the current workspace of  *)
(* a workspace change, the new revision and the audit record) and then     *)
(* the node call. When the node refuses, a second transaction puts the     *)
(* old value back. When that fails, the Administration keeps the undo and  *)
(* runs no request before it worked (pc "undo").                           *)
(*                                                                         *)
(* The MCP client is hostile inside its interface: it sends any socket     *)
(* request in any order. It cannot sign a grant, but it can send again a   *)
(* signed grant that it saw before.                                        *)
(*                                                                         *)
(* Abstracted away: dry runs, field validation, the 300 s expiry as a      *)
(* clock (a pending change expires at any moment), the audit records       *)
(* other than `applied` (an audit record has no value column, so it holds  *)
(* no secret by its type), grant expiry and a stored grant that no longer  *)
(* verifies at start, the range of a limit (the running limit is 1 to 32;  *)
(* here every limit is 1 to 3), the `admin.<kind>.<id>` values of workspace, *)
(* capability, directory and policy changes (nothing reads them at         *)
(* start), set_relay, cancel_task, access groups, and the store writes of  *)
(* save_policy_settings_in_store, which are one step here. A live revoke   *)
(* does not fail.                                                          *)
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
\* "policy": the settings of UpdatePolicy, which wait as a pending change
\* when they come from MCP and are wider than the stored policy.
Kinds == {"grant", "workspace", "limit", "capability", "directory", "policy"}
\* "lo": a direct invite grant (direct::grant_is_low_risk_invite). "hi":
\* every other grant.
GVals == {"lo", "hi"}
WVals == {"w1", "w2"}
LimIds == {"queue", "running"}
Limits == 1..3
\* The limits of a new node: two waiting tasks, one running task (the
\* default of `running_tasks`).
InitLimit == [l \in LimIds |-> IF l = "queue" THEN 2 ELSE 1]
NoLimits == [l \in LimIds |-> 0]
InitWs == "w1"

\* The value of a change: `val` for a grant or a workspace, `num` for the
\* limit that `lim` names, `lims` (both limits) for the policy settings.
\* Capability and directory changes have one value.
ValsOf(kind) ==
    CASE kind = "grant" -> [val : GVals, num : {0}, lim : {None}, lims : {NoLimits}]
      [] kind = "workspace" -> [val : WVals, num : {0}, lim : {None}, lims : {NoLimits}]
      [] kind = "limit" -> [val : {None}, num : Limits, lim : LimIds, lims : {NoLimits}]
      [] kind = "policy" -> [val : WVals, num : {0}, lim : {None},
                             lims : [LimIds -> Limits]]
      [] OTHER -> [val : {None}, num : {0}, lim : {None}, lims : {NoLimits}]

\* Change::is_high_risk.
HighRisk(kind, v) ==
    \/ kind = "grant" /\ v.val = "hi"
    \/ kind \in {"workspace", "capability"}

Pcs == {"idle", "live", "revert", "undo", "rv_live"}

VARIABLES
    mode,        \* "serve": a node runs in the process; "admin": none
    op,          \* the request that holds the Administration mutex
    durGrant,    \* store: admin.grant.<id>
    durLimit,    \* store: admin.limit.<id> of each limit, 0 when not set
    curWs,       \* store: admin.current.workspace
    pol,         \* store: workspace and limits of the stored policy
    revoked,     \* store: admin.revoked.<id>
    pending,     \* store: pending_changes and admin.pending_revision.<id>
    drev,        \* store: admin.revision
    mrev,        \* Administration::revision, in memory
    liveGrant,   \* node: the grants of the live policy
    liveLimit,   \* node: each limit
    liveWs,      \* node: the workspace
    liveCap,     \* node: a capability was published
    audits,      \* store: the `applied` audit records
    nOps, restarts,
    \* Ghosts.
    grOk,        \* grants that a UI action asked for or confirmed
    wsOk,        \* workspaces that a UI action asked for or confirmed
    limOk,       \* the highest value of each limit that a UI action asked for
    capOk,       \* a UI action confirmed a capability
    dead,        \* grants revoked, with no new apply of the id since
    applies,     \* changes, revokes and policy saves that took effect
    mcpRevived,  \* MCP applied a grant id that the owner had revoked
    staleConfirmed, \* a confirm passed although a change landed after the queue
    otherConfirmed, \* the same, for a revoke or a policy save
    expiredApplied  \* a confirm applied an expired change

store == <<durGrant, durLimit, curWs, pol, revoked, pending, drev, audits>>
node == <<liveGrant, liveLimit, liveWs, liveCap>>
approvals == <<grOk, wsOk, limOk, capOk>>
flags == <<mcpRevived, staleConfirmed, otherConfirmed, expiredApplied>>
vars == <<mode, op, store, mrev, node, nOps, restarts, approvals, dead,
          applies, flags>>

NoPending == [kind |-> None, val |-> None, num |-> 0, lim |-> None,
              lims |-> NoLimits, base |-> 0, expired |-> FALSE, landed |-> "no"]
Idle == [pc |-> "idle", actor |-> "ui", kind |-> "grant",
         id |-> CHOOSE i \in Ids : TRUE, val |-> None, num |-> 0,
         lim |-> None, lims |-> NoLimits,
         prevVal |-> None, prevNum |-> 0, prevCur |-> None]

Max(a, b) == IF a >= b THEN a ELSE b

TypeOK ==
    /\ mode \in {"serve", "admin"}
    /\ op \in [pc : Pcs, actor : Actors, kind : Kinds, id : Ids,
               val : GVals \cup WVals \cup {None}, num : 0..3,
               lim : LimIds \cup {None}, lims : [LimIds -> 0..3],
               prevVal : GVals \cup {None}, prevNum : 0..3,
               prevCur : WVals \cup {None}]
    /\ durGrant \in [Ids -> GVals \cup {None}]
    /\ durLimit \in [LimIds -> 0..3]
    /\ curWs \in WVals \cup {None}
    /\ pol \in [ws : WVals, limit : [LimIds -> Limits]]
    /\ revoked \subseteq Ids
    /\ pending \in [Ids -> [kind : Kinds \cup {None},
                            val : GVals \cup WVals \cup {None}, num : 0..3,
                            lim : LimIds \cup {None}, lims : [LimIds -> 0..3],
                            base : 0..MaxOps, expired : BOOLEAN,
                            landed : {"no", "other", "change"}]]
    /\ drev \in 0..MaxOps /\ mrev \in 0..MaxOps
    /\ liveGrant \in [Ids -> GVals \cup {None}]
    /\ liveLimit \in [LimIds -> Limits]
    /\ liveWs \in WVals
    /\ liveCap \in BOOLEAN
    /\ audits \in 0..MaxOps /\ applies \in 0..MaxOps
    /\ nOps \in 0..MaxOps /\ restarts \in 0..MaxRestarts
    /\ grOk \subseteq Ids \X GVals
    /\ wsOk \subseteq WVals
    /\ limOk \in [LimIds -> Limits]
    /\ capOk \in BOOLEAN
    /\ dead \subseteq Ids
    /\ mcpRevived \in BOOLEAN /\ staleConfirmed \in BOOLEAN
    /\ otherConfirmed \in BOOLEAN /\ expiredApplied \in BOOLEAN

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
    /\ durLimit = NoLimits
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
    /\ wsOk = {InitWs}
    /\ limOk = InitLimit
    /\ capOk = FALSE
    /\ dead = {}
    /\ mcpRevived = FALSE /\ staleConfirmed = FALSE
    /\ otherConfirmed = FALSE /\ expiredApplied = FALSE

Fault == StoreFaults

\* Something landed: each pending change remembers it (ghost).
Landed(p, what) ==
    [id \in Ids |->
        IF p[id].kind = None \/ p[id].landed = "change" THEN p[id]
        ELSE [p[id] EXCEPT !.landed = what]]

\* What a UI action approves (ghost).
Approve(kind, id, v) ==
    /\ grOk' = IF kind = "grant" THEN grOk \cup {<<id, v.val>>} ELSE grOk
    /\ wsOk' = IF kind \in {"workspace", "policy"} THEN wsOk \cup {v.val}
               ELSE wsOk
    /\ limOk' = CASE kind = "limit" -> [limOk EXCEPT ![v.lim] = Max(@, v.num)]
                  [] kind = "policy" -> [l \in LimIds |-> Max(limOk[l], v.lims[l])]
                  [] OTHER -> limOk
    /\ capOk' = (capOk \/ kind = "capability")

-----------------------------------------------------------------------------
(* A change request: the socket's `Change` and `UpdatePolicy` (MCP), or a  *)
(* UI action that applies a change (the invite page, the consent page,     *)
(* the hub workers, the policy form).                                      *)

\* change_requires_confirmation. A limit from MCP is free only when the
\* live node reads a value of that limit that is not lower. Policy settings
\* from MCP are free only when they keep the workspace and raise no limit
\* (policy_is_wider: each limit of `QueueLimitId::ALL`, the running limit
\* too).
NeedsConfirmation(actor, kind, v) ==
    /\ actor = "mcp"
    /\ CASE kind = "limit" -> ~(mode = "serve" /\ v.num <= liveLimit[v.lim])
         [] kind = "policy" -> \/ v.val # pol.ws
                               \/ \E l \in LimIds : v.lims[l] > pol.limit[l]
         [] OTHER -> HighRisk(kind, v)

\* Administration::apply or save_policy_settings -> queue_pending ->
\* queue_change. The row's key is the id alone, so a second row for the id
\* is a store error; delete_expired first removes a row that expired.
Queue(actor, kind, id, v) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ NeedsConfirmation(actor, kind, v)
    /\ nOps' = nOps + 1
    /\ IF pending[id].kind = None \/ pending[id].expired
       THEN \/ pending' = [pending EXCEPT ![id] =
                             [kind |-> kind, val |-> v.val, num |-> v.num,
                              lim |-> v.lim, lims |-> v.lims,
                              base |-> mrev, expired |-> FALSE, landed |-> "no"]]
            \/ Fault /\ UNCHANGED pending
       ELSE UNCHANGED pending
    /\ UNCHANGED <<mode, op, durGrant, durLimit, curWs, pol, revoked, drev,
                   audits, mrev, node, restarts, approvals, dead, applies, flags>>

\* apply_change and confirm_pending_change -> keep_change: one transaction
\* keeps the value, the current workspace of a workspace change, the new
\* revision and the audit record. The revision in memory follows at once.
\* `p` is the pending table without the row that a confirm removes.
Keep(actor, kind, id, v, p) ==
    /\ op' = [pc |-> "live", actor |-> actor, kind |-> kind, id |-> id,
              val |-> v.val, num |-> v.num, lim |-> v.lim, lims |-> v.lims,
              prevVal |-> IF kind = "grant" THEN durGrant[id] ELSE None,
              prevNum |-> IF kind = "limit" THEN durLimit[v.lim] ELSE 0,
              prevCur |-> curWs]
    /\ durGrant' = IF kind = "grant" THEN [durGrant EXCEPT ![id] = v.val]
                   ELSE durGrant
    /\ durLimit' = IF kind = "limit" THEN [durLimit EXCEPT ![v.lim] = v.num]
                   ELSE durLimit
    /\ curWs' = IF kind = "workspace" THEN v.val ELSE curWs
    /\ drev' = mrev + 1 /\ mrev' = mrev + 1
    /\ audits' = audits + 1
    /\ pending' = Landed(p, "change")
    /\ dead' = IF kind = "grant" THEN dead \ {id} ELSE dead

\* check_live: the running node refuses a workspace (not approved, or not
\* a folder) before the store has it.
WorkspaceRefused(kind) == kind = "workspace" /\ mode = "serve"

\* Administration::apply -> apply_now, up to the store transaction. Policy
\* settings that need no confirmation go to SavePolicy.
ApplyNow(actor, kind, id, v) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ kind # "policy"
    /\ ~NeedsConfirmation(actor, kind, v)
    /\ nOps' = nOps + 1
    /\ \/ /\ Keep(actor, kind, id, v, pending)
          /\ mcpRevived' = (mcpRevived \/ (actor = "mcp" /\ kind = "grant"
                                           /\ id \in revoked))
          /\ IF actor = "ui" THEN Approve(kind, id, v) ELSE UNCHANGED approvals
       \/ /\ Fault \/ WorkspaceRefused(kind)
          /\ UNCHANGED <<op, durGrant, durLimit, curWs, drev, mrev, audits,
                         pending, dead, mcpRevived, approvals>>
    /\ UNCHANGED <<mode, pol, revoked, node, restarts, applies,
                   staleConfirmed, otherConfirmed, expiredApplied>>

\* Administration::confirm_ui -> confirm_pending: only the UI calls it.
\* check_not_stale compares the revision of the queue with the revision in
\* memory. confirm_pending_change refuses an expired row and removes it;
\* else it keeps the value and deletes the row in one transaction.
Confirm(id) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ pending[id].kind # None
    /\ nOps' = nOps + 1
    /\ LET p == pending[id]
           v == [val |-> p.val, num |-> p.num, lim |-> p.lim, lims |-> p.lims]
           nothing == UNCHANGED <<op, durGrant, durLimit, curWs, drev, mrev,
                                  audits, pending, dead, approvals,
                                  staleConfirmed, otherConfirmed,
                                  expiredApplied>>
       IN IF p.base # mrev
          THEN nothing
          ELSE IF p.expired
          THEN \/ nothing
               \/ /\ ~WorkspaceRefused(p.kind)
                  /\ pending' = [pending EXCEPT ![id] = NoPending]
                  /\ UNCHANGED <<op, durGrant, durLimit, curWs, drev, mrev,
                                 audits, dead, approvals, staleConfirmed,
                                 otherConfirmed, expiredApplied>>
          ELSE \/ /\ Keep("ui", p.kind, id, v,
                          [pending EXCEPT ![id] = NoPending])
                  /\ Approve(p.kind, id, v)
                  /\ staleConfirmed' = (staleConfirmed \/ p.landed = "change")
                  /\ otherConfirmed' = (otherConfirmed \/ p.landed = "other")
                  /\ expiredApplied' = (expiredApplied \/ p.expired)
               \/ (Fault \/ WorkspaceRefused(p.kind)) /\ nothing
    /\ UNCHANGED <<mode, pol, revoked, node, restarts, applies, mcpRevived>>

\* The node, or the stored policy, has the change.
LiveDone ==
    /\ applies' = applies + 1
    /\ op' = Idle

\* finish_live -> apply_live. Without a live node, a grant, a limit and a
\* workspace count as applied; a directory and a capability change are
\* unsupported. The node can refuse a grant (it does not verify), a
\* workspace (the folder went away after check_live), a directory and a
\* capability. Policy settings go to the stored policy
\* (save_policy_settings_in_store), which also removes the stored limit;
\* the running node keeps its settings until it starts again.
ApplyLive ==
    /\ op.pc = "live"
    /\ LET refuse == /\ op' = [op EXCEPT !.pc = "revert"]
                     /\ UNCHANGED <<applies, node, pol, durLimit>>
       IN CASE op.kind = "grant" ->
                 \/ /\ LiveDone
                    /\ liveGrant' = IF mode = "serve"
                                    THEN [liveGrant EXCEPT ![op.id] = op.val]
                                    ELSE liveGrant
                    /\ UNCHANGED <<liveLimit, liveWs, liveCap, pol, durLimit>>
                 \/ mode = "serve" /\ refuse
            [] op.kind = "limit" ->
                 /\ LiveDone
                 /\ liveLimit' = IF mode = "serve"
                                 THEN [liveLimit EXCEPT ![op.lim] = op.num]
                                 ELSE liveLimit
                 /\ UNCHANGED <<liveGrant, liveWs, liveCap, pol, durLimit>>
            [] op.kind = "workspace" ->
                 \/ /\ LiveDone
                    /\ liveWs' = IF mode = "serve" THEN op.val ELSE liveWs
                    /\ UNCHANGED <<liveGrant, liveLimit, liveCap, pol, durLimit>>
                 \/ mode = "serve" /\ refuse
            [] op.kind = "capability" ->
                 \/ /\ mode = "serve"
                    /\ LiveDone
                    /\ liveCap' = TRUE
                    /\ UNCHANGED <<liveGrant, liveLimit, liveWs, pol, durLimit>>
                 \/ refuse
            [] op.kind = "policy" ->
                 \/ /\ LiveDone
                    /\ pol' = [ws |-> op.val, limit |-> op.lims]
                    /\ durLimit' = NoLimits
                    /\ UNCHANGED node
                 \/ Fault /\ refuse
            [] OTHER ->
                 \/ mode = "serve" /\ LiveDone /\ UNCHANGED <<node, pol, durLimit>>
                 \/ refuse
    /\ UNCHANGED <<mode, durGrant, curWs, revoked, pending, drev, audits, mrev,
                   nOps, restarts, approvals, dead, flags>>

\* revert_change: one transaction puts back the value and the current
\* workspace that the change replaced and marks the audit record as
\* failed. The revision stays.
PutBack ==
    /\ durGrant' = IF op.kind = "grant"
                   THEN [durGrant EXCEPT ![op.id] = op.prevVal]
                   ELSE durGrant
    /\ durLimit' = IF op.kind = "limit"
                   THEN [durLimit EXCEPT ![op.lim] = op.prevNum]
                   ELSE durLimit
    /\ curWs' = IF op.kind = "workspace" THEN op.prevCur ELSE curWs
    /\ audits' = audits - 1
    /\ op' = Idle

\* finish_live, the failure branch. When the store fails, the
\* Administration keeps the undo.
Revert ==
    /\ op.pc = "revert"
    /\ \/ PutBack
       \/ /\ Fault
          /\ op' = [op EXCEPT !.pc = "undo"]
          /\ UNCHANGED <<durGrant, durLimit, curWs, audits>>
    /\ UNCHANGED <<mode, pol, revoked, pending, drev, mrev, node, nOps,
                   restarts, approvals, dead, applies, flags>>

\* Administration::settle: the next request, of any kind, first tries the
\* undo again, and is refused when the store fails again.
Settle ==
    /\ op.pc = "undo" /\ nOps < MaxOps
    /\ nOps' = nOps + 1
    /\ \/ PutBack
       \/ Fault /\ UNCHANGED <<op, durGrant, durLimit, curWs, audits>>
    /\ UNCHANGED <<mode, pol, revoked, pending, drev, mrev, node, restarts,
                   approvals, dead, applies, flags>>

-----------------------------------------------------------------------------
(* Revoke and the policy settings.                                         *)

\* Administration::revoke -> revoke_inner, first half:
\* revoke_applied_grant removes the stored grant, keeps the mark and writes
\* the audit record in one transaction. A process without a node refuses.
\* The socket's RevokeGrant (MCP) asks first that the live policy has the
\* grant.
Revoke(actor, id) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ mode = "serve"
    /\ actor = "mcp" => liveGrant[id] # None
    /\ nOps' = nOps + 1
    /\ \/ /\ revoked' = revoked \cup {id}
          /\ durGrant' = [durGrant EXCEPT ![id] = None]
          /\ audits' = audits + 1
          /\ op' = [Idle EXCEPT !.pc = "rv_live", !.actor = actor, !.id = id]
       \/ Fault /\ UNCHANGED <<revoked, durGrant, audits, op>>
    /\ UNCHANGED <<mode, durLimit, curWs, pol, pending, drev, mrev, node,
                   restarts, approvals, dead, applies, flags>>

\* revoke_inner: LiveNode::revoke_grant. It does not change the revision.
RevokeLive ==
    /\ op.pc = "rv_live"
    /\ liveGrant' = [liveGrant EXCEPT ![op.id] = None]
    /\ dead' = dead \cup {op.id}
    /\ applies' = applies + 1
    /\ pending' = Landed(pending, "other")
    /\ op' = Idle
    /\ UNCHANGED <<mode, durGrant, durLimit, curWs, pol, revoked, drev, audits,
                   mrev, liveLimit, liveWs, liveCap, nOps, restarts, approvals,
                   flags>>

\* The UI's policy form, or the socket's UpdatePolicy (MCP, the tool
\* aether.settings.update_policy) with settings that are not wider ->
\* Administration::save_policy_settings -> save_policy_settings_in_store.
\* The audit record is written first (record_change); when the save fails,
\* fail_change marks it, and when that fails too the record stays. The
\* save keeps the workspace and the limits in the stored policy and removes
\* the stored limit of `aether.change` of every limit. The running node
\* keeps its settings until it starts again.
SavePolicy(actor, v) ==
    /\ op.pc = "idle" /\ nOps < MaxOps
    /\ ~NeedsConfirmation(actor, "policy", v)
    /\ nOps' = nOps + 1
    /\ \/ /\ pol' = [ws |-> v.val, limit |-> v.lims]
          /\ durLimit' = NoLimits
          /\ audits' = audits + 1
          /\ applies' = applies + 1
          /\ pending' = Landed(pending, "other")
          /\ IF actor = "ui" THEN Approve("policy", op.id, v)
             ELSE UNCHANGED approvals
       \/ /\ Fault
          /\ audits' \in {audits, audits + 1}
          /\ UNCHANGED <<pol, durLimit, applies, pending, approvals>>
    /\ UNCHANGED <<mode, op, durGrant, curWs, revoked, drev, mrev, node,
                   restarts, dead, flags>>

-----------------------------------------------------------------------------
(* The environment.                                                        *)

\* 300 s pass after the queue.
Expire(id) ==
    /\ pending[id].kind # None /\ ~pending[id].expired
    /\ pending' = [pending EXCEPT ![id].expired = TRUE]
    /\ UNCHANGED <<mode, op, durGrant, durLimit, curWs, pol, revoked, drev,
                   audits, mrev, node, nOps, restarts, approvals, dead, applies,
                   flags>>

\* The process stops at any step and starts again, with or without a
\* node. Administration::with_store reads the stored revision. A node
\* start replays the store (node_builder), so it has a change that was
\* stored and not yet live, and an undo that was not done.
Restart(m) ==
    /\ restarts < MaxRestarts
    /\ restarts' = restarts + 1
    /\ mode' = m
    /\ op' = Idle
    /\ mrev' = drev
    /\ liveGrant' = [id \in Ids |-> IF m = "serve" THEN StartGrant(id) ELSE None]
    /\ liveLimit' = [l \in LimIds |->
                       IF durLimit[l] # 0 THEN durLimit[l] ELSE pol.limit[l]]
    /\ liveWs' = IF curWs # None THEN curWs ELSE pol.ws
    /\ liveCap' = FALSE
    /\ UNCHANGED <<store, nOps, approvals, dead, applies, flags>>

Next ==
    \/ \E actor \in Actors, kind \in Kinds, id \in Ids :
         \E v \in ValsOf(kind) :
            Queue(actor, kind, id, v) \/ ApplyNow(actor, kind, id, v)
    \/ \E actor \in Actors : \E v \in ValsOf("policy") : SavePolicy(actor, v)
    \/ \E id \in Ids : Confirm(id) \/ Expire(id)
    \/ \E actor \in Actors, id \in Ids : Revoke(actor, id)
    \/ ApplyLive \/ Revert \/ Settle \/ RevokeLive
    \/ \E m \in {"serve", "admin"} : Restart(m)

\* A request that started goes on to its end.
Fairness == WF_vars(ApplyLive) /\ WF_vars(Revert) /\ WF_vars(RevokeLive)

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
\* node only through Confirm. The stored workspace, the stored limit and
\* the stored policy come from a UI action, or the limit is not higher
\* than a limit that a UI action asked for.
StoredFromUi ==
    /\ curWs # None => curWs \in wsOk
    /\ \A l \in LimIds : durLimit[l] <= limOk[l]
    /\ pol.ws \in wsOk /\ \A l \in LimIds : pol.limit[l] <= limOk[l]

\* The live workspace and limits come from a UI action only. An MCP client
\* moves the workspace or raises a limit (the running-task limit too)
\* neither with a change nor with UpdatePolicy and a node start.
LiveFromUi ==
    /\ liveWs \in wsOk
    /\ \A l \in LimIds : liveLimit[l] <= limOk[l]

\* When no request runs and no undo waits, the node and the store agree,
\* also after a store write failed.
Agree ==
    /\ \A id \in Ids : liveGrant[id] = StartGrant(id)
    /\ curWs # None => liveWs = curWs
    /\ \A l \in LimIds : durLimit[l] # 0 => liveLimit[l] = durLimit[l]
LiveMatchesStore == (mode = "serve" /\ op.pc = "idle") => Agree

\* A revoked grant is not live and not in the store, and a node start does
\* not bring it back, until someone applies a grant with that id again.
RevokedStaysOut ==
    \A id \in dead : liveGrant[id] = None /\ durGrant[id] = None

\* A confirmation is refused when another change was stored after the
\* queue, also when the process stopped before the node had that change.
StaleRefused == ~staleConfirmed

\* Each change, revoke and policy save that took effect has an `applied`
\* audit record, which the store had no later than the change.
Audited == applies <= audits

\* A confirmation never applies an expired change. An expired row leaves
\* at the next confirm of its id or at the next queue (see Queue and
\* Confirm), so its id is free again.
ExpiredNeverApplies == ~expiredApplied

(***************************************************************************)
(* Properties that TLC finds false. They are not in PendingChange.cfg;     *)
(* each comment gives the counterexample.                                  *)
(***************************************************************************)

\* FALSE (by design): a revoke or a policy save after the queue makes the
\* confirmation stale. Neither changes the revision.
StaleSeesRevoke == ~otherConfirmed

\* FALSE (by design): only the owner brings back a grant id that the owner
\* revoked. The UI applies a low-risk grant, the UI revokes it, and MCP
\* sends the same signed grant in a Change: it is low risk, so it applies.
\* The revoked mark only stops a grant of the policy file.
NoMcpRevival == ~mcpRevived

(***************************************************************************)
(* Liveness.                                                               *)
(***************************************************************************)

\* A request that holds the mutex ends: its change is in the node, or its
\* value is put back or waits as an undo, or the process stops.
RequestEnds ==
    (op.pc \notin {"idle", "undo"}) ~> (op.pc \in {"idle", "undo"})

=============================================================================
