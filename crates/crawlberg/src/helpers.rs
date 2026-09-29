//! Shared helper functions used by the crawl engine.

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::http::http_fetch_robots_txt;
use crate::robots::{RobotsRules, is_path_allowed, parse_robots_txt};
use crate::types::CrawlConfig;

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
