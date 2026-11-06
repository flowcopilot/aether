//! The product's behaviour, as Gherkin scenarios.
//!
//! Each `.feature` file in `features/node/` at the repository root describes
//! one product feature. The step definitions live in `steps/`. Select
//! scenarios with `cargo test -p aether --test features -- --name "<regex>"`
//! or `-- --tags "@<tag>"`. Scenarios tagged `@unix` run only on unix.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::panic,
    clippy::unreachable,
    clippy::large_futures,
    clippy::needless_pass_by_value,
    clippy::needless_pass_by_ref_mut,
    clippy::too_many_arguments,
    reason = "test code: a panic is a failed test, the values are small constants, and cucumber passes captures by value and the world by `&mut`"
)]

mod common;
mod steps;

use std::path::PathBuf;

use cucumber::World as _;
use steps::World;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let features = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../features/node")
        .canonicalize()
        .expect("features/node must exist");
    World::cucumber()
        // A step without a definition is a missing test, not a pass.
        .fail_on_skipped()
        .filter_run_and_exit(features, |_, _, scenario| {
            cfg!(unix) || !scenario.tags.iter().any(|tag| tag == "unix")
        })
        .await;
}
