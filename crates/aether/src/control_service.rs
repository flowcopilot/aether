use std::{
    io,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use aether_core::time::{now, unix_timestamp};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_server::{Handle, tls_rustls::RustlsConfig};
use hex::FromHex;
use serde::{Deserialize, Serialize};

use crate::{
    data_dir::DataDir,
    discovery::{
        DiscoveryError, validate_record,
        wire::{
            self, EVENT_PAGE, WireDirectoryEvent, WireManagedEvent, WireManagedRedeem,
            WireRecord, route,
        },
    },
};

#[derive(Clone)]
pub struct ControlService {
    store: Arc<Mutex<aether_store::ControlStore>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("record is malformed")]
    InvalidRecord,
    #[error("record signature cannot be verified")]
    Forged,
    #[error("record has expired")]
    Expired,
    #[error("record was not found")]
    NotFound,
    #[error("control store error")]
    Store(#[from] aether_store::StoreError),
    #[error("control server error: {0}")]
    Server(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ControlService {
    fn store(
        &self,
    ) -> Result<MutexGuard<'_, aether_store::ControlStore>, ControlError> {
        self.store
            .lock()
            .map_err(|_| ControlError::Server("control store lock failed".to_owned()))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, ControlError> {
        Ok(Self {
            store: Arc::new(Mutex::new(aether_store::ControlStore::open(path)?)),
        })
    }

    pub fn publish(&self, wire: &WireRecord) -> Result<bool, ControlError> {
        let record = wire.to_record()?;
        validate_record(&record)?;
        let timestamp = now();
        let mut store = self.store()?;
        let is_renewal = match store.signed_record(&wire.agent_id) {
            Ok(stored) => {
                let existing = decode_record(&stored.record)?;
                if existing.signing_key != wire.signing_key {
                    return Err(ControlError::InvalidRecord);
                }
                stored.expires_at > timestamp
            }
            Err(aether_store::StoreError::NotFound) => false,
            Err(error) => return Err(ControlError::Store(error)),
        };
        store.publish_signed_record(
            &wire.signed_store_record().map_err(|_| ControlError::InvalidRecord)?,
            if is_renewal {
                aether_store::DirectoryEventType::Renewed
            } else {
                aether_store::DirectoryEventType::Published
            },
        )?;
        // Held from the read to the write, so two publishes cannot interleave.
        drop(store);
        Ok(is_renewal)
    }

    pub fn resolve(&self, agent_id: &str) -> Result<WireRecord, ControlError> {
        let stored = self.store()?.signed_record(agent_id).map_err(map_store_error)?;
        let wire = decode_record(&stored.record)?;
        let now = u64::try_from(now())
            .map_err(|_| ControlError::Server("the clock is before 1970".to_owned()))?;
        if wire.expires_at <= now {
            return Err(ControlError::Expired);
        }
        validate_record(&wire.to_record()?)?;
        Ok(wire)
    }

    pub fn search(&self, capability: &str) -> Result<Vec<WireRecord>, ControlError> {
        if capability.is_empty() {
            return Err(ControlError::InvalidRecord);
        }
        let timestamp = now();
        let records = self.store()?.active_signed_records(timestamp)?;
        let mut matches = Vec::new();
        for stored in records {
            let wire = decode_record(&stored.record)?;
            validate_record(&wire.to_record()?)?;
            if wire.capabilities.iter().any(|item| item == capability) {
                matches.push(wire);
            }
        }
        Ok(matches)
    }

    pub fn revoke(
        &self,
        agent_id: &str,
        wire: &WireRecord,
    ) -> Result<(), ControlError> {
        if wire.agent_id != agent_id {
            return Err(ControlError::InvalidRecord);
        }
        validate_record(&wire.to_record()?)?;
        let mut store = self.store()?;
        let existing = store.signed_record(agent_id).map_err(map_store_error)?;
        if decode_record(&existing.record)?.signing_key != wire.signing_key {
            return Err(ControlError::InvalidRecord);
        }
        store.revoke_signed_record(agent_id).map_err(map_store_error)?;
        // Held from the key check to the revocation.
        drop(store);
        Ok(())
    }

    pub fn changes_after(
        &self,
        cursor: i64,
    ) -> Result<Vec<WireDirectoryEvent>, ControlError> {
        if cursor < 0 {
            return Err(ControlError::InvalidRecord);
        }
        self.store()?
            .directory_events_after(cursor, EVENT_PAGE)?
            .into_iter()
            .map(|event| {
                Ok(WireDirectoryEvent {
                    cursor: event.sequence,
                    event: event.event_type,
                    scope_id: event.scope_id,
                    agent_id: event.agent_id,
                    record: event.record.as_deref().map(decode_record).transpose()?,
                })
            })
            .collect()
    }

    pub fn create_managed_invitation(
        &self,
        scope_id: &str,
        expires_in: Duration,
    ) -> Result<String, ControlError> {
        if scope_id.is_empty() || expires_in.is_zero() {
            return Err(ControlError::InvalidRecord);
        }
        let expires_at = now()
            .checked_add(
                i64::try_from(expires_in.as_secs())
                    .map_err(|_| ControlError::InvalidRecord)?,
            )
            .ok_or(ControlError::InvalidRecord)?;
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| {
            ControlError::Server("random source unavailable".to_owned())
        })?;
        self.store()?
            .create_managed_invitation(
                &uuid::Uuid::new_v4().to_string(),
                scope_id,
                blake3::hash(&token).as_bytes(),
                expires_at,
            )
            .map_err(map_store_error)?;
        Ok(hex::encode(token))
    }

    pub fn redeem_managed_invitation(
        &self,
        request: WireManagedRedeem,
    ) -> Result<aether_store::ManagedMember, ControlError> {
        let token = <[u8; 32]>::from_hex(request.token)
            .map_err(|_| ControlError::InvalidRecord)?;
        let record = request.record.to_record()?;
        validate_record(&record)?;
        let member = aether_store::ManagedMember {
            scope_id: record.scope_id,
            agent_id: record.agent_id,
            endpoint_id: record.endpoint_id,
            role: wire::MANAGED_MEMBER_ROLE.to_owned(),
            expires_at: unix_timestamp(record.expires_at)
                .ok_or(ControlError::InvalidRecord)?,
            revoked_at: None,
        };
        self.store()?
            .redeem_managed_invitation(blake3::hash(&token).as_bytes(), &member)
            .map_err(map_store_error)?;
        Ok(member)
    }

    pub fn managed_members(
        &self,
        scope_id: &str,
    ) -> Result<Vec<aether_store::ManagedMember>, ControlError> {
        self.store()?.managed_members(scope_id).map_err(map_store_error)
    }

    pub fn revoke_managed_member(
        &self,
        scope_id: &str,
        agent_id: &str,
    ) -> Result<(), ControlError> {
        self.store()?
            .revoke_managed_member(scope_id, agent_id)
            .map_err(map_store_error)?;
        Ok(())
    }

    pub fn managed_changes_after(
        &self,
        scope_id: &str,
        cursor: i64,
    ) -> Result<Vec<WireManagedEvent>, ControlError> {
        if cursor < 0 {
            return Err(ControlError::InvalidRecord);
        }
        let events = self
            .store()?
            .managed_events_after(scope_id, cursor, EVENT_PAGE)
            .map_err(map_store_error)?;
        Ok(events
            .into_iter()
            .map(|event| WireManagedEvent {
                cursor: event.sequence,
                event: event.event_type,
                scope_id: event.scope_id,
                agent_id: event.agent_id,
                member: event.member,
            })
            .collect())
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    capability: Option<String>,
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<i64>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidRecord => StatusCode::BAD_REQUEST,
            Self::Forged => StatusCode::UNAUTHORIZED,
            Self::Expired => StatusCode::GONE,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Store(_) | Self::Server(_) | Self::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, Json(ErrorBody { error: self.to_string() })).into_response()
    }
}

/// The limits of the control service. Many nodes keep a connection open
/// to it, so it takes more connections than the admin UI. A record is at
/// most a few kilobytes, and each handler answers from the store at once.
const CONTROL_HARDENING: crate::https::Hardening = crate::https::Hardening {
    handshake_timeout: Duration::from_secs(5),
    max_connections: 1_024,
    request_timeout: Duration::from_secs(10),
    body_limit: 64 * 1_024,
};

pub async fn serve(
    data_dir: &Path,
    port: u16,
    handle: Handle<std::net::SocketAddr>,
) -> Result<(), ControlError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let data_dir = DataDir::new(data_dir);
    data_dir.create()?;
    let service = ControlService::open(data_dir.control_store())?;
    let (certificate, key) = data_dir.control_tls()?;
    let config = RustlsConfig::from_der(vec![certificate], key)
        .await
        .map_err(|error| ControlError::Server(error.to_string()))?;
    let listener =
        std::net::TcpListener::bind(std::net::SocketAddr::from(([0, 0, 0, 0], port)))?;
    let actual_port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    println!(
        "{}",
        serde_json::json!({
            "event": "control_listening",
            "url": format!("https://127.0.0.1:{actual_port}"),
        })
    );
    crate::https::serve(listener, config, CONTROL_HARDENING, handle, router(service))
        .await
        .map_err(|error| ControlError::Server(error.to_string()))
}

fn router(service: ControlService) -> Router {
    Router::new()
        .route(&route(&wire::RECORDS), post(publish).get(search))
        .route(
            &route(&[&wire::RECORDS[..], &["{agent_id}"]].concat()),
            get(resolve).delete(revoke),
        )
        .route(&route(&wire::EVENTS), get(events))
        .route(&route(&wire::MANAGED_REDEEM), post(redeem_managed))
        .route(
            &route(
                &[&wire::MANAGED[..], &["{scope_id}", wire::MANAGED_EVENTS]].concat(),
            ),
            get(managed_events),
        )
        .route(
            &route(&[&wire::MANAGED[..], &["{scope_id}", wire::MEMBERS]].concat()),
            get(managed_members),
        )
        .with_state(service)
}

async fn publish(
    State(service): State<ControlService>,
    Json(record): Json<WireRecord>,
) -> Result<(StatusCode, Json<WireRecord>), ControlError> {
    let is_renewal = service.publish(&record)?;
    let status = if is_renewal { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(record)))
}

async fn resolve(
    State(service): State<ControlService>,
    AxumPath(agent_id): AxumPath<String>,
) -> Result<Json<WireRecord>, ControlError> {
    Ok(Json(service.resolve(&agent_id)?))
}

async fn search(
    State(service): State<ControlService>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<WireRecord>>, ControlError> {
    let capability = query.capability.ok_or(ControlError::InvalidRecord)?;
    Ok(Json(service.search(&capability)?))
}

async fn revoke(
    State(service): State<ControlService>,
    AxumPath(agent_id): AxumPath<String>,
    Json(record): Json<WireRecord>,
) -> Result<StatusCode, ControlError> {
    service.revoke(&agent_id, &record)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn events(
    State(service): State<ControlService>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<WireDirectoryEvent>>, ControlError> {
    Ok(Json(service.changes_after(query.after.unwrap_or_default())?))
}

async fn redeem_managed(
    State(service): State<ControlService>,
    Json(request): Json<WireManagedRedeem>,
) -> Result<(StatusCode, Json<aether_store::ManagedMember>), ControlError> {
    Ok((StatusCode::CREATED, Json(service.redeem_managed_invitation(request)?)))
}

async fn managed_members(
    State(service): State<ControlService>,
    AxumPath(scope_id): AxumPath<String>,
) -> Result<Json<Vec<aether_store::ManagedMember>>, ControlError> {
    Ok(Json(service.managed_members(&scope_id)?))
}

async fn managed_events(
    State(service): State<ControlService>,
    AxumPath(scope_id): AxumPath<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<WireManagedEvent>>, ControlError> {
    Ok(Json(service.managed_changes_after(&scope_id, query.after.unwrap_or_default())?))
}

fn decode_record(bytes: &[u8]) -> Result<WireRecord, ControlError> {
    serde_json::from_slice(bytes).map_err(|_| ControlError::InvalidRecord)
}

impl From<DiscoveryError> for ControlError {
    fn from(error: DiscoveryError) -> Self {
        match error {
            DiscoveryError::Expired => Self::Expired,
            DiscoveryError::Forged => Self::Forged,
            DiscoveryError::Malformed => Self::InvalidRecord,
        }
    }
}

fn map_store_error(error: aether_store::StoreError) -> ControlError {
    if error == aether_store::StoreError::NotFound {
        ControlError::NotFound
    } else {
        ControlError::Store(error)
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test clocks move by seconds, far from overflow"
)]
mod tests {
    use std::time::{Duration, SystemTime};

    use aether_store::{DirectoryEventType, ManagedEventType};

    use super::{ControlError, ControlService, DataDir, WireManagedRedeem, WireRecord};
    use crate::discovery::signed_record;
    use ed25519_dalek::SigningKey;

    #[tokio::test]
    async fn a_record_with_bad_hex_is_rejected_before_the_handler() {
        use tower::ServiceExt;

        let directory = tempfile::tempdir().expect("directory must create");
        let service =
            ControlService::open(DataDir::new(directory.path()).control_store())
                .expect("service must open");
        let mut record = serde_json::to_value(WireRecord::test_record())
            .expect("record must serialize");
        record["signature"] = serde_json::Value::String("zz".to_owned());
        let response = super::router(service)
            .oneshot(
                axum::http::Request::post("/v1/records")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(record.to_string()))
                    .expect("request must build"),
            )
            .await
            .expect("router must answer");
        assert_eq!(response.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    fn record_for(key: &SigningKey) -> WireRecord {
        WireRecord::from_record(&signed_record(
            key,
            "test-agent".to_owned(),
            "endpoint".to_owned(),
            vec!["127.0.0.1:9443".to_owned()],
            "test-scope".to_owned(),
            vec!["test".to_owned()],
            SystemTime::now() + Duration::from_secs(3_600),
        ))
    }

    #[test]
    fn records_can_be_published_found_and_revoked() {
        let directory = tempfile::tempdir().expect("directory must create");
        let directory = directory.path();
        let service = ControlService::open(DataDir::new(directory).control_store())
            .expect("service must open");
        let record = WireRecord::test_record();

        assert!(!service.publish(&record).expect("record must publish"));
        assert!(service.publish(&record).expect("record must renew"));
        drop(service);
        let service = ControlService::open(DataDir::new(directory).control_store())
            .expect("service must reopen");
        assert_eq!(service.resolve("test-agent").expect("record must resolve"), record);
        assert_eq!(
            service.search("test").expect("record must search"),
            vec![record.clone()]
        );
        service.revoke("test-agent", &record).expect("record must revoke");
        assert!(matches!(service.resolve("test-agent"), Err(ControlError::NotFound)));
        let changes = service.changes_after(0).expect("changes must load");
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].event, DirectoryEventType::Published);
        assert!(changes[0].record.is_some());
        assert_eq!(changes[1].event, DirectoryEventType::Renewed);
        assert_eq!(changes[2].event, DirectoryEventType::Revoked);
        assert!(changes[2].record.is_none());
        assert!(
            service
                .changes_after(changes[1].cursor)
                .expect("cursor must resume")
                .iter()
                .all(|change| change.cursor > changes[1].cursor)
        );
        assert!(matches!(service.changes_after(-1), Err(ControlError::InvalidRecord)));
    }

    #[test]
    fn forged_records_are_not_stored() {
        let directory = tempfile::tempdir().expect("directory must create");
        let service =
            ControlService::open(DataDir::new(directory.path()).control_store())
                .expect("service must open");
        let mut record = WireRecord::test_record();
        record.endpoint_id = "changed".to_owned();

        assert!(matches!(service.publish(&record), Err(ControlError::Forged)));
        assert!(matches!(service.resolve("test-agent"), Err(ControlError::NotFound)));
    }

    #[test]
    fn another_signer_cannot_replace_or_revoke_a_record() {
        let directory = tempfile::tempdir().expect("directory must create");
        let service =
            ControlService::open(DataDir::new(directory.path()).control_store())
                .expect("service must open");
        let record = WireRecord::test_record();
        let replacement = record_for(&SigningKey::from_bytes(&[8; 32]));

        service.publish(&record).expect("record must publish");
        assert!(matches!(
            service.publish(&replacement),
            Err(ControlError::InvalidRecord)
        ));
        assert_eq!(service.resolve("test-agent").expect("record must resolve"), record);
        assert!(matches!(
            service.revoke("test-agent", &replacement),
            Err(ControlError::InvalidRecord)
        ));
        assert!(service.resolve("test-agent").is_ok());
    }

    #[test]
    fn invited_signed_member_is_durable_and_can_be_revoked() {
        let directory = tempfile::tempdir().expect("directory must create");
        let directory = directory.path();
        let service = ControlService::open(DataDir::new(directory).control_store())
            .expect("service must open");
        let token = service
            .create_managed_invitation("managed", Duration::from_secs(60))
            .expect("invitation must create");
        let record = WireRecord::from_record(&signed_record(
            &SigningKey::from_bytes(&[8; 32]),
            "member-agent".to_owned(),
            "member-endpoint".to_owned(),
            vec!["127.0.0.1:9443".to_owned()],
            "managed".to_owned(),
            vec!["test".to_owned()],
            SystemTime::now() + Duration::from_secs(60),
        ));
        let request = WireManagedRedeem { token, record };

        let member = service
            .redeem_managed_invitation(request.clone())
            .expect("invitation must redeem");
        assert_eq!(member.agent_id, "member-agent");
        drop(service);
        let service = ControlService::open(DataDir::new(directory).control_store())
            .expect("service must reopen");
        assert_eq!(
            service.managed_members("managed").expect("members must load"),
            vec![member]
        );
        assert!(matches!(
            service.redeem_managed_invitation(request),
            Err(ControlError::NotFound)
        ));
        service
            .revoke_managed_member("managed", "member-agent")
            .expect("member must revoke");
        assert!(
            service.managed_members("managed").expect("members must load")[0]
                .revoked_at
                .is_some()
        );
        let events =
            service.managed_changes_after("managed", 0).expect("events must load");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, ManagedEventType::Joined);
        assert_eq!(events[1].event, ManagedEventType::Revoked);
        assert!(events[0].member.is_some());
        assert!(events[1].member.is_none());
        assert!(
            service
                .managed_changes_after("managed", events[0].cursor)
                .expect("events must resume")
                .iter()
                .all(|event| event.cursor > events[0].cursor)
        );
        assert!(matches!(
            service.managed_changes_after("managed", -1),
            Err(ControlError::InvalidRecord)
        ));
    }
}
