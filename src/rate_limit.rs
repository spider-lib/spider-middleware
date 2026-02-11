//! Rate Limit Middleware for controlling request frequency.
//!
//! This module provides the `RateLimitMiddleware`, designed to manage the rate
//! at which HTTP requests are sent to target servers. It helps prevent
//! overloading websites and respects server-side rate limits, making crawls
//! more robust and polite.
//!
//! The middleware supports:
//! - **Different scopes:** Applying rate limits globally or per-domain.
//! - **Pluggable limiters:** Offering an `AdaptiveLimiter` that dynamically adjusts
//!   delays based on response status (e.g., increasing delay on errors, decreasing on success),
//!   and a `TokenBucketLimiter` for enforcing a fixed requests-per-second rate.
//!
//! This flexibility allows for fine-tuned control over crawl speed and server interaction.

use async_trait::async_trait;
use moka::future::Cache;
use rand::distributions::{Distribution, Uniform};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};
use log::{debug, info};

use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter as GovernorRateLimiter};
use std::num::NonZeroU32;

use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::error::SpiderError;
use spider_util::request::Request;
use spider_util::response::Response;

/// Determines the scope at which rate limits are applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// A single global rate limit for all requests.
    Global,
    /// A separate rate limit for each domain.
    Domain,
}

/// A trait for asynchronous, stateful rate limiters.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Blocks until a request is allowed to proceed.
    async fn acquire(&self);
    /// Adjusts the rate limit based on the response.
    async fn adjust(&self, response: &Response);
    /// Returns the current delay between requests.
    async fn current_delay(&self) -> Duration;
}

const INITIAL_DELAY: Duration = Duration::from_millis(500);
const MIN_DELAY: Duration = Duration::from_millis(50);
const MAX_DELAY: Duration = Duration::from_secs(60);

const ERROR_PENALTY_MULTIPLIER: f64 = 1.5;
const SUCCESS_DECAY_MULTIPLIER: f64 = 0.95;
const FORBIDDEN_PENALTY_MULTIPLIER: f64 = 1.2;

struct AdaptiveState {
    delay: Duration,
    next_allowed_at: Instant,
}

/// An adaptive rate limiter that adjusts the delay based on response status.
pub struct AdaptiveLimiter {
    state: Mutex<AdaptiveState>,
    jitter: bool,
}

impl AdaptiveLimiter {
    /// Creates a new `AdaptiveLimiter` with a given initial delay and jitter setting.
    pub fn new(initial_delay: Duration, jitter: bool) -> Self {
        Self {
            state: Mutex::new(AdaptiveState {
                delay: initial_delay,
                next_allowed_at: Instant::now(),
            }),
            jitter,
        }
    }

    fn apply_jitter(&self, delay: Duration) -> Duration {
        if !self.jitter || delay.is_zero() {
            return delay;
        }

        let max_jitter = Duration::from_millis(500);
        let jitter_window = delay.mul_f64(0.25).min(max_jitter);

        let low = delay.saturating_sub(jitter_window);
        let high = delay + jitter_window;

        let mut rng = rand::thread_rng();
        let uniform = Uniform::new_inclusive(low, high);
        uniform.sample(&mut rng)
    }
}

#[async_trait]
impl RateLimiter for AdaptiveLimiter {
    async fn acquire(&self) {
        let sleep_duration = {
            let mut state = self.state.lock().await;
            let now = Instant::now();

            let delay = state.delay;
            if now < state.next_allowed_at {
                let wait = state.next_allowed_at - now;
                state.next_allowed_at += delay;
                wait
            } else {
                state.next_allowed_at = now + delay;
                Duration::ZERO
            }
        };

        let sleep_duration = self.apply_jitter(sleep_duration);
        if !sleep_duration.is_zero() {
            debug!("Rate limiting: sleeping for {:?}", sleep_duration);
            sleep(sleep_duration).await;
        }
    }

    async fn adjust(&self, response: &Response) {
        let mut state = self.state.lock().await;

        let old_delay = state.delay;
        let status = response.status.as_u16();
        let new_delay = match status {
            200..=399 => state.delay.mul_f64(SUCCESS_DECAY_MULTIPLIER),
            403 => state.delay.mul_f64(FORBIDDEN_PENALTY_MULTIPLIER),
            429 | 500..=599 => state.delay.mul_f64(ERROR_PENALTY_MULTIPLIER),
            _ => state.delay,
        };

        state.delay = new_delay.clamp(MIN_DELAY, MAX_DELAY);

        if old_delay != state.delay {
            debug!(
                "Adjusting delay for status {}: {:?} -> {:?}",
                status, old_delay, state.delay
            );
        }
    }

    async fn current_delay(&self) -> Duration {
        self.state.lock().await.delay
    }
}

/// A rate limiter that uses a token bucket algorithm for a fixed rate.
pub struct TokenBucketLimiter {
    limiter: Arc<GovernorRateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
}

impl TokenBucketLimiter {
    /// Creates a new `TokenBucketLimiter` with the specified rate (requests per second).
    pub fn new(requests_per_second: u32) -> Self {
        let quota = Quota::per_second(
            NonZeroU32::new(requests_per_second).expect("requests_per_second must be non-zero"),
        );
        TokenBucketLimiter {
            limiter: Arc::new(GovernorRateLimiter::direct_with_clock(
                quota,
                &DefaultClock::default(),
            )),
        }
    }
}

#[async_trait]
impl RateLimiter for TokenBucketLimiter {
    async fn acquire(&self) {
        self.limiter.until_ready().await;
    }

    /// A fixed-rate limiter does not adjust based on responses.
    async fn adjust(&self, _response: &Response) {
        // No-op for a fixed-rate limiter
    }

    async fn current_delay(&self) -> Duration {
        // Token bucket doesn't directly expose a "current delay", but rather
        // manages when the next request is allowed.
        // Returning Duration::ZERO is a simplification, as delay is handled by `acquire`.
        Duration::ZERO
    }
}

/// A middleware for rate limiting requests.
pub struct RateLimitMiddleware {
    scope: Scope,
    limiters: Cache<String, Arc<dyn RateLimiter>>,
    limiter_factory: Arc<dyn Fn() -> Arc<dyn RateLimiter> + Send + Sync>,
}

impl RateLimitMiddleware {
    /// Creates a new `RateLimitMiddlewareBuilder`.
    pub fn builder() -> RateLimitMiddlewareBuilder {
        RateLimitMiddlewareBuilder::default()
    }

    fn scope_key(&self, request: &Request) -> String {
        match self.scope {
            Scope::Global => "global".to_string(),
            Scope::Domain => spider_util::utils::normalize_origin(request),
        }
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for RateLimitMiddleware {
    fn name(&self) -> &str {
        "RateLimitMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let key = self.scope_key(&request);

        let limiter = self
            .limiters
            .get_with(key.clone(), async { (self.limiter_factory)() })
            .await;

        let current_delay = limiter.current_delay().await;
        debug!(
            "Acquiring lock for key '{}' (delay: {:?})",
            key, current_delay
        );

        limiter.acquire().await;
        Ok(MiddlewareAction::Continue(request))
    }

    async fn process_response(
        &mut self,
        response: Response,
    ) -> Result<MiddlewareAction<Response>, SpiderError> {
        let key = self.scope_key(&response.request_from_response());

        if let Some(limiter) = self.limiters.get(&key).await {
            let old_delay = limiter.current_delay().await;
            limiter.adjust(&response).await;
            let new_delay = limiter.current_delay().await;
            if old_delay != new_delay {
                debug!(
                    "Adjusted rate limit for key '{}': {:?} -> {:?}",
                    key, old_delay, new_delay
                );
            }
        }

        Ok(MiddlewareAction::Continue(response))
    }
}

/// Builder for `RateLimitMiddleware`.
pub struct RateLimitMiddlewareBuilder {
    scope: Scope,
    cache_ttl: Duration,
    cache_capacity: u64,
    limiter_factory: Box<dyn Fn() -> Arc<dyn RateLimiter> + Send + Sync>,
}

impl Default for RateLimitMiddlewareBuilder {
    fn default() -> Self {
        Self {
            scope: Scope::Domain,
            cache_ttl: Duration::from_secs(3600),
            cache_capacity: 10_000,
            limiter_factory: Box::new(|| Arc::new(AdaptiveLimiter::new(INITIAL_DELAY, true))),
        }
    }
}

impl RateLimitMiddlewareBuilder {
    /// Sets the scope for the rate limiter.
    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Configures the builder to use a `TokenBucketLimiter` with the specified requests per second.
    pub fn use_token_bucket_limiter(mut self, requests_per_second: u32) -> Self {
        self.limiter_factory =
            Box::new(move || Arc::new(TokenBucketLimiter::new(requests_per_second)));
        self
    }

    /// Sets a specific rate limiter instance to be used.
    pub fn limiter(mut self, limiter: impl RateLimiter + 'static) -> Self {
        let arc = Arc::new(limiter);
        self.limiter_factory = Box::new(move || arc.clone());
        self
    }

    /// Sets a factory function for creating rate limiters.
    pub fn limiter_factory(
        mut self,
        factory: impl Fn() -> Arc<dyn RateLimiter> + Send + Sync + 'static,
    ) -> Self {
        self.limiter_factory = Box::new(factory);
        self
    }

    /// Builds the `RateLimitMiddleware`.
    pub fn build(self) -> RateLimitMiddleware {
        info!(
            "Initializing RateLimitMiddleware with config: scope={:?}, cache_ttl={:?}, cache_capacity={}",
            self.scope, self.cache_ttl, self.cache_capacity
        );
        RateLimitMiddleware {
            scope: self.scope,
            limiters: Cache::builder()
                .time_to_idle(self.cache_ttl)
                .max_capacity(self.cache_capacity)
                .build(),
            limiter_factory: self.limiter_factory.into(),
        }
    }
}
