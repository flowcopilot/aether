//! The node's own record in a pinned directory, published at start and
//! renewed while the node runs.

pub mod cli;

use std::path::Path;

use ed25519_dalek::SigningKey;
use tokio::sync::Mutex;

use crate::{
    data_dir::DataDir, directory_client::DirectoryClient, discovery::wire::WireRecord,
};

/// The directory refused the record, or the record cannot be made.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("directory publication failed")]
pub struct PublicationError;

/// What `aether node serve` publishes to its directory.
pub struct DirectoryPublication {
    directory: String,
    client: DirectoryClient,
    signing_key: SigningKey,
    // An admin-triggered capability change (see `publish_with_capability`)
    // updates the capability on success, so a later periodic renewal keeps
    // the newly approved capability.
    record: Mutex<PublicationRecord>,
    // A copy of the record's ttl, which never changes, so reading it does
    // not wait for the lock.
    ttl: u64,
}

/// What a node publishes: its name, address, scope and capability, and how
/// long each record lives, in seconds.
pub struct PublicationRecord {
    pub agent_id: String,
    pub address: String,
    pub scope: String,
    pub capability: String,
    pub ttl: u64,
}

#[must_use]
pub fn valid_publication_fields(
    agent_id: &str,
    address: &str,
    scope: &str,
    capability: &str,
    ttl: u64,
    configured_scope: &str,
) -> bool {
    ttl >= 2
        && !agent_id.is_empty()
        && !scope.is_empty()
        && !capability.is_empty()
        && scope == configured_scope
        && address.parse::<std::net::SocketAddr>().is_ok()
}

impl DirectoryPublication {
    /// A publication to `directory`, reached through its pinned
    /// `certificate`. Records are signed with the discovery key of
    /// `data_dir`, so the client trusts only that key.
    ///
    /// # Errors
    ///
    /// [`PublicationError`] when the key cannot load or the client cannot
    /// pin the certificate.
    pub fn new(
        directory: String,
        certificate: &[u8],
        data_dir: &Path,
        record: PublicationRecord,
    ) -> Result<Self, PublicationError> {
        let signing_key =
            DataDir::new(data_dir).discovery_key().map_err(|_| PublicationError)?;
        let client = DirectoryClient::with_pinned_certificate(
            &directory,
            certificate,
            signing_key.verifying_key().to_bytes(),
        )
        .map_err(|_| PublicationError)?;
        Ok(Self {
            directory,
            client,
            signing_key,
            ttl: record.ttl,
            record: Mutex::new(record),
        })
    }

    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }

    /// How long each published record lives, in seconds.
    #[must_use]
    pub const fn ttl(&self) -> u64 {
        self.ttl
    }

    /// Publishes or renews the record of `endpoint_id`, and returns what the
    /// directory keeps.
    ///
    /// # Errors
    ///
    /// [`PublicationError`] when the record cannot be signed or the
    /// directory refuses it.
    pub async fn renew(
        &self,
        endpoint_id: &str,
    ) -> Result<WireRecord, PublicationError> {
        let record = self.record.lock().await;
        self.publish(&record, &record.capability, endpoint_id).await
    }

    /// Publishes the record with `capability` in place of the current one,
    /// and only on success keeps it for future renewals. Directory records
    /// are the only way a capability is published.
    ///
    /// # Errors
    ///
    /// [`PublicationError`] when the record cannot be signed or the
    /// directory refuses it.
    pub async fn publish_with_capability(
        &self,
        endpoint_id: &str,
        capability: String,
    ) -> Result<(), PublicationError> {
        let mut record = self.record.lock().await;
        self.publish(&record, &capability, endpoint_id).await?;
        record.capability = capability;
        drop(record);
        Ok(())
    }

    /// Signs and publishes `record` with `capability`. Callers hold the
    /// record's lock until the directory answers, so a renewal that signed
    /// the old capability cannot land after a capability change.
    async fn publish(
        &self,
        record: &PublicationRecord,
        capability: &str,
        endpoint_id: &str,
    ) -> Result<WireRecord, PublicationError> {
        let signed = signed_record(&self.signing_key, record, capability, endpoint_id)?;
        self.client.publish(&signed).await.map_err(|_| PublicationError)
    }
}

/// A directory record for the node in `data_dir`, signed with its
/// discovery key.
///
/// # Errors
///
/// [`PublicationError`] when the key cannot load or the expiry is past the
/// clock's end.
pub fn signed_node_record(
    data_dir: &Path,
    record: &PublicationRecord,
    endpoint_id: &str,
) -> Result<WireRecord, PublicationError> {
    let signing_key =
        DataDir::new(data_dir).discovery_key().map_err(|_| PublicationError)?;
    signed_record(&signing_key, record, &record.capability, endpoint_id)
}

/// `record` signed with `capability` in place of its own.
fn signed_record(
    signing_key: &SigningKey,
    record: &PublicationRecord,
    capability: &str,
    endpoint_id: &str,
) -> Result<WireRecord, PublicationError> {
    let expires_at = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(record.ttl))
        .ok_or(PublicationError)?;
    let record = crate::discovery::signed_record(
        signing_key,
        record.agent_id.clone(),
        endpoint_id.to_owned(),
        vec![record.address.clone()],
        record.scope.clone(),
        vec![capability.to_owned()],
        expires_at,
    );
    Ok(WireRecord::from_record(&record))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::{net::TcpListener, time::timeout};

    use super::{DirectoryPublication, PublicationRecord};

    #[tokio::test]
    async fn capability_change_waits_for_an_in_flight_renewal() {
        let data_dir = tempfile::tempdir().expect("directory must create");
        // A directory that accepts connections and never answers, so a
        // request stays in flight until its connection is dropped.
        let directory =
            TcpListener::bind("127.0.0.1:0").await.expect("listener must bind");
        let port =
            directory.local_addr().expect("listener must have an address").port();
        let certificate =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
                .expect("certificate must generate")
                .cert;
        let publication = Arc::new(
            DirectoryPublication::new(
                format!("https://127.0.0.1:{port}"),
                certificate.der(),
                data_dir.path(),
                PublicationRecord {
                    agent_id: "agent".to_owned(),
                    address: "127.0.0.1:9443".to_owned(),
                    scope: "scope".to_owned(),
                    capability: "old".to_owned(),
                    ttl: 60,
                },
            )
            .expect("publication must build"),
        );

        let renewal = tokio::spawn({
            let publication = Arc::clone(&publication);
            async move { publication.renew("endpoint").await }
        });
        let (renewal_connection, _) =
            timeout(Duration::from_secs(5), directory.accept())
                .await
                .expect("renewal must connect")
                .expect("renewal connection must open");
        let change = tokio::spawn({
            let publication = Arc::clone(&publication);
            async move {
                publication.publish_with_capability("endpoint", "new".to_owned()).await
            }
        });
        assert!(
            timeout(Duration::from_millis(300), directory.accept()).await.is_err(),
            "a capability change must not publish while a renewal is in flight"
        );

        drop(renewal_connection);
        assert!(renewal.await.expect("renewal task must finish").is_err());
        timeout(Duration::from_secs(5), directory.accept())
            .await
            .expect("capability change must publish once the renewal ends")
            .expect("capability change connection must open");
        change.abort();
    }
}
