use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime},
};

use axum::{Router as AxumRouter, response::Response as AxumResponse};
use futures_util::{Stream, StreamExt, stream};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use topcoat::{
    Result as TopcoatResult,
    context::{Cx, app_context},
    cookie::RouterBuilderCookieExt,
    router::{
        Body as TopcoatBody, BodyLimit, HeaderValue, Next, Router as TopcoatRouter,
        Slot,
        content::sse::{Event, KeepAlive, Sse, last_event_id},
        error::{forbidden, internal_server_error},
        header, layer, layout, page,
        request::headers,
        response::IntoResponse as TopcoatIntoResponse,
        route,
    },
    session::{self, RouterBuilderSessionExt, SessionConfig, TokenHash},
    view::{View, component, view},
};
use tower::util::MapResponse;

mod advanced;
mod audit;
mod caller;
mod host;
pub(crate) mod hub;
mod layout;
pub(crate) mod listing;
mod pact;

use crate::admin::SharedAdministration;
use advanced::dashboard;
use caller::caller_screen;
use host::{Notice, host_screen};

const UI_SESSION_LIFETIME: Duration = Duration::from_secs(3_600);
/// Matches the `DefaultBodyLimit` of `UI_HARDENING`: both cap the same
/// forms, one at the topcoat router, the other at the axum extractor.
const UI_BODY_LIMIT: usize = 8_192;
const UI_HARDENING: crate::https::Hardening = crate::https::Hardening {
    handshake_timeout: Duration::from_secs(5),
    max_connections: 32,
    request_timeout: Duration::from_secs(10),
    body_limit: UI_BODY_LIMIT,
};

/// Copies a `data-copy-text` value on click and says so in the button's
/// `data-copy-label`. Only the Advanced page, which has `#activity-status`,
/// opens the activity stream.
const ADMIN_JAVASCRIPT: &str = "document.addEventListener('click',async event=>{const button=event.target.closest('[data-copy-text]');if(!button)return;const label=button.querySelector('[data-copy-label]')||button;const text=label.textContent;try{await navigator.clipboard.writeText(button.dataset.copyText);label.textContent='Copied'}catch{label.textContent='Select and copy'}setTimeout(()=>{label.textContent=text},1600)});const status=document.getElementById('activity-status');if(status){const events=new EventSource('/events');events.addEventListener('ready',()=>{status.textContent='Activity updates are connected.'});events.addEventListener('audit',event=>{status.textContent=JSON.parse(event.data).entry});events.addEventListener('task',event=>{const task=JSON.parse(event.data);status.textContent=`Task ${task.taskId}: ${task.event}`});events.onerror=()=>{status.textContent='Activity updates will retry.'}}if(document.querySelector('[data-hub-waiting]')){const reload=()=>{const active=document.activeElement;if(active&&active.matches('input,select,textarea')&&active.value){setTimeout(reload,4000);return}location.assign('/')};setTimeout(reload,4000)}const dots=document.querySelectorAll('[data-presence]');if(dots.length){const update=async()=>{try{const answer=await fetch('/hub/presence');if(!answer.ok)return;const presence=await answer.json();for(const dot of dots){const online=presence[dot.dataset.presence];dot.dataset.state=online===undefined?'unknown':online?'online':'offline';dot.querySelector('[data-presence-label]').textContent=online===undefined?'':online?'Online':'Offline'}}catch{}};update();setInterval(update,15000)}";

//
// topcoat has no TLS of its own; its docs point at fronting it with a
// reverse proxy. Here the "reverse proxy" is the axum server this module
// already ran: `crate::https::serve` (`axum_server`, rustls and the
// limits of `UI_HARDENING`), and a topcoat `Router` is mounted underneath it
// as an ordinary `tower::Service` through topcoat's own `TowerService`,
// which converts the request body. The response body and the security
// headers are converted here, in `into_axum_response`.
type UiService = MapResponse<
    topcoat::router::tower::TowerService,
    fn(topcoat::router::response::Response) -> AxumResponse,
>;

fn ui_service(router: TopcoatRouter) -> UiService {
    let into_axum: fn(_) -> _ = into_axum_response;
    MapResponse::new(topcoat::router::tower::TowerService::new(router), into_axum)
}

/// Converts a topcoat response into an axum one and attaches the security
/// headers every response carries, success or error alike. topcoat's
/// `Router::handle` already turns a handler `Err` into a concrete
/// `Response` (its own status-code mapping), so by the time a response
/// reaches this function there is no error case left to special-case.
fn into_axum_response(response: topcoat::router::response::Response) -> AxumResponse {
    let mut response = response.map(axum::body::Body::new);
    let headers = response.headers_mut();
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; connect-src 'self'; script-src 'self'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

/// The loopback port this admin UI is bound to, so the host-check layer can
/// reject a request that names a different port (or host) even though it
/// physically arrived on the loopback interface.
struct AdminPort(u16);

#[derive(Clone, Copy)]
struct Managed(bool);

/// The single admin session this process trusts, recorded the way
/// topcoat's session docs ask: topcoat carries the token, the application
/// owns the record. This admin UI has no login step and no user account —
/// whichever browser first loads the page is trusted, so there is exactly
/// one slot rather than a table keyed by token hash.
struct AdminSession {
    hash: TokenHash,
    expires_at: SystemTime,
}

struct AdminSessionSlot(Mutex<Option<AdminSession>>);

impl AdminSessionSlot {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn valid(&self, hash: &TokenHash) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(
            |session| &session.hash == hash && session.expires_at > SystemTime::now(),
        )
    }

    fn store(&self, session: session::Session) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(AdminSession {
            hash: session.token_hash,
            expires_at: session.expires_at,
        });
    }

    #[cfg(test)]
    fn expire(&self) {
        if let Some(session) =
            self.0.lock().unwrap_or_else(PoisonError::into_inner).as_mut()
        {
            session.expires_at = SystemTime::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or(SystemTime::UNIX_EPOCH);
        }
    }
}

/// Maps a one-time opaque token to the real (possibly sensitive) pending
/// change id, so the rendered page never carries the real id — see
/// `pending_changes_show_no_secret_value`. Rebuilt on every home-page render.
struct AdminConfirmations(Mutex<BTreeMap<String, String>>);

impl AdminConfirmations {
    const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    fn reset(
        &self,
        pending: Vec<crate::admin::PendingChange>,
    ) -> TopcoatResult<Vec<String>> {
        let mut confirmations = BTreeMap::new();
        let mut tokens = Vec::with_capacity(pending.len());
        for change in pending {
            let token = confirmation_token()?;
            confirmations.insert(token.clone(), change.id);
            tokens.push(token);
        }
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = confirmations;
        Ok(tokens)
    }

    fn resolve(&self, token: &str) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(token)
    }
}

fn confirmation_token() -> TopcoatResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(internal_server_error)?;
    Ok(hex::encode(bytes))
}

/// Rejects a request whose `Host` header does not name this exact loopback
/// address. This is independent of topcoat's own `OriginPolicy` (which the
/// router applies to every request before any layer runs, and which only
/// concerns state-changing cross-origin requests): a `Host` mismatch is a
/// DNS-rebinding style attack and applies to every method, including a
/// plain `GET /`.
#[layer("/")]
async fn enforce_loopback_host(
    cx: &Cx,
    body: TopcoatBody,
    next: Next<'_>,
) -> TopcoatResult<topcoat::router::response::Response> {
    let AdminPort(port) = *app_context::<AdminPort>(cx);
    let expected = format!("127.0.0.1:{port}");
    let host_matches = headers(cx)
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if !host_matches {
        return Err(forbidden().into());
    }
    next.run(cx, body).await
}

/// Requires a valid admin session. Combined with topcoat's default
/// `OriginPolicy` (same-origin only for anything but `GET`/`HEAD`/
/// `OPTIONS`), so there is no separate CSRF token: the origin check
/// and the session cookie (`HttpOnly`, so no script on any origin can read
/// it to forge the header) already establish both "this request came from
/// our own page" and "this browser holds our session".
///
/// A function, not a `#[layer]`: `session::token_hash` only resolves once
/// topcoat's own cookie/session layers have run, and those are registered
/// through `.cookies()`/`.sessions(...)`, not through this module's
/// `.layer(...)` calls, so their relative nesting is not this module's to
/// control. Calling this from each state-changing handler (`confirm`,
/// `cancel`) — a `cx` function, in the shape topcoat's own docs recommend
/// for exactly this ("functions, not middlewares") — sidesteps the
/// ordering question entirely: a page handler always runs inside every
/// layer that matched its route.
async fn require_admin_session(cx: &Cx) -> TopcoatResult<()> {
    let sessions = app_context::<Arc<AdminSessionSlot>>(cx);
    let valid =
        session::token_hash(cx).await?.is_some_and(|hash| sessions.valid(&hash));
    if valid { Ok(()) } else { Err(forbidden().into()) }
}

/// Establishes (or slides the expiry of) the one admin session this process
/// trusts. There is no login form: whichever browser first loads the page
/// is the operator.
async fn ensure_admin_session(cx: &Cx) -> TopcoatResult<()> {
    let sessions = app_context::<Arc<AdminSessionSlot>>(cx);
    if let Some(hash) = session::token_hash(cx).await?
        && sessions.valid(&hash)
    {
        if let Some(session) = session::refresh(cx).await? {
            sessions.store(session);
        }
        return Ok(());
    }
    let session = session::start(cx).await?;
    sessions.store(session);
    Ok(())
}

#[layout("/")]
async fn root_layout(slot: Slot<'_>) -> TopcoatResult<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"Aether control"</title>
                <link rel="stylesheet" href="/styles.css">
            </head>
            <body class="min-h-screen">
                <a
                    class="sr-only focus:not-sr-only focus:fixed focus:top-4 focus:left-4 focus:z-10 focus:rounded-lg focus:bg-background focus:px-3 focus:py-2 focus:shadow-sm"
                    href="#main"
                >
                    "Skip to content"
                </a>
                (slot)
                <script defer="" src="/admin.js"></script>
            </body>
        </html>
    })
}

#[component]
async fn hub_form_screen(
    cx: &Cx,
    #[default] error: String,
    #[default] setting_error: String,
) -> TopcoatResult<impl View> {
    let Page(page) = app_context::<Page>(cx);
    let hosting = page.as_ref().is_some_and(|page| page.host_ui);
    Ok(view! {
        if hosting {
            host_screen(notice: Notice::hub(error.clone(), setting_error.clone()))
        } else {
            caller_screen(hub_error: error.clone(), hub_setting_error: setting_error.clone())
        }
    })
}

/// The screen of this UI's mode: the host screen for `node serve`, the
/// caller screen for `node admin`, and the Advanced page when neither is
/// set.
#[page("/")]
async fn home(cx: &Cx) -> TopcoatResult<impl View> {
    let Page(page) = app_context::<Page>(cx);
    Ok(view! {
        if page.as_ref().is_some_and(|page| page.host_ui) {
            host_screen()
        } else if page.as_ref().is_some_and(|page| page.caller) {
            caller_screen()
        } else {
            dashboard()
        }
    })
}

#[page("/agents")]
async fn agents(cx: &Cx) -> TopcoatResult<impl View> {
    let Page(Some(page)) = app_context::<Page>(cx) else {
        return Err(forbidden().into());
    };
    if !page.caller {
        return Err(forbidden().into());
    }
    Ok(view! { caller_screen() })
}

/// The page script: copy buttons, and live activity on the Advanced page.
#[route(GET "/admin.js")]
async fn admin_javascript() -> TopcoatResult<impl TopcoatIntoResponse> {
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        ADMIN_JAVASCRIPT,
    ))
}

/// The compiled theme, built by `build.rs` (`topcoat::tailwind::BuildConfig`)
/// from `styles.css` at the package root. Served directly with
/// `include_str!` rather than through topcoat's asset bundler: the bundler
/// needs a separate `topcoat asset bundle` step before the URL it hands out
/// resolves, which does not fit a plain `cargo build`/`cargo test` — the CSS
/// itself is already sitting in `OUT_DIR` the moment the build script runs,
/// with no extra step required to read it in here.
#[route(GET "/styles.css")]
async fn stylesheet() -> TopcoatResult<impl TopcoatIntoResponse> {
    Ok((
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/css; charset=utf-8"))],
        include_str!(concat!(env!("OUT_DIR"), "/tailwind.css")),
    ))
}

#[derive(Clone, Copy, Default)]
struct ActivityCursor {
    audit: usize,
    task: i64,
}

impl ActivityCursor {
    fn parse(value: &str) -> Option<Self> {
        let (audit, task) = value.split_once(':')?;
        Some(Self { audit: audit.parse().ok()?, task: task.parse().ok()? })
    }

    fn id(self) -> String {
        format!("{}:{}", self.audit, self.task)
    }
}

#[route(GET "/events")]
async fn events(
    cx: &Cx,
) -> TopcoatResult<Sse<impl Stream<Item = TopcoatResult<Event>> + use<>>> {
    let cursor = last_event_id(cx).and_then(ActivityCursor::parse).unwrap_or_default();
    let feed =
        ActivityFeed::new(app_context::<SharedAdministration>(cx).clone(), cursor);
    let events = stream::once(async move {
        Ok(Event::new().event("ready").data("{\"state\":\"ready\"}"))
    })
    .chain(stream::unfold(feed, ActivityFeed::next));
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// The most audit records, and the most task events, read at one time.
const ACTIVITY_BATCH: usize = 100;
/// How long the feed waits with no signal. Audit records that another
/// process writes come after this time at the latest.
const ACTIVITY_FALLBACK: Duration = Duration::from_secs(3);

/// The audit records and the task events after a cursor, read in batches.
/// The feed reads again when the administration writes an audit record,
/// when a task of the running node changes, or after the fallback time.
struct ActivityFeed {
    administration: SharedAdministration,
    cursor: ActivityCursor,
    ready: VecDeque<Event>,
    audited: tokio::sync::watch::Receiver<()>,
    tasks: Option<tokio::sync::broadcast::Receiver<String>>,
}

impl ActivityFeed {
    fn new(administration: SharedAdministration, cursor: ActivityCursor) -> Self {
        let (audited, tasks) = {
            let administration =
                administration.lock().unwrap_or_else(PoisonError::into_inner);
            (administration.watch_audit(), administration.subscribe_task_events())
        };
        Self { administration, cursor, ready: VecDeque::new(), audited, tasks }
    }

    async fn next(mut self) -> Option<(TopcoatResult<Event>, Self)> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Some((Ok(event), self));
            }
            if !self.read_batch() {
                self.wait().await;
            }
        }
    }

    /// Queues the next batch of events. Returns `false` when there is none.
    fn read_batch(&mut self) -> bool {
        let (audit, tasks) = {
            let administration =
                self.administration.lock().unwrap_or_else(PoisonError::into_inner);
            (
                administration.audit_page(self.cursor.audit, ACTIVITY_BATCH),
                administration.task_activity_after(self.cursor.task, ACTIVITY_BATCH),
            )
        };
        let found = !audit.is_empty() || !tasks.is_empty();
        for entry in audit {
            self.cursor.audit = self.cursor.audit.saturating_add(1);
            self.ready.push_back(
                Event::new()
                    .id(self.cursor.id())
                    .event("audit")
                    .data(serde_json::json!({"entry": entry}).to_string()),
            );
        }
        for task in tasks {
            self.cursor.task = task.cursor;
            self.ready.push_back(
                Event::new().id(self.cursor.id()).event("task").data(
                    serde_json::json!({
                        "taskId": task.task_id,
                        "event": task.event_type.as_str(),
                    })
                    .to_string(),
                ),
            );
        }
        found
    }

    async fn wait(&mut self) {
        let Self { audited, tasks, .. } = self;
        let task_changed = async {
            match tasks.as_mut() {
                Some(receiver) => {
                    matches!(
                        receiver.recv().await,
                        Err(tokio::sync::broadcast::error::RecvError::Closed)
                    )
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = audited.changed() => {}
            closed = task_changed => {
                if closed {
                    *tasks = None;
                }
            }
            () = tokio::time::sleep(ACTIVITY_FALLBACK) => {}
        }
    }
}

/// Everything the admin UI needs: the finished topcoat `Router` and the two
/// handles tests use to reach into its app context (production code only
/// needs the router itself).
struct AdminUi {
    router: TopcoatRouter,
    #[cfg_attr(not(test), allow(dead_code))]
    sessions: Arc<AdminSessionSlot>,
}

/// The result of switching a running node's relay.
pub type RelayFuture =
    Pin<Box<dyn Future<Output = Result<(), crate::transport::TransportError>> + Send>>;

/// Switches the running node from its current relay (the first argument) to
/// another one, or to none. Tests stand in for the network with their own.
pub type RelaySwitcher =
    Arc<dyn Fn(Option<String>, Option<String>) -> RelayFuture + Send + Sync>;

#[derive(Clone)]
pub enum HubAccess {
    Off,
    Ready(Box<crate::hub_client::HubClient>),
    Unavailable,
}

pub struct PageContext {
    pub data_dir: PathBuf,
    pub task_folder_mode: bool,
    pub host_ui: bool,
    pub caller: bool,
    name: Option<String>,
    relay_url: Mutex<Option<String>>,
    switch_relay: RelaySwitcher,
    hub: crate::hub_client::NodeHub,
    hub_sign: Mutex<()>,
}

impl PageContext {
    #[must_use]
    pub fn host(
        data_dir: PathBuf,
        name: Option<String>,
        relay_url: Option<String>,
        switch_relay: RelaySwitcher,
        hub_flag: Option<String>,
    ) -> Self {
        let hub = crate::hub_client::NodeHub::new(data_dir.clone(), hub_flag);
        Self {
            data_dir,
            task_folder_mode: false,
            host_ui: true,
            caller: false,
            name,
            relay_url: Mutex::new(relay_url),
            switch_relay,
            hub,
            hub_sign: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn admin_caller(data_dir: PathBuf) -> Self {
        let hub = crate::hub_client::NodeHub::new(data_dir.clone(), None);
        Self {
            data_dir,
            task_folder_mode: false,
            host_ui: false,
            caller: true,
            name: None,
            relay_url: Mutex::new(None),
            switch_relay: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            hub,
            hub_sign: Mutex::new(()),
        }
    }

    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn with_caller(mut self, enabled: bool) -> Self {
        self.caller = enabled;
        self
    }

    pub(crate) fn agent_name(&self) -> String {
        crate::invites::invite_agent_name(&self.data_dir, self.name.as_deref())
    }

    #[must_use]
    pub fn with_hub(self, hub: Option<crate::hub_client::HubClient>) -> Self {
        if let Some(client) = hub {
            let _ = self.hub.prime(client);
        }
        self
    }

    #[must_use]
    pub const fn with_task_folder_mode(mut self, enabled: bool) -> Self {
        self.task_folder_mode = enabled;
        self
    }

    pub(super) fn hub_access(&self) -> HubAccess {
        match self.hub.client() {
            Ok(Some(client)) => HubAccess::Ready(Box::new(client)),
            Ok(None) => HubAccess::Off,
            Err(_) => HubAccess::Unavailable,
        }
    }

    pub(crate) fn current_hub(&self) -> Option<crate::hub_client::HubClient> {
        match self.hub_access() {
            HubAccess::Ready(client) => Some(*client),
            HubAccess::Off | HubAccess::Unavailable => None,
        }
    }

    pub(super) fn set_hub(&self, hub: Option<crate::hub_client::HubClient>) {
        if let Some(client) = hub {
            let _ = self.hub.prime(client);
        }
    }

    pub(crate) const fn hub_sign(&self) -> &Mutex<()> {
        &self.hub_sign
    }

    pub(super) fn relay_url(&self) -> Option<String> {
        crate::settings::setting::<crate::transport::relay::ActiveRelay>(&self.data_dir)
            .unwrap_or_else(|_| {
                self.relay_url.lock().unwrap_or_else(PoisonError::into_inner).clone()
            })
    }

    pub(super) async fn set_relay_url(
        &self,
        relay: Option<String>,
    ) -> Result<(), crate::transport::TransportError> {
        crate::transport::relay::apply_relay(
            &self.data_dir,
            &relay,
            (self.switch_relay)(self.relay_url(), relay.clone()),
        )
        .await?;
        *self.relay_url.lock().unwrap_or_else(PoisonError::into_inner) = relay;
        Ok(())
    }
}

#[derive(Default)]
pub struct UiOptions {
    pub managed: bool,
    pub page: Option<Arc<PageContext>>,
    pub pact: Option<PactUi>,
}

/// What the PACT consent page needs from the node.
pub struct PactUi {
    pub consent: Arc<crate::pact::consent::PactConsent>,
    /// Set to this UI's origin once it listens, so the gateway can link to
    /// the consent page.
    pub link: Arc<crate::pact::consent::ConsentLink>,
}

pub(super) struct Page(pub Option<Arc<PageContext>>);

struct Pact(Option<Arc<crate::pact::consent::PactConsent>>);

fn build_admin_ui(
    administration: SharedAdministration,
    options: UiOptions,
    port: u16,
) -> AdminUi {
    let UiOptions { managed, page, pact } = options;
    if let Some(pact) = &pact {
        pact.link.set(format!("https://127.0.0.1:{port}"));
    }
    let sessions = Arc::new(AdminSessionSlot::new());
    // Browsers share cookies across ports, so a host and a caller page on
    // one machine would overwrite each other's session under one name.
    let token_store =
        session::cookie::CookieTokenStore::new().name(format!("session-{port}"));
    let router = TopcoatRouter::builder()
        .cookies()
        .sessions(
            SessionConfig::builder()
                .token_store(token_store)
                .lifetime(UI_SESSION_LIFETIME)
                .build(),
        )
        .layout(root_layout)
        .layer(enforce_loopback_host)
        .layer(BodyLimit::max(UI_BODY_LIMIT))
        .page(home)
        .page(agents)
        .page(advanced::advanced)
        .page(audit::audit)
        .page(advanced::confirm)
        .page(advanced::cancel)
        .page(advanced::save_policy)
        .page(host::invite)
        .page(host::change_group)
        .page(host::revoke)
        .page(host::workspace)
        .page(host::relay)
        .page(host::save_card)
        .page(listing::publish_listing)
        .page(caller::import)
        .page(caller::remove_contact)
        .page(caller::confirm_request)
        .page(caller::discard_request)
        .page(hub::claim_name)
        .page(hub::set_search)
        .page(hub::invite_by_name)
        .page(hub::accept_invite)
        .page(hub::remove_invite)
        .page(hub::block_invite)
        .page(hub::approve_request)
        .page(hub::block_request)
        .page(hub::claim_owner)
        .page(hub::confirm_owner)
        .page(hub::remove_owner)
        .page(hub::unblock)
        .page(hub::hub_url)
        .route(hub::presence)
        .page(pact::consent_page)
        .page(pact::consent_decision)
        .route(events)
        .route(admin_javascript)
        .route(stylesheet)
        .app_context(administration)
        .app_context(Managed(managed))
        .app_context(Page(page))
        .app_context(Pact(pact.map(|pact| pact.consent)))
        .app_context(AdminPort(port))
        .app_context(Arc::clone(&sessions))
        .app_context(Arc::new(AdminConfirmations::new()));
    AdminUi { router: router.build(), sessions }
}

pub async fn serve_tls_with_scope(
    port: u16,
    data_dir: &Path,
    administration: SharedAdministration,
    options: UiOptions,
) -> io::Result<()> {
    serve_tls_with_scope_and_handle(
        port,
        data_dir,
        administration,
        options,
        axum_server::Handle::new(),
    )
    .await
}

pub async fn serve_tls_with_scope_and_handle(
    port: u16,
    data_dir: &Path,
    administration: SharedAdministration,
    options: UiOptions,
    handle: axum_server::Handle<std::net::SocketAddr>,
) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let actual_port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let config =
        axum_server::tls_rustls::RustlsConfig::from_config(tls_config(data_dir)?);
    // The event comes after `build_admin_ui`, which gives the PACT gateway
    // the consent page link.
    let admin_ui = build_admin_ui(administration, options, actual_port);
    println!(
        "{}",
        serde_json::json!({
            "event": "admin_listening",
            "url": format!("https://127.0.0.1:{actual_port}/"),
        })
    );
    let app = router(UiState { service: ui_service(admin_ui.router) });
    crate::https::serve(listener, config, UI_HARDENING, handle, app).await
}

#[derive(Clone)]
struct UiState {
    service: UiService,
}

/// The outer axum router: it gives every request to the topcoat router,
/// which answers an unknown path with `404`. `crate::https::serve` adds the
/// request timeout and the body limit.
fn router(state: UiState) -> AxumRouter {
    AxumRouter::new().fallback_service(state.service)
}

fn tls_config(data_dir: &Path) -> io::Result<Arc<ServerConfig>> {
    let (certificate, key) = crate::data_dir::DataDir::new(data_dir).admin_tls()?;
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| io::Error::other(error.to_string()))?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(certificate)],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(Arc::new(config))
}

#[cfg(test)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures hold a lock only while they set up; nothing contends"
)]
mod tests {
    use std::{sync::Arc, time::Duration};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt;
    use tower::ServiceExt;

    use super::{
        AdminUi, PageContext, UiOptions, UiState, build_admin_ui, router, tls_config,
        ui_service,
    };
    use crate::admin::{
        ChangeRequest, SharedAdministration, shared_with_store,
        test_support::{FakeNode, shared_in},
    };
    use aether_store::{
        AuditActor, AuditResult, ManagedMember, NodeStore, QueueLimits, TaskInput,
    };

    const PORT: u16 = 3000;

    #[test]
    fn the_page_hub_reads_a_saved_change_without_restarting_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let _ = crate::data_dir::DataDir::new(directory.path()).endpoint_key();
        let page = PageContext::host(
            directory.path().to_path_buf(),
            None,
            None,
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            None,
        )
        .with_hub(Some(
            crate::hub_client::HubClient::for_data_dir(
                crate::hub_client::DEFAULT_HUB_URL,
                directory.path(),
            )
            .expect("hub client must create"),
        ));
        assert!(page.current_hub().is_some());
        crate::settings::store_setting::<crate::hub_client::settings::Hub>(
            directory.path(),
            &Some("off".to_owned()),
        )
        .expect("hub choice must save");
        assert!(page.current_hub().is_none());
    }

    #[test]
    fn a_store_read_error_is_not_treated_as_hub_off() {
        let directory = tempfile::tempdir().expect("directory must create");
        let _ = crate::data_dir::DataDir::new(directory.path()).endpoint_key();
        crate::settings::store_setting::<crate::hub_client::settings::Hub>(
            directory.path(),
            &Some(crate::hub_client::DEFAULT_HUB_URL.to_owned()),
        )
        .expect("store");
        std::fs::create_dir_all(directory.path()).expect("directory");
        let _ = NodeStore::in_dir(directory.path()).expect("store");
        std::fs::write(directory.path().join("node.sqlite"), b"not sqlite")
            .expect("break store");
        let page = PageContext::host(
            directory.path().to_path_buf(),
            None,
            None,
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            None,
        );
        assert!(matches!(page.hub_access(), super::HubAccess::Unavailable));
    }

    #[test]
    fn the_page_hub_uses_the_serve_flag_over_the_saved_url() {
        let directory = tempfile::tempdir().expect("directory must create");
        let _ = crate::data_dir::DataDir::new(directory.path()).endpoint_key();
        crate::settings::store_setting::<crate::hub_client::settings::Hub>(
            directory.path(),
            &Some("https://stored.example".to_owned()),
        )
        .expect("store");
        let page = PageContext::host(
            directory.path().to_path_buf(),
            None,
            None,
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            Some("https://flag.example".to_owned()),
        );
        assert_eq!(page.current_hub().expect("hub").url(), "https://flag.example");
    }

    fn managed_options() -> UiOptions {
        UiOptions { managed: true, ..UiOptions::default() }
    }

    /// A host UI over a store in `data_dir`, whose grant applier records each
    /// grant it receives instead of reaching a running node.
    fn host_ui(data_dir: &std::path::Path) -> (AdminUi, Arc<FakeNode>) {
        host_ui_with_mode(data_dir, false)
    }

    fn host_ui_with_mode(
        data_dir: &std::path::Path,
        task_folder_mode: bool,
    ) -> (AdminUi, Arc<FakeNode>) {
        host_ui_with_roles(data_dir, task_folder_mode, false)
    }

    /// An administration service whose changes reach the returned fake
    /// node, on workspace `/start`.
    fn test_administration(
        data_dir: &std::path::Path,
    ) -> (SharedAdministration, Arc<FakeNode>) {
        let administration = shared_in(data_dir);
        let node = Arc::new(FakeNode::default().with_workspace("/start"));
        administration.lock().expect("administration must lock").set_live(node.clone());
        (administration, node)
    }

    fn host_ui_with_roles(
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
    type Switches = Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>>;

    /// A host on relay.test whose relay switches are recorded in `switches`
    /// instead of reaching a node. `https://down.test` never answers.
    fn test_host(data_dir: &std::path::Path, switches: Switches) -> PageContext {
        PageContext::host(
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

    #[test]
    fn the_agent_name_comes_from_the_hub_name_then_the_invite_name_then_the_endpoint() {
        let directory = tempfile::tempdir().expect("directory must create");
        let host = |name: Option<&str>| {
            PageContext::host(
                directory.path().to_path_buf(),
                name.map(str::to_owned),
                None,
                Arc::new(|_, _| Box::pin(async { Ok(()) })),
                None,
            )
        };
        let endpoint =
            crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
        assert_eq!(
            host(None).agent_name(),
            crate::direct::default_agent_name(&endpoint)
        );
        assert!(host(None).agent_name().starts_with("agent-"));
        assert_eq!(host(None).agent_name().len(), "agent-".len() + 8);
        assert_eq!(host(Some("server")).agent_name(), "server");
        crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            directory.path(),
            &Some("tudor#0042".to_owned()),
        )
        .expect("hub name must store");
        assert_eq!(host(Some("server")).agent_name(), "tudor#0042");
        assert_eq!(host(None).agent_name(), "tudor#0042");
    }

    /// Waits until the hub invite `id` is delivered, or fails the test.
    async fn wait_for_delivery(hub: &crate::hub_client::test_hub::TestHub, id: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let delivered = hub
                    .state()
                    .invites
                    .iter()
                    .any(|invite| invite.id == id && invite.status == "delivered");
                if delivered {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the approved invite must be delivered");
    }

    /// Keeps the local approval of the hub invite `id` to `peer`.
    fn approve(data_dir: &std::path::Path, id: &str, peer: &str) {
        NodeStore::in_dir(data_dir)
            .expect("store must open")
            .put_hub_approval(&aether_store::HubApproval {
                invite_id: id.to_owned(),
                side: aether_store::ApprovalSide::Sender,
                kind: aether_store::ApprovalKind::Invite,
                peer_name: peer.to_owned(),
                peer_endpoint: None,
                days: 7,
                access_group: "friends".to_owned(),
                bundle: None,
                created_at: 1,
                expires_at: i64::MAX,
                listing: None,
            })
            .expect("approval must store");
    }

    fn peer_endpoint(seed: u8) -> String {
        iroh::SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    #[tokio::test]
    async fn a_hub_row_the_owner_never_sent_gets_no_grant() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (administration, node) = test_administration(directory.path());
        let host =
            test_host(directory.path(), Arc::new(std::sync::Mutex::new(Vec::new())))
                .with_hub(Some(hub.client(directory.path())));
        crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            directory.path(),
            &Some("tudor#0042".to_owned()),
        )
        .expect("hub name must store");
        let own =
            crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
        let (ana, eve) = (peer_endpoint(5), peer_endpoint(6));
        let (unsent, sent) = {
            let mut state = hub.state();
            state.add_account(&own, "tudor#0042");
            state.add_account(&ana, "ana#0007");
            state.add_account(&eve, "eve#0008");
            (
                state.add_invite(&own, &eve, "accepted"),
                state.add_invite(&own, &ana, "accepted"),
            )
        };
        approve(directory.path(), &sent, "ana#0007");
        let worker = tokio::spawn(crate::hub_workers::host_worker(
            administration,
            Arc::new(host),
        ));
        wait_for_delivery(&hub, &sent).await;
        worker.abort();

        let applied = node.granted();
        assert_eq!(applied.len(), 1);
        let grant =
            crate::identity::decode_grant(&applied[0]).expect("grant must parse");
        assert_eq!(grant.subject_endpoint_id.as_str(), ana);
        let state = hub.state();
        let unsent = state.invites.iter().find(|invite| invite.id == unsent);
        assert_eq!(unsent.map(|invite| invite.status.as_str()), Some("accepted"));
        assert_eq!(unsent.and_then(|invite| invite.bundle.as_deref()), None);
    }

    /// A line that the host in `host_dir` signed as `agent` for `caller`.
    fn signed_line(host_dir: &std::path::Path, agent: &str, caller: &str) -> String {
        let access = crate::identity::AccessInput {
            issuer_agent_id: "node".to_owned(),
            subject_agent_id: "ana".to_owned(),
            subject_endpoint_id: caller.to_owned(),
            scope_id: crate::direct::SCOPE.to_owned(),
            capabilities: vec![crate::execution::GENERAL_SKILL.to_owned()],
            workspaces: vec!["workspace".to_owned()],
            access_mode: aether_core::AccessMode::ReadOnly,
            approval_rule: aether_core::ApprovalRule::Automatic,
            lifetime: std::time::Duration::from_secs(60),
        };
        let (claim, _) = crate::identity::sign_access(
            &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
            &access,
        )
        .expect("access must sign");
        let authorization =
            crate::transport::A2aAuthorization::for_access(&access, &claim);
        crate::direct::InvitationBundle {
            format: crate::direct::BUNDLE_FORMAT_V1.to_owned(),

            agent_card: None,

            agent_card_signature: None,
            invitation: crate::direct::create_invitation(
                host_dir,
                agent,
                std::time::Duration::from_secs(60),
            )
            .expect("invitation must sign"),
            authorization,
            relay_url: None,
        }
        .encode()
        .expect("the line must encode")
    }

    #[tokio::test]
    async fn an_accepted_invite_imports_only_a_bundle_from_the_sender_endpoint() {
        let caller_dir = tempfile::tempdir().expect("directory must create");
        let host_dir = tempfile::tempdir().expect("directory must create");
        let mallory_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let client = hub.client(caller_dir.path());
        let caller = client.endpoint_id();
        let host =
            crate::direct::endpoint_id(host_dir.path()).expect("endpoint must load");
        let mallory =
            crate::direct::endpoint_id(mallory_dir.path()).expect("endpoint must load");
        let (honest, lying) = {
            let mut state = hub.state();
            state.add_account(&caller, "ana");
            state.add_account(&host, "tudor");
            state.add_account(&mallory, "eve");
            (
                state.add_invite(&host, &caller, "pending"),
                state.add_invite(&mallory, &caller, "pending"),
            )
        };
        crate::hub_membership::accept(&client, caller_dir.path(), &honest)
            .await
            .expect("accept");
        crate::hub_membership::accept(&client, caller_dir.path(), &lying)
            .await
            .expect("accept");
        {
            let mut state = hub.state();
            for invite in &mut state.invites {
                invite.status = "delivered".to_owned();
                // Mallory's line names another agent than the row.
                invite.bundle = Some(if invite.id == honest {
                    signed_line(host_dir.path(), "tudor", &caller)
                } else {
                    signed_line(mallory_dir.path(), "tudor", &caller)
                });
            }
        }
        let context = Arc::new(
            PageContext::admin_caller(caller_dir.path().to_path_buf())
                .with_hub(Some(client)),
        );
        let worker = tokio::spawn(crate::hub_workers::caller_worker(context));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !hub.state().invites.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the worker must handle both rows");
        worker.abort();

        let contacts = crate::direct::contact_bundles(
            &crate::direct::open_store(caller_dir.path()).expect("store must open"),
        )
        .expect("contacts");
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].invitation.agent_id, "tudor");
        assert_eq!(contacts[0].invitation.endpoint_id, host);
    }

    /// A host page whose hub is `hub`.
    fn hub_host_ui(
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
            Arc::clone(&page),
        ));
        let options = UiOptions { page: Some(page), ..UiOptions::default() };
        app(build_admin_ui(administration, options, PORT))
    }

    #[tokio::test]
    async fn the_host_page_claims_a_name_and_shows_the_full_name() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_host_ui(directory.path(), &hub);
        let cookie = session(&page).await;
        let claimed =
            post(page.clone(), "/hub/name", Some(&cookie), "name=tudor").await;
        assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        assert!(claimed.body.contains("tudor#0001"), "{}", claimed.body);
        assert!(claimed.body.contains("bob#0042"));
        assert_eq!(
            crate::settings::setting::<crate::hub_client::settings::HubName>(
                directory.path()
            )
            .ok()
            .flatten()
            .as_deref(),
            Some("tudor#0001")
        );
    }

    /// A caller page, with no host, whose hub is `hub`.
    fn hub_caller_ui(
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

    #[tokio::test]
    async fn a_host_without_a_name_chooses_personal_or_service() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_host_ui(directory.path(), &hub);
        let home = get(page.clone(), "/").await;
        assert!(home.body.contains("Personal agent: it stands for you."));
        assert!(home.body.contains("Service agent: it offers a service"));
        let cookie = session(&page).await;
        let claimed =
            post(page.clone(), "/hub/name", Some(&cookie), "name=law&kind=service")
                .await;
        assert!(claimed.body.contains("This is a service agent."), "{}", claimed.body);
        assert!(!claimed.body.contains("Their name on Aether"));
        assert_eq!(
            crate::settings::setting::<crate::hub_client::settings::AgentKind>(
                directory.path()
            )
            .ok()
            .flatten(),
            Some(crate::hub_client::AgentKind::Service)
        );
        let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
        assert_eq!(
            hub.state().kinds.get(&endpoint).copied(),
            Some(crate::hub_client::AgentKind::Service)
        );
    }

    #[tokio::test]
    async fn a_caller_only_page_offers_only_a_personal_agent() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_caller_ui(directory.path(), &hub);
        let home = get(page.clone(), "/").await;
        assert!(home.body.contains("Choose your name on Aether"));
        assert!(!home.body.contains("Service agent"));
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=service").await;
        let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
        assert_eq!(
            hub.state().kinds.get(&endpoint).copied(),
            Some(crate::hub_client::AgentKind::Personal)
        );
    }

    #[tokio::test]
    async fn a_personal_agent_turns_name_search_on_and_off() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_host_ui(directory.path(), &hub);
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=tudor&kind=personal")
            .await;
        let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
        let on =
            post(page.clone(), "/hub/searchable", Some(&cookie), "searchable=on").await;
        assert_eq!(on.status, StatusCode::OK, "{}", on.body);
        assert!(hub.state().searchable.contains(&endpoint));
        post(page.clone(), "/hub/searchable", Some(&cookie), "").await;
        assert!(!hub.state().searchable.contains(&endpoint));

        let service_dir = tempfile::tempdir().expect("directory must create");
        let service = hub_host_ui(service_dir.path(), &hub);
        let cookie = session(&service).await;
        post(service.clone(), "/hub/name", Some(&cookie), "name=law&kind=service")
            .await;
        let refused =
            post(service.clone(), "/hub/searchable", Some(&cookie), "searchable=on")
                .await;
        assert!(
            refused.body.contains("Only a personal agent can be found by name search.")
        );
        let service_endpoint =
            crate::direct::endpoint_id(service_dir.path()).expect("endpoint");
        assert!(!hub.state().searchable.contains(&service_endpoint));
    }

    /// A host page for a service agent `law#0001` on `hub`, with the first
    /// node policy in `<directory>/node`.
    async fn service_page(
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

    #[tokio::test]
    async fn a_service_agent_lists_its_card_text() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        let home = get_with(page.clone(), "/", &cookie).await;
        assert!(home.body.contains("Public listing"), "{}", home.body);
        assert!(home.body.contains("People see this"));
        assert!(home.body.contains("Not listed."));
        assert!(home.body.contains("1 waiting task in all and 1 for each person"));
        assert!(home.body.contains("each computer can see the public IP address"));

        let listed =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=list").await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert!(listed.body.contains("Stop listing"));
        let endpoint = crate::direct::endpoint_id(&data_dir).expect("endpoint");
        let held = hub.state().listings.get(&endpoint).cloned().expect("listed");
        let text = held["listing"].as_str().expect("text");
        let expires_at = held["expiresAt"].as_u64().expect("expiry");
        let now = aether_core::time::unix_seconds(std::time::SystemTime::now())
            .expect("the clock must be after 1970");
        crate::listing::verify(
            text,
            held["signature"].as_str().expect("signature"),
            &endpoint,
            expires_at,
            now,
        )
        .expect("the held listing must verify");
        assert!(expires_at > now + 29 * 24 * 60 * 60);
        let listing = crate::listing::parse(text, "law#0001").expect("listing");
        assert_eq!(
            listing.description,
            crate::agent_card::settings(&data_dir).description
        );
        let state = crate::settings::setting::<crate::listing::settings::ListingState>(
            &data_dir,
        )
        .ok()
        .flatten()
        .expect("state");
        assert_eq!(state.listing, text);

        let stopped =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=stop").await;
        assert!(stopped.body.contains("Not listed."), "{}", stopped.body);
        assert!(!hub.state().listings.contains_key(&endpoint));
        assert_eq!(
            crate::settings::setting::<crate::listing::settings::ListingState>(
                &data_dir
            )
            .ok()
            .flatten(),
            None
        );
    }

    #[tokio::test]
    async fn the_queue_text_shows_the_limits_in_force() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        NodeStore::in_dir(&data_dir)
            .expect("store must open")
            .apply_change(aether_store::ChangeKind::Limit, "queue_global", "3")
            .expect("limit must store");
        let home = get_with(page, "/", &cookie).await;
        assert!(
            home.body.contains("3 waiting tasks in all and 1 for each person"),
            "{}",
            home.body
        );
    }

    /// The agent IDs in the access group `group` of `data_dir`.
    fn group_members(data_dir: &std::path::Path, group: &str) -> Vec<String> {
        NodeStore::in_dir(data_dir)
            .expect("store must open")
            .access_groups()
            .expect("groups must read")
            .into_iter()
            .find(|entry| entry.name == group)
            .map(|entry| {
                entry.members.into_iter().map(|member| member.agent_id).collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn approving_a_request_grants_access_in_the_requests_group() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
        let ana = peer_endpoint(5);
        let id = {
            let mut state = hub.state();
            state.add_account(&ana, "ana#0001");
            state.add_request(&service, &ana, 7)
        };
        let home = get_with(page.clone(), "/", &cookie).await;
        assert!(home.body.contains("asks for access for 7 days"), "{}", home.body);
        assert!(home.body.contains(&ana.chars().take(4).collect::<String>()));
        assert!(!home.body.contains("data-hub-waiting"));

        // The form has the days only: the group is always requests.
        let with_group = post(
            page.clone(),
            "/hub/approve",
            Some(&cookie),
            &format!("id={id}&days=7&access_group=friends"),
        )
        .await;
        assert_ne!(with_group.status, StatusCode::OK);
        let store = NodeStore::in_dir(&data_dir).expect("store");
        assert_eq!(store.hub_approval(&id).expect("approval must read"), None);

        let approved = post(
            page.clone(),
            "/hub/approve",
            Some(&cookie),
            &format!("id={id}&days=7"),
        )
        .await;
        assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
        let approval =
            store.hub_approval(&id).expect("approval must read").expect("approval");
        assert_eq!(approval.kind, aether_store::ApprovalKind::Request);
        assert_eq!(approval.access_group, "requests");
        assert_eq!(approval.peer_endpoint.as_deref(), Some(ana.as_str()));
        wait_for_delivery(&hub, &id).await;
        assert_eq!(group_members(&data_dir, "requests"), ["ana#0001"]);
        assert!(!group_members(&data_dir, "friends").contains(&"ana#0001".to_owned()));
        // Only the requester gets a hub contact.
        assert!(!hub.state().contacts.contains(&(service, ana)));
    }

    #[tokio::test]
    async fn blocking_a_request_removes_it_and_keeps_the_block() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, data_dir) = service_page(directory.path(), &hub).await;
        let service = crate::direct::endpoint_id(&data_dir).expect("endpoint");
        let ana = peer_endpoint(5);
        let id = {
            let mut state = hub.state();
            state.add_account(&ana, "ana#0001");
            state.add_request(&service, &ana, 7)
        };
        let blocked = post(
            page.clone(),
            "/hub/block-request",
            Some(&cookie),
            &format!("id={id}"),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::OK, "{}", blocked.body);
        assert!(blocked.body.contains("Blocked"));
        assert!(!hub.state().invites.iter().any(|invite| invite.id == id));
        assert!(hub.state().blocks.contains(&(service.clone(), ana.clone())));
        let store = NodeStore::in_dir(&data_dir).expect("store");
        assert!(store.hub_blocked(&ana).expect("block must read"));

        // A hub that sends an accepted row for the blocked key gets no line,
        // even with an approval for it.
        let forged = {
            let mut state = hub.state();
            let forged = state.add_request(&service, &ana, 7);
            if let Some(invite) =
                state.invites.iter_mut().find(|invite| invite.id == forged)
            {
                invite.status = "accepted".to_owned();
            }
            forged
        };
        store
            .put_hub_approval(&aether_store::HubApproval {
                invite_id: forged.clone(),
                side: aether_store::ApprovalSide::Sender,
                kind: aether_store::ApprovalKind::Request,
                peer_name: "ana#0001".to_owned(),
                peer_endpoint: Some(ana.clone()),
                days: 7,
                access_group: "requests".to_owned(),
                bundle: None,
                created_at: 1,
                expires_at: i64::MAX,
                listing: None,
            })
            .expect("approval must store");
        // A change made in the state wakes no long poll; any request does.
        let other = tempfile::tempdir().expect("directory must create");
        let _ = hub.client(other.path()).account().await;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while hub.state().invites.iter().any(|invite| invite.id == forged) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the worker must remove the row");
        assert!(group_members(&data_dir, "requests").is_empty());

        let unblocked = post(
            page.clone(),
            "/hub/unblock",
            Some(&cookie),
            &format!("endpoint={ana}"),
        )
        .await;
        assert_eq!(unblocked.status, StatusCode::OK, "{}", unblocked.body);
        assert!(!store.hub_blocked(&ana).expect("block must read"));
    }

    /// Lists the service agent in `host_dir` on `hub` as `handle`, and
    /// returns its search row as a stranger gets it.
    fn listed_service(
        hub: &crate::hub_client::test_hub::TestHub,
        host_dir: &std::path::Path,
        handle: &str,
    ) -> crate::hub_client::AgentRow {
        let bytes = crate::listing::listing_bytes(
            &crate::agent_card::CardSettings::default(),
            handle,
        );
        hub.add_listing(host_dir, host_dir, handle, &bytes)
    }

    #[tokio::test]
    async fn confirming_a_request_sends_it_and_records_the_approval() {
        let caller_dir = tempfile::tempdir().expect("directory must create");
        let service_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_caller_ui(caller_dir.path(), &hub);
        let me = crate::direct::endpoint_id(caller_dir.path()).expect("endpoint");
        hub.state().add_account(&me, "ana#0001");
        let row = listed_service(&hub, service_dir.path(), "law#0042");
        let draft =
            crate::access_requests::draft(caller_dir.path(), "law#0042", &row, 7)
                .expect("the request must draft");
        let cookie = session(&page).await;
        let home = get_with(page.clone(), "/", &cookie).await;
        for text in [
            "Requests your agent wants to send",
            "Requests to confirm",
            "law#0042",
            "The agent&#x27;s owner wrote this. Aether did not check it.",
            "each computer can see the public IP address of the other",
        ] {
            assert!(
                home.body.contains(text)
                    || home.body.contains(&text.replace("&#x27;", "'")),
                "{text}: {}",
                home.body
            );
        }
        assert!(!hub.requests().iter().any(|request| request.target == "/v1/requests"));

        let confirmed = post(
            page.clone(),
            "/requests/confirm",
            Some(&cookie),
            &format!("id={}&days=30", draft.id),
        )
        .await;
        assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
        assert!(
            confirmed.body.contains("Waiting for law#0042 to approve"),
            "{}",
            confirmed.body
        );
        let service = crate::direct::endpoint_id(service_dir.path()).expect("endpoint");
        let invite = hub
            .state()
            .invites
            .iter()
            .find(|invite| invite.receiver == me)
            .cloned()
            .expect("the hub must have the request");
        assert_eq!(
            (
                invite.sender.as_str(),
                invite.status.as_str(),
                invite.origin.as_str(),
                invite.days
            ),
            (service.as_str(), "requested", "request", 30)
        );
        let store = NodeStore::in_dir(caller_dir.path()).expect("store");
        let approval = store
            .hub_approval(&invite.id)
            .expect("approval must read")
            .expect("approval");
        assert_eq!(approval.side, aether_store::ApprovalSide::Receiver);
        assert_eq!(approval.kind, aether_store::ApprovalKind::Request);
        assert_eq!(approval.peer_name, "law#0042");
        assert_eq!(approval.listing, Some(draft.listing));
        assert!(store.access_request_drafts(0).expect("drafts").is_empty());
    }

    #[tokio::test]
    async fn a_sixth_open_request_shows_the_limit_text() {
        let caller_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_caller_ui(caller_dir.path(), &hub);
        let me = crate::direct::endpoint_id(caller_dir.path()).expect("endpoint");
        hub.state().add_account(&me, "ana#0001");
        let services: Vec<tempfile::TempDir> = (0..6)
            .map(|_| tempfile::tempdir().expect("directory must create"))
            .collect();
        for (index, service) in services.iter().take(5).enumerate() {
            listed_service(&hub, service.path(), &format!("law{index}#0001"));
            let endpoint =
                crate::direct::endpoint_id(service.path()).expect("endpoint");
            hub.state().add_request(&endpoint, &me, 7);
        }
        let sixth = services.last().expect("six services");
        let row = listed_service(&hub, sixth.path(), "law5#0001");
        let draft =
            crate::access_requests::draft(caller_dir.path(), "law5#0001", &row, 7)
                .expect("the request must draft");
        let cookie = session(&page).await;
        let refused = post(
            page.clone(),
            "/requests/confirm",
            Some(&cookie),
            &format!("id={}&days=7", draft.id),
        )
        .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(
            refused.body.contains("You reached a hub limit. Remove an old item first."),
            "{}",
            refused.body
        );
        assert!(!refused.body.contains("Nobody has this name"));
    }

    #[tokio::test]
    async fn discarding_a_request_sends_nothing() {
        let caller_dir = tempfile::tempdir().expect("directory must create");
        let service_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_caller_ui(caller_dir.path(), &hub);
        let me = crate::direct::endpoint_id(caller_dir.path()).expect("endpoint");
        hub.state().add_account(&me, "ana#0001");
        let row = listed_service(&hub, service_dir.path(), "law#0042");
        let draft =
            crate::access_requests::draft(caller_dir.path(), "law#0042", &row, 7)
                .expect("the request must draft");
        let cookie = session(&page).await;
        let discarded = post(
            page.clone(),
            "/requests/discard",
            Some(&cookie),
            &format!("id={}", draft.id),
        )
        .await;
        assert_eq!(discarded.status, StatusCode::OK, "{}", discarded.body);
        assert!(!discarded.body.contains("Requests your agent wants to send"));
        let store = NodeStore::in_dir(caller_dir.path()).expect("store");
        assert!(store.access_request_drafts(0).expect("drafts").is_empty());
        assert!(!hub.requests().iter().any(|request| request.target == "/v1/requests"));
        assert!(hub.state().invites.is_empty());
    }

    /// A host page for the personal agent `ana#0001` on `hub`.
    async fn personal_page(
        data_dir: &std::path::Path,
        hub: &crate::hub_client::test_hub::TestHub,
    ) -> (axum::Router, String) {
        let page = hub_host_ui(data_dir, hub);
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=personal").await;
        (page, cookie)
    }

    #[tokio::test]
    async fn a_personal_agent_claims_a_service_and_the_service_confirms() {
        let personal_dir = tempfile::tempdir().expect("directory must create");
        let service_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (personal, personal_cookie) =
            personal_page(personal_dir.path(), &hub).await;
        let (service, service_cookie, _) = service_page(service_dir.path(), &hub).await;

        let home = get_with(personal.clone(), "/", &personal_cookie).await;
        for text in [
            "Your service agents",
            "When the service agent confirms, its public listing shows your full name.",
            "Everyone can see when your service agent is online.",
            "people can guess when you are online",
        ] {
            assert!(home.body.contains(text), "{text}: {}", home.body);
        }
        let claimed = post(
            personal.clone(),
            "/hub/owner/claim",
            Some(&personal_cookie),
            "service=law%230001",
        )
        .await;
        assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        assert!(claimed.body.contains("waits for the service agent to confirm"));
        let id = {
            let state = hub.state();
            assert_eq!(state.owner_links.len(), 1);
            assert!(!state.owner_links[0].confirmed);
            state.owner_links[0].id.clone()
        };

        let claims = get_with(service.clone(), "/", &service_cookie).await;
        assert!(claims.body.contains("claims this agent."), "{}", claims.body);
        assert!(
            claims.body.contains("Confirm only a claim from your own personal agent.")
        );
        let confirmed = post(
            service.clone(),
            "/hub/owner/confirm",
            Some(&service_cookie),
            &format!("id={id}"),
        )
        .await;
        assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
        assert!(hub.state().owner_links[0].confirmed);
        assert!(confirmed.body.contains("is the owner of this agent."));
        let linked = get_with(personal.clone(), "/", &personal_cookie).await;
        assert!(linked.body.contains("law#0001 (linked)"), "{}", linked.body);
    }

    #[tokio::test]
    async fn a_personal_page_cannot_confirm_and_a_service_page_cannot_claim() {
        let personal_dir = tempfile::tempdir().expect("directory must create");
        let service_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (personal, personal_cookie) =
            personal_page(personal_dir.path(), &hub).await;
        let (service, service_cookie, _) = service_page(service_dir.path(), &hub).await;
        post(
            personal.clone(),
            "/hub/owner/claim",
            Some(&personal_cookie),
            "service=law%230001",
        )
        .await;
        let id = hub.state().owner_links[0].id.clone();

        let confirm = post(
            personal.clone(),
            "/hub/owner/confirm",
            Some(&personal_cookie),
            &format!("id={id}"),
        )
        .await;
        assert_eq!(confirm.status, StatusCode::BAD_REQUEST);
        let claim = post(
            service.clone(),
            "/hub/owner/claim",
            Some(&service_cookie),
            "service=ana%230001",
        )
        .await;
        assert_eq!(claim.status, StatusCode::BAD_REQUEST);
        let state = hub.state();
        assert_eq!(state.owner_links.len(), 1);
        assert!(!state.owner_links[0].confirmed);
    }

    #[tokio::test]
    async fn a_personal_agent_cannot_list_itself() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let page = hub_host_ui(directory.path(), &hub);
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=ana&kind=personal").await;
        assert!(
            !get_with(page.clone(), "/", &cookie).await.body.contains("Public listing")
        );
        let refused =
            post(page.clone(), "/listing/publish", Some(&cookie), "action=list").await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(hub.state().listings.is_empty());
    }

    /// A caller page named `ana` on `hub`, and a host `bob` that invited it.
    async fn invited_caller(
        data_dir: &std::path::Path,
        bob_dir: &std::path::Path,
        hub: &crate::hub_client::test_hub::TestHub,
    ) -> (axum::Router, String, crate::hub_client::HubClient, String) {
        let page = hub_caller_ui(data_dir, hub);
        let cookie = session(&page).await;
        post(page.clone(), "/hub/name", Some(&cookie), "name=ana").await;
        let bob = hub.client(bob_dir);
        bob.claim("bob", None).await.expect("bob must claim");
        bob.invite("ana#0001", 7).await.expect("bob must invite");
        (page, cookie, bob, crate::direct::endpoint_id(bob_dir).expect("endpoint"))
    }

    fn received_invite(
        hub: &crate::hub_client::test_hub::TestHub,
        from: &str,
    ) -> String {
        hub.state()
            .invites
            .iter()
            .find(|invite| invite.sender == from)
            .map(|invite| invite.id.clone())
            .expect("the invite must exist")
    }

    #[tokio::test]
    async fn a_blocked_invite_sender_and_its_new_invites_do_not_show() {
        let directory = tempfile::tempdir().expect("directory must create");
        let bob_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, bob, bob_endpoint) =
            invited_caller(directory.path(), bob_dir.path(), &hub).await;
        let wanted = "bob#0001 wants to share their agent with you";
        assert!(get_with(page.clone(), "/", &cookie).await.body.contains(wanted));

        let id = received_invite(&hub, &bob_endpoint);
        let blocked =
            post(page.clone(), "/hub/block", Some(&cookie), &format!("id={id}")).await;
        assert!(!blocked.body.contains(wanted), "{}", blocked.body);
        assert!(blocked.body.contains("Blocked"));
        assert!(blocked.body.contains("Unblock"));
        bob.invite("ana#0001", 7).await.expect("bob must invite again");
        assert!(!get_with(page.clone(), "/", &cookie).await.body.contains(wanted));

        post(
            page.clone(),
            "/hub/unblock",
            Some(&cookie),
            &format!("endpoint={bob_endpoint}"),
        )
        .await;
        bob.invite("ana#0001", 7).await.expect("bob must invite after the unblock");
        assert!(get_with(page.clone(), "/", &cookie).await.body.contains(wanted));
    }

    #[tokio::test]
    async fn a_full_block_list_shows_the_limit_message() {
        let directory = tempfile::tempdir().expect("directory must create");
        let bob_dir = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (page, cookie, _, bob_endpoint) =
            invited_caller(directory.path(), bob_dir.path(), &hub).await;
        hub.state().block_limit = Some(0);
        let id = received_invite(&hub, &bob_endpoint);
        let refused =
            post(page.clone(), "/hub/block", Some(&cookie), &format!("id={id}")).await;
        assert!(
            refused.body.contains("You reached a hub limit. Remove an old item first."),
            "{}",
            refused.body
        );
    }

    #[tokio::test]
    async fn an_old_hub_shows_the_older_hub_text_and_gets_no_claim() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        hub.state().old = true;
        let page = hub_host_ui(directory.path(), &hub);
        let cookie = session(&page).await;
        let claimed =
            post(page.clone(), "/hub/name", Some(&cookie), "name=tudor").await;
        assert!(
            claimed.body.contains("This hub is older than this node."),
            "{}",
            claimed.body
        );
        assert!(
            !hub.requests()
                .iter()
                .any(|request| request.method == "PUT"
                    && request.target == "/v1/account")
        );
    }

    #[tokio::test]
    async fn a_host_with_an_invite_name_still_signs_its_own_hub_invite() {
        let directory = tempfile::tempdir().expect("directory must create");
        let hub = crate::hub_client::test_hub::TestHub::start().await;
        let (administration, node) = test_administration(directory.path());
        let host = PageContext::host(
            directory.path().to_path_buf(),
            Some("server".to_owned()),
            None,
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            None,
        )
        .with_hub(Some(hub.client(directory.path())));
        crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            directory.path(),
            &Some("tudor#0042".to_owned()),
        )
        .expect("hub name must store");
        let own =
            crate::direct::endpoint_id(directory.path()).expect("endpoint must load");
        let ana = peer_endpoint(5);
        let id = {
            let mut state = hub.state();
            state.add_account(&own, "tudor#0042");
            state.add_account(&ana, "ana#0007");
            state.add_invite(&own, &ana, "accepted")
        };
        approve(directory.path(), &id, "ana#0007");
        let worker = tokio::spawn(crate::hub_workers::host_worker(
            administration,
            Arc::new(host),
        ));
        wait_for_delivery(&hub, &id).await;
        worker.abort();

        assert_eq!(node.granted().len(), 1);
        let line = hub
            .state()
            .invites
            .iter()
            .find(|invite| invite.id == id)
            .and_then(|invite| invite.bundle.clone())
            .expect("the line must be delivered");
        let bundle =
            crate::direct::InvitationBundle::decode(&line).expect("bundle must decode");
        let invitation =
            bundle.invitation.to_invitation().expect("invitation must verify");
        assert_eq!(invitation.agent_id, "tudor#0042");
        assert_eq!(bundle.authorization.claim.subject_endpoint_id, ana);
    }

    #[tokio::test]
    async fn the_owner_edits_the_agent_card_on_the_host_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let cookie = session(&application).await;
        let home = get_with(application.clone(), "/", &cookie).await;
        assert!(home.body.contains("Your agent's card"));
        assert!(home.body.contains("It is never public."));
        let saved = post(
            application.clone(),
            "/card",
            Some(&cookie),
            "description=Legal+help.&skill_name=Legal+questions&skill_description=Answers+one+question.&tags=law%2C+contracts",
        )
        .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        assert!(saved.body.contains("Card version 2."));
        let card = crate::agent_card::settings(directory.path());
        assert_eq!(card.tags, ["law", "contracts"]);

        let refused = post(
            application.clone(),
            "/card",
            Some(&cookie),
            "description=&skill_name=Legal+questions&skill_description=Answers.&tags=law",
        )
        .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(refused.body.contains("role=\"alert\""));
        assert_eq!(crate::agent_card::settings(directory.path()), card);
    }

    #[tokio::test]
    async fn the_host_sets_the_relay_from_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let switches = Arc::new(std::sync::Mutex::new(Vec::new()));
        let options = UiOptions {
            page: Some(Arc::new(test_host(directory.path(), Arc::clone(&switches)))),
            ..UiOptions::default()
        };
        let application =
            app(build_admin_ui(shared_in(directory.path()), options, PORT));
        let cookie = session(&application).await;
        let relay = |value: &str| {
            let application = application.clone();
            let cookie = cookie.clone();
            let form = form_value("relay_url", value);
            async move { post(application, "/relay", Some(&cookie), &form).await }
        };

        let response = relay("https://relay.example.com/").await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body.contains("Reachable through relay.example.com"));
        assert_eq!(
            crate::settings::setting::<crate::transport::relay::Relay>(
                directory.path()
            )
            .ok()
            .flatten()
            .as_deref(),
            Some("https://relay.example.com/")
        );

        // A relay that does not answer leaves the node where it was.
        let response = relay("https://down.test").await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("The relay did not answer."));
        assert!(response.body.contains("Reachable through relay.example.com"));

        let response = relay("not a url").await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This is not a relay URL."));

        // Empty means the local network only, and nothing to start with.
        let response = relay("").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("No relay yet"));
        assert_eq!(
            crate::settings::setting::<crate::transport::relay::Relay>(
                directory.path()
            )
            .ok()
            .flatten(),
            None
        );

        assert_eq!(
            switches.lock().expect("switches must lock").as_slice(),
            [
                (
                    Some("https://relay.test".to_owned()),
                    Some("https://relay.example.com/".to_owned())
                ),
                (
                    Some("https://relay.example.com/".to_owned()),
                    Some("https://down.test".to_owned())
                ),
                (Some("https://relay.example.com/".to_owned()), None),
            ]
        );
    }

    /// The `value` of the input with `id`, with HTML escapes undone.
    fn input_value(body: &str, id: &str) -> String {
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

    #[tokio::test]
    async fn an_invitation_applies_a_grant_and_the_host_sees_the_agent_answer() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Invite someone"));
        // Each field's label names its control.
        assert!(home.body.contains("for=\"endpoint_id\""));
        assert!(!home.body.contains("r#for"));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let friend = iroh::SecretKey::generate().public().to_string();
        // A code copied from the caller's page keeps its groups of four.
        let grouped = crate::admin_components::grouped(&friend);
        let response = post(
            application.clone(),
            "/invite",
            Some(cookie),
            &format!("friend_name=ana&{}&days=7", form_value("endpoint_id", &grouped)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let line = input_value(&response.body, "invite-line");
        assert_eq!(
            input_value(&response.body, "invite-command"),
            format!("aether node direct import '{line}'")
        );
        let bundle =
            crate::direct::InvitationBundle::decode(&line).expect("bundle must decode");
        assert_eq!(bundle.format, crate::direct::BUNDLE_FORMAT_V2);
        assert_eq!(bundle.relay_url.as_deref(), Some("https://relay.test"));
        let invitation =
            bundle.invitation.to_invitation().expect("invitation must verify");
        assert_eq!(invitation.agent_id, "tudor");
        let card = crate::agent_card::validate(
            bundle.agent_card.as_deref().expect("a v2 line has a card").as_bytes(),
            &invitation.agent_id,
            &invitation.endpoint_id,
        )
        .expect("the card must be valid");
        assert_eq!(card.name, invitation.agent_id);
        assert_eq!(bundle.authorization.caller_agent_id, "ana");
        assert_eq!(bundle.authorization.capability_id, crate::execution::GENERAL_SKILL);
        assert_eq!(bundle.authorization.claim.subject_endpoint_id, friend);
        let issuer = crate::data_dir::DataDir::new(directory.path())
            .issuer_key()
            .expect("issuer key must load");
        {
            let applied = node.granted();
            let grant = crate::identity::decode_grant(
                applied.first().expect("the grant must reach the node"),
            )
            .expect("grant must parse");
            assert_eq!(grant.signing_key, issuer.verifying_key().to_bytes());
            assert_eq!(grant.subject_endpoint_id.as_str(), friend);
            assert_eq!(
                grant.capabilities,
                std::collections::BTreeSet::from([aether_core::CapabilityId::new(
                    crate::execution::GENERAL_SKILL
                )])
            );
        }
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        assert_eq!(
            store
                .applied_changes(aether_store::ChangeKind::Grant)
                .expect("grants must read")
                .len(),
            1
        );
        store
            .put_scope(crate::direct::SCOPE, aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("greeting"),
                        id: aether_core::RequestId::new("greeting-request"),
                        caller_agent_id: aether_core::AgentId::new("ana"),
                        caller_endpoint_id: aether_core::EndpointId::new(friend),
                        scope_id: aether_core::ScopeId::new(crate::direct::SCOPE),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "greeting-key".to_owned(),
                    request_fingerprint: "greeting-fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "Greet Bob".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must admit");
        store.lease_next("worker", "execution", i64::MAX).expect("task must lease");
        store.mark_running("greeting", "execution").expect("task must run");
        store
            .mark_succeeded("greeting", "execution", "Hello, Bob. <ready>", i64::MAX)
            .expect("task must finish");
        let response = get(application, "/").await;
        assert!(response.body.contains("Read answer"));
        assert!(response.body.contains("Greet Bob"));
        assert!(response.body.contains("Hello, Bob. &lt;ready&gt;"));
    }

    #[tokio::test]
    async fn a_revoked_friend_leaves_the_list_and_the_revocation_is_stored() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Nobody has access."));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let friend = iroh::SecretKey::generate().public().to_string();
        let invited = post(
            application.clone(),
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={friend}&days=7"),
        )
        .await;
        assert!(
            invited.body.contains("aria-label=\"Revoke ana\""),
            "ana must be listed"
        );
        assert!(invited.body.contains("No tasks yet"));
        let grant_id = crate::identity::decode_grant(
            node.granted().first().expect("grant must exist"),
        )
        .expect("grant must parse")
        .id;
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        // The first click only asks.
        let asked = post(
            application.clone(),
            "/revoke",
            Some(&cookie),
            &format!("grant_id={}", grant_id.as_str()),
        )
        .await;
        assert_eq!(asked.status, StatusCode::OK);
        assert!(asked.body.contains("Revoke ana's access?"), "{}", asked.body);
        assert_eq!(store.grant_revoked(grant_id.as_str()), Ok(false));
        let response = post(
            application,
            "/revoke",
            Some(&cookie),
            &format!("grant_id={}&confirm=yes", grant_id.as_str()),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Nobody has access."));
        assert_eq!(store.grant_revoked(grant_id.as_str()), Ok(true));
        assert!(
            store
                .applied_changes(aether_store::ChangeKind::Grant)
                .expect("grants")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_host_changes_the_agent_workspace() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("value=\"/start\""));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let project = directory.path().join("project");
        std::fs::create_dir_all(&project).expect("project must create");
        let project = project.to_str().expect("path must be UTF-8");
        let response = post(
            application.clone(),
            "/workspace",
            Some(&cookie),
            &form_value("path", project),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains(project), "the page must show the new path");
        let missing = directory.path().join("missing");
        let response = post(
            application,
            "/workspace",
            Some(&cookie),
            &form_value("path", missing.to_str().expect("path must be UTF-8")),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn task_folder_page_refuses_folder_changes() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui_with_mode(directory.path(), true);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Each task runs in a new empty folder"));
        assert!(!home.body.contains("Change folder"));
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let response = post(
            application,
            "/workspace",
            Some(&cookie),
            &form_value("path", directory.path().to_str().expect("path must be UTF-8")),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn one_node_shows_host_and_caller_pages_for_the_same_data_directory() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui_with_roles(directory.path(), true, true);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Who can use your agent"));
        assert!(home.body.contains("Incoming access groups"));
        assert!(home.body.contains("friends"));
        assert!(home.body.contains("href=\"/agents\""));
        let agents = get(application, "/agents").await;
        assert_eq!(agents.status, StatusCode::OK);
        assert!(agents.body.contains("Connect to a friend's agent"));
        assert!(agents.body.contains("href=\"/\""));
    }

    #[tokio::test]
    async fn host_page_creates_and_deletes_an_incoming_group() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        )
        .to_owned();
        let created = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            "action=create&group=work",
        )
        .await;
        assert_eq!(created.status, StatusCode::OK);
        assert!(created.body.contains("work"));
        let endpoint = iroh::SecretKey::generate().public().to_string();
        let invited = post(
            application.clone(),
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={endpoint}&days=7"),
        )
        .await;
        assert_eq!(invited.status, StatusCode::OK);
        let added = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            &format!("action=add&group=work&endpoint_id={endpoint}"),
        )
        .await;
        assert_eq!(added.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .any(|group| group.name == "work"
                    && group.members.iter().any(|member| member.agent_id == "ana"))
        );
        let removed = post(
            application.clone(),
            "/groups",
            Some(&cookie),
            &format!("action=remove&group=work&agent=ana&endpoint_id={endpoint}"),
        )
        .await;
        assert_eq!(removed.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .any(|group| group.name == "work" && group.members.is_empty())
        );
        let deleted =
            post(application, "/groups", Some(&cookie), "action=delete&group=work")
                .await;
        assert_eq!(deleted.status, StatusCode::OK);
        assert!(
            crate::access_groups::list(directory.path())
                .expect("groups must list")
                .iter()
                .all(|group| group.name != "work")
        );
    }

    #[tokio::test]
    async fn an_invitation_needs_the_session_and_a_real_endpoint_id() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, node) = host_ui(directory.path());
        let application = app(admin_ui);
        let friend = iroh::SecretKey::generate().public().to_string();
        let form = format!("friend_name=ana&endpoint_id={friend}&days=7");
        let response = post(application.clone(), "/invite", None, &form).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let response = post(
            application,
            "/invite",
            Some(cookie),
            "friend_name=ana&endpoint_id=not-a-key&days=7",
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This is not a connect code."));
        assert!(node.granted().is_empty());
    }

    #[tokio::test]
    async fn another_local_page_session_does_not_hide_this_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let own = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let other_port = PORT + 1;
        let cookies = format!("__Host-session-{other_port}=other-page-token; {own}");
        let response = post(
            application,
            "/invite",
            Some(&cookies),
            "friend_name=ana&endpoint_id=not-a-key&days=7",
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }

    /// A caller UI over `data_dir`.
    fn caller_ui(data_dir: &std::path::Path) -> AdminUi {
        let options = UiOptions {
            page: Some(Arc::new(PageContext::admin_caller(data_dir.to_path_buf()))),
            ..UiOptions::default()
        };
        build_admin_ui(shared_in(data_dir), options, PORT)
    }

    fn form_value(name: &str, value: &str) -> String {
        let mut url = reqwest::Url::parse("http://form.test/").expect("URL must parse");
        url.query_pairs_mut().append_pair(name, value);
        url.query().expect("query must exist").to_owned()
    }

    /// Opens `application`'s home page and returns its session cookie.
    async fn session(application: &axum::Router) -> String {
        let home = get(application.clone(), "/").await;
        session_cookie_pair(home.set_cookie.as_deref().expect("home must set a cookie"))
            .to_owned()
    }

    /// Creates an invitation on a fresh host for the endpoint `caller_id`.
    async fn bundle_for(caller_id: &str) -> String {
        let host_directory = tempfile::tempdir().expect("directory must create");
        let (host, _) = host_ui(host_directory.path());
        let host = app(host);
        let cookie = session(&host).await;
        let response = post(
            host,
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={caller_id}&days=7"),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        input_value(&response.body, "invite-line")
    }

    #[tokio::test]
    async fn a_caller_imports_an_invitation_and_lists_the_agent() {
        let directory = tempfile::tempdir().expect("directory must create");
        let caller = caller_ui(directory.path());
        let caller = app(caller);
        // One request for both: a page load without the cookie would start a
        // new session and end this one.
        let home = get(caller.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home must set a cookie"),
        )
        .to_owned();
        let caller_id = crate::data_dir::DataDir::new(directory.path())
            .endpoint_key()
            .expect("caller key must load")
            .public()
            .to_string();
        assert!(home.body.contains("Connect to a friend's agent"));
        // Shown in groups of four; the copy button holds it whole.
        assert!(home.body.contains(&crate::admin_components::grouped(
            caller_id.get(..32).expect("an endpoint id has 64 characters")
        )));
        assert!(home.body.contains(&format!("data-copy-text=\"{caller_id}\"")));
        let bundle = bundle_for(&caller_id).await;
        let response = post(
            caller.clone(),
            "/import",
            Some(&cookie),
            &form_value("bundle", &bundle),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body.contains("Agents you can use"));
        assert!(response.body.contains("Connect another agent"));
        assert_eq!(
            input_value(&response.body, "request-0"),
            "aether node request --scope direct --agent-id tudor --text 'Give me three ideas for a project name'"
        );
        let kept = crate::direct::contact_bundle(
            &crate::direct::open_store(directory.path()).expect("store must open"),
            "tudor",
        )
        .expect("the bundle must be kept");
        assert_eq!(kept.relay_url.as_deref(), Some("https://relay.test"));
        // The line was v2: its card is kept apart from the contact.
        let card = crate::direct::contact_card(
            &crate::direct::open_store(directory.path()).expect("store must open"),
            &kept,
        )
        .expect("the card must be kept");
        assert_eq!(card.name, "tudor");
        assert!(
            response
                .body
                .contains("An agent that does tasks for people its owner invites.")
        );
        assert!(response.body.contains("Skill: General tasks"));
    }

    #[tokio::test]
    async fn the_card_names_the_hub_name_when_the_node_has_one() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (administration, _) = test_administration(directory.path());
        let options = UiOptions {
            page: Some(Arc::new(PageContext::host(
                directory.path().to_path_buf(),
                Some("server".to_owned()),
                None,
                Arc::new(|_, _| Box::pin(async { Ok(()) })),
                None,
            ))),
            ..UiOptions::default()
        };
        crate::settings::store_setting::<crate::hub_client::settings::HubName>(
            directory.path(),
            &Some("tudor#0042".to_owned()),
        )
        .expect("hub name must store");
        let host = app(build_admin_ui(administration, options, PORT));
        let cookie = session(&host).await;
        let friend = iroh::SecretKey::generate().public().to_string();
        let response = post(
            host,
            "/invite",
            Some(&cookie),
            &format!("friend_name=ana&endpoint_id={friend}&days=7"),
        )
        .await;
        let line = input_value(&response.body, "invite-line");
        let bundle = crate::direct::InvitationBundle::decode(&line).expect("bundle");
        let card: serde_json::Value = serde_json::from_str(
            bundle.agent_card.as_deref().expect("a v2 line has a card"),
        )
        .expect("the card is JSON");
        assert_eq!(card["name"], "tudor#0042");
        assert_eq!(bundle.invitation.agent_id, "tudor#0042");
    }

    #[test]
    fn the_longest_card_keeps_the_invite_line_under_the_hub_limit() {
        let directory = tempfile::tempdir().expect("directory must create");
        crate::agent_card::save_settings(
            directory.path(),
            &crate::agent_card::CardInput {
                description: "d".repeat(crate::agent_card::MAX_DESCRIPTION),
                skill_name: "n".repeat(crate::agent_card::MAX_SKILL_NAME),
                skill_description: "s".repeat(crate::agent_card::MAX_SKILL_DESCRIPTION),
                tags: (0..crate::agent_card::MAX_TAGS)
                    .map(|index| {
                        format!("{index}{}", "t".repeat(crate::agent_card::MAX_TAG - 1))
                    })
                    .collect(),
            },
        )
        .expect("the longest card must save");
        let (administration, _) = test_administration(directory.path());
        let host =
            test_host(directory.path(), Arc::new(std::sync::Mutex::new(Vec::new())));
        let longest = format!("{}#0001", "a".repeat(32));
        let (line, _) = crate::invites::sign_direct_invite(
            &administration,
            &crate::invites::DirectInviteSigning::from_page(&host),
            &longest,
            &longest,
            &iroh::SecretKey::generate().public().to_string(),
            30,
            "friends",
            aether_store::AuditActor::Ui,
        )
        .expect("the invite must sign");
        assert!(line.len() + "bundle=".len() + 2 <= 8 * 1024, "{}", line.len());
    }

    #[tokio::test]
    async fn caller_page_shows_a_not_delivered_task_and_an_answer() {
        let host = tempfile::tempdir().expect("directory must create");
        let directory = tempfile::tempdir().expect("directory must create");
        let endpoint = crate::direct::endpoint_id(directory.path()).expect("endpoint");
        crate::direct::import_bundle(
            directory.path(),
            &crate::direct::test_bundle(host.path(), &endpoint, false),
        )
        .expect("the contact must import");
        let now = aether_core::time::unix_timestamp(std::time::SystemTime::now())
            .expect("time");
        let store = NodeStore::in_dir(directory.path()).expect("store");
        let answered = crate::outbox::enqueue(directory.path(), "tudor", "one", now)
            .expect("task");
        let lost = crate::outbox::enqueue(directory.path(), "tudor", "two", now)
            .expect("task");
        store
            .record_sent_attempt(
                &answered.task_id,
                None,
                &aether_store::SentAttempt {
                    state: aether_store::SentState::Answered,
                    reason: None,
                    host_state: None,
                    answer: Some("Three names".to_owned()),
                    last_event_sequence: 1,
                    next_attempt_at: now,
                },
                now,
            )
            .expect("record");
        store
            .end_sent_tasks_for_endpoint(
                &lost.endpoint_id,
                "the access to this agent ended",
                now,
            )
            .expect("end");
        let caller = app(caller_ui(directory.path()));
        let page = get(caller, "/").await;
        assert!(page.body.contains("Sent tasks"));
        assert!(page.body.contains("Answered"));
        assert!(page.body.contains("Three names"));
        assert!(page.body.contains("Not delivered"));
        assert!(page.body.contains("the access to this agent ended"));
    }

    #[tokio::test]
    async fn a_caller_removes_a_contact_from_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let caller = app(caller_ui(directory.path()));
        let cookie = session(&caller).await;
        let caller_id =
            crate::direct::endpoint_id(directory.path()).expect("caller key must load");
        let bundle = bundle_for(&caller_id).await;
        let imported = post(
            caller.clone(),
            "/import",
            Some(&cookie),
            &form_value("bundle", &bundle),
        )
        .await;
        assert!(imported.body.contains("Remove tudor"));
        let removed =
            post(caller.clone(), "/contacts/remove", Some(&cookie), "agent=tudor")
                .await;
        assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
        assert!(!removed.body.contains("Agents you can use"));
        assert!(
            crate::direct::contact_bundle(
                &crate::direct::open_store(directory.path()).expect("store must open"),
                "tudor"
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_caller_refuses_an_invitation_for_another_endpoint() {
        let directory = tempfile::tempdir().expect("directory must create");
        let caller = caller_ui(directory.path());
        let caller = app(caller);
        let cookie = session(&caller).await;
        let other = iroh::SecretKey::generate().public().to_string();
        let bundle = bundle_for(&other).await;
        let response = post(
            caller.clone(),
            "/import",
            Some(&cookie),
            &form_value("bundle", &bundle),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This invite is for another computer."));
        let response = post(
            caller,
            "/import",
            Some(&cookie),
            &form_value("bundle", "{\"format\":\"something-else\"}"),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(response.body.contains("This is not an Aether invite."));
        assert!(
            crate::direct::contact_bundles(
                &crate::direct::open_store(directory.path()).expect("store must open")
            )
            .expect("contacts must list")
            .is_empty()
        );
    }

    #[tokio::test]
    async fn a_ui_without_a_running_node_cannot_invite() {
        let directory = tempfile::tempdir().expect("directory must create");
        let admin_ui =
            build_admin_ui(shared_in(directory.path()), UiOptions::default(), PORT);
        let home = get(app(admin_ui), "/").await;
        assert!(!home.body.contains("Invite someone"));
        assert!(home.body.contains("Pending changes"));
    }

    /// An `AdminUi` whose `Change::Directory` applies immediately (a no-op
    /// applier), for tests that use a directory change only as a stand-in
    /// low-risk fixture rather than to test directory behavior itself. Hands
    /// back the same `SharedAdministration` the UI was built with, so a test
    /// can queue further changes after the UI (and its topcoat `Router`)
    /// already exist.
    fn admin_ui_with_directory_applier()
    -> (tempfile::TempDir, AdminUi, SharedAdministration) {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        administration.lock().expect("administration must lock").set_live(Arc::new(
            FakeNode::default().with_directory("https://directory.test"),
        ));
        let admin_ui =
            build_admin_ui(administration.clone(), UiOptions::default(), PORT);
        (directory, admin_ui, administration)
    }

    fn add_pending(administration: &SharedAdministration, id: &str, url: &str) {
        administration
            .lock()
            .expect("administration must lock")
            .submit(
                &ChangeRequest::Directory { id: id.to_owned(), url: url.to_owned() }
                    .into_change()
                    .expect("change must parse"),
                aether_store::AuditActor::Ui,
                false,
            )
            .expect("change must apply");
    }

    fn app(admin_ui: AdminUi) -> axum::Router {
        super::UI_HARDENING
            .bounded(router(UiState { service: ui_service(admin_ui.router) }))
    }

    async fn get(app: axum::Router, path: &str) -> AxumTestResponse {
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
    async fn get_with(app: axum::Router, path: &str, cookie: &str) -> AxumTestResponse {
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

    async fn post(
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
    struct AxumTestResponse {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: String,
        set_cookie: Option<String>,
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
    fn session_cookie_pair(set_cookie: &str) -> &str {
        set_cookie.split(';').next().expect("a Set-Cookie header has at least one part")
    }

    #[tokio::test]
    async fn loopback_page_sets_session_and_security_headers() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = get(app(admin_ui), "/").await;
        assert_eq!(response.status, StatusCode::OK);
        let set_cookie =
            response.set_cookie.expect("home page must set a session cookie");
        assert!(set_cookie.starts_with(&format!("__Host-session-{PORT}=")));
        assert!(set_cookie.contains("Secure"));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Lax"));
        assert_eq!(
            response.headers.get("content-security-policy").expect("CSP header"),
            "default-src 'none'; connect-src 'self'; script-src 'self'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        );
        assert_eq!(
            response.headers.get("cache-control").expect("cache-control header"),
            "no-store"
        );
        assert!(response.body.contains("src=\"/admin.js\""));
        assert!(response.body.contains("href=\"/styles.css\""));
        assert!(response.body.contains("class=\"navigation-bar"));
        assert!(response.body.contains("Skip to content"));
        for section in [
            "Agents",
            "Capabilities",
            "Directories",
            "Connections",
            "Permissions",
            "Workspaces",
            "Tasks",
            "Activity",
        ] {
            assert!(response.body.contains(section));
        }
        assert!(!response.body.contains("id=\"members\""));
    }

    #[tokio::test]
    async fn ui_cancellation_uses_the_live_node_callback() {
        let directory = tempfile::tempdir().expect("directory must create");
        let administration = shared_in(directory.path());
        let node = Arc::new(FakeNode::default());
        administration.lock().expect("administration must lock").set_live(node.clone());
        let admin_ui = build_admin_ui(administration, UiOptions::default(), PORT);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let response = post(
            application,
            "/cancel",
            Some(cookie),
            "task_id=00000000-0000-4000-8000-000000000001",
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(node.calls(), ["cancel 00000000-0000-4000-8000-000000000001"]);
    }

    #[tokio::test]
    async fn axum_router_serves_the_page_script() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = get(app(admin_ui), "/admin.js").await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.headers.get("content-type").expect("content-type header"),
            "text/javascript; charset=utf-8",
        );
        assert!(response.body.contains("EventSource"));
        assert!(response.body.contains("data-copy-text"));
    }

    #[tokio::test]
    async fn the_advanced_page_is_served_beside_a_mode_screen() {
        let directory = tempfile::tempdir().expect("directory must create");
        let (admin_ui, _) = host_ui(directory.path());
        let response = get(app(admin_ui), "/advanced").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Pending changes"));
        assert!(response.body.contains("href=\"/\""));
        assert!(response.body.contains("href=\"/advanced\" aria-current=\"page\""));
        assert!(response.body.contains("href=\"/#invite\""));
        assert!(response.body.contains("href=\"/#groups\""));
    }

    #[tokio::test]
    async fn advanced_page_shows_a_store_error_instead_of_empty_counts() {
        let (directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let db_path = directory.path().join(aether_store::NODE_DATABASE);
        rusqlite::Connection::open(&db_path)
            .expect("database must open")
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("invalid audit row must insert");
        let response = get(app(admin_ui), "/advanced").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Administration store is unavailable"));
        assert!(response.body.contains("Overview counts cannot be loaded"));
        assert!(!response.body.contains("0 known agents"));
    }

    #[tokio::test]
    async fn audit_page_shows_a_store_error_instead_of_an_empty_list() {
        let (directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let db_path = directory.path().join(aether_store::NODE_DATABASE);
        rusqlite::Connection::open(&db_path)
            .expect("database must open")
            .execute(
                "INSERT INTO audit_records(actor_type,action,result,created_at) VALUES('mcp','change','private-credential',0)",
                [],
            )
            .expect("invalid audit row must insert");
        let response = get(app(admin_ui), "/audit").await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.body.contains("Administration store is unavailable"));
        assert!(!response.body.contains("No audit records yet"));
    }

    #[tokio::test]
    async fn advanced_page_saves_task_policy_in_the_node_database() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = directory.path().join("node");
        crate::node_builder::load_or_create_service(
            &directory.path().join("missing-policy.json"),
            &data_dir,
            "codex".into(),
            None,
        )
        .expect("first node policy must create");
        let (admin_ui, _) = host_ui(&data_dir);
        let application = app(admin_ui);
        let cookie = session(&application).await;
        let mut form =
            reqwest::Url::parse("http://form.test/").expect("form URL must parse");
        form.query_pairs_mut()
            .append_pair(
                "workspace",
                &directory.path().join("tasks").display().to_string(),
            )
            .append_pair("max_context_tokens", "12000")
            .append_pair("max_seconds", "90")
            .append_pair("queue_global", "3")
            .append_pair("queue_per_peer", "2");
        let denied = post(
            application.clone(),
            "/policy",
            None,
            form.query().expect("form must encode"),
        )
        .await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        let response = post(
            application.clone(),
            "/policy",
            Some(&cookie),
            form.query().expect("form must encode"),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body.contains("Policy saved"));
        let settings = crate::policy::policy_settings(&data_dir)
            .expect("policy must read")
            .expect("policy must exist");
        assert_eq!(settings.max_context_tokens, 12_000);
        assert_eq!(settings.max_seconds, 90);
        assert_eq!(settings.queue.global, 3);
        assert_eq!(settings.queue.per_peer, 2);
    }

    #[tokio::test]
    async fn axum_router_serves_the_compiled_stylesheet() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = get(app(admin_ui), "/styles.css").await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.headers.get("content-type").expect("content-type header"),
            "text/css; charset=utf-8",
        );
        assert!(!response.body.is_empty());
    }

    #[tokio::test]
    async fn page_shows_durable_change_counts_without_values() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .apply_change(
                aether_store::ChangeKind::Capability,
                "review",
                "private-credential",
            )
            .expect("change must apply");
        store
            .record_change(AuditActor::Mcp, AuditResult::Applied)
            .expect("change must record");
        store
            .put_scope("scope", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("task"),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("agent"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("scope"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: "fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "private task data".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must admit");
        let admin_ui =
            build_admin_ui(shared_with_store(store), UiOptions::default(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(
            response.body.contains("1 configured capabilities."),
            "{}",
            response.body
        );
        assert!(response.body.contains("1 stored tasks. 1 tasks are waiting."));
        assert!(!response.body.contains("private-credential"));
        assert!(!response.body.contains("private task data"));
    }

    #[tokio::test]
    async fn managed_sections_require_a_managed_scope() {
        let directory = tempfile::tempdir().expect("directory must create");
        let admin_ui =
            build_admin_ui(shared_in(directory.path()), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("id=\"members\""));
        assert!(response.body.contains("id=\"invitations\""));
    }

    #[tokio::test]
    async fn managed_page_shows_cached_member_states() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        for (cursor, agent, endpoint, expires_at) in [
            (1, "active-agent", "active-endpoint", i64::MAX),
            (2, "revoked-agent", "revoked-endpoint", i64::MAX),
            (3, "expired-agent", "expired-endpoint", 1),
        ] {
            store
                .apply_managed_member(
                    "https://control.test",
                    cursor,
                    &ManagedMember {
                        scope_id: "managed".to_owned(),
                        agent_id: agent.to_owned(),
                        endpoint_id: endpoint.to_owned(),
                        role: "member".to_owned(),
                        expires_at,
                        revoked_at: None,
                    },
                )
                .expect("member must store");
        }
        store
            .revoke_managed_member(
                "https://control.test",
                4,
                "managed",
                "revoked-agent",
            )
            .expect("member must revoke");
        let admin_ui =
            build_admin_ui(shared_with_store(store), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("active-agent in managed: active."));
        assert!(response.body.contains("revoked-agent in managed: revoked."));
        assert!(response.body.contains("expired-agent in managed: expired."));
    }

    #[tokio::test]
    async fn member_names_are_escaped_in_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .apply_managed_member(
                "https://control.test",
                1,
                &ManagedMember {
                    scope_id: "managed".to_owned(),
                    agent_id: "<script>alert(1)</script>".to_owned(),
                    endpoint_id: "endpoint&id".to_owned(),
                    role: "member".to_owned(),
                    expires_at: i64::MAX,
                    revoked_at: None,
                },
            )
            .expect("member must store");
        let admin_ui =
            build_admin_ui(shared_with_store(store), managed_options(), PORT);
        let response = get(app(admin_ui), "/").await;
        assert!(response.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(response.body.contains("endpoint&amp;id"));
        assert!(!response.body.contains("<script>alert(1)</script>"));
    }

    #[tokio::test]
    async fn tls_identity_is_persisted_in_the_node_data_directory() {
        let temporary = tempfile::tempdir().expect("directory must create");
        let directory = temporary.path();
        tls_config(directory).expect("TLS configuration must create");
        assert!(directory.join("admin-cert.der").exists());
        assert!(directory.join("admin-key.der").exists());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(directory.join("admin-key.der"))
                .expect("TLS key metadata must load")
                .permissions()
                .mode()
                & 0o077,
            0
        );
        tls_config(directory).expect("TLS configuration must reload");
    }

    #[tokio::test]
    async fn a_post_without_any_session_cannot_confirm_a_change() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = post(app(admin_ui), "/confirm", None, "id=one").await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn expired_session_cannot_confirm_a_change() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let sessions = Arc::clone(&admin_ui.sessions);
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        sessions.expire();
        let response = post(application, "/confirm", Some(cookie), "id=one").await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_stale_session_cookie_cannot_confirm_a_change() {
        // Stands in for the old hidden `csrf` field: with sessions now
        // owned by topcoat, "wrong csrf" and "wrong session" are the same
        // failure — a cookie that does not match the one on record.
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = post(
            app(admin_ui),
            "/confirm",
            Some(&format!("__Host-session-{PORT}=not-a-real-token")),
            "id=one",
        )
        .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cross_origin_post_cannot_confirm_a_change() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("home page must set a cookie"),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/confirm")
            .header("host", format!("127.0.0.1:{PORT}"))
            .header("origin", "https://evil.example")
            .header("cookie", cookie.to_owned())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from("id=one"))
            .expect("request must build");
        let response = application.oneshot(request).await.expect("router must respond");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn router_rejects_another_host() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("host", "example.test")
                    .body(axum::body::Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn router_rejects_a_form_larger_than_the_body_limit() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/confirm")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(axum::body::Body::from("x".repeat(super::UI_BODY_LIMIT + 1)))
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test(start_paused = true)]
    async fn router_stops_a_partial_body_at_the_request_time_limit() {
        let body = axum::body::Body::from_stream(
            futures_util::stream::once(async {
                Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"id=one&"))
            })
            .chain(futures_util::stream::pending()),
        );
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/confirm")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .header("origin", format!("https://127.0.0.1:{PORT}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(body)
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn router_serves_concurrent_requests() {
        let (_directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        let application = app(admin_ui);
        let page = |application: axum::Router| async move {
            application
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("host", format!("127.0.0.1:{PORT}"))
                        .body(axum::body::Body::empty())
                        .expect("request must build"),
                )
                .await
                .expect("router must respond")
        };
        let (first, second) =
            tokio::join!(page(application.clone()), page(application));
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn router_streams_auditable_events_with_security_headers() {
        let (_directory, admin_ui, administration) = admin_ui_with_directory_applier();
        add_pending(&administration, "directory", "https://directory.test");
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .body(axum::body::Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type"),
            Some(&axum::http::HeaderValue::from_static("text/event-stream"))
        );
        assert_eq!(
            response.headers().get("cache-control"),
            Some(&axum::http::HeaderValue::from_static("no-store"))
        );
        let mut stream = response.into_body().into_data_stream();
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("event must arrive")
            .expect("stream must contain an event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("event: ready"));
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("audit event must arrive")
            .expect("stream must contain an audit event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("event: audit"));
        assert!(event.contains("\"entry\""));
    }

    #[tokio::test]
    async fn router_resumes_after_the_last_audit_event() {
        let (_directory, admin_ui, administration) = admin_ui_with_directory_applier();
        add_pending(&administration, "first", "https://directory.test");
        add_pending(&administration, "second", "https://directory.test");
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .header("last-event-id", "1:0")
                    .body(axum::body::Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        let mut stream = response.into_body().into_data_stream();
        let _ = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("ready event must arrive");
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("audit event must arrive")
            .expect("stream must contain an event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("id: 2:0"));
        assert!(event.contains("event: audit"));
    }

    #[tokio::test]
    async fn router_streams_a_new_audit_event_before_the_fallback_time() {
        let (_directory, admin_ui, administration) = admin_ui_with_directory_applier();
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .body(axum::body::Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        let mut stream = response.into_body().into_data_stream();
        let _ = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("ready event must arrive");
        let next = tokio::spawn(async move { stream.next().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        add_pending(&administration, "late", "https://directory.test");
        let event = tokio::time::timeout(super::ACTIVITY_FALLBACK / 3, next)
            .await
            .expect("audit event must arrive before the fallback time")
            .expect("stream task must finish")
            .expect("stream must contain an event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("id: 1:0"));
        assert!(event.contains("event: audit"));
    }

    #[tokio::test]
    async fn router_streams_durable_task_events_without_task_content() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .put_scope("scope", aether_core::ScopeKind::Direct)
            .expect("scope must store");
        store
            .admit(
                &TaskInput {
                    request: aether_core::Request {
                        task_id: aether_core::TaskId::new("task"),
                        id: aether_core::RequestId::new("request"),
                        caller_agent_id: aether_core::AgentId::new("agent"),
                        caller_endpoint_id: aether_core::EndpointId::new("endpoint"),
                        scope_id: aether_core::ScopeId::new("scope"),
                        capability_id: aether_core::CapabilityId::new("review"),
                        workspace_id: aether_core::WorkspaceId::new("workspace"),
                        access_mode: aether_core::AccessMode::ReadOnly,
                    },
                    idempotency_key: "key".to_owned(),
                    request_fingerprint: "fingerprint".to_owned(),
                    skill: "review".to_owned(),
                    remote_text: "private task data".to_owned(),
                },
                QueueLimits { global: 1, per_peer: 1 },
            )
            .expect("task must admit");
        let admin_ui =
            build_admin_ui(shared_with_store(store), UiOptions::default(), PORT);
        let response = app(admin_ui)
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .body(axum::body::Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        let mut stream = response.into_body().into_data_stream();
        let _ = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("ready event must arrive");
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("task event must arrive")
            .expect("stream must contain a task event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("event: task"));
        assert!(event.contains("\"taskId\":\"task\""));
        assert!(event.contains("\"event\":\"queued\""));
        assert!(!event.contains("private task data"));
    }

    #[tokio::test]
    async fn pending_changes_show_no_secret_value() {
        let (_directory, admin_ui, administration) = admin_ui_with_directory_applier();
        administration
            .lock()
            .expect("administration must lock")
            .submit(
                &{
                    let grant = crate::admin::peer_grant();
                    ChangeRequest::Grant { id: grant.id.as_str().to_owned(), grant }
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must add");
        let application = app(admin_ui);
        let home = get(application.clone(), "/").await;
        assert!(home.body.contains("Confirm change"));
        assert!(!home.body.contains("08080808080808080808080808080808"));
        let token = extract_confirm_token(&home.body)
            .expect("a confirmation token must render");
        let cookie = session_cookie_pair(
            home.set_cookie.as_deref().expect("cookie must be set"),
        );
        let response =
            post(application, "/confirm", Some(cookie), &format!("id={token}")).await;
        assert!(response.body.contains("No changes need confirmation."));
        assert_eq!(
            administration.lock().expect("administration must unlock").audit(),
            ["pending", "confirmed", "applied"]
        );
    }

    fn extract_confirm_token(body: &str) -> Option<&str> {
        let (_, rest) = body.split_once("name=\"id\" value=\"")?;
        let (token, _) = rest.split_once('"')?;
        Some(token)
    }
}
