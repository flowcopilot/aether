use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
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
        error::{bad_request, forbidden, internal_server_error},
        header, layer, layout, page,
        request::headers,
        response::IntoResponse as TopcoatIntoResponse,
        route,
    },
    session::{self, RouterBuilderSessionExt, SessionConfig, TokenHash},
    view::{View, view},
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
mod shared;
#[cfg(test)]
mod test_support;

use crate::admin::SharedAdministration;
use advanced::dashboard;
use caller::caller_screen;
use host::host_screen;

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
/// opens the activity stream. A hub section that waits for the other side
/// names its screen in `data-hub-waiting`, and the script reloads that
/// screen: the address bar may still hold the path of a form, which only
/// answers a POST, and a node with both pages shows the caller at `/agents`.
const ADMIN_JAVASCRIPT: &str = "document.addEventListener('click',async event=>{const button=event.target.closest('[data-copy-text]');if(!button)return;const label=button.querySelector('[data-copy-label]')||button;const text=label.textContent;try{await navigator.clipboard.writeText(button.dataset.copyText);label.textContent='Copied'}catch{label.textContent='Select and copy'}setTimeout(()=>{label.textContent=text},1600)});const status=document.getElementById('activity-status');if(status){const events=new EventSource('/events');events.addEventListener('ready',()=>{status.textContent='Activity updates are connected.'});events.addEventListener('audit',event=>{status.textContent=JSON.parse(event.data).entry});events.addEventListener('task',event=>{const task=JSON.parse(event.data);status.textContent=`Task ${task.taskId}: ${task.event}`});events.addEventListener('unavailable',()=>{status.textContent='Activity updates cannot read the administration store.'});events.onerror=()=>{status.textContent='Activity updates will retry.'}}const waiting=document.querySelector('[data-hub-waiting]');if(waiting){const reload=()=>{const active=document.activeElement;if(active&&active.matches('input,select,textarea')&&active.value){setTimeout(reload,4000);return}location.assign(waiting.dataset.hubWaiting||'/')};setTimeout(reload,4000)}const dots=document.querySelectorAll('[data-presence]');if(dots.length){const update=async()=>{try{const answer=await fetch('/hub/presence');if(!answer.ok)return;const presence=await answer.json();for(const dot of dots){const online=presence[dot.dataset.presence];dot.dataset.state=online===undefined?'unknown':online?'online':'offline';dot.querySelector('[data-presence-label]').textContent=online===undefined?'':online?'Online':'Offline'}}catch{}};update();setInterval(update,15000)}";

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

    /// Runs `confirm` on the change behind `token`, and uses the token up
    /// only when it succeeds: a confirm that fails leaves the change pending,
    /// so the same button can try again. `None` when the token is unknown.
    /// The map stays locked meanwhile, so two posts of one token cannot both
    /// confirm.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock spans the confirm on purpose"
    )]
    fn confirm<E>(
        &self,
        token: &str,
        confirm: impl FnOnce(&str) -> Result<(), E>,
    ) -> Option<Result<(), E>> {
        let mut confirmations = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let result = confirm(confirmations.get(token)?);
        if result.is_ok() {
            confirmations.remove(token);
        }
        Some(result)
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

/// Requires the admin session, then the node page of this UI. `refusal` is
/// the `400` text when the UI has no page.
async fn admin_page<'cx>(
    cx: &'cx Cx,
    refusal: &'static str,
) -> TopcoatResult<&'cx PageContext> {
    require_admin_session(cx).await?;
    let Page(page) = app_context::<Page>(cx);
    page.as_deref().ok_or_else(|| bad_request(refusal).into())
}

/// The administration, locked. A panic elsewhere does not lock it out.
fn administration(cx: &Cx) -> MutexGuard<'_, crate::admin::Administration> {
    app_context::<SharedAdministration>(cx)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
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

/// The screen of this UI's mode: the host screen for `node serve`, the
/// caller screen for `node admin`, and the Advanced page when neither is
/// set.
#[page("/")]
async fn home(cx: &Cx) -> TopcoatResult<impl View> {
    let mode = app_context::<Page>(cx).mode();
    Ok(view! {
        if mode.is_some_and(UiMode::hosts) {
            host_screen()
        } else if mode.is_some_and(UiMode::calls) {
            caller_screen()
        } else {
            dashboard()
        }
    })
}

#[page("/agents")]
async fn agents(cx: &Cx) -> TopcoatResult<impl View> {
    if !app_context::<Page>(cx).mode().is_some_and(UiMode::calls) {
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
    /// Whether the last read failed, so the page hears it once per outage.
    unavailable: bool,
}

impl ActivityFeed {
    fn new(administration: SharedAdministration, cursor: ActivityCursor) -> Self {
        let (audited, tasks) = {
            let administration =
                administration.lock().unwrap_or_else(PoisonError::into_inner);
            (administration.watch_audit(), administration.subscribe_task_events())
        };
        Self {
            administration,
            cursor,
            ready: VecDeque::new(),
            audited,
            tasks,
            unavailable: false,
        }
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
        let read = {
            let administration =
                self.administration.lock().unwrap_or_else(PoisonError::into_inner);
            administration.audit_page(self.cursor.audit, ACTIVITY_BATCH).and_then(
                |audit| {
                    administration
                        .task_activity_after(self.cursor.task, ACTIVITY_BATCH)
                        .map(|tasks| (audit, tasks))
                },
            )
        };
        let Ok((audit, tasks)) = read else {
            let first = !self.unavailable;
            self.unavailable = true;
            if first {
                self.ready.push_back(Event::new().event("unavailable").data("{}"));
            }
            return first;
        };
        self.unavailable = false;
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

pub use crate::hub_client::HubAccess;

/// The screens of a page: the host screen of `node serve`, the caller screen
/// of `node admin`, or both on one node. A UI without a page has neither and
/// shows the Advanced page at `/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiMode {
    Host,
    Caller,
    HostAndCaller,
}

impl UiMode {
    /// Whether the page has the host screen.
    #[must_use]
    pub const fn hosts(self) -> bool {
        matches!(self, Self::Host | Self::HostAndCaller)
    }

    /// Whether the page has the caller screen.
    #[must_use]
    pub const fn calls(self) -> bool {
        matches!(self, Self::Caller | Self::HostAndCaller)
    }
}

pub struct PageContext {
    pub data_dir: PathBuf,
    pub task_folder_mode: bool,
    pub mode: UiMode,
    name: Option<String>,
    relay_url: Mutex<Option<String>>,
    switch_relay: RelaySwitcher,
    hub: Arc<crate::hub_client::NodeHub>,
    hub_sign: Arc<Mutex<()>>,
}

impl PageContext {
    #[must_use]
    pub fn host(
        data_dir: PathBuf,
        name: Option<String>,
        relay_url: Option<String>,
        switch_relay: RelaySwitcher,
        hub: Arc<crate::hub_client::NodeHub>,
        hub_sign: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            data_dir,
            task_folder_mode: false,
            mode: UiMode::Host,
            name,
            relay_url: Mutex::new(relay_url),
            switch_relay,
            hub,
            hub_sign,
        }
    }

    #[must_use]
    pub fn admin_caller(data_dir: PathBuf) -> Self {
        let hub = Arc::new(crate::hub_client::NodeHub::new(data_dir.clone(), None));
        Self {
            data_dir,
            task_folder_mode: false,
            mode: UiMode::Caller,
            name: None,
            relay_url: Mutex::new(None),
            switch_relay: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            hub,
            hub_sign: Arc::new(Mutex::new(())),
        }
    }

    /// Adds the caller screen to a host page, or takes it away. A page
    /// without a host keeps its caller screen.
    #[must_use]
    pub const fn with_caller(mut self, enabled: bool) -> Self {
        self.mode = match (self.mode, enabled) {
            (UiMode::Host | UiMode::HostAndCaller, true) => UiMode::HostAndCaller,
            (UiMode::Host | UiMode::HostAndCaller, false) => UiMode::Host,
            (UiMode::Caller, _) => UiMode::Caller,
        };
        self
    }

    pub(crate) fn agent_name(&self) -> String {
        crate::invites::invite_agent_name(&self.data_dir, self.name.as_deref())
    }

    #[must_use]
    pub fn with_hub(self, hub: Option<crate::hub_client::HubClient>) -> Self {
        self.set_hub(hub);
        self
    }

    #[must_use]
    pub const fn with_task_folder_mode(mut self, enabled: bool) -> Self {
        self.task_folder_mode = enabled;
        self
    }

    pub(super) fn hub_access(&self) -> HubAccess {
        self.hub.access()
    }

    pub(crate) fn current_hub(&self) -> Option<crate::hub_client::HubClient> {
        match self.hub_access() {
            HubAccess::Ready(client) => Some(*client),
            HubAccess::Off | HubAccess::Unavailable => None,
        }
    }

    pub(super) fn set_hub(&self, hub: Option<crate::hub_client::HubClient>) {
        if let Some(client) = hub {
            let _ = self.hub.as_ref().prime(client);
        }
    }

    pub(crate) const fn hub_sign(&self) -> &Arc<Mutex<()>> {
        &self.hub_sign
    }

    #[cfg(test)]
    pub(crate) fn worker_context(
        &self,
    ) -> Arc<crate::node_serve_context::NodeServeContext> {
        Arc::new(crate::node_serve_context::NodeServeContext {
            data_dir: self.data_dir.clone(),
            hub: Arc::clone(&self.hub),
            hub_sign: Arc::clone(&self.hub_sign),
            caller: self.mode.calls(),
        })
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

impl Page {
    /// The screens of this UI; `None` when it has no page.
    fn mode(&self) -> Option<UiMode> {
        self.0.as_ref().map(|page| page.mode)
    }
}

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
        .page(host::publish_listing)
        .page(caller::import)
        .page(caller::remove_contact)
        .page(caller::confirm_request)
        .page(caller::discard_request)
        .page(hub::claim_name)
        .page(hub::set_search)
        .page(hub::invite_by_name)
        .page(caller::accept_invite)
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
mod tests {
    use std::{sync::Arc, time::Duration};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt;
    use tower::ServiceExt;

    use super::{
        UiOptions, build_admin_ui,
        test_support::{
            PORT, admin_ui_with_directory_applier, app, get, host_ui,
            host_ui_with_roles, post, session_cookie_pair, test_host_page,
        },
        tls_config,
    };
    use crate::admin::{ChangeRequest, SharedAdministration, shared_with_store};
    use aether_store::{NodeStore, QueueLimits, TaskInput};

    #[test]
    fn the_page_hub_reads_a_saved_change_without_restarting_the_page() {
        let directory = tempfile::tempdir().expect("directory must create");
        let _ = crate::data_dir::DataDir::new(directory.path()).endpoint_key();
        let page = test_host_page(
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
        let page = test_host_page(
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
        let page = test_host_page(
            directory.path().to_path_buf(),
            None,
            None,
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            Some("https://flag.example".to_owned()),
        );
        assert_eq!(page.current_hub().expect("hub").url(), "https://flag.example");
    }

    #[test]
    fn the_agent_name_comes_from_the_hub_name_then_the_invite_name_then_the_endpoint() {
        let directory = tempfile::tempdir().expect("directory must create");
        let host = |name: Option<&str>| {
            test_host_page(
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
        // A waiting hub section reloads its own screen, not always `/`.
        assert!(response.body.contains("location.assign(waiting.dataset.hubWaiting"));
        assert!(!response.body.contains("location.assign('/')"));
    }

    #[tokio::test]
    async fn the_activity_feed_says_when_it_cannot_read_the_store() {
        let (directory, admin_ui, _administration) = admin_ui_with_directory_applier();
        rusqlite::Connection::open(directory.path().join(aether_store::NODE_DATABASE))
            .expect("database must open")
            .execute("DROP TABLE audit_records", [])
            .expect("audit records must drop");
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
            .expect("unavailable event must arrive")
            .expect("stream must contain an event")
            .expect("event must be valid bytes");
        let event = std::str::from_utf8(&event).expect("event must be UTF-8");
        assert!(event.contains("event: unavailable"), "{event}");
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
}
