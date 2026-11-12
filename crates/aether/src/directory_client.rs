pub mod cli;

use std::path::Path;

use hex::FromHex;
use reqwest::StatusCode;

use aether_store::DirectoryEventType;

use crate::{
    discovery::{
        DiscoveryError, validate_record,
        wire::{self, WireDirectoryEvent, WireRecord},
    },
    pinned_json::{self, HttpError, PinnedJsonClient},
    transport::{self, Frame, TransportError},
};

#[derive(Debug, thiserror::Error)]
pub enum DirectoryClientError {
    #[error("directory URL must be an HTTPS origin")]
    InvalidDirectory,
    #[error("directory certificate is invalid")]
    InvalidCertificate,
    #[error("directory request failed")]
    Request,
    #[error("directory returned an unexpected status")]
    Status,
    #[error("directory returned an invalid signed record")]
    InvalidRecord,
    #[error("directory record signer is not trusted")]
    UntrustedRecord,
    #[error("directory record has expired")]
    ExpiredRecord,
}

impl From<HttpError> for DirectoryClientError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Origin => Self::InvalidDirectory,
            HttpError::Certificate => Self::InvalidCertificate,
            HttpError::Client | HttpError::Request => Self::Request,
            HttpError::Status => Self::Status,
            HttpError::Body => Self::InvalidRecord,
        }
    }
}

#[derive(Clone)]
pub struct DirectoryClient {
    http: PinnedJsonClient,
    trusted_signing_key: [u8; 32],
}

impl DirectoryClient {
    pub fn with_pinned_certificate(
        directory: &str,
        certificate_der: &[u8],
        trusted_signing_key: [u8; 32],
    ) -> Result<Self, DirectoryClientError> {
        Ok(Self {
            http: PinnedJsonClient::open(directory, certificate_der)?,
            trusted_signing_key,
        })
    }

    pub async fn publish(
        &self,
        record: &WireRecord,
    ) -> Result<WireRecord, DirectoryClientError> {
        let record = pinned_json::json(
            self.http.client().post(self.records_url()?).json(record),
            &[StatusCode::CREATED, StatusCode::OK],
        )
        .await?;
        self.validate(&record)?;
        Ok(record)
    }

    pub async fn resolve(
        &self,
        agent_id: &str,
    ) -> Result<WireRecord, DirectoryClientError> {
        let record = pinned_json::json(
            self.http.client().get(self.record_url(agent_id)?),
            &[StatusCode::OK],
        )
        .await?;
        self.validate_resolved(agent_id, &record)?;
        Ok(record)
    }

    pub async fn search(
        &self,
        capability: &str,
    ) -> Result<Vec<WireRecord>, DirectoryClientError> {
        if capability.is_empty() {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let records: Vec<WireRecord> = pinned_json::json(
            self.http
                .client()
                .get(self.records_url()?)
                .query(&[("capability", capability)]),
            &[StatusCode::OK],
        )
        .await?;
        for record in &records {
            self.validate(record)?;
        }
        Ok(records)
    }

    pub async fn revoke(
        &self,
        record: &WireRecord,
    ) -> Result<(), DirectoryClientError> {
        pinned_json::send(
            self.http.client().delete(self.record_url(&record.agent_id)?).json(record),
            &[StatusCode::NO_CONTENT],
        )
        .await?;
        Ok(())
    }

    /// The events of the directory after `cursor`, in order.
    ///
    /// The directory keeps each event for ever, and it takes a record of
    /// any key for a free agent ID. So an honest directory also sends a
    /// record that has expired since, or one that another key signed. Such
    /// an event comes back without its record: the caller has nothing to
    /// keep, and moves its cursor past the event.
    pub async fn events_after(
        &self,
        cursor: i64,
    ) -> Result<Vec<WireDirectoryEvent>, DirectoryClientError> {
        if cursor < 0 {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let mut events: Vec<WireDirectoryEvent> = pinned_json::json(
            self.http
                .client()
                .get(self.events_url()?)
                .query(&[("after", &cursor.to_string())]),
            &[StatusCode::OK],
        )
        .await?;
        if !pinned_json::monotonic_cursors(
            cursor,
            events.iter().map(|event| event.cursor),
        ) {
            return Err(DirectoryClientError::InvalidRecord);
        }
        for event in &mut events {
            if !self.validate_event(event)? {
                event.record = None;
            }
        }
        Ok(events)
    }

    fn records_url(&self) -> Result<reqwest::Url, DirectoryClientError> {
        self.http.url(&wire::RECORDS).map_err(Into::into)
    }

    fn record_url(&self, agent_id: &str) -> Result<reqwest::Url, DirectoryClientError> {
        PinnedJsonClient::require_segment(agent_id)?;
        self.http
            .url(&[wire::RECORDS[0], wire::RECORDS[1], agent_id])
            .map_err(Into::into)
    }

    fn events_url(&self) -> Result<reqwest::Url, DirectoryClientError> {
        self.http.url(&wire::EVENTS).map_err(Into::into)
    }

    fn validate(&self, record: &WireRecord) -> Result<(), DirectoryClientError> {
        let record =
            record.to_record().map_err(|_| DirectoryClientError::InvalidRecord)?;
        if record.signing_key != self.trusted_signing_key {
            return Err(DirectoryClientError::UntrustedRecord);
        }
        validate_record(&record).map_err(|error| match error {
            DiscoveryError::Expired => DirectoryClientError::ExpiredRecord,
            DiscoveryError::Forged | DiscoveryError::Malformed => {
                DirectoryClientError::InvalidRecord
            }
        })
    }

    /// Checks the answer of the directory to a question about `agent_id`.
    /// The directory chooses the record that it sends, so a valid record of
    /// another agent is refused here.
    fn validate_resolved(
        &self,
        agent_id: &str,
        record: &WireRecord,
    ) -> Result<(), DirectoryClientError> {
        if record.agent_id != agent_id {
            return Err(DirectoryClientError::InvalidRecord);
        }
        self.validate(record)
    }

    /// Whether the caller can keep the record of `event`. `false` for a
    /// record that expired or that another key signed; an event that is
    /// malformed, or whose signature does not fit, is an error.
    fn validate_event(
        &self,
        event: &WireDirectoryEvent,
    ) -> Result<bool, DirectoryClientError> {
        if event.scope_id.is_empty() {
            return Err(DirectoryClientError::InvalidRecord);
        }
        self.record_url(&event.agent_id)?;
        match event.event {
            DirectoryEventType::Published | DirectoryEventType::Renewed => {
                let record =
                    event.record.as_ref().ok_or(DirectoryClientError::InvalidRecord)?;
                if record.agent_id != event.agent_id
                    || record.scope_id != event.scope_id
                {
                    return Err(DirectoryClientError::InvalidRecord);
                }
                match self.validate(record) {
                    Ok(()) => Ok(true),
                    Err(
                        DirectoryClientError::UntrustedRecord
                        | DirectoryClientError::ExpiredRecord,
                    ) => Ok(false),
                    Err(error) => Err(error),
                }
            }
            DirectoryEventType::Revoked if event.record.is_none() => Ok(true),
            DirectoryEventType::Revoked => Err(DirectoryClientError::InvalidRecord),
        }
    }
}

/// A local read, a store write or a synchronization failed. Callers only
/// report that it did, so the cause is dropped.
///
/// It is not a [`std::error::Error`] itself, so any error converts into
/// it with `?`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Failed;

impl<E: std::error::Error> From<E> for Failed {
    fn from(_: E) -> Self {
        Self
    }
}

impl From<crate::discovery::DiscoveryError> for Failed {
    fn from(_: crate::discovery::DiscoveryError) -> Self {
        Self
    }
}

pub fn read_authorization(path: &Path) -> Result<transport::A2aAuthorization, Failed> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn read_verifying_key(path: &Path) -> Result<[u8; 32], Failed> {
    Ok(<[u8; 32]>::from_hex(std::fs::read_to_string(path)?.trim())?)
}

pub fn read_directory_record(path: &Path) -> Result<WireRecord, Failed> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn cache_directory_record(
    data_dir: &Path,
    record: &crate::discovery::Record,
) -> Result<(), Failed> {
    let store = aether_store::NodeStore::in_dir(data_dir)?;
    cache_wire_directory_record(&store, &WireRecord::from_record(record))
}

pub fn cache_wire_directory_record(
    store: &aether_store::NodeStore,
    record: &WireRecord,
) -> Result<(), Failed> {
    Ok(store.cache_directory_record(&record.signed_store_record()?)?)
}

pub async fn sync_directory(
    data_dir: &Path,
    directory: &str,
    trusted_key: [u8; 32],
    client: &DirectoryClient,
) -> Result<(i64, usize), Failed> {
    std::fs::create_dir_all(data_dir)?;
    let store = aether_store::NodeStore::in_dir(data_dir)?;
    let mut cursor = store.directory_cursor(directory, &trusted_key)?;
    let mut records = 0usize;
    loop {
        let changes = client.events_after(cursor).await?;
        if changes.is_empty() {
            return Ok((cursor, records));
        }
        // `events_after` refuses a page whose cursors do not climb past
        // `cursor`, and an event whose record does not match its type, so
        // each change moves the cursor forward and needs no further check.
        // It takes the record out of an event that this node cannot use.
        for change in changes {
            match change.event {
                DirectoryEventType::Published | DirectoryEventType::Renewed => {
                    if let Some(record) = change.record.as_ref() {
                        cache_wire_directory_record(&store, record)?;
                        records = records.checked_add(1).ok_or(Failed)?;
                    }
                }
                DirectoryEventType::Revoked => {
                    store.mark_directory_record_stale(
                        &change.scope_id,
                        &change.agent_id,
                    )?;
                }
            }
            cursor = change.cursor;
            store.advance_directory_cursor(directory, &trusted_key, cursor)?;
        }
    }
}

pub fn cached_directory_record(
    data_dir: &Path,
    scope_id: &str,
    agent_id: &str,
    trusted_key: [u8; 32],
) -> Result<crate::discovery::Record, Failed> {
    let store = aether_store::NodeStore::in_dir(data_dir)?;
    let stored = store.directory_record(scope_id, agent_id)?;
    if stored.scope_id != scope_id || stored.agent_id != agent_id {
        return Err(Failed);
    }
    if stored.expires_at <= aether_core::time::now() {
        return Err(Failed);
    }
    let wire: WireRecord = serde_json::from_slice(&stored.record)?;
    let record = wire.to_record()?;
    if record.signing_key != trusted_key {
        return Err(Failed);
    }
    validate_record(&record)?;
    Ok(record)
}

#[allow(clippy::match_same_arms)]
pub async fn request_via_directory(
    data_dir: &Path,
    directory: &str,
    certificate: &Path,
    key: &Path,
    agent_id: &str,
    scope: &str,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    let certificate =
        std::fs::read(certificate).map_err(|_| TransportError::Address)?;
    let trusted_key = read_verifying_key(key).map_err(|_| TransportError::Address)?;
    let client =
        DirectoryClient::with_pinned_certificate(directory, &certificate, trusted_key)
            .map_err(|_| TransportError::Address)?;
    let record = match client.resolve(agent_id).await.and_then(|wire| {
        wire.to_record().map_err(|_| DirectoryClientError::InvalidRecord)
    }) {
        Ok(record) if record.scope_id == scope => {
            if cache_directory_record(data_dir, &record).is_err() {
                return Err(TransportError::Network(
                    "local directory state is unavailable".to_owned(),
                ));
            }
            record
        }
        Ok(_) => return Err(TransportError::Address),
        Err(DirectoryClientError::Request) => {
            cached_directory_record(data_dir, scope, agent_id, trusted_key)
                .map_err(|_| TransportError::Address)?
        }
        Err(_) => return Err(TransportError::Address),
    };
    transport::request_once_record(data_dir, &record, request).await
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use aether_store::DirectoryEventType;
    use axum_server::Handle;

    use super::{
        DirectoryClient, DirectoryClientError, cached_directory_record, sync_directory,
    };
    use crate::{
        control_service::serve,
        discovery::{
            signed_record,
            wire::{WireDirectoryEvent, WireRecord},
        },
    };
    use ed25519_dalek::SigningKey;

    fn trusted_signing_key() -> [u8; 32] {
        SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes()
    }

    #[test]
    fn trusted_client_requires_an_https_origin() {
        assert!(matches!(
            DirectoryClient::with_pinned_certificate(
                "http://directory.test",
                b"invalid",
                trusted_signing_key(),
            ),
            Err(DirectoryClientError::InvalidDirectory)
        ));
    }

    #[test]
    fn record_ids_are_one_encoded_path_segment() {
        let client = DirectoryClient::with_pinned_certificate(
            "https://directory.test",
            rcgen::generate_simple_self_signed(vec!["directory.test".to_owned()])
                .expect("certificate must generate")
                .cert
                .der(),
            trusted_signing_key(),
        )
        .expect("client must build");
        for (id, path) in [
            ("server", "/v1/records/server"),
            ("a?b#c", "/v1/records/a%3Fb%23c"),
            ("50%", "/v1/records/50%25"),
            ("server \"one\"", "/v1/records/server%20%22one%22"),
            ("a\\b", "/v1/records/a%5Cb"),
        ] {
            let url = client.record_url(id).expect("id must build a URL");
            assert_eq!(url.path(), path, "{id}");
            assert_eq!(url.query(), None, "{id}");
            assert_eq!(url.fragment(), None, "{id}");
        }
        for id in ["", ".", "..", "a/b", "a\0b"] {
            assert!(
                matches!(
                    client.record_url(id),
                    Err(DirectoryClientError::InvalidRecord)
                ),
                "{id:?}"
            );
        }
    }

    #[test]
    fn signed_records_are_verified_before_use() {
        let mut record = WireRecord::test_record();
        let client = DirectoryClient::with_pinned_certificate(
            "https://directory.test",
            rcgen::generate_simple_self_signed(vec!["directory.test".to_owned()])
                .expect("certificate must generate")
                .cert
                .der(),
            trusted_signing_key(),
        )
        .expect("client must build");
        client.validate(&record).expect("record must verify");
        record.endpoint_id = "changed".to_owned();
        assert!(matches!(
            client.validate(&record),
            Err(DirectoryClientError::InvalidRecord)
        ));
        let untrusted = DirectoryClient::with_pinned_certificate(
            "https://directory.test",
            rcgen::generate_simple_self_signed(vec!["directory.test".to_owned()])
                .expect("certificate must generate")
                .cert
                .der(),
            [0; 32],
        )
        .expect("client must build");
        assert!(matches!(
            untrusted.validate(&WireRecord::test_record()),
            Err(DirectoryClientError::UntrustedRecord)
        ));
    }

    fn offline_client() -> DirectoryClient {
        DirectoryClient::with_pinned_certificate(
            "https://directory.test",
            rcgen::generate_simple_self_signed(vec!["directory.test".to_owned()])
                .expect("certificate must generate")
                .cert
                .der(),
            trusted_signing_key(),
        )
        .expect("client must build")
    }

    fn record_of(key: u8, agent_id: &str, expires_at: SystemTime) -> WireRecord {
        WireRecord::from_record(&signed_record(
            &SigningKey::from_bytes(&[key; 32]),
            agent_id.to_owned(),
            "endpoint".to_owned(),
            vec!["127.0.0.1:9443".to_owned()],
            "test-scope".to_owned(),
            vec!["test".to_owned()],
            expires_at,
        ))
    }

    fn published(record: WireRecord) -> WireDirectoryEvent {
        WireDirectoryEvent {
            cursor: 1,
            event: DirectoryEventType::Published,
            scope_id: record.scope_id.clone(),
            agent_id: record.agent_id.clone(),
            record: Some(record),
        }
    }

    #[test]
    fn a_resolved_record_of_another_agent_is_refused() {
        let client = offline_client();
        let record = WireRecord::test_record();
        client
            .validate_resolved("test-agent", &record)
            .expect("the record of the agent must pass");
        // A directory that answers with a valid record of another agent.
        assert!(matches!(
            client.validate_resolved("other-agent", &record),
            Err(DirectoryClientError::InvalidRecord)
        ));
    }

    #[test]
    fn an_event_with_a_record_that_cannot_be_used_is_skipped_not_refused() {
        let client = offline_client();
        let soon = SystemTime::now() + Duration::from_secs(60);
        let past = SystemTime::now() - Duration::from_secs(60);
        assert!(matches!(
            client.validate_event(&published(record_of(7, "agent", soon))),
            Ok(true)
        ));
        // The record expired after the directory took it.
        assert!(matches!(
            client.validate_event(&published(record_of(7, "agent", past))),
            Ok(false)
        ));
        // The directory takes a record of any key for a free agent ID.
        assert!(matches!(
            client.validate_event(&published(record_of(8, "agent", soon))),
            Ok(false)
        ));
        let mut forged = published(record_of(7, "agent", soon));
        if let Some(record) = forged.record.as_mut() {
            record.endpoint_id = "changed".to_owned();
        }
        assert!(matches!(
            client.validate_event(&forged),
            Err(DirectoryClientError::InvalidRecord)
        ));
        let mut misfiled = published(record_of(7, "agent", soon));
        misfiled.agent_id = "other".to_owned();
        assert!(matches!(
            client.validate_event(&misfiled),
            Err(DirectoryClientError::InvalidRecord)
        ));
    }

    #[tokio::test]
    async fn a_record_of_another_key_in_the_log_does_not_stop_the_sync() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("control");
        let node_dir = directory.path().join("node");
        let handle = Handle::new();
        let server = tokio::spawn({
            let handle = handle.clone();
            let data_dir = data_dir.clone();
            async move { serve(&data_dir, 0, handle).await }
        });
        let address = tokio::time::timeout(Duration::from_secs(5), handle.listening())
            .await
            .expect("server must listen")
            .expect("server address must exist");
        let certificate =
            fs::read(crate::data_dir::DataDir::new(&data_dir).control_certificate())
                .expect("certificate must exist");
        let url = format!("https://127.0.0.1:{}", address.port());
        let client = DirectoryClient::with_pinned_certificate(
            &url,
            &certificate,
            trusted_signing_key(),
        )
        .expect("client must trust the directory");
        let soon = SystemTime::now() + Duration::from_secs(60);
        let stranger = DirectoryClient::with_pinned_certificate(
            &url,
            &certificate,
            SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
        )
        .expect("client must build");
        stranger
            .publish(&record_of(8, "stranger", soon))
            .await
            .expect("the directory takes a record of any key");
        let listed = record_of(7, "listed", soon);
        client.publish(&listed).await.expect("record must publish");

        let (cursor, records) =
            sync_directory(&node_dir, &url, trusted_signing_key(), &client)
                .await
                .expect("the sync must pass the record of the other key");
        assert_eq!((cursor, records), (2, 1));
        assert!(
            cached_directory_record(
                &node_dir,
                "test-scope",
                "listed",
                trusted_signing_key()
            )
            .is_ok()
        );
        assert!(
            cached_directory_record(
                &node_dir,
                "test-scope",
                "stranger",
                trusted_signing_key()
            )
            .is_err()
        );
        handle.graceful_shutdown(Some(Duration::from_secs(5)));
        server.await.expect("server task must finish").expect("server must stop");
    }

    #[tokio::test]
    async fn trusted_client_completes_directory_record_operations() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        let handle = Handle::new();
        let server = tokio::spawn({
            let handle = handle.clone();
            let data_dir = data_dir.clone();
            async move { serve(&data_dir, 0, handle).await }
        });
        let address = tokio::time::timeout(Duration::from_secs(5), handle.listening())
            .await
            .expect("server must listen")
            .expect("server address must exist");
        let certificate =
            fs::read(crate::data_dir::DataDir::new(&data_dir).control_certificate())
                .expect("certificate must exist");
        let client = DirectoryClient::with_pinned_certificate(
            &format!("https://127.0.0.1:{}", address.port()),
            &certificate,
            trusted_signing_key(),
        )
        .expect("client must trust the directory");
        let record = WireRecord::test_record();
        assert_eq!(client.publish(&record).await.expect("record must publish"), record);
        assert_eq!(
            client.resolve(&record.agent_id).await.expect("record must resolve"),
            record
        );
        assert_eq!(client.publish(&record).await.expect("record must renew"), record);
        assert_eq!(
            client.search("test").await.expect("record must search"),
            vec![record.clone()]
        );
        client.revoke(&record).await.expect("record must revoke");
        let events = client.events_after(0).await.expect("events must load");
        assert_eq!(
            events.iter().map(|event| event.event).collect::<Vec<_>>(),
            [
                aether_store::DirectoryEventType::Published,
                aether_store::DirectoryEventType::Renewed,
                aether_store::DirectoryEventType::Revoked
            ]
        );
        assert!(
            client
                .events_after(events[1].cursor)
                .await
                .expect("cursor must resume")
                .iter()
                .all(|event| event.cursor > events[1].cursor)
        );
        handle.graceful_shutdown(Some(Duration::from_secs(5)));
        server.await.expect("server task must finish").expect("server must stop");
    }
}
