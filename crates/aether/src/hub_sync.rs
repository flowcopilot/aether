//! The hub decisions of a node, apart from the network and the page: which
//! hub rows the host signs a grant for, and which lines the caller keeps.
//! The hub only carries invites; it cannot make a grant. A host signs only a
//! row that its own page sent or approved, and only for the endpoint that it
//! first signed for. A caller keeps only a line from the endpoint and the
//! name that its own page accepted.

use aether_store::{ApprovalKind, ApprovalSide, HubApproval};

use crate::{
    direct::InvitationBundle,
    hub_client::{Invite, Origin, Role, Status},
};

/// What the host does with one hub row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum HostStep {
    /// Nothing now: the row is not ready, or this page never sent it.
    Skip,
    /// Sign access for `friend_name` at `endpoint` for `days` days, in
    /// `access_group`, then send the line.
    Sign { friend_name: String, endpoint: String, days: u64, access_group: String },
    /// The line was signed before: send the same line again.
    Deliver(String),
    /// The row does not match what the owner approved: remove it.
    Refuse,
}

/// The step for `invite` with the local `approval` of its id. `own_name` is
/// this node's name on the hub (the `HubName` setting), never a
/// flag: without it the host makes no step. `blocked` is whether the owner
/// blocked the endpoint that the row names: the host never signs for it.
#[must_use]
pub fn host_step(
    invite: &Invite,
    approval: Option<&HubApproval>,
    own_name: Option<&str>,
    blocked: bool,
) -> HostStep {
    let (Role::Sender, Status::Accepted, Some(endpoint)) =
        (invite.role, invite.status, invite.receiver_endpoint.as_deref())
    else {
        return HostStep::Skip;
    };
    let (Some(own_name), Some(approval)) = (own_name, approval) else {
        return HostStep::Skip;
    };
    if blocked
        || endpoint.parse::<iroh::EndpointId>().is_err()
        || !crate::hub_client::valid_full_name(&invite.to)
        || approval.side != ApprovalSide::Sender
        || invite.to != approval.peer_name
        || invite.days > u64::from(approval.days)
        || invite.from != own_name
    {
        return HostStep::Refuse;
    }
    let matches_kind = match approval.kind {
        ApprovalKind::Invite => invite.origin == Origin::Invite,
        // The owner approved one endpoint with the click, and a grant from
        // a request always joins the requests group.
        ApprovalKind::Request => {
            invite.origin == Origin::Request
                && approval.peer_endpoint.as_deref() == Some(endpoint)
                && approval.access_group == crate::access_groups::REQUESTS_GROUP
        }
    };
    if !matches_kind {
        return HostStep::Refuse;
    }
    if approval.peer_endpoint.as_deref().is_some_and(|pinned| pinned != endpoint) {
        return HostStep::Refuse;
    }
    if let Some(bundle) = &approval.bundle {
        return HostStep::Deliver(bundle.clone());
    }
    HostStep::Sign {
        friend_name: invite.to.clone(),
        endpoint: endpoint.to_owned(),
        days: invite.days,
        access_group: approval.access_group.clone(),
    }
}

/// What the caller does with one hub row.
#[derive(Debug, Clone)]
pub enum CallerStep {
    /// Nothing: the row has no line for this node yet.
    Skip,
    /// Keep this line as a contact.
    Import(Box<InvitationBundle>),
    /// Do not keep the line, for this reason; remove the row.
    Refuse(&'static str),
}

/// The step for `invite` with the local `approval` of its id.
#[must_use]
pub fn caller_step(invite: &Invite, approval: Option<&HubApproval>) -> CallerStep {
    let (Role::Receiver, Status::Delivered, Some(line)) =
        (invite.role, invite.status, invite.bundle.as_deref())
    else {
        return CallerStep::Skip;
    };
    let Some(approval) =
        approval.filter(|approval| approval.side == ApprovalSide::Receiver)
    else {
        return CallerStep::Refuse("this page did not accept the invite");
    };
    let Some(sender) = invite.sender_endpoint.as_deref() else {
        return CallerStep::Refuse("the hub did not name the sender endpoint");
    };
    match (approval.kind, invite.origin) {
        (ApprovalKind::Invite, Origin::Invite) => {}
        // The line of a request must come from the endpoint that signed the
        // listing. The listing counts as it was when the person confirmed
        // the request: it can expire before the owner approves.
        (ApprovalKind::Request, Origin::Request) => {
            let Some(listing) = &approval.listing else {
                return CallerStep::Refuse("this page kept no listing for the request");
            };
            let at = u64::try_from(approval.created_at).unwrap_or(u64::MAX);
            let expires_at = u64::try_from(listing.expires_at).unwrap_or(0);
            if crate::listing::verify(
                &listing.text,
                &listing.signature,
                sender,
                expires_at,
                at,
            )
            .is_err()
            {
                return CallerStep::Refuse(
                    "the endpoint that sent the line did not sign the listing",
                );
            }
        }
        _ => {
            return CallerStep::Refuse(
                "the row is not of the kind that this page accepted",
            );
        }
    }
    let Ok(bundle) = InvitationBundle::decode(line) else {
        return CallerStep::Refuse("the line is not an invite");
    };
    if bundle.invitation.endpoint_id != sender {
        return CallerStep::Refuse("the line names another endpoint");
    }
    if bundle.invitation.agent_id != invite.from
        || bundle.invitation.agent_id != approval.peer_name
    {
        return CallerStep::Refuse("the line names another agent");
    }
    CallerStep::Import(Box::new(bundle))
}

#[cfg(test)]
mod tests {
    use aether_store::{ApprovalKind, ApprovalSide, HubApproval};

    use super::{CallerStep, HostStep, caller_step, host_step};
    use crate::hub_client::{Invite, Origin, Role, Status};

    fn endpoint() -> String {
        iroh::SecretKey::from_bytes(&[3; 32]).public().to_string()
    }

    fn accepted() -> Invite {
        Invite {
            id: "0123456789abcdef0123456789abcdef".to_owned(),
            from: "tudor#0042".to_owned(),
            to: "ana#0007".to_owned(),
            role: Role::Sender,
            days: 7,
            status: Status::Accepted,
            origin: Origin::Invite,
            receiver_endpoint: Some(endpoint()),
            bundle: None,
            sender_endpoint: None,
            created_at: 1,
        }
    }

    fn approval() -> HubApproval {
        HubApproval {
            invite_id: accepted().id,
            side: ApprovalSide::Sender,
            kind: ApprovalKind::Invite,
            peer_name: "ana#0007".to_owned(),
            peer_endpoint: None,
            days: 7,
            access_group: "friends".to_owned(),
            bundle: None,
            created_at: 1,
            expires_at: 2,
            listing: None,
        }
    }

    #[test]
    fn the_host_signs_only_an_invite_it_sent() {
        assert_eq!(
            host_step(&accepted(), None, Some("tudor#0042"), false),
            HostStep::Skip
        );
        assert_eq!(
            host_step(&accepted(), Some(&approval()), None, false),
            HostStep::Skip
        );
        assert_eq!(
            host_step(&accepted(), Some(&approval()), Some("tudor#0042"), false),
            HostStep::Sign {
                friend_name: "ana#0007".to_owned(),
                endpoint: endpoint(),
                days: 7,
                access_group: "friends".to_owned(),
            }
        );
        let pending = Invite { status: Status::Pending, ..accepted() };
        assert_eq!(
            host_step(&pending, Some(&approval()), Some("tudor#0042"), false),
            HostStep::Skip
        );
        let received = Invite { role: Role::Receiver, ..accepted() };
        assert_eq!(
            host_step(&received, Some(&approval()), Some("tudor#0042"), false),
            HostStep::Skip
        );
    }

    #[test]
    fn the_host_refuses_a_row_whose_name_days_or_endpoint_changed() {
        let approval = approval();
        let refuse = |invite: Invite| {
            host_step(&invite, Some(&approval), Some("tudor#0042"), false)
        };
        assert_eq!(
            refuse(Invite { to: "eve".to_owned(), ..accepted() }),
            HostStep::Refuse
        );
        assert_eq!(refuse(Invite { days: 30, ..accepted() }), HostStep::Refuse);
        assert_eq!(
            refuse(Invite { from: "mallory".to_owned(), ..accepted() }),
            HostStep::Refuse
        );
        assert_eq!(
            refuse(Invite {
                receiver_endpoint: Some("not-a-key".to_owned()),
                ..accepted()
            }),
            HostStep::Refuse
        );
        let pinned = HubApproval {
            peer_endpoint: Some(
                iroh::SecretKey::from_bytes(&[4; 32]).public().to_string(),
            ),
            ..approval.clone()
        };
        assert_eq!(
            host_step(&accepted(), Some(&pinned), Some("tudor#0042"), false),
            HostStep::Refuse
        );
        let received = HubApproval { side: ApprovalSide::Receiver, ..approval.clone() };
        assert_eq!(
            host_step(&accepted(), Some(&received), Some("tudor#0042"), false),
            HostStep::Refuse
        );
        assert_eq!(
            refuse(Invite { days: 1, ..accepted() }),
            HostStep::Sign {
                friend_name: "ana#0007".to_owned(),
                endpoint: endpoint(),
                days: 1,
                access_group: "friends".to_owned(),
            }
        );
    }

    /// A delivered row for the caller, with a line that `host` signed as
    /// `agent` for the caller key `caller`.
    fn delivered(host: &std::path::Path, agent: &str, caller: &str) -> Invite {
        let access = crate::identity::AccessInput {
            issuer_agent_id: "node".to_owned(),
            subject_agent_id: "ana#0007".to_owned(),
            subject_endpoint_id: caller.to_owned(),
            scope_id: crate::direct::SCOPE.to_owned(),
            capabilities: vec![crate::execution::GENERAL_SKILL.to_owned()],
            workspaces: vec!["workspace".to_owned()],
            access_mode: aether_core::AccessMode::ReadOnly,
            approval_rule: aether_core::ApprovalRule::Automatic,
            lifetime: std::time::Duration::from_secs(60),
        };
        let (claim, _) = crate::identity::sign_access(
            &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
            &access,
        )
        .expect("access must sign");
        let authorization =
            crate::transport::A2aAuthorization::for_access(&access, &claim);
        let line = crate::direct::InvitationBundle {
            format: crate::direct::BUNDLE_FORMAT_V1.to_owned(),

            agent_card: None,

            agent_card_signature: None,
            invitation: crate::direct::create_invitation(
                host,
                agent,
                std::time::Duration::from_secs(60),
            )
            .expect("invitation must sign"),
            authorization,
            relay_url: None,
        }
        .encode()
        .expect("the line must encode");
        Invite {
            from: "tudor#0042".to_owned(),
            to: "ana#0007".to_owned(),
            role: Role::Receiver,
            status: Status::Delivered,
            receiver_endpoint: None,
            bundle: Some(line),
            sender_endpoint: Some(crate::direct::endpoint_id(host).expect("endpoint")),
            ..accepted()
        }
    }

    fn accepted_by_caller() -> HubApproval {
        HubApproval {
            side: ApprovalSide::Receiver,
            peer_name: "tudor#0042".to_owned(),
            ..approval()
        }
    }

    #[test]
    fn the_caller_imports_only_a_bundle_from_the_named_sender_endpoint() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = endpoint();
        let row = delivered(host.path(), "tudor#0042", &caller);
        let approval = accepted_by_caller();
        assert!(matches!(caller_step(&row, Some(&approval)), CallerStep::Import(_)));

        let pending = Invite { status: Status::Pending, bundle: None, ..row.clone() };
        assert!(matches!(caller_step(&pending, Some(&approval)), CallerStep::Skip));
        assert!(matches!(caller_step(&row, None), CallerStep::Refuse(_)));
        let sent = HubApproval { side: ApprovalSide::Sender, ..approval.clone() };
        assert!(matches!(caller_step(&row, Some(&sent)), CallerStep::Refuse(_)));
        let unnamed = Invite { sender_endpoint: None, ..row.clone() };
        assert!(matches!(
            caller_step(&unnamed, Some(&approval)),
            CallerStep::Refuse(_)
        ));
        let other_endpoint = Invite {
            sender_endpoint: Some(
                iroh::SecretKey::from_bytes(&[8; 32]).public().to_string(),
            ),
            ..row.clone()
        };
        assert!(matches!(
            caller_step(&other_endpoint, Some(&approval)),
            CallerStep::Refuse(_)
        ));
        let renamed = delivered(host.path(), "mallory", &caller);
        assert!(matches!(
            caller_step(&renamed, Some(&approval)),
            CallerStep::Refuse(_)
        ));
        let other_peer = HubApproval { peer_name: "bob".to_owned(), ..approval };
        assert!(matches!(caller_step(&row, Some(&other_peer)), CallerStep::Refuse(_)));
        let garbage = Invite { bundle: Some("aether-invite:junk".to_owned()), ..row };
        assert!(matches!(
            caller_step(&garbage, Some(&accepted_by_caller())),
            CallerStep::Refuse(_)
        ));
    }

    /// An accepted request from `endpoint()`, and the approval of the
    /// owner's click for it.
    fn approved_request() -> (Invite, HubApproval) {
        let invite = Invite {
            from: "law#0042".to_owned(),
            to: "ana#0001".to_owned(),
            origin: Origin::Request,
            ..accepted()
        };
        let approval = HubApproval {
            kind: ApprovalKind::Request,
            peer_name: "ana#0001".to_owned(),
            peer_endpoint: Some(endpoint()),
            access_group: "requests".to_owned(),
            ..approval()
        };
        (invite, approval)
    }

    #[test]
    fn a_request_is_signed_only_for_the_endpoint_the_owner_approved() {
        let (invite, approval) = approved_request();
        let step = |invite: &Invite, approval: &HubApproval| {
            host_step(invite, Some(approval), Some("law#0042"), false)
        };
        assert_eq!(
            step(&invite, &approval),
            HostStep::Sign {
                friend_name: "ana#0001".to_owned(),
                endpoint: endpoint(),
                days: 7,
                access_group: "requests".to_owned(),
            }
        );
        let other = Invite {
            receiver_endpoint: Some(
                iroh::SecretKey::from_bytes(&[5; 32]).public().to_string(),
            ),
            ..invite.clone()
        };
        assert_eq!(step(&other, &approval), HostStep::Refuse);
        let unpinned = HubApproval { peer_endpoint: None, ..approval.clone() };
        assert_eq!(step(&invite, &unpinned), HostStep::Refuse);
        // An invite approval does not sign a request row, nor the reverse.
        let as_invite = HubApproval { kind: ApprovalKind::Invite, ..approval.clone() };
        assert_eq!(step(&invite, &as_invite), HostStep::Refuse);
        let invite_row = Invite { origin: Origin::Invite, ..invite.clone() };
        assert_eq!(step(&invite_row, &approval), HostStep::Refuse);
        let longer = Invite { days: 30, ..invite };
        assert_eq!(step(&longer, &approval), HostStep::Refuse);
    }

    #[test]
    fn a_request_grant_always_joins_the_requests_group() {
        let (invite, approval) = approved_request();
        let friends = HubApproval { access_group: "friends".to_owned(), ..approval };
        assert_eq!(
            host_step(&invite, Some(&friends), Some("law#0042"), false),
            HostStep::Refuse
        );
    }

    #[test]
    fn a_blocked_endpoint_is_never_signed() {
        let (request, request_approval) = approved_request();
        assert_eq!(
            host_step(&request, Some(&request_approval), Some("law#0042"), true),
            HostStep::Refuse
        );
        assert_eq!(
            host_step(&accepted(), Some(&approval()), Some("tudor#0042"), true),
            HostStep::Refuse
        );
        let signed = HubApproval {
            peer_endpoint: Some(endpoint()),
            bundle: Some("aether-invite:signed".to_owned()),
            ..approval()
        };
        assert_eq!(
            host_step(&accepted(), Some(&signed), Some("tudor#0042"), true),
            HostStep::Refuse
        );
    }

    /// A delivered request row from the service in `host`, named
    /// `law#0042`, and the approval of the person who confirmed it with the
    /// listing signed by the endpoint key in `signer`.
    fn delivered_request(
        host: &std::path::Path,
        signer: &std::path::Path,
        listing_expires_at: i64,
        created_at: i64,
    ) -> (Invite, HubApproval) {
        use base64::Engine;
        let row = Invite {
            from: "law#0042".to_owned(),
            origin: Origin::Request,
            ..delivered(host, "law#0042", &endpoint())
        };
        let bytes = crate::listing::listing_bytes(
            &crate::agent_card::CardSettings::default(),
            "law#0042",
        );
        let key = crate::data_dir::DataDir::new(signer)
            .endpoint_key()
            .expect("key must load");
        let expires = u64::try_from(listing_expires_at).expect("expiry");
        let approval = HubApproval {
            kind: ApprovalKind::Request,
            peer_name: "law#0042".to_owned(),
            created_at,
            listing: Some(aether_store::KeptListing {
                text: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes),
                signature: crate::listing::sign(&key, &bytes, expires),
                expires_at: listing_expires_at,
            }),
            ..accepted_by_caller()
        };
        (row, approval)
    }

    fn now() -> i64 {
        aether_core::time::unix_timestamp(std::time::SystemTime::now()).expect("time")
    }

    #[test]
    fn a_request_bundle_from_an_endpoint_that_did_not_sign_the_listing_is_refused() {
        let host = tempfile::tempdir().expect("directory must create");
        let other = tempfile::tempdir().expect("directory must create");
        let (row, approval) =
            delivered_request(host.path(), host.path(), now() + 3600, now());
        assert!(matches!(caller_step(&row, Some(&approval)), CallerStep::Import(_)));

        let (row, forged) =
            delivered_request(host.path(), other.path(), now() + 3600, now());
        assert!(matches!(caller_step(&row, Some(&forged)), CallerStep::Refuse(_)));
        let unkept = HubApproval { listing: None, ..approval.clone() };
        assert!(matches!(caller_step(&row, Some(&unkept)), CallerStep::Refuse(_)));
        // An invite approval does not take the line of a request.
        let as_invite = HubApproval { kind: ApprovalKind::Invite, ..approval };
        assert!(matches!(caller_step(&row, Some(&as_invite)), CallerStep::Refuse(_)));
    }

    #[test]
    fn a_kept_listing_that_expires_before_delivery_still_accepts_a_bundle_from_its_endpoint()
     {
        let host = tempfile::tempdir().expect("directory must create");
        let asked = now() - 3 * 24 * 60 * 60;
        let (row, approval) =
            delivered_request(host.path(), host.path(), asked + 60, asked);
        assert!(matches!(caller_step(&row, Some(&approval)), CallerStep::Import(_)));
        // A listing that had expired when the person asked is refused.
        let (row, late) = delivered_request(host.path(), host.path(), asked - 1, asked);
        assert!(matches!(caller_step(&row, Some(&late)), CallerStep::Refuse(_)));
    }

    #[test]
    fn a_signed_invite_is_delivered_again_without_a_new_grant() {
        let signed = HubApproval {
            peer_endpoint: Some(endpoint()),
            bundle: Some("aether-invite:signed".to_owned()),
            ..approval()
        };
        assert_eq!(
            host_step(&accepted(), Some(&signed), Some("tudor#0042"), false),
            HostStep::Deliver("aether-invite:signed".to_owned())
        );
    }
}
