//! Shared helper functions used by the crawl engine.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// How many times one `include_paths`/`exclude_paths` match may backtrack before it gives up.
///
/// ~keep The library default is 1,000,000. This limit counts backtracks of the backtracking
/// engine only: the body of a look-around runs on a second engine that scans the rest of the
/// text on each call, and those scans are not counted. It therefore does not bound the time of
/// a look-around match; [`LOOKAROUND_MAX_TEXT_BYTES`] does that, by bounding the text.
const PATH_PATTERN_BACKTRACK_LIMIT: usize = 100_000;

/// The longest text, in bytes, that a look-around or backreference pattern is evaluated on.
///
/// ~keep Such a pattern runs on a backtracking engine whose cost grows faster than linearly
/// with the text, and the crawled site chooses the length of a discovered URL. A longer text
/// is not evaluated: it takes the same fail-closed path as a match that hits the backtrack
/// limit. 2048 is the sitemaps protocol limit, which requires a URL shorter than 2048
/// characters, so every URL a sitemap may list is evaluated. A pattern that the `regex` crate
/// accepts runs in linear time and has no such limit.
pub(crate) const LOOKAROUND_MAX_TEXT_BYTES: usize = 2048;

/// The largest compiled size, in bytes, of one `include_paths`/`exclude_paths` pattern.
///
/// ~keep The default of both the `regex` crate and `regex-automata`: a lower limit refuses
/// patterns the `regex` crate accepts (1 MiB already refuses `\w{30}`), and a pattern is
/// compiled once per crawl, not once per URL.
const PATH_PATTERN_SIZE_LIMIT: usize = 10 * (1 << 20);

/// A compiled `include_paths`/`exclude_paths` pattern.
#[derive(Clone)]
pub(crate) struct PathPattern {
    engine: PatternEngine,
    /// Set once a match of this pattern has failed and been logged. Clones share it, so every
    /// task of one crawl logs a failing pattern once.
    warned: Arc<AtomicBool>,
}

/// ~keep Every pattern the `regex` crate accepts compiles there, so it keeps the meaning and
/// the linear match time it had before look-around was supported. Only a pattern the `regex`
/// crate refuses for its look-around or its backreference compiles with `fancy_regex`; any other
/// refusal refuses the pattern, because `fancy_regex` gives a malformed pattern such as `a{2,1}`
/// a meaning of its own. So `Fancy` means exactly "needs the backtracking engine", which the
/// REST API refusal and [`LOOKAROUND_MAX_TEXT_BYTES`] rely on.
#[derive(Clone)]
enum PatternEngine {
    /// Compiled by the `regex` crate; a match always finishes, in linear time.
    Plain(regex::Regex),
    /// Refused by the `regex` crate, compiled by `fancy_regex`; a match can fail.
    Fancy(fancy_regex::Regex),
}

/// Why a pattern could not say whether it matches a text.
#[derive(Debug)]
pub(crate) enum Unevaluated {
    /// The text is longer than [`LOOKAROUND_MAX_TEXT_BYTES`].
    TextTooLong(usize),
    /// The backtracking engine gave up, at its backtrack limit.
    Engine(fancy_regex::Error),
}

impl std::fmt::Display for Unevaluated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TextTooLong(len) => write!(
                f,
                "the text is {len} bytes, and a look-around or backreference pattern is only \
                 evaluated on up to {LOOKAROUND_MAX_TEXT_BYTES} bytes"
            ),
            Self::Engine(error) => error.fmt(f),
        }
    }
}

impl PathPattern {
    /// Compile `pattern`, or return the error of the engine that refused it.
    pub(crate) fn new(pattern: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let engine = match regex::RegexBuilder::new(pattern)
            .size_limit(PATH_PATTERN_SIZE_LIMIT)
            .build()
        {
            Ok(regex) => PatternEngine::Plain(regex),
            Err(error) if !uses_look_around_or_backreference(pattern) => return Err(error.into()),
            Err(_) => fancy_regex::RegexBuilder::new(pattern)
                .backtrack_limit(PATH_PATTERN_BACKTRACK_LIMIT)
                .delegate_size_limit(PATH_PATTERN_SIZE_LIMIT)
                .build()
                .map(PatternEngine::Fancy)?,
        };
        Ok(Self {
            engine,
            warned: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Whether the pattern needs the backtracking engine: it uses look-around or a backreference.
    pub(crate) fn needs_backtracking(&self) -> bool {
        matches!(self.engine, PatternEngine::Fancy(_))
    }

    /// The pattern as written in the config.
    pub(crate) fn as_str(&self) -> &str {
        match &self.engine {
            PatternEngine::Fancy(regex) => regex.as_str(),
            PatternEngine::Plain(regex) => regex.as_str(),
        }
    }

    /// Whether the pattern matches `text`, or why that cannot be known.
    pub(crate) fn is_match(&self, text: &str) -> Result<bool, Unevaluated> {
        if self.needs_backtracking() && text.len() > LOOKAROUND_MAX_TEXT_BYTES {
            return Err(Unevaluated::TextTooLong(text.len()));
        }
        match &self.engine {
            PatternEngine::Plain(regex) => Ok(regex.is_match(text)),
            PatternEngine::Fancy(regex) => regex.is_match(text).map_err(Unevaluated::Engine),
        }
    }
}

/// Whether the `regex` crate's parser refuses `pattern` for a look-around or a backreference.
fn uses_look_around_or_backreference(pattern: &str) -> bool {
    use regex_syntax::ast::ErrorKind;
    regex_syntax::ast::parse::Parser::new()
        .parse(pattern)
        .is_err_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::UnsupportedLookAround | ErrorKind::UnsupportedBackreference
            )
        })
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
/// at its backtrack limit, and is not run at all on a matched text (the path by default) longer
/// than [`LOOKAROUND_MAX_TEXT_BYTES`]. The caller passes the answer that keeps the URL out of the
/// crawl (excluded, or not included), so a pattern that cannot be evaluated never widens the
/// crawl. A crawl's seed is exempt from the include check, so an include pattern never drops it.
/// The warning is logged for the first such URL only, so its count does not grow with the
/// number of URLs.
fn pattern_matches(pattern: &PathPattern, text: &str, on_error: bool) -> bool {
    pattern.is_match(text).unwrap_or_else(|error| {
        if !pattern.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                pattern = pattern.as_str(),
                %error,
                "path pattern could not be evaluated; URLs it cannot evaluate are kept out of the crawl, \
                 and later ones are not logged"
            );
        }
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
            assert!(
                matches!(pattern.engine, PatternEngine::Plain(_)),
                "{source} must be compiled by the regex crate"
            );
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
    fn only_a_pattern_the_regex_crate_refuses_needs_backtracking() {
        for plain in ["^/docs", r"\?p=\d+", r"(?-u)\w", r"^https://example\.com/private/"] {
            let pattern = PathPattern::new(plain).expect("valid pattern");
            assert!(!pattern.needs_backtracking(), "{plain} must run on the regex crate");
        }
        for backtracking in ["^/(?!private/)", r".*(?<!\.html)$", r"^/(a|aa)+\1b"] {
            let pattern = PathPattern::new(backtracking).expect("valid pattern");
            assert!(pattern.needs_backtracking(), "{backtracking} must run on fancy_regex");
        }
    }

    /// Patterns the `regex` crate refuses for a reason other than look-around or a backreference.
    /// `fancy_regex` accepts each one with a meaning of its own: `a{2,1}` matches "aa", `a{,3}`
    /// matches every text, `\O` matches any character and `\N{..}` matches nothing.
    const MALFORMED_PATTERNS: [&str; 4] = [r"a{2,1}", r"a{,3}", r"\O", r"\N{LATIN SMALL LETTER A}"];

    #[test]
    fn a_malformed_pattern_is_refused_and_look_around_still_compiles() {
        for malformed in MALFORMED_PATTERNS {
            let error = PathPattern::new(malformed).err().expect("{malformed} must be refused");
            assert_eq!(
                error.to_string(),
                regex::Regex::new(malformed).unwrap_err().to_string(),
                "{malformed} must report the regex crate's own error text"
            );
            let config = CrawlConfig {
                exclude_paths: vec![malformed.to_owned()],
                ..CrawlConfig::default()
            };
            assert!(config.validate().is_err(), "{malformed} must refuse the config");
        }
        for backtracking in [r"^/(?!private/)", r"^/(a)\1"] {
            let pattern = PathPattern::new(backtracking).expect("valid pattern");
            assert!(
                pattern.needs_backtracking(),
                "{backtracking} must compile with fancy_regex"
            );
        }
    }

    /// A URL whose full text is exactly `len` bytes, a path of `x`s.
    fn full_url_of_len(len: usize) -> Url {
        const PREFIX: &str = "https://example.com/";
        let url = Url::parse(&format!("{PREFIX}{}", "x".repeat(len - PREFIX.len()))).expect("valid URL");
        assert_eq!(PathPatternTarget::FullUrl.text(&url).len(), len);
        url
    }

    /// Never matches a URL of `x`s, so only a URL that cannot be evaluated is excluded by it.
    const NEGATIVE_LOOK_AHEAD: &str = r"^https://example\.com/(?!x)";

    #[test]
    #[serial_test::serial(path_pattern_warning)]
    fn look_around_pattern_is_not_evaluated_on_a_url_over_the_limit() {
        let exclude = compile_regexes(&[NEGATIVE_LOOK_AHEAD.to_owned()]).expect("valid pattern");
        let url = full_url_of_len(LOOKAROUND_MAX_TEXT_BYTES + 1);
        assert!(matches!(
            exclude[0].is_match(&PathPatternTarget::FullUrl.text(&url)),
            Err(Unevaluated::TextTooLong(len)) if len == LOOKAROUND_MAX_TEXT_BYTES + 1
        ));
        let mut urls_filtered = 0usize;
        let warnings = record_warnings(|_| {
            assert!(
                !passes_path_patterns(
                    &url,
                    &exclude,
                    &[],
                    true,
                    PathPatternTarget::FullUrl,
                    &mut urls_filtered
                ),
                "an exclude pattern that cannot be evaluated on a long URL must count as a match"
            );
        });
        assert_eq!(urls_filtered, 1);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .error
                .contains(&format!("is {} bytes", LOOKAROUND_MAX_TEXT_BYTES + 1))
                && warnings[0]
                    .error
                    .contains(&format!("up to {LOOKAROUND_MAX_TEXT_BYTES} bytes")),
            "the warning must name the text length and the limit: {warnings:?}"
        );
        assert!(
            warnings[0].pattern == NEGATIVE_LOOK_AHEAD,
            "the warning must name the pattern: {warnings:?}"
        );
    }

    #[test]
    fn look_around_pattern_is_evaluated_on_a_url_at_the_limit() {
        let exclude = compile_regexes(&[NEGATIVE_LOOK_AHEAD.to_owned()]).expect("valid pattern");
        let url = full_url_of_len(LOOKAROUND_MAX_TEXT_BYTES);
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(
                &url,
                &exclude,
                &[],
                true,
                PathPatternTarget::FullUrl,
                &mut urls_filtered
            ),
            "a {LOOKAROUND_MAX_TEXT_BYTES}-byte URL must still be evaluated, and the pattern does not match it"
        );
        assert_eq!(urls_filtered, 0);
    }

    #[test]
    fn plain_pattern_is_evaluated_on_a_url_over_the_limit() {
        let exclude = compile_regexes(&[r"^https://example\.com/y".to_owned()]).expect("valid pattern");
        let url = full_url_of_len(LOOKAROUND_MAX_TEXT_BYTES * 32);
        let mut urls_filtered = 0usize;
        assert!(
            passes_path_patterns(
                &url,
                &exclude,
                &[],
                true,
                PathPatternTarget::FullUrl,
                &mut urls_filtered
            ),
            "the text limit applies to look-around and backreference patterns only"
        );
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
    #[serial_test::serial(path_pattern_warning)]
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
    #[serial_test::serial(path_pattern_warning)]
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

    /// A URL of `len` characters ending in `.html`, which `.*(?<!\.html)$` never matches: every
    /// start position is tried, so the backtracks needed grow with the square of `len`.
    fn html_url_text(len: usize) -> String {
        format!("https://example.com/{}.html", "x".repeat(len - 25))
    }

    #[test]
    fn backtrack_limit_is_lower_than_the_library_default() {
        const QUADRATIC: &str = r".*(?<!\.html)$";
        let long = html_url_text(600);
        assert!(
            fancy_regex::Regex::new(QUADRATIC)
                .expect("valid pattern")
                .is_match(&long)
                .is_ok(),
            "the fixture must finish under the library's default limit, or it cannot tell the limits apart"
        );
        let pattern = PathPattern::new(QUADRATIC).expect("valid pattern");
        assert!(
            pattern.is_match(&long).is_err(),
            "a 600-character URL must exceed the path pattern backtrack limit"
        );
        assert_eq!(
            pattern.is_match(&html_url_text(150)).ok(),
            Some(false),
            "the same pattern must still be evaluated on a 150-character URL"
        );
    }

    /// A `tracing` subscriber that records the `error` and `pattern` fields of each WARN event.
    struct WarnRecorder(Arc<std::sync::Mutex<Vec<WarnFields>>>);

    /// The `error` and `pattern` fields of one event, as the log shows them.
    #[derive(Default, Clone, Debug)]
    struct WarnFields {
        error: String,
        pattern: String,
    }

    impl tracing::field::Visit for WarnFields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            match field.name() {
                "error" => self.error = format!("{value:?}"),
                "pattern" => self.pattern = format!("{value:?}"),
                _ => {}
            }
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "pattern" {
                self.pattern = value.to_owned();
            }
        }
    }

    /// Run `f` with a [`WarnRecorder`] as the default subscriber, and return the `error` and
    /// `pattern` fields of each WARN event it logged. `f` gets the number of warnings logged so far.
    fn record_warnings(f: impl FnOnce(&dyn Fn() -> usize)) -> Vec<WarnFields> {
        let warnings = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logged = || warnings.lock().expect("no test panicked while holding the lock").len();
        tracing::subscriber::with_default(WarnRecorder(Arc::clone(&warnings)), || f(&logged));
        warnings
            .lock()
            .expect("no test panicked while holding the lock")
            .clone()
    }

    impl tracing::Subscriber for WarnRecorder {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                let mut fields = WarnFields::default();
                event.record(&mut fields);
                self.0
                    .lock()
                    .expect("no test panicked while holding the lock")
                    .push(fields);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// ~keep Every test that reaches the warning runs in the `path_pattern_warning` group. While
    /// one test's subscriber is the only one registered, tracing-core computes the interest of
    /// a callsite first reached on another thread from that thread's subscriber (none) and
    /// caches "never", so a concurrent test that reaches the warning first hides it from this one.
    #[test]
    #[serial_test::serial(path_pattern_warning)]
    fn a_pattern_that_cannot_be_evaluated_warns_once_per_compilation() {
        let warnings = record_warnings(|logged| {
            let exclude = compile_regexes(&[BACKTRACK_BOMB.to_owned()]).expect("valid pattern");
            let cloned = exclude.clone();
            let mut urls_filtered = 0usize;
            for patterns in [&exclude, &cloned, &exclude, &cloned] {
                passes_path_patterns(
                    &backtrack_bomb_url(),
                    patterns,
                    &[],
                    true,
                    PathPatternTarget::Path,
                    &mut urls_filtered,
                );
            }
            assert_eq!(urls_filtered, 4, "every URL must still be kept out of the crawl");
            assert_eq!(logged(), 1, "a pattern and its clones must warn once, not once per URL");

            let recompiled = compile_regexes(&[BACKTRACK_BOMB.to_owned()]).expect("valid pattern");
            passes_path_patterns(
                &backtrack_bomb_url(),
                &recompiled,
                &[],
                true,
                PathPatternTarget::Path,
                &mut urls_filtered,
            );
        });
        assert_eq!(
            warnings.len(),
            2,
            "a pattern compiled for another crawl must warn again"
        );
        let engine_error = fancy_regex::RegexBuilder::new(BACKTRACK_BOMB)
            .backtrack_limit(PATH_PATTERN_BACKTRACK_LIMIT)
            .build()
            .expect("valid pattern")
            .is_match(backtrack_bomb_url().path())
            .expect_err("the fixture hits the backtrack limit")
            .to_string();
        assert!(!engine_error.is_empty());
        assert_eq!(
            warnings.iter().map(|w| w.error.clone()).collect::<Vec<_>>(),
            [engine_error.clone(), engine_error],
            "the warning must carry the engine's error"
        );
        assert!(
            warnings.iter().all(|w| w.pattern == BACKTRACK_BOMB),
            "the warning must name the pattern: {warnings:?}"
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
