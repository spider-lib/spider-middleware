//! Cookie Middleware to manage the Set-Cookie header
//!
//! This middleware is responsible for managing cookies during the scraping process,
//! persisting them across requests to simulate a browser-like session.

use async_trait::async_trait;
use cookie::Cookie;
use cookie_store::CookieStore;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use url::Url;

use spider_util::error::SpiderError;
use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::request::Request;
use spider_util::response::Response;

/// Middleware for managing cookies across requests.
pub struct CookieMiddleware {
    pub store: Arc<Mutex<CookieStore>>,
}

impl CookieMiddleware {
    /// Creates a new `CookieMiddleware` with a shared `CookieStore`.
    pub fn new() -> Self {
        Self::with_store(CookieStore::default())
    }

    /// Creates a new `CookieMiddleware` with a pre-populated `CookieStore`.
    pub fn with_store(store: CookieStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    /// Load cookies from a JSON file, replacing all cookies in the store.
    /// The JSON format is the one used by the `cookie_store` crate.
    pub async fn from_json<P: AsRef<Path>>(path: P) -> Result<Self, SpiderError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let store =
            CookieStore::load_json(reader).map_err(|e| SpiderError::GeneralError(e.to_string()))?;

        Ok(Self::with_store(store))
    }

    /// Load cookies from a Netscape cookie file.
    /// This will add to, not replace, the existing cookies in the store.
    pub async fn from_netscape_file<P: AsRef<Path>>(path: P) -> Result<Self, SpiderError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut store = CookieStore::default();

        for line in reader.lines() {
            let line = line?;
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 7 {
                return Err(SpiderError::GeneralError(format!(
                    "Malformed Netscape cookie line: Expected 7 parts, got {}",
                    parts.len()
                )));
            }

            let domain = parts[0];
            let secure = parts[3].eq_ignore_ascii_case("TRUE");
            let path = parts[2];
            let expires_timestamp = parts[4].parse::<i64>().map_err(|e| {
                SpiderError::GeneralError(format!("Invalid timestamp format: {}", e))
            })?;

            let expires = if expires_timestamp == 0 {
                None
            } else {
                Some(
                    OffsetDateTime::from_unix_timestamp(expires_timestamp).map_err(|e| {
                        SpiderError::GeneralError(format!("Invalid timestamp value: {}", e))
                    })?,
                )
            };

            let name = parts[5].to_string();
            let value = parts[6].to_string();

            let mut cookie_builder = Cookie::build(name, value).path(path).secure(secure);
            if let Some(expires) = expires {
                cookie_builder = cookie_builder.expires(expires);
            }

            let mut domain_for_url = domain;
            if domain_for_url.starts_with('.') {
                domain_for_url = &domain_for_url[1..];
            }

            let url_str = format!(
                "{}://{}",
                if secure { "https" } else { "http" },
                domain_for_url
            );

            let url = Url::parse(&url_str)?;
            let cookie = cookie_builder
                .domain(domain.to_string())
                .finish()
                .into_owned();

            store.store_response_cookies(std::iter::once(cookie), &url);
        }

        Ok(Self::with_store(store))
    }

    /// Load cookies from a file where each line is a `Set-Cookie` header value.
    /// The `Domain` attribute must be explicitly set in each cookie line.
    pub async fn from_rfc6265<P: AsRef<Path>>(path: P) -> Result<Self, SpiderError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut store = CookieStore::default();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let cookie =
                Cookie::parse(line) // Fixed parse_owned to parse
                    .map_err(|e| {
                        SpiderError::GeneralError(format!("Failed to parse cookie: {}", e))
                    })?;

            let domain = cookie.domain().ok_or_else(|| {
                SpiderError::GeneralError(
                    "Cookie in file must have an explicit Domain attribute".to_string(),
                )
            })?;

            let secure = cookie.secure().unwrap_or(false);

            let url_str = format!("{}://{}", if secure { "https" } else { "http" }, domain);
            let url = Url::parse(&url_str)?;

            store.store_response_cookies(std::iter::once(cookie), &url);
        }

        Ok(Self::with_store(store))
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for CookieMiddleware {
    fn name(&self) -> &str {
        "CookieMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        mut request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let store = self.store.lock().await;

        let cookie_header = store
            .get_request_values(&request.url)
            .map(|(name, value)| format!("{}={}", name, value))
            .collect::<Vec<_>>()
            .join("; ");

        if !cookie_header.is_empty() {
            request.headers.insert(
                http::header::COOKIE,
                http::HeaderValue::from_str(&cookie_header)?,
            );
        }

        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        let cookies_to_store = response
            .headers
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|val| val.to_str().ok())
            .filter_map(|s| Cookie::parse(s).ok());

        self.store
            .lock()
            .await
            .store_response_cookies(cookies_to_store.map(|c| c.into_owned()), &response.url);

        Ok(MiddlewareAction::Continue(response))
    }
}

impl Default for CookieMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

