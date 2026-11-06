#![allow(clippy::must_use_candidate)]

use std::{path::PathBuf, process::ExitCode};

use usage_rs::Subcommands;

use super::{ManagedClient, ManagedClientError, sync_managed};
use crate::directory_client::read_directory_record;

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
    let result = match options.command {
        Command::Redeem { token_file, record } => {
            let token = match std::fs::read_to_string(token_file) {
                Ok(token) if !token.trim().is_empty() => token,
                _ => {
                    eprintln!("aether: managed invitation token cannot be read");
                    return ExitCode::from(2);
                }
            };
            if let Ok(record) = read_directory_record(&record) {
                client.redeem(token.trim(), record).await
            } else {
                eprintln!("aether: managed member record is invalid");
                return ExitCode::from(2);
            }
        }
        Command::Members { scope } => match client.members(&scope).await {
            Ok(members) => match serde_json::to_string(&members) {
                Ok(members) => {
                    println!("{members}");
                    return ExitCode::SUCCESS;
                }
                Err(_) => Err(ManagedClientError::InvalidRecord),
            },
            Err(error) => Err(error),
        },
        Command::Events { scope, after } => {
            match client.events_after(&scope, after.unwrap_or_default()).await {
                Ok(events) => match serde_json::to_string(&events) {
                    Ok(events) => {
                        println!("{events}");
                        return ExitCode::SUCCESS;
                    }
                    Err(_) => Err(ManagedClientError::InvalidRecord),
                },
                Err(error) => Err(error),
            }
        }
        Command::Sync { scope, data_dir } => {
            let data_dir = crate::data_dir::resolve_node_data_dir(data_dir);
            if let Ok((cursor, changes)) =
                sync_managed(&data_dir, &options.control, &scope, &client).await
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "managed_roster_synchronized",
                        "cursor": cursor,
                        "changes": changes,
                    })
                );
                return ExitCode::SUCCESS;
            }
            eprintln!("aether: managed roster synchronization failed");
            return ExitCode::from(1);
        }
    };
    if let Ok(member) = result {
        if let Ok(member) = serde_json::to_string(&member) {
            println!("{member}");
            ExitCode::SUCCESS
        } else {
            eprintln!("aether: managed member cannot be encoded");
            ExitCode::from(1)
        }
    } else {
        eprintln!("aether: managed control request failed");
        ExitCode::from(1)
    }
}
