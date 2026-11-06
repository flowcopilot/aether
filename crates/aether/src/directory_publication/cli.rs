#![allow(clippy::must_use_candidate)]

use std::{path::Path, process::ExitCode};

pub struct RecordInput<'a> {
    pub data_dir: &'a Path,
    pub agent_id: String,
    pub address: String,
    pub scope: String,
    pub capability: String,
    pub expires_in: u64,
}

pub fn run_record(input: RecordInput<'_>) -> ExitCode {
    if !super::valid_publication_fields(
        &input.agent_id,
        &input.address,
        &input.scope,
        &input.capability,
        input.expires_in,
        &input.scope,
    ) {
        eprintln!("aether: node record is invalid");
        return ExitCode::from(2);
    }
    let Ok(endpoint) = crate::data_dir::DataDir::new(input.data_dir).endpoint_key()
    else {
        eprintln!("aether: node record is unavailable");
        return ExitCode::from(1);
    };
    let Ok(wire) = super::signed_node_record(
        input.data_dir,
        &super::PublicationRecord {
            agent_id: input.agent_id,
            address: input.address,
            scope: input.scope,
            capability: input.capability,
            ttl: input.expires_in,
        },
        &endpoint.public().to_string(),
    ) else {
        eprintln!("aether: node record is unavailable");
        return ExitCode::from(1);
    };
    if let Ok(wire) = serde_json::to_string(&wire) {
        println!("{wire}");
        ExitCode::SUCCESS
    } else {
        eprintln!("aether: node record cannot be encoded");
        ExitCode::from(1)
    }
}
