//! A proof node in this process: admission, scopes, execution and cancellation.

mod fixtures;
mod node;
mod queue;
mod scopes;
mod transport;
mod workspace;

#[derive(Debug, Default)]
pub struct State {
    /// How a caller is known in a scope, and what its grant admits.
    scopes: scopes::State,
    /// A proof node with a fake agent.
    node: node::State,
    /// The node's task queue on its own.
    queue: queue::State,
    /// One agent turn in a workspace.
    workspace: workspace::State,
    /// Two nodes over Iroh.
    transport: transport::State,
}
