use reqwest::{Client, StatusCode, Url};

use crate::{
    control_service::{WireDirectoryEvent, WireRecord},
    discovery::validate_record,
    pinned_https::{self, PinnedError},
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

#[derive(Clone)]
pub struct DirectoryClient {
    client: Client,
    directory: Url,
    trusted_signing_key: [u8; 32],
}

impl DirectoryClient {
    pub fn with_pinned_certificate(
        directory: &str,
        certificate_der: &[u8],
        trusted_signing_key: [u8; 32],
    ) -> Result<Self, DirectoryClientError> {
        let (client, directory) = pinned_https::client(directory, certificate_der)
            .map_err(|error| match error {
                PinnedError::Origin => DirectoryClientError::InvalidDirectory,
                PinnedError::Certificate => DirectoryClientError::InvalidCertificate,
                PinnedError::Client => DirectoryClientError::Request,
            })?;
        Ok(Self { client, directory, trusted_signing_key })
    }

    pub async fn publish(
        &self,
        record: &WireRecord,
    ) -> Result<WireRecord, DirectoryClientError> {
        let response = self
            .client
            .post(self.records_url()?)
            .json(record)
            .send()
            .await
            .map_err(|_| DirectoryClientError::Request)?;
        if !matches!(response.status(), StatusCode::CREATED | StatusCode::OK) {
            return Err(DirectoryClientError::Status);
        }
        let record =
            response.json().await.map_err(|_| DirectoryClientError::InvalidRecord)?;
        self.validate(&record)?;
        Ok(record)
    }

    pub async fn resolve(
        &self,
        agent_id: &str,
    ) -> Result<WireRecord, DirectoryClientError> {
        let response = self
            .client
            .get(self.record_url(agent_id)?)
            .send()
            .await
            .map_err(|_| DirectoryClientError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(DirectoryClientError::Status);
        }
        let record =
            response.json().await.map_err(|_| DirectoryClientError::InvalidRecord)?;
        self.validate(&record)?;
        Ok(record)
    }

    pub async fn resolve_record(
        &self,
        agent_id: &str,
    ) -> Result<crate::discovery::Record, DirectoryClientError> {
        self.resolve(agent_id)
            .await?
            .to_record()
            .map_err(|_| DirectoryClientError::InvalidRecord)
    }

    pub async fn renew(
        &self,
        record: &WireRecord,
    ) -> Result<WireRecord, DirectoryClientError> {
        self.publish(record).await
    }

    pub async fn search(
        &self,
        capability: &str,
    ) -> Result<Vec<WireRecord>, DirectoryClientError> {
        if capability.is_empty() {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let response = self
            .client
            .get(self.records_url()?)
            .query(&[("capability", capability)])
            .send()
            .await
            .map_err(|_| DirectoryClientError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(DirectoryClientError::Status);
        }
        let records: Vec<WireRecord> =
            response.json().await.map_err(|_| DirectoryClientError::InvalidRecord)?;
        for record in &records {
            self.validate(record)?;
        }
        Ok(records)
    }

    pub async fn revoke(
        &self,
        record: &WireRecord,
    ) -> Result<(), DirectoryClientError> {
        let response = self
            .client
            .delete(self.record_url(&record.agent_id)?)
            .json(record)
            .send()
            .await
            .map_err(|_| DirectoryClientError::Request)?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(DirectoryClientError::Status);
        }
        Ok(())
    }

    pub async fn events_after(
        &self,
        cursor: i64,
    ) -> Result<Vec<WireDirectoryEvent>, DirectoryClientError> {
        if cursor < 0 {
            return Err(DirectoryClientError::InvalidRecord);
        }
        let response = self
            .client
            .get(self.events_url()?)
            .query(&[("after", cursor)])
            .send()
            .await
            .map_err(|_| DirectoryClientError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(DirectoryClientError::Status);
        }
        let events: Vec<WireDirectoryEvent> =
            response.json().await.map_err(|_| DirectoryClientError::InvalidRecord)?;
        let mut previous = cursor;
        for event in &events {
            if event.cursor <= previous {
                return Err(DirectoryClientError::InvalidRecord);
            }
            self.validate_event(event)?;
            previous = event.cursor;
        }
        Ok(events)
    }

    fn records_url(&self) -> Result<Url, DirectoryClientError> {
        self.url(&["v1", "records"])
    }

    fn record_url(&self, agent_id: &str) -> Result<Url, DirectoryClientError> {
        if !pinned_https::valid_segment(agent_id) {
            return Err(DirectoryClientError::InvalidRecord);
        }
        self.url(&["v1", "records", agent_id])
    }

    fn events_url(&self) -> Result<Url, DirectoryClientError> {
        self.url(&["v1", "events"])
    }

    fn url(&self, segments: &[&str]) -> Result<Url, DirectoryClientError> {
        pinned_https::url(&self.directory, segments)
            .ok_or(DirectoryClientError::InvalidDirectory)
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
        match event.event.as_str() {
            "published" | "renewed" => {
                let record =
                    event.record.as_ref().ok_or(DirectoryClientError::InvalidRecord)?;
                if record.agent_id != event.agent_id
                    || record.scope_id != event.scope_id
                {
                    return Err(DirectoryClientError::InvalidRecord);
                }
                self.validate(record)
            }
            "revoked" if event.record.is_none() => Ok(()),
            _ => Err(DirectoryClientError::InvalidRecord),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use axum_server::Handle;

    use super::{DirectoryClient, DirectoryClientError};
    use crate::control_service::{WireRecord, serve};
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
        let certificate = fs::read(data_dir.join("control-cert.der"))
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
        assert_eq!(
            client
                .resolve_record(&record.agent_id)
                .await
                .expect("record must resolve")
                .endpoint_id,
            record.endpoint_id
        );
        assert_eq!(client.renew(&record).await.expect("record must renew"), record);
        assert_eq!(
            client.search("test").await.expect("record must search"),
            vec![record.clone()]
        );
        client.revoke(&record).await.expect("record must revoke");
        let events = client.events_after(0).await.expect("events must load");
        assert_eq!(
            events.iter().map(|event| event.event.as_str()).collect::<Vec<_>>(),
            ["published", "renewed", "revoked"]
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
