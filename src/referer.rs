//! Referer Middleware for managing HTTP Referer headers.
//!
//! This module provides the `RefererMiddleware`, which automatically sets
//! the `Referer` HTTP header for outgoing requests. Its primary purpose
//! is to simulate natural browsing behavior, making web crawls appear
//! more legitimate and potentially bypassing certain anti-scraping measures.
//!
//! Key features include:
//! - Automatic tracking of the request chain to determine the appropriate referer.
//! - Configuration options for enforcing same-origin referers, limiting the
//!   referer chain length, and controlling the inclusion of URL fragments.
//! - Dynamic insertion of the `Referer` header into outgoing requests.

use async_trait::async_trait;
use dashmap::DashMap;
use reqwest::header::{HeaderValue, REFERER};
use std::sync::Arc;
use url::Url;

use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::error::SpiderError;
use spider_util::request::Request;
use spider_util::response::Response;
use log::{debug, info};

/// Referer middleware that automatically sets Referer headers
/// based on the navigation chain
#[derive(Debug, Clone)]
pub struct RefererMiddleware {
    /// Whether to use same-origin only referer
    pub same_origin_only: bool,
    /// Maximum referer chain length to keep in memory
    pub max_chain_length: usize,
    /// Whether to include fragment in referer URL
    pub include_fragment: bool,
    /// Map of request ID to referer URL
    referer_map: Arc<DashMap<String, Url>>,
}

impl Default for RefererMiddleware {
    fn default() -> Self {
        let middleware = RefererMiddleware {
            same_origin_only: true,
            max_chain_length: 1000,
            include_fragment: false,
            referer_map: Arc::new(DashMap::new()),
        };
        info!(
            "Initializing RefererMiddleware with config: {:?}",
            middleware
        );
        middleware
    }
}

impl RefererMiddleware {
    /// Create a new RefererMiddleware with default config
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether to use same-origin only referer.
    pub fn same_origin_only(mut self, same_origin_only: bool) -> Self {
        self.same_origin_only = same_origin_only;
        self
    }

    /// Set the maximum referer chain length to keep in memory.
    pub fn max_chain_length(mut self, max_chain_length: usize) -> Self {
        self.max_chain_length = max_chain_length;
        self
    }

    /// Set whether to include the fragment in the referer URL.
    pub fn include_fragment(mut self, include_fragment: bool) -> Self {
        self.include_fragment = include_fragment;
        self
    }

    /// Extract referer from request metadata and clean it
    fn get_referer_for_request(&self, request: &Request) -> Option<Url> {
        if let Some(referer_value) = request.meta.get("referer")
            && let Some(referer_str) = referer_value.value().as_str()
            && let Ok(url) = Url::parse(referer_str)
        {
            if self.same_origin_only {
                let request_origin = format!(
                    "{}://{}",
                    request.url.scheme(),
                    request.url.host_str().unwrap_or("")
                );
                let referer_origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));

                if request_origin == referer_origin {
                    return Some(self.clean_url(&url));
                }
            } else {
                return Some(self.clean_url(&url));
            }
        }

        None
    }

    /// Clean URL by removing fragments if configured
    fn clean_url(&self, url: &Url) -> Url {
        if !self.include_fragment && url.fragment().is_some() {
            let mut cleaned = url.clone();
            cleaned.set_fragment(None);
            cleaned
        } else {
            url.clone()
        }
    }

    /// Generate a unique key for request tracking
    fn request_key(&self, request: &Request) -> String {
        // Simple key based on URL and method
        format!("{}:{}", request.method, request.url)
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for RefererMiddleware {
    fn name(&self) -> &str {
        "RefererMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        mut request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let referer = self.get_referer_for_request(&request);
        let referer = if let Some(ref_from_meta) = referer {
            Some(ref_from_meta)
        } else {
            let request_key = self.request_key(&request);
            self.referer_map
                .get(&request_key)
                .map(|entry| entry.value().clone())
        };

        let referer = if let Some(ref_url) = referer {
            Some(ref_url)
        } else if let Some(parent_id) = request.meta.get("parent_request_id") {
            if let Some(parent_id_str) = parent_id.value().as_str() {
                self.referer_map
                    .get(parent_id_str)
                    .map(|entry| entry.value().clone())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(referer) = referer {
            match HeaderValue::from_str(referer.as_str()) {
                Ok(header_value) => {
                    request.headers.insert(REFERER, header_value);
                    debug!(
                        "Set Referer header to: {} for request: {}",
                        referer, request.url
                    );
                }
                Err(e) => {
                    debug!("Failed to set Referer header: {}", e);
                }
            }
        }

        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        let response_url = response.url.clone();
        let request = response.request_from_response();
        let request_id = format!("req_{:x}", seahash::hash(request.url.as_str().as_bytes()));

        // Store mapping:
        // 1. Request ID -> Response URL (for parent-child relationships)
        // 2. Request key -> Response URL (for direct lookups)

        let request_key = self.request_key(&request);
        let cleaned_url = self.clean_url(&response_url);

        if self.referer_map.len() < self.max_chain_length {
            self.referer_map.insert(request_key, cleaned_url.clone());
            self.referer_map.insert(request_id.clone(), cleaned_url);

            debug!(
                "Stored referer mapping for request {}: {}",
                request.url, response_url
            );
        }

        Ok(MiddlewareAction::Continue(response))
    }
}

