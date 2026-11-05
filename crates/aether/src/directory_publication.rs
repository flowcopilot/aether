//! The node's own record in a pinned directory, published at start and
//! renewed while the node runs.

use std::path::{Path, PathBuf};

use crate::control_service::WireRecord;

/// The directory refused the record, or the record cannot be made.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("directory publication failed")]
pub struct PublicationError;

/// What `aether node serve` publishes to its directory.
pub struct DirectoryPublication {
    pub directory: String,
    pub certificate: Vec<u8>,
    pub signing_key: [u8; 32],
    pub data_dir: PathBuf,
    pub agent_id: String,
    pub address: String,
    pub scope: String,
    // Mutex, not a plain field: an admin-triggered capability change (see
    // `publish_with_capability`) updates this on success so a later
    // periodic renewal keeps publishing the newly approved capability
    // instead of silently reverting to the one set at startup.
    pub capability: std::sync::Mutex<String>,
    pub ttl: u64,
}

impl DirectoryPublication {
    pub fn record(&self, endpoint_id: String) -> Result<WireRecord, PublicationError> {
        let capability = self.capability.lock().map_err(|_| PublicationError)?.clone();
        signed_node_record(
            &self.data_dir,
            self.agent_id.clone(),
            endpoint_id,
            self.address.clone(),
            self.scope.clone(),
            capability,
            self.ttl,
        )
    }

    pub async fn renew(&self, endpoint_id: &str) -> Result<(), PublicationError> {
        let record = self.record(endpoint_id.to_owned())?;
        let client = crate::directory_client::DirectoryClient::with_pinned_certificate(
            &self.directory,
            &self.certificate,
            self.signing_key,
        )
        .map_err(|_| PublicationError)?;
        client.renew(&record).await.map(|_| ()).map_err(|_| PublicationError)
    }

    /// Publishes the pinned directory record with `capability` replacing
    /// the current one, and only on success keeps it for future renewals
    /// (a failed attempt must not affect what the periodic renewer next
    /// republishes). This is the only concrete meaning "capability
    /// publication" has today: directory records are the sole vehicle for
    /// publishing a capability, and there is no separate capability-record
    /// primitive to invent one for.
    pub async fn publish_with_capability(
        &self,
        endpoint_id: &str,
        capability: String,
    ) -> Result<(), PublicationError> {
        let record = signed_node_record(
            &self.data_dir,
            self.agent_id.clone(),
            endpoint_id.to_owned(),
            self.address.clone(),
            self.scope.clone(),
            capability.clone(),
            self.ttl,
        )?;
        let client = crate::directory_client::DirectoryClient::with_pinned_certificate(
            &self.directory,
            &self.certificate,
            self.signing_key,
        )
        .map_err(|_| PublicationError)?;
        client.renew(&record).await.map_err(|_| PublicationError)?;
        *self.capability.lock().map_err(|_| PublicationError)? = capability;
        Ok(())
    }
}

/// A directory record for this node, signed with its discovery key.
pub fn signed_node_record(
    data_dir: &Path,
    agent_id: String,
    endpoint_id: String,
    address: String,
    scope: String,
    capability: String,
    expires_in: u64,
) -> Result<WireRecord, PublicationError> {
    let signing_key =
        crate::discovery::load_or_create_signing_key(data_dir.join("discovery.key"))
            .map_err(|_| PublicationError)?;
    let expires_at = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(expires_in))
        .ok_or(PublicationError)?;
    let record = crate::discovery::signed_record(
        &signing_key,
        agent_id,
        endpoint_id,
        vec![address],
        scope,
        vec![capability],
        expires_at,
    );
    Ok(WireRecord::from_record(&record))
}
