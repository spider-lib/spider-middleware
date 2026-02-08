//! Robots.txt Middleware for respecting website crawling policies.
//!
//! This module provides the `RobotsTxtMiddleware`, which automatically
//! fetches, caches, and interprets `robots.txt` files from websites.
//! Before each outgoing request, this middleware checks if the request's
//! URL and User-Agent are permitted by the target host's `robots.txt` rules.
//!
//! This ensures that the crawler adheres to the website's specified crawling
//! policies, preventing access to disallowed paths and promoting polite web scraping.
//! It uses a caching mechanism to avoid repeatedly fetching `robots.txt` files.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http::header::USER_AGENT;
use moka::future::Cache;
use robotstxt::DefaultMatcher;
use tracing::{debug, info, warn};

use spider_util::error::SpiderError;
use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::request::Request;
use reqwest::StatusCode;
use bytes::Bytes;
use reqwest::Client;

/// A simple HTTP client trait for fetching web content.
#[async_trait]
pub trait SimpleHttpClient: Send + Sync {
    /// Fetches the content of a URL as text.
    async fn get_text(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<(StatusCode, Bytes), SpiderError>;
}

// Implement the trait for reqwest::Client
#[async_trait]
impl SimpleHttpClient for Client {
    async fn get_text(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<(StatusCode, Bytes), SpiderError> {
        let request_builder = self.get(url).timeout(timeout);
        let response = request_builder.send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        Ok((status, body))
    }
}

/// Robots.txt middleware
#[derive(Debug)]
pub struct RobotsTxtMiddleware {
    cache_ttl: Duration,
    cache_capacity: u64,
    request_timeout: Duration,
    cache: Cache<String, Arc<String>>,
}

impl Default for RobotsTxtMiddleware {
    fn default() -> Self {
        let cache_ttl = Duration::from_secs(60 * 60 * 24);
        let cache_capacity = 10_000;
        let cache = Cache::builder()
            .time_to_live(cache_ttl)
            .max_capacity(cache_capacity)
            .build();

        let middleware = Self {
            cache_ttl,
            cache_capacity,
            request_timeout: Duration::from_secs(5),
            cache,
        };
        info!(
            "Initializing RobotsTxtMiddleware with config: {:?}",
            middleware
        );
        middleware
    }
}

impl RobotsTxtMiddleware {
    /// Creates a new `RobotsTxtMiddleware` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the time-to-live for the cache.
    pub fn cache_ttl(mut self, cache_ttl: Duration) -> Self {
        self.cache_ttl = cache_ttl;
        self.rebuild_cache();
        self
    }

    /// Set the max capacity for the cache.
    pub fn cache_capacity(mut self, cache_capacity: u64) -> Self {
        self.cache_capacity = cache_capacity;
        self.rebuild_cache();
        self
    }

    /// Set the timeout for fetching robots.txt files.
    pub fn request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    /// Rebuilds the cache with the current settings.
    fn rebuild_cache(&mut self) {
        self.cache = Cache::builder()
            .time_to_live(self.cache_ttl)
            .max_capacity(self.cache_capacity)
            .build();
    }

    async fn fetch_robots_content<C: SimpleHttpClient>(
        &self,
        client: &C,
        origin: &str,
    ) -> Arc<String> {
        let robots_url = format!("{}/robots.txt", origin);
        debug!("Fetching robots.txt from: {}", robots_url);

        let permissive = || Arc::new(String::new());

        match client.get_text(&robots_url, self.request_timeout).await {
            Ok((status, body)) if status.is_success() => match String::from_utf8(body.into()) {
                Ok(text) => Arc::new(text),
                Err(e) => {
                    warn!("Failed to read robots.txt {}: {}", robots_url, e);
                    permissive()
                }
            },
            Ok((status, _)) => {
                debug!(
                    "robots.txt {} returned {} — allowing all",
                    robots_url, status
                );
                permissive()
            }
            Err(e) => {
                warn!("Failed to fetch robots.txt {}: {}", robots_url, e);
                permissive()
            }
        }
    }
}

#[async_trait]
impl<C: SimpleHttpClient> Middleware<C> for RobotsTxtMiddleware {
    fn name(&self) -> &str {
        "RobotsTxtMiddleware"
    }

    async fn process_request(
        &mut self,
        client: &C,
        request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let url = request.url.clone();
        let origin = match url.origin().unicode_serialization() {
            s if s == "null" => return Ok(MiddlewareAction::Continue(request)),
            s => s,
        };

        let robots_body = match self.cache.get(&origin).await {
            Some(body) => body,
            None => {
                let body = self.fetch_robots_content(client, &origin).await;
                self.cache.insert(origin.clone(), body.clone()).await;
                body
            }
        };

        if let Some(user_agent) = request.headers.get(USER_AGENT) {
            let ua = user_agent
                .to_str()
                .map_err(|e| SpiderError::HeaderValueError(e.to_string()))?;

            let mut matcher = DefaultMatcher::default();
            if matcher.one_agent_allowed_by_robots(robots_body.as_str(), ua, url.as_str()) {
                return Ok(MiddlewareAction::Continue(request));
            }
        }

        debug!("Blocked by robots.txt: {}", url);
        Err(SpiderError::BlockedByRobotsTxt)
    }
}

