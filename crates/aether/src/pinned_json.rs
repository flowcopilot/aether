use reqwest::{StatusCode, Url};
use serde::{Serialize, de::DeserializeOwned};

use crate::pinned_https::{self, PinnedError, PinnedOrigin};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HttpError {
    Origin,
    Certificate,
    Client,
    Request,
    Status,
    Body,
}

impl From<PinnedError> for HttpError {
    fn from(error: PinnedError) -> Self {
        match error {
            PinnedError::Origin => Self::Origin,
            PinnedError::Certificate => Self::Certificate,
            PinnedError::Client => Self::Client,
        }
    }
}

#[derive(Clone)]
pub struct PinnedJsonClient {
    pinned: PinnedOrigin,
}

impl PinnedJsonClient {
    pub fn open(origin: &str, certificate_der: &[u8]) -> Result<Self, HttpError> {
        Ok(Self { pinned: PinnedOrigin::open(origin, certificate_der)? })
    }

    pub fn url(&self, segments: &[&str]) -> Result<Url, HttpError> {
        self.pinned.url(segments).ok_or(HttpError::Origin)
    }

    pub fn require_segment(segment: &str) -> Result<(), HttpError> {
        if pinned_https::valid_segment(segment) { Ok(()) } else { Err(HttpError::Body) }
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: Url,
    ) -> Result<T, HttpError> {
        let response =
            self.pinned.client.get(url).send().await.map_err(|_| HttpError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(HttpError::Status);
        }
        response.json().await.map_err(|_| HttpError::Body)
    }

    pub async fn get_json_query<T: DeserializeOwned>(
        &self,
        url: Url,
        query: &[(&str, &str)],
    ) -> Result<T, HttpError> {
        let response = self
            .pinned
            .client
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|_| HttpError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(HttpError::Status);
        }
        response.json().await.map_err(|_| HttpError::Body)
    }

    pub async fn post_json<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        url: Url,
        body: &B,
        expected: StatusCode,
    ) -> Result<T, HttpError> {
        let response = self
            .pinned
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|_| HttpError::Request)?;
        if response.status() != expected {
            return Err(HttpError::Status);
        }
        response.json().await.map_err(|_| HttpError::Body)
    }

    pub async fn post_json_any<T: DeserializeOwned, B: Serialize + Sync>(
        &self,
        url: Url,
        body: &B,
        expected: &[StatusCode],
    ) -> Result<T, HttpError> {
        let response = self
            .pinned
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|_| HttpError::Request)?;
        if !expected.contains(&response.status()) {
            return Err(HttpError::Status);
        }
        response.json().await.map_err(|_| HttpError::Body)
    }

    pub async fn delete_json<B: Serialize + Sync>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<(), HttpError> {
        let response = self
            .pinned
            .client
            .delete(url)
            .json(body)
            .send()
            .await
            .map_err(|_| HttpError::Request)?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(HttpError::Status);
        }
        Ok(())
    }
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
