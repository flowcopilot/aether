use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::access_groups::{self, Action};
use crate::admin::ChangeRequest;
use crate::hub_client::{AgentKind, HUB_OFF, valid_name_part};

use super::super::McpServer;
use super::super::support::{
    AuditField, audit_item, no_node, tool_error, tool_success,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditArguments {
    pub cursor: Option<usize>,
    pub limit: Option<usize>,
    pub fields: Vec<AuditField>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeArguments {
    pub change: ChangeRequest,
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupArguments {
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupMemberArguments {
    pub group: String,
    pub agent: String,
    pub endpoint_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdateArguments {
    /// Existing absolute folder for new empty task folders.
    pub workspace: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub max_seconds: Option<u64>,
    pub queue_global: Option<usize>,
    pub queue_per_peer: Option<usize>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UrlSettingArguments {
    /// An HTTP(S) URL, or "off" to turn this connection off.
    pub url: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NameArguments {
    pub handle: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeInviteArguments {
    pub friend_name: String,
    pub endpoint_id: String,
    pub days: u64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeImportArguments {
    pub code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeArguments {
    pub grant_id: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[tool_router(router = admin_tool_router, vis = "pub(crate)")]
impl McpServer {
    #[tool(
        name = "aether.settings.get",
        description = "Read the node's saved task policy, relay and hub settings. Task policy changes need a node restart."
    )]
    async fn settings_get(&self) -> CallToolResult {
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        let policy = match crate::policy::policy_settings(data_dir) {
            Ok(policy) => policy.map(|settings| {
                json!({
                    "workspace": settings.workspace,
                    "maxContextTokens": settings.max_context_tokens,
                    "maxSeconds": settings.max_seconds,
                    "queueGlobal": settings.queue.global,
                    "queuePerPeer": settings.queue.per_peer,
                })
            }),
            Err(_) => return tool_error("the node policy is unavailable"),
        };
        tool_success(json!({
            "policy": policy,
            "relayUrl": crate::settings::setting::<crate::transport::relay::Relay>(data_dir).ok().flatten(),
            "hubUrl": self.node_hub(data_dir).resolve_url().ok().flatten(),
            "policyChangesNeedRestart": true,
        }).to_string())
    }

    #[tool(
        name = "aether.settings.update_policy",
        description = "Validate or save task policy limits and the task folder root. Omitted fields keep their values. Saved changes take effect after a node restart."
    )]
    async fn settings_update_policy(
        &self,
        Parameters(arguments): Parameters<PolicyUpdateArguments>,
    ) -> CallToolResult {
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        if arguments.workspace.is_none()
            && arguments.max_context_tokens.is_none()
            && arguments.max_seconds.is_none()
            && arguments.queue_global.is_none()
            && arguments.queue_per_peer.is_none()
        {
            return tool_error("give at least one policy field");
        }
        match crate::policy::policy_settings(data_dir) {
            Ok(Some(_)) => {}
            Ok(None) => return tool_error("start the node once to create its policy"),
            Err(_) => return tool_error("the node policy is unavailable"),
        }
        match crate::admin_socket::send(
            &self.socket,
            &crate::admin_socket::Request::UpdatePolicy {
                workspace: arguments.workspace,
                max_context_tokens: arguments.max_context_tokens,
                max_seconds: arguments.max_seconds,
                queue_global: arguments.queue_global,
                queue_per_peer: arguments.queue_per_peer,
                dry_run: arguments.dry_run,
            },
        ) {
            Ok(crate::admin_socket::Response::Validated) => tool_success(
                json!({
                    "status": "validated",
                    "restartRequired": false,
                })
                .to_string(),
            ),
            Ok(crate::admin_socket::Response::Applied) => tool_success(
                json!({
                    "status": "saved",
                    "restartRequired": true,
                })
                .to_string(),
            ),
            Ok(crate::admin_socket::Response::Error { code }) => {
                tool_error(format!("the node refused the policy change: {code}"))
            }
            _ => tool_error("administration service is unavailable"),
        }
    }

    #[tool(
        name = "aether.settings.set_relay",
        description = "Set the node's relay URL. Use 'off' for local access only. A running node switches relay now; otherwise the saved choice applies on its next start."
    )]
    async fn settings_set_relay(
        &self,
        Parameters(arguments): Parameters<UrlSettingArguments>,
    ) -> CallToolResult {
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        let url = arguments.url.trim();
        let relay = if url.is_empty() || url == crate::transport::relay::RELAY_OFF {
            None
        } else {
            Some(url)
        };
        if relay.is_some_and(|url| !crate::transport::relay::valid_relay_url(url)) {
            return tool_error("use a relay URL that starts with https://, or 'off'");
        }
        if arguments.dry_run {
            return tool_success(json!({"status": "validated"}).to_string());
        }
        match crate::admin_socket::send(
            &self.socket,
            &crate::admin_socket::Request::SetRelay { url: relay.map(str::to_owned) },
        ) {
            Ok(crate::admin_socket::Response::Applied) => tool_success(
                json!({"status": "applied", "restartRequired": false}).to_string(),
            ),
            Ok(crate::admin_socket::Response::Error { code }) => {
                tool_error(format!("the node refused the relay: {code}"))
            }
            // No node runs: it uses the saved choice on its next start.
            Err(error) if no_node(&error) => {
                match crate::settings::store_setting::<crate::transport::relay::Relay>(
                    data_dir,
                    &relay.map(str::to_owned),
                ) {
                    Ok(()) => tool_success(
                        json!({"status": "saved", "restartRequired": true}).to_string(),
                    ),
                    Err(_) => tool_error("the relay setting could not be saved"),
                }
            }
            _ => tool_error("the running node is unavailable"),
        }
    }

    #[tool(
        name = "aether.name.get",
        description = "Read this node's name on the Aether hub."
    )]
    async fn name_get(&self) -> CallToolResult {
        let hub = match self.hub() {
            Ok(hub) => hub,
            Err(error) => return tool_error(error),
        };
        match hub.account().await {
            Ok(account) => tool_success(
                json!({"name": account.map(|account| account.handle)}).to_string(),
            ),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        name = "aether.name.set",
        description = "Claim or change this node's name on the Aether hub. The hub adds a number, as in ana#0042. A first claim keeps the agent type of this node, else makes a personal agent: claim a service agent on the Aether page."
    )]
    async fn name_set(
        &self,
        Parameters(arguments): Parameters<NameArguments>,
    ) -> CallToolResult {
        let handle = arguments.handle.trim();
        if !valid_name_part(handle) {
            return tool_error(
                "use 1 to 32 lowercase letters, digits, dots, dashes or underscores",
            );
        }
        let hub = match self.hub() {
            Ok(hub) => hub,
            Err(error) => return tool_error(error),
        };
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        if arguments.dry_run {
            return tool_success("validated");
        }
        // The hub sets the type once: only the first claim names it.
        let kind = match hub.account().await {
            Ok(None) => Some(
                crate::settings::setting::<crate::hub_client::settings::AgentKind>(
                    data_dir,
                )
                .ok()
                .flatten()
                .unwrap_or(AgentKind::Personal),
            ),
            Ok(Some(_)) => None,
            Err(error) => return tool_error(error.to_string()),
        };
        match hub.claim(handle, kind).await {
            Ok(account) => {
                crate::hub_membership::keep_hub_name(data_dir, &account.handle);
                let _ = crate::hub_client::settings::store_agent_kind(
                    data_dir,
                    account.kind,
                );
                tool_success(json!({"name": account.handle}).to_string())
            }
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        name = "aether.invites.code_create",
        description = "Grant a person direct access and return a one-time connect code for their endpoint. The running node applies the grant at once."
    )]
    async fn invites_code_create(
        &self,
        Parameters(arguments): Parameters<CodeInviteArguments>,
    ) -> CallToolResult {
        if !crate::direct::valid_name(&arguments.friend_name)
            || arguments
                .endpoint_id
                .split_whitespace()
                .collect::<String>()
                .parse::<iroh::EndpointId>()
                .is_err()
            || !crate::direct::valid_direct_invite_days(arguments.days)
        {
            return tool_error(
                "use a valid friend name, endpoint ID and 1, 7 or 30 days",
            );
        }
        if arguments.dry_run {
            return tool_success("validated");
        }
        match crate::admin_socket::send(
            &self.socket,
            &crate::admin_socket::Request::CreateInvite {
                friend_name: arguments.friend_name,
                endpoint_id: arguments.endpoint_id.split_whitespace().collect(),
                days: arguments.days,
            },
        ) {
            Ok(crate::admin_socket::Response::Invitation { line }) => {
                tool_success(json!({"code": line}).to_string())
            }
            Ok(crate::admin_socket::Response::Error { code }) => {
                tool_error(format!("the node refused the invite: {code}"))
            }
            _ => tool_error("the running node is unavailable"),
        }
    }

    #[tool(
        name = "aether.invites.code_import",
        description = "Import a connect code that another person gave this computer. It becomes an agent you can send tasks to."
    )]
    async fn invites_code_import(
        &self,
        Parameters(arguments): Parameters<CodeImportArguments>,
    ) -> CallToolResult {
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        let bundle = match crate::direct::InvitationBundle::decode(&arguments.code) {
            Ok(bundle) => bundle,
            Err(error) => return tool_error(error.to_string()),
        };
        match crate::direct::import_bundle(data_dir, &bundle) {
            Ok(invitation) => {
                tool_success(json!({"agent": invitation.agent_id}).to_string())
            }
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        name = "aether.access.list",
        description = "List people who can send tasks to this running node and their grant IDs."
    )]
    async fn access_list(&self) -> CallToolResult {
        match crate::admin_socket::send(
            &self.socket,
            &crate::admin_socket::Request::DirectGrants,
        ) {
            Ok(crate::admin_socket::Response::DirectGrants { grants }) => {
                tool_success(json!({"grants": grants}).to_string())
            }
            _ => tool_error("the running node is unavailable"),
        }
    }

    #[tool(
        name = "aether.access.revoke",
        description = "Revoke one person's direct access by grant ID. The running node stops accepting new tasks from that grant at once."
    )]
    async fn access_revoke(
        &self,
        Parameters(arguments): Parameters<RevokeArguments>,
    ) -> CallToolResult {
        if arguments.grant_id.is_empty() {
            return tool_error("give a grant ID from aether.access.list");
        }
        if arguments.dry_run {
            return tool_success("validated");
        }
        match crate::admin_socket::send(
            &self.socket,
            &crate::admin_socket::Request::RevokeGrant { id: arguments.grant_id },
        ) {
            Ok(crate::admin_socket::Response::Applied) => tool_success("revoked"),
            Ok(crate::admin_socket::Response::Error { code }) => {
                tool_error(format!("the node refused the revocation: {code}"))
            }
            _ => tool_error("the running node is unavailable"),
        }
    }
    #[tool(
        name = "aether.groups.list",
        description = "List incoming access groups and their members."
    )]
    async fn groups_list(&self) -> CallToolResult {
        let Some(data_dir) = &self.caller else {
            return tool_error("this MCP server has no node data directory");
        };
        match access_groups::list(data_dir) {
            Ok(groups) => tool_success(json!({ "groups": groups.iter().map(|group| json!({
                "name": group.name,
                "members": group.members.iter().map(|member| json!({"agent": member.agent_id, "endpointId": member.endpoint_id})).collect::<Vec<_>>()
            })).collect::<Vec<_>>() }).to_string()),
            Err(error) => tool_error(error),
        }
    }

    #[tool(
        name = "aether.groups.create",
        description = "Create a named incoming access group."
    )]
    async fn groups_create(
        &self,
        Parameters(arguments): Parameters<GroupArguments>,
    ) -> CallToolResult {
        self.group_change(Action::Create, &arguments.name, None, None)
    }

    #[tool(
        name = "aether.groups.add",
        description = "Add a person with active direct access to an incoming access group."
    )]
    async fn groups_add(
        &self,
        Parameters(arguments): Parameters<GroupMemberArguments>,
    ) -> CallToolResult {
        self.group_change(
            Action::Add,
            &arguments.group,
            Some(&arguments.agent),
            arguments.endpoint_id.as_deref(),
        )
    }

    #[tool(
        name = "aether.groups.remove",
        description = "Remove a person from an incoming access group."
    )]
    async fn groups_remove(
        &self,
        Parameters(arguments): Parameters<GroupMemberArguments>,
    ) -> CallToolResult {
        self.group_change(
            Action::Remove,
            &arguments.group,
            Some(&arguments.agent),
            arguments.endpoint_id.as_deref(),
        )
    }

    #[tool(
        name = "aether.groups.delete",
        description = "Delete an incoming access group and its memberships."
    )]
    async fn groups_delete(
        &self,
        Parameters(arguments): Parameters<GroupArguments>,
    ) -> CallToolResult {
        self.group_change(Action::Delete, &arguments.name, None, None)
    }
    #[tool(
        name = "aether.audit.list",
        description = "List redacted administration audit results."
    )]
    async fn audit_list(
        &self,
        Parameters(arguments): Parameters<AuditArguments>,
    ) -> CallToolResult {
        let entries = match self.audit_entries() {
            Ok(entries) => entries,
            Err(message) => return tool_error(message),
        };
        let cursor = arguments.cursor.unwrap_or(0);
        let limit = arguments.limit.unwrap_or(50);
        if !(1..=50).contains(&limit) {
            return tool_error("aether.audit.list limit must be from 1 to 50");
        }
        if arguments.fields.is_empty() || arguments.fields.len() > 3 {
            return tool_error("aether.audit.list requires one to three fields");
        }
        if arguments.fields.iter().enumerate().any(|(index, field)| {
            arguments.fields.iter().take(index).any(|seen| seen == field)
        }) {
            return tool_error("aether.audit.list fields must be unique");
        }
        let items = entries
            .into_iter()
            .skip(cursor)
            .take(limit)
            .map(|entry| audit_item(&arguments.fields, &entry))
            .collect::<Vec<_>>();
        let next_cursor =
            (items.len() == limit).then_some(cursor.saturating_add(items.len()));
        let body = json!({"items": items, "next_cursor": next_cursor});
        match serde_json::to_string(&body) {
            Ok(body) => tool_success(body),
            Err(_) => tool_error("aether.audit.list could not encode its response"),
        }
    }

    #[tool(
        name = "aether.change",
        description = "Validate or apply one typed administration change."
    )]
    async fn change(
        &self,
        Parameters(arguments): Parameters<ChangeArguments>,
    ) -> CallToolResult {
        match self.change_request(arguments.change, arguments.dry_run) {
            Ok(message) => tool_success(message),
            Err(message) => tool_error(message),
        }
    }
    #[tool(
        name = "aether.settings.set_hub",
        description = "Set the name hub URL. Use 'off' to turn name invites off. The owner page and hub workers use the new value on their next request."
    )]
    async fn settings_set_hub(
        &self,
        Parameters(arguments): Parameters<UrlSettingArguments>,
    ) -> CallToolResult {
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        // `off` turns the hub off here, as an empty URL does.
        let url = arguments.url.trim().trim_end_matches('/');
        let stored = if url == HUB_OFF {
            Some(HUB_OFF.to_owned())
        } else if let Ok(stored) = crate::hub_client::hub_setting(url) {
            stored
        } else {
            return tool_error("use an HTTPS hub URL, or 'off'");
        };
        if !arguments.dry_run
            && crate::hub_client::set_hub_url(data_dir, &stored).is_err()
        {
            return tool_error("the hub setting could not be saved");
        }
        tool_success(json!({"status": if arguments.dry_run {"validated"} else {"saved"}, "restartRequired": false}).to_string())
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;

    use aether_store::NodeStore;
    use rmcp::handler::server::wrapper::Parameters;

    use crate::mcp::McpServer;
    use crate::mcp::support::AuditField;

    use super::{
        AuditArguments, ChangeArguments, CodeImportArguments, CodeInviteArguments,
        GroupArguments, NameArguments, PolicyUpdateArguments, RevokeArguments,
        UrlSettingArguments,
    };
    use crate::{
        admin::{
            ChangeRequest, shared_with_store,
            test_support::{FakeNode, shared_in},
        },
        admin_socket::start,
    };

    use super::super::test_support::*;

    #[tokio::test]
    async fn group_tools_use_the_nodes_data_directory() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store =
            aether_store::NodeStore::in_dir(directory.path()).expect("store must open");
        drop(store);
        let server = served(directory.path(), shared_in(directory.path()))
            .with_caller(directory.path().to_path_buf());
        let created = server
            .groups_create(Parameters(GroupArguments { name: "work".to_owned() }))
            .await;
        assert_eq!(created.is_error, Some(false));
        let groups = decode(&server.groups_list().await);
        assert!(
            groups["groups"]
                .as_array()
                .expect("groups must list")
                .iter()
                .any(|group| group["name"] == "work")
        );
        let deleted = server
            .groups_delete(Parameters(GroupArguments { name: "work".to_owned() }))
            .await;
        assert_eq!(deleted.is_error, Some(false));
    }

    #[tokio::test]
    async fn settings_tools_save_the_policy_used_by_the_owner_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("node");
        crate::node_builder::load_or_create_service(
            &directory.path().join("missing-policy.json"),
            &data_dir,
            "codex".into(),
            None,
        )
        .expect("first policy must create");
        let server =
            served(&data_dir, shared_in(&data_dir)).with_caller(data_dir.clone());
        let request = PolicyUpdateArguments {
            workspace: None,
            max_context_tokens: None,
            max_seconds: Some(75),
            queue_global: Some(2),
            queue_per_peer: None,
            dry_run: true,
        };
        let validated = server.settings_update_policy(Parameters(request)).await;
        assert_eq!(validated.is_error, Some(false), "{}", decode_text(&validated));
        assert_eq!(
            crate::policy::policy_settings(&data_dir)
                .expect("policy must read")
                .expect("policy must exist")
                .max_seconds,
            120
        );
        let saved = server
            .settings_update_policy(Parameters(PolicyUpdateArguments {
                workspace: None,
                max_context_tokens: None,
                max_seconds: Some(75),
                queue_global: Some(2),
                queue_per_peer: None,
                dry_run: false,
            }))
            .await;
        assert_eq!(saved.is_error, Some(false));
        assert_eq!(decode(&saved)["restartRequired"], true);
        let settings = crate::policy::policy_settings(&data_dir)
            .expect("policy must read")
            .expect("policy must exist");
        assert_eq!(settings.max_seconds, 75);
        assert_eq!(settings.queue.global, 2);
    }

    #[tokio::test]
    async fn connect_code_tools_grant_import_and_revoke_live_access() {
        let host = tempfile::tempdir().expect("host must create");
        let caller = tempfile::tempdir().expect("caller must create");
        let data_dir = host.path().join("node");
        let service = Arc::new(
            crate::node_builder::load_or_create_service(
                &host.path().join("missing-policy.json"),
                &data_dir,
                "codex".into(),
                None,
            )
            .expect("node must create"),
        );
        crate::settings::store_setting::<crate::direct::HostName>(
            &data_dir,
            &"bob".to_owned(),
        )
        .expect("host name must save");
        crate::settings::store_setting::<crate::transport::relay::ActiveRelay>(
            &data_dir,
            &Some("https://relay.example.com".to_owned()),
        )
        .expect("active relay must save");
        let store = NodeStore::in_dir(&data_dir).expect("store must open");
        let administration = shared_with_store(store);
        administration.lock().expect("administration must lock").set_live(Arc::new(
            crate::node_service::RunningNode::new(
                Arc::clone(&service),
                data_dir.clone(),
            ),
        ));
        let socket = crate::data_dir::DataDir::new(&data_dir).admin_socket();
        start(&socket, administration.clone()).expect("socket must start");
        let server = McpServer::socket(socket).with_caller(data_dir.clone());
        let endpoint = crate::direct::endpoint_id(caller.path())
            .expect("caller endpoint must create");
        let result = server
            .invites_code_create(Parameters(CodeInviteArguments {
                friend_name: "alice".to_owned(),
                endpoint_id: endpoint.clone(),
                days: 7,
                dry_run: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(false), "{}", decode_text(&result));
        // As with an invite from the page, the new person joins friends: the
        // node admits a direct task only from a member of a group.
        assert!(
            NodeStore::in_dir(&data_dir)
                .expect("store must open")
                .access_group_allows("alice", &endpoint)
                .expect("groups must read"),
            "the invited person must join a group"
        );
        let code =
            decode(&result)["code"].as_str().expect("code must be returned").to_owned();
        // The code holds the card, and the grant is for the general skill.
        let bundle =
            crate::direct::InvitationBundle::decode(&code).expect("code must decode");
        assert_eq!(bundle.format, crate::direct::BUNDLE_FORMAT_V2);
        let grants = administration
            .lock()
            .expect("administration must lock")
            .grants()
            .expect("grants must load");
        assert_eq!(grants.len(), 1);
        assert!(grants.iter().all(|grant| {
            grant
                .capabilities
                .iter()
                .map(aether_core::CapabilityId::as_str)
                .eq([crate::execution::GENERAL_SKILL])
        }));
        let access = decode(&server.access_list().await);
        let grant_id = access["grants"][0]["id"]
            .as_str()
            .expect("grant ID must be listed")
            .to_owned();
        assert_eq!(access["grants"][0]["agent"], "alice");
        let caller_server = served(caller.path(), shared_in(caller.path()))
            .with_caller(caller.path().to_path_buf());
        let imported = caller_server
            .invites_code_import(Parameters(CodeImportArguments { code }))
            .await;
        assert_eq!(decode(&imported)["agent"], "bob");
        let revoked = server
            .access_revoke(Parameters(RevokeArguments { grant_id, dry_run: false }))
            .await;
        assert_eq!(revoked.is_error, Some(false));
        assert!(
            decode(&server.access_list().await)["grants"]
                .as_array()
                .expect("grants must list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn name_tools_keep_the_hub_name_that_invites_carry() {
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let directory = tempfile::tempdir().expect("directory must create");
        crate::settings::store_setting::<crate::hub_client::settings::Hub>(
            directory.path(),
            &Some(hub.url.clone()),
        )
        .expect("the hub must store");
        let server = served(directory.path(), shared_in(directory.path()))
            .with_caller(directory.path().to_path_buf());
        let claimed = server
            .name_set(Parameters(NameArguments {
                handle: "ana".to_owned(),
                dry_run: false,
            }))
            .await;
        assert_eq!(claimed.is_error, Some(false), "{}", decode_text(&claimed));
        let name =
            decode(&claimed)["name"].as_str().expect("name must return").to_owned();
        assert!(name.starts_with("ana#"));
        assert_eq!(
            crate::settings::setting::<crate::hub_client::settings::HubName>(
                directory.path()
            )
            .ok()
            .flatten(),
            Some(name)
        );
        assert_eq!(
            crate::settings::setting::<crate::hub_client::settings::AgentKind>(
                directory.path()
            )
            .ok()
            .flatten(),
            Some(crate::hub_client::AgentKind::Personal)
        );
        let changed = server
            .settings_set_hub(Parameters(UrlSettingArguments {
                url: "off".to_owned(),
                dry_run: false,
            }))
            .await;
        assert_eq!(changed.is_error, Some(false));
        assert_eq!(
            crate::settings::setting::<crate::hub_client::settings::HubName>(
                directory.path()
            )
            .ok()
            .flatten(),
            None
        );
    }

    #[tokio::test]
    async fn relay_setting_reaches_the_running_node() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().to_path_buf();
        let administration =
            shared_with_store(NodeStore::in_dir(&data_dir).expect("store must open"));
        let node = Arc::new(FakeNode::default());
        administration.lock().expect("administration must lock").set_live(node.clone());
        let socket = crate::data_dir::DataDir::new(&data_dir).admin_socket();
        start(&socket, administration).expect("socket must start");
        let server = McpServer::socket(socket).with_caller(data_dir.clone());
        let result = server
            .settings_set_relay(Parameters(UrlSettingArguments {
                url: "off".to_owned(),
                dry_run: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(false), "{}", decode_text(&result));
        assert_eq!(decode(&result)["restartRequired"], false);
        assert_eq!(node.calls(), ["relay off"]);
    }

    #[tokio::test]
    async fn socket_change_uses_the_ui_administration_state() {
        let directory = tempfile::tempdir().expect("directory must create");
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        let administration = shared_in(directory.path());
        start(&socket, administration.clone()).expect("socket must start");
        let response = McpServer::socket(socket.clone())
            .change(Parameters(ChangeArguments {
                change: ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                },
                dry_run: false,
            }))
            .await;
        assert_eq!(response.is_error, Some(false));
        administration
            .lock()
            .expect("administration must unlock")
            .confirm_ui("workspace")
            .expect("ui must confirm");
    }

    #[tokio::test]
    async fn a_signed_grant_and_workspace_change_apply_live_through_mcp_and_the_socket()
    {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        let store = NodeStore::open(&database).expect("store must open");
        let administration = shared_with_store(store);
        let node = Arc::new(FakeNode::default());
        administration
            .lock()
            .expect("administration must unlock")
            .set_live(node.clone());
        start(&socket, administration.clone()).expect("socket must start");

        // MCP submits the workspace change through the live administration
        // socket, exactly as a caller connecting over MCP would. It is
        // high-risk, so it lands pending, not applied — an MCP caller
        // cannot lower a change's risk class by confirming its own request.
        let workspace_response = McpServer::socket(socket.clone())
            .change(Parameters(ChangeArguments {
                change: ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                },
                dry_run: false,
            }))
            .await;
        assert_eq!(workspace_response.is_error, Some(false));
        assert!(node.calls().is_empty());

        // Confirmation is a local-UI action, not an MCP one — simulate it
        // the same way `admin_ui.rs`'s confirmed form does, directly on the
        // shared administration state the socket also serves.
        administration
            .lock()
            .expect("administration must unlock")
            .confirm_ui("workspace")
            .expect("workspace must confirm");
        assert_eq!(node.calls(), ["workspace workspace project"]);

        // Queuing and confirming the grant change only after the workspace
        // change fully applied (rather than queuing both up front) matters:
        let grant_response = McpServer::socket(socket.clone())
            .change(Parameters(ChangeArguments {
                change: {
                    let grant = crate::admin::peer_grant();
                    ChangeRequest::Grant { id: grant.id.as_str().to_owned(), grant }
                },
                dry_run: false,
            }))
            .await;
        assert_eq!(grant_response.is_error, Some(false));
        assert_eq!(node.calls(), ["workspace workspace project"]);
        administration
            .lock()
            .expect("administration must unlock")
            .confirm_ui("peer")
            .expect("grant must confirm");
        assert_eq!(node.calls(), ["workspace workspace project", "grant peer"]);
        let grant_text = node.granted().into_iter().next().expect("grant must apply");
        crate::identity::decode_grant(&grant_text).expect("grant must decode");

        // Both changes must have left durable state behind, readable through
        // an independent connection to the same database — the same
        // guarantee an actual restart depends on, without needing to stop the
        // socket's own background listener thread to prove it.
        let restarted = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(restarted.current_workspace(), Ok(Some("project".to_owned())));
        assert_eq!(
            restarted.applied_change(aether_store::ChangeKind::Grant, "peer"),
            Ok(Some(grant_text))
        );
    }

    #[tokio::test]
    async fn high_risk_change_needs_ui_confirmation() {
        let directory = tempfile::tempdir().expect("directory must create");
        let response = served(directory.path(), shared_in(directory.path()))
            .change(Parameters(ChangeArguments {
                change: ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                },
                dry_run: false,
            }))
            .await;
        assert_eq!(response.is_error, Some(false));
        assert_eq!(
            response.content[0].as_text().map(|text| &text.text),
            Some(&"pending local confirmation".to_owned())
        );
    }

    #[tokio::test]
    async fn audit_requires_selected_unique_fields() {
        let directory = tempfile::tempdir().expect("directory must create");
        let response = served(directory.path(), shared_in(directory.path()))
            .audit_list(Parameters(AuditArguments {
                cursor: None,
                limit: None,
                fields: vec![AuditField::Result, AuditField::Result],
            }))
            .await;
        assert_eq!(response.is_error, Some(true));
    }

    #[test]
    fn generated_change_schema_is_strict_and_typed() {
        let tool = McpServer::change_tool_attr();
        assert_eq!(tool.name, "aether.change");
        assert_eq!(tool.input_schema.get("additionalProperties"), Some(&false.into()));
        assert!(tool.input_schema.get("properties").is_some());
        let variants = tool.input_schema["$defs"]["ChangeRequest"]["oneOf"]
            .as_array()
            .expect("each change type must be in the schema");
        let types: Vec<&str> = variants
            .iter()
            .map(|variant| {
                assert_eq!(variant["additionalProperties"], false);
                variant["properties"]["type"]["const"].as_str().expect("type tag")
            })
            .collect();
        assert_eq!(types, ["workspace", "grant", "directory", "capability", "limit"]);
        let tool = McpServer::cancel_tool_attr();
        assert_eq!(tool.name, "aether.task.cancel");
        assert_eq!(tool.input_schema.get("additionalProperties"), Some(&false.into()));
    }

    #[tokio::test]
    async fn audit_pages_and_redacts_its_results() {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        {
            // One queued and confirmed change writes three audit records:
            // pending, confirmed and applied.
            let mut locked = administration.lock().expect("administration must lock");
            let grant = crate::admin::peer_grant();
            let id = grant.id.as_str().to_owned();
            locked
                .submit(
                    &ChangeRequest::Grant { id: id.clone(), grant }
                        .into_change()
                        .expect("change must parse"),
                    aether_store::AuditActor::Mcp,
                    false,
                )
                .expect("change must queue");
            locked.confirm_ui(&id).expect("change must confirm");
        }
        let server = served(directory.path(), administration);
        let first = server
            .audit_list(Parameters(AuditArguments {
                cursor: None,
                limit: Some(2),
                fields: vec![AuditField::Actor, AuditField::Result],
            }))
            .await;
        assert_eq!(first.is_error, Some(false));
        let first = decode(&first);
        assert_eq!(first["items"].as_array().map(Vec::len), Some(2));
        assert_eq!(first["next_cursor"], serde_json::json!(2));
        assert_eq!(first["items"][0]["actor"], "mcp");
        assert!(first["items"][0].get("action").is_none());
        let second = server
            .audit_list(Parameters(AuditArguments {
                cursor: first["next_cursor"]
                    .as_u64()
                    .map(|cursor| usize::try_from(cursor).expect("a cursor must fit")),
                limit: Some(2),
                fields: vec![AuditField::Result],
            }))
            .await;
        let second = decode(&second);
        assert_eq!(second["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(second["next_cursor"], serde_json::Value::Null);
        assert!(!first.to_string().contains("private-grant-token"));
        assert!(!second.to_string().contains("private-grant-token"));
    }

    #[tokio::test]
    async fn audit_rejects_an_out_of_range_limit() {
        let directory = tempfile::tempdir().expect("directory must create");
        let response = served(directory.path(), shared_in(directory.path()))
            .audit_list(Parameters(AuditArguments {
                cursor: None,
                limit: Some(51),
                fields: vec![AuditField::Result],
            }))
            .await;
        assert_eq!(response.is_error, Some(true));
    }
}
