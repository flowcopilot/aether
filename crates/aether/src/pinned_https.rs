//! HTTPS clients for the directory and the managed control, which trust
//! one pinned certificate and nothing else.

use std::time::Duration;

use reqwest::{Certificate, Client, Url, redirect::Policy};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PinnedError {
    /// The URL is not a bare HTTPS origin.
    Origin,
    Certificate,
    Client,
}

#[derive(Clone)]
pub struct PinnedOrigin {
    pub client: Client,
    pub origin: Url,
}

impl PinnedOrigin {
    pub fn open(origin: &str, certificate_der: &[u8]) -> Result<Self, PinnedError> {
        let (client, origin) = client(origin, certificate_der)?;
        Ok(Self { client, origin })
    }

    pub fn url(&self, segments: &[&str]) -> Option<Url> {
        url(&self.origin, segments)
    }
}

/// A client that trusts only `certificate_der`, and the origin it talks
/// to. The origin must be HTTPS with a host and nothing after it: no
/// credentials, query or fragment. Any path is dropped.
pub fn client(
    origin: &str,
    certificate_der: &[u8],
) -> Result<(Client, Url), PinnedError> {
    let mut origin = Url::parse(origin).map_err(|_| PinnedError::Origin)?;
    if origin.scheme() != "https"
        || origin.host().is_none()
        || origin.username() != ""
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(PinnedError::Origin);
    }
    origin.set_path("/");
    let certificate =
        Certificate::from_der(certificate_der).map_err(|_| PinnedError::Certificate)?;
    let client = Client::builder()
        .add_root_certificate(certificate)
        .redirect(Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| PinnedError::Client)?;
    Ok((client, origin))
}

/// `origin` with `segments` as its path, each one percent-encoded, so `?`,
/// `#` and `%` in an id cannot change the URL.
pub fn url(origin: &Url, segments: &[&str]) -> Option<Url> {
    let mut url = origin.clone();
    url.path_segments_mut().ok()?.clear().extend(segments);
    Some(url)
}

/// Whether `id` can be one path segment that names the same id on the
/// server. `/` is refused because the server decodes `%2F` back to `/`, and
/// `.` and `..` because a URL parser collapses them.
pub fn valid_segment(id: &str) -> bool {
    !matches!(id, "" | "." | "..") && !id.contains('/') && !id.contains('\0')
}
