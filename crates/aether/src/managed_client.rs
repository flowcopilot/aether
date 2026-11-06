pub mod cli;

use std::path::Path;

use hex::FromHex;
use reqwest::{StatusCode, Url};

use aether_store::ManagedEventType;

use crate::{
    discovery::{
        validate_record,
        wire::{self, WireManagedEvent, WireManagedRedeem, WireRecord},
    },
    pinned_https::{self, PinnedError},
    transport::{self, Frame, TransportError},
};

#[derive(Debug, thiserror::Error)]
pub enum ManagedClientError {
    #[error("control URL must be an HTTPS origin")]
    InvalidControl,
    #[error("control certificate is invalid")]
    InvalidCertificate,
    #[error("control request failed")]
    Request,
    #[error("control returned an unexpected status")]
    Status,
    #[error("control returned an invalid managed record")]
    InvalidRecord,
}

#[derive(Clone)]
pub struct ManagedClient {
    pinned: pinned_https::PinnedOrigin,
}

impl ManagedClient {
    pub fn with_pinned_certificate(
        control: &str,
        certificate_der: &[u8],
    ) -> Result<Self, ManagedClientError> {
        let pinned = pinned_https::PinnedOrigin::open(control, certificate_der)
            .map_err(|error| match error {
                PinnedError::Origin => ManagedClientError::InvalidControl,
                PinnedError::Certificate => ManagedClientError::InvalidCertificate,
                PinnedError::Client => ManagedClientError::Request,
            })?;
        Ok(Self { pinned })
    }

    pub async fn redeem(
        &self,
        token: &str,
        record: WireRecord,
    ) -> Result<aether_store::ManagedMember, ManagedClientError> {
        let token = <[u8; 32]>::from_hex(token)
            .map_err(|_| ManagedClientError::InvalidRecord)?;
        let signed =
            record.to_record().map_err(|_| ManagedClientError::InvalidRecord)?;
        validate_record(&signed).map_err(|_| ManagedClientError::InvalidRecord)?;
        let expires_at = aether_core::time::unix_timestamp(signed.expires_at)
            .ok_or(ManagedClientError::InvalidRecord)?;
        let response = self
            .pinned
            .client
            .post(self.redeem_url()?)
            .json(&WireManagedRedeem {
                token: hex::encode(token),
                record: record.clone(),
            })
            .send()
            .await
            .map_err(|_| ManagedClientError::Request)?;
        if response.status() != StatusCode::CREATED {
            return Err(ManagedClientError::Status);
        }
        let member =
            response.json().await.map_err(|_| ManagedClientError::InvalidRecord)?;
        Self::validate_member(&member)?;
        if member.scope_id != record.scope_id
            || member.agent_id != record.agent_id
            || member.endpoint_id != record.endpoint_id
            || member.expires_at != expires_at
        {
            return Err(ManagedClientError::InvalidRecord);
        }
        Ok(member)
    }

    pub async fn members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<aether_store::ManagedMember>, ManagedClientError> {
        let response = self
            .pinned
            .client
            .get(self.members_url(scope_id)?)
            .send()
            .await
            .map_err(|_| ManagedClientError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(ManagedClientError::Status);
        }
        let members: Vec<aether_store::ManagedMember> =
            response.json().await.map_err(|_| ManagedClientError::InvalidRecord)?;
        for member in &members {
            if member.scope_id != scope_id {
                return Err(ManagedClientError::InvalidRecord);
            }
            Self::validate_member(member)?;
        }
        Ok(members)
    }

    pub async fn events_after(
        &self,
        scope_id: &str,
        cursor: i64,
    ) -> Result<Vec<WireManagedEvent>, ManagedClientError> {
        if cursor < 0 {
            return Err(ManagedClientError::InvalidRecord);
        }
        let response = self
            .pinned
            .client
            .get(self.events_url(scope_id)?)
            .query(&[("after", cursor)])
            .send()
            .await
            .map_err(|_| ManagedClientError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(ManagedClientError::Status);
        }
        let events: Vec<WireManagedEvent> =
            response.json().await.map_err(|_| ManagedClientError::InvalidRecord)?;
        let mut previous = cursor;
        for event in &events {
            if event.cursor <= previous
                || event.scope_id != scope_id
                || event.agent_id.is_empty()
            {
                return Err(ManagedClientError::InvalidRecord);
            }
            match event.event {
                ManagedEventType::Joined => {
                    let member = event
                        .member
                        .as_ref()
                        .ok_or(ManagedClientError::InvalidRecord)?;
                    if member.agent_id != event.agent_id
                        || member.scope_id != event.scope_id
                    {
                        return Err(ManagedClientError::InvalidRecord);
                    }
                    Self::validate_member(member)?;
                }
                ManagedEventType::Revoked if event.member.is_none() => {}
                ManagedEventType::Revoked => {
                    return Err(ManagedClientError::InvalidRecord);
                }
            }
            previous = event.cursor;
        }
        Ok(events)
    }

    fn redeem_url(&self) -> Result<Url, ManagedClientError> {
        self.url(&wire::MANAGED_REDEEM)
    }

    fn members_url(&self, scope_id: &str) -> Result<Url, ManagedClientError> {
        self.scope_url(scope_id, wire::MEMBERS)
    }

    fn events_url(&self, scope_id: &str) -> Result<Url, ManagedClientError> {
        self.scope_url(scope_id, wire::MANAGED_EVENTS)
    }

    fn scope_url(
        &self,
        scope_id: &str,
        operation: &str,
    ) -> Result<Url, ManagedClientError> {
        if !pinned_https::valid_segment(scope_id) {
            return Err(ManagedClientError::InvalidRecord);
        }
        self.url(&[wire::MANAGED[0], wire::MANAGED[1], scope_id, operation])
    }

    fn url(&self, segments: &[&str]) -> Result<Url, ManagedClientError> {
        self.pinned.url(segments).ok_or(ManagedClientError::InvalidControl)
    }

    fn validate_member(
        member: &aether_store::ManagedMember,
    ) -> Result<(), ManagedClientError> {
        if member.scope_id.is_empty()
            || member.agent_id.is_empty()
            || member.endpoint_id.is_empty()
            || member.role != "member"
            || member.expires_at <= 0
        {
            return Err(ManagedClientError::InvalidRecord);
        }
        Ok(())
    }
}

#[must_use]
pub fn managed_member_active(
    member: &aether_store::ManagedMember,
    agent_id: &str,
) -> bool {
    member.agent_id == agent_id && member.is_active(aether_core::time::now())
}

#[allow(clippy::result_unit_err)]
pub fn cached_managed_member(
    data_dir: &Path,
    scope_id: &str,
    agent_id: &str,
) -> Result<aether_store::ManagedMember, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    store
        .managed_members(scope_id)
        .map_err(|_| ())?
        .into_iter()
        .find(|member| managed_member_active(member, agent_id))
        .ok_or(())
}

#[allow(clippy::result_unit_err)]
pub async fn sync_managed(
    data_dir: &Path,
    control: &str,
    scope_id: &str,
    client: &ManagedClient,
) -> Result<(i64, usize), ()> {
    std::fs::create_dir_all(data_dir).map_err(|_| ())?;
    let mut store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let mut cursor = store.managed_cursor(control, scope_id).map_err(|_| ())?;
    let mut changes = 0usize;
    let control = control.to_owned();
    loop {
        let events = client.events_after(scope_id, cursor).await.map_err(|_| ())?;
        if events.is_empty() {
            return Ok((cursor, changes));
        }
        for event in events {
            if event.cursor <= cursor {
                return Err(());
            }
            crate::roster_sync::apply_managed_change(&mut store, &control, &event)?;
            cursor = event.cursor;
            changes = changes.checked_add(1).ok_or(())?;
        }
    }
}

pub async fn request_task(
    input: &crate::node_request::RequestInput,
    data_dir: &Path,
    control: &str,
    request: Frame,
    relay_url: Option<&str>,
) -> Result<Vec<Frame>, TransportError> {
    let certificate_path =
        input.managed_control_certificate.as_deref().ok_or(TransportError::Address)?;
    let agent_id = input.agent_id.as_deref().ok_or(TransportError::Address)?;
    let certificate =
        std::fs::read(certificate_path).map_err(|_| TransportError::Address)?;
    let client = ManagedClient::with_pinned_certificate(control, &certificate)
        .map_err(|_| TransportError::Address)?;
    if sync_managed(data_dir, control, &input.scope, &client).await.is_err() {
        eprintln!(
            "{}",
            serde_json::json!({
                "level": "error",
                "event": "managed_sync_failed",
            })
        );
    }
    let members = match client.members(&input.scope).await {
        Ok(members) => members,
        Err(ManagedClientError::Request) => {
            vec![cached_managed_member(data_dir, &input.scope, agent_id).map_err(
                |()| TransportError::Network("roster lookup failed".to_owned()),
            )?]
        }
        Err(_) => {
            return Err(TransportError::Network("roster lookup failed".to_owned()));
        }
    };
    let member = members
        .into_iter()
        .find(|member| managed_member_active(member, agent_id))
        .ok_or(TransportError::Address)?;
    let relay_url = relay_url.ok_or(TransportError::Address)?;
    transport::request_once_via_relay(data_dir, &member.endpoint_id, relay_url, request)
        .await
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use axum_server::Handle;
    use ed25519_dalek::SigningKey;

    use super::{ManagedClient, ManagedClientError};
    use crate::{
        control_service::{ControlService, serve},
        discovery::{signed_record, wire::WireRecord},
    };

    #[test]
    fn managed_client_requires_an_https_origin() {
        assert!(matches!(
            ManagedClient::with_pinned_certificate("http://control.test", b"invalid"),
            Err(ManagedClientError::InvalidControl)
        ));
    }

    #[test]
    fn scope_ids_are_one_encoded_path_segment() {
        let client = ManagedClient::with_pinned_certificate(
            "https://control.test",
            rcgen::generate_simple_self_signed(vec!["control.test".to_owned()])
                .expect("certificate must generate")
                .cert
                .der(),
        )
        .expect("client must build");
        let url = client.members_url("a?b#c 50%").expect("id must build a URL");
        assert_eq!(url.path(), "/v1/managed/a%3Fb%23c%2050%25/members");
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), None);
        for id in ["", ".", "..", "a/b", "a\0b"] {
            assert!(
                matches!(client.events_url(id), Err(ManagedClientError::InvalidRecord)),
                "{id:?}"
            );
        }
    }

    #[tokio::test]
    async fn managed_client_redeems_and_resumes_roster_changes() {
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
        let service = ControlService::open(
            crate::data_dir::DataDir::new(&data_dir).control_store(),
        )
        .expect("control must open");
        let token = service
            .create_managed_invitation("managed", Duration::from_secs(60))
            .expect("invitation must create");
        let record = WireRecord::from_record(&signed_record(
            &SigningKey::from_bytes(&[9; 32]),
            "managed-agent".to_owned(),
            "managed-endpoint".to_owned(),
            vec!["127.0.0.1:9443".to_owned()],
            "managed".to_owned(),
            vec!["test".to_owned()],
            SystemTime::now() + Duration::from_secs(60),
        ));
        let client = ManagedClient::with_pinned_certificate(
            &format!("https://127.0.0.1:{}", address.port()),
            &certificate,
        )
        .expect("client must build");

        let member = client.redeem(&token, record).await.expect("member must redeem");
        assert_eq!(
            client.members("managed").await.expect("members must load"),
            vec![member]
        );
        let events = client.events_after("managed", 0).await.expect("events must load");
        assert_eq!(events.len(), 1);
        service
            .revoke_managed_member("managed", "managed-agent")
            .expect("member must revoke");
        assert_eq!(
            client
                .events_after("managed", events[0].cursor)
                .await
                .expect("events must resume")[0]
                .event,
            aether_store::ManagedEventType::Revoked
        );

        handle.graceful_shutdown(Some(Duration::from_secs(1)));
        server.await.expect("server task must finish").expect("server must stop");
        drop(service);
    }
}
