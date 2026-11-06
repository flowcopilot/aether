use aether_store::{DirectoryEventType, ManagedEventType, NodeStore};

use crate::discovery::wire::{WireDirectoryEvent, WireManagedEvent, WireRecord};

#[allow(clippy::result_unit_err)]
pub fn apply_directory_change(
    store: &NodeStore,
    change: &WireDirectoryEvent,
) -> Result<(), ()> {
    match change.event {
        DirectoryEventType::Published | DirectoryEventType::Renewed => {
            let record = change.record.as_ref().ok_or(())?;
            cache_wire_directory_record(store, record)
        }
        DirectoryEventType::Revoked => store
            .mark_directory_record_stale(&change.scope_id, &change.agent_id)
            .map(|_| ())
            .map_err(|_| ()),
    }
}

#[allow(clippy::result_unit_err)]
pub fn apply_managed_change(
    store: &mut NodeStore,
    control: &str,
    event: &WireManagedEvent,
) -> Result<(), ()> {
    match event.event {
        ManagedEventType::Joined => {
            let member = event.member.as_ref().ok_or(())?;
            store.apply_managed_member(control, event.cursor, member).map_err(|_| ())
        }
        ManagedEventType::Revoked if event.member.is_none() => store
            .revoke_managed_member(
                control,
                event.cursor,
                &event.scope_id,
                &event.agent_id,
            )
            .map_err(|_| ()),
        ManagedEventType::Revoked => Err(()),
    }
}

#[allow(clippy::result_unit_err)]
fn cache_wire_directory_record(
    store: &NodeStore,
    record: &WireRecord,
) -> Result<(), ()> {
    store
        .cache_directory_record(&aether_store::SignedRecord {
            scope_id: record.scope_id.clone(),
            agent_id: record.agent_id.clone(),
            record: serde_json::to_vec(record).map_err(|_| ())?,
            expires_at: i64::try_from(record.expires_at).map_err(|_| ())?,
        })
        .map_err(|_| ())
}
