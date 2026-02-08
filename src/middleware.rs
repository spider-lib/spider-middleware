//! Core Middleware trait and related types for the `spider-core` framework.
//!
//! This module defines the `Middleware` trait, which is a fundamental abstraction
//! for injecting custom logic into the web crawling pipeline. Middlewares can
//! intercept and modify `Request`s before they are sent, `Response`s after they
//! are received, and handle errors that occur during the process.
//!
//! The `MiddlewareAction` enum provides a flexible way for middlewares to control
//! the subsequent flow of execution, allowing actions such to continuing processing,
//! retrying a request, dropping an item, or directly returning a response.

use async_trait::async_trait;
use std::any::Any;
use std::time::Duration;

use spider_util::error::SpiderError;
use spider_util::request::Request;
use spider_util::response::Response;

#[allow(clippy::large_enum_variant)]
/// Enum returned by middleware methods to control further processing.
pub enum MiddlewareAction<T> {
    /// Continue processing with the provided item.
    Continue(T),
    /// Retry the Request after the specified duration. (Only valid for Response processing)
    Retry(Box<Request>, Duration),
    /// Drop the item, stopping further processing.
    Drop,
    /// Return a Response directly, bypassing the downloader. (Only valid for Request processing)
    ReturnResponse(Response),
}

/// A trait for processing requests and responses.
#[async_trait]
pub trait Middleware<C: Send + Sync>: Any + Send + Sync + 'static {
    fn name(&self) -> &str;

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
        Ok(MiddlewareAction::Continue(response))
    }

    async fn handle_error(
        &mut self,
        _request: &Request,
        error: &SpiderError,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        // The default implementation is to just pass the error through by cloning it.
        Err(error.clone())
    }
}