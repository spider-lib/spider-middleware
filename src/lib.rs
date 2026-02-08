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
//! - **User-Agent Rotation**: Manages and rotates user agents
//! - **Referer Management**: Handles the `Referer` header
//! - **Cookies**: Persists cookies across requests to maintain sessions
//! - **HTTP Caching**: Caches responses to accelerate development
//! - **Robots.txt**: Adheres to `robots.txt` rules
//! - **Proxy**: Manages and rotates proxy servers
//!
//! ## Architecture
//!
//! Each middleware implements the `Middleware` trait, allowing them to intercept
//! requests before they're sent and responses after they're received. This
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
pub mod user_agent;

pub use spider_util::request::Request;
pub use spider_util::response::Response;

pub mod prelude;
pub mod cookies;
pub mod http_cache;
pub mod proxy;
pub mod robots_txt;
