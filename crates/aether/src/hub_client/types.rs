use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum HubError {
    #[error("hub URL must be an HTTPS origin, or HTTP on a loopback address")]
    InvalidUrl,
    #[error("the hub cannot be reached")]
    Unreachable,
    #[error("the hub refused the signature")]
    Unauthorized,
    #[error("this node has no name on the hub")]
    NoAccount,
    #[error("the hub refused the request")]
    Forbidden,
    #[error("the hub does not know this name or invite")]
    NotFound,
    #[error("this name belongs to someone else")]
    Taken,
    #[error("the hub refused the request as invalid")]
    Invalid,
    #[error("too many requests to the hub")]
    RateLimited,
    #[error("the hub returned an unexpected answer")]
    Unexpected,
    #[error("this hub is older than this node")]
    OldHub,
    #[error("this node is older than the hub")]
    OldNode,
    #[error("a hub limit is reached")]
    Limit,
}

impl HubError {
    /// Whether the hub may have done the request although it refused
    /// nothing: the answer was lost, or it was a server error or could not
    /// be read. Any other error is a refusal, or came before the request.
    #[must_use]
    pub const fn may_have_applied(self) -> bool {
        matches!(self, Self::Unreachable | Self::Unexpected)
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    #[default]
    Personal,
    Service,
}

impl AgentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Service => "service",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "personal" => Some(Self::Personal),
            "service" => Some(Self::Service),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Account {
    pub handle: String,
    pub endpoint: String,
    #[serde(default)]
    pub kind: AgentKind,
    #[serde(default)]
    pub searchable: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Requested,
    Accepted,
    Delivered,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    #[default]
    Invite,
    Request,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub id: String,
    pub from: String,
    pub to: String,
    pub role: Role,
    pub days: u64,
    pub status: Status,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub receiver_endpoint: Option<String>,
    #[serde(default)]
    pub bundle: Option<String>,
    #[serde(default)]
    pub sender_endpoint: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Invites {
    pub version: u64,
    pub invites: Vec<Invite>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub handle: String,
    pub endpoint: String,
    pub online: bool,
}

#[derive(Deserialize)]
pub(super) struct Contacts {
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct Block {
    pub endpoint: String,
    pub handle: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Blocks {
    pub blocks: Vec<Block>,
}

#[derive(Serialize)]
pub(super) struct ClaimBody<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<AgentKind>,
}

#[derive(Serialize)]
pub(super) struct SearchBody {
    pub searchable: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldListing {
    pub listed: bool,
    pub listing: String,
    pub signature: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    pub handle: String,
    pub listing: String,
    pub signature: String,
    pub expires_at: u64,
    pub online: bool,
    #[serde(default)]
    pub contact: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AgentRows {
    pub agents: Vec<AgentRow>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct PersonRow {
    pub handle: String,
    #[serde(default)]
    pub contact: bool,
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct PersonRows {
    pub people: Vec<PersonRow>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerLink {
    pub id: String,
    pub role: AgentKind,
    pub service: String,
    pub personal: String,
    pub expires_at: u64,
    pub confirmed: bool,
    #[serde(default)]
    pub personal_endpoint: Option<String>,
    #[serde(default)]
    pub service_endpoint: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OwnerLinks {
    pub links: Vec<OwnerLink>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OwnerClaimBody<'a> {
    pub service: &'a str,
    pub expires_at: u64,
    pub signature: &'a str,
}

#[derive(Serialize)]
pub(super) struct SignatureBody<'a> {
    pub signature: &'a str,
}

#[derive(Deserialize)]
pub(super) struct HubInfo {
    pub protocol: u64,
}

#[derive(Serialize)]
pub(super) struct InviteBody<'a> {
    pub to: &'a str,
    pub days: u64,
}

#[derive(Serialize)]
pub(super) struct ApproveBody {
    pub days: u64,
}

#[derive(Serialize)]
pub(super) struct BundleBody<'a> {
    pub bundle: &'a str,
}
