//! The direct scope between two people: the invitation a host gives, the
//! contact a caller keeps, and one text task sent to that contact. The CLI
//! and the admin web UI both use these functions.

pub mod cli;

use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime},
};

use aether_store::NodeStore;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::{
    data_dir::DataDir,
    discovery::{Invitation, wire::WireInvitation},
    transport::{A2aAuthorization, A2aRequest, Frame, FrameKind, TransportError},
};

/// The scope id and kind of a direct connection.
pub const SCOPE: &str = "direct";

pub const INVITE_WORKSPACE_ID: &str = "workspace";

pub const DIRECT_OFFER_DAYS: [u64; 3] = [1, 7, 30];
pub const DEFAULT_DIRECT_ACCESS_DAYS: u64 = 7;

#[must_use]
pub fn valid_direct_invite_days(days: u64) -> bool {
    DIRECT_OFFER_DAYS.contains(&days)
}

#[must_use]
pub fn grant_is_low_risk_invite(grant: &aether_core::Grant) -> bool {
    grant.scope_id.as_str() == SCOPE
        && grant.approval_rule == aether_core::ApprovalRule::Automatic
        && grant
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == crate::execution::GENERAL_SKILL)
        && grant
            .workspaces
            .iter()
            .any(|workspace| workspace.as_str() == INVITE_WORKSPACE_ID)
}

/// The `format` value of an [`InvitationBundle`] without an Agent Card.
pub const BUNDLE_FORMAT_V1: &str = "aether-invitation/v1";
/// The `format` value of an [`InvitationBundle`] with a signed Agent Card.
pub const BUNDLE_FORMAT_V2: &str = "aether-invitation/v2";

/// Everything a caller needs from a host, in one file.
///
/// It holds the host's signed invitation, the caller's signed
/// authorization, and the relay the host listens through. The authorization
/// names the caller's endpoint, so only the caller's key can use it. A `v2`
/// bundle also holds the host's Agent Card, signed with the invitation key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvitationBundle {
    pub format: String,
    pub invitation: WireInvitation,
    pub authorization: A2aAuthorization,
    pub relay_url: Option<String>,
    /// The exact JSON text of the Agent Card (`v2` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card: Option<String>,
    /// The signature of the card with the invitation key, unpadded
    /// base64url (`v2` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card_signature: Option<String>,
}

/// The name of connect codes while the node has no name on the hub.
pub struct HostName;

impl crate::settings::Setting for HostName {
    const KEY: &'static str = "node.runtime.host_name";
    type Value = String;

    fn decode(stored: String) -> Result<Self::Value, aether_store::StoreError> {
        Ok(stored)
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, aether_store::StoreError> {
        if !valid_name(value) {
            return Err(aether_store::StoreError::InvalidState);
        }
        Ok(Some(value.clone()))
    }
}

/// The prefix of an [`InvitationBundle`]'s one-line form.
pub const INVITE_PREFIX: &str = "aether-invite:";

impl InvitationBundle {
    /// A bundle without an Agent Card.
    #[must_use]
    pub fn v1(
        invitation: WireInvitation,
        authorization: A2aAuthorization,
        relay_url: Option<String>,
    ) -> Self {
        Self {
            format: BUNDLE_FORMAT_V1.to_owned(),
            invitation,
            authorization,
            relay_url,
            agent_card: None,
            agent_card_signature: None,
        }
    }

    /// A bundle with the Agent Card `card` and its `signature`.
    #[must_use]
    pub fn v2(
        invitation: WireInvitation,
        authorization: A2aAuthorization,
        relay_url: Option<String>,
        card: String,
        signature: String,
    ) -> Self {
        Self {
            format: BUNDLE_FORMAT_V2.to_owned(),
            invitation,
            authorization,
            relay_url,
            agent_card: Some(card),
            agent_card_signature: Some(signature),
        }
    }

    /// The one-line form a host can paste into a chat: [`INVITE_PREFIX`] and
    /// the bundle's JSON in unpadded base64url.
    pub fn encode(&self) -> Result<String, DirectError> {
        let json = serde_json::to_vec(self).map_err(|_| DirectError::Invalid)?;
        Ok(format!("{INVITE_PREFIX}{}", URL_SAFE_NO_PAD.encode(json)))
    }

    /// Reads a bundle from its one-line form or from its JSON.
    pub fn decode(text: &str) -> Result<Self, DirectError> {
        let text = text.trim();
        let bundle: Self = match text.strip_prefix(INVITE_PREFIX) {
            Some(encoded) => {
                let json = URL_SAFE_NO_PAD
                    .decode(encoded.trim())
                    .map_err(|_| DirectError::Invalid)?;
                serde_json::from_slice(&json)
            }
            None => serde_json::from_str(text),
        }
        .map_err(|_| DirectError::Invalid)?;
        let card = (bundle.agent_card.is_some(), bundle.agent_card_signature.is_some());
        match (bundle.format.as_str(), card) {
            (BUNDLE_FORMAT_V1, (false, false)) | (BUNDLE_FORMAT_V2, (true, true)) => {
                Ok(bundle)
            }
            _ => Err(DirectError::Invalid),
        }
    }
}

/// Whether `name` can be an agent name in a contact id and a grant: 1 to 64
/// letters, digits, `.`, `_` or `-`, then optionally `#` and the 4 digits
/// that a hub gives, as in `ana#0042`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    if name.contains('#') {
        return crate::hub_client::valid_full_name(name);
    }
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Sign one person's direct access. The caller applies the grant to the
/// running node before it creates the connect code.
pub fn sign_invite_access(
    data_dir: &Path,
    friend_name: &str,
    endpoint_id: &str,
    days: u64,
) -> Result<(A2aAuthorization, aether_core::Grant), DirectError> {
    if !valid_name(friend_name)
        || endpoint_id.parse::<iroh::EndpointId>().is_err()
        || !valid_direct_invite_days(days)
    {
        return Err(DirectError::Invalid);
    }
    let issuer =
        DataDir::new(data_dir).issuer_key().map_err(|_| DirectError::Unavailable)?;
    let access = crate::identity::AccessInput {
        issuer_agent_id: crate::identity::NODE_ISSUER.to_owned(),
        subject_agent_id: friend_name.to_owned(),
        subject_endpoint_id: endpoint_id.to_owned(),
        scope_id: SCOPE.to_owned(),
        capabilities: vec![crate::execution::GENERAL_SKILL.to_owned()],
        workspaces: vec![INVITE_WORKSPACE_ID.to_owned()],
        access_mode: aether_core::AccessMode::ReadOnly,
        approval_rule: aether_core::ApprovalRule::Automatic,
        lifetime: Duration::from_secs(days.saturating_mul(24 * 60 * 60)),
    };
    let (claim, grant) = crate::identity::sign_access(&issuer, &access)
        .map_err(|_| DirectError::Invalid)?;
    Ok((A2aAuthorization::for_access(&access, &claim), grant))
}

/// Finish a connect code after the running node accepts the grant.
/// `agent_card` is the card and its signature from [`create_card`], made
/// before the grant, so that a card error applies no grant.
pub fn encode_invite_after_grant(
    data_dir: &Path,
    agent_name: &str,
    days: u64,
    authorization: A2aAuthorization,
    relay_url: Option<String>,
    agent_card: (String, String),
) -> Result<String, DirectError> {
    if !valid_name(agent_name) || !valid_direct_invite_days(days) {
        return Err(DirectError::Invalid);
    }
    let invitation = create_invitation(
        data_dir,
        agent_name,
        Duration::from_secs(days.saturating_mul(24 * 60 * 60)),
    )?;
    let (card, card_signature) = agent_card;
    InvitationBundle::v2(invitation, authorization, relay_url, card, card_signature)
        .encode()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum DirectError {
    #[error("direct invitation is invalid")]
    Invalid,
    #[error("direct contacts are unavailable")]
    Unavailable,
    #[error("direct contact cannot be resolved")]
    UnknownContact,
    #[error("direct invitation is for another endpoint")]
    OtherEndpoint,
    #[error("another contact already has this name")]
    NameInUse,
}

/// Signs an invitation to this node, valid for `expires_in`. `agent_id` is
/// the name the caller will know this node by.
pub fn create_invitation(
    data_dir: &Path,
    agent_id: &str,
    expires_in: Duration,
) -> Result<WireInvitation, DirectError> {
    if agent_id.is_empty() || expires_in.is_zero() {
        return Err(DirectError::Invalid);
    }
    let layout = DataDir::new(data_dir);
    let endpoint = layout.endpoint_key().map_err(|_| DirectError::Unavailable)?;
    let signing_key = layout.discovery_key().map_err(|_| DirectError::Unavailable)?;
    let expires_at =
        SystemTime::now().checked_add(expires_in).ok_or(DirectError::Invalid)?;
    let invitation = crate::discovery::signed_invitation(
        &signing_key,
        SCOPE.to_owned(),
        agent_id.to_owned(),
        endpoint.public().to_string(),
        expires_at,
    );
    Ok(WireInvitation::from_invitation(&invitation))
}

/// The Agent Card of this node for the agent name `agent_id`, and its
/// signature with the invitation key, in unpadded base64url.
pub fn create_card(
    data_dir: &Path,
    agent_id: &str,
) -> Result<(String, String), DirectError> {
    if !valid_name(agent_id) {
        return Err(DirectError::Invalid);
    }
    let endpoint = endpoint_id(data_dir)?;
    let card = crate::agent_card::compose(
        &crate::agent_card::settings(data_dir),
        agent_id,
        &endpoint,
    );
    let key =
        DataDir::new(data_dir).discovery_key().map_err(|_| DirectError::Unavailable)?;
    let signature = crate::discovery::sign_agent_card(&key, &endpoint, agent_id, &card);
    let card = String::from_utf8(card).map_err(|_| DirectError::Invalid)?;
    Ok((card, URL_SAFE_NO_PAD.encode(signature)))
}

/// Verifies `wire` and stores it as a contact in `data_dir`.
pub fn import_invitation(
    data_dir: &Path,
    wire: &WireInvitation,
) -> Result<Invitation, DirectError> {
    import_invitation_in(&open_store(data_dir)?, wire)
}

fn import_invitation_in(
    store: &NodeStore,
    wire: &WireInvitation,
) -> Result<Invitation, DirectError> {
    let invitation = match wire.to_invitation() {
        Ok(invitation) if invitation.scope_id == SCOPE => invitation,
        _ => return Err(DirectError::Invalid),
    };
    // A name stays with its endpoint: an invite from another key does not
    // take over a contact that is still valid.
    if contact(store, &invitation.agent_id)
        .is_ok_and(|current| current.endpoint_id != invitation.endpoint_id)
    {
        return Err(DirectError::NameInUse);
    }
    store
        .put_contact_record(
            &wire.signed_store_record().map_err(|_| DirectError::Invalid)?,
        )
        .map_err(|_| DirectError::Unavailable)?;
    Ok(invitation)
}

/// The stored, still valid invitation of the contact named `agent_id`.
pub fn contact(
    store: &NodeStore,
    agent_id: &str,
) -> Result<WireInvitation, DirectError> {
    if agent_id.is_empty() {
        return Err(DirectError::UnknownContact);
    }
    let stored =
        store.contact_record(agent_id).map_err(|_| DirectError::UnknownContact)?;
    let now = aether_core::time::now();
    if stored.scope_id != SCOPE
        || stored.agent_id != agent_id
        || stored.expires_at <= now
    {
        return Err(DirectError::UnknownContact);
    }
    let wire = serde_json::from_slice::<WireInvitation>(&stored.record)
        .map_err(|_| DirectError::UnknownContact)?;
    let invitation = wire.to_invitation().map_err(|_| DirectError::UnknownContact)?;
    if invitation.scope_id != SCOPE || invitation.agent_id != agent_id {
        return Err(DirectError::UnknownContact);
    }
    Ok(wire)
}

/// The prefix of the configuration key of an imported contact's bundle.
const BUNDLE_PREFIX: &str = "caller.contact.";

fn bundle_key(agent_id: &str) -> String {
    format!("{BUNDLE_PREFIX}{agent_id}")
}

/// The configuration key of the exact Agent Card of the contact at
/// `endpoint_id`.
fn card_key(endpoint_id: &str) -> String {
    format!("caller.card.{endpoint_id}")
}

/// The signed Agent Card of a `v2` bundle, checked; `None` for `v1`.
fn checked_card(bundle: &InvitationBundle) -> Result<Option<&str>, DirectError> {
    let (Some(card), Some(signature)) =
        (&bundle.agent_card, &bundle.agent_card_signature)
    else {
        return Ok(None);
    };
    let invitation = &bundle.invitation;
    let signature =
        URL_SAFE_NO_PAD.decode(signature).map_err(|_| DirectError::Invalid)?;
    crate::discovery::verify_agent_card(
        &invitation.signing_key,
        &invitation.endpoint_id,
        &invitation.agent_id,
        card.as_bytes(),
        &signature,
    )
    .map_err(|_| DirectError::Invalid)?;
    crate::agent_card::validate(
        card.as_bytes(),
        &invitation.agent_id,
        &invitation.endpoint_id,
    )
    .map_err(|_| DirectError::Invalid)?;
    Ok(Some(card))
}

/// The node store of `data_dir`, which the contact lookups read.
pub fn open_store(data_dir: &Path) -> Result<NodeStore, DirectError> {
    DataDir::new(data_dir).store().map_err(|_| DirectError::Unavailable)
}

/// The agent name of a node without a hub name or `--invite-name`.
///
/// It is `agent-` and the first 8 characters of the endpoint ID. The
/// endpoint ID is already in every invite, so this name tells nothing new.
#[must_use]
pub fn default_agent_name(endpoint_id: &str) -> String {
    format!("agent-{}", endpoint_id.get(..8).unwrap_or(endpoint_id))
}

/// This data directory's endpoint id: what a host needs to invite it.
pub fn endpoint_id(data_dir: &Path) -> Result<String, DirectError> {
    DataDir::new(data_dir)
        .endpoint_key()
        .map(|key| key.public().to_string())
        .map_err(|_| DirectError::Unavailable)
}

/// Verifies `bundle` for the endpoint key in `data_dir`, imports its
/// invitation, and keeps the whole bundle, so a later request can use its
/// authorization and relay without naming them.
pub fn import_bundle(
    data_dir: &Path,
    bundle: &InvitationBundle,
) -> Result<Invitation, DirectError> {
    if bundle.authorization.caller_endpoint_id != endpoint_id(data_dir)? {
        return Err(DirectError::OtherEndpoint);
    }
    if !valid_name(&bundle.invitation.agent_id) {
        return Err(DirectError::Invalid);
    }
    // The card is checked before anything is stored.
    let card = checked_card(bundle)?;
    let store = open_store(data_dir)?;
    let invitation = import_invitation_in(&store, &bundle.invitation)?;
    // The contact keeps the v1 shape; the card text is kept on its own.
    let kept = InvitationBundle::v1(
        bundle.invitation.clone(),
        bundle.authorization.clone(),
        bundle.relay_url.clone(),
    );
    let value = serde_json::to_string(&kept).map_err(|_| DirectError::Invalid)?;
    store
        .put_configuration(&bundle_key(&invitation.agent_id), &value)
        .map_err(|_| DirectError::Unavailable)?;
    match card {
        Some(card) => store.put_configuration(&card_key(&invitation.endpoint_id), card),
        None => store.delete_configuration(&card_key(&invitation.endpoint_id)),
    }
    .map_err(|_| DirectError::Unavailable)?;
    // The same key under a new name is a rename: the old name goes.
    for old in contact_bundles(&store)? {
        if old.invitation.endpoint_id == invitation.endpoint_id
            && old.invitation.agent_id != invitation.agent_id
        {
            remove_contact_in(&store, &old.invitation.agent_id)?;
        }
    }
    Ok(invitation)
}

/// The kept bundle of the contact at `endpoint_id`, when its invitation is
/// still valid.
pub fn contact_by_endpoint(
    store: &NodeStore,
    endpoint_id: &str,
) -> Result<InvitationBundle, DirectError> {
    contacts_by_endpoint(store)?.remove(endpoint_id).ok_or(DirectError::UnknownContact)
}

/// The bundle of each endpoint that [`contact_by_endpoint`] finds.
pub fn contacts_by_endpoint(
    store: &NodeStore,
) -> Result<BTreeMap<String, InvitationBundle>, DirectError> {
    let mut contacts = BTreeMap::new();
    for bundle in contact_bundles(store)? {
        let endpoint_id = &bundle.invitation.endpoint_id;
        if !contacts.contains_key(endpoint_id)
            && contact(store, &bundle.invitation.agent_id)
                .is_ok_and(|wire| wire.endpoint_id == *endpoint_id)
        {
            contacts.insert(endpoint_id.clone(), bundle);
        }
    }
    Ok(contacts)
}

/// The kept bundle of the contact named `agent_id`.
pub fn contact_bundle(
    store: &NodeStore,
    agent_id: &str,
) -> Result<InvitationBundle, DirectError> {
    let value = store
        .configuration(&bundle_key(agent_id))
        .map_err(|_| DirectError::UnknownContact)?;
    serde_json::from_str(&value).map_err(|_| DirectError::UnknownContact)
}

/// Forgets the contact named `agent_id`: its signed invitation and its
/// bundle. [`DirectError::UnknownContact`] when there is no such contact.
pub fn remove_contact(data_dir: &Path, agent_id: &str) -> Result<(), DirectError> {
    remove_contact_in(&open_store(data_dir)?, agent_id)
}

fn remove_contact_in(store: &NodeStore, agent_id: &str) -> Result<(), DirectError> {
    if !valid_name(agent_id) {
        return Err(DirectError::UnknownContact);
    }
    if let Ok(bundle) = contact_bundle(store, agent_id) {
        store
            .delete_configuration(&card_key(&bundle.invitation.endpoint_id))
            .map_err(|_| DirectError::Unavailable)?;
    }
    let record = store.remove_contact_record(agent_id);
    let had_bundle = store.configuration(&bundle_key(agent_id)).is_ok();
    store
        .delete_configuration(&bundle_key(agent_id))
        .map_err(|_| DirectError::Unavailable)?;
    match record {
        Ok(()) => Ok(()),
        Err(aether_store::StoreError::NotFound) if had_bundle => Ok(()),
        Err(aether_store::StoreError::NotFound) => Err(DirectError::UnknownContact),
        Err(_) => Err(DirectError::Unavailable),
    }
}

/// The checked Agent Card of the contact of `bundle`, when its host sent one.
#[must_use]
pub fn contact_card(
    store: &NodeStore,
    bundle: &InvitationBundle,
) -> Option<crate::agent_card::CardView> {
    let card = store.configuration(&card_key(&bundle.invitation.endpoint_id)).ok()?;
    crate::agent_card::validate(
        card.as_bytes(),
        &bundle.invitation.agent_id,
        &bundle.invitation.endpoint_id,
    )
    .ok()
}

/// Every kept bundle, in name order.
pub fn contact_bundles(
    store: &NodeStore,
) -> Result<Vec<InvitationBundle>, DirectError> {
    let values = store
        .configuration_values(BUNDLE_PREFIX)
        .map_err(|_| DirectError::Unavailable)?;
    Ok(values.iter().filter_map(|value| serde_json::from_str(value).ok()).collect())
}

/// The identifiers of one request. Each is a fresh UUID unless the caller
/// supplies it.
#[derive(Debug, Clone)]
pub struct RequestIds {
    pub task_id: String,
    pub idempotency_key: String,
}

impl RequestIds {
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            task_id: uuid::Uuid::new_v4().to_string(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// The request frame for one text task in `scope_id`, carrying
/// `authorization`.
pub fn request_frame(
    mut authorization: A2aAuthorization,
    scope_id: &str,
    text: &str,
    ids: RequestIds,
) -> Result<Frame, DirectError> {
    authorization.request_id.clone_from(&ids.task_id);
    let mut payload: A2aRequest =
        serde_json::from_value(crate::transport::text_request_payload(
            &ids.task_id,
            &ids.idempotency_key,
            &uuid::Uuid::new_v4().to_string(),
            text,
        ))
        .map_err(|_| DirectError::Invalid)?;
    payload.authorization = Some(authorization);
    Ok(Frame {
        version: 0,
        kind: FrameKind::Request,
        exchange_id: uuid::Uuid::new_v4().to_string(),
        scope_id: scope_id.to_owned(),
        task_id: Some(ids.task_id),
        payload: serde_json::to_value(payload).map_err(|_| DirectError::Invalid)?,
    })
}

/// The request frame that sends the task `ids` again.
///
/// It has the same task ID, request ID and idempotency key, so the host keeps
/// one task, and a new exchange ID and nonce. `after` is the last event
/// sequence already seen.
pub fn resend_frame(
    authorization: A2aAuthorization,
    text: &str,
    ids: RequestIds,
    after: u64,
) -> Result<Frame, DirectError> {
    let mut frame = request_frame(authorization, SCOPE, text, ids)?;
    let mut payload: A2aRequest =
        serde_json::from_value(frame.payload).map_err(|_| DirectError::Invalid)?;
    payload.last_event_sequence = Some(after);
    frame.payload = serde_json::to_value(payload).map_err(|_| DirectError::Invalid)?;
    Ok(frame)
}

/// The kept bundle of the contact named `agent_id`, when its invitation is
/// still valid.
pub fn contact_by_name(
    store: &NodeStore,
    agent_id: &str,
) -> Result<InvitationBundle, DirectError> {
    contact(store, agent_id)?;
    contact_bundle(store, agent_id)
}

/// When the access of `bundle` ends: the earlier of the invitation expiry
/// and the claim expiry, in Unix seconds.
#[must_use]
pub fn access_ends_at(bundle: &InvitationBundle) -> u64 {
    bundle.invitation.expires_at.min(bundle.authorization.claim.expires_at)
}

/// Sends `request` to the endpoint in `invitation`: through `relay_url` when
/// one is set, otherwise directly to `address`.
pub async fn send(
    data_dir: &Path,
    invitation: &WireInvitation,
    address: Option<&str>,
    relay_url: Option<&str>,
    request: Frame,
) -> Result<Vec<Frame>, TransportError> {
    match (relay_url, address) {
        (Some(relay_url), _) => {
            crate::transport::request_once_via_relay(
                data_dir,
                &invitation.endpoint_id,
                relay_url,
                request,
            )
            .await
        }
        (None, Some(address)) => {
            crate::transport::request_once(
                data_dir,
                &invitation.endpoint_id,
                address,
                request,
            )
            .await
        }
        (None, None) => Err(TransportError::Address),
    }
}

/// What a contact's agent answered to one task.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub task_id: String,
    /// The task's last state, such as `TASK_STATE_COMPLETED`.
    pub state: String,
    /// The text of the task's artifacts, or the error the host gave.
    pub text: String,
    /// Whether the host returned an error instead of a result.
    pub failed: bool,
}

/// The answer in the frames of one exchange: the text of the result's
/// artifacts, or the message of an error.
#[must_use]
pub fn answer(task_id: String, frames: &[Frame]) -> Option<Answer> {
    let last = frames
        .iter()
        .rev()
        .find(|frame| matches!(frame.kind, FrameKind::Result | FrameKind::Error))?;
    if last.kind == FrameKind::Error {
        let text_at =
            |pointer| last.payload.pointer(pointer).and_then(serde_json::Value::as_str);
        let state = text_at("/data/state").unwrap_or("failed");
        let message = text_at("/message").unwrap_or("the task failed");
        return Some(Answer {
            task_id,
            state: state.to_owned(),
            text: message.to_owned(),
            failed: true,
        });
    }
    let text = last
        .payload
        .pointer("/task/artifacts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artifact| {
            artifact.get("parts").and_then(serde_json::Value::as_array)
        })
        .flatten()
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Answer {
        task_id,
        state: last
            .payload
            .pointer("/task/status/state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        text,
        failed: false,
    })
}

#[cfg(test)]
mod answer_tests {
    use super::answer;
    use crate::transport::{Frame, FrameKind};

    fn frame(kind: FrameKind, payload: serde_json::Value) -> Frame {
        Frame {
            version: 0,
            kind,
            exchange_id: "exchange".to_owned(),
            scope_id: "direct".to_owned(),
            task_id: Some("task".to_owned()),
            payload,
        }
    }

    #[test]
    fn the_answer_joins_the_text_of_the_result_artifacts() {
        let frames = [
            frame(FrameKind::Event, serde_json::json!({})),
            frame(
                FrameKind::Result,
                serde_json::json!({"task": {
                    "status": {"state": "TASK_STATE_COMPLETED"},
                    "artifacts": [
                        {"parts": [{"text": "one"}, {"data": {}}]},
                        {"parts": [{"text": "two"}]},
                    ],
                }}),
            ),
        ];
        let answer = answer("task".to_owned(), &frames).expect("a result must answer");
        assert_eq!(answer.state, "TASK_STATE_COMPLETED");
        assert_eq!(answer.text, "one\ntwo");
        assert!(!answer.failed);
    }

    #[test]
    fn an_error_frame_is_a_failed_answer_with_its_message() {
        let frames = [frame(
            FrameKind::Error,
            serde_json::json!({"code": -32010, "message": "permission denied", "data": {"state": "rejected"}}),
        )];
        let failed = answer("task".to_owned(), &frames).expect("an error must answer");
        assert_eq!(failed.state, "rejected");
        assert_eq!(failed.text, "permission denied");
        assert!(failed.failed);
        assert_eq!(answer("task".to_owned(), &[]), None);
    }
}

/// A bundle from the host in `host_dir` named `tudor` for the endpoint
/// `caller_endpoint`, with a signed Agent Card when `with_card`.
#[cfg(test)]
pub(crate) fn test_bundle(
    host_dir: &Path,
    caller_endpoint: &str,
    with_card: bool,
) -> InvitationBundle {
    test_bundle_named(host_dir, caller_endpoint, "tudor", with_card)
}

/// As [`test_bundle`], with the host named `agent`.
#[cfg(test)]
pub(crate) fn test_bundle_named(
    host_dir: &Path,
    caller_endpoint: &str,
    agent: &str,
    with_card: bool,
) -> InvitationBundle {
    let access = crate::identity::AccessInput {
        issuer_agent_id: "node".to_owned(),
        subject_agent_id: "ana".to_owned(),
        subject_endpoint_id: caller_endpoint.to_owned(),
        scope_id: SCOPE.to_owned(),
        capabilities: vec![crate::execution::GENERAL_SKILL.to_owned()],
        workspaces: vec!["workspace".to_owned()],
        access_mode: aether_core::AccessMode::ReadOnly,
        approval_rule: aether_core::ApprovalRule::Automatic,
        lifetime: Duration::from_secs(60),
    };
    let (claim, _) = crate::identity::sign_access(
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
        &access,
    )
    .expect("access must sign");
    let authorization = crate::transport::A2aAuthorization::for_access(&access, &claim);
    let invitation = create_invitation(host_dir, agent, Duration::from_secs(60))
        .expect("invitation");
    let relay_url = Some("https://relay.test".to_owned());
    if !with_card {
        return InvitationBundle::v1(invitation, authorization, relay_url);
    }
    let card = crate::agent_card::compose(
        &crate::agent_card::CardSettings::default(),
        &invitation.agent_id,
        &invitation.endpoint_id,
    );
    let key = DataDir::new(host_dir).discovery_key().expect("discovery key must load");
    let signature = crate::discovery::sign_agent_card(
        &key,
        &invitation.endpoint_id,
        &invitation.agent_id,
        &card,
    );
    InvitationBundle::v2(
        invitation,
        authorization,
        relay_url,
        String::from_utf8(card).expect("the card is UTF-8"),
        URL_SAFE_NO_PAD.encode(signature),
    )
}

#[cfg(test)]
mod tests {
    /// A bundle from a fresh host for the endpoint key in `caller_dir`.
    fn bundle_for(
        host_dir: &std::path::Path,
        caller_endpoint: &str,
    ) -> super::InvitationBundle {
        bundle_named(host_dir, caller_endpoint, "tudor")
    }

    /// A bundle from the host in `host_dir` that names it `agent`.
    fn bundle_named(
        host_dir: &std::path::Path,
        caller_endpoint: &str,
        agent: &str,
    ) -> super::InvitationBundle {
        let access = crate::identity::AccessInput {
            issuer_agent_id: "node".to_owned(),
            subject_agent_id: "ana".to_owned(),
            subject_endpoint_id: caller_endpoint.to_owned(),
            scope_id: super::SCOPE.to_owned(),
            capabilities: vec![crate::execution::GENERAL_SKILL.to_owned()],
            workspaces: vec!["workspace".to_owned()],
            access_mode: aether_core::AccessMode::ReadOnly,
            approval_rule: aether_core::ApprovalRule::Automatic,
            lifetime: std::time::Duration::from_secs(60),
        };
        let (claim, _) = crate::identity::sign_access(
            &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
            &access,
        )
        .expect("access must sign");
        let authorization =
            crate::transport::A2aAuthorization::for_access(&access, &claim);
        super::InvitationBundle {
            format: super::BUNDLE_FORMAT_V1.to_owned(),

            agent_card: None,

            agent_card_signature: None,
            invitation: super::create_invitation(
                host_dir,
                agent,
                std::time::Duration::from_secs(60),
            )
            .expect("invitation must sign"),
            authorization,
            relay_url: Some("https://relay.test".to_owned()),
        }
    }

    fn store(data_dir: &std::path::Path) -> aether_store::NodeStore {
        super::open_store(data_dir).expect("store must open")
    }

    fn kept_card(caller: &std::path::Path, host: &std::path::Path) -> Option<String> {
        let endpoint = super::endpoint_id(host).expect("endpoint must load");
        super::open_store(caller)
            .expect("store must open")
            .configuration(&super::card_key(&endpoint))
            .ok()
    }

    #[test]
    fn resent_frames_keep_the_task_id_and_key_and_change_the_nonce() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        let bundle = super::test_bundle(host.path(), &endpoint, false);
        let ids = super::RequestIds::fresh();
        let first =
            super::resend_frame(bundle.authorization.clone(), "work", ids.clone(), 0)
                .expect("frame must build");
        let second = super::resend_frame(bundle.authorization, "work", ids.clone(), 3)
            .expect("frame must build");
        let payload = |frame: &super::Frame| -> super::A2aRequest {
            serde_json::from_value(frame.payload.clone()).expect("payload must decode")
        };
        assert_eq!(first.task_id.as_deref(), Some(ids.task_id.as_str()));
        assert_eq!(first.task_id, second.task_id);
        assert_ne!(first.exchange_id, second.exchange_id);
        let (first, second) = (payload(&first), payload(&second));
        assert_eq!(first.idempotency_key, ids.idempotency_key);
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_ne!(first.request_nonce, second.request_nonce);
        assert_eq!(second.last_event_sequence, Some(3));
        let request_id = |payload: &super::A2aRequest| {
            payload
                .authorization
                .as_ref()
                .map(|authorization| authorization.request_id.clone())
        };
        assert_eq!(request_id(&first), Some(ids.task_id));
        assert_eq!(request_id(&first), request_id(&second));
    }

    #[test]
    fn access_ends_at_the_earlier_of_invitation_and_claim() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        let mut bundle = super::test_bundle(host.path(), &endpoint, false);
        bundle.invitation.expires_at = 500;
        bundle.authorization.claim.expires_at = 400;
        assert_eq!(super::access_ends_at(&bundle), 400);
        bundle.authorization.claim.expires_at = 900;
        assert_eq!(super::access_ends_at(&bundle), 500);
        super::import_bundle(
            caller.path(),
            &super::test_bundle(host.path(), &endpoint, false),
        )
        .expect("the bundle must import");
        assert!(super::contact_by_name(&store(caller.path()), "tudor").is_ok());
        assert!(super::contact_by_name(&store(caller.path()), "nobody").is_err());
    }

    #[test]
    fn a_v2_invite_keeps_its_signed_card_by_endpoint() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        let bundle = super::test_bundle(host.path(), &endpoint, true);
        let line = bundle.encode().expect("the line must encode");
        let decoded =
            super::InvitationBundle::decode(&line).expect("the line must decode");
        super::import_bundle(caller.path(), &decoded).expect("the bundle must import");
        assert_eq!(kept_card(caller.path(), host.path()), bundle.agent_card);
        let kept =
            super::contact_bundle(&store(caller.path()), "tudor").expect("contact");
        let card = super::contact_card(&store(caller.path()), &kept)
            .expect("the card must show");
        assert_eq!(card.name, "tudor");
        assert_eq!(card.skill_name, "General tasks");
    }

    #[test]
    fn the_kept_bundle_keeps_the_v1_shape() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(
            caller.path(),
            &super::test_bundle(host.path(), &endpoint, true),
        )
        .expect("the bundle must import");
        let kept =
            super::contact_bundle(&store(caller.path()), "tudor").expect("contact");
        assert_eq!(kept.format, super::BUNDLE_FORMAT_V1);
        assert_eq!((kept.agent_card, kept.agent_card_signature), (None, None));
    }

    #[test]
    fn a_v2_invite_with_a_changed_or_unsigned_card_is_refused() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        let mut changed = super::test_bundle(host.path(), &endpoint, true);
        changed.agent_card = changed.agent_card.map(|card| {
            card.replace("An agent that does tasks", "An agent that leaks")
        });
        assert_eq!(
            super::import_bundle(caller.path(), &changed).err(),
            Some(super::DirectError::Invalid)
        );
        assert!(super::contact(&store(caller.path()), "tudor").is_err());
        assert_eq!(kept_card(caller.path(), host.path()), None);

        let mut unsigned = super::test_bundle(host.path(), &endpoint, true);
        unsigned.agent_card_signature = None;
        let line = unsigned.encode().expect("the line must encode");
        assert!(super::InvitationBundle::decode(&line).is_err());
        let mut v1_with_card = super::test_bundle(host.path(), &endpoint, true);
        v1_with_card.format = super::BUNDLE_FORMAT_V1.to_owned();
        let line = v1_with_card.encode().expect("the line must encode");
        assert!(super::InvitationBundle::decode(&line).is_err());
    }

    #[test]
    fn a_v1_invite_still_imports_and_clears_an_old_card() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(
            caller.path(),
            &super::test_bundle(host.path(), &endpoint, true),
        )
        .expect("the v2 bundle must import");
        assert!(kept_card(caller.path(), host.path()).is_some());
        super::import_bundle(
            caller.path(),
            &super::test_bundle(host.path(), &endpoint, false),
        )
        .expect("the v1 bundle must import");
        assert_eq!(kept_card(caller.path(), host.path()), None);
        super::remove_contact(caller.path(), "tudor").expect("the contact must remove");
    }

    #[test]
    fn an_agent_name_may_end_in_a_hub_number() {
        for good in [
            "ana",
            "ana#0042",
            "a.b_c-d#9999",
            &"a".repeat(64),
            &format!("{}#0001", "a".repeat(32)),
        ] {
            assert!(super::valid_name(good), "{good}");
        }
        for bad in [
            "",
            "#0042",
            "ana#42",
            "ana#00421",
            "ana##0042",
            "a#b#0001",
            "ana#00a2",
            "an a",
            &"a".repeat(65),
        ] {
            assert!(!super::valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_contact_with_a_hub_number_imports_and_resolves() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(
            caller.path(),
            &bundle_named(host.path(), &endpoint, "tudor#0042"),
        )
        .expect("the bundle must import");
        assert_eq!(
            super::contact(&store(caller.path()), "tudor#0042")
                .expect("contact must resolve")
                .agent_id,
            "tudor#0042"
        );
        assert!(super::contact_bundle(&store(caller.path()), "tudor#0042").is_ok());
    }

    #[test]
    fn a_new_name_from_the_same_endpoint_replaces_the_old_contact() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(
            caller.path(),
            &bundle_named(host.path(), &endpoint, "tudor"),
        )
        .expect("the first name must import");
        super::import_bundle(
            caller.path(),
            &bundle_named(host.path(), &endpoint, "tudor-k"),
        )
        .expect("the new name must import");
        let names: Vec<String> = super::contact_bundles(&store(caller.path()))
            .expect("contacts must list")
            .into_iter()
            .map(|bundle| bundle.invitation.agent_id)
            .collect();
        assert_eq!(names, ["tudor-k"]);
        assert!(super::contact(&store(caller.path()), "tudor").is_err());
        let host_endpoint =
            super::endpoint_id(host.path()).expect("endpoint must load");
        assert_eq!(
            super::contact_by_endpoint(&store(caller.path()), &host_endpoint)
                .expect("the contact must resolve")
                .invitation
                .agent_id,
            "tudor-k"
        );
    }

    #[test]
    fn a_name_held_by_another_endpoint_is_refused() {
        let first = tempfile::tempdir().expect("directory must create");
        let second = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(caller.path(), &bundle_for(first.path(), &endpoint))
            .expect("the first host must import");
        assert_eq!(
            super::import_bundle(caller.path(), &bundle_for(second.path(), &endpoint))
                .err(),
            Some(super::DirectError::NameInUse)
        );
        let first_endpoint =
            super::endpoint_id(first.path()).expect("endpoint must load");
        assert_eq!(
            super::contact(&store(caller.path()), "tudor")
                .expect("the contact must stay")
                .endpoint_id,
            first_endpoint
        );
    }

    #[test]
    fn an_invite_line_imports_and_keeps_its_authorization() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        let bundle = bundle_for(host.path(), &endpoint);
        let line = bundle.encode().expect("the bundle must encode");
        assert!(line.starts_with(super::INVITE_PREFIX));
        assert!(!line.contains(char::is_whitespace));
        let decoded = super::InvitationBundle::decode(&format!("  {line}\n"))
            .expect("the line must decode");
        let json = serde_json::to_string(&bundle).expect("bundle must encode");
        assert_eq!(
            serde_json::to_string(
                &super::InvitationBundle::decode(&json).expect("JSON must decode")
            )
            .expect("bundle must encode"),
            json
        );
        let invitation = super::import_bundle(caller.path(), &decoded)
            .expect("the bundle must import");
        assert_eq!(invitation.agent_id, "tudor");
        let kept = super::contact_bundle(&store(caller.path()), "tudor")
            .expect("contact must be kept");
        assert_eq!(kept.relay_url.as_deref(), Some("https://relay.test"));
        assert_eq!(kept.authorization.caller_endpoint_id, endpoint);
        assert_eq!(
            super::contact_bundles(&store(caller.path()))
                .expect("contacts must list")
                .len(),
            1
        );
    }

    #[test]
    fn remove_contact_forgets_the_bundle_and_the_record() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let endpoint = super::endpoint_id(caller.path()).expect("endpoint must load");
        super::import_bundle(caller.path(), &bundle_for(host.path(), &endpoint))
            .expect("the bundle must import");
        super::remove_contact(caller.path(), "tudor").expect("the contact must remove");
        assert!(super::contact_bundle(&store(caller.path()), "tudor").is_err());
        assert_eq!(
            super::contact(&store(caller.path()), "tudor").err(),
            Some(super::DirectError::UnknownContact)
        );
        assert!(
            super::contact_bundles(&store(caller.path()))
                .expect("contacts must list")
                .is_empty()
        );
        assert_eq!(
            super::remove_contact(caller.path(), "tudor"),
            Err(super::DirectError::UnknownContact)
        );
    }

    #[test]
    fn an_invite_for_another_endpoint_or_format_is_refused() {
        let host = tempfile::tempdir().expect("directory must create");
        let caller = tempfile::tempdir().expect("directory must create");
        let other = iroh::SecretKey::generate().public().to_string();
        let bundle = bundle_for(host.path(), &other);
        assert_eq!(
            super::import_bundle(caller.path(), &bundle).err(),
            Some(super::DirectError::OtherEndpoint)
        );
        assert!(
            super::contact_bundles(&store(caller.path()))
                .expect("contacts must list")
                .is_empty()
        );
        let mut unknown = bundle;
        unknown.format = "something-else".to_owned();
        let json = serde_json::to_string(&unknown).expect("bundle must encode");
        assert_eq!(
            super::InvitationBundle::decode(&json).err(),
            Some(super::DirectError::Invalid)
        );
        assert_eq!(
            super::InvitationBundle::decode("aether-invite:not base64!").err(),
            Some(super::DirectError::Invalid)
        );
    }

    #[test]
    fn an_expired_invitation_is_not_imported() {
        let directory = tempfile::tempdir().expect("directory must create");
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let expired = crate::discovery::signed_invitation(
            &signing_key,
            super::SCOPE.to_owned(),
            "tudor".to_owned(),
            iroh::SecretKey::generate().public().to_string(),
            std::time::SystemTime::now() - std::time::Duration::from_secs(1),
        );
        let wire = crate::discovery::wire::WireInvitation::from_invitation(&expired);
        assert_eq!(
            super::import_invitation(directory.path(), &wire).err(),
            Some(super::DirectError::Invalid)
        );
        assert_eq!(
            super::contact(&store(directory.path()), "tudor").err(),
            Some(super::DirectError::UnknownContact)
        );
    }
}
