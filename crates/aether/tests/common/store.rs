use std::path::Path;

use aether::{
    direct::{self, DirectError},
    directory_client::{Failed, cache_wire_directory_record, cached_directory_record},
    discovery::{Invitation, Record, wire::WireInvitation},
};
use aether_store::{NodeStore, SignedRecord};

pub fn open_store(root: &Path) -> NodeStore {
    NodeStore::in_dir(root).expect("node store must open")
}

pub fn store_contact_invitation(
    store: &NodeStore,
    invitation: &Invitation,
) -> Result<(), DirectError> {
    let wire = WireInvitation::from_invitation(invitation);
    let expires_at =
        i64::try_from(wire.expires_at).map_err(|_| DirectError::Invalid)?;
    store
        .put_contact_record(&SignedRecord {
            scope_id: invitation.scope_id.clone(),
            agent_id: invitation.agent_id.clone(),
            record: serde_json::to_vec(&wire).map_err(|_| DirectError::Invalid)?,
            expires_at,
        })
        .map_err(|_| DirectError::Unavailable)?;
    Ok(())
}

pub fn load_contact(
    store: &NodeStore,
    agent_id: &str,
) -> Result<WireInvitation, DirectError> {
    direct::contact(store, agent_id)
}

pub fn store_directory_record(
    store: &NodeStore,
    record: &Record,
) -> Result<(), Failed> {
    cache_wire_directory_record(
        store,
        &aether::discovery::wire::WireRecord::from_record(record),
    )
}

pub fn load_directory_record(
    data_dir: &Path,
    scope_id: &str,
    agent_id: &str,
    trusted_key: [u8; 32],
) -> Result<Record, Failed> {
    cached_directory_record(data_dir, scope_id, agent_id, trusted_key)
}
