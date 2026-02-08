//! Commonly used items from the `spider-middleware` crate.
//!
//! This module provides a convenient way to import the most commonly used types
//! and traits from the `spider-middleware` crate in a single import.

pub use spider_util::request::Request;
pub use spider_util::response::Response;

// Core middleware (always available)
pub use crate::rate_limit::RateLimitMiddleware;
pub use crate::retry::RetryMiddleware;
pub use crate::referer::RefererMiddleware;

// Optional middleware (available when features are enabled)
#[cfg(feature = "middleware-user-agent")]
pub use crate::user_agent::UserAgentMiddleware;

#[cfg(feature = "middleware-cookies")]
pub use crate::cookies::CookieMiddleware;

#[cfg(feature = "middleware-cache")]
pub use crate::http_cache::HttpCacheMiddleware;

#[cfg(feature = "middleware-proxy")]
pub use crate::proxy::ProxyMiddleware;

#[cfg(feature = "middleware-robots")]
pub use crate::robots_txt::RobotsTxtMiddleware;

// Re-export the core middleware trait
pub use crate::middleware::{Middleware, MiddlewareAction};