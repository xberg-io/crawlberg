//! Shared helper functions used by the crawl engine.

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::http::http_fetch;
use crate::normalize::robots_url;
use crate::robots::{RobotsRules, parse_robots_txt};
use crate::types::CrawlConfig;

/// Find the byte offset of `needle` (ASCII only) in `haystack` using case-insensitive matching.
///
/// Returns `Some(pos)` where `pos` is the byte offset in the original `haystack` string,
/// safe for slicing because `needle` is pure ASCII.
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

/// What reading robots.txt established for an origin, per RFC 9309 section 2.3.1.
pub(crate) enum RobotsOutcome {
    /// The file was read and parsed (section 2.3.1.1).
    Rules(RobotsRules),
    /// The file is unavailable, so every path is allowed (section 2.3.1.3).
    AllowAll,
    /// The file is unreachable, so every path is disallowed (section 2.3.1.4).
    /// Carries the reason for the caller to report.
    Unreachable(String),
}

impl RobotsOutcome {
    fn unreachable(robots_url: &str, reason: &str) -> Self {
        Self::Unreachable(format!(
            "robots.txt at {robots_url} could not be read ({reason}); every path on this origin is disallowed"
        ))
    }
}

/// Fetch robots.txt for the given URL's origin and classify the answer.
pub(crate) async fn fetch_robots_rules(url: &str, config: &CrawlConfig, client: &reqwest::Client) -> RobotsOutcome {
    let Ok(parsed) = Url::parse(url) else {
        return RobotsOutcome::AllowAll;
    };
    if parsed.host_str().is_none() {
        return RobotsOutcome::AllowAll;
    }
    let robots_url = robots_url(&parsed);
    let ua = config
        .user_agent
        .as_deref()
        .unwrap_or(concat!("crawlberg/", env!("CARGO_PKG_VERSION")));

    // ~keep RFC 9309 section 2.3.1.4: a robots.txt the crawler cannot read — a refused
    // ~keep connection, a timeout, a redirect chain past `max_redirects`, or a 5xx answer — means
    // ~keep complete disallow, not "no rules". Only a 4xx answer (section 2.3.1.3) lets the crawl
    // ~keep proceed unrestricted. `http_fetch` already follows redirects (section 2.3.1.2), so a
    // ~keep 3xx here is the hop limit rather than a target the crawler declined to follow.
    let resp = match http_fetch(&robots_url, config, &std::collections::HashMap::new(), client).await {
        Ok(resp) => resp,
        Err(error) => return classify_robots_error(&robots_url, &error),
    };
    match resp.status {
        500..=599 => RobotsOutcome::unreachable(&robots_url, &format!("HTTP {}", resp.status)),
        400..=499 => RobotsOutcome::AllowAll,
        _ => RobotsOutcome::Rules(parse_robots_txt(&resp.body, ua)),
    }
}

/// Classify a robots.txt fetch that returned an error rather than a response.
///
/// ~keep `http_fetch` turns 401, 403, 404, 408, 410, 429 and every 5xx into a typed error
/// before it returns, so for those codes the status is only reachable through the variant.
fn classify_robots_error(robots_url: &str, error: &CrawlError) -> RobotsOutcome {
    match error {
        // ~keep RFC 9309 section 2.3.1.3: a 4xx answer means the file is unavailable and the
        // ~keep crawl may proceed with no rules. 429 is a 4xx and is classified as the RFC writes it.
        CrawlError::NotFound { .. }
        | CrawlError::Unauthorized { .. }
        | CrawlError::Forbidden { .. }
        | CrawlError::Gone { .. }
        | CrawlError::RateLimited { .. } => RobotsOutcome::AllowAll,
        // ~keep RFC 9309 section 2.3.1.4: everything else means the file was not read, so every
        // ~keep path is disallowed. Two variants are deliberately here rather than above because they
        // ~keep are ambiguous and this is the safe reading: `Timeout` is raised both for HTTP 408 and
        // ~keep for a transport timeout, and `WafBlocked` both for a 403 and for a WAF fingerprint on
        // ~keep a 2xx body. In each pair the crawler did not read the file.
        _ => RobotsOutcome::unreachable(robots_url, &error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_robots_rules_disallows_every_path_when_the_connection_is_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("a bound address").port();
        drop(listener);

        let config = CrawlConfig::builder()
            .respect_robots_txt(true)
            .allow_private_networks(true)
            .build();
        let client = crate::http::build_client(&config).expect("the client builds");

        let outcome = fetch_robots_rules(&format!("http://127.0.0.1:{port}/"), &config, &client).await;

        match outcome {
            RobotsOutcome::Unreachable(reason) => assert!(
                reason.contains("every path on this origin is disallowed"),
                "expected a complete-disallow reason, got {reason:?}"
            ),
            RobotsOutcome::Rules(_) | RobotsOutcome::AllowAll => {
                panic!("a refused connection to robots.txt must disallow every path")
            }
        }
    }
}
