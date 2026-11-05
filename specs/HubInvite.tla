----------------------------- MODULE HubInvite -----------------------------
(***************************************************************************)
(* The hub invite and access-request handshake between a host node and a   *)
(* caller node, through a hub that the nodes do not trust.                 *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/hub_sync.rs          host_step, caller_step         *)
(*   crates/aether/src/admin_ui/hub.rs      host_worker, caller_worker,    *)
(*                                          approve, block_requester,      *)
(*                                          unblock                        *)
(*   crates/aether/src/admin_ui/host.rs     sign_invite                    *)
(*   crates/aether/src/admin_ui/caller.rs   the access request send        *)
(*   crates/aether/src/hub_membership.rs    offer, accept                  *)
(*   crates/aether-store/src/lib.rs         put_hub_approval,              *)
(*                                          pin_hub_approval,              *)
(*                                          remove_hub_approval,           *)
(*                                          clear_expired_hub_approvals,   *)
(*                                          put_hub_block, remove_hub_block*)
(*   server/lib/aether_hub/invites.ex       the honest hub rows            *)
(*                                                                         *)
(* The hub is untrusted. When `honest` is FALSE, every row that a node     *)
(* reads can be any row: the hub can change any field, replay old lines,   *)
(* reorder rows, or drop them. When `honest` is TRUE, the hub follows      *)
(* invites.ex. `honest` is chosen in Init and does not change, so one TLC  *)
(* run checks the safety properties against an attacker hub and the        *)
(* liveness property against an honest hub.                                *)
(*                                                                         *)
(* Abstracted away: days (host_step refuses more days than approved; this  *)
(* spec has no days), access groups (approve always sets "requests"), the  *)
(* name and endpoint syntax checks, the own-name setting (always set), the *)
(* `side` field (the host and the caller have separate stores here), time  *)
(* (expiry is a nondeterministic removal), the hub version long poll, and  *)
(* import_bundle (a line that caller_step imports counts as kept).         *)
(* A line is the signer endpoint, the agent name that it claims, and a     *)
(* serial number. Signatures are perfect: only HostEp makes HostEp lines.  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Ids,          \* invite ids on the hub
    HostEp,       \* the endpoint of the host node
    HostName,     \* the hub name of the host node (its agent id)
    EvilEp,       \* an endpoint that the attacker controls
    EvilName,     \* another host-side name on the hub
    CallerEp,     \* the endpoint of the caller node
    CallerName,   \* the hub name of the caller node
    OtherEp,      \* the endpoint of another person
    OtherName,    \* the hub name of another person
    MaxSigns,     \* bound on grants that the host signs
    MaxApprovals, \* bound on approvals that the host keeps for one id
    MaxCallerApprovals, \* bound on approvals that the caller keeps for one id
    StoreFaults   \* whether pin_hub_approval can fail with a store error

None == "none"
HostEps == {HostEp, EvilEp}
HostNames == {HostName, EvilName}
CallerEps == {CallerEp, OtherEp}
CallerNames == {CallerName, OtherName}
Origins == {"invite", "request"}
Statuses == {"pending", "requested", "accepted", "delivered"}

Line == [signer: HostEps \cup {None}, agent: HostNames \cup {None}, n: 0..MaxSigns]
NoLine == [signer |-> None, agent |-> None, n |-> 0]
EvilLines == {[signer |-> EvilEp, agent |-> a, n |-> 0] : a \in HostNames}

Row == [origin: Origins, status: Statuses, from: HostNames, to: CallerNames,
        recv: CallerEps \cup {None}, send: HostEps \cup {None}, bundle: Line]
NoRow == [origin |-> "invite", status |-> None, from |-> HostName, to |-> CallerName,
          recv |-> None, send |-> None, bundle |-> NoLine]

\* A host approval (hub_approvals row, side sender). `ver` is a ghost that
\* counts each put_hub_approval of the id, so that two approvals of one id
\* do not count as one.
HostApp == [kind: Origins \cup {None}, peer: CallerNames \cup {None},
            pinned: CallerEps \cup {None}, bundle: Line, ver: Nat]
NoHostApp == [kind |-> None, peer |-> None, pinned |-> None, bundle |-> NoLine, ver |-> 0]

\* A caller approval (hub_approvals row, side receiver). `lep` is the
\* endpoint that signed the kept listing of a request.
CallerApp == [kind: Origins \cup {None}, peer: HostNames \cup {None}, lep: HostEps \cup {None}]
NoCallerApp == [kind |-> None, peer |-> None, lep |-> None]

VARIABLES
    honest,       \* whether the hub follows invites.ex (fixed in Init)
    hub,          \* the honest hub rows
    hApp,         \* host: hub_approvals
    hVer,         \* ghost: the last approval version of each id
    hBlocked,     \* host: hub_blocks
    hIntent,      \* ghost: each approval that the owner page or offer made
    hw,           \* host_worker: its place inside one row
    grants,       \* every grant that sign_invite applied
    revoked,      \* the serials of the grants that revoke_ui removed
    nSigns,       \* how many grants the host signed
    signedBlocked,\* ghost: sign_invite ran for an endpoint that was blocked
    cApp,         \* caller: hub_approvals
    cIntent,      \* ghost: each approval that the caller page made
    cVer,         \* ghost: how many approvals the caller page made per id
    kept          \* each line that caller_worker imported

vars == <<honest, hub, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns,
          signedBlocked, cApp, cIntent, cVer, kept>>

Idle == [pc |-> "idle", id |-> None, ep |-> None, name |-> None, ver |-> 0, line |-> NoLine]

HostLines == {[signer |-> HostEp, agent |-> HostName, n |-> g.n] : g \in grants}

\* Each row that the host can read for `id`: the honest row, or any row.
ArbHostRows == [origin: Origins, status: {"accepted", "requested"}, from: HostNames,
                to: CallerNames, recv: CallerEps \cup {None}, send: {None}, bundle: {NoLine}]
HostReads(id) ==
    (IF hub[id] = NoRow THEN {} ELSE {hub[id]})
    \cup (IF honest THEN {} ELSE ArbHostRows)

\* Each row that the caller can read for `id`. The attacker can replay any
\* line that the host ever signed and any line that it signs itself.
ArbCallerRows == [origin: Origins, status: {"pending", "delivered"}, from: HostNames,
                  to: {CallerName}, recv: {None}, send: HostEps \cup {None},
                  bundle: HostLines \cup EvilLines \cup {NoLine}]
CallerReads(id) ==
    (IF hub[id] = NoRow \/ hub[id].to # CallerName THEN {} ELSE {hub[id]})
    \cup (IF honest THEN {} ELSE ArbCallerRows)

TypeOK ==
    /\ honest \in BOOLEAN
    /\ hub \in [Ids -> Row \cup {NoRow}]
    /\ hApp \in [Ids -> HostApp]
    /\ hVer \in [Ids -> Nat]
    /\ hBlocked \subseteq CallerEps
    /\ hw.pc \in {"idle", "sign", "pin", "deliver"}
    /\ grants \subseteq [id: Ids, ver: Nat, ep: CallerEps, name: CallerNames, n: 1..MaxSigns]
    /\ revoked \subseteq 1..MaxSigns
    /\ nSigns \in 0..MaxSigns
    /\ signedBlocked \in BOOLEAN
    /\ cApp \in [Ids -> CallerApp]
    /\ kept \subseteq [id: Ids, signer: HostEps, agent: HostNames]

Init ==
    /\ honest \in BOOLEAN
    /\ hub = [id \in Ids |-> NoRow]
    /\ hApp = [id \in Ids |-> NoHostApp]
    /\ hVer = [id \in Ids |-> 0]
    /\ hBlocked = {}
    /\ hIntent = {}
    /\ hw = Idle
    /\ grants = {}
    /\ revoked = {}
    /\ nSigns = 0
    /\ signedBlocked = FALSE
    /\ cApp = [id \in Ids |-> NoCallerApp]
    /\ cIntent = {}
    /\ cVer = [id \in Ids |-> 0]
    /\ kept = {}

PutHostApp(id, kind, peer, pinned) ==
    /\ hVer[id] < MaxApprovals
    /\ hVer' = [hVer EXCEPT ![id] = @ + 1]
    /\ hApp' = [hApp EXCEPT ![id] =
                  [kind |-> kind, peer |-> peer, pinned |-> pinned,
                   bundle |-> NoLine, ver |-> hVer[id] + 1]]

(***************************************************************************)
(* Host owner actions.                                                     *)
(***************************************************************************)

\* hub_membership::offer: the hub makes the invite and answers with it;
\* the node keeps the approval for `invite.to` from that answer. `typed` is
\* the name that the owner typed.
Offer(id, typed) ==
    /\ IF honest THEN hub[id] = NoRow /\ hApp[id] = NoHostApp ELSE TRUE
    /\ \E echoed \in (IF honest THEN {typed} ELSE CallerNames) :
        /\ PutHostApp(id, "invite", echoed, None)
        /\ hIntent' = hIntent \cup {[id |-> id, ver |-> hVer[id] + 1, kind |-> "invite",
                                     typed |-> typed, peer |-> echoed, ep |-> None]}
    /\ hub' = IF honest
                THEN [hub EXCEPT ![id] = [origin |-> "invite", status |-> "pending",
                        from |-> HostName, to |-> typed, recv |-> None,
                        send |-> None, bundle |-> NoLine]]
                ELSE hub
    /\ UNCHANGED <<honest, hBlocked, hw, grants, revoked, nSigns, signedBlocked, cApp, cIntent, cVer, kept>>

\* admin_ui/hub.rs approve: the owner approves the endpoint on the page.
ApproveRequest(id) ==
    /\ \E row \in HostReads(id) :
        /\ row.origin = "request" /\ row.status = "requested" /\ row.recv # None
        /\ row.recv \notin hBlocked
        /\ PutHostApp(id, "request", row.to, row.recv)
        /\ hIntent' = hIntent \cup {[id |-> id, ver |-> hVer[id] + 1, kind |-> "request",
                                     typed |-> row.to, peer |-> row.to, ep |-> row.recv]}
        /\ hub' = IF honest THEN [hub EXCEPT ![id].status = "accepted"] ELSE hub
    /\ UNCHANGED <<honest, hBlocked, hw, grants, revoked, nSigns, signedBlocked, cApp, cIntent, cVer, kept>>

\* admin_ui/hub.rs block_requester: keep the block, remove the approval.
\* It holds HostContext::hub_sign, as host_action does from its read to its
\* pin, so it runs only before host_worker reads a row or after it pinned.
BlockRequester(id) ==
    /\ hw.pc \in {"idle", "deliver"}
    /\ \E row \in HostReads(id) :
        /\ row.origin = "request" /\ row.recv # None
        /\ hBlocked' = hBlocked \cup {row.recv}
        /\ hApp' = [hApp EXCEPT ![id] = NoHostApp]
        /\ hub' = IF honest THEN [hub EXCEPT ![id] = NoRow] ELSE hub
    /\ UNCHANGED <<honest, hVer, hIntent, hw, grants, revoked, nSigns, signedBlocked, cApp, cIntent, cVer, kept>>

\* admin_ui/hub.rs unblock.
Unblock(ep) ==
    /\ ep \in hBlocked
    /\ hBlocked' = hBlocked \ {ep}
    /\ UNCHANGED <<honest, hub, hApp, hVer, hIntent, hw, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

\* clear_expired_hub_approvals, or remove_hub_approval after a failed
\* approve. It does not hold hub_sign, so it can run while host_worker is
\* inside a row.
DropHostApp(id) ==
    /\ hApp[id] # NoHostApp
    /\ hApp' = [hApp EXCEPT ![id] = NoHostApp]
    /\ UNCHANGED <<honest, hub, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

(***************************************************************************)
(* host_worker, one row at a time. host_action holds hub_sign from        *)
(* HostRead to HostPin; the hub delivery comes after it, in HostDeliver.   *)
(***************************************************************************)

\* hub_sync::host_step. The approval kinds always match their side here.
HostStep(row, app, blocked) ==
    IF row.status # "accepted" \/ row.recv = None THEN "skip"
    ELSE IF app = NoHostApp THEN "skip"
    ELSE IF blocked \/ row.to # app.peer \/ row.from # HostName THEN "refuse"
    ELSE IF app.kind # row.origin THEN "refuse"
    ELSE IF app.kind = "request" /\ app.pinned # row.recv THEN "refuse"
    ELSE IF app.pinned # None /\ app.pinned # row.recv THEN "refuse"
    ELSE IF app.bundle # NoLine THEN "deliver"
    ELSE "sign"

DeliverTo(id, line) ==
    IF honest /\ hub[id] # NoRow
        THEN [hub EXCEPT ![id] = [@ EXCEPT !.status = "delivered", !.send = HostEp,
                                            !.bundle = line]]
        ELSE hub

\* host_worker: read the approval and the block, then act on host_step.
HostRead(id) ==
    /\ hw.pc = "idle"
    /\ \E row \in HostReads(id) :
        LET step == HostStep(row, hApp[id], row.recv \in hBlocked) IN
        /\ step # "skip"
        /\ CASE step = "refuse" ->
                  /\ hub' = IF honest THEN [hub EXCEPT ![id] = NoRow] ELSE hub
                  /\ UNCHANGED hw
             [] step = "deliver" ->
                  /\ hw' = [Idle EXCEPT !.pc = "deliver", !.id = id,
                                        !.line = hApp[id].bundle]
                  /\ UNCHANGED hub
             [] step = "sign" ->
                  /\ nSigns < MaxSigns
                  /\ hw' = [pc |-> "sign", id |-> id, ep |-> row.recv, name |-> row.to,
                            ver |-> hApp[id].ver, line |-> NoLine]
                  /\ UNCHANGED hub
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

\* admin_ui/host.rs sign_invite: apply the grant and make the line.
\* signedBlocked records a sign for an endpoint that is blocked now.
HostSign ==
    /\ hw.pc = "sign"
    /\ grants' = grants \cup {[id |-> hw.id, ver |-> hw.ver, ep |-> hw.ep,
                              name |-> hw.name, n |-> nSigns + 1]}
    /\ nSigns' = nSigns + 1
    /\ signedBlocked' = (signedBlocked \/ hw.ep \in hBlocked)
    /\ hw' = [hw EXCEPT !.pc = "pin",
                        !.line = [signer |-> HostEp, agent |-> HostName, n |-> nSigns + 1]]
    /\ UNCHANGED <<honest, hub, hApp, hVer, hBlocked, hIntent, revoked, cApp, cIntent, cVer, kept>>

\* host_action: pin_hub_approval with the record that HostRead read. It
\* pins only that record: same peer_name and created_at (here: `ver`, so
\* the spec assumes that two puts of one id never share a created_at
\* second). When the pin fails (a replaced or removed record, another
\* endpoint, or a store error), revoke_ui removes the grant.
HostPin ==
    /\ hw.pc = "pin"
    /\ LET id == hw.id
           app == hApp[id]
           ok == /\ app # NoHostApp
                 /\ app.ver = hw.ver /\ app.peer = hw.name
                 /\ (app.pinned = None \/ app.pinned = hw.ep)
       IN \/ /\ ok
             /\ hApp' = [hApp EXCEPT ![id] = [@ EXCEPT !.pinned = hw.ep, !.bundle = hw.line]]
             /\ hw' = [hw EXCEPT !.pc = "deliver"]
             /\ UNCHANGED revoked
          \/ /\ ~ok \/ StoreFaults
             /\ revoked' = revoked \cup {hw.line.n}
             /\ hw' = Idle
             /\ UNCHANGED hApp
    /\ UNCHANGED <<honest, hub, hVer, hBlocked, hIntent, grants, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

\* host_worker: hub.deliver, after host_action released hub_sign.
HostDeliver ==
    /\ hw.pc = "deliver"
    /\ hub' = DeliverTo(hw.id, hw.line)
    /\ hw' = Idle
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

(***************************************************************************)
(* Caller.                                                                 *)
(***************************************************************************)

\* hub_membership::accept: keep the approval for the name on the page.
CallerAccept(id) ==
    /\ cVer[id] < MaxCallerApprovals
    /\ cVer' = [cVer EXCEPT ![id] = @ + 1]
    /\ \E row \in CallerReads(id) :
        /\ row.origin = "invite" /\ row.status = "pending"
        /\ cApp' = [cApp EXCEPT ![id] = [kind |-> "invite", peer |-> row.from, lep |-> None]]
        /\ cIntent' = cIntent \cup {[id |-> id, kind |-> "invite", name |-> row.from, ep |-> None]}
        /\ hub' = IF honest
                    THEN [hub EXCEPT ![id] = [@ EXCEPT !.status = "accepted", !.recv = CallerEp]]
                    ELSE hub
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked, kept>>

\* admin_ui/caller.rs: send an access request to `name`, whose listing
\* `lep` signed. The honest hub lists HostName at HostEp only.
CallerRequest(id, name, lep) ==
    /\ cVer[id] < MaxCallerApprovals
    /\ cVer' = [cVer EXCEPT ![id] = @ + 1]
    /\ IF honest THEN hub[id] = NoRow /\ (name = HostName <=> lep = HostEp) ELSE TRUE
    /\ cApp' = [cApp EXCEPT ![id] = [kind |-> "request", peer |-> name, lep |-> lep]]
    /\ cIntent' = cIntent \cup {[id |-> id, kind |-> "request", name |-> name, ep |-> lep]}
    /\ hub' = IF honest
                THEN [hub EXCEPT ![id] = [origin |-> "request", status |-> "requested",
                        from |-> name, to |-> CallerName, recv |-> CallerEp,
                        send |-> None, bundle |-> NoLine]]
                ELSE hub
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked, kept>>

\* hub_sync::caller_step.
CallerKeeps(row, app) ==
    /\ app # NoCallerApp
    /\ row.send # None
    /\ app.kind = row.origin
    /\ app.kind = "request" => row.send = app.lep
    /\ row.bundle.signer = row.send
    /\ row.bundle.agent = row.from
    /\ row.bundle.agent = app.peer

\* caller_worker: keep or refuse a delivered line, then remove the
\* approval and the row.
CallerWork(id) ==
    /\ \E row \in CallerReads(id) :
        /\ row.status = "delivered" /\ row.bundle # NoLine
        /\ kept' = IF CallerKeeps(row, cApp[id])
                     THEN kept \cup {[id |-> id, signer |-> row.bundle.signer,
                                      agent |-> row.bundle.agent]}
                     ELSE kept
        /\ cApp' = [cApp EXCEPT ![id] = NoCallerApp]
        /\ hub' = IF honest THEN [hub EXCEPT ![id] = NoRow] ELSE hub
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked,
                   cIntent, cVer>>

(***************************************************************************)
(* Other people on an honest hub, so that the host sees their rows too.    *)
(***************************************************************************)

OtherAccept(id) ==
    /\ honest
    /\ hub[id] # NoRow /\ hub[id].to = OtherName
    /\ hub[id].origin = "invite" /\ hub[id].status = "pending"
    /\ hub' = [hub EXCEPT ![id] = [@ EXCEPT !.status = "accepted", !.recv = OtherEp]]
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

OtherRequest(id) ==
    /\ honest
    /\ hub[id] = NoRow
    /\ hub' = [hub EXCEPT ![id] = [origin |-> "request", status |-> "requested",
                 from |-> HostName, to |-> OtherName, recv |-> OtherEp,
                 send |-> None, bundle |-> NoLine]]
    /\ UNCHANGED <<honest, hApp, hVer, hBlocked, hIntent, hw, grants, revoked, nSigns, signedBlocked,
                   cApp, cIntent, cVer, kept>>

Next ==
    \/ \E id \in Ids :
        \/ \E typed \in CallerNames : Offer(id, typed)
        \/ ApproveRequest(id)
        \/ BlockRequester(id)
        \/ DropHostApp(id)
        \/ HostRead(id)
        \/ CallerAccept(id)
        \/ \E name \in HostNames, lep \in HostEps : CallerRequest(id, name, lep)
        \/ CallerWork(id)
        \/ OtherAccept(id)
        \/ OtherRequest(id)
    \/ \E ep \in CallerEps : Unblock(ep)
    \/ HostSign
    \/ HostPin
    \/ HostDeliver

Fairness ==
    /\ \A id \in Ids : WF_vars(HostRead(id)) /\ WF_vars(CallerWork(id))
    /\ WF_vars(HostSign)
    /\ WF_vars(HostPin)
    /\ WF_vars(HostDeliver)

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Safety, checked against the attacker hub.                               *)
(***************************************************************************)

\* The host signs only for an approval that its own page or offer made: for
\* the name in that approval and, for a request, for the endpoint that the
\* owner approved on the page.
GrantFollowsApproval ==
    \A g \in grants : \E a \in hIntent :
        /\ a.id = g.id /\ a.ver = g.ver /\ a.peer = g.name
        /\ a.kind = "request" => a.ep = g.ep

LiveGrants == {g \in grants : g.n \notin revoked}

\* The live grants of one approval are for one endpoint only: the one that
\* the approval is pinned to. A grant that revoke_ui removed after a failed
\* pin pinned nothing, so the next pass can sign for another endpoint.
OneEndpointPerApproval ==
    \A g1, g2 \in LiveGrants : g1.id = g2.id /\ g1.ver = g2.ver => g1.ep = g2.ep

\* A second delivery sends the same line: one approval has one live grant.
\* A grant that revoke_ui removed after a failed pin does not count.
OneGrantPerApproval ==
    \A g1, g2 \in LiveGrants : g1.id = g2.id /\ g1.ver = g2.ver => g1 = g2

\* A kept bundle belongs to the live grant of that approval. pin_hub_approval
\* checks peer_name and created_at, so an approval that offer replaced with
\* the same id does not keep the line of the old grant.
BundleMatchesApproval ==
    \A id \in Ids : hApp[id].bundle # NoLine =>
        \E g \in LiveGrants : /\ g.n = hApp[id].bundle.n
                          /\ g.id = id /\ g.ver = hApp[id].ver
                          /\ g.ep = hApp[id].pinned /\ g.name = hApp[id].peer

\* The caller keeps a line only for the name that its page accepted and,
\* for a request, only from the endpoint that signed the listing.
CallerKeepsAccepted ==
    \A k \in kept : \E a \in cIntent :
        /\ a.id = k.id /\ a.name = k.agent
        /\ a.kind = "request" => a.ep = k.signer

\* The host never signs for an endpoint that is blocked when it signs.
\* hub_sign keeps block_requester out of host_action, between the block
\* check and the grant.
NeverSignBlocked == ~signedBlocked

(***************************************************************************)
(* Properties that TLC finds false. They are not in HubInvite.cfg; each    *)
(* comment gives the counterexample.                                       *)
(***************************************************************************)


\* FALSE: the grant goes to the name that the owner typed. offer keeps
\* `invite.to` from the hub answer (hub_membership.rs approve_invite), not
\* the `to` that the owner typed, so an attacker hub chooses the name.
GrantToTypedName ==
    \A g \in grants : \E a \in hIntent :
        a.id = g.id /\ a.ver = g.ver /\ a.typed = g.name

\* FALSE: a kept line that names HostName comes from HostEp. For an invite
\* (not a request) the caller has no key for the name and takes the
\* sender endpoint from the hub (hub_sync.rs caller_step), so an attacker
\* hub can deliver a line that EvilEp signed as HostName. This is the trust
\* that invites put in the hub; requests check the listing key.
KeptLineFromNamedHost ==
    \A k \in kept : k.agent = HostName => k.signer = HostEp

(***************************************************************************)
(* Liveness, under an honest hub.                                          *)
(***************************************************************************)

\* An invite that the host offered to the caller and that the caller
\* accepted ends with the host's line kept, unless the owner removed the
\* approval or blocked the caller, or the model ran out of signs.
Ready(id) ==
    /\ honest
    /\ hApp[id].kind = "invite" /\ hApp[id].peer = CallerName
    /\ cApp[id].kind = "invite"
    /\ hub[id] # NoRow /\ hub[id].status = "accepted"

Delivered(id) ==
    \/ \E k \in kept : k.id = id /\ k.signer = HostEp /\ k.agent = HostName
    \/ hApp[id] = NoHostApp
    \/ CallerEp \in hBlocked
    \/ nSigns = MaxSigns

InviteDelivered == \A id \in Ids : Ready(id) ~> Delivered(id)

=============================================================================
