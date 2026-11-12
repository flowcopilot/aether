//! Fixtures the admin page tests share: pages over a temporary data
//! directory, and requests that go through the same router as a browser.

use std::sync::{Arc, Mutex};

use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::{
    AdminUi, PageContext, UiOptions, UiState, build_admin_ui, router, ui_service,
};
use crate::admin::{
    SharedAdministration,
    test_support::{FakeNode, shared_in},
};

pub(super) const PORT: u16 = 3000;

pub(super) fn test_node_hub(
    data_dir: &std::path::Path,
    hub_flag: Option<String>,
) -> Arc<crate::hub_client::NodeHub> {
    Arc::new(crate::hub_client::NodeHub::new(data_dir.to_path_buf(), hub_flag))
}

pub(super) fn test_host_page(
    data_dir: std::path::PathBuf,
    name: Option<String>,
    relay_url: Option<String>,
    switch_relay: crate::admin_ui::RelaySwitcher,
    hub_flag: Option<String>,
) -> PageContext {
    let hub = test_node_hub(&data_dir, hub_flag);
    PageContext::host(
        data_dir,
        name,
        relay_url,
        switch_relay,
        hub,
        Arc::new(Mutex::new(())),
    )
}

/// A host UI over a store in `data_dir`, whose grant applier records each
/// grant it receives instead of reaching a running node.
pub(super) fn host_ui(data_dir: &std::path::Path) -> (AdminUi, Arc<FakeNode>) {
    host_ui_with_mode(data_dir, false)
}

pub(super) fn host_ui_with_mode(
    data_dir: &std::path::Path,
    task_folder_mode: bool,
) -> (AdminUi, Arc<FakeNode>) {
    host_ui_with_roles(data_dir, task_folder_mode, false)
}

/// An administration service whose changes reach the returned fake
/// node, on workspace `/start`.
pub(super) fn test_administration(
    data_dir: &std::path::Path,
) -> (SharedAdministration, Arc<FakeNode>) {
    let administration = shared_in(data_dir);
    let node = Arc::new(FakeNode::default().with_workspace("/start"));
    administration.lock().expect("administration must lock").set_live(node.clone());
    (administration, node)
}

pub(super) fn host_ui_with_roles(
    data_dir: &std::path::Path,
    task_folder_mode: bool,
    caller: bool,
) -> (AdminUi, Arc<FakeNode>) {
    let (administration, node) = test_administration(data_dir);
    let mut page = test_host(data_dir, Arc::new(std::sync::Mutex::new(Vec::new())))
        .with_task_folder_mode(task_folder_mode);
    if caller {
        page = page.with_caller(true);
    }
    let options = UiOptions { page: Some(Arc::new(page)), ..UiOptions::default() };
    (build_admin_ui(administration, options, PORT), node)
}

/// Each relay switch as (from, to).
pub(super) type Switches = Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>>;

/// A host on relay.test whose relay switches are recorded in `switches`
/// instead of reaching a node. `https://down.test` never answers.
pub(super) fn test_host(data_dir: &std::path::Path, switches: Switches) -> PageContext {
    test_host_page(
        data_dir.to_path_buf(),
        Some("tudor".to_owned()),
        Some("https://relay.test".to_owned()),
        Arc::new(move |previous, relay| {
            let down = relay.as_deref() == Some("https://down.test");
            switches.lock().expect("switches must lock").push((previous, relay));
            Box::pin(async move {
                if down {
                    Err(crate::transport::TransportError::Timeout)
                } else {
                    Ok(())
                }
            })
        }),
        None,
    )
}

/// A host page whose hub is `hub`.
pub(super) fn hub_host_ui(
    data_dir: &std::path::Path,
    hub: &crate::hub_client::test_hub::TestHub,
) -> axum::Router {
    let (administration, _) = test_administration(data_dir);
    let page = Arc::new(
        test_host(data_dir, Arc::new(std::sync::Mutex::new(Vec::new())))
            .with_hub(Some(hub.client(data_dir))),
    );
    tokio::spawn(crate::hub_workers::host_worker(
        administration.clone(),
        page.worker_context(),
    ));
    let options = UiOptions { page: Some(page), ..UiOptions::default() };
    app(build_admin_ui(administration, options, PORT))
}

/// A caller page, with no host, whose hub is `hub`.
pub(super) fn hub_caller_ui(
    data_dir: &std::path::Path,
    hub: &crate::hub_client::test_hub::TestHub,
) -> axum::Router {
    let options = UiOptions {
        page: Some(Arc::new(
            PageContext::admin_caller(data_dir.to_path_buf())
                .with_hub(Some(hub.client(data_dir))),
        )),
        ..UiOptions::default()
    };
    app(build_admin_ui(shared_in(data_dir), options, PORT))
}

/// A host page for a service agent `law#0001` on `hub`, with the first
/// node policy in `<directory>/node`.
pub(super) async fn service_page(
    directory: &std::path::Path,
    hub: &crate::hub_client::test_hub::TestHub,
) -> (axum::Router, String, std::path::PathBuf) {
    let data_dir = directory.join("node");
    crate::node_builder::load_or_create_service(
        &directory.join("missing-policy.json"),
        &data_dir,
        "codex".into(),
        None,
    )
    .expect("first node policy must create");
    let page = hub_host_ui(&data_dir, hub);
    let cookie = session(&page).await;
    post(page.clone(), "/hub/name", Some(&cookie), "name=law&kind=service").await;
    (page, cookie, data_dir)
}

/// The `value` of the input with `id`, with HTML escapes undone.
pub(super) fn input_value(body: &str, id: &str) -> String {
    let (_, from_id) =
        body.split_once(&format!("id=\"{id}\"")).expect("page must have the input");
    let (tag, _) = from_id.split_once('>').expect("input must close");
    let (_, value) = tag.split_once("value=\"").expect("input must have a value");
    let (value, _) = value.split_once('"').expect("value must close");
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub(super) fn form_value(name: &str, value: &str) -> String {
    let mut url = reqwest::Url::parse("http://form.test/").expect("URL must parse");
    url.query_pairs_mut().append_pair(name, value);
    url.query().expect("query must exist").to_owned()
}

/// Opens `application`'s home page and returns its session cookie.
pub(super) async fn session(application: &axum::Router) -> String {
    let home = get(application.clone(), "/").await;
    session_cookie_pair(home.set_cookie.as_deref().expect("home must set a cookie"))
        .to_owned()
}

/// An `AdminUi` whose `Change::Directory` applies immediately (a no-op
/// applier), for tests that use a directory change only as a stand-in
/// low-risk fixture rather than to test directory behavior itself. Hands
/// back the same `SharedAdministration` the UI was built with, so a test
/// can queue further changes after the UI (and its topcoat `Router`)
/// already exist.
pub(super) fn admin_ui_with_directory_applier()
-> (tempfile::TempDir, AdminUi, SharedAdministration) {
    let directory = tempfile::tempdir().expect("directory must create");
    let administration = shared_in(directory.path());
    administration.lock().expect("administration must lock").set_live(Arc::new(
        FakeNode::default().with_directory("https://directory.test"),
    ));
    let admin_ui = build_admin_ui(administration.clone(), UiOptions::default(), PORT);
    (directory, admin_ui, administration)
}

pub(super) fn app(admin_ui: AdminUi) -> axum::Router {
    super::UI_HARDENING
        .bounded(router(UiState { service: ui_service(admin_ui.router) }))
}

pub(super) async fn get(app: axum::Router, path: &str) -> AxumTestResponse {
    AxumTestResponse::from(
        app.oneshot(
            Request::builder()
                .uri(path)
                .header("host", format!("127.0.0.1:{PORT}"))
                .body(axum::body::Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond"),
    )
    .await
}

/// A page load in the session of `cookie`. A load without the cookie
/// would start a new session and end that one.
pub(super) async fn get_with(
    app: axum::Router,
    path: &str,
    cookie: &str,
) -> AxumTestResponse {
    AxumTestResponse::from(
        app.oneshot(
            Request::builder()
                .uri(path)
                .header("host", format!("127.0.0.1:{PORT}"))
                .header("cookie", cookie.to_owned())
                .body(axum::body::Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond"),
    )
    .await
}

pub(super) async fn post(
    app: axum::Router,
    path: &str,
    cookie: Option<&str>,
    form: &str,
) -> AxumTestResponse {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", format!("127.0.0.1:{PORT}"))
        .header("origin", format!("http://127.0.0.1:{PORT}"))
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie.to_owned());
    }
    AxumTestResponse::from(
        app.oneshot(
            request
                .body(axum::body::Body::from(form.to_owned()))
                .expect("request must build"),
        )
        .await
        .expect("router must respond"),
    )
    .await
}

/// A fully-buffered response, for the tests that only need to assert on
/// status, headers, and body text rather than stream events.
pub(super) struct AxumTestResponse {
    pub(super) status: StatusCode,
    pub(super) headers: axum::http::HeaderMap,
    pub(super) body: String,
    pub(super) set_cookie: Option<String>,
}

impl AxumTestResponse {
    async fn from(response: axum::response::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let set_cookie = headers
            .get(axum::http::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body must read");
        let body = String::from_utf8(body.to_vec()).expect("body must be UTF-8");
        Self { status, headers, body, set_cookie }
    }
}

/// Extracts the session cookie's `name=value` pair from a `Set-Cookie`
/// header, dropping the attributes, so a later request can present it
/// back with the `cookie` header.
pub(super) fn session_cookie_pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().expect("a Set-Cookie header has at least one part")
}
