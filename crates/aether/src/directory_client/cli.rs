#![allow(clippy::must_use_candidate)]

use std::{path::PathBuf, process::ExitCode};

use super::{
    DirectoryClient, DirectoryClientError, read_directory_record, read_verifying_key,
    sync_directory,
};

pub enum Command {
    Publish { record: PathBuf },
    Renew { record: PathBuf },
    Resolve { agent_id: String },
    Search { capability: String },
    Revoke { record: PathBuf },
    Sync { data_dir: PathBuf },
}

pub struct Options {
    pub directory: String,
    pub directory_certificate: PathBuf,
    pub agent_verifying_key: PathBuf,
    pub command: Command,
}

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
    let result = match options.command {
        Command::Publish { record } | Command::Renew { record } => {
            if let Ok(record) = read_directory_record(&record) {
                client.publish(&record).await.map(Some)
            } else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            }
        }
        Command::Resolve { agent_id } => client.resolve(&agent_id).await.map(Some),
        Command::Search { capability } => match client.search(&capability).await {
            Ok(records) => match serde_json::to_string(&records) {
                Ok(records) => {
                    println!("{records}");
                    return ExitCode::SUCCESS;
                }
                Err(_) => Err(DirectoryClientError::InvalidRecord),
            },
            Err(error) => Err(error),
        },
        Command::Revoke { record } => {
            if let Ok(record) = read_directory_record(&record) {
                match client.revoke(&record).await {
                    Ok(()) => {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "directory_record_revoked" })
                        );
                        return ExitCode::SUCCESS;
                    }
                    Err(error) => Err(error),
                }
            } else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            }
        }
        Command::Sync { data_dir } => {
            if let Ok((cursor, records)) =
                sync_directory(&data_dir, &options.directory, verifying_key, &client)
                    .await
            {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "directory_synchronized",
                        "cursor": cursor,
                        "records": records,
                    })
                );
                return ExitCode::SUCCESS;
            }
            eprintln!("aether: directory synchronization failed");
            return ExitCode::from(1);
        }
    };
    match result {
        Ok(Some(record)) => {
            if let Ok(record) = serde_json::to_string(&record) {
                println!("{record}");
                ExitCode::SUCCESS
            } else {
                eprintln!("aether: directory response is invalid");
                ExitCode::from(1)
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("aether: directory operation failed");
            ExitCode::from(1)
        }
    }
}
