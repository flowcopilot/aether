#![allow(clippy::must_use_candidate)]

use std::{path::PathBuf, process::ExitCode};

use usage_rs::Subcommands;

use super::{ManagedClient, sync_managed};
use crate::directory_client::{cli::print_json, read_directory_record};

#[derive(Subcommands)]
pub enum Command {
    Redeem {
        #[usage(long)]
        token_file: PathBuf,
        record: PathBuf,
    },
    Members {
        scope: String,
    },
    Events {
        scope: String,
        #[usage(long)]
        after: Option<i64>,
    },
    Sync {
        scope: String,
        #[usage(long)]
        data_dir: Option<PathBuf>,
    },
}

pub struct Options {
    pub control: String,
    pub control_certificate: PathBuf,
    pub command: Command,
}

const FAILED: &str = "managed control request failed";

pub async fn run(options: Options) -> ExitCode {
    let Ok(certificate) = std::fs::read(&options.control_certificate) else {
        eprintln!("aether: control certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(client) =
        ManagedClient::with_pinned_certificate(&options.control, &certificate)
    else {
        eprintln!("aether: managed control connection is invalid");
        return ExitCode::from(2);
    };
    match options.command {
        Command::Redeem { token_file, record } => {
            let token = match std::fs::read_to_string(token_file) {
                Ok(token) if !token.trim().is_empty() => token,
                _ => {
                    eprintln!("aether: managed invitation token cannot be read");
                    return ExitCode::from(2);
                }
            };
            let Ok(record) = read_directory_record(&record) else {
                eprintln!("aether: managed member record is invalid");
                return ExitCode::from(2);
            };
            print_json(
                client.redeem(token.trim(), record).await,
                FAILED,
                "managed member cannot be encoded",
            )
        }
        Command::Members { scope } => {
            print_json(client.members(&scope).await, FAILED, FAILED)
        }
        Command::Events { scope, after } => print_json(
            client.events_after(&scope, after.unwrap_or_default()).await,
            FAILED,
            FAILED,
        ),
        Command::Sync { scope, data_dir } => {
            let data_dir = crate::data_dir::resolve_node_data_dir(data_dir);
            let synchronized =
                sync_managed(&data_dir, &options.control, &scope, &client).await.map(
                    |(cursor, changes)| {
                        serde_json::json!({
                            "event": "managed_roster_synchronized",
                            "cursor": cursor,
                            "changes": changes,
                        })
                    },
                );
            let failed = "managed roster synchronization failed";
            print_json(synchronized, failed, failed)
        }
    }
}
