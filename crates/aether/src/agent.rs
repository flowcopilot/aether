//! The agents that can answer a node's tasks. Each one is an adapter behind
//! the `execution::Agent` seam:
//!
//! - `codex`: the node starts `codex app-server` for each task;
//! - `connected`: an agent that runs on its own attaches to the node's agent
//!   socket (the Hermes plugin in `integrations/hermes-plugin` is one).
//!
//! The caller never learns which adapter answers: the Agent Card, hub
//! listings and task states are the same for all of them.

pub mod codex;
pub mod connected;

use std::path::Path;

use crate::{
    execution::{Agent, ExecutionError, ExecutionEvent, TurnContext},
    policy::AgentAdapter,
};

/// The agent of one node, chosen in the policy file.
#[derive(Clone)]
pub enum NodeAgent {
    Codex(codex::CodexAppServer),
    Connected(connected::ConnectedAgent),
}

impl NodeAgent {
    #[must_use]
    pub const fn kind(&self) -> AgentAdapter {
        match self {
            Self::Codex(_) => AgentAdapter::Codex,
            Self::Connected(_) => AgentAdapter::Connected,
        }
    }
}

impl Agent for NodeAgent {
    fn start_turn_with_events(
        &mut self,
        ctx: &TurnContext,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        match self {
            Self::Codex(agent) => agent.start_turn_with_events(ctx, events),
            Self::Connected(agent) => agent.start_turn_with_events(ctx, events),
        }
    }

    fn cancel(&self, task_id: &str) {
        match self {
            Self::Codex(agent) => agent.cancel(task_id),
            Self::Connected(agent) => agent.cancel(task_id),
        }
    }

    fn set_workspace(&self, workspace: &Path) {
        match self {
            Self::Codex(agent) => agent.set_workspace(workspace),
            Self::Connected(agent) => agent.set_workspace(workspace),
        }
    }

    fn supports_workspace_change(&self) -> bool {
        match self {
            Self::Codex(agent) => agent.supports_workspace_change(),
            Self::Connected(agent) => agent.supports_workspace_change(),
        }
    }
}
