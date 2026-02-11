//! Auto-Rotate Proxy Middleware for rotating proxies during crawling.
//!
//! This middleware manages and rotates proxy URLs for outgoing requests.
//! It supports loading proxies from a list or a file and offers different
//! rotation strategies.

use async_trait::async_trait;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use log::{info, warn};

use spider_util::error::SpiderError;
use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::request::Request;
use spider_util::response::Response;

/// Defines the strategy for rotating proxies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProxyRotationStrategy {
    /// Sequentially cycles through the available proxies.
    #[default]
    Sequential,
    /// Randomly selects a proxy from the available pool.
    Random,
    /// Uses one proxy until a failure is detected (based on status or body), then rotates.
    StickyFailover,
}

/// Defines the source from which proxies are loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProxySource {
    /// A direct list of proxy URLs.
    List(Vec<String>),
    /// Path to a file containing proxy URLs, one per line.
    File(PathBuf),
}

impl Default for ProxySource {
    fn default() -> Self {
        ProxySource::List(Vec::new())
    }
}

/// Builder for creating an `ProxyMiddleware`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyMiddlewareBuilder {
    source: ProxySource,
    strategy: ProxyRotationStrategy,
    block_detection_texts: Vec<String>,
}

impl ProxyMiddlewareBuilder {
    /// Sets the primary source for proxies.
    pub fn source(mut self, source: ProxySource) -> Self {
        self.source = source;
        self
    }

    /// Sets the strategy to use for rotating proxies.
    pub fn strategy(mut self, strategy: ProxyRotationStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Sets the texts to detect in the response body to trigger a proxy rotation.
    /// This is only used with the `StickyFailover` strategy.
    pub fn with_block_detection_texts(mut self, texts: Vec<String>) -> Self {
        self.block_detection_texts = texts;
        self
    }

    /// Builds the `ProxyMiddleware`.
    /// This can fail if a proxy source file is specified but cannot be read.
    pub fn build(self) -> Result<ProxyMiddleware, SpiderError> {
        let proxies = Arc::new(ProxyMiddleware::load_proxies(&self.source)?);

        let block_texts = if self.block_detection_texts.is_empty() {
            None
        } else {
            Some(self.block_detection_texts)
        };

        let middleware = ProxyMiddleware {
            strategy: self.strategy,
            proxies,
            current_index: AtomicUsize::new(0),
            block_detection_texts: block_texts,
        };

        info!(
            "Initializing ProxyMiddleware with config: {:?}",
            middleware
        );

        Ok(middleware)
    }
}

pub struct ProxyMiddleware {
    strategy: ProxyRotationStrategy,
    proxies: Arc<Vec<String>>,
    current_index: AtomicUsize,
    block_detection_texts: Option<Vec<String>>,
}

impl Debug for ProxyMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyMiddleware")
            .field("strategy", &self.strategy)
            .field("proxies", &format!("Pool({})", self.proxies.len()))
            .field("current_index", &self.current_index)
            .field("block_detection_texts", &self.block_detection_texts)
            .finish()
    }
}

impl ProxyMiddleware {
    /// Creates a new `ProxyMiddlewareBuilder` to start building the middleware.
    pub fn builder() -> ProxyMiddlewareBuilder {
        ProxyMiddlewareBuilder::default()
    }

    fn load_proxies(source: &ProxySource) -> Result<Vec<String>, SpiderError> {
        match source {
            ProxySource::List(list) => Ok(list.clone()),
            ProxySource::File(path) => Self::load_from_file(path),
        }
    }

    fn load_from_file(path: &Path) -> Result<Vec<String>, SpiderError> {
        if !path.exists() {
            return Err(SpiderError::IoError(
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Proxy file not found: {}", path.display()),
                )
                .to_string(),
            ));
        }
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let proxies: Vec<String> = reader
            .lines()
            .map_while(Result::ok)
            .filter(|line| !line.trim().is_empty())
            .collect();

        if proxies.is_empty() {
            warn!(
                "Proxy file {:?} is empty or contains no valid proxy URLs.",
                path
            );
        }
        Ok(proxies)
    }

    fn get_proxy(&self) -> Option<String> {
        if self.proxies.is_empty() {
            return None;
        }

        match self.strategy {
            ProxyRotationStrategy::Sequential => {
                let current = self.current_index.fetch_add(1, Ordering::SeqCst);
                let index = current % self.proxies.len();
                self.proxies.get(index).cloned()
            }
            ProxyRotationStrategy::Random => {
                let mut rng = rand::thread_rng();
                self.proxies.choose(&mut rng).cloned()
            }
            ProxyRotationStrategy::StickyFailover => {
                let current = self.current_index.load(Ordering::SeqCst);
                let index = current % self.proxies.len();
                self.proxies.get(index).cloned()
            }
        }
    }

    fn rotate_proxy(&self) {
        if !self.proxies.is_empty() {
            self.current_index.fetch_add(1, Ordering::SeqCst);
            info!("Proxy rotation triggered due to failure.");
        }
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for ProxyMiddleware {
    fn name(&self) -> &str {
        "ProxyMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        if let Some(proxy) = self.get_proxy() {
            request.meta.insert(Cow::Borrowed("proxy"), proxy.into());
        }
        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        if self.strategy != ProxyRotationStrategy::StickyFailover {
            return Ok(MiddlewareAction::Continue(response));
        }

        let mut rotate = false;
        let status = response.status;

        // Check for bad status codes
        if status.is_client_error() || status.is_server_error() {
            // e.g., 403 Forbidden, 429 Too Many Requests, 5xx errors
            rotate = true;
        }

        // Check for block texts in body if status is OK
        if status.is_success() && let Some(texts) = &self.block_detection_texts {
            let body_str = String::from_utf8_lossy(&response.body);
            if texts.iter().any(|text| body_str.contains(text)) {
                rotate = true;
                info!(
                    "Block detection text found in response body from {}",
                    response.url
                );
            }
        }

        if rotate {
            self.rotate_proxy();
        }

        Ok(MiddlewareAction::Continue(response))
    }

    async fn handle_error(
        &mut self,
        _request: &Request,
        error: &SpiderError,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        if self.strategy == ProxyRotationStrategy::StickyFailover {
            self.rotate_proxy();
        }

        // Pass the error along for other middlewares (like Retry) to handle.
        Err(error.clone())
    }
}