//! Steps for the limits of one agent turn: remote text and the workspace.

use std::{fs, path::PathBuf};

use aether::execution::{Agent, ExecutionError, Executor, Limits, Task, TurnContext};
use cucumber::{given, then, when};
use tempfile::TempDir;

use super::fixtures::task;
use crate::{common::blocking, steps::World};

#[derive(Debug, Default)]
pub struct State {
    root: Option<TempDir>,
    remote_text: Option<String>,
    workspace_id: Option<String>,
    /// The directory the task asks to run in, inside the workspace.
    requested: Option<PathBuf>,
    result: Option<Result<String, ExecutionError>>,
}

impl State {
    fn root(&self) -> PathBuf {
        self.root.as_ref().expect("a workspace must exist").path().to_owned()
    }
}

/// An agent that answers with `<skill>:<remote text>`.
#[derive(Clone)]
struct EchoAgent;

impl Agent for EchoAgent {
    fn start_turn(&mut self, ctx: &TurnContext) -> Result<String, ExecutionError> {
        Ok(format!("{}:{}", ctx.skill, ctx.text))
    }

    fn cancel(&self, _task_id: &str) {}
}

#[given("a workspace for agent turns")]
async fn workspace(world: &mut World) {
    let root = blocking(|| {
        let root = tempfile::tempdir().expect("temporary directory must create");
        fs::create_dir_all(root.path().join("workspace"))
            .expect("workspace must exist");
        root
    })
    .await;
    world.proof.workspace.root = Some(root);
}

#[given(expr = "a task whose remote text says {string}")]
fn remote_text(world: &mut World, text: String) {
    world.proof.workspace.remote_text = Some(text);
}

#[given(expr = "a task for the workspace {string}")]
fn task_workspace(world: &mut World, workspace_id: String) {
    world.proof.workspace.workspace_id = Some(workspace_id);
}

#[given("a link in the workspace that points outside it")]
async fn outside_link(world: &mut World) {
    let root = world.proof.workspace.root();
    let linked = blocking(move || {
        let outside = root.join("outside");
        fs::create_dir_all(&outside).expect("outside directory must exist");
        let linked = root.join("workspace").join("linked-outside");
        std::os::unix::fs::symlink(&outside, &linked)
            .expect("symbolic link must create");
        linked
    })
    .await;
    world.proof.workspace.requested = Some(linked);
}

#[when("the agent runs the task")]
async fn runs(world: &mut World) {
    let state = &world.proof.workspace;
    let workspace = state.root().join("workspace");
    let run_at = state.requested.clone().unwrap_or_else(|| workspace.clone());
    let base = task("one");
    let task = Task {
        remote_text: state
            .remote_text
            .clone()
            .unwrap_or_else(|| base.remote_text.clone()),
        workspace_id: state
            .workspace_id
            .clone()
            .unwrap_or_else(|| base.workspace_id.clone()),
        ..base
    };
    let result = blocking(move || {
        let mut executor = Executor::new(
            EchoAgent,
            Limits {
                workspace: workspace.clone(),
                max_context_tokens: 32,
                max_seconds: 10,
            },
        );
        executor.set_workspace(&run_at)?;
        executor.execute_with_confirmed_start(&task, &mut || Ok(()), &mut |_| {})
    })
    .await;
    world.proof.workspace.result = Some(result);
}

#[then(expr = "the agent answers {string}")]
fn answers(world: &mut World, answer: String) {
    assert_eq!(world.proof.workspace.result, Some(Ok(answer)));
}

#[then("the node refuses the path")]
fn refuses_path(world: &mut World) {
    assert_eq!(world.proof.workspace.result, Some(Err(ExecutionError::Path)));
}
