use std::process::ExitCode;

use hex::FromHex;

use super::{
    NodeDirect, NodeDirectCommand, NodeDirectory, NodeDirectoryCommand, NodeInvite,
    NodeManaged, NodeManagedCommand, NodeRecord, NodeRequest, data_dir_or_default,
};

pub async fn managed_request(
    arguments: &NodeRequest,
    data_dir: &std::path::Path,
    control: &str,
    request: aether::transport::Frame,
) -> Result<Vec<aether::transport::Frame>, aether::transport::TransportError> {
    // The usage spec rejects the other addressing flags with
    // `--managed-control`, and requires the certificate and the agent id.
    let certificate_path = arguments
        .managed_control_certificate
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    let agent_id = arguments
        .agent_id
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    let certificate = std::fs::read(certificate_path)
        .map_err(|_| aether::transport::TransportError::Address)?;
    let client = aether::managed_client::ManagedClient::with_pinned_certificate(
        control,
        &certificate,
    )
    .map_err(|_| aether::transport::TransportError::Address)?;
    if aether::node_runtime::sync_managed(data_dir, control, &arguments.scope, &client)
        .await
        .is_err()
    {
        eprintln!(
            "{}",
            serde_json::json!({
                "level": "error",
                "event": "managed_sync_failed",
            })
        );
    }
    let members = match client.members(&arguments.scope).await {
        Ok(members) => members,
        Err(aether::managed_client::ManagedClientError::Request) => {
            vec![cached_managed_member(data_dir, &arguments.scope, agent_id).map_err(
                |()| {
                    aether::transport::TransportError::Network(
                        "roster lookup failed".to_owned(),
                    )
                },
            )?]
        }
        Err(_) => {
            return Err(aether::transport::TransportError::Network(
                "roster lookup failed".to_owned(),
            ));
        }
    };
    let member = members
        .into_iter()
        .find(|member| aether::node_runtime::managed_member_active(member, agent_id))
        .ok_or(aether::transport::TransportError::Address)?;
    let relay_url = arguments
        .relay_url
        .as_deref()
        .ok_or(aether::transport::TransportError::Address)?;
    aether::transport::request_once_via_relay(
        data_dir,
        &member.endpoint_id,
        relay_url,
        request,
    )
    .await
}

pub fn cached_managed_member(
    data_dir: &std::path::Path,
    scope_id: &str,
    agent_id: &str,
) -> Result<aether_store::ManagedMember, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    store
        .managed_members(scope_id)
        .map_err(|_| ())?
        .into_iter()
        .find(|member| aether::node_runtime::managed_member_active(member, agent_id))
        .ok_or(())
}

pub fn run_direct(arguments: NodeDirect) -> ExitCode {
    let data_dir = data_dir_or_default(arguments.data_dir);
    match arguments.command {
        NodeDirectCommand::Import { invitation } => {
            let source = if invitation.starts_with(aether::direct::INVITE_PREFIX) {
                invitation
            } else if let Ok(source) = std::fs::read_to_string(invitation) {
                source
            } else {
                eprintln!("aether: direct invitation cannot be read");
                return ExitCode::from(2);
            };
            // An invite line or a bundle carries the caller's authorization
            // too; a bare invitation file is the host's signed record only.
            let imported = match aether::direct::InvitationBundle::decode(&source) {
                Ok(bundle) => aether::direct::import_bundle(&data_dir, &bundle),
                Err(_) => match serde_json::from_str(&source) {
                    Ok(wire) => aether::direct::import_invitation(&data_dir, &wire),
                    Err(_) => Err(aether::direct::DirectError::Invalid),
                },
            };
            let invitation = match imported {
                Ok(invitation) => invitation,
                Err(
                    error @ (aether::direct::DirectError::Invalid
                    | aether::direct::DirectError::OtherEndpoint
                    | aether::direct::DirectError::NameInUse),
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
        NodeDirectCommand::Remove { agent_id } => {
            match aether::direct::remove_contact(&data_dir, &agent_id) {
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
                Err(error @ aether::direct::DirectError::UnknownContact) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(2)
                }
                Err(error) => {
                    eprintln!("aether: {error}");
                    ExitCode::from(1)
                }
            }
        }
        NodeDirectCommand::Resolve { agent_id } => {
            match aether::direct::open_store(&data_dir)
                .and_then(|store| aether::direct::contact(&store, &agent_id))
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

pub fn run_invite(arguments: &NodeInvite) -> ExitCode {
    match aether::direct::create_invitation(
        &arguments.data_dir,
        &arguments.agent_id,
        std::time::Duration::from_secs(arguments.expires_in),
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
        Err(aether::direct::DirectError::Invalid) => {
            eprintln!("aether: direct invitation is invalid");
            ExitCode::from(2)
        }
        Err(_) => {
            eprintln!("aether: direct invitation is unavailable");
            ExitCode::from(1)
        }
    }
}

pub fn run_record(arguments: NodeRecord) -> ExitCode {
    if arguments.agent_id.is_empty()
        || arguments.scope.is_empty()
        || arguments.capability.is_empty()
        || arguments.expires_in == 0
        || arguments.address.parse::<std::net::SocketAddr>().is_err()
    {
        eprintln!("aether: node record is invalid");
        return ExitCode::from(2);
    }
    let Ok(endpoint) =
        aether::data_dir::DataDir::new(&arguments.data_dir).endpoint_key()
    else {
        eprintln!("aether: node record is unavailable");
        return ExitCode::from(1);
    };
    let Ok(wire) = aether::directory_publication::signed_node_record(
        &arguments.data_dir,
        &aether::directory_publication::PublicationRecord {
            agent_id: arguments.agent_id,
            address: arguments.address,
            scope: arguments.scope,
            capability: arguments.capability,
            ttl: arguments.expires_in,
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

pub async fn run_managed(arguments: NodeManaged) -> ExitCode {
    let Ok(certificate) = std::fs::read(&arguments.control_certificate) else {
        eprintln!("aether: control certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(client) = aether::managed_client::ManagedClient::with_pinned_certificate(
        &arguments.control,
        &certificate,
    ) else {
        eprintln!("aether: managed control connection is invalid");
        return ExitCode::from(2);
    };
    let result = match arguments.command {
        NodeManagedCommand::Redeem { token_file, record } => {
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
        NodeManagedCommand::Members { scope } => match client.members(&scope).await {
            Ok(members) => match serde_json::to_string(&members) {
                Ok(members) => {
                    println!("{members}");
                    return ExitCode::SUCCESS;
                }
                Err(_) => {
                    Err(aether::managed_client::ManagedClientError::InvalidRecord)
                }
            },
            Err(error) => Err(error),
        },
        NodeManagedCommand::Events { scope, after } => {
            match client.events_after(&scope, after.unwrap_or_default()).await {
                Ok(events) => match serde_json::to_string(&events) {
                    Ok(events) => {
                        println!("{events}");
                        return ExitCode::SUCCESS;
                    }
                    Err(_) => {
                        Err(aether::managed_client::ManagedClientError::InvalidRecord)
                    }
                },
                Err(error) => Err(error),
            }
        }
        NodeManagedCommand::Sync { scope, data_dir } => {
            if let Ok((cursor, changes)) = aether::node_runtime::sync_managed(
                &data_dir,
                &arguments.control,
                &scope,
                &client,
            )
            .await
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

pub async fn run_directory(arguments: NodeDirectory) -> ExitCode {
    let Ok(certificate) = std::fs::read(&arguments.directory_certificate) else {
        eprintln!("aether: directory certificate cannot be read");
        return ExitCode::from(2);
    };
    let Ok(verifying_key) = read_verifying_key(&arguments.agent_verifying_key) else {
        eprintln!("aether: agent verification key is invalid");
        return ExitCode::from(2);
    };
    let Ok(client) = aether::directory_client::DirectoryClient::with_pinned_certificate(
        &arguments.directory,
        &certificate,
        verifying_key,
    ) else {
        eprintln!("aether: directory connection is invalid");
        return ExitCode::from(2);
    };
    let result = match arguments.command {
        NodeDirectoryCommand::Publish { record }
        | NodeDirectoryCommand::Renew { record } => {
            if let Ok(record) = read_directory_record(&record) {
                client.publish(&record).await.map(Some)
            } else {
                eprintln!("aether: directory record is invalid");
                return ExitCode::from(2);
            }
        }
        NodeDirectoryCommand::Resolve { agent_id } => {
            client.resolve(&agent_id).await.map(Some)
        }
        NodeDirectoryCommand::Search { capability } => {
            match client.search(&capability).await {
                Ok(records) => match serde_json::to_string(&records) {
                    Ok(records) => {
                        println!("{records}");
                        return ExitCode::SUCCESS;
                    }
                    Err(_) => Err(
                        aether::directory_client::DirectoryClientError::InvalidRecord,
                    ),
                },
                Err(error) => Err(error),
            }
        }
        NodeDirectoryCommand::Revoke { record } => {
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
        NodeDirectoryCommand::Sync { data_dir } => {
            if let Ok((cursor, records)) =
                sync_directory(&data_dir, &arguments.directory, verifying_key, &client)
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

pub async fn sync_directory(
    data_dir: &std::path::Path,
    directory: &str,
    trusted_key: [u8; 32],
    client: &aether::directory_client::DirectoryClient,
) -> Result<(i64, usize), ()> {
    std::fs::create_dir_all(data_dir).map_err(|_| ())?;
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let mut cursor = store.directory_cursor(directory, &trusted_key).map_err(|_| ())?;
    let mut records = 0usize;
    loop {
        let changes = client.events_after(cursor).await.map_err(|_| ())?;
        if changes.is_empty() {
            return Ok((cursor, records));
        }
        for change in changes {
            if change.cursor <= cursor {
                return Err(());
            }
            match change.event {
                aether_store::DirectoryEventType::Published
                | aether_store::DirectoryEventType::Renewed => {
                    let record = change.record.ok_or(())?;
                    cache_wire_directory_record(&store, &record)?;
                    records = records.checked_add(1).ok_or(())?;
                }
                aether_store::DirectoryEventType::Revoked => {
                    store
                        .mark_directory_record_stale(&change.scope_id, &change.agent_id)
                        .map_err(|_| ())?;
                }
            }
            cursor = change.cursor;
            store
                .advance_directory_cursor(directory, &trusted_key, cursor)
                .map_err(|_| ())?;
        }
    }
}

pub fn read_authorization(
    path: &std::path::Path,
) -> Result<aether::transport::A2aAuthorization, ()> {
    let source = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&source).map_err(|_| ())
}

pub fn read_verifying_key(path: &std::path::Path) -> Result<[u8; 32], ()> {
    let source = std::fs::read_to_string(path).map_err(|_| ())?;
    <[u8; 32]>::from_hex(source.trim()).map_err(|_| ())
}

pub fn read_directory_record(
    path: &std::path::Path,
) -> Result<aether::discovery::wire::WireRecord, ()> {
    serde_json::from_slice(&std::fs::read(path).map_err(|_| ())?).map_err(|_| ())
}

pub fn cache_directory_record(
    data_dir: &std::path::Path,
    record: &aether::discovery::Record,
) -> Result<(), ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    cache_wire_directory_record(
        &store,
        &aether::discovery::wire::WireRecord::from_record(record),
    )
}

pub fn cache_wire_directory_record(
    store: &aether_store::NodeStore,
    record: &aether::discovery::wire::WireRecord,
) -> Result<(), ()> {
    store
        .cache_directory_record(&aether_store::SignedRecord {
            scope_id: record.scope_id.clone(),
            agent_id: record.agent_id.clone(),
            record: serde_json::to_vec(record).map_err(|_| ())?,
            expires_at: i64::try_from(record.expires_at).map_err(|_| ())?,
        })
        .map_err(|_| ())
}

pub fn cached_directory_record(
    data_dir: &std::path::Path,
    scope_id: &str,
    agent_id: &str,
    trusted_key: [u8; 32],
) -> Result<aether::discovery::Record, ()> {
    let store = aether_store::NodeStore::in_dir(data_dir).map_err(|_| ())?;
    let stored = store.directory_record(scope_id, agent_id).map_err(|_| ())?;
    if stored.scope_id != scope_id || stored.agent_id != agent_id {
        return Err(());
    }
    if stored.expires_at <= aether_core::time::now() {
        return Err(());
    }
    let wire: aether::discovery::wire::WireRecord =
        serde_json::from_slice(&stored.record).map_err(|_| ())?;
    let record = wire.to_record().map_err(|_| ())?;
    if record.signing_key != trusted_key {
        return Err(());
    }
    aether::discovery::validate_record(&record).map_err(|_| ())?;
    Ok(record)
}
