//! The PACT Provider gateway.
//!
//! [PACT 1.0](https://github.com/openpactprotocol/openpactprotocol) lets a
//! personal agent call a Brand's agent over A2A 1.0 HTTP+JSON. Here the node
//! is the Provider, and each configured Brand runs Codex like an Aether task.
//! `plans/pact-provider/PLAN.md` records the mapping and the decisions.

pub mod config;
pub mod consent;
pub mod gateway;
pub mod jwks;
pub mod keys;
pub mod server;
pub mod tokens;
