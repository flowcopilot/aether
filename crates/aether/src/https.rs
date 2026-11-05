//! Serves an axum router over HTTPS with limits on each connection and
//! request.

use std::{
    future::Ready,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Router, extract::DefaultBodyLimit, http::StatusCode, middleware,
    response::IntoResponse,
};
use axum_server::{
    Handle,
    tls_rustls::{RustlsAcceptor, RustlsConfig},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// The limits of one HTTPS server.
#[derive(Clone, Copy, Debug)]
pub struct Hardening {
    /// The longest time a client may take to finish the TLS handshake.
    pub handshake_timeout: Duration,
    /// The most open connections; the server drops a connection above it.
    pub max_connections: usize,
    /// The longest time a request may take to give its response head. A
    /// streamed response body continues after it.
    pub request_timeout: Duration,
    /// The largest request body an extractor reads.
    pub body_limit: usize,
}

impl Hardening {
    /// `router` with the request timeout (`408`) and the body limit.
    pub fn bounded(self, router: Router) -> Router {
        let request_timeout = self.request_timeout;
        router
            .layer(middleware::from_fn(
                move |request: axum::extract::Request, next: middleware::Next| async move {
                    tokio::time::timeout(request_timeout, next.run(request))
                        .await
                        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
                },
            ))
            .layer(DefaultBodyLimit::max(self.body_limit))
    }
}

/// Serves `router` on `listener` over TLS with `hardening`, until `handle`
/// shuts it down.
pub async fn serve(
    listener: std::net::TcpListener,
    tls: RustlsConfig,
    hardening: Hardening,
    handle: Handle<std::net::SocketAddr>,
    router: Router,
) -> io::Result<()> {
    let acceptor = RustlsAcceptor::new(tls)
        .handshake_timeout(hardening.handshake_timeout)
        .acceptor(ConnectionLimitAcceptor::new(hardening.max_connections));
    axum_server::from_tcp(listener)?
        .acceptor(acceptor)
        .handle(handle)
        .serve(hardening.bounded(router).into_make_service())
        .await
}

#[derive(Clone)]
struct ConnectionLimitAcceptor {
    permits: Arc<Semaphore>,
}

impl ConnectionLimitAcceptor {
    fn new(limit: usize) -> Self {
        Self { permits: Arc::new(Semaphore::new(limit)) }
    }
}

struct LimitedStream {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl<S> axum_server::accept::Accept<TcpStream, S> for ConnectionLimitAcceptor {
    type Stream = LimitedStream;
    type Service = S;
    type Future = Ready<io::Result<(Self::Stream, Self::Service)>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        std::future::ready(
            self.permits
                .clone()
                .try_acquire_owned()
                .map(|permit| (LimitedStream { stream, _permit: permit }, service))
                .map_err(|_| io::Error::from(io::ErrorKind::ConnectionAborted)),
        )
    }
}

impl AsyncRead for LimitedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for LimitedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(context, buffer)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(context)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionLimitAcceptor;

    #[tokio::test]
    async fn connection_limit_holds_a_permit_until_the_peer_disconnects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener must bind");
        let acceptor = ConnectionLimitAcceptor::new(1);
        let first_client =
            tokio::net::TcpStream::connect(listener.local_addr().expect("address"))
                .await
                .expect("first client must connect");
        let (first_stream, _) =
            listener.accept().await.expect("first stream must accept");
        let first = axum_server::accept::Accept::accept(&acceptor, first_stream, ())
            .await
            .expect("first stream must pass");
        let second_client =
            tokio::net::TcpStream::connect(listener.local_addr().expect("address"))
                .await
                .expect("second client must connect");
        let (second_stream, _) =
            listener.accept().await.expect("second stream must accept");
        let Err(error) =
            axum_server::accept::Accept::accept(&acceptor, second_stream, ()).await
        else {
            panic!("second stream must be limited");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        drop(first);
        let third_client =
            tokio::net::TcpStream::connect(listener.local_addr().expect("address"))
                .await
                .expect("third client must connect");
        let (third_stream, _) =
            listener.accept().await.expect("third stream must accept");
        let third = axum_server::accept::Accept::accept(&acceptor, third_stream, ())
            .await
            .expect("third stream must pass");
        drop(first_client);
        drop(second_client);
        drop(third_client);
        drop(third);
    }
}
