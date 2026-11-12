---------------------------- MODULE DirectInvite ----------------------------
(***************************************************************************)
(* Connecting two people: the host signs a direct invite for a caller's    *)
(* connect code, the caller imports the invite line and keeps a contact,   *)
(* and the caller's first request is admitted under that invite.           *)
(*                                                                         *)
(* Code that this spec models:                                             *)
(*   crates/aether/src/invites.rs        sign_direct_invite_with           *)
(*   crates/aether/src/access_groups.rs  join_and_apply, leave             *)
(*   crates/aether/src/direct.rs         create_card, sign_invite_access,  *)
(*                                       encode_invite_after_grant,        *)
(*                                       import_bundle,                    *)
(*                                       import_invitation_in, contact,    *)
(*                                       remove_contact_in                 *)
(*   crates/aether/src/admin.rs          Administration::apply, revoke     *)
(*   crates/aether/src/proof/admission.rs  admit, check_group              *)
(*   crates/aether-core/src/lib.rs       PolicyEngine::admit, match_grant  *)
(*                                                                         *)
(* sign_direct_invite has three callers: the host page (admin_ui/host.rs   *)
(* create_invite), the admin socket (admin_socket.rs                       *)
(* create_invite_response, for an MCP client), and the hub worker          *)
(* (hub_workers.rs sign_and_pin). All three run in the node process, and   *)
(* sign_direct_invite_with holds the SIGNING lock of invites.rs for the    *)
(* whole call, so one call runs at a time. The owner's other actions (a    *)
(* revoke, a group change) and every request still interleave with a call; *)
(* each store call and each Administration call is one atomic step.        *)
(*                                                                         *)
(* An invite line holds two separate signed things: the invitation (agent  *)
(* name and endpoint of the host, signed with a key that the invitation    *)
(* itself carries, so anyone can make one for any name and endpoint) and   *)
(* the authorization (a claim for the caller's endpoint, signed with the   *)
(* host's issuer key, so only the host makes a real one). The attacker     *)
(* reads every line that the host made, makes its own invitations, puts    *)
(* any authorization in them, and sends requests from its own endpoint.    *)
(* QUIC authenticates the endpoint of a sender.                            *)
(*                                                                         *)
(* Abstracted away: days and the 1/7/30 choice (expiry is a flag that an   *)
(* Expire action sets at any time; a claim expires with its grant), the    *)
(* Agent Card (a bad card is the same as a line that does not import),     *)
(* named groups other than the one that the invite joins, pending changes  *)
(* (see PendingChange), the hub (see HubInvite), the queue and the replay  *)
(* table (see Admission), the task after admission (see ExecutionLease),   *)
(* a host restart, and a crash between the store calls of import_bundle    *)
(* (it is one step here).                                                  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Invites,     \* calls of sign_direct_invite; each signs one grant
    CallerName,  \* the name that the host owner types for the caller
    CallerEp,    \* the endpoint of the caller node
    EvilName,    \* a name that the owner may type for the attacker
    EvilEp,      \* the endpoint of the attacker
    HostName,    \* the agent name of the host in its invitations
    OtherName,   \* another contact name of the caller
    HostEp,      \* the endpoint of the host node
    Faults       \* TRUE: the grant apply and the line encode can fail

None == "none"

\* A person on the host: the name that the owner typed and the endpoint of
\* the connect code. To keep the state space small, each connect code has
\* one name here; a grant and a group row match on both, so a second name
\* for a code is one more person of the same kind.
Persons == {<<CallerName, CallerEp>>, <<EvilName, EvilEp>>}

ContactNames == {HostName, OtherName}
ContactEps == {HostEp, EvilEp}

Pcs == {"idle", "signed", "joined", "applied", "done", "failed"}
Idle == [pc |-> "idle", who |-> <<CallerName, CallerEp>>,
         was1 |-> FALSE, was2 |-> FALSE]

NoContact == [ep |-> None, auth |-> None, expired |-> FALSE]

VARIABLES
    inv,          \* host: where each sign_direct_invite call is
    members,      \* host: access_group_members of the invite's group
    live,         \* host: the grants in the live policy (one per invite)
    gone,         \* the invites whose grant and claim passed expires_at
    lines,        \* the invites whose line the host returned
    invited,      \* ghost: each person that the owner made an invite for
    ownerRemoved, \* ghost: each person that the owner took out of the group
    contacts,     \* caller: the kept bundle of each contact name
    admitted      \* host: <<endpoint, invite>> of each admitted request

vars == <<inv, members, live, gone, lines, invited, ownerRemoved, contacts,
          admitted>>

TypeOK ==
    /\ inv \in [Invites -> [pc : Pcs, who : Persons, was1 : BOOLEAN,
                            was2 : BOOLEAN]]
    /\ members \subseteq Persons
    /\ live \subseteq Invites
    /\ gone \subseteq Invites
    /\ lines \subseteq Invites
    /\ invited \subseteq Persons
    /\ ownerRemoved \subseteq Persons
    /\ contacts \in [ContactNames ->
                       [ep : ContactEps \cup {None},
                        auth : Invites \cup {None, "fake"},
                        expired : BOOLEAN]]
    /\ admitted \subseteq ({CallerEp, EvilEp} \X Invites)

Init ==
    /\ inv = [i \in Invites |-> Idle]
    /\ members = {}
    /\ live = {}
    /\ gone = {}
    /\ lines = {}
    /\ invited = {}
    /\ ownerRemoved = {}
    /\ contacts = [n \in ContactNames |-> NoContact]
    /\ admitted = {}

-----------------------------------------------------------------------------
(* The host: one call of sign_direct_invite_with.                          *)

\* A call holds the SIGNING lock from its start to its end.
Signing(i) == inv[i].pc \in {"signed", "joined", "applied"}

\* SIGNING.lock, then create_card, sign_invite_access, and the first
\* is_member read (`was_member` of sign_direct_invite_with).
Start(i, p) ==
    /\ inv[i].pc = "idle"
    /\ \A j \in Invites : ~Signing(j)
    /\ inv' = [inv EXCEPT ![i] = [pc |-> "signed", who |-> p,
                                  was1 |-> p \in members, was2 |-> FALSE]]
    /\ invited' = invited \cup {p}
    /\ UNCHANGED <<members, live, gone, lines, ownerRemoved, contacts, admitted>>

\* join_and_apply, first half: its own member_of read (`was_member` of
\* join_and_apply), then add_access_group_member.
Join(i) ==
    /\ inv[i].pc = "signed"
    /\ inv' = [inv EXCEPT ![i].pc = "joined",
                          ![i].was2 = inv[i].who \in members]
    /\ members' = members \cup {inv[i].who}
    /\ UNCHANGED <<live, gone, lines, invited, ownerRemoved, contacts, admitted>>

\* join_and_apply, second half: Administration::apply puts the grant in the
\* live policy. When it fails (a store error, a refusal), a person who was
\* not a member at the join leaves the group again.
Apply(i) ==
    /\ inv[i].pc = "joined"
    /\ \/ /\ live' = live \cup {i}
          /\ inv' = [inv EXCEPT ![i].pc = "applied"]
          /\ UNCHANGED members
       \/ /\ Faults
          /\ inv' = [inv EXCEPT ![i].pc = "failed"]
          /\ members' = IF inv[i].was2 THEN members ELSE members \ {inv[i].who}
          /\ UNCHANGED live
    /\ UNCHANGED <<gone, lines, invited, ownerRemoved, contacts, admitted>>

\* encode_invite_after_grant. When it fails, revoke takes the grant back,
\* and a person who was not a member at the start leaves the group.
Encode(i) ==
    /\ inv[i].pc = "applied"
    /\ \/ /\ lines' = lines \cup {i}
          /\ inv' = [inv EXCEPT ![i].pc = "done"]
          /\ UNCHANGED <<members, live>>
       \/ /\ Faults
          /\ inv' = [inv EXCEPT ![i].pc = "failed"]
          /\ live' = live \ {i}
          /\ members' = IF inv[i].was1 THEN members ELSE members \ {inv[i].who}
          /\ UNCHANGED lines
    /\ UNCHANGED <<gone, invited, ownerRemoved, contacts, admitted>>

\* Administration::revoke from the host page or the admin socket.
Revoke(i) ==
    /\ i \in live
    /\ live' = live \ {i}
    /\ UNCHANGED <<inv, members, gone, lines, invited, ownerRemoved, contacts,
                   admitted>>

\* access_groups::change with Action::Remove, on the host page.
RemoveMember(p) ==
    /\ p \in members
    /\ members' = members \ {p}
    /\ ownerRemoved' = ownerRemoved \cup {p}
    /\ UNCHANGED <<inv, live, gone, lines, invited, contacts, admitted>>

\* Time passes expires_at of the grant and of its claim.
Expire(i) ==
    /\ inv[i].pc # "idle"
    /\ i \notin gone
    /\ gone' = gone \cup {i}
    /\ UNCHANGED <<inv, members, live, lines, invited, ownerRemoved, contacts,
                   admitted>>

-----------------------------------------------------------------------------
(* The caller: import_bundle and the contacts.                             *)

\* The endpoint that an authorization names (`caller_endpoint_id`). A real
\* one names the endpoint of its invite. The attacker makes a fake one for
\* any endpoint; `fakeFor` is that endpoint.
AuthEp(a, fakeFor) == IF a = "fake" THEN fakeFor ELSE inv[a].who[2]

\* `contact`: the kept invitation of the name, when it is still valid.
Valid(n) == contacts[n].ep # None /\ ~contacts[n].expired

\* import_bundle with a line that names `n` at `ep` and carries the
\* authorization `a`. The host's own lines name HostName at HostEp; every
\* other invitation is the attacker's. import_invitation_in refuses a name
\* that a valid contact holds at another endpoint. A contact with another
\* name at the same endpoint is removed (a rename).
Import(n, ep, a, fakeFor) ==
    /\ a \in lines \cup {"fake"}
    /\ AuthEp(a, fakeFor) = CallerEp
    /\ ~(Valid(n) /\ contacts[n].ep # ep)
    /\ contacts' = [m \in ContactNames |->
                      IF m = n THEN [ep |-> ep, auth |-> a, expired |-> FALSE]
                      ELSE IF contacts[m].ep = ep THEN NoContact
                      ELSE contacts[m]]
    /\ UNCHANGED <<inv, members, live, gone, lines, invited, ownerRemoved, admitted>>

\* Time passes expires_at of the kept invitation.
ContactExpires(n) ==
    /\ Valid(n)
    /\ contacts' = [contacts EXCEPT ![n].expired = TRUE]
    /\ UNCHANGED <<inv, members, live, gone, lines, invited, ownerRemoved, admitted>>

\* remove_contact_in.
RemoveContact(n) ==
    /\ contacts[n].ep # None
    /\ contacts' = [contacts EXCEPT ![n] = NoContact]
    /\ UNCHANGED <<inv, members, live, gone, lines, invited, ownerRemoved, admitted>>

-----------------------------------------------------------------------------
(* Requests to the host.                                                   *)

\* PolicyEngine::admit, then check_group. `sender` is the endpoint that
\* QUIC authenticated; `a` is the invite whose claim the request carries.
\* The claim must be for the sender and not expired. match_grant then takes
\* any live grant for the same name and endpoint that has not expired, not
\* only the grant that was signed with the claim.
Admits(sender, a) ==
    /\ a \in Invites
    /\ inv[a].pc # "idle"
    /\ inv[a].who[2] = sender
    /\ a \notin gone
    /\ \E g \in live : inv[g].who = inv[a].who /\ g \notin gone
    /\ inv[a].who \in members

\* The caller sends a task to a valid contact at the host's endpoint, with
\* the authorization that it kept. A request to a contact at EvilEp goes to
\* the attacker, which already has every line.
CallerRequest(n) ==
    /\ Valid(n)
    /\ contacts[n].ep = HostEp
    /\ Admits(CallerEp, contacts[n].auth)
    /\ admitted' = admitted \cup {<<CallerEp, contacts[n].auth>>}
    /\ UNCHANGED <<inv, members, live, gone, lines, invited, ownerRemoved, contacts>>

\* The attacker sends a request from its endpoint with the claim of any
\* invite. A claim that the host did not sign fails `verify`.
EvilRequest(a) ==
    /\ Admits(EvilEp, a)
    /\ admitted' = admitted \cup {<<EvilEp, a>>}
    /\ UNCHANGED <<inv, members, live, gone, lines, invited, ownerRemoved, contacts>>

-----------------------------------------------------------------------------

Next ==
    \/ \E i \in Invites :
          \/ \E p \in Persons : Start(i, p)
          \/ Join(i) \/ Apply(i) \/ Encode(i) \/ Revoke(i) \/ Expire(i)
          \/ EvilRequest(i)
    \/ \E p \in Persons : RemoveMember(p)
    \/ \E n \in ContactNames :
          \/ \E ep \in ContactEps, a \in Invites \cup {"fake"},
                f \in {CallerEp, EvilEp} : Import(n, ep, a, f)
          \/ ContactExpires(n) \/ RemoveContact(n) \/ CallerRequest(n)

\* A call of sign_direct_invite runs to its end.
Fairness == \A i \in Invites : WF_vars(Join(i)) /\ WF_vars(Apply(i))
                               /\ WF_vars(Encode(i))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Safety.                                                                 *)

\* A grant is live only while its call still runs or after the call
\* returned its line: a call that failed leaves no grant.
NoOrphanGrant == \A i \in live : inv[i].pc \in {"applied", "done"}

\* The host admits a request only from an endpoint that the owner made an
\* invite for, and only with a claim that was signed for that endpoint. A
\* line that another person reads or copies gives them nothing.
AdmitOnlyInvited ==
    \A x \in admitted :
        /\ inv[x[2]].who[2] = x[1]
        /\ inv[x[2]].who \in invited

\* The caller keeps only an authorization that names its own endpoint.
KeptAuthIsOwn ==
    \A n \in ContactNames :
        contacts[n].auth \in Invites => inv[contacts[n].auth].who[2] = CallerEp

\* The caller has at most one contact name for one endpoint.
OneNamePerEndpoint ==
    \A n, m \in ContactNames :
        (n # m /\ contacts[n].ep # None) => contacts[n].ep # contacts[m].ep

\* While a contact is valid, its name stays with its endpoint: no invite
\* from another key moves the name to another endpoint.
NameKeepsEndpoint ==
    [][\A n \in ContactNames :
         (Valid(n) /\ contacts'[n].ep # None) => contacts'[n].ep = contacts[n].ep
      ]_vars

\* A person is in the group only through a call that reached the join and
\* did not fail, so failed calls alone leave nobody in the group.
NoStrayMember ==
    \A p \in members :
        \E i \in Invites : inv[i].who = p /\ inv[i].pc \in {"joined", "applied", "done"}

\* A line that the host returned works. When no call for the person still
\* runs, the grant is live, and the owner did not take the person out of
\* the group, the person is in the group, so check_group admits them.
\* Without the SIGNING lock a second call for the same person that fails
\* takes the person out of the group under the first call's line.
ReturnedLineWorks ==
    \A i \in lines :
        (/\ i \in live
         /\ inv[i].who \notin ownerRemoved
         /\ \A j \in Invites :
              inv[j].who = inv[i].who => inv[j].pc \in {"idle", "done", "failed"})
        => inv[i].who \in members

-----------------------------------------------------------------------------
(* Liveness.                                                               *)

\* Each call of sign_direct_invite ends with a line or with a failure.
InviteEnds ==
    \A i \in Invites : inv[i].pc # "idle" ~> inv[i].pc \in {"done", "failed"}

=============================================================================
