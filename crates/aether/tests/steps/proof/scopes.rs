//! Steps for how a caller becomes known in a scope and what a grant admits.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use aether_core::{Admission, EndpointId, Grant, PolicyEngine};
use aether_store::NodeStore;
use cucumber::{given, then, when};
use tempfile::TempDir;

use super::fixtures::{
    CALLER, CALLER_ENDPOINT, DirectContacts, Directory, blocking, claim, grant,
    invitation, managed_member, record, request, scope, trust_provider,
};
use crate::steps::World;

type Roster = Arc<Mutex<NodeStore>>;

#[derive(Default)]
pub struct State {
    contacts: DirectContacts,
    directory: Directory,
    /// The managed roster, kept in a store on disk as a node keeps it.
    roster: Option<(TempDir, Roster)>,
    grants: Vec<Grant>,
    admission: Option<Admission>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("contacts", &self.contacts)
            .field("directory", &self.directory)
            .field("grants", &self.grants)
            .field("admission", &self.admission)
            .finish_non_exhaustive()
    }
}

impl State {
    async fn roster(&mut self) -> Roster {
        if self.roster.is_none() {
            let opened = blocking(|| {
                let root =
                    tempfile::tempdir().expect("temporary directory must create");
                let store =
                    NodeStore::open(root.path().join(aether_store::NODE_DATABASE))
                        .expect("managed roster must open");
                (root, Arc::new(Mutex::new(store)))
            })
            .await;
            self.roster = Some(opened);
        }
        let (_, roster) = self.roster.as_ref().expect("the roster must be open");
        Arc::clone(roster)
    }
}

#[given(expr = "the caller is a known peer in the {string} scope")]
async fn known_peer(world: &mut World, scope_id: String) {
    let state = &mut world.proof.scopes;
    match scope_id.as_str() {
        "direct" => state
            .contacts
            .import(invitation("direct", CALLER, CALLER_ENDPOINT))
            .expect("direct invitation must import"),
        "directory" => state
            .directory
            .publish(record("directory", CALLER, CALLER_ENDPOINT, "127.0.0.1:1"))
            .expect("directory record must publish"),
        "managed" => {
            let roster = state.roster().await;
            blocking(move || {
                roster
                    .lock()
                    .expect("roster lock")
                    .apply_managed_member(
                        "https://control.test",
                        1,
                        &managed_member(CALLER, CALLER_ENDPOINT, "managed"),
                    )
                    .expect("managed membership must apply");
            })
            .await;
        }
        other => unreachable!("unknown scope {other}"),
    }
}

#[then(expr = "the {string} scope lists the caller")]
async fn scope_lists_caller(world: &mut World, scope_id: String) {
    let state = &mut world.proof.scopes;
    match scope_id.as_str() {
        "direct" => assert_eq!(
            state.contacts.resolve(CALLER).expect("direct contact must exist").scope_id,
            "direct"
        ),
        "directory" => assert_eq!(
            state
                .directory
                .resolve(CALLER)
                .expect("directory record must resolve")
                .scope_id,
            "directory"
        ),
        "managed" => {
            let roster = state.roster().await;
            let members = blocking(move || {
                roster.lock().expect("roster lock").managed_members("managed")
            })
            .await
            .expect("managed roster must load");
            let member = members.first().expect("managed roster must contain member");
            assert_eq!(member.scope_id, "managed");
            assert_eq!(member.agent_id, CALLER);
            assert_eq!(member.endpoint_id, CALLER_ENDPOINT);
            assert!(member.revoked_at.is_none());
        }
        other => unreachable!("unknown scope {other}"),
    }
}

#[given(expr = "the caller holds a review grant for the {string} scope")]
fn holds_grant(world: &mut World, scope_id: String) {
    world.proof.scopes.grants.push(grant(&scope(&scope_id)));
}

#[when(expr = "the caller asks to review in the {string} scope")]
fn asks_to_review(world: &mut World, scope_id: String) {
    let state = &mut world.proof.scopes;
    let scope = scope(&scope_id);
    let request = request(&scope);
    let admission = PolicyEngine::new(trust_provider(), state.grants.clone()).admit(
        &request,
        &claim(&scope),
        &scope,
        &EndpointId::new(CALLER_ENDPOINT),
    );
    state.admission = Some(admission);
}

#[then("the node approves the request")]
fn approves(world: &mut World) {
    assert_eq!(world.proof.scopes.admission, Some(Admission::Approved));
}

#[then("the node does not approve the request")]
fn does_not_approve(world: &mut World) {
    let admission = world.proof.scopes.admission.as_ref().expect("the caller must ask");
    assert_ne!(admission, &Admission::Approved);
}

#[when("the hub revokes the caller from the managed scope")]
async fn hub_revokes(world: &mut World) {
    let roster = world.proof.scopes.roster().await;
    blocking(move || {
        roster
            .lock()
            .expect("roster lock")
            .revoke_managed_member("https://control.test", 2, "managed", CALLER)
            .expect("membership must revoke");
    })
    .await;
}

#[then("the managed roster marks the caller as revoked")]
async fn roster_marks_revoked(world: &mut World) {
    let roster = world.proof.scopes.roster().await;
    let members = blocking(move || {
        roster.lock().expect("roster lock").managed_members("managed")
    })
    .await
    .expect("roster must load");
    let member = members.first().expect("managed roster must contain member");
    assert!(member.revoked_at.is_some());
}
