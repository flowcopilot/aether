pub mod cli;

use std::path::Path;

use hex::FromHex;
use reqwest::StatusCode;

use aether_store::DirectoryEventType;

use crate::{
    discovery::{
        validate_record,
        wire::{self, WireDirectoryEvent, WireRecord},
    },
    pinned_https::PinnedError,
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
}

impl From<PinnedError> for DirectoryClientError {
    fn from(error: PinnedError) -> Self {
        match error {
            PinnedError::Origin => Self::InvalidDirectory,
            PinnedError::Certificate => Self::InvalidCertificate,
            PinnedError::Client => Self::Request,
        }
    }
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
        let record = self
            .http
            .post_json_any(
                self.records_url()?,
                record,
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
        let record = self.http.get_json(self.record_url(agent_id)?).await?;
        self.validate(&record)?;
        Ok(record)
    }

    pub async fn search(
        &self,
        capability: &str,
    ) -> Result<Vec<WireRecord>, DirectoryClientError> {
        if capability.is_empty() {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let records: Vec<WireRecord> = self
            .http
            .get_json_query(self.records_url()?, &[("capability", capability)])
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
        self.http.delete_json(self.record_url(&record.agent_id)?, record).await?;
        Ok(())
    }

    pub async fn events_after(
        &self,
        cursor: i64,
    ) -> Result<Vec<WireDirectoryEvent>, DirectoryClientError> {
        if cursor < 0 {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let events: Vec<WireDirectoryEvent> = self
            .http
            .get_json_query(self.events_url()?, &[("after", &cursor.to_string())])
            .await?;
        if !pinned_json::monotonic_cursors(
            cursor,
            events.iter().map(|event| event.cursor),
        ) {
            return Err(DirectoryClientError::InvalidRecord);
        }
        for event in &events {
            self.validate_event(event)?;
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
        validate_record(&record).map_err(|_| DirectoryClientError::InvalidRecord)
    }

    fn validate_event(
        &self,
        event: &WireDirectoryEvent,
    ) -> Result<(), DirectoryClientError> {
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
                self.validate(record)
            }
            DirectoryEventType::Revoked if event.record.is_none() => Ok(()),
            DirectoryEventType::Revoked => Err(DirectoryClientError::InvalidRecord),
        }
    }
}

#[allow(
    clippy::result_unit_err,
    clippy::ignored_unit_patterns,
    clippy::match_same_arms
)]
pub fn read_authorization(path: &Path) -> Result<transport::A2aAuthorization, ()> {
    let source = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&source).map_err(|_| ())
}

#[allow(clippy::result_unit_err)]
pub fn read_verifying_key(path: &Path) -> Result<[u8; 32], ()> {
    let source = std::fs::read_to_string(path).map_err(|_| ())?;
    <[u8; 32]>::from_hex(source.trim()).map_err(|_| ())
}

#[allow(clippy::result_unit_err)]
pub fn read_directory_record(path: &Path) -> Result<WireRecord, ()> {
    serde_json::from_slice(&std::fs::read(path).map_err(|_| ())?).map_err(|_| ())
}

#[allow(clippy::result_unit_err)]
pub fn cache_directory_record(
    data_dir: &Path,
    record: &crate::discovery::Record,
) -> Result<(), ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    crate::roster_sync::cache_wire_directory_record(
        &store,
        &WireRecord::from_record(record),
    )
}

pub use crate::roster_sync::cache_wire_directory_record;

#[allow(clippy::result_unit_err)]
pub async fn sync_directory(
    data_dir: &Path,
    directory: &str,
    trusted_key: [u8; 32],
    client: &DirectoryClient,
) -> Result<(i64, usize), ()> {
    std::fs::create_dir_all(data_dir).map_err(|_| ())?;
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let mut cursor = store.directory_cursor(directory, &trusted_key).map_err(|_| ())?;
    let mut records = 0usize;
    let directory = directory.to_owned();
    loop {
        let changes = client.events_after(cursor).await.map_err(|_| ())?;
        if changes.is_empty() {
            return Ok((cursor, records));
        }
        for change in changes {
            if change.cursor <= cursor {
                return Err(());
            }
            if matches!(
                change.event,
                aether_store::DirectoryEventType::Published
                    | aether_store::DirectoryEventType::Renewed
            ) {
                records = records.checked_add(1).ok_or(())?;
            }
            crate::roster_sync::apply_directory_change(&store, &change)?;
            cursor = change.cursor;
            store
                .advance_directory_cursor(&directory, &trusted_key, cursor)
                .map_err(|_| ())?;
        }
    }
}

#[allow(clippy::result_unit_err)]
pub fn cached_directory_record(
    data_dir: &Path,
    scope_id: &str,
    agent_id: &str,
    trusted_key: [u8; 32],
) -> Result<crate::discovery::Record, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let stored = store.directory_record(scope_id, agent_id).map_err(|_| ())?;
    if stored.scope_id != scope_id || stored.agent_id != agent_id {
        return Err(());
    }
    if stored.expires_at <= aether_core::time::now() {
        return Err(());
    }
    let wire: WireRecord = serde_json::from_slice(&stored.record).map_err(|_| ())?;
    let record = wire.to_record().map_err(|_| ())?;
    if record.signing_key != trusted_key {
        return Err(());
    }
    validate_record(&record).map_err(|_| ())?;
    Ok(record)
}

#[allow(clippy::ignored_unit_patterns, clippy::match_same_arms)]
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
    use std::{fs, time::Duration};

    use axum_server::Handle;

    use super::{DirectoryClient, DirectoryClientError};
    use crate::{control_service::serve, discovery::wire::WireRecord};
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
