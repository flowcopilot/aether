//! Steps for `cli-help.feature`: the help pages and the machine-readable spec.
//!
//! The pages are insta snapshots in `snapshots/` next to this file.

use cucumber::{then, when};

use super::assert_success;
use crate::common::{aether_command, run};

#[when(expr = "I ask for help on `aether node {word}`")]
async fn ask_for_help(world: &mut crate::steps::World, command: String) {
    let mut aether = aether_command();
    // Help wraps to the terminal width; fix it so the page does not depend on
    // the terminal that runs the tests.
    aether.args(["node", &command, "--help"]).env("COLUMNS", "100");
    world.cli.output = Some(run(aether).await);
}

#[when("I ask for the machine-readable spec")]
async fn ask_for_spec(world: &mut crate::steps::World) {
    let mut aether = aether_command();
    aether.arg("__usage_spec__");
    world.cli.output = Some(run(aether).await);
}

#[then(expr = "the help matches the reviewed {string} page")]
fn help_matches(world: &mut crate::steps::World, page: String) {
    assert_success(world.cli.output());
    insta::assert_snapshot!(page, world.cli.stdout());
}

#[then(expr = "the spec matches the reviewed {string} page")]
fn spec_matches(world: &mut crate::steps::World, page: String) {
    assert_success(world.cli.output());
    // The version changes with every release and is not part of the shape.
    let spec = world.cli.stdout().replace(env!("CARGO_PKG_VERSION"), "[version]");
    insta::assert_snapshot!(page, spec);
}
