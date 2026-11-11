use std::sync::{Arc, atomic::Ordering};

use rmcp::handler::server::wrapper::Parameters;

use super::caller::SendArguments;
use super::hub::AccessRequestArguments;
use crate::admin_socket::start;
use crate::mcp::McpServer;

pub(super) async fn read_message<R>(reader: &mut R) -> serde_json::Value
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    let mut line = String::new();
    reader.read_line(&mut line).await.expect("message must read");
    serde_json::from_str(&line).expect("message must decode")
}

/// An MCP server for `administration` behind a real socket in
/// `directory`, as `aether node serve` runs it.
pub(super) fn served(
    directory: &std::path::Path,
    administration: crate::admin::SharedAdministration,
) -> McpServer {
    let socket = crate::data_dir::DataDir::new(directory).admin_socket();
    start(&socket, administration).expect("socket must start");
    McpServer::new(socket, directory.to_path_buf(), None, false)
}

pub(super) fn decode(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    serde_json::from_str(&decode_text(result)).expect("body must decode")
}

pub(super) fn decode_text(result: &rmcp::model::CallToolResult) -> String {
    result.content[0]
        .as_text()
        .map(|text| text.text.clone())
        .expect("result must carry text")
}

/// A courier that counts its exchanges and gives one answer to each.
pub(super) struct Counting {
    calls: std::sync::atomic::AtomicUsize,
    answer:
        fn() -> Result<Vec<crate::transport::Frame>, crate::transport::TransportError>,
}

impl Counting {
    pub(super) fn new(
        answer: fn() -> Result<
            Vec<crate::transport::Frame>,
            crate::transport::TransportError,
        >,
    ) -> Arc<Self> {
        Arc::new(Self { calls: std::sync::atomic::AtomicUsize::new(0), answer })
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl crate::outbox::Courier for Counting {
    fn exchange<'a>(
        &'a self,
        _bundle: &'a crate::direct::InvitationBundle,
        _link: &'a mut crate::transport::Link,
        _request: crate::transport::Frame,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<crate::transport::Frame>,
                        crate::transport::TransportError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = (self.answer)();
        Box::pin(async move { answer })
    }
}

#[expect(clippy::unnecessary_wraps, reason = "a courier answer is a Result")]
pub(super) fn completed()
-> Result<Vec<crate::transport::Frame>, crate::transport::TransportError> {
    Ok(vec![crate::transport::Frame {
        version: 0,
        kind: crate::transport::FrameKind::Result,
        exchange_id: "e".to_owned(),
        scope_id: "direct".to_owned(),
        task_id: None,
        payload: crate::transport::text_result_payload(
            "00000000-0000-4000-8000-000000000001",
            crate::transport::A2aTaskStatus::Completed,
            "Three names",
        ),
    }])
}

pub(super) const fn unreachable()
-> Result<Vec<crate::transport::Frame>, crate::transport::TransportError> {
    Err(crate::transport::TransportError::Timeout)
}

/// A caller data directory with the contact `tudor`.
pub(super) fn caller() -> (tempfile::TempDir, tempfile::TempDir) {
    let host = tempfile::tempdir().expect("directory must create");
    let caller = tempfile::tempdir().expect("directory must create");
    crate::settings::store_setting::<crate::hub_client::settings::Hub>(
        caller.path(),
        &Some(crate::hub_client::HUB_OFF.to_owned()),
    )
    .expect("the hub must turn off");
    let endpoint = crate::direct::endpoint_id(caller.path()).expect("endpoint");
    crate::direct::import_bundle(
        caller.path(),
        &crate::direct::test_bundle(host.path(), &endpoint, false),
    )
    .expect("the contact must import");
    (host, caller)
}

/// A caller data directory that uses `hub`, with the name `me#0001`.
pub(super) fn hub_caller(
    hub: &crate::hub_client::test_hub::TestHub,
) -> tempfile::TempDir {
    let caller = tempfile::tempdir().expect("directory must create");
    crate::settings::store_setting::<crate::hub_client::settings::Hub>(
        caller.path(),
        &Some(hub.url.clone()),
    )
    .expect("the hub must store");
    let endpoint = crate::direct::endpoint_id(caller.path()).expect("endpoint");
    hub.state().add_account(&endpoint, "me#0001");
    caller
}

/// Lists the service agent of `host` on `hub` as `handle`, with the
/// listing `bytes` signed by the endpoint key in `signer`. Returns the
/// endpoint of `host`.
pub(super) fn list_bytes(
    hub: &crate::hub_client::test_hub::TestHub,
    host: &std::path::Path,
    signer: &std::path::Path,
    handle: &str,
    bytes: &[u8],
) -> String {
    hub.add_listing(host, signer, handle, bytes);
    crate::direct::endpoint_id(host).expect("endpoint")
}

/// Lists the agent of `host` on `hub` as `handle` with `description`.
pub(super) fn list_on_hub(
    hub: &crate::hub_client::test_hub::TestHub,
    host: &std::path::Path,
    handle: &str,
    description: &str,
) -> String {
    let settings = crate::agent_card::CardSettings {
        description: description.to_owned(),
        ..crate::agent_card::CardSettings::default()
    };
    list_bytes(
        hub,
        host,
        host,
        handle,
        &crate::listing::listing_bytes(&settings, handle),
    )
}

pub(super) fn access_arguments(agent: &str) -> Parameters<AccessRequestArguments> {
    Parameters(AccessRequestArguments { agent: agent.to_owned(), days: None })
}

pub(super) fn send_arguments(agent: &str, text: &str) -> Parameters<SendArguments> {
    Parameters(SendArguments { agent: agent.to_owned(), text: text.to_owned() })
}

pub(super) fn import_named(
    caller: &std::path::Path,
    host: &std::path::Path,
    name: &str,
) {
    let endpoint = crate::direct::endpoint_id(caller).expect("endpoint");
    crate::direct::import_bundle(
        caller,
        &crate::direct::test_bundle_named(host, &endpoint, name, false),
    )
    .expect("the contact must import");
}
