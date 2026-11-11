#![allow(clippy::must_use_candidate)]

use std::{path::Path, process::ExitCode};

use usage_rs::Subcommands;

use super::{DirectError, INVITE_PREFIX};

#[derive(Subcommands)]
pub enum Command {
    /// Imports an `aether-invite:` line, or a file with an invitation or an
    /// invitation bundle.
    Import {
        invitation: String,
    },
    Resolve {
        agent_id: String,
    },
    /// Forgets a contact: its invitation and its stored access.
    Remove {
        agent_id: String,
    },
}

pub fn run(data_dir: &Path, command: Command) -> ExitCode {
    match command {
        Command::Import { invitation } => {
            let source = if invitation.starts_with(INVITE_PREFIX) {
                invitation
            } else if let Ok(source) = std::fs::read_to_string(invitation) {
                source
            } else {
                eprintln!("aether: direct invitation cannot be read");
                return ExitCode::from(2);
            };
            let imported = match super::InvitationBundle::decode(&source) {
                Ok(bundle) => super::import_bundle(data_dir, &bundle),
                Err(_) => match serde_json::from_str(&source) {
                    Ok(wire) => super::import_invitation(data_dir, &wire),
                    Err(_) => Err(DirectError::Invalid),
                },
            };
            let invitation = match imported {
                Ok(invitation) => invitation,
                Err(
                    error @ (DirectError::Invalid
                    | DirectError::OtherEndpoint
                    | DirectError::NameInUse),
                ) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(2);
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    return ExitCode::from(1);
                }
            };
            println!(
                "{}",
                serde_json::json!({
                    "event": "direct_contact_imported",
                    "agent": invitation.agent_id,
                    "endpoint": invitation.endpoint_id,
                })
            );
            ExitCode::SUCCESS
        }
        Command::Remove { agent_id } => {
            match super::remove_contact(data_dir, &agent_id) {
                Ok(()) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "direct_contact_removed",
                            "agent": agent_id,
                        })
                    );
                    ExitCode::SUCCESS
                }
                Err(error @ DirectError::UnknownContact) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(2)
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Resolve { agent_id } => {
            match super::open_store(data_dir)
                .and_then(|store| super::contact(&store, &agent_id))
            {
                Ok(wire) => {
                    if let Ok(wire) = serde_json::to_string(&wire) {
                        println!("{wire}");
                        ExitCode::SUCCESS
                    } else {
                        eprintln!("aether: direct contact cannot be encoded");
                        ExitCode::from(1)
                    }
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

pub fn run_invite(data_dir: &Path, agent_id: &str, expires_in: u64) -> ExitCode {
    match super::create_invitation(
        data_dir,
        agent_id,
        std::time::Duration::from_secs(expires_in),
    ) {
        Ok(invitation) => {
            if let Ok(invitation) = serde_json::to_string(&invitation) {
                println!("{invitation}");
                ExitCode::SUCCESS
            } else {
                eprintln!("aether: direct invitation cannot be encoded");
                ExitCode::from(1)
            }
        }
        Err(DirectError::Invalid) => {
            eprintln!("aether: direct invitation is invalid");
            ExitCode::from(2)
        }
        Err(_) => {
            eprintln!("aether: direct invitation is unavailable");
            ExitCode::from(1)
        }
    }
}
