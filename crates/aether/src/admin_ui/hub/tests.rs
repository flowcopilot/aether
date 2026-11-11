#![expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures hold a lock only while they set up; nothing contends"
)]

use std::sync::Arc;

use axum::http::StatusCode;

use super::{InviteOffer, invite_offer};
use crate::admin_ui::{
    PageContext, UiOptions, build_admin_ui,
    test_support::{
        PORT, app, get, get_with, hub_caller_ui, hub_host_ui, post, service_page,
        session, test_administration, test_host, test_host_page,
    },
};
use crate::hub_client::{Invite, Origin, Role, Status};
use aether_store::NodeStore;

fn invite(role: Role, status: Status) -> Invite {
    Invite {
        id: "one".to_owned(),
        from: "bob#0001".to_owned(),
        to: "ana#0002".to_owned(),
        role,
        days: 7,
        status,
        origin: Origin::default(),
        receiver_endpoint: None,
        bundle: None,
        sender_endpoint: None,
        created_at: 0,
    }
}

fn offer(
    text: &str,
    spinner: bool,
    answer: bool,
    remove: Option<&'static str>,
) -> InviteOffer {
    InviteOffer { text: text.to_owned(), spinner, answer, remove }
}

#[test]
fn an_invite_row_offers_what_its_side_and_status_allow() {
    let stale = "This invite was made before an update. Cancel it and invite again.";
    let cases = [
        (
            Role::Sender,
            Status::Pending,
            false,
            offer(stale, false, false, Some("Cancel")),
        ),
        (
            Role::Sender,
            Status::Accepted,
            false,
            offer(stale, true, false, Some("Cancel")),
        ),
        (
            Role::Sender,
            Status::Pending,
            true,
            offer("Waiting for ana#0002 to accept", false, false, Some("Cancel")),
        ),
        (
            Role::Sender,
            Status::Accepted,
            true,
            offer("Connecting ana#0002…", true, false, None),
        ),
        (
            Role::Sender,
            Status::Delivered,
            true,
            offer("Connecting ana#0002…", true, false, None),
        ),
        (
            Role::Receiver,
            Status::Pending,
            false,
            offer(
                "bob#0001 wants to share their agent with you for 7 days",
                false,
                true,
                Some("Decline"),
            ),
        ),
        (
            Role::Receiver,
            Status::Requested,
            false,
            offer("Waiting for bob#0001 to approve", false, false, Some("Cancel")),
        ),
        (
            Role::Receiver,
            Status::Accepted,
            false,
            offer("Connecting to bob#0001…", true, false, None),
        ),
    ];
    for (role, status, approved, expected) in cases {
        assert_eq!(
            invite_offer(&invite(role, status), approved),
            expected,
            "{role:?} {status:?} approved={approved}"
        );
    }
}

/// Waits until the hub invite `id` is delivered, or fails the test.
async fn wait_for_delivery(hub: &crate::hub_client::test_hub::TestHub, id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let delivered = hub
                .state()
                .invites
                .iter()
                .any(|invite| invite.id == id && invite.status == "delivered");
            if delivered {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the approved invite must be delivered");
}

/// Keeps the local approval of the hub invite `id` to `peer`.
fn approve(data_dir: &std::path::Path, id: &str, peer: &str) {
    NodeStore::in_dir(data_dir)
        .expect("store must open")
        .put_hub_approval(&aether_store::HubApproval {
            invite_id: id.to_owned(),
            side: aether_store::ApprovalSide::Sender,
            kind: aether_store::ApprovalKind::Invite,
            peer_name: peer.to_owned(),
            peer_endpoint: None,
            days: 7,
            access_group: "friends".to_owned(),
            bundle: None,
            created_at: 1,
            expires_at: i64::MAX,
            listing: None,
        })
        .expect("approval must store");
}

fn peer_endpoint(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32]).public().to_string()
}

#[tokio::test]
async fn a_hub_row_the_owner_never_sent_gets_no_grant() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (administration, node) = test_administration(directory.path());
    let host = test_host(directory.path(), Arc::new(std::sync::Mutex::new(Vec::new())))
        .with_hub(Some(hub.client(directory.path())));
    crate::settings::store_setting::<crate::hub_client::settings::HubName>(
        directory.path(),
        &Some("tudor#0042".to_owned()),
    )
    .expect("hub name must store");
    let own = crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
    let (ana, eve) = (peer_endpoint(5), peer_endpoint(6));
    let (unsent, sent) = {
        let mut state = hub.state();
        state.add_account(&own, "tudor#0042");
        state.add_account(&ana, "ana#0007");
        state.add_account(&eve, "eve#0008");
        (
            state.add_invite(&own, &eve, "accepted"),
            state.add_invite(&own, &ana, "accepted"),
        )
    };
    approve(directory.path(), &sent, "ana#0007");
    let worker = tokio::spawn(crate::hub_workers::host_worker(
        administration,
        host.worker_context(),
    ));
    wait_for_delivery(&hub, &sent).await;
    worker.abort();

    let applied = node.granted();
    assert_eq!(applied.len(), 1);
    let grant = crate::identity::decode_grant(&applied[0]).expect("grant must parse");
    assert_eq!(grant.subject_endpoint_id.as_str(), ana);
    let state = hub.state();
    let unsent = state.invites.iter().find(|invite| invite.id == unsent);
    assert_eq!(unsent.map(|invite| invite.status.as_str()), Some("accepted"));
    assert_eq!(unsent.and_then(|invite| invite.bundle.as_deref()), None);
}

/// A line that the host in `host_dir` signed as `agent` for `caller`.
fn signed_line(host_dir: &std::path::Path, agent: &str, caller: &str) -> String {
    let access = crate::identity::AccessInput {
        issuer_agent_id: "node".to_owned(),
        subject_agent_id: "ana".to_owned(),
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
    let authorization = crate::transport::A2aAuthorization::for_access(&access, &claim);
    crate::direct::InvitationBundle {
        format: crate::direct::BUNDLE_FORMAT_V1.to_owned(),

        agent_card: None,

        agent_card_signature: None,
        invitation: crate::direct::create_invitation(
            host_dir,
            agent,
            std::time::Duration::from_secs(60),
        )
        .expect("invitation must sign"),
        authorization,
        relay_url: None,
    }
    .encode()
    .expect("the line must encode")
}

#[tokio::test]
async fn an_accepted_invite_imports_only_a_bundle_from_the_sender_endpoint() {
    let caller_dir = tempfile::tempdir().expect("directory must create");
    let host_dir = tempfile::tempdir().expect("directory must create");
    let mallory_dir = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let client = hub.client(caller_dir.path());
    let caller = client.endpoint_id();
    let host = crate::direct::endpoint_id(host_dir.path()).expect("endpoint must load");
    let mallory =
        crate::direct::endpoint_id(mallory_dir.path()).expect("endpoint must load");
    let (honest, lying) = {
        let mut state = hub.state();
        state.add_account(&caller, "ana");
        state.add_account(&host, "tudor");
        state.add_account(&mallory, "eve");
        (
            state.add_invite(&host, &caller, "pending"),
            state.add_invite(&mallory, &caller, "pending"),
        )
    };
    crate::hub_membership::accept(&client, caller_dir.path(), &honest)
        .await
        .expect("accept");
    crate::hub_membership::accept(&client, caller_dir.path(), &lying)
        .await
        .expect("accept");
    {
        let mut state = hub.state();
        for invite in &mut state.invites {
            invite.status = "delivered".to_owned();
            // Mallory's line names another agent than the row.
            invite.bundle = Some(if invite.id == honest {
                signed_line(host_dir.path(), "tudor", &caller)
            } else {
                signed_line(mallory_dir.path(), "tudor", &caller)
            });
        }
    }
    let context = PageContext::admin_caller(caller_dir.path().to_path_buf())
        .with_hub(Some(client));
    let worker =
        tokio::spawn(crate::hub_workers::caller_worker(context.worker_context()));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !hub.state().invites.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the worker must handle both rows");
    worker.abort();

    let contacts = crate::direct::contact_bundles(
        &crate::direct::open_store(caller_dir.path()).expect("store must open"),
    )
    .expect("contacts");
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].invitation.agent_id, "tudor");
    assert_eq!(contacts[0].invitation.endpoint_id, host);
}

#[tokio::test]
async fn the_host_page_claims_a_name_and_shows_the_full_name() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let page = hub_host_ui(directory.path(), &hub);
    let cookie = session(&page).await;
    let claimed = post(page.clone(), "/hub/name", Some(&cookie), "name=tudor").await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    assert!(claimed.body.contains("tudor#0001"), "{}", claimed.body);
    assert!(claimed.body.contains("bob#0042"));
    assert_eq!(
        crate::settings::setting::<crate::hub_client::settings::HubName>(
            directory.path()
        )
        .ok()
        .flatten()
        .as_deref(),
        Some("tudor#0001")
    );
}

#[tokio::test]
async fn a_host_without_a_name_chooses_personal_or_service() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let page = hub_host_ui(directory.path(), &hub);
    let home = get(page.clone(), "/").await;
    assert!(home.body.contains("Personal agent: it stands for you."));
    assert!(home.body.contains("Service agent: it offers a service"));
    let cookie = session(&page).await;
    let claimed =
        post(page.clone(), "/hub/name", Some(&cookie), "name=law&kind=service").await;
    assert!(claimed.body.contains("This is a service agent."), "{}", claimed.body);
    assert!(!claimed.body.contains("Their name on Aether"));
    assert_eq!(
        crate::settings::setting::<crate::hub_client::settings::AgentKind>(
            directory.path()
        )
        .ok()
        .flatten(),
        Some(crate::hub_client::AgentKind::Service)
    );
    let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
    assert_eq!(
        hub.state().kinds.get(&endpoint).copied(),
        Some(crate::hub_client::AgentKind::Service)
    );
}

#[tokio::test]
async fn a_caller_only_page_offers_only_a_personal_agent() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let page = hub_caller_ui(directory.path(), &hub);
    let home = get(page.clone(), "/").await;
    assert!(home.body.contains("Choose your name on Aether"));
    assert!(!home.body.contains("Service agent"));
    let cookie = session(&page).await;
    post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=service").await;
    let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
    assert_eq!(
        hub.state().kinds.get(&endpoint).copied(),
        Some(crate::hub_client::AgentKind::Personal)
    );
}

#[tokio::test]
async fn a_personal_agent_turns_name_search_on_and_off() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let page = hub_host_ui(directory.path(), &hub);
    let cookie = session(&page).await;
    post(page.clone(), "/hub/name", Some(&cookie), "name=tudor&kind=personal").await;
    let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
    let on =
        post(page.clone(), "/hub/searchable", Some(&cookie), "searchable=on").await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    assert!(hub.state().searchable.contains(&endpoint));
    post(page.clone(), "/hub/searchable", Some(&cookie), "").await;
    assert!(!hub.state().searchable.contains(&endpoint));

    let service_dir = tempfile::tempdir().expect("directory must create");
    let service = hub_host_ui(service_dir.path(), &hub);
    let cookie = session(&service).await;
    post(service.clone(), "/hub/name", Some(&cookie), "name=law&kind=service").await;
    let refused =
        post(service.clone(), "/hub/searchable", Some(&cookie), "searchable=on").await;
    assert!(
        refused.body.contains("Only a personal agent can be found by name search.")
    );
    let service_endpoint =
        crate::direct::endpoint_id(service_dir.path()).expect("endpoint");
    assert!(!hub.state().searchable.contains(&service_endpoint));
}

/// The agent IDs in the access group `group` of `data_dir`.
fn group_members(data_dir: &std::path::Path, group: &str) -> Vec<String> {
    NodeStore::in_dir(data_dir)
        .expect("store must open")
        .access_groups()
        .expect("groups must read")
        .into_iter()
        .find(|entry| entry.name == group)
        .map(|entry| entry.members.into_iter().map(|member| member.agent_id).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn approving_a_request_grants_access_in_the_requests_group() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
    let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
    let ana = peer_endpoint(5);
    let id = {
        let mut state = hub.state();
        state.add_account(&ana, "ana#0001");
        state.add_request(&service, &ana, 7)
    };
    let home = get_with(page.clone(), "/", &cookie).await;
    assert!(home.body.contains("asks for access for 7 days"), "{}", home.body);
    assert!(home.body.contains(&ana.chars().take(4).collect::<String>()));
    assert!(!home.body.contains("data-hub-waiting"));

    // The form has the days only: the group is always requests.
    let with_group = post(
        page.clone(),
        "/hub/approve",
        Some(&cookie),
        &format!("id={id}&days=7&access_group=friends"),
    )
    .await;
    assert_ne!(with_group.status, StatusCode::OK);
    let store = NodeStore::in_dir(&data_dir).expect("store");
    assert_eq!(store.hub_approval(&id).expect("approval must read"), None);

    let approved =
        post(page.clone(), "/hub/approve", Some(&cookie), &format!("id={id}&days=7"))
            .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    let approval =
        store.hub_approval(&id).expect("approval must read").expect("approval");
    assert_eq!(approval.kind, aether_store::ApprovalKind::Request);
    assert_eq!(approval.access_group, "requests");
    assert_eq!(approval.peer_endpoint.as_deref(), Some(ana.as_str()));
    wait_for_delivery(&hub, &id).await;
    assert_eq!(group_members(&data_dir, "requests"), ["ana#0001"]);
    assert!(!group_members(&data_dir, "friends").contains(&"ana#0001".to_owned()));
    // Only the requester gets a hub contact.
    assert!(!hub.state().contacts.contains(&(service, ana)));
}

#[tokio::test]
async fn an_approval_whose_answer_is_lost_is_still_signed() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
    let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
    let ana = peer_endpoint(5);
    let id = {
        let mut state = hub.state();
        state.add_account(&ana, "ana#0001");
        let id = state.add_request(&service, &ana, 7);
        state.lose_answers.insert(format!("/v1/invites/{id}/approve"));
        id
    };
    // The hub accepts the row, but the page gets no answer.
    let approved =
        post(page.clone(), "/hub/approve", Some(&cookie), &format!("id={id}&days=7"))
            .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    assert!(approved.body.contains("cannot be reached"), "{}", approved.body);
    assert!(
        hub.state()
            .invites
            .iter()
            .any(|invite| invite.id == id && invite.status == "accepted")
    );
    // The owner approved the row, so the node keeps the approval and
    // signs the accepted row instead of leaving it stuck.
    let store = NodeStore::in_dir(&data_dir).expect("store");
    assert!(store.hub_approval(&id).expect("approval must read").is_some());
    wait_for_delivery(&hub, &id).await;
    assert_eq!(group_members(&data_dir, "requests"), ["ana#0001"]);
}

#[tokio::test]
async fn blocking_a_request_removes_it_and_keeps_the_block() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
    let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
    let ana = peer_endpoint(5);
    let id = {
        let mut state = hub.state();
        state.add_account(&ana, "ana#0001");
        state.add_request(&service, &ana, 7)
    };
    let blocked =
        post(page.clone(), "/hub/block-request", Some(&cookie), &format!("id={id}"))
            .await;
    assert_eq!(blocked.status, StatusCode::OK, "{}", blocked.body);
    assert!(blocked.body.contains("Blocked"));
    assert!(!hub.state().invites.iter().any(|invite| invite.id == id));
    assert!(hub.state().blocks.contains(&(service.clone(), ana.clone())));
    let store = NodeStore::in_dir(&data_dir).expect("store");
    assert!(store.hub_blocked(&ana).expect("block must read"));

    // A hub that sends an accepted row for the blocked key gets no line,
    // even with an approval for it.
    let forged = {
        let mut state = hub.state();
        let forged = state.add_request(&service, &ana, 7);
        if let Some(invite) =
            state.invites.iter_mut().find(|invite| invite.id == forged)
        {
            invite.status = "accepted".to_owned();
        }
        forged
    };
    store
        .put_hub_approval(&aether_store::HubApproval {
            invite_id: forged.clone(),
            side: aether_store::ApprovalSide::Sender,
            kind: aether_store::ApprovalKind::Request,
            peer_name: "ana#0001".to_owned(),
            peer_endpoint: Some(ana.clone()),
            days: 7,
            access_group: "requests".to_owned(),
            bundle: None,
            created_at: 1,
            expires_at: i64::MAX,
            listing: None,
        })
        .expect("approval must store");
    // A change made in the state wakes no long poll; any request does.
    let other = tempfile::tempdir().expect("directory must create");
    let _ = hub.client(other.path()).account().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while hub.state().invites.iter().any(|invite| invite.id == forged) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the worker must remove the row");
    assert!(group_members(&data_dir, "requests").is_empty());

    let unblocked =
        post(page.clone(), "/hub/unblock", Some(&cookie), &format!("endpoint={ana}"))
            .await;
    assert_eq!(unblocked.status, StatusCode::OK, "{}", unblocked.body);
    assert!(!store.hub_blocked(&ana).expect("block must read"));
}

#[tokio::test]
async fn an_unblock_this_computer_cannot_keep_says_so_and_can_be_retried() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
    let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
    let ana = peer_endpoint(5);
    let id = {
        let mut state = hub.state();
        state.add_account(&ana, "ana#0001");
        state.add_request(&service, &ana, 7)
    };
    let blocked =
        post(page.clone(), "/hub/block-request", Some(&cookie), &format!("id={id}"))
            .await;
    assert_eq!(blocked.status, StatusCode::OK, "{}", blocked.body);
    let database = data_dir.join(aether_store::NODE_DATABASE);
    let keep = "CREATE TRIGGER keep_blocks BEFORE DELETE ON hub_blocks \
                BEGIN SELECT RAISE(ABORT, 'kept'); END;";
    rusqlite::Connection::open(&database)
        .expect("database must open")
        .execute_batch(keep)
        .expect("trigger must create");

    let unblocked =
        post(page.clone(), "/hub/unblock", Some(&cookie), &format!("endpoint={ana}"))
            .await;
    assert_eq!(unblocked.status, StatusCode::OK, "{}", unblocked.body);
    assert!(
        unblocked.body.contains("could not remove the block"),
        "{}",
        unblocked.body
    );
    let store = NodeStore::in_dir(&data_dir).expect("store");
    assert!(store.hub_blocked(&ana).expect("block must read"));
    // The hub still lists the block, so the owner can try again.
    assert!(hub.state().blocks.contains(&(service.clone(), ana.clone())));

    rusqlite::Connection::open(&database)
        .expect("database must open")
        .execute_batch("DROP TRIGGER keep_blocks;")
        .expect("trigger must drop");
    let again =
        post(page.clone(), "/hub/unblock", Some(&cookie), &format!("endpoint={ana}"))
            .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert!(!store.hub_blocked(&ana).expect("block must read"));
    assert!(!hub.state().blocks.contains(&(service, ana)));
}

/// A host page for the personal agent `ana#0001` on `hub`.
async fn personal_page(
    data_dir: &std::path::Path,
    hub: &crate::hub_client::test_hub::TestHub,
) -> (axum::Router, String) {
    let page = hub_host_ui(data_dir, hub);
    let cookie = session(&page).await;
    post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=personal").await;
    (page, cookie)
}

#[tokio::test]
async fn a_personal_agent_claims_a_service_and_the_service_confirms() {
    let personal_dir = tempfile::tempdir().expect("directory must create");
    let service_dir = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (personal, personal_cookie) = personal_page(personal_dir.path(), &hub).await;
    let (service, service_cookie, _) = service_page(service_dir.path(), &hub).await;

    let home = get_with(personal.clone(), "/", &personal_cookie).await;
    for text in [
        "Your service agents",
        "When the service agent confirms, its public listing shows your full name.",
        "Everyone can see when your service agent is online.",
        "people can guess when you are online",
    ] {
        assert!(home.body.contains(text), "{text}: {}", home.body);
    }
    let claimed = post(
        personal.clone(),
        "/hub/owner/claim",
        Some(&personal_cookie),
        "service=law%230001",
    )
    .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    assert!(claimed.body.contains("waits for the service agent to confirm"));
    let id = {
        let state = hub.state();
        assert_eq!(state.owner_links.len(), 1);
        assert!(!state.owner_links[0].confirmed);
        state.owner_links[0].id.clone()
    };

    let claims = get_with(service.clone(), "/", &service_cookie).await;
    assert!(claims.body.contains("claims this agent."), "{}", claims.body);
    assert!(claims.body.contains("Confirm only a claim from your own personal agent."));
    let confirmed = post(
        service.clone(),
        "/hub/owner/confirm",
        Some(&service_cookie),
        &format!("id={id}"),
    )
    .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    assert!(hub.state().owner_links[0].confirmed);
    assert!(confirmed.body.contains("is the owner of this agent."));
    let linked = get_with(personal.clone(), "/", &personal_cookie).await;
    assert!(linked.body.contains("law#0001 (linked)"), "{}", linked.body);
}

#[tokio::test]
async fn a_personal_page_cannot_confirm_and_a_service_page_cannot_claim() {
    let personal_dir = tempfile::tempdir().expect("directory must create");
    let service_dir = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (personal, personal_cookie) = personal_page(personal_dir.path(), &hub).await;
    let (service, service_cookie, _) = service_page(service_dir.path(), &hub).await;
    post(
        personal.clone(),
        "/hub/owner/claim",
        Some(&personal_cookie),
        "service=law%230001",
    )
    .await;
    let id = hub.state().owner_links[0].id.clone();

    let confirm = post(
        personal.clone(),
        "/hub/owner/confirm",
        Some(&personal_cookie),
        &format!("id={id}"),
    )
    .await;
    assert_eq!(confirm.status, StatusCode::BAD_REQUEST);
    let claim = post(
        service.clone(),
        "/hub/owner/claim",
        Some(&service_cookie),
        "service=ana%230001",
    )
    .await;
    assert_eq!(claim.status, StatusCode::BAD_REQUEST);
    let state = hub.state();
    assert_eq!(state.owner_links.len(), 1);
    assert!(!state.owner_links[0].confirmed);
}

/// A caller page named `ana` on `hub`, and a host `bob` that invited it.
async fn invited_caller(
    data_dir: &std::path::Path,
    bob_dir: &std::path::Path,
    hub: &crate::hub_client::test_hub::TestHub,
) -> (axum::Router, String, crate::hub_client::HubClient, String) {
    let page = hub_caller_ui(data_dir, hub);
    let cookie = session(&page).await;
    post(page.clone(), "/hub/name", Some(&cookie), "name=ana").await;
    let bob = hub.client(bob_dir);
    bob.claim("bob", None).await.expect("bob must claim");
    bob.invite("ana#0001", 7).await.expect("bob must invite");
    (page, cookie, bob, crate::direct::endpoint_id(bob_dir).expect("endpoint"))
}

fn received_invite(hub: &crate::hub_client::test_hub::TestHub, from: &str) -> String {
    hub.state()
        .invites
        .iter()
        .find(|invite| invite.sender == from)
        .map(|invite| invite.id.clone())
        .expect("the invite must exist")
}

#[tokio::test]
async fn a_blocked_invite_sender_and_its_new_invites_do_not_show() {
    let directory = tempfile::tempdir().expect("directory must create");
    let bob_dir = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, bob, bob_endpoint) =
        invited_caller(directory.path(), bob_dir.path(), &hub).await;
    let wanted = "bob#0001 wants to share their agent with you";
    assert!(get_with(page.clone(), "/", &cookie).await.body.contains(wanted));

    let id = received_invite(&hub, &bob_endpoint);
    let blocked =
        post(page.clone(), "/hub/block", Some(&cookie), &format!("id={id}")).await;
    assert!(!blocked.body.contains(wanted), "{}", blocked.body);
    assert!(blocked.body.contains("Blocked"));
    assert!(blocked.body.contains("Unblock"));
    bob.invite("ana#0001", 7).await.expect("bob must invite again");
    assert!(!get_with(page.clone(), "/", &cookie).await.body.contains(wanted));

    post(
        page.clone(),
        "/hub/unblock",
        Some(&cookie),
        &format!("endpoint={bob_endpoint}"),
    )
    .await;
    bob.invite("ana#0001", 7).await.expect("bob must invite after the unblock");
    assert!(get_with(page.clone(), "/", &cookie).await.body.contains(wanted));
}

#[tokio::test]
async fn a_full_block_list_shows_the_limit_message() {
    let directory = tempfile::tempdir().expect("directory must create");
    let bob_dir = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (page, cookie, _, bob_endpoint) =
        invited_caller(directory.path(), bob_dir.path(), &hub).await;
    hub.state().block_limit = Some(0);
    let id = received_invite(&hub, &bob_endpoint);
    let refused =
        post(page.clone(), "/hub/block", Some(&cookie), &format!("id={id}")).await;
    assert!(
        refused.body.contains("You reached a hub limit. Remove an old item first."),
        "{}",
        refused.body
    );
}

#[tokio::test]
async fn an_old_hub_shows_the_older_hub_text_and_gets_no_claim() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    hub.state().old = true;
    let page = hub_host_ui(directory.path(), &hub);
    let cookie = session(&page).await;
    let claimed = post(page.clone(), "/hub/name", Some(&cookie), "name=tudor").await;
    assert!(
        claimed.body.contains("This hub is older than this node."),
        "{}",
        claimed.body
    );
    assert!(
        !hub.requests()
            .iter()
            .any(|request| request.method == "PUT" && request.target == "/v1/account")
    );
}

#[tokio::test]
async fn a_host_with_an_invite_name_still_signs_its_own_hub_invite() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (administration, node) = test_administration(directory.path());
    let host = test_host_page(
        directory.path().to_path_buf(),
        Some("server".to_owned()),
        None,
        Arc::new(|_, _| Box::pin(async { Ok(()) })),
        None,
    )
    .with_hub(Some(hub.client(directory.path())));
    crate::settings::store_setting::<crate::hub_client::settings::HubName>(
        directory.path(),
        &Some("tudor#0042".to_owned()),
    )
    .expect("hub name must store");
    let own = crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
    let ana = peer_endpoint(5);
    let id = {
        let mut state = hub.state();
        state.add_account(&own, "tudor#0042");
        state.add_account(&ana, "ana#0007");
        state.add_invite(&own, &ana, "accepted")
    };
    approve(directory.path(), &id, "ana#0007");
    let worker = tokio::spawn(crate::hub_workers::host_worker(
        administration,
        host.worker_context(),
    ));
    wait_for_delivery(&hub, &id).await;
    worker.abort();

    assert_eq!(node.granted().len(), 1);
    let line = hub
        .state()
        .invites
        .iter()
        .find(|invite| invite.id == id)
        .and_then(|invite| invite.bundle.clone())
        .expect("the line must be delivered");
    let bundle =
        crate::direct::InvitationBundle::decode(&line).expect("bundle must decode");
    let invitation = bundle.invitation.to_invitation().expect("invitation must verify");
    assert_eq!(invitation.agent_id, "tudor#0042");
    assert_eq!(bundle.authorization.claim.subject_endpoint_id, ana);
}

#[tokio::test]
async fn a_waiting_invite_on_the_agents_page_reloads_the_agents_page() {
    let directory = tempfile::tempdir().expect("directory must create");
    let hub = crate::hub_client::test_hub::TestHub::start().await;
    let (administration, _) = test_administration(directory.path());
    let page = test_host(directory.path(), Arc::new(std::sync::Mutex::new(Vec::new())))
        .with_caller(true)
        .with_hub(Some(hub.client(directory.path())));
    let own = crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
    let ana = peer_endpoint(5);
    {
        let mut state = hub.state();
        state.add_account(&own, "tudor#0042");
        state.add_account(&ana, "ana#0007");
        state.add_invite(&ana, &own, "accepted");
    }
    let options = UiOptions { page: Some(Arc::new(page)), ..UiOptions::default() };
    let application = app(build_admin_ui(administration, options, PORT));
    let agents = get(application, "/agents").await;
    assert_eq!(agents.status, StatusCode::OK);
    assert!(agents.body.contains("data-hub-waiting=\"/agents\""), "{}", agents.body);
}
