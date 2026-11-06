#![allow(clippy::redundant_pub_crate)]
use schemars::JsonSchema;
use serde::Deserialize;

use crate::admin::ChangeRequest;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditArguments {
    pub(crate) cursor: Option<usize>,
    pub(crate) limit: Option<usize>,
    pub(crate) fields: Vec<AuditField>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditField {
    Actor,
    Action,
    Result,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeArguments {
    pub(crate) change: ChangeRequest,
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelArguments {
    pub(crate) task_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendArguments {
    /// The agent's name, from `aether.agents.list`.
    pub(crate) agent: String,
    /// The task, in plain words.
    pub(crate) text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxListArguments {
    /// The `nextCursor` of the previous page.
    pub(crate) cursor: Option<String>,
    /// 1 to 50; 20 when it is missing.
    pub(crate) limit: Option<usize>,
    /// Only tasks in this status.
    pub(crate) status: Option<String>,
    /// Only tasks whose answer was not read.
    pub(crate) unread_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxReadArguments {
    /// The task ID from `aether.task.send` or `aether.inbox.list`.
    pub(crate) task_id: String,
    /// The `nextOffset` of the previous part; 0 when it is missing.
    pub(crate) offset: Option<usize>,
    /// 0 to 20: how long to wait for a task that is still waiting.
    pub(crate) wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentsFindArguments {
    /// Words about the service, such as "contract review": 1 to 100
    /// characters.
    pub(crate) query: String,
    /// 1 to 20; 10 when it is missing.
    pub(crate) limit: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessRequestArguments {
    /// The full name of a listed service agent, such as law#0042, from
    /// aether.agents.find.
    pub(crate) agent: String,
    /// The days of access to ask for: 1 to 30; 7 when it is missing.
    pub(crate) days: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeopleFindArguments {
    /// A full name such as ana#0042, or the start of a name: 1 to 64
    /// characters.
    pub(crate) query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupArguments {
    pub(crate) name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupMemberArguments {
    pub(crate) group: String,
    pub(crate) agent: String,
    pub(crate) endpoint_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyUpdateArguments {
    /// Existing absolute folder for new empty task folders.
    pub(crate) workspace: Option<String>,
    pub(crate) max_context_tokens: Option<u32>,
    pub(crate) max_seconds: Option<u64>,
    pub(crate) queue_global: Option<usize>,
    pub(crate) queue_per_peer: Option<usize>,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrlSettingArguments {
    /// An HTTP(S) URL, or "off" to turn this connection off.
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NameArguments {
    pub(crate) handle: String,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InviteNameArguments {
    pub(crate) to: String,
    pub(crate) days: u64,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InviteIdArguments {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeInviteArguments {
    pub(crate) friend_name: String,
    pub(crate) endpoint_id: String,
    pub(crate) days: u64,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeImportArguments {
    pub(crate) code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeArguments {
    pub(crate) grant_id: String,
    #[serde(default)]
    pub(crate) dry_run: bool,
}
