//! # spider-middleware
//!
//! Provides built-in middleware implementations for the `spider-lib` framework.
//!
//! ## Overview
//!
//! The `spider-middleware` crate contains a comprehensive collection of middleware
//! implementations that extend the functionality of web crawlers. Middlewares
//! intercept and process requests and responses, enabling features like rate
//! limiting, retries, user-agent rotation, and more.
//!
//! ## Available Middlewares
//!
//! - **Rate Limiting**: Controls request rates to prevent server overload
//! - **Retries**: Automatically retries failed or timed-out requests
//! - **Referer Management**: Handles the `Referer` header
//! - **User-Agent Rotation**: Manages and rotates user agents (feature: `middleware-user-agent`)
//! - **Cookies**: Persists cookies across requests to maintain sessions (feature: `middleware-cookies`)
//! - **HTTP Caching**: Caches responses to accelerate development (feature: `middleware-cache`)
//! - **Robots.txt**: Adheres to `robots.txt` rules (feature: `middleware-robots`)
//! - **Proxy**: Manages and rotates proxy servers (feature: `middleware-proxy`)
//!
//! ## Architecture
//!
//! Each middleware implements the `Middleware` trait, allowing them to intercept
//! requests before they are sent and responses after they are received. This
//! enables flexible, composable behavior customization for crawlers.
//!
//! ## Example
//!
//! ```rust,ignore
//! use spider_middleware::rate_limit::RateLimitMiddleware;
//! use spider_middleware::retry::RetryMiddleware;
//!
//! // Add middlewares to your crawler
//! let crawler = CrawlerBuilder::new(MySpider)
//!     .add_middleware(RateLimitMiddleware::default())
//!     .add_middleware(RetryMiddleware::new())
//!     .build()
//!     .await?;
//! ```

pub mod middleware;
pub mod rate_limit;
pub mod referer;
pub mod request;
pub mod retry;

pub use spider_util::request::Request;
pub use spider_util::response::Response;

pub mod prelude;

#[cfg(feature = "middleware-user-agent")]
pub mod user_agent;

#[cfg(feature = "middleware-cookies")]
pub mod cookies;

#[cfg(feature = "middleware-cache")]
pub mod http_cache;

#[cfg(feature = "middleware-proxy")]
pub mod proxy;

#[cfg(feature = "middleware-robots")]
pub mod robots_txt;
