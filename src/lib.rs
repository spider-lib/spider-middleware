//! # spider-middleware
//!
//! Built-in middleware implementations for the `spider-lib` framework.
//!
//! Provides middlewares for rate limiting, retries, user-agent rotation, cookies, and more.
//!
//! ## Example
//!
//! ```rust,ignore
//! use spider_middleware::rate_limit::RateLimitMiddleware;
//! use spider_middleware::retry::RetryMiddleware;
//!
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
