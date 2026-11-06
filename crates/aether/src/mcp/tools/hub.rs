use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::hub_client::valid_full_name;

use super::super::support::{tool_error, tool_success};
use super::super::{FIND_WAIT, McpServer, find_error};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessRequestArguments {
    /// The full name of a listed service agent, such as law#0042, from
    /// aether.agents.find.
    pub agent: String,
    /// The days of access to ask for: 1 to 30; 7 when it is missing.
    pub days: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InviteNameArguments {
    pub to: String,
    pub days: u64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InviteIdArguments {
    pub id: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[tool_router(router = hub_tool_router, vis = "pub(crate)")]
impl McpServer {
    #[tool(
        name = "aether.invites.list",
        description = "List this node's open hub invites by ID, sender, recipient and status. Use an ID to accept or remove an invite."
    )]
    async fn invites_list(&self) -> CallToolResult {
        let hub = match self.hub() {
            Ok(hub) => hub,
            Err(error) => return tool_error(error),
        };
        match hub.invites(None, std::time::Duration::ZERO).await {
            Ok(list) => tool_success(
                json!({"invites": list.invites.iter().map(|invite| json!({
                "id": invite.id, "from": invite.from, "to": invite.to,
                "role": invite.role, "days": invite.days, "status": invite.status,
            })).collect::<Vec<_>>()})
                .to_string(),
            ),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        name = "aether.invites.offer",
        description = "Offer this node's agent to a person by full Aether name, such as bob#0042, for 1, 7 or 30 days. The person accepts on their node."
    )]
    async fn invites_offer(
        &self,
        Parameters(arguments): Parameters<InviteNameArguments>,
    ) -> CallToolResult {
        let to = arguments.to.trim();
        if !valid_full_name(to)
            || !crate::hub_client::protocol::OFFER_DAYS.contains(&arguments.days)
        {
            return tool_error(
                "use a full Aether name with its number, such as bob#0042, and 1, 7 or 30 days",
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
        match crate::hub_membership::offer(&hub, data_dir, to, arguments.days).await {
            Ok(invite) => tool_success(
                json!({"id": invite.id, "status": invite.status}).to_string(),
            ),
            Err(error) => tool_error(error),
        }
    }

    #[tool(
        name = "aether.invites.accept",
        description = "Accept one hub invite by ID. This node's worker will then import the access code."
    )]
    async fn invites_accept(
        &self,
        Parameters(arguments): Parameters<InviteIdArguments>,
    ) -> CallToolResult {
        let hub = match self.hub() {
            Ok(hub) => hub,
            Err(error) => return tool_error(error),
        };
        if arguments.id.is_empty() {
            return tool_error("give an invite ID");
        }
        if arguments.dry_run {
            return tool_success("validated");
        }
        let Ok(data_dir) = self.data_dir() else {
            return tool_error("this MCP server has no node data directory");
        };
        match crate::hub_membership::accept(&hub, data_dir, &arguments.id).await {
            Ok(invite) => tool_success(
                json!({"id": invite.id, "status": invite.status}).to_string(),
            ),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        name = "aether.invites.remove",
        description = "Decline or cancel one hub invite by ID."
    )]
    async fn invites_remove(
        &self,
        Parameters(arguments): Parameters<InviteIdArguments>,
    ) -> CallToolResult {
        let hub = match self.hub() {
            Ok(hub) => hub,
            Err(error) => return tool_error(error),
        };
        if arguments.id.is_empty() {
            return tool_error("give an invite ID");
        }
        if arguments.dry_run {
            return tool_success("validated");
        }
        match hub.remove(&arguments.id).await {
            Ok(()) => tool_success("removed"),
            Err(error) => tool_error(error.to_string()),
        }
    }
    #[tool(
        name = "aether.access.request",
        description = "Prepare a request for access to a listed service agent from aether.agents.find. It sends nothing: the user must open the Aether page on this computer and confirm the request there. It returns a request ID and status needs_confirmation. After the owner of the agent approves, aether.agents.find shows access ready."
    )]
    async fn access_request(
        &self,
        Parameters(arguments): Parameters<AccessRequestArguments>,
    ) -> CallToolResult {
        let Some(data_dir) = &self.caller else {
            return tool_error("this MCP server has no caller data directory");
        };
        let agent = arguments.agent.trim();
        if !crate::hub_client::valid_full_name(agent) {
            return tool_error(
                "aether.access.request agent must be a full name such as law#0042",
            );
        }
        let days = arguments.days.unwrap_or(7);
        if !(crate::hub_client::protocol::ACCESS_DAYS_MIN
            ..=crate::hub_client::protocol::ACCESS_DAYS_MAX)
            .contains(&days)
        {
            return tool_error("aether.access.request days must be from 1 to 30");
        }
        let hub = match self.caller_hub(data_dir) {
            Ok(Some(hub)) => hub,
            Ok(None) => {
                return tool_error("no hub is set, so there are no access requests");
            }
            Err(error) => return tool_error(error),
        };
        let row = match tokio::time::timeout(FIND_WAIT, hub.agent(agent)).await {
            Ok(Ok(row)) => row,
            Ok(Err(crate::hub_client::HubError::NotFound)) => {
                return tool_error("no listed service agent has this name");
            }
            Ok(Err(error)) => return tool_error(find_error(error)),
            Err(_) => {
                return tool_error(find_error(
                    crate::hub_client::HubError::Unreachable,
                ));
            }
        };
        match crate::access_requests::draft(data_dir, agent, &row, days) {
            Ok(draft) => tool_success(
                json!({
                    "requestId": draft.id,
                    "agent": draft.target_name,
                    "status": "needs_confirmation",
                    "message": "Ask the user to open the Aether page on this computer and confirm the request.",
                })
                .to_string(),
            ),
            Err(error) => tool_error(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {

    use aether_store::NodeStore;
    use rmcp::handler::server::wrapper::Parameters;

    use super::{AccessRequestArguments, InviteIdArguments, InviteNameArguments};

    use crate::admin::test_support::shared_in;

    use super::super::test_support::*;

    #[tokio::test]
    async fn access_request_creates_a_pending_item_and_sends_nothing_to_the_hub() {
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let caller = hub_caller(&hub);
        let lawyer = tempfile::tempdir().expect("directory must create");
        list_on_hub(&hub, lawyer.path(), "lawyer#0042", "Reviews contracts");
        let server = served(caller.path(), shared_in(caller.path()))
            .with_caller(caller.path().to_path_buf());

        let result = server.access_request(access_arguments("lawyer#0042")).await;
        assert_eq!(result.is_error, Some(false), "{}", decode_text(&result));
        let answer = decode(&result);
        assert_eq!(answer["agent"], "lawyer#0042");
        assert_eq!(answer["status"], "needs_confirmation");
        assert!(
            answer["message"].as_str().is_some_and(|text| text.contains("confirm"))
        );
        let drafts = NodeStore::in_dir(caller.path())
            .expect("store")
            .access_request_drafts(0)
            .expect("drafts");
        assert_eq!(drafts.len(), 1);
        assert_eq!(answer["requestId"], drafts[0].id);
        assert_eq!(drafts[0].days, 7);
        assert!(!hub.requests().iter().any(|request| request.target == "/v1/requests"));
        assert!(hub.state().invites.is_empty());
    }

    #[tokio::test]
    async fn access_request_refuses_an_agent_that_is_not_a_listed_service_agent() {
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let caller = hub_caller(&hub);
        {
            let mut state = hub.state();
            state.add_account(&"a".repeat(64), "ana#0042");
            state.add_account(&"b".repeat(64), "quiet#0001");
            state.kinds.insert("b".repeat(64), crate::hub_client::AgentKind::Service);
        }
        let server = served(caller.path(), shared_in(caller.path()))
            .with_caller(caller.path().to_path_buf());
        for (agent, message) in [
            ("ana#0042", "no listed service agent has this name"),
            ("quiet#0001", "no listed service agent has this name"),
            ("nobody#0001", "no listed service agent has this name"),
            (
                "lawyer",
                "aether.access.request agent must be a full name such as law#0042",
            ),
        ] {
            let result = server.access_request(access_arguments(agent)).await;
            assert_eq!(result.is_error, Some(true), "{agent}");
            assert_eq!(decode_text(&result), message, "{agent}");
        }
        let days = server
            .access_request(Parameters(AccessRequestArguments {
                agent: "ana#0042".to_owned(),
                days: Some(31),
            }))
            .await;
        assert_eq!(
            decode_text(&days),
            "aether.access.request days must be from 1 to 30"
        );
        assert!(
            NodeStore::in_dir(caller.path())
                .expect("store")
                .access_request_drafts(0)
                .expect("drafts")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn access_request_refuses_a_contact_a_second_request_and_more_than_five() {
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let caller = hub_caller(&hub);
        let lawyer = tempfile::tempdir().expect("directory must create");
        list_on_hub(&hub, lawyer.path(), "lawyer#0042", "Reviews contracts");
        import_named(caller.path(), lawyer.path(), "lawyer#0042");
        let server = served(caller.path(), shared_in(caller.path()))
            .with_caller(caller.path().to_path_buf());
        let contact = server.access_request(access_arguments("lawyer#0042")).await;
        assert_eq!(decode_text(&contact), "this agent is a contact already");

        let hosts: Vec<tempfile::TempDir> = (0..6)
            .map(|_| tempfile::tempdir().expect("directory must create"))
            .collect();
        for (index, host) in hosts.iter().enumerate() {
            list_on_hub(
                &hub,
                host.path(),
                &format!("service{index}#0001"),
                "Reviews tax",
            );
        }
        let first = server.access_request(access_arguments("service0#0001")).await;
        assert_eq!(first.is_error, Some(false), "{}", decode_text(&first));
        let second = server.access_request(access_arguments("service0#0001")).await;
        assert_eq!(
            decode_text(&second),
            crate::access_requests::DraftError::Duplicate.to_string()
        );
        for index in 1..5 {
            let result = server
                .access_request(access_arguments(&format!("service{index}#0001")))
                .await;
            assert_eq!(result.is_error, Some(false), "{}", decode_text(&result));
        }
        let sixth = server.access_request(access_arguments("service5#0001")).await;
        assert_eq!(
            decode_text(&sixth),
            crate::access_requests::DraftError::Limit.to_string()
        );
    }

    #[tokio::test]
    async fn hub_invite_tools_keep_the_local_approvals_that_the_worker_needs() {
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let host = hub_caller(&hub);
        let friend = tempfile::tempdir().expect("directory must create");
        crate::settings::store_setting::<crate::hub_client::settings::Hub>(
            friend.path(),
            &Some(hub.url.clone()),
        )
        .expect("the hub must store");
        let friend_endpoint =
            crate::direct::endpoint_id(friend.path()).expect("endpoint");
        hub.state().add_account(&friend_endpoint, "bob#0042");
        let host_server = served(host.path(), shared_in(host.path()))
            .with_caller(host.path().to_path_buf());
        let offer = |to: &str| InviteNameArguments {
            to: to.to_owned(),
            days: 7,
            dry_run: false,
        };
        let bare = host_server.invites_offer(Parameters(offer("bob"))).await;
        assert_eq!(bare.is_error, Some(true));
        let offered = host_server.invites_offer(Parameters(offer("bob#0042"))).await;
        assert_eq!(offered.is_error, Some(false), "{}", decode_text(&offered));
        let id =
            decode(&offered)["id"].as_str().expect("invite must have an id").to_owned();
        let approval = |data_dir: &std::path::Path| {
            NodeStore::in_dir(data_dir)
                .expect("store must open")
                .hub_approval(&id)
                .expect("approval must read")
                .expect("approval must exist")
        };
        let sent = approval(host.path());
        assert_eq!(sent.side, aether_store::ApprovalSide::Sender);
        assert_eq!((sent.peer_name.as_str(), sent.days), ("bob#0042", 7));

        let friend_server = served(friend.path(), shared_in(friend.path()))
            .with_caller(friend.path().to_path_buf());
        let accepted = friend_server
            .invites_accept(Parameters(InviteIdArguments {
                id: id.clone(),
                dry_run: false,
            }))
            .await;
        assert_eq!(accepted.is_error, Some(false), "{}", decode_text(&accepted));
        assert_eq!(decode(&accepted)["status"], "accepted");
        let kept = approval(friend.path());
        assert_eq!(kept.side, aether_store::ApprovalSide::Receiver);
        assert_eq!(kept.peer_name, "me#0001");
    }
}
