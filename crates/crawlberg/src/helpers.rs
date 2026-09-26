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

/// Strip `config.tracking_params` from a seed URL when `config.strip_tracking_params` is set.
///
/// ~keep Shared by the native and wasm crawl loops, applied once at the top of each, before
/// the seed is parsed, redirect-resolved, or dedup-keyed: every later use of the seed URL
/// (fetch, `final_url`, `normalized_url`) derives from this string, so a single strip keeps
/// them all consistent without a second pass downstream.
pub(crate) fn strip_seed_tracking_params(config: &CrawlConfig, url: &str) -> String {
    if config.strip_tracking_params {
        crate::normalize::strip_tracking_params(url, &config.tracking_params)
    } else {
        url.to_owned()
    }
}

/// The text an `include_paths`/`exclude_paths` regex is matched against: the path alone, or
/// the path with `?query` appended when `match_query` is set.
///
/// ~keep Path-only stays the default because a pattern anchored with `$` changes meaning once
/// the query joins the text (`/feed/?$` stops matching `/feed?x=1`), so flipping the default
/// would silently change what today's `exclude_paths`/`include_paths` configs match.
pub(crate) fn path_pattern_target(url: &Url, match_query: bool) -> String {
    match (match_query, url.query()) {
        (true, Some(query)) => format!("{}?{query}", url.path()),
        _ => url.path().to_owned(),
    }
}

/// Whether `url` survives `exclude_paths`/`include_paths`, incrementing `urls_filtered` and
/// returning `false` the first time a rule rejects it.
///
/// `check_include` lets a caller skip the include check for a URL it already vetted through
/// some other rule before reaching this call — see `RedirectPolicy::admits`, which applies
/// this only to genuine redirect targets and not to the chain's own starting URL.
pub(crate) fn passes_path_patterns(
    url: &Url,
    exclude_regexes: &[Regex],
    include_regexes: &[Regex],
    check_include: bool,
    match_query: bool,
    urls_filtered: &mut usize,
) -> bool {
    let target = path_pattern_target(url, match_query);
    if !exclude_regexes.is_empty() && exclude_regexes.iter().any(|re| re.is_match(&target)) {
        *urls_filtered += 1;
        return false;
    }
    if check_include && !include_regexes.is_empty() && !include_regexes.iter().any(|re| re.is_match(&target)) {
        *urls_filtered += 1;
        return false;
    }
    true
}

/// Why a robots.txt fetch failed closed, and therefore how far the denial can be trusted.
///
/// ~keep The outcome cache is shared by every crawl on an engine handle, so a denial cached
/// for one crawl denies all of them. A `Sustained` denial is the origin's own answer and
/// retaining it is the intended backoff; a `Transient` one teaches nothing about the origin,
/// so retaining it for as long would let a single DNS blip fail-closed unrelated crawls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RobotsDenial {
    /// The origin answered and refused (429, 5xx, a WAF interstitial), or the request could
    /// never have succeeded (an unparseable URL).
    Sustained,
    /// The origin was never reached at all: DNS, TLS, connect or read timeout.
    Transient,
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
    /// robots.txt is unreachable; every path is denied. Carries the reason for reporting and
    /// the denial's durability, which decides how long the outcome cache retains it.
    DisallowAll { reason: String, denial: RobotsDenial },
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

    /// Whether this outcome denies the origin over a condition that is expected to clear on
    /// its own, rather than over an answer the origin actually gave.
    ///
    /// ~keep Only the engine's shared outcome cache reads this, and that module is native-only,
    /// so on wasm32 this method -- and with it the `denial` field it is the sole reader of --
    /// is unreachable and would trip `dead_code` under `-D warnings`.
    #[cfg_attr(
        target_arch = "wasm32",
        expect(dead_code, reason = "the only caller, engine::robots_cache, is native-only")
    )]
    pub(crate) fn is_transient_denial(&self) -> bool {
        matches!(
            self,
            Self::DisallowAll {
                denial: RobotsDenial::Transient,
                ..
            }
        )
    }

    /// The reason the whole origin is denied, if it is.
    pub(crate) fn disallow_all_reason(&self) -> Option<&str> {
        match self {
            Self::DisallowAll { reason, .. } => Some(reason),
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
        | CrawlError::Gone { .. } => RobotsOutcome::AllowAll,
        // ~keep Also RFC 9309 2.3.1.4 "unreachable", but split out of the catch-all below: these
        // four never reached the origin, so the denial reflects our side of the wire rather than
        // anything the site said, and the cache is entitled to forget it quickly.
        CrawlError::Timeout { .. }
        | CrawlError::Connection { .. }
        | CrawlError::Dns { .. }
        | CrawlError::Ssl { .. } => RobotsOutcome::DisallowAll {
            reason: error.to_string(),
            denial: RobotsDenial::Transient,
        },
        // ~keep Everything else is RFC 9309 2.3.1.4 "unreachable". The catch-all arm must be
        // the closed one: fail-closed is only sound if an unrecognised failure denies. It is
        // also the durable one, so an unrecognised failure does not additionally get the
        // shortest memory of it.
        // `RateLimited` (429) lands here deliberately -- it is a 4xx that the RFC files under
        // "unavailable", but a site actively rate-limiting us is the worst possible moment to
        // conclude "no rules, crawl everything". Google's robots handling treats it the same way.
        // `WafBlocked` is here for a different reason: `http_fetch` raises it for a 403 but also for
        // a WAF fingerprint on a *2xx* body or header (http.rs), so it does not imply a 4xx at all.
        // What it does imply is that the bytes we hold are an interstitial rather than the origin's
        // robots.txt -- reading that as "unavailable" hands a WAF-protected site an unrestricted crawl.
        _ => RobotsOutcome::DisallowAll {
            reason: error.to_string(),
            denial: RobotsDenial::Sustained,
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
            denial: RobotsDenial::Sustained,
        };
    };
    // ~keep `robots_url` uses `authority()`, which keeps a non-default port. Building this
    // from `host_str()` instead sent every port-bearing seed's robots request to the default
    // port, where it failed and silently degraded to "no rules".
    let robots_url = crate::normalize::robots_url(&parsed);
    match http_fetch(&robots_url, config, &std::collections::HashMap::new(), client).await {
        Ok(resp) if resp.status >= 500 => RobotsOutcome::DisallowAll {
            reason: format!("robots.txt returned HTTP {}", resp.status),
            denial: RobotsDenial::Sustained,
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

    #[test]
    fn exclude_pattern_matching_only_the_query_does_not_exclude_when_match_query_is_off() {
        let url = Url::parse("https://example.com/blog?p=42").expect("valid URL");
        let exclude = compile_regexes(&[r"\?p=\d+".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(&url, &exclude, &[], true, false, &mut urls_filtered),
            "path-only matching must not see the query string, so /blog?p=42 must still be fetched"
        );
        assert_eq!(urls_filtered, 0);
    }

    #[test]
    fn exclude_pattern_matching_the_query_excludes_when_match_query_is_on() {
        let url = Url::parse("https://example.com/blog?p=42").expect("valid URL");
        let exclude = compile_regexes(&[r"\?p=\d+".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            !passes_path_patterns(&url, &exclude, &[], true, true, &mut urls_filtered),
            "with match_query on, /blog?p=42 must be excluded"
        );
        assert_eq!(urls_filtered, 1);
    }

    #[test]
    fn include_pattern_matching_only_the_query_does_not_admit_when_match_query_is_off() {
        let url = Url::parse("https://example.com/blog?p=42").expect("valid URL");
        let include = compile_regexes(&[r"\?p=\d+".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            !passes_path_patterns(&url, &[], &include, true, false, &mut urls_filtered),
            "path-only matching must not see the query string, so the include pattern never matches"
        );
    }

    #[test]
    fn include_pattern_matching_the_query_admits_when_match_query_is_on() {
        let url = Url::parse("https://example.com/blog?p=42").expect("valid URL");
        let include = compile_regexes(&[r"\?p=\d+".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(&url, &[], &include, true, true, &mut urls_filtered),
            "with match_query on, /blog?p=42 must satisfy the include pattern"
        );
    }

    #[test]
    fn check_include_false_skips_the_include_check_entirely() {
        let url = Url::parse("https://example.com/blog").expect("valid URL");
        let include = compile_regexes(&["^/docs".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(&url, &[], &include, false, false, &mut urls_filtered),
            "check_include=false must admit a URL even when it fails every include pattern"
        );
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
    fn should_disallow_all_when_robots_txt_is_behind_a_waf() {
        // ~keep `WafBlocked` is raised for a WAF fingerprint on a 2xx body as well as for a 403, so
        // the file was not read in either case and "unavailable" would be the wrong reading.
        let outcome = outcome_for_fetch_error(&CrawlError::waf_blocked(
            "cloudflare",
            "waf/blocked detected on 2xx (body): cloudflare",
        ));
        assert!(
            is_disallow_all(&outcome),
            "a WAF interstitial in place of robots.txt must fail closed"
        );
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
    fn should_mark_the_denial_transient_when_the_origin_was_never_reached() {
        // ~keep These four denials say nothing about the origin's policy, so the cache is
        // entitled to forget them quickly; the assertion pins which errors qualify.
        for error in [
            CrawlError::timeout("timeout"),
            CrawlError::connection("connection"),
            CrawlError::dns("dns"),
            CrawlError::ssl("ssl"),
        ] {
            assert!(
                outcome_for_fetch_error(&error).is_transient_denial(),
                "{error} never reached the origin and must not pin a long-lived denial"
            );
        }
    }

    #[test]
    fn should_mark_the_denial_sustained_when_the_origin_refused() {
        // ~keep The negative control for the split above: a refusal the origin actually issued,
        // and any failure we do not recognise, must keep the full backoff.
        for error in [
            CrawlError::rate_limited("rate_limited"),
            CrawlError::server_error("server_error"),
            CrawlError::bad_gateway("bad_gateway"),
            CrawlError::data_loss("data_loss"),
            CrawlError::other("other"),
            CrawlError::waf_blocked("cloudflare", "waf/blocked detected on 2xx (body): cloudflare"),
        ] {
            let outcome = outcome_for_fetch_error(&error);
            assert!(is_disallow_all(&outcome), "{error} must still fail closed");
            assert!(
                !outcome.is_transient_denial(),
                "{error} is the origin's own answer and must serve the full backoff"
            );
        }
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
            denial: RobotsDenial::Sustained,
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
