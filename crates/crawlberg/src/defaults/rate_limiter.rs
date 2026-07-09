//! Rate limiter implementations.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::RateLimiter;

/// Maximum backoff duration for 429 responses.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A rate limiter that does nothing, allowing all requests through immediately.
#[derive(Debug, Clone, Default)]
pub struct NoopRateLimiter;

#[async_trait]
impl RateLimiter for NoopRateLimiter {
    async fn acquire(&self, _domain: &str) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn record_response(&self, _domain: &str, _status: u16) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn set_crawl_delay(&self, _domain: &str, _delay: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

/// Per-domain state tracked by [`PerDomainThrottle`].
#[derive(Debug, Clone)]
struct DomainState {
    last_request: Instant,
    crawl_delay: Option<Duration>,
    robots_delay: Option<Duration>,
    consecutive_success: u32,
}

/// A per-domain token bucket rate limiter.
///
/// Enforces a configurable delay between requests to the same domain.
/// Respects robots.txt crawl-delay via `set_crawl_delay`.
#[derive(Debug)]
pub struct PerDomainThrottle {
    default_delay: Duration,
    /// Per-domain state: last request time and optional crawl-delay override.
    state: Mutex<AHashMap<String, DomainState>>,
}

impl PerDomainThrottle {
    /// Create a new limiter with the given default delay between requests.
    pub fn new(default_delay: Duration) -> Self {
        Self {
            default_delay,
            state: Mutex::new(AHashMap::new()),
        }
    }
}

#[async_trait]
impl RateLimiter for PerDomainThrottle {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        let sleep_duration = {
            let mut state = self.state.lock().expect("lock poisoned");
            let now = Instant::now();
            let domain_state = state.entry(domain.to_owned()).or_insert(DomainState {
                last_request: now - self.default_delay,
                crawl_delay: None,
                robots_delay: None,
                consecutive_success: 0,
            });

            let effective = match (&domain_state.crawl_delay, &domain_state.robots_delay) {
                (Some(cd), Some(rd)) => std::cmp::max(*cd, *rd),
                (Some(cd), None) => *cd,
                (None, Some(rd)) => *rd,
                (None, None) => self.default_delay,
            };

            let elapsed = now.duration_since(domain_state.last_request);

            if elapsed < effective {
                let duration = effective - elapsed;
                domain_state.last_request = now + duration;
                Some(duration)
            } else {
                domain_state.last_request = now;
                None
            }
        };

        if let Some(duration) = sleep_duration {
            tokio::time::sleep(duration).await;
        }

        Ok(())
    }

    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
        let mut state = self.state.lock().expect("lock poisoned");
        if let Some(domain_state) = state.get_mut(domain) {
            if status == 429 {
                domain_state.consecutive_success = 0;
                let current = domain_state.crawl_delay.unwrap_or(self.default_delay);
                let new_delay = (current * 2).min(MAX_BACKOFF);
                domain_state.crawl_delay = Some(new_delay);
            } else if status < 400 {
                domain_state.consecutive_success += 1;
                if domain_state.consecutive_success >= 5 {
                    if let Some(ref mut cd) = domain_state.crawl_delay {
                        let floor = domain_state.robots_delay.unwrap_or(self.default_delay);
                        let halved = *cd / 2;
                        if halved <= floor {
                            domain_state.crawl_delay = None;
                        } else {
                            *cd = halved;
                        }
                    }
                    domain_state.consecutive_success = 0;
                }
            }
        }
        Ok(())
    }

    async fn set_crawl_delay(&self, domain: &str, delay: Duration) -> Result<(), CrawlError> {
        let mut state = self.state.lock().expect("lock poisoned");
        let domain_state = state.entry(domain.to_owned()).or_insert(DomainState {
            last_request: Instant::now() - self.default_delay,
            crawl_delay: None,
            robots_delay: None,
            consecutive_success: 0,
        });
        domain_state.robots_delay = Some(delay);
        domain_state.crawl_delay = Some(delay);
        Ok(())
    }
}
