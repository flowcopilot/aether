use std::{
    future::Future,
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::DefaultBodyLimit, http::StatusCode, middleware, response::IntoResponse,
};
use tower::{ServiceBuilder, limit::ConcurrencyLimitLayer, load_shed::LoadShedLayer};

use super::{
    config::PactConfig,
    consent::{ConsentLink, PactConsent, SharedPactStore},
    gateway::{Gateway, PactNode, router},
    jwks::{HttpJwksSource, PaVerifier, UrlPolicy},
    keys::ProviderKey,
};

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 128;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("the PACT signing key cannot be loaded")]
    Key,
    #[error("the PACT store cannot be opened")]
    Store,
    #[error("the JWKS client cannot be built")]
    Client,
}

pub struct Started {
    pub gateway: Arc<Gateway>,
    pub consent: Arc<PactConsent>,
    pub link: Arc<ConsentLink>,
}

pub fn start(
    config: &PactConfig,
    data_dir: &Path,
    node: Arc<dyn PactNode>,
) -> Result<Started, StartError> {
    let key =
        Arc::new(ProviderKey::load_or_create(data_dir).map_err(|_| StartError::Key)?);
    let store: SharedPactStore = Arc::new(Mutex::new(
        aether_store::pact::PactStore::in_dir(data_dir)
            .map_err(|_| StartError::Store)?,
    ));
    let consent = Arc::new(PactConsent::new(
        config.clone(),
        Arc::clone(&store),
        data_dir.to_owned(),
    ));
    let policy =
        UrlPolicy { allow_insecure_local: config.allow_insecure_local_issuers };
    let source = HttpJwksSource::new(policy).map_err(|_| StartError::Client)?;
    let verifier = PaVerifier::new(config.audience.clone(), policy, Arc::new(source));
    let link = Arc::new(ConsentLink::default());
    let gateway = Arc::new(Gateway::new(
        config.clone(),
        verifier,
        key,
        store,
        node,
        Arc::clone(&link),
    ));
    Ok(Started { gateway, consent, link })
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    gateway: Arc<Gateway>,
    max_seconds: u64,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let request_limit = Duration::from_secs(max_seconds.saturating_add(60));
    let maintenance = tokio::spawn({
        let gateway = Arc::clone(&gateway);
        async move {
            let mut interval = tokio::time::interval(MAINTENANCE_INTERVAL);
            loop {
                interval.tick().await;
                let gateway = Arc::clone(&gateway);
                let _ = tokio::task::spawn_blocking(move || gateway.maintain()).await;
            }
        }
    });
    let app = router(gateway)
        .layer(middleware::from_fn(move |request, next: middleware::Next| async move {
            tokio::time::timeout(request_limit, next.run(request))
                .await
                .unwrap_or_else(|_| StatusCode::GATEWAY_TIMEOUT.into_response())
        }))
        .layer(
            ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(|_| async {
                    StatusCode::SERVICE_UNAVAILABLE
                }))
                .layer(LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS)),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));
    let result = axum::serve(listener, app).with_graceful_shutdown(shutdown).await;
    maintenance.abort();
    result
}
