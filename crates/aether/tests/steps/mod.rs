//! Step definitions for `features/node/*.feature`.
//!
//! The steps share one [`World`] for each scenario. Each area of the product
//! keeps its own state in its own module, so steps for one area do not see
//! the fields of another.
//!
//! Cucumber polls the scenarios of a run on the same task. A step that blocks
//! (a process wait, a `std::thread::sleep`, a blocking store call) must run in
//! [`tokio::task::spawn_blocking`] or it stops every other scenario. Tag a
//! scenario `@serial` when it must not run beside the others.

pub mod cli;
pub mod pact;
pub mod proof;
pub mod tasks;

/// The state of one scenario.
#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// One-shot `aether` commands and the files they read and write.
    pub cli: cli::State,
    /// The PACT gateway and the personal agent that uses it.
    pub pact: pact::State,
    /// A proof node in this process, with fake agents.
    pub proof: proof::State,
    /// Signed tasks sent to a running `node serve`.
    pub tasks: tasks::State,
}
