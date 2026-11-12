//! HTTPS JSON clients for the directory and the managed control, which
//! trust one pinned certificate and nothing else.

use std::time::Duration;

use reqwest::{
    Certificate, Client, RequestBuilder, Response, StatusCode, Url, redirect::Policy,
};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HttpError {
    /// The URL is not a bare HTTPS origin.
    Origin,
    Certificate,
    Client,
    Request,
    Status,
    Body,
}

#[derive(Clone)]
pub struct PinnedJsonClient {
    client: Client,
    origin: Url,
}

impl PinnedJsonClient {
    /// A client that trusts only `certificate_der`, for `origin`. The origin
    /// must be HTTPS with a host and nothing after it: no credentials, query
    /// or fragment. Any path is dropped.
    pub fn open(origin: &str, certificate_der: &[u8]) -> Result<Self, HttpError> {
        let mut origin = Url::parse(origin).map_err(|_| HttpError::Origin)?;
        if origin.scheme() != "https"
            || origin.host().is_none()
            || origin.username() != ""
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(HttpError::Origin);
        }
        origin.set_path("/");
        let certificate = Certificate::from_der(certificate_der)
            .map_err(|_| HttpError::Certificate)?;
        let client = Client::builder()
            .add_root_certificate(certificate)
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| HttpError::Client)?;
        Ok(Self { client, origin })
    }

    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// The origin with `segments` as its path, each one percent-encoded, so
    /// `?`, `#` and `%` in an id cannot change the URL.
    pub fn url(&self, segments: &[&str]) -> Result<Url, HttpError> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|()| HttpError::Origin)?
            .clear()
            .extend(segments);
        Ok(url)
    }

    /// Whether `segment` can be one path segment that names the same id on
    /// the server. `/` is refused because the server decodes `%2F` back to
    /// `/`, and `.` and `..` because a URL parser collapses them.
    pub fn require_segment(segment: &str) -> Result<(), HttpError> {
        if matches!(segment, "" | "." | "..")
            || segment.contains('/')
            || segment.contains('\0')
        {
            return Err(HttpError::Body);
        }
        Ok(())
    }
}

/// Sends `request` and returns its response when the status is one of
/// `accepted`.
pub async fn send(
    request: RequestBuilder,
    accepted: &[StatusCode],
) -> Result<Response, HttpError> {
    let response = request.send().await.map_err(|_| HttpError::Request)?;
    if !accepted.contains(&response.status()) {
        return Err(HttpError::Status);
    }
    Ok(response)
}

/// Sends `request` and decodes its JSON body when the status is one of
/// `accepted`.
pub async fn json<T: DeserializeOwned>(
    request: RequestBuilder,
    accepted: &[StatusCode],
) -> Result<T, HttpError> {
    send(request, accepted).await?.json().await.map_err(|_| HttpError::Body)
}

pub fn monotonic_cursors(after: i64, cursors: impl IntoIterator<Item = i64>) -> bool {
    if after < 0 {
        return false;
    }
    let mut previous = after;
    for cursor in cursors {
        if cursor <= previous {
            return false;
        }
        previous = cursor;
    }
    true
}
