//! Shared helper functions used by the crawl engine.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use url::{Position, Url};

use crate::error::CrawlError;
use crate::http::http_fetch_robots_txt;
use crate::robots::{RobotsRules, is_path_allowed, parse_robots_txt};
use crate::types::CrawlConfig;

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
/// ~keep The robots.txt fetch maps most non-2xx statuses to typed errors before returning, so the
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
        // `WafBlocked` is here for a different reason: the robots.txt fetch raises it for a 403 but
        // also for a *2xx* block page of any size the classifier reads (http.rs), so it does not
        // imply a 4xx at all.
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
    if let Some(value) = custom_user_agent_header(config) {
        return value;
    }
    config
        .user_agent
        .as_deref()
        .unwrap_or(concat!("crawlberg/", env!("CARGO_PKG_VERSION")))
}

/// A `user-agent` entry in `config.custom_headers`, matched case-insensitively as HTTP
/// header names are. Blank (empty or whitespace-only) counts as absent: a caller who unsets
/// the header by emptying its value, rather than removing the key, gets the configured or
/// default agent instead of an empty one (crawlberg#423).
///
/// ~keep `apply_headers` (tower/service.rs) always layers `custom_headers` onto the request
/// to the seed's host, so a caller-set `user-agent` there is the agent that actually goes out
/// on the wire, ahead of `config.user_agent` and the rotation layer's own default. Every
/// caller of `default_robots_user_agent` -- the engine's robots.txt group selection, the
/// header realized on the wire, and the fallback `scrape()`/crawl-loop directive matching use
/// when no rotation pinned a value -- reads this one function, so this is the single place a
/// custom-header agent needs to be taken into account for robots decisions to judge the agent
/// actually sent (crawlberg#423).
fn custom_user_agent_header(config: &CrawlConfig) -> Option<&str> {
    config
        .custom_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

/// Whether `name`/`value` is a `custom_headers` entry naming `user-agent` with no real value.
///
/// ~keep Shared by [`custom_user_agent_header`] (the judging side) and
/// [`crate::net::credentials::seed_host_headers`] (the sending side, read by `apply_headers`,
/// the chromiumoxide SSRF interceptor, and both native-browser `origin_headers` builders): a
/// blank `user-agent` entry must be treated as absent by every one of them, or robots would
/// judge the configured agent while a browser tier still puts an empty header on the wire
/// (crawlberg#423).
pub(crate) fn is_blank_user_agent_override(name: &str, value: &str) -> bool {
    name.eq_ignore_ascii_case("user-agent") && value.trim().is_empty()
}

pub(crate) async fn fetch_robots_outcome(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    user_agent: &str,
) -> RobotsOutcome {
    fetch_robots_document(url, config, client, user_agent).await.0
}

/// [`fetch_robots_outcome`], plus the address that served robots.txt after redirects when the
/// file was read. A relative `Sitemap:` line resolves against that address.
pub(crate) async fn fetch_robots_document(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    user_agent: &str,
) -> (RobotsOutcome, Option<String>) {
    let Ok(parsed) = Url::parse(url) else {
        let outcome = RobotsOutcome::DisallowAll {
            reason: format!("invalid URL: {}", crate::net::redact_url_credentials(url)),
            denial: RobotsDenial::Sustained,
        };
        return (outcome, None);
    };
    // ~keep `robots_url` uses `authority()`, which keeps a non-default port. Building this
    // from `host_str()` instead sent every port-bearing seed's robots request to the default
    // port, where it failed and silently degraded to "no rules".
    let robots_url = crate::normalize::robots_url(&parsed);
    match http_fetch_robots_txt(&robots_url, config, client).await {
        Ok(resp) if resp.status >= 500 => {
            let outcome = RobotsOutcome::DisallowAll {
                reason: format!("robots.txt returned HTTP {}", resp.status),
                denial: RobotsDenial::Sustained,
            };
            (outcome, None)
        }
        Ok(resp) if resp.status >= 400 => (RobotsOutcome::AllowAll, None),
        Ok(resp) => (
            RobotsOutcome::Rules(parse_robots_txt(&resp.body, user_agent)),
            Some(resp.final_url),
        ),
        Err(error) => (outcome_for_fetch_error(&error), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_allow_all(outcome: &RobotsOutcome) -> bool {
        matches!(outcome, RobotsOutcome::AllowAll)
    }

    #[tokio::test]
    async fn an_unparseable_robots_address_is_named_through_the_redactor() {
        let config = CrawlConfig::builder().allow_private_networks(false).build();
        let client = crate::http::build_client(&config).expect("client must build");
        let outcome = fetch_robots_outcome("alice@example.com", &config, &client, "ua").await;
        assert_eq!(
            outcome.disallow_all_reason(),
            Some("invalid URL: [address hidden: it may carry credentials]"),
            "an address that does not parse must be refused without showing its credential"
        );
    }

    fn describe(outcome: &RobotsOutcome) -> String {
        match outcome {
            RobotsOutcome::Rules(_) => "Rules".to_owned(),
            RobotsOutcome::AllowAll => "AllowAll".to_owned(),
            RobotsOutcome::DisallowAll { reason, .. } => format!("DisallowAll({reason})"),
        }
    }

    /// The robots.txt outcome for a 200 carrying `body` and `headers`.
    async fn robots_outcome_for(body: String, headers: &[(&str, &str)]) -> RobotsOutcome {
        robots_outcome_as("text/html", body, headers).await
    }

    /// [`robots_outcome_for`] with `content_type` as the response's content type.
    async fn robots_outcome_as(content_type: &str, body: String, headers: &[(&str, &str)]) -> RobotsOutcome {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut template = ResponseTemplate::new(200)
            .append_header("content-type", content_type)
            .set_body_string(body);
        for (name, value) in headers {
            template = template.append_header(*name, *value);
        }
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(template)
            .mount(&mock)
            .await;
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.retry_count = 0;
        let client = crate::http::build_client(&config).expect("client must build");
        fetch_robots_outcome(&format!("{}/page", mock.uri()), &config, &client, "bot").await
    }

    /// A robots.txt follows an HTTP redirect only: a `Refresh` header on it is not followed, so its
    /// own rules apply.
    #[tokio::test]
    async fn a_robots_txt_does_not_follow_a_refresh() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "text/plain")
                    .append_header("refresh", "0; url=/elsewhere.txt")
                    .set_body_string("User-agent: *\nDisallow: /private\n"),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/elsewhere.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "text/plain")
                    .set_body_string("User-agent: *\nDisallow: /\n"),
            )
            .mount(&mock)
            .await;
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.retry_count = 0;
        let client = crate::http::build_client(&config).expect("client must build");

        let outcome = fetch_robots_outcome(&format!("{}/page", mock.uri()), &config, &client, "bot").await;

        assert!(
            matches!(outcome, RobotsOutcome::Rules(_)) && outcome.allows("/public") && !outcome.allows("/private"),
            "a robots.txt with a Refresh header must be read as its own rules, got {} with /public={} /private={}",
            describe(&outcome),
            outcome.allows("/public"),
            outcome.allows("/private")
        );
    }

    /// A robots.txt answered with a block page denies the whole origin, whatever the page's size
    /// up to the classifier's 100 KB limit, so a WAF-protected site never gets an unrestricted crawl.
    #[tokio::test]
    async fn a_robots_txt_block_page_denies_the_origin_at_any_size_the_classifier_reads() {
        let tag = "<script src=\"https://js.datadome.co/tags.js\"></script>";
        for (label, len, headers) in [
            (
                "x-datadome and the tag, 400 bytes",
                400,
                &[("x-datadome", "protected")][..],
            ),
            ("x-datadome and the tag, 6 KB", 6000, &[("x-datadome", "protected")][..]),
            ("the tag alone, 6 KB", 6000, &[][..]),
            (
                "x-datadome and the tag, 90 KB",
                90_000,
                &[("x-datadome", "protected")][..],
            ),
        ] {
            let body = format!("<html>{tag}<!--{}--></html>", "x".repeat(len));
            let outcome = robots_outcome_for(body, headers).await;
            assert!(
                outcome
                    .disallow_all_reason()
                    .is_some_and(|reason| reason.contains("datadome")),
                "{label}: a robots.txt block page must deny the origin, got {}",
                describe(&outcome)
            );
            assert!(!outcome.allows("/private"), "{label}: /private must not be allowed");
        }
    }

    /// A real robots.txt served through a CDN that stamps its own header is read as rules.
    #[tokio::test]
    async fn a_robots_txt_behind_a_cdn_presence_header_is_read_as_rules() {
        let outcome = robots_outcome_for(
            "User-agent: *\nDisallow: /private\n".to_owned(),
            &[("x-sucuri-id", "18012")],
        )
        .await;
        assert!(
            matches!(outcome, RobotsOutcome::Rules(_)),
            "an ordinary robots.txt must be read as rules, got {}",
            describe(&outcome)
        );
        assert!(outcome.allows("/public"), "the rules must allow /public");
        assert!(!outcome.allows("/private"), "the rules must disallow /private");
    }

    /// The robots.txt from crawlberg#507: rules with "blocked" in a comment, served by Cloudflare.
    fn robots_txt_with_a_blocked_comment(padding_rules: usize) -> String {
        format!(
            "# AI crawlers are blocked below\nUser-agent: GPTBot\nDisallow: /\n\nUser-agent: *\nDisallow: /private\n{}",
            "Disallow: /archive/page-000000\n".repeat(padding_rules)
        )
    }

    /// A robots.txt that says "blocked" in a comment is the site's rules, not a Cloudflare block
    /// page, below and above the 5000-byte page limit (crawlberg#507).
    #[tokio::test]
    async fn a_robots_txt_that_says_blocked_in_a_comment_is_read_as_rules_behind_cloudflare() {
        for (padding_rules, len) in [(0, 97), (200, 6297)] {
            let body = robots_txt_with_a_blocked_comment(padding_rules);
            assert_eq!(body.len(), len, "the fixture must be the issue's {len}-byte body");
            let outcome = robots_outcome_for(body, &[("server", "cloudflare")]).await;
            assert!(
                matches!(outcome, RobotsOutcome::Rules(_)),
                "{len} bytes: a robots.txt with rules must be read as rules, got {}",
                describe(&outcome)
            );
            assert!(outcome.allows("/public"), "{len} bytes: the rules must allow /public");
            assert!(
                !outcome.allows("/private"),
                "{len} bytes: the rules must disallow /private"
            );
        }
    }

    /// A robots.txt that opens with a UTF-8 byte-order mark still has its first group read
    /// (crawlberg#516), served behind Cloudflare so the fix does not disturb the #514 block-page
    /// classifier.
    #[tokio::test]
    async fn a_robots_txt_with_a_leading_byte_order_mark_is_read_as_rules() {
        let body = "\u{feff}User-agent: *\r\nDisallow: /private\r\n# blocked\r\n";
        let outcome = robots_outcome_as("text/plain", body.to_owned(), &[("server", "cloudflare")]).await;
        assert!(
            matches!(outcome, RobotsOutcome::Rules(_)),
            "a robots.txt with a leading BOM must be read as rules, got {}",
            describe(&outcome)
        );
        assert!(outcome.allows("/public"), "the rules must allow /public");
        assert!(
            !outcome.allows("/private"),
            "the leading BOM must not hide the Disallow rule, so /private must stay refused"
        );
    }

    /// A Cloudflare block page served as /robots.txt still denies the origin: an HTML page, even
    /// one that shows robots.txt lines, and a text page with no robots.txt directive.
    #[tokio::test]
    async fn a_robots_txt_block_page_behind_cloudflare_still_denies_the_origin() {
        let block_page = "<html><head><title>Attention Required</title></head><body><h1>Sorry, you have been blocked</h1></body></html>";
        for (label, body) in [
            ("an HTML block page", block_page.to_owned()),
            (
                "an HTML block page of 6 KB",
                format!("{block_page}<!--{}-->", "x".repeat(6000)),
            ),
            (
                "an HTML block page that shows robots.txt lines",
                "<html><body><pre>\nUser-agent: *\nDisallow: /private\n</pre><h1>Access blocked</h1></body></html>"
                    .to_owned(),
            ),
            (
                "a text block page with no robots.txt directive",
                "Status: blocked\nReason: automated traffic\n".to_owned(),
            ),
        ] {
            let outcome = robots_outcome_for(body, &[("server", "cloudflare")]).await;
            assert!(
                outcome
                    .disallow_all_reason()
                    .is_some_and(|reason| reason.contains("cloudflare")),
                "{label}: a Cloudflare block page must deny the origin, got {}",
                describe(&outcome)
            );
            assert!(!outcome.allows("/public"), "{label}: /public must not be allowed");
        }
    }

    /// A label, the content type, the body and the headers of one 2xx robots.txt response.
    type Served = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    );

    const CLOUDFLARE: &[(&str, &str)] = &[("server", "cloudflare")];
    const SUCURI: &[(&str, &str)] = &[("x-sucuri-id", "18012"), ("server", "Sucuri/Cloudproxy")];

    /// Assert that every block page in `cases`, served as /robots.txt, denies the origin.
    async fn assert_every_block_page_denies(cases: &[Served]) {
        for &(label, content_type, body, headers) in cases {
            let outcome = robots_outcome_as(content_type, body.to_owned(), headers).await;
            assert!(
                outcome.disallow_all_reason().is_some(),
                "{label}: a block page must deny the origin, got {}",
                describe(&outcome)
            );
            assert!(!outcome.allows("/public"), "{label}: /public must not be allowed");
        }
    }

    /// A 2xx text block page that echoes request headers, or carries a lone Sitemap, User-agent
    /// or `allow:` line, denies the origin.
    #[tokio::test]
    async fn a_text_block_page_that_echoes_robots_like_lines_still_denies_the_origin() {
        assert_every_block_page_denies(&[
            (
                "a Cloudflare text page that echoes User-Agent",
                "text/plain",
                "Sorry, you have been blocked\nYou are unable to access example.com\nRay ID: 8c1f2a3b4d5e6f70\nUser-Agent: bot\nIP: 203.0.113.9\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare challenge script with an allow key",
                "application/javascript",
                "window._cf_chl_opt = {\n  cvId: '3',\n  cType: 'managed',\n  allow: false,\n  cRay: '8c1f2a3b4d5e6f70'\n};\n",
                CLOUDFLARE,
            ),
            (
                "a DataDome text page that echoes User-Agent",
                "text/plain",
                "Blocked by DataDome\nUser-Agent: bot\nReference: AHrlqAAAAAMA\n",
                &[("x-datadome", "protected")],
            ),
            (
                "a Sucuri text page that echoes User-Agent",
                "text/plain",
                "Sucuri WebSite Firewall - Access Denied\nBlock reason: Access from your area has been temporarily denied.\nYour IP: 203.0.113.9\nURL: example.com/robots.txt\nUser-Agent: bot\nBlock ID: GEO01\n",
                SUCURI,
            ),
            (
                "a generic text page that echoes the request headers",
                "text/plain",
                "Request blocked.\nHost: example.com\nUser-Agent: bot\nAccept: */*\n",
                &[],
            ),
            (
                "a Cloudflare text page with a lone Sitemap line",
                "text/plain",
                "Access blocked\nSitemap: https://example.com/sitemap.xml\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare text page with an empty User-agent line",
                "text/plain",
                "Access blocked\nUser-agent:\n",
                CLOUDFLARE,
            ),
        ])
        .await;
    }

    /// A 2xx block page that quotes robots.txt rules, or echoes a User-Agent header before an
    /// Allow or Disallow line, denies the origin: its fingerprint is outside any comment.
    #[tokio::test]
    async fn a_block_page_that_quotes_robots_txt_rules_still_denies_the_origin() {
        assert_every_block_page_denies(&[
            (
                "a Cloudflare help page that quotes rules allowing everything",
                "text/plain",
                "Sorry, you have been blocked\nIf you run a crawler, add these lines to your robots.txt\nUser-agent: *\nAllow: /\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare help page that quotes a rule for another bot",
                "text/plain",
                "Sorry, you have been blocked\nOur robots.txt reads\nUser-agent: BadBot\nDisallow: /\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare page that echoes our User-Agent and the path",
                "text/plain",
                "Request blocked\nUser-Agent: bot\nDisallow: /\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare page that echoes a browser User-Agent and the path",
                "text/plain",
                "Request blocked\nUser-Agent: Mozilla/5.0 (compatible; bot/1.0)\nDisallow: /\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare page that echoes User-Agent and an HTTP Allow header",
                "text/plain",
                "Request blocked\nUser-Agent: bot\nAllow: GET, HEAD\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare script after a User-agent echo",
                "application/javascript",
                "// blocked\nUser-agent: bot\nwindow._cf_chl_opt = {\n  cType: 'managed',\n  allow: false,\n};\n",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare script with quoted User-agent and allow keys",
                "application/javascript",
                "// blocked\nwindow.cfg = {\n  'User-agent': 'bot',\n  allow: false,\n};\n",
                CLOUDFLARE,
            ),
            (
                "a Sucuri page with a Disallow reason line",
                "text/plain",
                "Sucuri WebSite Firewall - Access Denied\nUser-Agent: bot\nDisallow: automated clients\n",
                &[("x-sucuri-id", "18012")],
            ),
            (
                "a Cloudflare HTML help page that quotes rules",
                "text/html",
                "<p>Sorry, you have been blocked</p>\nUser-agent: *\nAllow: /\n",
                CLOUDFLARE,
            ),
        ])
        .await;
    }

    /// A real robots.txt whose only fingerprint match is in a whole-line comment is read as rules,
    /// whatever directives it holds.
    #[tokio::test]
    async fn a_robots_txt_whose_fingerprint_is_only_in_a_whole_line_comment_is_read_as_rules() {
        let cases: [Served; 6] = [
            (
                "Crawl-delay only",
                "text/plain",
                "# scrapers blocked by rate\nUser-agent: *\nCrawl-delay: 10\n",
                CLOUDFLARE,
            ),
            (
                "Sitemap only",
                "text/plain",
                "# nothing blocked here\nSitemap: https://example.com/sitemap.xml\n",
                CLOUDFLARE,
            ),
            (
                "an empty Disallow",
                "text/plain",
                "# nothing blocked\nUser-agent: *\nDisallow:\n",
                CLOUDFLARE,
            ),
            (
                "a rule before any User-agent line",
                "text/plain",
                "# blocked paths\nDisallow: /private\nSitemap: https://example.com/s.xml\n",
                CLOUDFLARE,
            ),
            (
                "a comment that names Sucuri",
                "text/plain",
                "# protected by Sucuri\nUser-agent: *\nDisallow: /private\n",
                &[],
            ),
            (
                "a comment that says request blocked",
                "text/plain",
                "# request blocked for scrapers\nUser-agent: *\nDisallow: /private\n",
                &[],
            ),
        ];
        for (label, content_type, body, headers) in cases {
            let outcome = robots_outcome_as(content_type, body.to_owned(), headers).await;
            assert!(
                matches!(outcome, RobotsOutcome::Rules(_)),
                "{label}: a robots.txt with the fingerprint only in a whole-line comment must be read as rules, got {}",
                describe(&outcome)
            );
        }
    }

    /// A robots.txt whose fingerprint word is outside a whole-line comment still denies behind a
    /// matching header, as does one with a `<` anywhere: only whole-line comments of a body with
    /// no `<` are left out of the check.
    #[tokio::test]
    async fn a_robots_txt_whose_fingerprint_is_outside_a_whole_line_comment_still_denies() {
        for (label, body) in [
            (
                "a word in a rule",
                "# crawl rules\nUser-agent: *\nDisallow: /blocked-users\n",
            ),
            (
                "a word in a trailing comment",
                "User-agent: *\nDisallow: /private # blocked for bots\n",
            ),
            (
                "a word in a whole-line comment of a body with a `<`",
                "# blocked bots\nUser-agent: *\nDisallow: /private\nDisallow: /*<script\n",
            ),
        ] {
            let outcome = robots_outcome_as("text/plain", body.to_owned(), CLOUDFLARE).await;
            assert!(
                outcome
                    .disallow_all_reason()
                    .is_some_and(|reason| reason.contains("cloudflare")),
                "{label}: the fingerprint must deny the origin, got {}",
                describe(&outcome)
            );
        }
    }

    /// An HTML block page with a `#` in a style rule or a link still denies: a `#` before the block
    /// phrase on its line, or a style rule that starts a line with `#`, does not hide the phrase.
    #[tokio::test]
    async fn an_html_block_page_with_a_hash_still_denies_the_origin() {
        assert_every_block_page_denies(&[
            (
                "a one-line Cloudflare page with a style rule before the phrase",
                "text/html",
                "<html><head><style>h1{color:#333}</style></head><body><h1>Sorry, you have been blocked</h1></body></html>",
                CLOUDFLARE,
            ),
            (
                "the same Cloudflare page on two lines",
                "text/html",
                "<html><head><style>h1{color:#333}</style></head>\n<body><h1>Sorry, you have been blocked</h1></body></html>\n",
                CLOUDFLARE,
            ),
            (
                "a one-line DataDome page with a style rule before the tag",
                "text/html",
                "<html><head><style>#cmsg{display:none}</style><script src=\"https://js.datadome.co/tags.js\"></script></head></html>",
                &[("x-datadome", "protected")],
            ),
            (
                "a one-line Cloudflare page with a link after the phrase",
                "text/html",
                "<html><body><h1>Sorry, you have been blocked</h1><a href=\"#\">x</a></body></html>",
                CLOUDFLARE,
            ),
            (
                "a Cloudflare page whose only match is a style rule that starts a line",
                "text/html",
                "<html><head><style>\n#blocked-msg { color: red }\n</style></head>\n<body><p>Access denied</p></body></html>\n",
                CLOUDFLARE,
            ),
        ])
        .await;
    }

    /// The classifier's body limit is taken on the robots.txt as fetched: a file over 100 KB is
    /// read as rules even when it is mostly comments.
    #[tokio::test]
    async fn a_robots_txt_over_the_body_limit_is_read_as_rules_even_when_mostly_comments() {
        let body = format!(
            "{}User-agent: *\nDisallow: /blocked-users\n",
            "# archive\n".repeat(11_000)
        );
        assert!(
            body.len() > 100 * 1024,
            "the fixture must be over the 100 KB body limit"
        );
        let outcome = robots_outcome_as("text/plain", body, CLOUDFLARE).await;
        assert!(
            matches!(outcome, RobotsOutcome::Rules(_)),
            "a robots.txt over the body limit must be read as rules, got {}",
            describe(&outcome)
        );
        assert!(
            !outcome.allows("/blocked-users"),
            "the rules must disallow /blocked-users"
        );
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

    #[test]
    fn host_anchored_exclude_pattern_matches_a_url_that_carries_only_a_password() {
        let url = Url::parse("https://:hunter2@example.com/private/a").expect("valid URL");
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
            "a password with no username in a link must not let it escape a host-anchored exclude pattern"
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

    /// A Unicode-aware repetition compiles past 1 MiB, so the size limit must not be lower than
    /// the `regex` crate's own default.
    #[test]
    fn a_pattern_larger_than_one_mebibyte_compiled_still_compiles() {
        let pattern = PathPattern::new(r"\w{30}").expect("the regex crate's default size limit accepts it");
        assert!(!pattern.needs_backtracking());
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
