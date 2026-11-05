use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::admin::{AdminError, AuditEntry, ChangeRequest, SharedAdministration};
use crate::node_config::settings;

const MAX_SOCKET_BYTES: u64 = 65_536;
/// A busy node can delay its accept loop. One second was too short under load,
/// and a caller then saw a timeout for a request that the node still answers.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Audit,
    DirectGrants,
    CreateInvite {
        friend_name: String,
        endpoint_id: String,
        days: u64,
    },
    RevokeGrant {
        id: String,
    },
    SetRelay {
        url: Option<String>,
    },
    Cancel {
        task_id: String,
    },
    Change {
        change: ChangeRequest,
        dry_run: bool,
    },
    ConfirmMcp {
        id: String,
    },
    /// Starts a round of the node's outbox.
    OutboxWake,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Audit { entries: Vec<AuditEntry> },
    DirectGrants { grants: Vec<DirectGrantInfo> },
    Invitation { line: String },
    Cancelled,
    Applied,
    Pending,
    Validated,
    Error { code: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectGrantInfo {
    pub id: String,
    pub agent: String,
    pub endpoint_id: String,
    pub expires_at: i64,
}

pub fn start(path: &Path, administration: SharedAdministration) -> io::Result<()> {
    fs::create_dir_all(
        path.parent().ok_or_else(|| io::Error::other("socket path has no parent"))?,
    )?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "socket path is not a socket",
            ));
        }
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "administration service is already running",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(error) => return Err(error),
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let data_dir = path
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no parent"))?
        .to_path_buf();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve(stream, &administration, &data_dir);
        }
    });
    Ok(())
}

pub fn send(path: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    serde_json::to_writer(&mut stream, request).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    read_limited(&mut stream, &mut response)?;
    serde_json::from_slice(&response).map_err(io::Error::other)
}

fn serve(
    mut stream: UnixStream,
    administration: &SharedAdministration,
    data_dir: &Path,
) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let mut request = Vec::new();
    let response = match read_request(&mut stream, &mut request)
        .and_then(|()| serde_json::from_slice(&request).map_err(io::Error::other))
    {
        Ok(request) => handle(administration, data_dir, request),
        Err(_) => Response::Error { code: "invalid".to_owned() },
    };
    serde_json::to_writer(&mut stream, &response).map_err(io::Error::other)
}

/// Reads one request, ended by a newline or by end of file. The newline is
/// what a current client sends: on macOS, a client's `shutdown(Write)` that
/// happens while its connection still waits in the accept backlog can fail
/// to reach this side, so a request framed only by end of file then stalls
/// both peers until their timeouts.
fn read_request(stream: &mut UnixStream, output: &mut Vec<u8>) -> io::Result<()> {
    let mut reader = BufReader::new(stream.take(MAX_SOCKET_BYTES + 1));
    reader.read_until(b'\n', output)?;
    if over_limit(output.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "socket message is too large",
        ));
    }
    Ok(())
}

fn read_limited(stream: &mut UnixStream, output: &mut Vec<u8>) -> io::Result<()> {
    let mut reader = stream.take(MAX_SOCKET_BYTES + 1);
    reader.read_to_end(output)?;
    if over_limit(output.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "socket message is too large",
        ));
    }
    Ok(())
}

fn handle(
    administration: &SharedAdministration,
    data_dir: &Path,
    request: Request,
) -> Response {
    let Ok(mut administration) = administration.lock() else {
        return Response::Error { code: "unavailable".to_owned() };
    };
    match request {
        Request::Audit => Response::Audit { entries: administration.audit_entries() },
        Request::DirectGrants => Response::DirectGrants {
            grants: administration
                .grants()
                .into_iter()
                .filter(|grant| {
                    grant.scope_id.as_str() == crate::direct::SCOPE
                        && grant.expires_at > std::time::SystemTime::now()
                })
                .map(|grant| DirectGrantInfo {
                    id: grant.id.as_str().to_owned(),
                    agent: grant.subject_agent_id.as_str().to_owned(),
                    endpoint_id: grant.subject_endpoint_id.as_str().to_owned(),
                    expires_at: aether_core::time::unix_timestamp(grant.expires_at)
                        .unwrap_or(i64::MAX),
                })
                .collect(),
        },
        Request::CreateInvite { friend_name, endpoint_id, days } => {
            // The hub name comes first, as on the host page.
            let Some(agent_name) =
                crate::node_config::setting::<settings::HubName>(data_dir)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        crate::node_config::setting::<settings::HostName>(data_dir).ok()
                    })
            else {
                return Response::Error { code: "unavailable".to_owned() };
            };
            let Ok(relay_url) =
                crate::node_config::setting::<settings::ActiveRelay>(data_dir)
            else {
                return Response::Error { code: "unavailable".to_owned() };
            };
            // The card comes first: when it fails, the node applies no grant.
            let Ok(agent_card) = crate::direct::create_card(data_dir, &agent_name)
            else {
                return Response::Error { code: "unavailable".to_owned() };
            };
            let Ok((authorization, grant)) = crate::direct::sign_invite_access(
                data_dir,
                &friend_name,
                &endpoint_id,
                days,
            ) else {
                return Response::Error { code: "invalid".to_owned() };
            };
            let change = match (crate::admin::ChangeRequest::Grant {
                id: grant.id.as_str().to_owned(),
                grant: crate::node_config::grant_json(&grant).to_string(),
            })
            .into_change()
            {
                Ok(change) => change,
                Err(error) => return error_response(&error),
            };
            // New invites join friends, as on the host page.
            match crate::access_groups::join_and_apply(
                data_dir,
                crate::access_groups::FRIENDS_GROUP,
                &friend_name,
                &endpoint_id,
                || administration.apply_ui(&change),
            ) {
                Ok(()) => {}
                Err(crate::access_groups::JoinError::Apply(error)) => {
                    return error_response(&error);
                }
                Err(crate::access_groups::JoinError::Store(_)) => {
                    return Response::Error { code: "unavailable".to_owned() };
                }
            }
            match crate::direct::encode_invite_after_grant(
                data_dir,
                &agent_name,
                days,
                authorization,
                relay_url,
                agent_card,
            ) {
                Ok(line) => Response::Invitation { line },
                Err(_) => Response::Error { code: "unavailable".to_owned() },
            }
        }
        Request::RevokeGrant { id } => {
            if !administration.grants().iter().any(|grant| {
                grant.id.as_str() == id
                    && grant.scope_id.as_str() == crate::direct::SCOPE
            }) {
                return Response::Error { code: "not_found".to_owned() };
            }
            match administration.revoke_ui(&id) {
                Ok(()) => Response::Applied,
                Err(error) => error_response(&error),
            }
        }
        Request::SetRelay { url } => match administration.set_relay_ui(url) {
            Ok(()) => Response::Applied,
            Err(error) => error_response(&error),
        },
        Request::Cancel { task_id } => match administration.cancel_task(&task_id) {
            Ok(()) => Response::Cancelled,
            Err(error) => error_response(&error),
        },
        Request::OutboxWake => match administration.wake_outbox() {
            Ok(()) => Response::Applied,
            Err(error) => error_response(&error),
        },
        Request::ConfirmMcp { id } => match administration.confirm_mcp(&id) {
            Ok(()) => Response::Applied,
            Err(error) => error_response(&error),
        },
        Request::Change { change, dry_run } => {
            let change = match change.into_change() {
                Ok(change) => change,
                Err(error) => return error_response(&error),
            };
            let classified_high_risk = change.is_high_risk();
            match administration.change(&change, dry_run) {
                Ok(()) if dry_run => Response::Validated,
                Ok(()) if classified_high_risk => Response::Pending,
                Ok(()) => Response::Applied,
                Err(error) => error_response(&error),
            }
        }
    }
}

/// Whether a message of `length` bytes is over the socket's limit.
fn over_limit(length: usize) -> bool {
    u64::try_from(length).map_or(true, |length| length > MAX_SOCKET_BYTES)
}

fn error_response(error: &AdminError) -> Response {
    let code = match error {
        AdminError::InvalidField(_) => "invalid",
        AdminError::Expired => "expired",
        AdminError::ConfirmationRequired => "confirmation_required",
        AdminError::Store => "unavailable",
        AdminError::Task => "task",
        AdminError::Unsupported(_) => "unsupported",
        AdminError::Stale => "stale",
    };
    Response::Error { code: code.to_owned() }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
    };

    use super::{Request, Response, send, start};
    use crate::admin::{AuditEntry, test_support::shared_in};

    #[test]
    fn socket_shares_pending_changes_without_mcp_confirmation() {
        let directory = tempfile::tempdir().expect("directory must create");
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        let administration = shared_in(directory.path());
        start(&socket, administration.clone()).expect("socket must start");
        assert!(matches!(
            send(
                &socket,
                &Request::Change {
                    change: crate::admin::ChangeRequest::Workspace {
                        id: "workspace".to_owned(),
                        path: "project".to_owned(),
                    },
                    dry_run: false,
                }
            )
            .expect("change must send"),
            Response::Pending
        ));
        assert!(matches!(
            send(&socket, &Request::ConfirmMcp { id: "workspace".to_owned() })
                .expect("confirmation must send"),
            Response::Error { code } if code == "confirmation_required"
        ));
        administration
            .lock()
            .expect("administration must unlock")
            .confirm_ui("workspace")
            .expect("ui must confirm");
        assert!(matches!(
            send(&socket, &Request::Audit).expect("audit must send"),
            Response::Audit { entries }
                if entries
                    == [("mcp", "pending"), ("ui", "confirmed"), ("ui", "applied")]
                        .map(|(actor, result)| AuditEntry {
                            actor: actor.to_owned(),
                            action: "change".to_owned(),
                            result: result.to_owned(),
                        })
        ));
    }

    #[test]
    fn outbox_wake_reaches_the_node_and_is_unsupported_without_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        let administration = shared_in(directory.path());
        start(&socket, administration.clone()).expect("socket must start");
        assert!(matches!(
            send(&socket, &Request::OutboxWake).expect("wake must send"),
            Response::Error { code } if code == "unsupported"
        ));
        let node = std::sync::Arc::new(crate::admin::test_support::FakeNode::default());
        administration
            .lock()
            .expect("administration must unlock")
            .set_live(node.clone());
        assert!(matches!(
            send(&socket, &Request::OutboxWake).expect("wake must send"),
            Response::Applied
        ));
        // The wake is kept for the next wait of the outbox.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime must build");
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                node.outbox().notified(),
            )
            .await
            .expect("the outbox must be woken");
        });
    }

    #[test]
    fn socket_rejects_oversized_messages_and_recovers_stale_sockets() {
        let directory = tempfile::tempdir().expect("directory must create");
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        drop(UnixListener::bind(&socket).expect("stale socket must create"));
        let administration = shared_in(directory.path());
        start(&socket, administration).expect("stale socket must recover");
        let mut stream = std::os::unix::net::UnixStream::connect(&socket)
            .expect("socket must connect");
        stream
            .write_all(&vec![
                b'x';
                usize::try_from(super::MAX_SOCKET_BYTES + 1)
                    .expect("the limit must fit in memory")
            ])
            .expect("message must write");
        // The server stops reading at the limit and may answer and close
        // before this shutdown; macOS then reports `NotConnected`. The
        // answer is still there to read.
        match stream.shutdown(std::net::Shutdown::Write) {
            Err(error) if error.kind() != std::io::ErrorKind::NotConnected => {
                panic!("write must close: {error}")
            }
            _ => {}
        }
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("response must read");
        assert!(matches!(
            serde_json::from_slice::<Response>(&response).expect("response must decode"),
            Response::Error { code } if code == "invalid"
        ));
    }

    #[test]
    fn socket_does_not_replace_an_active_owner() {
        let directory = tempfile::tempdir().expect("directory must create");
        let socket = crate::data_dir::DataDir::new(directory.path()).admin_socket();
        let listener = UnixListener::bind(&socket).expect("socket must bind");

        let error = start(&socket, shared_in(directory.path()))
            .expect_err("second owner must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(socket.exists());

        drop(listener);
    }
}
