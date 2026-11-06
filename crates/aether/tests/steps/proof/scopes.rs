//! Steps for how a caller becomes known in a scope and what a grant admits.

use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use aether_core::{Admission, EndpointId, Grant, PolicyEngine};
use aether_store::NodeStore;
use cucumber::{given, then, when};
use tempfile::TempDir;

use super::fixtures::{
    CALLER, CALLER_ENDPOINT, blocking, claim, grant, invitation, managed_member,
    record, request, scope, trust_provider,
};
use crate::common::{
    discovery_signing_key, load_contact, load_directory_record,
    store_contact_invitation, store_directory_record,
};
use crate::steps::World;

type Store = Arc<Mutex<NodeStore>>;

#[derive(Default)]
pub struct State {
    store: Option<(TempDir, Store)>,
    grants: Vec<Grant>,
    admission: Option<Admission>,
}

impl fmt::Debug for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("State")
            .field("grants", &self.grants)
            .field("admission", &self.admission)
            .finish_non_exhaustive()
    }
}

impl State {
    async fn data_and_store(&mut self) -> (PathBuf, Store) {
        if self.store.is_none() {
            let opened = blocking(|| {
                let root =
                    tempfile::tempdir().expect("temporary directory must create");
                let store =
                    NodeStore::in_dir(root.path()).expect("node store must open");
                (root, Arc::new(Mutex::new(store)))
            })
            .await;
            self.store = Some(opened);
        }
        let (root, store) = self.store.as_ref().expect("the store must be open");
        (root.path().to_path_buf(), Arc::clone(store))
    }
}

#[given(expr = "the caller is a known peer in the {string} scope")]
async fn known_peer(world: &mut World, scope_id: String) {
    let state = &mut world.proof.scopes;
    let (_, store) = state.data_and_store().await;
    match scope_id.as_str() {
        "direct" => {
            let invitation = invitation("direct", CALLER, CALLER_ENDPOINT);
            blocking(move || {
                store_contact_invitation(
                    &store.lock().expect("store lock"),
                    &invitation,
                )
                .expect("direct invitation must store");
            })
            .await;
        }
        "directory" => {
            let signed = record("directory", CALLER, CALLER_ENDPOINT, "127.0.0.1:1");
            blocking(move || {
                store_directory_record(&store.lock().expect("store lock"), &signed)
                    .expect("directory record must store");
            })
            .await;
        }
        "managed" => {
            blocking(move || {
                store
                    .lock()
                    .expect("store lock")
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
    let (root, store) = state.data_and_store().await;
    match scope_id.as_str() {
        "direct" => {
            let wire = blocking(move || {
                load_contact(&store.lock().expect("store lock"), CALLER)
            })
            .await
            .expect("direct contact must load");
            assert_eq!(wire.scope_id, "direct");
        }
        "directory" => {
            let data_dir = root;
            let trusted = discovery_signing_key().verifying_key().to_bytes();
            let loaded = blocking(move || {
                load_directory_record(&data_dir, "directory", CALLER, trusted)
            })
            .await
            .expect("directory record must load");
            assert_eq!(loaded.scope_id, "directory");
        }
        "managed" => {
            let members = blocking(move || {
                store.lock().expect("store lock").managed_members("managed")
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
    let (_, store) = world.proof.scopes.data_and_store().await;
    blocking(move || {
        store
            .lock()
            .expect("store lock")
            .revoke_managed_member("https://control.test", 2, "managed", CALLER)
            .expect("membership must revoke");
    })
    .await;
}

#[then("the managed roster marks the caller as revoked")]
async fn roster_marks_revoked(world: &mut World) {
    let (_, store) = world.proof.scopes.data_and_store().await;
    let members =
        blocking(move || store.lock().expect("store lock").managed_members("managed"))
            .await
            .expect("roster must load");
    let member = members.first().expect("managed roster must contain member");
    assert!(member.revoked_at.is_some());
}
