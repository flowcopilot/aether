#![allow(clippy::must_use_candidate)]

use std::{path::PathBuf, process::ExitCode};

use serde::Serialize;
use usage_rs::Subcommands;

use super::{
    DirectoryClient, read_directory_record, read_verifying_key, sync_directory,
};

#[derive(Subcommands)]
pub enum Command {
    Publish {
        record: PathBuf,
    },
    Renew {
        record: PathBuf,
    },
    Resolve {
        agent_id: String,
    },
    Search {
        capability: String,
    },
    Revoke {
        record: PathBuf,
    },
    Sync {
        #[usage(long)]
        data_dir: Option<PathBuf>,
    },
}

pub struct Options {
    pub directory: String,
    pub directory_certificate: PathBuf,
    pub agent_verifying_key: PathBuf,
    pub command: Command,
}

const FAILED: &str = "directory operation failed";

pub async fn run(options: Options) -> ExitCode {
    let Ok(certificate) = std::fs::read(&options.directory_certificate) else {
        eprintln!("aether: directory certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(verifying_key) = read_verifying_key(&options.agent_verifying_key) else {
        eprintln!("aether: agent verification key is invalid");
        return ExitCode::from(2);
    };
    let Ok(client) = DirectoryClient::with_pinned_certificate(
        &options.directory,
        &certificate,
        verifying_key,
    ) else {
        eprintln!("aether: directory connection is invalid");
        return ExitCode::from(2);
    };
    match options.command {
        Command::Publish { record } | Command::Renew { record } => {
            let Ok(record) = read_directory_record(&record) else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            };
            print_json(
                client.publish(&record).await,
                FAILED,
                "directory response is invalid",
            )
        }
        Command::Resolve { agent_id } => print_json(
            client.resolve(&agent_id).await,
            FAILED,
            "directory response is invalid",
        ),
        Command::Search { capability } => {
            print_json(client.search(&capability).await, FAILED, FAILED)
        }
        Command::Revoke { record } => {
            let Ok(record) = read_directory_record(&record) else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            };
            let revoked = client
                .revoke(&record)
                .await
                .map(|()| serde_json::json!({ "event": "directory_record_revoked" }));
            print_json(revoked, FAILED, FAILED)
        }
        Command::Sync { data_dir } => {
            let data_dir = crate::data_dir::resolve_node_data_dir(data_dir);
            let synchronized =
                sync_directory(&data_dir, &options.directory, verifying_key, &client)
                    .await
                    .map(|(cursor, records)| {
                        serde_json::json!({
                            "event": "directory_synchronized",
                            "cursor": cursor,
                            "records": records,
                        })
                    });
            let failed = "directory synchronization failed";
            print_json(synchronized, failed, failed)
        }
    }
}

/// Prints `result` as one line of JSON and succeeds. Otherwise prints
/// `failed`, or `unencodable` when the value cannot be encoded, and exits
/// with 1.
pub(crate) fn print_json<T: Serialize, E>(
    result: Result<T, E>,
    failed: &str,
    unencodable: &str,
) -> ExitCode {
    let Ok(value) = result else {
        eprintln!("aether: {failed}");
        return ExitCode::from(1);
    };
    let Ok(json) = serde_json::to_string(&value) else {
        eprintln!("aether: {unencodable}");
        return ExitCode::from(1);
    };
    println!("{json}");
    ExitCode::SUCCESS
}
