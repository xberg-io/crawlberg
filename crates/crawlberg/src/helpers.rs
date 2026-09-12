//! Shared helper functions used by the crawl engine.

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::http::http_fetch;
use crate::robots::{RobotsRules, is_path_allowed, parse_robots_txt};
use crate::types::CrawlConfig;

/// Find the byte offset of `needle` (ASCII only) in `haystack` using case-insensitive matching.
///
/// Returns `Some(pos)` where `pos` is the byte offset in the original `haystack` string,
/// safe for slicing because `needle` is pure ASCII.
// ~keep Only the native crawl loop parses `Refresh:` headers, and that module is
// wasm-gated, so this would be dead code under `-D warnings` on wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > haystack_bytes.len() {
        return None;
    }
    (0..=(haystack_bytes.len() - needle_bytes.len())).find(|&i| {
        haystack_bytes[i..i + needle_bytes.len()]
            .iter()
            .zip(needle_bytes.iter())
            .all(|(h, n)| h.to_ascii_lowercase() == *n)
    })
}

/// Compile a slice of regex pattern strings, returning an error if any pattern is invalid.
pub(crate) fn compile_regexes(patterns: &[String]) -> Result<Vec<Regex>, CrawlError> {
    patterns
        .iter()
        .map(|pat| Regex::new(pat).map_err(|e| CrawlError::other(format!("invalid regex pattern \"{pat}\": {e}"))))
        .collect()
}

/// What a robots.txt fetch told us about crawling an origin.
///
/// ~keep RFC 9309 section 2.3.1 distinguishes three cases that a bare `Option<RobotsRules>`
/// collapsed into two: a parsed file, an "unavailable" file (4xx) that permits crawling with
/// no rules, and an "unreachable" file (5xx or a network failure) that requires assuming
/// complete disallow. Collapsing the third into "no rules" let a site with a failing
/// robots.txt be crawled in full.
pub(crate) enum RobotsOutcome {
    /// robots.txt was fetched and parsed.
    Rules(RobotsRules),
    /// robots.txt is unavailable (4xx); every path is permitted.
    AllowAll,
    /// robots.txt is unreachable; every path is denied. Carries the reason for reporting.
    DisallowAll { reason: String },
}

impl RobotsOutcome {
    /// Whether `path` may be fetched under this outcome.
    pub(crate) fn allows(&self, path: &str) -> bool {
        match self {
            Self::Rules(rules) => is_path_allowed(path, rules),
            Self::AllowAll => true,
            Self::DisallowAll { .. } => false,
        }
    }

    /// The parsed rules, if robots.txt was actually read.
    ///
    /// ~keep Native-only because its sole caller applies `Crawl-delay` to the rate limiter.
    /// The wasm crawl loop never calls `RateLimiter::acquire`, and `DefaultRateLimiter`
    /// sleeps on `tokio::time`, which has no timer driver under `wasm-bindgen-futures`, so
    /// publishing a crawl delay there would be inert.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn rules(&self) -> Option<&RobotsRules> {
        match self {
            Self::Rules(rules) => Some(rules),
            _ => None,
        }
    }

    /// The reason the whole origin is denied, if it is.
    pub(crate) fn disallow_all_reason(&self) -> Option<&str> {
        match self {
            Self::DisallowAll { reason } => Some(reason),
            _ => None,
        }
    }
}

/// Classify a failed robots.txt fetch per RFC 9309 section 2.3.1.
///
/// ~keep `http_fetch` maps most non-2xx statuses to typed errors before returning, so the
/// status code is not observable here -- the error variant is what carries it.
fn outcome_for_fetch_error(error: &CrawlError) -> RobotsOutcome {
    match error {
        // ~keep RFC 9309 2.3.1.3 "unavailable": 4xx means crawl with no rules.
        CrawlError::NotFound { .. }
        | CrawlError::Unauthorized { .. }
        | CrawlError::Forbidden { .. }
        | CrawlError::WafBlocked { .. }
        | CrawlError::Gone { .. } => RobotsOutcome::AllowAll,
        // ~keep Everything else is RFC 9309 2.3.1.4 "unreachable". The catch-all arm must be
        // the closed one: fail-closed is only sound if an unrecognised failure denies.
        // `RateLimited` (429) lands here deliberately -- it is a 4xx that the RFC files under
        // "unavailable", but a site actively rate-limiting us is the worst possible moment to
        // conclude "no rules, crawl everything". Google's robots handling treats it the same way.
        _ => RobotsOutcome::DisallowAll {
            reason: error.to_string(),
        },
    }
}

/// Fetch and classify robots.txt for the given URL's origin.
///
/// Never fails: an unreachable robots.txt is a `DisallowAll` outcome, not an error.
/// The user-agent the crawl path matches robots.txt groups against.
///
/// ~keep `scrape()` and `map()` deliberately pass `"*"` instead. That is a real
/// inconsistency -- `parse_robots_txt` guards `ua_lower != "*"`, so passing the wildcard
/// disables specific-group matching entirely and a site's `User-agent: crawlberg` group
/// becomes invisible to them. Unifying it changes which rules apply to every caller of
/// those two functions, so it is deferred out of this patch release rather than folded
/// into a robots *correctness* fix.
pub(crate) fn default_robots_user_agent(config: &CrawlConfig) -> &str {
    config
        .user_agent
        .as_deref()
        .unwrap_or(concat!("crawlberg/", env!("CARGO_PKG_VERSION")))
}

pub(crate) async fn fetch_robots_outcome(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    user_agent: &str,
) -> RobotsOutcome {
    let Ok(parsed) = Url::parse(url) else {
        return RobotsOutcome::DisallowAll {
            reason: format!("invalid URL: {url}"),
        };
    };
    // ~keep `robots_url` uses `authority()`, which keeps a non-default port. Building this
    // from `host_str()` instead sent every port-bearing seed's robots request to the default
    // port, where it failed and silently degraded to "no rules".
    let robots_url = crate::normalize::robots_url(&parsed);
    match http_fetch(&robots_url, config, &std::collections::HashMap::new(), client).await {
        Ok(resp) if resp.status >= 500 => RobotsOutcome::DisallowAll {
            reason: format!("robots.txt returned HTTP {}", resp.status),
        },
        Ok(resp) if resp.status >= 400 => RobotsOutcome::AllowAll,
        Ok(resp) => RobotsOutcome::Rules(parse_robots_txt(&resp.body, user_agent)),
        Err(error) => outcome_for_fetch_error(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_allow_all(outcome: &RobotsOutcome) -> bool {
        matches!(outcome, RobotsOutcome::AllowAll)
    }

    fn is_disallow_all(outcome: &RobotsOutcome) -> bool {
        matches!(outcome, RobotsOutcome::DisallowAll { .. })
    }

    #[test]
    fn should_allow_all_when_robots_txt_is_unavailable() {
        // ~keep RFC 9309 2.3.1.3: every 4xx means "no policy here", so crawling proceeds.
        for error in [
            CrawlError::not_found("not_found"),
            CrawlError::gone("gone"),
            CrawlError::forbidden("forbidden"),
            CrawlError::unauthorized("unauthorized"),
        ] {
            let outcome = outcome_for_fetch_error(&error);
            assert!(
                is_allow_all(&outcome),
                "{error} must be treated as unavailable (allow all)"
            );
        }
    }

    #[test]
    fn should_disallow_all_when_robots_txt_is_unreachable() {
        // ~keep RFC 9309 2.3.1.4: the policy is unknown, and an unknown policy is not permission.
        for error in [
            CrawlError::server_error("server_error"),
            CrawlError::bad_gateway("bad_gateway"),
            CrawlError::timeout("timeout"),
            CrawlError::connection("connection"),
            CrawlError::dns("dns"),
            CrawlError::ssl("ssl"),
            CrawlError::data_loss("data_loss"),
            CrawlError::other("other"),
        ] {
            let outcome = outcome_for_fetch_error(&error);
            assert!(
                is_disallow_all(&outcome),
                "{error} must be treated as unreachable (disallow all)"
            );
        }
    }

    #[test]
    fn should_disallow_all_when_robots_txt_is_rate_limited() {
        // ~keep Pins the deliberate divergence from a literal RFC 9309 2.3.1.3 reading: 429 is
        // numerically 4xx, but answering "crawl everything" to a site that is actively shedding
        // our traffic would have us crawl harder, inverting the point of robots.txt.
        let outcome = outcome_for_fetch_error(&CrawlError::rate_limited("rate_limited"));
        assert!(is_disallow_all(&outcome), "429 on robots.txt must fail closed");
    }

    #[test]
    fn should_allow_every_path_when_robots_txt_is_unavailable() {
        assert!(RobotsOutcome::AllowAll.allows("/anything"));
        assert!(RobotsOutcome::AllowAll.rules().is_none());
        assert!(RobotsOutcome::AllowAll.disallow_all_reason().is_none());
    }

    #[test]
    fn should_deny_every_path_when_robots_txt_is_unreachable() {
        let outcome = RobotsOutcome::DisallowAll {
            reason: "boom".to_owned(),
        };
        assert!(!outcome.allows("/"), "disallow-all must deny even the root path");
        assert_eq!(outcome.disallow_all_reason(), Some("boom"));
    }

    #[test]
    fn should_apply_parsed_rules_when_robots_txt_was_read() {
        let outcome = RobotsOutcome::Rules(parse_robots_txt("User-agent: *\nDisallow: /private/\n", "*"));
        assert!(outcome.allows("/public"), "an unlisted path stays allowed");
        assert!(!outcome.allows("/private/secret"), "a disallowed prefix must be denied");
    }
}
