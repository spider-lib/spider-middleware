# spider-middleware

Provides built-in middleware implementations for the `spider-lib` framework.

## Overview

The `spider-middleware` crate contains a comprehensive collection of middleware implementations that extend the functionality of web crawlers. Middlewares intercept and process requests and responses, enabling features like rate limiting, retries, user-agent rotation, and more.

## Available Middlewares

- **Rate Limiting**: Controls request rates to prevent server overload
- **Retries**: Automatically retries failed or timed-out requests
- **User-Agent Rotation**: Manages and rotates user agents
- **Referer Management**: Handles the `Referer` header
- **Cookies**: Persists cookies across requests to maintain sessions
- **HTTP Caching**: Caches responses to accelerate development
- **Robots.txt**: Adheres to `robots.txt` rules
- **Proxy**: Manages and rotates proxy servers

## Architecture

Each middleware implements the `Middleware` trait, allowing them to intercept requests before they're sent and responses after they're received. This enables flexible, composable behavior customization for crawlers.

## Usage

```rust
use spider_middleware::rate_limit::RateLimitMiddleware;
use spider_middleware::retry::RetryMiddleware;

// Add middlewares to your crawler
let crawler = CrawlerBuilder::new(MySpider)
    .add_middleware(RateLimitMiddleware::default())
    .add_middleware(RetryMiddleware::new())
    .build()
    .await?;
```

## Middleware Types

### Rate Limiting

Controls the frequency of requests to respect server resources and avoid being blocked. The `RateLimitMiddleware` offers two different rate limiting algorithms:

#### Adaptive Limiter (Default)
Dynamically adjusts delays based on response status codes. Increases delay on errors (429, 5xx) and decreases on successful responses.

**Configuration:**
```rust
use spider_middleware::rate_limit::{RateLimitMiddleware, Scope};

let rate_limit_middleware = RateLimitMiddleware::builder()
    .scope(Scope::Domain)  // Apply rate limits per domain (or Scope::Global)
    .limiter(AdaptiveLimiter::new(Duration::from_millis(500), true))  // Initial delay of 500ms with jitter
    .build();
```

#### Token Bucket Limiter
Enforces a fixed requests-per-second rate regardless of response status.

**Configuration:**
```rust
use spider_middleware::rate_limit::RateLimitMiddleware;

let rate_limit_middleware = RateLimitMiddleware::builder()
    .use_token_bucket_limiter(2)  // 2 requests per second
    .build();
```

### Retries

Automatically retries failed requests with configurable backoff strategies.

**Configuration:**
```rust
use spider_middleware::retry::RetryMiddleware;
use std::time::Duration;

let retry_middleware = RetryMiddleware::new()
    .max_retries(3)  // Maximum 3 retry attempts
    .retry_http_codes(vec![500, 502, 503, 504, 408, 429])  // Status codes to retry
    .backoff_factor(1.0)  // Backoff factor for exponential backoff
    .max_delay(Duration::from_secs(180));  // Maximum delay between retries
```

### User-Agent Rotation

Rotates user agent strings to avoid detection and blocking. Supports multiple rotation strategies and sources.

**Configuration:**
```rust
use spider_middleware::user_agent::{UserAgentMiddleware, UserAgentSource, UserAgentRotationStrategy, BuiltinUserAgentList};
use std::path::PathBuf;

// Using built-in user agents
let user_agent_middleware = UserAgentMiddleware::builder()
    .source(UserAgentSource::Builtin(BuiltinUserAgentList::Random))
    .strategy(UserAgentRotationStrategy::Random)
    .build()?;

// Using custom list
let user_agent_middleware = UserAgentMiddleware::builder()
    .source(UserAgentSource::List(vec![
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36".to_string(),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36".to_string(),
    ]))
    .strategy(UserAgentRotationStrategy::Sequential)
    .build()?;

// Using file source
let mut file_path = PathBuf::new();
file_path.push("user-agents.txt");
let user_agent_middleware = UserAgentMiddleware::builder()
    .source(UserAgentSource::File(file_path))
    .strategy(UserAgentRotationStrategy::Sticky)
    .session_duration(Duration::from_secs(300))  // 5 minutes for sticky session
    .build()?;
```

### Referer Management

Handles the referer header appropriately for requests to simulate natural browsing behavior.

**Configuration:**
```rust
use spider_middleware::referer::RefererMiddleware;

let referer_middleware = RefererMiddleware::new()
    .same_origin_only(true)       // Only use referers from the same origin
    .max_chain_length(1000)       // Maximum number of referers to keep in memory
    .include_fragment(false);     // Exclude URL fragments from referer
```

### Cookies

Manages cookies across requests to maintain sessions.

**Configuration:**
```rust
use spider_middleware::cookies::CookieMiddleware;

// Basic usage
let cookie_middleware = CookieMiddleware::new();

// Loading from JSON file
let cookie_middleware = CookieMiddleware::from_json("cookies.json").await?;

// Loading from Netscape cookie file
let cookie_middleware = CookieMiddleware::from_netscape_file("cookies.txt").await?;

// Loading from RFC6265 format
let cookie_middleware = CookieMiddleware::from_rfc6265("cookies.rfc6265").await?;
```

### HTTP Caching

Caches responses locally to speed up development and reduce server load.

**Configuration:**
```rust
use spider_middleware::http_cache::HttpCacheMiddleware;
use std::path::PathBuf;

let mut cache_dir = PathBuf::new();
cache_dir.push("cache");

let http_cache_middleware = HttpCacheMiddleware::builder()
    .cache_dir(cache_dir)
    .build()?;
```

### Robots.txt

Ensures compliance with robots.txt rules.

**Configuration:**
```rust
use spider_middleware::robots_txt::RobotsTxtMiddleware;
use std::time::Duration;

let robots_txt_middleware = RobotsTxtMiddleware::new()
    .cache_ttl(Duration::from_secs(86400))      // Cache TTL: 24 hours
    .cache_capacity(10_000)                     // Max cache entries
    .request_timeout(Duration::from_secs(5));   // Timeout for fetching robots.txt
```

### Proxy

Manages proxy servers for requests to avoid IP-based blocking.

**Configuration:**
```rust
use spider_middleware::proxy::{ProxyMiddleware, ProxySource, ProxyRotationStrategy};
use std::path::PathBuf;

// Using custom list
let proxy_middleware = ProxyMiddleware::builder()
    .source(ProxySource::List(vec![
        "http://proxy1.example.com:8080".to_string(),
        "http://proxy2.example.com:8080".to_string(),
    ]))
    .strategy(ProxyRotationStrategy::Sequential)
    .build()?;

// Using file source
let mut file_path = PathBuf::new();
file_path.push("proxies.txt");
let proxy_middleware = ProxyMiddleware::builder()
    .source(ProxySource::File(file_path))
    .strategy(ProxyRotationStrategy::Random)
    .build()?;

// Sticky failover strategy with block detection
let proxy_middleware = ProxyMiddleware::builder()
    .source(ProxySource::List(vec![
        "http://proxy1.example.com:8080".to_string(),
        "http://proxy2.example.com:8080".to_string(),
    ]))
    .strategy(ProxyRotationStrategy::StickyFailover)
    .with_block_detection_texts(vec!["Access Denied".to_string()])
    .build()?;
```

## Dependencies

This crate depends on:
- `spider-util`: For request and response data structures
- Various external crates for specific functionality (governor for rate limiting, reqwest for HTTP operations, etc.)

## License

This project is licensed under the MIT License - see the [LICENSE](../LICENSE) file for details.