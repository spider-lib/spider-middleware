//! Retry Middleware for handling failed requests.
//!
//! This module provides the `RetryMiddleware`, which automatically retries requests
//! that fail due to specific HTTP status codes or network errors (e.g., connection issues, timeouts).
//! It implements an exponential backoff strategy to space out retry attempts,
//! respecting a configurable maximum number of retries and a maximum delay between attempts.
//!
//! The middleware intercepts responses and errors, deciding whether to re-enqueue
//! the request for another attempt or to drop it if the retry limit is exceeded.

use async_trait::async_trait;
use std::time::Duration;
use tracing::{info, trace, warn};

use spider_util::error::SpiderError;
use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::request::Request;
use spider_util::response::Response;

/// Middleware that retries failed requests.
#[derive(Debug, Clone)]
pub struct RetryMiddleware {
    /// Maximum number of times to retry a request.
    pub max_retries: u32,
    /// HTTP status codes that should trigger a retry.
    pub retry_http_codes: Vec<u16>,
    /// Factor for exponential backoff (delay = backoff_factor * (2^retries)).
    pub backoff_factor: f64,
    /// Maximum delay between retries.
    pub max_delay: Duration,
}

impl Default for RetryMiddleware {
    fn default() -> Self {
        let middleware = RetryMiddleware {
            max_retries: 3,
            retry_http_codes: vec![500, 502, 503, 504, 408, 429],
            backoff_factor: 1.0,
            max_delay: Duration::from_secs(180),
        };
        info!("Initializing RetryMiddleware with config: {:?}", middleware);
        middleware
    }
}

impl RetryMiddleware {
    /// Creates a new `RetryMiddleware` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of times to retry a request.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets the HTTP status codes that should trigger a retry.
    pub fn retry_http_codes(mut self, retry_http_codes: Vec<u16>) -> Self {
        self.retry_http_codes = retry_http_codes;
        self
    }

    /// Sets the factor for exponential backoff.
    pub fn backoff_factor(mut self, backoff_factor: f64) -> Self {
        self.backoff_factor = backoff_factor;
        self
    }

    /// Sets the maximum delay between retries.
    pub fn max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for RetryMiddleware {
    fn name(&self) -> &str {
        "RetryMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        trace!("Processing response for URL: {} with status: {}", response.url, response.status);

        if self.retry_http_codes.contains(&response.status.as_u16()) {
            let mut request = response.request_from_response();
            let current_attempts = request.get_retry_attempts();

            if current_attempts < self.max_retries {
                request.increment_retry_attempts();
                let delay = self.calculate_delay(current_attempts);
                info!(
                    "Retrying {} (status: {}, attempt {}/{}) after {:?}",
                    request.url,
                    response.status,
                    current_attempts + 1,
                    self.max_retries,
                    delay
                );
                return Ok(MiddlewareAction::Retry(Box::new(request), delay));
            } else {
                warn!(
                    "Max retries ({}) reached for {} (status: {}). Dropping response.",
                    self.max_retries, request.url, response.status
                );
                return Ok(MiddlewareAction::Drop);
            }
        } else {
            trace!("Response status {} is not in retry codes, continuing", response.status);
        }

        Ok(MiddlewareAction::Continue(response))
    }

    async fn handle_error(
        &mut self,
        request: &Request,
        error: &SpiderError,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        trace!("Handling error for request {}: {:?}", request.url, error);

        if let SpiderError::ReqwestError(err_details) = error
            && (err_details.is_connect || err_details.is_timeout)
        {
            let mut new_request = request.clone();
            let current_attempts = new_request.get_retry_attempts();

            if current_attempts < self.max_retries {
                new_request.increment_retry_attempts();
                let delay = self.calculate_delay(current_attempts);
                info!(
                    "Retrying {} (error: {}, attempt {}/{}) after {:?}",
                    new_request.url,
                    err_details.message,
                    current_attempts + 1,
                    self.max_retries,
                    delay
                );
                return Ok(MiddlewareAction::Retry(Box::new(new_request), delay));
            } else {
                warn!(
                    "Max retries ({}) reached for {} (error: {}). Dropping request.",
                    self.max_retries, new_request.url, err_details.message
                );
                return Ok(MiddlewareAction::Drop);
            }
        } else {
            trace!("Error is not a retryable error, returning original error");
        }

        Err(error.clone())
    }
}

impl RetryMiddleware {
    fn calculate_delay(&self, retries: u32) -> Duration {
        let delay_secs = self.backoff_factor * (2.0f64.powi(retries as i32));
        let delay = Duration::from_secs_f64(delay_secs);
        delay.min(self.max_delay)
    }
}