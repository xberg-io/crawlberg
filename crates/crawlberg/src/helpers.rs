//! Shared helper functions used by the crawl engine.

use std::borrow::Cow;

use url::{Position, Url};

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

/// A compiled `include_paths`/`exclude_paths` pattern.
///
/// ~keep `fancy_regex` parses every pattern with its own parser, and runs one without
/// look-around or backreferences on `regex-automata`, the engine behind the `regex` crate.
/// Its parser refuses a few constructs the `regex` crate accepts, such as inline Unicode-mode
/// flags (`(?-u)`), so a pattern it refuses is compiled with the `regex` crate instead: every
/// pattern the `regex` crate accepts keeps compiling, with the meaning it has there.
#[derive(Clone)]
pub(crate) enum PathPattern {
    /// Compiled by `fancy_regex`; may backtrack, so a match can fail.
    Fancy(fancy_regex::Regex),
    /// Refused by `fancy_regex`, compiled by the `regex` crate.
    Plain(regex::Regex),
}

impl PathPattern {
    /// Compile `pattern`, or return `fancy_regex`'s error when neither engine accepts it.
    pub(crate) fn new(pattern: &str) -> Result<Self, fancy_regex::Error> {
        match fancy_regex::Regex::new(pattern) {
            Ok(regex) => Ok(Self::Fancy(regex)),
            Err(error) => regex::Regex::new(pattern).map(Self::Plain).map_err(|_| error),
        }
    }

    /// The pattern as written in the config.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Fancy(regex) => regex.as_str(),
            Self::Plain(regex) => regex.as_str(),
        }
    }

    /// Whether the pattern matches `text`; an error means the backtracking limit was hit.
    pub(crate) fn is_match(&self, text: &str) -> Result<bool, fancy_regex::Error> {
        match self {
            Self::Fancy(regex) => regex.is_match(text),
            Self::Plain(regex) => Ok(regex.is_match(text)),
        }
    }
}

/// Compile `include_paths`/`exclude_paths` patterns, returning an error naming the first invalid one.
pub(crate) fn compile_regexes(patterns: &[String]) -> Result<Vec<PathPattern>, CrawlError> {
    patterns
        .iter()
        .map(|pat| {
            PathPattern::new(pat).map_err(|e| CrawlError::other(format!("invalid regex pattern \"{pat}\": {e}")))
        })
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

/// The text an `include_paths`/`exclude_paths` regex is matched against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathPatternTarget {
    /// The path alone, the default.
    Path,
    /// The path with `?query` appended (`path_patterns_match_query`).
    PathAndQuery,
    /// The whole URL up to and including the query, without userinfo or the fragment
    /// (`path_patterns_match_url`).
    FullUrl,
}

impl PathPatternTarget {
    /// The target `config` selects; `path_patterns_match_url` wins over `path_patterns_match_query`.
    pub(crate) fn from_config(config: &CrawlConfig) -> Self {
        if config.path_patterns_match_url {
            Self::FullUrl
        } else if config.path_patterns_match_query {
            Self::PathAndQuery
        } else {
            Self::Path
        }
    }

    /// The text of `url` a pattern is matched against.
    ///
    /// ~keep Path-only stays the default because a pattern anchored with `$` changes meaning
    /// once the query joins the text (`/feed/?$` stops matching `/feed?x=1`), so flipping the
    /// default would silently change what today's `exclude_paths`/`include_paths` configs match.
    pub(crate) fn text<'u>(self, url: &'u Url) -> Cow<'u, str> {
        match (self, url.query()) {
            (Self::FullUrl, _) if url.username().is_empty() && url.password().is_none() => {
                Cow::Borrowed(&url[..Position::AfterQuery])
            }
            (Self::FullUrl, _) => Cow::Owned(format!(
                "{}{}",
                &url[..Position::BeforeUsername],
                &url[Position::BeforeHost..Position::AfterQuery]
            )),
            (Self::PathAndQuery, Some(query)) => Cow::Owned(format!("{}?{query}", url.path())),
            _ => Cow::Borrowed(url.path()),
        }
    }
}

/// Whether `pattern` matches `text`, or `on_error` when the match cannot finish.
///
/// ~keep A look-around or backreference pattern runs on a backtracking engine that gives up
/// at its backtrack limit. The caller passes the answer that keeps the URL out of the crawl
/// (excluded, or not included), so a pattern that cannot be evaluated never widens the crawl.
fn pattern_matches(pattern: &PathPattern, text: &str, on_error: bool) -> bool {
    pattern.is_match(text).unwrap_or_else(|error| {
        tracing::warn!(
            pattern = pattern.as_str(),
            %error,
            "path pattern could not be evaluated; the URL is kept out of the crawl"
        );
        on_error
    })
}

/// Whether `url` survives `exclude_paths`/`include_paths`, incrementing `urls_filtered` and
/// returning `false` the first time a rule rejects it.
///
/// `check_include` lets a caller skip the include check for a URL it already vetted through
/// some other rule before reaching this call — see `RedirectPolicy::admits`, which applies
/// this only to genuine redirect targets and not to the chain's own starting URL.
pub(crate) fn passes_path_patterns(
    url: &Url,
    exclude_regexes: &[PathPattern],
    include_regexes: &[PathPattern],
    check_include: bool,
    target: PathPatternTarget,
    urls_filtered: &mut usize,
) -> bool {
    let text = target.text(url);
    if exclude_regexes.iter().any(|re| pattern_matches(re, &text, true)) {
        *urls_filtered += 1;
        return false;
    }
    if check_include
        && !include_regexes.is_empty()
        && !include_regexes.iter().any(|re| pattern_matches(re, &text, false))
    {
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
            passes_path_patterns(&url, &exclude, &[], true, PathPatternTarget::Path, &mut urls_filtered),
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
            !passes_path_patterns(
                &url,
                &exclude,
                &[],
                true,
                PathPatternTarget::PathAndQuery,
                &mut urls_filtered
            ),
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
            !passes_path_patterns(&url, &[], &include, true, PathPatternTarget::Path, &mut urls_filtered),
            "path-only matching must not see the query string, so the include pattern never matches"
        );
    }

    #[test]
    fn include_pattern_matching_the_query_admits_when_match_query_is_on() {
        let url = Url::parse("https://example.com/blog?p=42").expect("valid URL");
        let include = compile_regexes(&[r"\?p=\d+".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(
                &url,
                &[],
                &include,
                true,
                PathPatternTarget::PathAndQuery,
                &mut urls_filtered
            ),
            "with match_query on, /blog?p=42 must satisfy the include pattern"
        );
    }

    #[test]
    fn check_include_false_skips_the_include_check_entirely() {
        let url = Url::parse("https://example.com/blog").expect("valid URL");
        let include = compile_regexes(&["^/docs".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(&url, &[], &include, false, PathPatternTarget::Path, &mut urls_filtered),
            "check_include=false must admit a URL even when it fails every include pattern"
        );
    }

    #[test]
    fn full_url_target_keeps_scheme_host_port_and_query_and_drops_the_fragment() {
        let url = Url::parse("http://127.0.0.1:8080/private/x?p=1#top").expect("valid URL");
        assert_eq!(
            PathPatternTarget::FullUrl.text(&url),
            "http://127.0.0.1:8080/private/x?p=1"
        );
        assert_eq!(PathPatternTarget::PathAndQuery.text(&url), "/private/x?p=1");
        assert_eq!(PathPatternTarget::Path.text(&url), "/private/x");

        let url = Url::parse("https://user:pw@example.com:443/x").expect("valid URL");
        assert_eq!(
            PathPatternTarget::FullUrl.text(&url),
            "https://example.com/x",
            "userinfo and the default port are not part of the text"
        );

        let url = Url::parse("https://b\u{fc}cher.de/x").expect("valid URL");
        assert_eq!(
            PathPatternTarget::FullUrl.text(&url),
            "https://xn--bcher-kva.de/x",
            "the host is matched in punycode"
        );
    }

    #[test]
    fn host_anchored_exclude_pattern_matches_a_url_that_carries_userinfo() {
        let url = Url::parse("https://x@example.com/private/a").expect("valid URL");
        let exclude = compile_regexes(&[r"^https://example\.com/private/".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            !passes_path_patterns(
                &url,
                &exclude,
                &[],
                true,
                PathPatternTarget::FullUrl,
                &mut urls_filtered
            ),
            "userinfo in a link must not let it escape a host-anchored exclude pattern"
        );
    }

    /// Inline Unicode-mode flags that the `regex` crate accepts and `fancy_regex` refuses.
    const REGEX_ONLY_PATTERNS: [&str; 4] = [r"(?-u)\w", r"(?-u:\w)", r"(?i-u)a", r"(?-u:\b)x"];

    #[test]
    fn patterns_the_regex_crate_accepts_still_compile_and_match() {
        let patterns: Vec<String> = REGEX_ONLY_PATTERNS.iter().map(|p| (*p).to_owned()).collect();
        let compiled = compile_regexes(&patterns).expect("every pattern the regex crate accepts must compile");
        let url = Url::parse("https://example.com/xA").expect("valid URL");
        for (pattern, source) in compiled.iter().zip(REGEX_ONLY_PATTERNS) {
            let mut urls_filtered = 0usize;
            assert!(
                !passes_path_patterns(
                    &url,
                    std::slice::from_ref(pattern),
                    &[],
                    true,
                    PathPatternTarget::Path,
                    &mut urls_filtered
                ),
                "{source} must match /xA and exclude it"
            );
        }
    }

    #[test]
    fn config_validation_accepts_patterns_the_regex_crate_accepts() {
        let config = CrawlConfig {
            include_paths: REGEX_ONLY_PATTERNS.iter().map(|p| (*p).to_owned()).collect(),
            exclude_paths: REGEX_ONLY_PATTERNS.iter().map(|p| (*p).to_owned()).collect(),
            ..CrawlConfig::default()
        };
        assert!(config.validate().is_ok(), "{:?}", config.validate());
    }

    #[test]
    fn path_patterns_match_url_wins_over_path_patterns_match_query() {
        let mut config = CrawlConfig::default();
        assert_eq!(PathPatternTarget::from_config(&config), PathPatternTarget::Path);
        config.path_patterns_match_query = true;
        assert_eq!(PathPatternTarget::from_config(&config), PathPatternTarget::PathAndQuery);
        config.path_patterns_match_url = true;
        assert_eq!(PathPatternTarget::from_config(&config), PathPatternTarget::FullUrl);
    }

    #[test]
    fn host_anchored_exclude_pattern_excludes_only_with_the_full_url_target() {
        let url = Url::parse("https://example.com/private/x").expect("valid URL");
        let exclude = compile_regexes(&[r"^https://example\.com/private/".to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(passes_path_patterns(
            &url,
            &exclude,
            &[],
            true,
            PathPatternTarget::PathAndQuery,
            &mut urls_filtered
        ));
        assert!(!passes_path_patterns(
            &url,
            &exclude,
            &[],
            true,
            PathPatternTarget::FullUrl,
            &mut urls_filtered
        ));
        assert_eq!(urls_filtered, 1);
    }

    /// A backreference after an ambiguous repetition: matching it against a long run of `a`s
    /// with no `b` exhausts the backtracking engine's limit instead of answering.
    const BACKTRACK_BOMB: &str = r"^/(a|aa)+\1b";

    fn backtrack_bomb_url() -> Url {
        Url::parse(&format!("https://example.com/{}", "a".repeat(64))).expect("valid URL")
    }

    #[test]
    fn backtrack_bomb_pattern_cannot_be_evaluated() {
        let pattern = PathPattern::new(BACKTRACK_BOMB).expect("valid pattern");
        let url = backtrack_bomb_url();
        assert!(
            pattern.is_match(url.path()).is_err(),
            "the fixture must hit the backtrack limit, or the fail-closed tests below prove nothing"
        );
    }

    #[test]
    fn exclude_pattern_that_cannot_be_evaluated_excludes_the_url() {
        let exclude = compile_regexes(&[BACKTRACK_BOMB.to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            !passes_path_patterns(
                &backtrack_bomb_url(),
                &exclude,
                &[],
                true,
                PathPatternTarget::Path,
                &mut urls_filtered
            ),
            "an exclude pattern that hits the backtrack limit must count as a match"
        );
        assert_eq!(urls_filtered, 1);
    }

    #[test]
    fn include_pattern_that_cannot_be_evaluated_does_not_admit_the_url() {
        let include = compile_regexes(&[BACKTRACK_BOMB.to_owned()]).expect("valid pattern");
        let mut urls_filtered = 0usize;
        assert!(
            !passes_path_patterns(
                &backtrack_bomb_url(),
                &[],
                &include,
                true,
                PathPatternTarget::Path,
                &mut urls_filtered
            ),
            "an include pattern that hits the backtrack limit must count as no match"
        );
        assert_eq!(urls_filtered, 1);
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
        let outcome = outcome_for_fetch_error(&CrawlError::WafBlocked {
            vendor: "cloudflare".to_owned(),
            message: "waf/blocked detected on 2xx (body): cloudflare".to_owned(),
        });
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
            CrawlError::WafBlocked {
                vendor: "cloudflare".to_owned(),
                message: "waf/blocked detected on 2xx (body): cloudflare".to_owned(),
            },
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
