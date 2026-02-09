//! HTTP Cache Middleware for caching web responses.
//!
//! This module provides the `HttpCacheMiddleware`, which intercepts HTTP requests and
//! responses to implement a caching mechanism. It stores successful HTTP responses (e.g., 200 OK)
//! to a local directory, and for subsequent identical requests, it serves the cached response
//! instead of making a new network request. This can significantly reduce network traffic,
//! improve crawling speed, and enable offline processing or replay of crawls.
//!
//! The cache uses request fingerprints to identify unique requests and associates them
//! with their corresponding cached responses. Responses are serialized and deserialized
//! using `bincode`.

use async_trait::async_trait;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, trace, warn};

use crate::middleware::{Middleware, MiddlewareAction};
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use spider_util::error::SpiderError;
use spider_util::request::Request;
use spider_util::response::Response;
use url::Url;

fn serialize_headermap<S>(headers: &HeaderMap, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = std::collections::HashMap::<String, String>::new();
    for (name, value) in headers.iter() {
        map.insert(
            name.to_string(),
            value.to_str().unwrap_or_default().to_string(),
        );
    }
    map.serialize(serializer)
}

fn deserialize_headermap<'de, D>(deserializer: D) -> Result<HeaderMap, D::Error>
where
    D: Deserializer<'de>,
{
    let map = std::collections::HashMap::<String, String>::deserialize(deserializer)?;
    let mut headers = HeaderMap::new();
    for (name, value) in map {
        if let (Ok(header_name), Ok(header_value)) =
            (name.parse::<HeaderName>(), value.parse::<HeaderValue>())
        {
            headers.insert(header_name, header_value);
        } else {
            warn!("Failed to parse header: {} = {}", name, value);
        }
    }
    Ok(headers)
}

fn serialize_statuscode<S>(status: &StatusCode, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    status.as_u16().serialize(serializer)
}

fn deserialize_statuscode<'de, D>(deserializer: D) -> Result<StatusCode, D::Error>
where
    D: Deserializer<'de>,
{
    let status_u16 = u16::deserialize(deserializer)?;
    StatusCode::from_u16(status_u16).map_err(serde::de::Error::custom)
}

fn serialize_url<S>(url: &Url, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    url.to_string().serialize(serializer)
}

fn deserialize_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Url::parse(&s).map_err(serde::de::Error::custom)
}

/// Represents a cached response, including enough information to reconstruct a `Response` object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedResponse {
    #[serde(serialize_with = "serialize_url", deserialize_with = "deserialize_url")]
    url: Url,
    #[serde(
        serialize_with = "serialize_statuscode",
        deserialize_with = "deserialize_statuscode"
    )]
    status: StatusCode,
    #[serde(
        serialize_with = "serialize_headermap",
        deserialize_with = "deserialize_headermap"
    )]
    headers: HeaderMap,
    body: Vec<u8>,
    #[serde(serialize_with = "serialize_url", deserialize_with = "deserialize_url")]
    request_url: Url,
}

impl From<Response> for CachedResponse {
    fn from(response: Response) -> Self {
        CachedResponse {
            url: response.url,
            status: response.status,
            headers: response.headers,
            body: response.body.to_vec(),
            request_url: response.request_url,
        }
    }
}

impl From<CachedResponse> for Response {
    fn from(cached_response: CachedResponse) -> Self {
        Response {
            url: cached_response.url,
            status: cached_response.status,
            headers: cached_response.headers,
            body: Bytes::from(cached_response.body),
            request_url: cached_response.request_url,
            meta: Default::default(),
            cached: true,
        }
    }
}

/// Builder for `HttpCacheMiddleware`.
#[derive(Default)]
pub struct HttpCacheMiddlewareBuilder {
    cache_dir: Option<PathBuf>,
}

impl HttpCacheMiddlewareBuilder {
    /// Sets the directory where cache files will be stored.
    pub fn cache_dir(mut self, path: PathBuf) -> Self {
        self.cache_dir = Some(path);
        self
    }

    /// Builds the `HttpCacheMiddleware`.
    /// This can fail if the cache directory cannot be created or determined.
    pub fn build(self) -> Result<HttpCacheMiddleware, SpiderError> {
        let cache_dir = if let Some(path) = self.cache_dir {
            path
        } else {
            dirs::cache_dir()
                .ok_or_else(|| {
                    SpiderError::ConfigurationError(
                        "Could not determine cache directory".to_string(),
                    )
                })?
                .join("spider-lib")
                .join("http_cache")
        };

        std::fs::create_dir_all(&cache_dir)?;

        let middleware = HttpCacheMiddleware { cache_dir };
        info!(
            "Initializing HttpCacheMiddleware with config: {:?}",
            middleware
        );

        Ok(middleware)
    }
}

#[derive(Debug)]
pub struct HttpCacheMiddleware {
    cache_dir: PathBuf,
}

impl HttpCacheMiddleware {
    /// Creates a new `HttpCacheMiddlewareBuilder` to start building an `HttpCacheMiddleware`.
    pub fn builder() -> HttpCacheMiddlewareBuilder {
        HttpCacheMiddlewareBuilder::default()
    }

    fn get_cache_file_path(&self, fingerprint: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.bin", fingerprint))
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for HttpCacheMiddleware {
    fn name(&self) -> &str {
        "HttpCacheMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let fingerprint = request.fingerprint();
        let cache_file_path = self.get_cache_file_path(&fingerprint);

        trace!(
            "Checking cache for request: {} (fingerprint: {})",
            request.url, fingerprint
        );
        if fs::metadata(&cache_file_path).await.is_ok() {
            debug!("Cache hit for request: {}", request.url);
            match fs::read(&cache_file_path).await {
                Ok(cached_bytes) => match bincode::deserialize::<CachedResponse>(&cached_bytes) {
                    Ok(cached_resp) => {
                        trace!(
                            "Successfully deserialized cached response for {}",
                            request.url
                        );
                        let mut response: Response = cached_resp.into();
                        response.meta = request.meta;
                        debug!("Returning cached response for {}", response.url);
                        return Ok(MiddlewareAction::ReturnResponse(response));
                    }
                    Err(e) => {
                        warn!(
                            "Failed to deserialize cached response from {}: {}. Deleting invalid cache file.",
                            cache_file_path.display(),
                            e
                        );
                        fs::remove_file(&cache_file_path).await.ok();
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to read cache file {}: {}. Deleting invalid cache file.",
                        cache_file_path.display(),
                        e
                    );
                    fs::remove_file(&cache_file_path).await.ok();
                }
            }
        } else {
            trace!(
                "Cache miss for request: {} (no cache file found)",
                request.url
            );
        }

        trace!("Continuing request to downloader: {}", request.url);
        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        trace!(
            "Processing response for caching: {} with status: {}",
            response.url, response.status
        );

        // Only cache successful responses (e.g., 200 OK)
        if response.status.is_success() {
            let original_request_fingerprint = response.request_from_response().fingerprint();
            let cache_file_path = self.get_cache_file_path(&original_request_fingerprint);

            trace!(
                "Serializing response for caching to: {}",
                cache_file_path.display()
            );
            let cached_response: CachedResponse = response.clone().into();
            match bincode::serialize(&cached_response) {
                Ok(serialized_bytes) => {
                    let bytes_count = serialized_bytes.len();
                    trace!(
                        "Writing {} bytes to cache file: {}",
                        bytes_count,
                        cache_file_path.display()
                    );
                    fs::write(&cache_file_path, serialized_bytes)
                        .await
                        .map_err(|e| SpiderError::IoError(e.to_string()))?;
                    debug!(
                        "Cached response for {} ({} bytes)",
                        response.url, bytes_count
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to serialize response for caching {}: {}",
                        response.url, e
                    );
                }
            }
        } else {
            trace!(
                "Response status {} is not successful, skipping cache for: {}",
                response.status, response.url
            );
        }

        trace!("Continuing response: {}", response.url);
        Ok(MiddlewareAction::Continue(response))
    }
}
