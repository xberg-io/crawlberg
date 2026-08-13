//! Shared helper functions used by the crawl engine.

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::http::http_fetch;
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

/// Fetch and parse robots.txt for the given URL's origin.
///
/// Returns `None` if the fetch fails (gracefully degraded).
pub(crate) async fn fetch_robots_rules(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> Option<RobotsRules> {
    let parsed = Url::parse(url).ok()?;
    let robots_url = format!("{}://{}/robots.txt", parsed.scheme(), parsed.host_str()?);
    let ua = config
        .user_agent
        .as_deref()
        .unwrap_or(concat!("crawlberg/", env!("CARGO_PKG_VERSION")));
    let resp = http_fetch(&robots_url, config, &std::collections::HashMap::new(), client)
        .await
        .ok()?;
    if resp.status >= 400 {
        return None;
    }
    Some(parse_robots_txt(&resp.body, ua))
}
