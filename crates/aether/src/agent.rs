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

use std::{collections::VecDeque, io::Write, sync::Mutex};

use serde::Serialize;

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
}

/// Writes `message` to `writer` as one line of JSON.
fn write_json_line<W: Write>(
    writer: &Mutex<W>,
    message: &impl Serialize,
) -> Result<(), ExecutionError> {
    let mut line = serde_json::to_vec(message).map_err(|_| ExecutionError::Protocol)?;
    line.push(b'\n');
    let mut writer = writer.lock().map_err(|_| ExecutionError::Unavailable)?;
    writer.write_all(&line).map_err(|_| ExecutionError::Unavailable)?;
    writer.flush().map_err(|_| ExecutionError::Unavailable)
}

/// How many task IDs `EarlyCancels` keeps in each of its lists.
const REMEMBERED_TASKS: usize = 256;

/// The cancels that reach an adapter for a task it is not running.
///
/// The node cancels a task without knowing whether its turn has started,
/// is running or has ended. A cancel that comes before the turn starts
/// cancels it at the start; a cancel for a turn that already ended is
/// dropped. Both lists keep only the latest `REMEMBERED_TASKS` IDs, so a
/// cancel for a task that never starts cannot hold memory for ever.
#[derive(Default)]
struct EarlyCancels {
    cancelled: VecDeque<String>,
    ended: VecDeque<String>,
}

impl EarlyCancels {
    /// Keeps a cancel for `task_id`, whose turn is not running, unless the
    /// turn already ended.
    fn cancel(&mut self, task_id: &str) {
        if !self.ended.iter().any(|id| id == task_id)
            && !self.cancelled.iter().any(|id| id == task_id)
        {
            remember(&mut self.cancelled, task_id);
        }
    }

    /// Whether `task_id` was cancelled before its turn started. The cancel
    /// is used up.
    fn take(&mut self, task_id: &str) -> bool {
        self.cancelled
            .iter()
            .position(|id| id == task_id)
            .and_then(|index| self.cancelled.remove(index))
            .is_some()
    }

    /// Notes that the turn of `task_id` ended, so that a later cancel for it
    /// is dropped.
    fn end(&mut self, task_id: &str) {
        remember(&mut self.ended, task_id);
    }
}

fn remember(ids: &mut VecDeque<String>, task_id: &str) {
    if ids.len() >= REMEMBERED_TASKS {
        ids.pop_front();
    }
    ids.push_back(task_id.to_owned());
}

#[cfg(test)]
mod tests {
    use super::{EarlyCancels, REMEMBERED_TASKS};

    #[test]
    fn a_cancel_before_the_start_is_taken_once() {
        let mut cancels = EarlyCancels::default();
        cancels.cancel("task");
        assert!(cancels.take("task"));
        assert!(!cancels.take("task"));
    }

    #[test]
    fn a_cancel_after_the_end_is_dropped() {
        let mut cancels = EarlyCancels::default();
        cancels.end("task");
        cancels.cancel("task");
        assert!(!cancels.take("task"));
    }

    #[test]
    fn only_the_latest_cancels_are_kept() {
        let mut cancels = EarlyCancels::default();
        for index in 0..=REMEMBERED_TASKS {
            cancels.cancel(&format!("task-{index}"));
        }
        assert_eq!(cancels.cancelled.len(), REMEMBERED_TASKS);
        assert!(!cancels.take("task-0"));
        assert!(cancels.take(&format!("task-{REMEMBERED_TASKS}")));
        for index in 0..=REMEMBERED_TASKS {
            cancels.end(&format!("task-{index}"));
        }
        assert_eq!(cancels.ended.len(), REMEMBERED_TASKS);
    }
}
