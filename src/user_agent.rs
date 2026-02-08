//! User-Agent Middleware for rotating User-Agents during crawling.
//!
//! This module provides `UserAgentMiddleware` for managing and rotating User-Agent strings for
//! outgoing requests. It supports various rotation strategies and allows for detailed
//! configuration on a per-domain basis.

use async_trait::async_trait;
use dashmap::DashMap;
use moka::sync::Cache;
use reqwest::header::{HeaderValue, USER_AGENT};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::{debug, info, warn};
use ua_generator::ua::*;

use rand::seq::SliceRandom;

use spider_util::error::SpiderError;
use crate::middleware::{Middleware, MiddlewareAction};
use spider_util::request::Request;

/// Defines the strategy for rotating User-Agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UserAgentRotationStrategy {
    /// Randomly selects a User-Agent from the available pool.
    #[default]
    Random,
    /// Sequentially cycles through the available User-Agents.
    Sequential,
    /// Selects a User-Agent on first encounter with a domain and uses it for all subsequent requests to that domain.
    Sticky,
    /// Selects a User-Agent on first encounter with a domain and uses it for a configured duration (session).
    StickySession,
}

/// Predefined lists of User-Agents for common scenarios.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinUserAgentList {
    /// Generic Chrome User-Agents.
    Chrome,
    /// Chrome User-Agents on Linux.
    ChromeLinux,
    /// Chrome User-Agents on Mac.
    ChromeMac,
    /// Chrome Mobile User-Agents.
    ChromeMobile,
    /// Chrome Tablet User-Agents.
    ChromeTablet,
    /// Chrome User-Agents on Windows.
    ChromeWindows,
    /// Generic Firefox User-Agents.
    Firefox,
    /// Firefox User-Agents on Linux.
    FirefoxLinux,
    /// Firefox User-Agents on Mac.
    FirefoxMac,
    /// Firefox Mobile User-Agents.
    FirefoxMobile,
    /// Firefox Tablet User-Agents.
    FirefoxTablet,
    /// Firefox User-Agents on Windows.
    FirefoxWindows,
    /// Generic Safari User-Agents.
    Safari,
    /// Safari User-Agents on Mac.
    SafariMac,
    /// Safari Mobile User-Agents.
    SafariMobile,
    /// Safari Tablet User-Agents.
    SafariTablet,
    /// Safari User-Agents on Windows.
    SafariWindows,
    /// A random selection from all available User-Agents.
    Random,
}

/// Defines the source from which User-Agents are loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserAgentSource {
    /// A direct list of User-Agent strings.
    List(Vec<String>),
    /// Path to a file containing User-Agent strings, one per line.
    File(PathBuf),
    /// Use a predefined, built-in list of User-Agents.
    Builtin(BuiltinUserAgentList),
    /// No User-Agent source specified, will fallback to a default if available.
    None,
}

impl Default for UserAgentSource {
    fn default() -> Self {
        UserAgentSource::Builtin(BuiltinUserAgentList::Random)
    }
}

/// Custom serializer for Arc<String>
fn serialize_arc_string<S>(x: &Arc<String>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(x.as_str())
}

/// Custom deserializer for Arc<String>
fn deserialize_arc_string<'de, D>(deserializer: D) -> Result<Arc<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(Arc::new(s))
}

/// Represents a User-Agent profile, including the User-Agent string and other associated headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAgentProfile {
    /// The User-Agent string.
    #[serde(serialize_with = "serialize_arc_string", deserialize_with = "deserialize_arc_string")]
    pub user_agent: Arc<String>,
    /// Additional headers that should be sent with this User-Agent to mimic a real browser.
    #[serde(default)]
    pub headers: DashMap<String, String>,
}

impl From<String> for UserAgentProfile {
    fn from(user_agent: String) -> Self {
        UserAgentProfile {
            user_agent: Arc::new(user_agent),
            headers: DashMap::new(),
        }
    }
}

impl From<&str> for UserAgentProfile {
    fn from(user_agent: &str) -> Self {
        UserAgentProfile {
            user_agent: Arc::new(user_agent.to_string()),
            headers: DashMap::new(),
        }
    }
}

/// Builder for creating a `UserAgentMiddleware`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserAgentMiddlewareBuilder {
    source: UserAgentSource,
    strategy: UserAgentRotationStrategy,
    fallback_user_agent: Option<String>,
    per_domain_source: DashMap<String, UserAgentSource>,
    per_domain_strategy: DashMap<String, UserAgentRotationStrategy>,
    session_duration: Option<Duration>,
}

impl UserAgentMiddlewareBuilder {
    /// Sets the primary source for User-Agents.
    pub fn source(mut self, source: UserAgentSource) -> Self {
        self.source = source;
        self
    }

    /// Sets the default strategy to use for rotating User-Agents.
    pub fn strategy(mut self, strategy: UserAgentRotationStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Sets the duration for a "sticky session" in the `StickySession` strategy.
    pub fn session_duration(mut self, duration: Duration) -> Self {
        self.session_duration = Some(duration);
        self
    }

    /// Sets a fallback User-Agent to use if no other User-Agents are available.
    pub fn fallback_user_agent(mut self, fallback_user_agent: String) -> Self {
        self.fallback_user_agent = Some(fallback_user_agent);
        self
    }

    /// Adds a domain-specific User-Agent source.
    pub fn per_domain_source(self, domain: String, source: UserAgentSource) -> Self {
        self.per_domain_source.insert(domain, source);
        self
    }

    /// Adds a domain-specific User-Agent rotation strategy, overriding the default.
    pub fn per_domain_strategy(self, domain: String, strategy: UserAgentRotationStrategy) -> Self {
        self.per_domain_strategy.insert(domain, strategy);
        self
    }

    /// Builds the `UserAgentMiddleware`.
    /// This can fail if a User-Agent source file is specified but cannot be read.
    pub fn build(self) -> Result<UserAgentMiddleware, SpiderError> {
        let default_pool = Arc::new(UserAgentMiddleware::load_user_agents(&self.source)?);

        let domain_cache = Cache::builder()
            .time_to_live(Duration::from_secs(30 * 60)) // 30 minutes
            .build();

        for entry in self.per_domain_source.iter() {
            let domain = entry.key().clone();
            let source = entry.value().clone();
            let pool = Arc::new(UserAgentMiddleware::load_user_agents(&source)?);
            domain_cache.insert(domain, pool);
        }

        let session_cache = Cache::builder()
            .time_to_live(self.session_duration.unwrap_or(Duration::from_secs(5 * 60)))
            .build();

        let middleware = UserAgentMiddleware {
            strategy: self.strategy,
            fallback_user_agent: self.fallback_user_agent,
            domain_cache,
            default_pool,
            sticky_cache: DashMap::new(),
            session_cache,
            per_domain_strategy: self.per_domain_strategy,
            current_index: AtomicUsize::new(0),
        };

        info!(
            "Initializing UserAgentMiddleware with config: {:?}",
            middleware
        );

        Ok(middleware)
    }
}

pub struct UserAgentMiddleware {
    strategy: UserAgentRotationStrategy,
    fallback_user_agent: Option<String>,
    domain_cache: Cache<String, Arc<Vec<UserAgentProfile>>>,
    default_pool: Arc<Vec<UserAgentProfile>>,
    sticky_cache: DashMap<String, UserAgentProfile>,
    session_cache: Cache<String, UserAgentProfile>,
    per_domain_strategy: DashMap<String, UserAgentRotationStrategy>,
    current_index: AtomicUsize,
}

impl Debug for UserAgentMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserAgentMiddleware")
            .field("strategy", &self.strategy)
            .field("fallback_user_agent", &self.fallback_user_agent)
            .field(
                "domain_cache",
                &format!("Cache({})", self.domain_cache.weighted_size()),
            )
            .field(
                "default_pool",
                &format!("Pool({})", self.default_pool.len()),
            )
            .field(
                "sticky_cache",
                &format!("DashMap({})", self.sticky_cache.len()),
            )
            .field(
                "session_cache",
                &format!("Cache({})", self.session_cache.weighted_size()),
            )
            .field(
                "per_domain_strategy",
                &format!("DashMap({})", self.per_domain_strategy.len()),
            )
            .field("current_index", &self.current_index)
            .finish()
    }
}

impl UserAgentMiddleware {
    /// Creates a new `UserAgentMiddlewareBuilder` to start building a `UserAgentMiddleware`.
    pub fn builder() -> UserAgentMiddlewareBuilder {
        UserAgentMiddlewareBuilder::default()
    }

    fn load_user_agents(source: &UserAgentSource) -> Result<Vec<UserAgentProfile>, SpiderError> {
        match source {
            UserAgentSource::List(list) => Ok(list
                .iter()
                .map(|ua| UserAgentProfile::from(ua.clone()))
                .collect()),
            UserAgentSource::File(path) => Self::load_from_file(path),
            UserAgentSource::Builtin(builtin_list) => {
                Ok(Self::load_builtin_user_agents(builtin_list))
            }
            UserAgentSource::None => Ok(Vec::new()),
        }
    }

    fn load_from_file(path: &Path) -> Result<Vec<UserAgentProfile>, SpiderError> {
        if !path.exists() {
            return Err(SpiderError::IoError(
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("User-agent file not found: {}", path.display()),
                )
                .to_string(),
            ));
        }
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let user_agents: Vec<UserAgentProfile> = reader
            .lines()
            .map_while(Result::ok)
            .filter(|line| !line.trim().is_empty())
            .map(UserAgentProfile::from)
            .collect();

        if user_agents.is_empty() {
            warn!(
                "User-Agent file {:?} is empty or contains no valid User-Agents.",
                path
            );
        }
        Ok(user_agents)
    }

    fn load_builtin_user_agents(list_type: &BuiltinUserAgentList) -> Vec<UserAgentProfile> {
        let ua = match list_type {
            BuiltinUserAgentList::Chrome => STATIC_CHROME_AGENTS,
            BuiltinUserAgentList::ChromeLinux => STATIC_CHROME_LINUX_AGENTS,
            BuiltinUserAgentList::ChromeMac => STATIC_CHROME_MAC_AGENTS,
            BuiltinUserAgentList::ChromeMobile => STATIC_CHROME_MOBILE_AGENTS,
            BuiltinUserAgentList::ChromeTablet => STATIC_CHROME_TABLET_AGENTS,
            BuiltinUserAgentList::ChromeWindows => STATIC_CHROME_WINDOWS_AGENTS,
            BuiltinUserAgentList::Firefox => STATIC_FIREFOX_AGENTS,
            BuiltinUserAgentList::FirefoxLinux => STATIC_FIREFOX_LINUX_AGENTS,
            BuiltinUserAgentList::FirefoxMac => STATIC_FIREFOX_MAC_AGENTS,
            BuiltinUserAgentList::FirefoxMobile => STATIC_FIREFOX_MOBILE_AGENTS,
            BuiltinUserAgentList::FirefoxTablet => STATIC_FIREFOX_TABLET_AGENTS,
            BuiltinUserAgentList::FirefoxWindows => STATIC_FIREFOX_WINDOWS_AGENTS,
            BuiltinUserAgentList::Safari => STATIC_SAFARI_AGENTS,
            BuiltinUserAgentList::SafariMac => STATIC_SAFARI_MAC_AGENTS,
            BuiltinUserAgentList::SafariMobile => STATIC_SAFARI_MOBILE_AGENTS,
            BuiltinUserAgentList::SafariTablet => STATIC_SAFARI_TABLET_AGENTS,
            BuiltinUserAgentList::SafariWindows => STATIC_FIREFOX_WINDOWS_AGENTS,
            BuiltinUserAgentList::Random => all_static_agents(),
        };

        ua.iter().map(|&v| UserAgentProfile::from(v)).collect()
    }

    fn get_user_agent(&self, domain: Option<&str>) -> Option<UserAgentProfile> {
        let mut rng = rand::thread_rng();

        let domain_str = domain.unwrap_or_default().to_string();

        let strategy = self
            .per_domain_strategy
            .get(&domain_str)
            .map(|s| s.value().clone())
            .unwrap_or_else(|| self.strategy.clone());

        let pool = || {
            domain
                .and_then(|d| self.domain_cache.get(d))
                .unwrap_or_else(|| self.default_pool.clone())
        };

        let get_fallback = || {
            debug!("User-Agent pool is empty or no UA selected.");
            self.fallback_user_agent
                .as_ref()
                .map(|ua| UserAgentProfile::from(ua.clone()))
        };

        match strategy {
            UserAgentRotationStrategy::Random => {
                let p = pool();
                if p.is_empty() {
                    return get_fallback();
                }
                p.choose(&mut rng).cloned()
            }
            UserAgentRotationStrategy::Sequential => {
                let p = pool();
                if p.is_empty() {
                    return get_fallback();
                }
                let current = self.current_index.fetch_add(1, Ordering::SeqCst);
                let index = current % p.len();
                p.get(index).cloned()
            }
            UserAgentRotationStrategy::Sticky => {
                if let Some(profile) = self.sticky_cache.get(&domain_str) {
                    return Some(profile.clone());
                }

                let p = pool();
                if p.is_empty() {
                    return get_fallback();
                }

                if let Some(profile) = p.choose(&mut rng).cloned() {
                    self.sticky_cache.insert(domain_str, profile.clone());
                    Some(profile)
                } else {
                    get_fallback()
                }
            }
            UserAgentRotationStrategy::StickySession => {
                if let Some(profile) = self.session_cache.get(&domain_str) {
                    return Some(profile);
                }

                let p = pool();
                if p.is_empty() {
                    return get_fallback();
                }

                if let Some(profile) = p.choose(&mut rng).cloned() {
                    self.session_cache.insert(domain_str, profile.clone());
                    Some(profile)
                } else {
                    get_fallback()
                }
            }
        }
    }
}

#[async_trait]
impl<C: Send + Sync> Middleware<C> for UserAgentMiddleware {
    fn name(&self) -> &str {
        "UserAgentMiddleware"
    }

    async fn process_request(
        &mut self,
        _client: &C,
        mut request: Request,
    ) -> Result<MiddlewareAction<Request>, SpiderError> {
        let domain = request.url.domain();
        if let Some(profile) = self.get_user_agent(domain) {
            debug!("Applying User-Agent: {}", profile.user_agent);
            request.headers.insert(
                USER_AGENT,
                HeaderValue::from_str(&profile.user_agent).map_err(|e| {
                    SpiderError::HeaderValueError(format!(
                        "Invalid User-Agent string '{}': {}",
                        profile.user_agent, e
                    ))
                })?,
            );
            for header in profile.headers.iter() {
                request.headers.insert(
                    reqwest::header::HeaderName::from_bytes(header.key().as_bytes()).map_err(
                        |e| SpiderError::HeaderValueError(format!("Invalid header name: {}", e)),
                    )?,
                    HeaderValue::from_str(header.value().as_str()).map_err(|e| {
                        SpiderError::HeaderValueError(format!(
                            "Invalid header value for {}: {}",
                            header.key(),
                            e
                        ))
                    })?,
                );
            }
        } else {
            debug!("No User-Agent applied.");
        }
        Ok(MiddlewareAction::Continue(request))
    }
}