//! State of the PACT gateway.
//!
//! It holds conversations, retries, device authorizations, delegation
//! grants, refresh tokens, and receipts. It lives in its own database file,
//! `pact.sqlite`, so the node's own schema does not change.
//!
//! Every time is a Unix timestamp in seconds, passed in by the caller.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{StoreError, migrate::open_connection};

pub const PACT_DATABASE: &str = "pact.sqlite";

pub const PACT_MIGRATIONS: &[&str] =
    &[include_str!("../migrations/pact/001_initial.sql")];

pub struct PactStore {
    connection: Connection,
}

/// A conversation's owner and state.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PactContext {
    pub context_id: String,
    pub brand_id: String,
    pub pa_issuer: String,
    pub pa_subject: String,
    pub brand_user: Option<String>,
    pub closed: bool,
}

/// The result of reserving a `messageId` in a context.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Reservation {
    /// The message is new; the caller runs the turn.
    New,
    /// The message was seen before. `None` while its turn still runs.
    Existing(Option<String>),
}

/// A new device authorization (RFC 8628).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NewDeviceAuthorization {
    pub device_code_hash: String,
    pub user_code: String,
    pub brand_id: String,
    pub pa_issuer: String,
    pub pa_subject: String,
    pub scopes: String,
    pub interval_seconds: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A device authorization that still waits for the owner's decision.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingDevice {
    pub user_code: String,
    pub brand_id: String,
    pub pa_issuer: String,
    pub pa_subject: String,
    pub scopes: String,
    pub expires_at: i64,
}

/// What the token endpoint answers to one device-code poll.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DevicePoll {
    Unknown,
    Expired,
    Denied,
    Consumed,
    Pending {
        slow_down: bool,
    },
    /// Approved and now consumed: the caller issues tokens for this grant.
    Approved(PactGrant),
}

/// A delegation the owner approved.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PactGrant {
    pub grant_id: String,
    pub brand_id: String,
    pub pa_issuer: String,
    pub pa_subject: String,
    pub brand_user: String,
    pub scopes: String,
    pub created_at: i64,
    pub expires_at: i64,
}

impl PactGrant {
    /// Reads the columns in `GRANT_COLUMNS` order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            grant_id: row.get(0)?,
            brand_id: row.get(1)?,
            pa_issuer: row.get(2)?,
            pa_subject: row.get(3)?,
            brand_user: row.get(4)?,
            scopes: row.get(5)?,
            created_at: row.get(6)?,
            expires_at: row.get(7)?,
        })
    }
}

const GRANT_COLUMNS: &str = "grant_id, brand_id, pa_issuer, pa_subject, brand_user, scopes, created_at, expires_at";

impl PactStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { connection: open_connection(path.as_ref(), PACT_MIGRATIONS)? })
    }

    pub fn in_dir(dir: &Path) -> Result<Self, StoreError> {
        Self::open(dir.join(PACT_DATABASE))
    }

    // -- Contexts (§4.2) --

    pub fn create_context(
        &self,
        context_id: &str,
        brand_id: &str,
        pa_issuer: &str,
        pa_subject: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO pact_contexts(context_id, brand_id, pa_issuer, pa_subject, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![context_id, brand_id, pa_issuer, pa_subject, now],
        )?;
        Ok(())
    }

    pub fn context(&self, context_id: &str) -> Result<Option<PactContext>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT context_id, brand_id, pa_issuer, pa_subject, brand_user, closed_at IS NOT NULL FROM pact_contexts WHERE context_id=?1",
                [context_id],
                |row| {
                    Ok(PactContext {
                        context_id: row.get(0)?,
                        brand_id: row.get(1)?,
                        pa_issuer: row.get(2)?,
                        pa_subject: row.get(3)?,
                        brand_user: row.get(4)?,
                        closed: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Binds the context to `brand_user` the first time a delegated turn
    /// runs in it. A context already bound to another Brand user refuses
    /// with `Unauthorized` (§5.5).
    pub fn bind_context_user(
        &self,
        context_id: &str,
        brand_user: &str,
    ) -> Result<(), StoreError> {
        let changed = self.connection.execute(
            "UPDATE pact_contexts SET brand_user=?2 WHERE context_id=?1 AND (brand_user IS NULL OR brand_user=?2)",
            params![context_id, brand_user],
        )?;
        if changed == 1 { Ok(()) } else { Err(StoreError::Unauthorized) }
    }

    /// Closes every open context with no message since `idle_before`, then
    /// deletes the messages of all closed contexts. Returns how many
    /// contexts it closed.
    pub fn close_idle_contexts(
        &mut self,
        idle_before: i64,
        now: i64,
    ) -> Result<usize, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let closed = transaction.execute(
            "UPDATE pact_contexts SET closed_at=?2 WHERE closed_at IS NULL AND updated_at < ?1",
            params![idle_before, now],
        )?;
        transaction.execute(
            "DELETE FROM pact_messages WHERE context_id IN (SELECT context_id FROM pact_contexts WHERE closed_at IS NOT NULL)",
            [],
        )?;
        transaction.commit()?;
        Ok(closed)
    }

    // -- Messages (§4.3) --

    /// Records `message_id` in an open context, or reports the stored
    /// reply of an earlier send with the same id.
    pub fn reserve_message(
        &mut self,
        context_id: &str,
        message_id: &str,
        request_text: &str,
        now: i64,
    ) -> Result<Reservation, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let existing: Option<Option<String>> = transaction
            .query_row(
                "SELECT response FROM pact_messages WHERE context_id=?1 AND message_id=?2",
                params![context_id, message_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(response) = existing {
            return Ok(Reservation::Existing(response));
        }
        let open = transaction.execute(
            "UPDATE pact_contexts SET updated_at=?2 WHERE context_id=?1 AND closed_at IS NULL",
            params![context_id, now],
        )?;
        if open != 1 {
            return Err(StoreError::NotFound);
        }
        transaction.execute(
            "INSERT INTO pact_messages(context_id, message_id, sequence, request_text, created_at) VALUES (?1, ?2, (SELECT COALESCE(MAX(sequence), 0) + 1 FROM pact_messages WHERE context_id=?1), ?3, ?4)",
            params![context_id, message_id, request_text, now],
        )?;
        transaction.commit()?;
        Ok(Reservation::New)
    }

    /// Stores the reply a retry gets back. `reply_text` is `None` for a
    /// reply that did not answer the turn, such as a step-up; such a turn
    /// stays out of later transcripts.
    pub fn complete_message(
        &self,
        context_id: &str,
        message_id: &str,
        reply_text: Option<&str>,
        response: &str,
    ) -> Result<(), StoreError> {
        let changed = self.connection.execute(
            "UPDATE pact_messages SET reply_text=?3, response=?4 WHERE context_id=?1 AND message_id=?2 AND response IS NULL",
            params![context_id, message_id, reply_text, response],
        )?;
        if changed == 1 { Ok(()) } else { Err(StoreError::InvalidState) }
    }

    /// Forgets a reserved message whose turn failed before it had a reply,
    /// so a retry runs it again.
    pub fn discard_message(
        &self,
        context_id: &str,
        message_id: &str,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "DELETE FROM pact_messages WHERE context_id=?1 AND message_id=?2 AND response IS NULL",
            params![context_id, message_id],
        )?;
        Ok(())
    }

    /// The last `limit` answered turns before `message_id`, oldest first,
    /// as `(request, reply)` pairs.
    pub fn transcript(
        &self,
        context_id: &str,
        message_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT request_text, reply_text FROM (SELECT sequence, request_text, reply_text FROM pact_messages WHERE context_id=?1 AND reply_text IS NOT NULL AND sequence < (SELECT sequence FROM pact_messages WHERE context_id=?1 AND message_id=?2) ORDER BY sequence DESC LIMIT ?3) ORDER BY sequence",
        )?;
        statement
            .query_map(params![context_id, message_id, limit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    // -- Device authorizations (§5.3, RFC 8628) --

    pub fn create_device_authorization(
        &self,
        authorization: &NewDeviceAuthorization,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO pact_device_authorizations(device_code_hash, user_code, brand_id, pa_issuer, pa_subject, scopes, status, interval_seconds, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?10, ?7, ?8, ?9)",
            params![
                authorization.device_code_hash,
                authorization.user_code,
                authorization.brand_id,
                authorization.pa_issuer,
                authorization.pa_subject,
                authorization.scopes,
                authorization.interval_seconds,
                authorization.created_at,
                authorization.expires_at,
                DeviceStatus::Pending.as_str(),
            ],
        )?;
        Ok(())
    }

    /// The pending, unexpired authorization with `user_code`.
    pub fn pending_device(
        &self,
        user_code: &str,
        now: i64,
    ) -> Result<Option<PendingDevice>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT user_code, brand_id, pa_issuer, pa_subject, scopes, expires_at FROM pact_device_authorizations WHERE user_code=?1 AND status=?3 AND expires_at > ?2",
                params![user_code, now, DeviceStatus::Pending.as_str()],
                |row| {
                    Ok(PendingDevice {
                        user_code: row.get(0)?,
                        brand_id: row.get(1)?,
                        pa_issuer: row.get(2)?,
                        pa_subject: row.get(3)?,
                        scopes: row.get(4)?,
                        expires_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Approves the pending authorization `user_code` and stores `grant`,
    /// in one transaction. `NotFound` when it is no longer pending.
    pub fn approve_device(
        &mut self,
        user_code: &str,
        grant: &PactGrant,
        now: i64,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE pact_device_authorizations SET status=?4, grant_id=?2 WHERE user_code=?1 AND status=?5 AND expires_at > ?3",
            params![
                user_code,
                grant.grant_id,
                now,
                DeviceStatus::Approved.as_str(),
                DeviceStatus::Pending.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        insert_grant(&transaction, grant)?;
        transaction.commit()?;
        Ok(())
    }

    /// Denies the pending authorization `user_code`.
    pub fn deny_device(&self, user_code: &str) -> Result<(), StoreError> {
        let changed = self.connection.execute(
            "UPDATE pact_device_authorizations SET status=?2 WHERE user_code=?1 AND status=?3",
            params![user_code, DeviceStatus::Denied.as_str(), DeviceStatus::Pending.as_str()],
        )?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    /// Answers one token-endpoint poll for a device code. Only the
    /// personal agent and user that started the flow, for the same Brand,
    /// can poll it; anyone else gets `Unknown` and changes nothing. An
    /// approved code is consumed by the first poll that sees it.
    pub fn poll_device(
        &mut self,
        device_code_hash: &str,
        brand_id: &str,
        pa_issuer: &str,
        pa_subject: &str,
        now: i64,
    ) -> Result<DevicePoll, StoreError> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.connection,
            TransactionBehavior::Immediate,
        )?;
        let row: Option<DeviceRow> = transaction
            .query_row(
                "SELECT status, grant_id, interval_seconds, last_polled_at, expires_at FROM pact_device_authorizations WHERE device_code_hash=?1 AND brand_id=?2 AND pa_issuer=?3 AND pa_subject=?4",
                params![device_code_hash, brand_id, pa_issuer, pa_subject],
                |row| {
                    Ok(DeviceRow {
                        status: row.get(0)?,
                        grant_id: row.get(1)?,
                        interval: row.get(2)?,
                        last_polled_at: row.get(3)?,
                        expires_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let Some(DeviceRow { status, grant_id, interval, last_polled_at, expires_at }) =
            row
        else {
            return Ok(DevicePoll::Unknown);
        };
        let poll = if expires_at <= now {
            DevicePoll::Expired
        } else {
            match DeviceStatus::parse(&status).ok_or(StoreError::InvalidState)? {
                DeviceStatus::Denied => DevicePoll::Denied,
                DeviceStatus::Consumed => DevicePoll::Consumed,
                DeviceStatus::Pending => {
                    let slow_down = last_polled_at
                        .is_some_and(|last| now.saturating_sub(last) < interval);
                    transaction.execute(
                        "UPDATE pact_device_authorizations SET last_polled_at=?2 WHERE device_code_hash=?1",
                        params![device_code_hash, now],
                    )?;
                    DevicePoll::Pending { slow_down }
                }
                DeviceStatus::Approved => {
                    transaction.execute(
                        "UPDATE pact_device_authorizations SET status=?2 WHERE device_code_hash=?1",
                        params![device_code_hash, DeviceStatus::Consumed.as_str()],
                    )?;
                    let grant = grant_id
                        .map(|grant_id| query_grant(&transaction, &grant_id))
                        .transpose()?
                        .flatten();
                    grant.map_or(DevicePoll::Consumed, DevicePoll::Approved)
                }
            }
        };
        transaction.commit()?;
        Ok(poll)
    }

    // -- Grants and refresh tokens --

    pub fn grant(&self, grant_id: &str) -> Result<Option<PactGrant>, StoreError> {
        query_grant(&self.connection, grant_id)
    }

    pub fn put_refresh_token(
        &self,
        token_hash: &str,
        grant_id: &str,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO pact_refresh_tokens(token_hash, grant_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token_hash, grant_id, expires_at],
        )?;
        Ok(())
    }

    /// Spends an unused, unexpired refresh token and returns its grant.
    /// A second use of the same token returns `None`.
    pub fn use_refresh_token(
        &mut self,
        token_hash: &str,
        now: i64,
    ) -> Result<Option<PactGrant>, StoreError> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.connection,
            TransactionBehavior::Immediate,
        )?;
        let grant_id: Option<String> = transaction
            .query_row(
                "UPDATE pact_refresh_tokens SET used_at=?2 WHERE token_hash=?1 AND used_at IS NULL AND expires_at > ?2 RETURNING grant_id",
                params![token_hash, now],
                |row| row.get(0),
            )
            .optional()?;
        let grant = grant_id
            .map(|grant_id| query_grant(&transaction, &grant_id))
            .transpose()?
            .flatten()
            .filter(|grant| grant.expires_at > now);
        transaction.commit()?;
        Ok(grant)
    }

    // -- Receipts (§5.6) --

    pub fn put_receipt(
        &self,
        context_id: &str,
        message_id: &str,
        grant_id: &str,
        jws: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO pact_receipts(context_id, message_id, grant_id, jws, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![context_id, message_id, grant_id, jws, now],
        )?;
        Ok(())
    }

    /// The receipts stored for `grant_id`, oldest first.
    pub fn receipts(&self, grant_id: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT jws FROM pact_receipts WHERE grant_id=?1 ORDER BY created_at, context_id, message_id",
        )?;
        statement
            .query_map([grant_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

/// The strings are the `pact_device_authorizations.status` column and its
/// `CHECK` constraint. Changing one breaks every stored row that holds it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DeviceStatus {
    Pending,
    Approved,
    Denied,
    Consumed,
}

aether_core::named!(DeviceStatus {
    Pending => "pending",
    Approved => "approved",
    Denied => "denied",
    Consumed => "consumed",
});

/// The device-authorization columns `poll_device` decides on.
struct DeviceRow {
    status: String,
    grant_id: Option<String>,
    interval: i64,
    last_polled_at: Option<i64>,
    expires_at: i64,
}

fn insert_grant(connection: &Connection, grant: &PactGrant) -> Result<(), StoreError> {
    connection.execute(
        &format!("INSERT INTO pact_grants({GRANT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
        params![
            grant.grant_id,
            grant.brand_id,
            grant.pa_issuer,
            grant.pa_subject,
            grant.brand_user,
            grant.scopes,
            grant.created_at,
            grant.expires_at,
        ],
    )?;
    Ok(())
}

fn query_grant(
    connection: &Connection,
    grant_id: &str,
) -> Result<Option<PactGrant>, StoreError> {
    Ok(connection
        .query_row(
            &format!("SELECT {GRANT_COLUMNS} FROM pact_grants WHERE grant_id=?1"),
            [grant_id],
            PactGrant::from_row,
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::{
        DevicePoll, NewDeviceAuthorization, PactGrant, PactStore, Reservation,
    };
    use crate::StoreError;

    fn store() -> (tempfile::TempDir, PactStore) {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = PactStore::open(directory.path().join("pact.sqlite"))
            .expect("store must open");
        (directory, store)
    }

    fn device(user_code: &str, hash: &str) -> NewDeviceAuthorization {
        NewDeviceAuthorization {
            device_code_hash: hash.to_owned(),
            user_code: user_code.to_owned(),
            brand_id: "review".to_owned(),
            pa_issuer: "https://pa.example".to_owned(),
            pa_subject: "user-1".to_owned(),
            scopes: "review:read_only".to_owned(),
            interval_seconds: 5,
            created_at: 100,
            expires_at: 700,
        }
    }

    fn grant(id: &str) -> PactGrant {
        PactGrant {
            grant_id: id.to_owned(),
            brand_id: "review".to_owned(),
            pa_issuer: "https://pa.example".to_owned(),
            pa_subject: "user-1".to_owned(),
            brand_user: "owner".to_owned(),
            scopes: "review:read_only".to_owned(),
            created_at: 100,
            expires_at: 10_000,
        }
    }

    #[test]
    fn device_status_names_stay_the_same_and_parse_back() {
        use super::DeviceStatus;
        for (status, name) in [
            (DeviceStatus::Pending, "pending"),
            (DeviceStatus::Approved, "approved"),
            (DeviceStatus::Denied, "denied"),
            (DeviceStatus::Consumed, "consumed"),
        ] {
            assert_eq!(status.as_str(), name);
            assert_eq!(DeviceStatus::parse(name), Some(status));
        }
        assert_eq!(DeviceStatus::parse("Pending"), None);
    }

    #[test]
    fn a_retry_returns_the_stored_reply_and_a_running_turn_has_none() {
        let (_directory, mut store) = store();
        store.create_context("c", "review", "https://pa.example", "user-1", 1).unwrap();
        assert_eq!(
            store.reserve_message("c", "m1", "hello", 2).unwrap(),
            Reservation::New
        );
        assert_eq!(
            store.reserve_message("c", "m1", "hello", 3).unwrap(),
            Reservation::Existing(None),
        );
        store.complete_message("c", "m1", Some("hi"), "{\"message\":1}").unwrap();
        assert_eq!(
            store.reserve_message("c", "m1", "hello", 4).unwrap(),
            Reservation::Existing(Some("{\"message\":1}".to_owned())),
        );
        assert_eq!(
            store.complete_message("c", "m1", Some("again"), "{}"),
            Err(StoreError::InvalidState),
        );
    }

    #[test]
    fn a_discarded_turn_can_run_again() {
        let (_directory, mut store) = store();
        store.create_context("c", "review", "https://pa.example", "user-1", 1).unwrap();
        store.reserve_message("c", "m1", "hello", 2).unwrap();
        store.discard_message("c", "m1").unwrap();
        assert_eq!(
            store.reserve_message("c", "m1", "hello", 3).unwrap(),
            Reservation::New
        );
    }

    #[test]
    fn the_transcript_holds_answered_turns_before_the_message_oldest_first() {
        let (_directory, mut store) = store();
        store.create_context("c", "review", "https://pa.example", "user-1", 1).unwrap();
        for (id, text, reply) in
            [("m1", "one", Some("1")), ("m2", "two", None), ("m3", "three", Some("3"))]
        {
            store.reserve_message("c", id, text, 2).unwrap();
            store.complete_message("c", id, reply, "{}").unwrap();
        }
        store.reserve_message("c", "m4", "four", 3).unwrap();
        assert_eq!(
            store.transcript("c", "m4", 10).unwrap(),
            [("one".to_owned(), "1".to_owned()), ("three".to_owned(), "3".to_owned())],
        );
        assert_eq!(
            store.transcript("c", "m4", 1).unwrap(),
            [("three".to_owned(), "3".to_owned())],
        );
    }

    #[test]
    fn an_idle_context_closes_and_loses_its_messages() {
        let (_directory, mut store) = store();
        store
            .create_context("old", "review", "https://pa.example", "user-1", 1)
            .unwrap();
        store
            .create_context("new", "review", "https://pa.example", "user-1", 1)
            .unwrap();
        store.reserve_message("old", "m1", "secret", 2).unwrap();
        store.reserve_message("new", "m1", "kept", 50).unwrap();
        assert_eq!(store.close_idle_contexts(10, 60).unwrap(), 1);
        assert!(store.context("old").unwrap().unwrap().closed);
        assert!(!store.context("new").unwrap().unwrap().closed);
        assert_eq!(
            store.reserve_message("old", "m2", "late", 61),
            Err(StoreError::NotFound),
        );
        assert_eq!(
            store.reserve_message("new", "m1", "kept", 61).unwrap(),
            Reservation::Existing(None),
        );
    }

    #[test]
    fn a_context_binds_to_one_brand_user() {
        let (_directory, store) = store();
        store.create_context("c", "review", "https://pa.example", "user-1", 1).unwrap();
        store.bind_context_user("c", "owner").unwrap();
        store.bind_context_user("c", "owner").unwrap();
        assert_eq!(
            store.bind_context_user("c", "other"),
            Err(StoreError::Unauthorized)
        );
    }

    #[test]
    fn a_device_poll_waits_slows_down_and_consumes_the_approval_once() {
        let (_directory, mut store) = store();
        store.create_device_authorization(&device("ABCD-EFGH", "hash")).unwrap();
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 200)
                .unwrap(),
            DevicePoll::Pending { slow_down: false }
        );
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 202)
                .unwrap(),
            DevicePoll::Pending { slow_down: true }
        );
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 210)
                .unwrap(),
            DevicePoll::Pending { slow_down: false }
        );
        assert!(store.pending_device("ABCD-EFGH", 210).unwrap().is_some());
        store.approve_device("ABCD-EFGH", &grant("g"), 220).unwrap();
        assert!(store.pending_device("ABCD-EFGH", 220).unwrap().is_none());
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 230)
                .unwrap(),
            DevicePoll::Approved(grant("g"))
        );
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 240)
                .unwrap(),
            DevicePoll::Consumed
        );
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 800)
                .unwrap(),
            DevicePoll::Expired
        );
    }

    #[test]
    fn only_the_starting_agent_user_and_brand_can_poll_a_device_code() {
        let (_directory, mut store) = store();
        store.create_device_authorization(&device("ABCD-EFGH", "hash")).unwrap();
        store.approve_device("ABCD-EFGH", &grant("g"), 150).unwrap();
        for (brand, issuer, subject) in [
            ("review", "https://other.example", "user-1"),
            ("other", "https://pa.example", "user-1"),
            ("review", "https://pa.example", "user-2"),
        ] {
            assert_eq!(
                store.poll_device("hash", brand, issuer, subject, 200).unwrap(),
                DevicePoll::Unknown,
            );
        }
        // Nobody else's poll consumed the approval.
        assert_eq!(
            store
                .poll_device("hash", "review", "https://pa.example", "user-1", 200)
                .unwrap(),
            DevicePoll::Approved(grant("g")),
        );
    }

    #[test]
    fn a_denied_or_expired_authorization_cannot_be_approved() {
        let (_directory, mut store) = store();
        store.create_device_authorization(&device("AAAA-AAAA", "one")).unwrap();
        store.create_device_authorization(&device("BBBB-BBBB", "two")).unwrap();
        store.deny_device("AAAA-AAAA").unwrap();
        assert_eq!(
            store.approve_device("AAAA-AAAA", &grant("g1"), 200),
            Err(StoreError::NotFound),
        );
        assert_eq!(
            store
                .poll_device("one", "review", "https://pa.example", "user-1", 200)
                .unwrap(),
            DevicePoll::Denied,
        );
        assert_eq!(
            store.approve_device("BBBB-BBBB", &grant("g2"), 800),
            Err(StoreError::NotFound),
        );
        assert!(store.grant("g1").unwrap().is_none());
        assert!(store.grant("g2").unwrap().is_none());
    }

    #[test]
    fn a_refresh_token_works_once_and_not_after_it_expires() {
        let (_directory, mut store) = store();
        store.create_device_authorization(&device("ABCD-EFGH", "hash")).unwrap();
        store.approve_device("ABCD-EFGH", &grant("g"), 200).unwrap();
        store.put_refresh_token("once", "g", 5_000).unwrap();
        store.put_refresh_token("late", "g", 5_000).unwrap();
        assert_eq!(store.use_refresh_token("once", 300).unwrap(), Some(grant("g")));
        assert_eq!(store.use_refresh_token("once", 301).unwrap(), None);
        assert_eq!(store.use_refresh_token("late", 5_000).unwrap(), None);
        assert_eq!(store.use_refresh_token("unknown", 300).unwrap(), None);
    }

    #[test]
    fn receipts_are_kept_per_grant() {
        let (_directory, store) = store();
        store.put_receipt("c", "m1", "g", "a.b.c", 1).unwrap();
        store.put_receipt("c", "m2", "g", "d.e.f", 2).unwrap();
        store.put_receipt("c", "m3", "other", "g.h.i", 3).unwrap();
        assert_eq!(store.receipts("g").unwrap(), ["a.b.c", "d.e.f"]);
    }
}
