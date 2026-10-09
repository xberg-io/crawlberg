//! HTTP fetching with redirect handling, retry logic, and cookie extraction.

mod body;
mod challenge;
mod client;
mod headers;
mod retry;
mod status;
mod waf;

use std::collections::HashMap;

use reqwest::header::{CONTENT_TYPE, HeaderMap, USER_AGENT};

use crate::error::{CrawlError, classify_reqwest_error, error_chain_string};
use crate::html::{PageScan, is_fetchable_scheme};
use crate::net::credentials::seed_host_headers;
use crate::net::ssrf::validate_url;
use crate::types::CrawlConfig;

pub(crate) use body::validate_content_encoding;
pub(crate) use body::{
    effective_max_body_size, read_body_bounded, redecode_with_charset, truncate_body_at_char_boundary,
};
pub(crate) use challenge::{challenge_status_error, is_challenge_status};
pub(crate) use client::{build_client, request_client};
pub(crate) use headers::build_headers_map;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use headers::extract_cookies_from_hashmap;
pub(crate) use headers::extract_response_meta_from_hashmap;
pub(crate) use retry::{fetch_with_retry, should_retry_error};
pub(crate) use status::status_error;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use status::{HttpStatus, error_status};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use waf::{engine_waf_signal, record_waf_block, waf_2xx_error};

/// Statuses that carry no document. A browser commits nothing for them, so a browser fetch
/// reports them with an empty body, as the HTTP fetch does.
///
/// ~keep Gated on the browser features, unlike `REDIRECT_STATUSES`: only the browser backends
/// ~keep read it, while the HTTP path reads `REDIRECT_STATUSES` on every build.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) const NO_DOCUMENT_STATUSES: [u16; 3] = [204, 205, 304];

/// Browser-specific extras attached to an `HttpResponse` produced by the native
/// browser backend. Populated when `browser_used` is true.
///
/// Exposed as `pub` because it is a field of the public [`HttpResponse`] struct.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct BrowserExtras {
    /// Result of an in-page JavaScript evaluation, if any.
    pub eval_result: Option<serde_json::Value>,
    /// Network-level metadata for sub-resource requests recorded by the browser.
    pub network_events: Vec<crate::types::ResponseMeta>,
    /// Cookies present in the browser session after page load.
    pub cookies: Vec<crate::types::CookieInfo>,
}

/// An HTTP response with status, headers, and body content.
///
/// Exposed as `pub` so that [`crate::types::WafClassifier`] implementations —
/// which are defined outside `crate::http` — can inspect responses. All
/// fields that are only used by internal paths carry `#[allow(dead_code)]`.
pub struct HttpResponse {
    /// HTTP status code (e.g. 200, 403).
    pub status: u16,
    /// Value of the `Content-Type` response header, or empty string if absent.
    pub content_type: String,
    /// Decoded response body as UTF-8 text.
    pub body: String,
    /// Raw response body bytes (before UTF-8 decoding).
    pub body_bytes: Vec<u8>,
    /// All response headers, keyed by lowercase header name.
    #[allow(dead_code)]
    pub headers: std::collections::HashMap<String, Vec<String>>,
    /// Optional browser-specific extras (eval result, network events, cookies).
    #[allow(dead_code)]
    pub browser_extras: Option<BrowserExtras>,
    /// The URL of the final response after any redirects.
    ///
    /// On native targets reqwest uses `Policy::none()` and `http_fetch` follows each
    /// redirect hop itself, so this is the URL of the last hop it requested.
    /// On wasm targets the browser's `fetch` follows redirects transparently
    /// and `reqwest::Response::url()` returns the post-redirect URL, which the wasm
    /// scrape path uses to populate `ScrapeResult::final_url`.
    #[allow(dead_code)]
    pub final_url: String,
    /// PNG screenshot bytes captured for this fetch, when the caller requested one
    /// (`CrawlConfig.capture_screenshot`) and the fetch went through a browser backend
    /// that supports capturing it. `None` for every plain-HTTP fetch and for browser
    /// fetches that did not request a screenshot.
    #[allow(dead_code)]
    pub screenshot: Option<Vec<u8>>,
}

/// Everything a fetch needs that does not change from one redirect hop to the next.
struct FetchContext<'a> {
    config: &'a CrawlConfig,
    extra_headers: &'a HashMap<String, String>,
    client: &'a reqwest::Client,
    fetched: Fetched,
}

/// What a fetch reads its response as, which picks the 2xx WAF decision the response gets.
#[derive(Clone, Copy)]
pub(crate) enum Fetched {
    /// A page or asset: [`waf::waf_2xx_error`].
    Page,
    /// A sitemap, or a page `map` reads as one when it is one: [`waf::sitemap_2xx_error`].
    Sitemap,
    /// The site's robots.txt: [`waf::robots_2xx_error`].
    RobotsTxt,
}

/// What one hop produced: a redirect target still to follow, or a finished response.
///
/// ~keep Not boxed despite the variant size gap: `Complete` carries the value `http_fetch`
/// ~keep returns, which was already moved by value out of this code before the hop loop was
/// ~keep split out. Boxing it to satisfy the size-ratio lint would add a heap allocation to
/// ~keep every successful fetch and buy nothing back.
#[allow(clippy::large_enum_variant)]
enum HopOutcome {
    Redirect(url::Url),
    Complete(HttpResponse),
}

/// The response a 404 past the first hop of a followed chain stops on: the crawl's chain reports
/// the same empty 404 at the missing URL.
fn not_found_response(url: &url::Url) -> HttpResponse {
    HttpResponse {
        status: 404,
        content_type: String::new(),
        body: String::new(),
        body_bytes: Vec::new(),
        headers: HashMap::new(),
        browser_extras: None,
        final_url: url.to_string(),
        screenshot: None,
    }
}

/// Where a 3xx response points.
enum RedirectTarget {
    /// The `Location` header, resolved against the URL that served the redirect.
    Follow(url::Url),
    /// A `Location` that does not resolve to a URL, or resolves to one with a scheme the crawler
    /// cannot fetch (`mailto:`, `data:`, `file:`, ...); the 3xx is returned as the response.
    Unfollowable,
}

/// Response metadata captured before the body is consumed.
struct ResponseHead {
    status: u16,
    content_type: String,
    final_url: String,
    headers: HeaderMap,
}

impl ResponseHead {
    fn from_response(resp: &reqwest::Response) -> Self {
        Self {
            status: resp.status().as_u16(),
            content_type: resp
                .headers()
                .get_all(CONTENT_TYPE)
                .iter()
                .next_back()
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned(),
            final_url: resp.url().to_string(),
            headers: resp.headers().clone(),
        }
    }

    fn content_length(&self) -> Option<usize> {
        self.headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
    }

    fn into_response(
        self,
        body: String,
        body_bytes: Vec<u8>,
        headers_map: HashMap<String, Vec<String>>,
    ) -> HttpResponse {
        HttpResponse {
            status: self.status,
            content_type: self.content_type,
            body,
            body_bytes,
            headers: headers_map,
            browser_extras: None,
            final_url: self.final_url,
            screenshot: None,
        }
    }
}

/// Whether a fetch follows a `Refresh` header or a `<meta http-equiv="refresh">` the way it
/// follows an HTTP 3xx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshRedirects {
    /// Follow them as the crawl does, and stop the whole chain where the crawl's chain stops.
    /// The refresh itself is followed on native only: wasm has no refresh reader, and its crawl
    /// follows no refresh either.
    Follow,
    /// Return the response that names them.
    Ignore,
}

/// A check every request of a fetch passes before it goes out: the URL the fetch starts from, then
/// each redirect or refresh hop, after the SSRF policy admits it.
pub(crate) trait HopPolicy {
    /// `Err` refuses `url`, which is then never requested, and ends the fetch with that error.
    /// `is_redirect_hop` is `false` for the URL the fetch starts from and `true` for each hop.
    async fn admit(&mut self, url: &url::Url, is_redirect_hop: bool) -> Result<(), CrawlError>;
}

/// A fetch with no check beyond the SSRF policy.
pub(crate) struct AdmitEvery;

impl HopPolicy for AdmitEvery {
    async fn admit(&mut self, _url: &url::Url, _is_redirect_hop: bool) -> Result<(), CrawlError> {
        Ok(())
    }
}

/// Perform a single HTTP GET request with the given configuration.
///
/// Handles user-agent, authentication, custom headers, error status codes,
/// content-length validation, and SSRF policy enforcement.
///
/// SSRF validation is applied to the initial URL and to every redirect target
/// (manual redirect loop to ensure policy applies to all hops).
pub(crate) async fn http_fetch(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &std::collections::HashMap<String, String>,
    client: &reqwest::Client,
) -> Result<HttpResponse, CrawlError> {
    http_fetch_with(url, config, extra_headers, client, RefreshRedirects::Ignore)
        .await
        .map(|page| page.response)
}

/// A fetched response, with the refresh check's read of its body when that check read one.
pub(crate) struct FetchedPage {
    pub(crate) response: HttpResponse,
    /// The meta refresh check's read of `response`'s body, so the caller does not read the page
    /// again.
    pub(crate) page_scan: Option<PageScan>,
}

impl FetchedPage {
    fn unread(response: HttpResponse) -> Self {
        Self {
            response,
            page_scan: None,
        }
    }
}

/// [`http_fetch`], following a refresh as well when `refresh` says so, with the refresh check's
/// read of the last page.
///
/// ~keep A refresh hop goes through the same loop as a 3xx: the same SSRF check, the same hop
/// ~keep count bounded by `max_redirects`, and the same per-hop credential scope in
/// ~keep `send_hop_request`. When refreshes are followed, every hop also takes the crawl's chain
/// ~keep rules (see `ChainRules`): the chain stops, and never fails, where the crawl's does.
pub(crate) async fn http_fetch_with(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &std::collections::HashMap<String, String>,
    client: &reqwest::Client,
    refresh: RefreshRedirects,
) -> Result<FetchedPage, CrawlError> {
    fetch_as(
        url,
        config,
        extra_headers,
        client,
        refresh,
        Fetched::Page,
        &mut AdmitEvery,
    )
    .await
}

/// [`http_fetch`] for a robots.txt: a 2xx body is refused when it fingerprints as a block page
/// without its whole-line comments, at any size up to the classifier's body limit.
pub(crate) async fn http_fetch_robots_txt(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> Result<HttpResponse, CrawlError> {
    fetch_as(
        url,
        config,
        &HashMap::new(),
        client,
        RefreshRedirects::Ignore,
        Fetched::RobotsTxt,
        &mut AdmitEvery,
    )
    .await
    .map(|page| page.response)
}

/// [`http_fetch`] for a sitemap: a 2xx body that reads as a sitemap document is returned whatever
/// its URLs say, and any other body gets the page decision.
pub(crate) async fn http_fetch_sitemap(
    url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> Result<HttpResponse, CrawlError> {
    fetch_as(
        url,
        config,
        &HashMap::new(),
        client,
        RefreshRedirects::Ignore,
        Fetched::Sitemap,
        &mut AdmitEvery,
    )
    .await
    .map(|page| page.response)
}

/// `policy` admits each request before it goes out: the start URL, then every hop.
async fn fetch_as(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &HashMap<String, String>,
    client: &reqwest::Client,
    refresh: RefreshRedirects,
    fetched: Fetched,
    policy: &mut impl HopPolicy,
) -> Result<FetchedPage, CrawlError> {
    let initial_url = url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;

    validate_url(&initial_url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url, e.to_string()))?;

    let context = FetchContext {
        config,
        extra_headers,
        client,
        fetched,
    };
    let mut rules = ChainRules::new(refresh, &initial_url);
    let mut current_url = initial_url;
    let mut redirects_followed: usize = 0;

    loop {
        policy.admit(&current_url, redirects_followed > 0).await?;
        let hop_left = redirects_followed < config.max_redirects;
        let follows_location = |status: u16, target: &url::Url| rules.follows_location(status, target, hop_left);
        let outcome = match fetch_one_hop(&context, &current_url, follows_location).await {
            Ok(outcome) => outcome,
            Err(error) if redirects_followed > 0 && rules.stops_on(&error) => {
                return Ok(FetchedPage::unread(not_found_response(&current_url)));
            }
            Err(error) => return Err(error),
        };
        let next_url = match outcome {
            HopOutcome::Redirect(next_url) => next_url,
            HopOutcome::Complete(response) => {
                if !hop_left {
                    return Ok(FetchedPage::unread(response));
                }
                let mut page_scan = None;
                match rules.refresh_target(&response, &current_url, &mut page_scan) {
                    Some(next_url) => next_url,
                    None => return Ok(FetchedPage { response, page_scan }),
                }
            }
        };

        if let Err(e) = validate_url(&next_url, &config.ssrf).await {
            return Err(CrawlError::ssrf_violation(&next_url, e.to_string()));
        }

        redirects_followed += 1;
        // ~keep `CrawlConfig.max_redirects` is the single effective redirect-hop bound for
        // every GET, matching `follow_redirects` in `engine/crawl_loop.rs`. It is the only
        // one of the two redirect-count fields exposed by the builder (see
        // `types/builder.rs::max_redirects`); `SsrfPolicy.max_redirects` is kept only for
        // backward-compatible (de)serialization of the SSRF policy shape (it is part of the
        // fixtures/schema.json contract) and is not read at runtime -- SSRF safety itself is
        // unaffected because every redirect target is still validated by `validate_url` above.
        if redirects_followed > config.max_redirects {
            return Err(CrawlError::ssrf_violation(&next_url, "too many redirects"));
        }

        rules.insert(&next_url);
        current_url = next_url;
    }
}

/// Statuses whose `Location` header the crawl follows (`engine::redirect::http_redirect_target`
/// reads this same constant as `crate::http::REDIRECT_STATUSES`; it lives here because
/// `engine::redirect` is native-only and this module is not).
pub(crate) const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

pub(crate) fn reject_redirect(target: &url::Url, unseen: bool, hop_left: bool) -> Result<(), CrawlError> {
    if !unseen || !hop_left {
        let mut error = CrawlError::from(crate::net::ssrf::SsrfError::TooManyRedirects.with_url(target));
        if let CrawlError::SsrfPolicyViolation { reason, .. } = &mut error {
            *reason = if !unseen {
                "redirect loop"
            } else {
                "redirect limit exceeded"
            }
            .to_owned();
        }
        return Err(error);
    }
    Ok(())
}

/// The crawl's chain rules, applied only when a fetch follows refreshes, with the URLs the fetch
/// has requested.
///
/// ~keep A parsed URL's serialization is already the crawl's cycle key for it
/// ~keep (`engine/redirect.rs`'s `canonical_redirect_key` re-parses and re-serializes), so the
/// ~keep serialization is stored and compared directly.
struct ChainRules(Option<std::collections::HashSet<String>>);

impl ChainRules {
    fn new(refresh: RefreshRedirects, initial_url: &url::Url) -> Self {
        let mut visited = Self((refresh == RefreshRedirects::Follow).then(std::collections::HashSet::new));
        visited.insert(initial_url);
        visited
    }

    fn insert(&mut self, url: &url::Url) {
        if let Some(seen) = self.0.as_mut() {
            seen.insert(url.as_str().to_owned());
        }
    }

    /// Whether the fetch goes on to the `Location` target `target` of a hop that answered with
    /// `status`, given whether a hop is left. `status` is checked against the crawl's own
    /// `REDIRECT_STATUSES` so a 300, 304 or 305 naming a `Location` stays unfollowed here too.
    fn follows_location(&self, status: u16, target: &url::Url, hop_left: bool) -> Result<bool, CrawlError> {
        if let Some(seen) = self.0.as_ref() {
            if !REDIRECT_STATUSES.contains(&status) {
                return Ok(false);
            }
            reject_redirect(target, !seen.contains(target.as_str()), hop_left)?;
        }
        Ok(true)
    }

    /// Whether `error`, raised past the first hop, ends the chain on a response instead.
    fn stops_on(&self, error: &CrawlError) -> bool {
        self.0.is_some() && matches!(error, CrawlError::NotFound { .. })
    }

    /// The unvisited URL a refresh in `response` names, read by the crawl's own redirect sources.
    /// The meta refresh check leaves its read of the body in `page_scan`.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_target(
        &self,
        response: &HttpResponse,
        current_url: &url::Url,
        page_scan: &mut Option<PageScan>,
    ) -> Option<url::Url> {
        let seen = self.0.as_ref()?;
        crate::engine::redirect::refresh_redirect_target(
            response,
            current_url.as_str(),
            |target| (!seen.contains(target)).then(|| target.to_owned()),
            page_scan,
        )
        .map(|(target, _)| target)
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_target(
        &self,
        _response: &HttpResponse,
        _current_url: &url::Url,
        _page_scan: &mut Option<PageScan>,
    ) -> Option<url::Url> {
        None
    }
}

/// Fetch `current_url` once, without following any redirect it returns.
///
/// A 3xx names its `Location` as the next hop when `follows_location` accepts it; otherwise the
/// 3xx is the response.
async fn fetch_one_hop(
    context: &FetchContext<'_>,
    current_url: &url::Url,
    follows_location: impl Fn(u16, &url::Url) -> Result<bool, CrawlError>,
) -> Result<HopOutcome, CrawlError> {
    let resp = send_hop_request(context, current_url).await?;
    body::validate_content_encoding(resp.headers())?;
    let head = ResponseHead::from_response(&resp);

    if (300..400).contains(&head.status)
        && let Some(target) = redirect_target(current_url, &head.headers)
    {
        if let RedirectTarget::Follow(next_url) = target
            && follows_location(head.status, &next_url)?
        {
            return Ok(HopOutcome::Redirect(next_url));
        }
        return Ok(HopOutcome::Complete(
            unfollowed_redirect_response(context.config, resp, head).await,
        ));
    }

    // ~keep Computed lazily and cached below rather than unconditionally up front: most
    // non-2xx statuses (404, 500, 502, ...) return before ever needing a header map, so
    // building one here would add an allocation to paths that previously had none. The
    // challenge statuses below are the exception and always need it.
    let mut headers_map_cache: Option<HashMap<String, Vec<String>>> = None;

    // ~keep Fingerprinting has to precede `status_error`: once a 429/503 has become a
    // `RateLimited`/`ServerError` the retry policy answers it with `Retry`, and a challenge
    // is then re-requested by the client that provoked it instead of escalating to the
    // browser tier (crawlberg#169).
    if is_challenge_status(head.status) {
        let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
        return Err(challenge_status_error(
            head.status,
            current_url.as_str(),
            headers_map,
            resp,
            effective_max_body_size(context.config),
        )
        .await);
    }
    if let Some(error) = status_error(head.status, current_url.as_str()) {
        return Err(error);
    }

    let expected_len = head.content_length();
    let body_bytes = read_validated_body(context.config, resp, expected_len).await?;
    let body = String::from_utf8_lossy(&body_bytes).into_owned();

    let headers_map = headers_map_cache.unwrap_or_else(|| build_headers_map(&head.headers));

    // ~keep The TOML corpus is the single WAF source of truth; do not hardcode header lists here.
    // The body is read before the check rather than after a header match because a header-only
    // fingerprint is not on its own grounds to refuse a 2xx (crawlberg#231). The check decides
    // which statuses it applies to, the same decision the Tower fetch makes, so it runs on every
    // response this hop returns. A robots.txt gets its own decision, which does not read its
    // whole-line comments, and a sitemap gets one that reads a sitemap as a sitemap whatever it lists.
    let refusal = match context.fetched {
        Fetched::Page => waf::waf_2xx_error(head.status, &body_bytes, &body, &headers_map),
        Fetched::Sitemap => waf::sitemap_2xx_error(head.status, &body_bytes, &body, &headers_map),
        Fetched::RobotsTxt => waf::robots_2xx_error(head.status, &body_bytes, &body, &headers_map),
    };
    if let Some(error) = refusal {
        return Err(error);
    }
    Ok(HopOutcome::Complete(head.into_response(body, body_bytes, headers_map)))
}

/// Build and send the GET for one hop.
async fn send_hop_request(context: &FetchContext<'_>, current_url: &url::Url) -> Result<reqwest::Response, CrawlError> {
    crate::net::userinfo::refuse(current_url)?;
    let client = request_client(context.client, context.config, current_url)?;
    // ~keep WASM has no client-level timeout; apply the budget to every redirect hop.
    let mut req = client
        .get(current_url.to_string())
        .timeout(context.config.request_timeout);

    // ~keep `extra_headers["user-agent"]` outranks everything else: on wasm, where every
    // ~keep request (including the page fetch) goes through this function, it is the crawl
    // ~keep loop's own per-page rotation pick (crawlberg#483). Falls back to
    // ~keep `custom_headers["user-agent"]` ahead of `config.user_agent`, the same precedence
    // ~keep every other sender uses (crawlberg#423); this hop's own robots.txt, sitemap and
    // ~keep asset fetches used to read `config.user_agent` only, so a custom-header agent
    // ~keep never reached them. Native's own Tower-routed page fetch never reaches this
    // ~keep function at all -- it sets its `User-Agent` header in `tower/service.rs` instead.
    let extra_user_agent = context
        .extra_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.as_str());
    req = req.header(
        USER_AGENT,
        extra_user_agent.unwrap_or_else(|| crate::helpers::default_robots_user_agent(context.config)),
    );

    // ~keep Redirects are followed manually under `Policy::none()`, so reqwest's own
    // ~keep strip-credentials-on-cross-host behaviour never runs; asking per hop for the
    // ~keep seed-host headers replaces it, for the custom headers as well as the credential.
    // ~keep A `user-agent` custom header is already reflected in the line above; re-adding it
    // ~keep here would append a second, redundant `User-Agent` header line rather than
    // ~keep replacing the first one, the same bug `tower/service.rs::apply_headers` had
    // ~keep before it was fixed (crawlberg#423).
    for (name, value) in seed_host_headers(context.config, current_url) {
        if name.eq_ignore_ascii_case("user-agent") {
            continue;
        }
        req = req.header(name.as_str(), value.as_str());
    }

    // ~keep `user-agent` is already reflected in the line above (as `extra_user_agent`);
    // ~keep re-adding it here would append a second, redundant header line instead of
    // ~keep replacing the first one -- the same duplicate-header bug the loop above already
    // ~keep guards against for `seed_host_headers` (crawlberg#423, crawlberg#483).
    for (k, v) in context.extra_headers {
        if k.eq_ignore_ascii_case("user-agent") {
            continue;
        }
        req = req.header(k.as_str(), v.as_str());
    }

    req.send().await.map_err(classify_reqwest_error)
}

/// Resolve a 3xx response's `Location` header against the URL that served it.
fn redirect_target(current_url: &url::Url, headers: &HeaderMap) -> Option<RedirectTarget> {
    let location = headers
        .get(reqwest::header::LOCATION)
        .map(headers::decode_header_value)?;

    Some(match crate::net::userinfo::resolve(current_url, &location) {
        Some(next_url) if is_fetchable_scheme(&next_url) => RedirectTarget::Follow(next_url),
        _ => RedirectTarget::Unfollowable,
    })
}

/// Return a 3xx whose `Location` is not followed as the response itself: it names no URL the
/// crawler can fetch, or the chain's rules stop before it.
async fn unfollowed_redirect_response(
    config: &CrawlConfig,
    resp: reqwest::Response,
    head: ResponseHead,
) -> HttpResponse {
    let (body_bytes, _) = read_body_bounded(resp, effective_max_body_size(config))
        .await
        .unwrap_or_default();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    let headers_map = build_headers_map(&head.headers);
    head.into_response(body, body_bytes, headers_map)
}

/// Apply HTTP mode's status handling to a page a browser rendered. A status the HTTP fetch
/// raises as an error raises the same error here. Where HTTP mode reports the status as a
/// page instead (a 404 or 403 under `soft_http_errors`, or a 404 at the end of a redirect),
/// the page keeps its status and loses its body, as the HTTP fetch reports it.
#[cfg(any(feature = "browser", feature = "browser-native"))]
pub(crate) fn rendered_status_outcome(
    mut response: HttpResponse,
    redirected: bool,
    config: &CrawlConfig,
) -> Result<HttpResponse, CrawlError> {
    let status = response.status;
    // ~keep A challenge status is fingerprinted before `status_error` maps it, as in HTTP mode:
    // ~keep a 429 or 503 WAF challenge is a WAF block, which escalates, not an error the retry
    // ~keep policy retries (crawlberg#169).
    let error = if challenge::is_challenge_status(status) {
        Some(challenge::challenge_body_error(
            status,
            &response.final_url,
            &response.body,
            &response.headers,
        ))
    } else {
        status_error(status, &response.final_url)
    };
    let Some(error) = error else {
        return Ok(response);
    };
    let reported_as_page = match status {
        404 => config.soft_http_errors || redirected,
        403 => config.soft_http_errors,
        _ => false,
    };
    if !reported_as_page {
        return Err(error);
    }
    response.content_type.clear();
    response.body.clear();
    response.body_bytes.clear();
    Ok(response)
}

/// Shortfall below the declared `content-length` that is read as a truncated transfer
/// rather than as ordinary framing slack.
const BODY_SHORTFALL_TOLERANCE_BYTES: usize = 100;

fn content_length_shortfall_error(expected: Option<usize>, actual: usize, hit_cap: bool) -> Option<CrawlError> {
    // ~keep A capped read stopping short of `content-length` is expected (that is the
    // point of `max_body_size`), not evidence of a truncated/failed transfer.
    if hit_cap {
        return None;
    }
    let expected = expected?;
    if actual < expected && expected - actual > BODY_SHORTFALL_TOLERANCE_BYTES {
        return Some(CrawlError::data_loss(format!(
            "expected {expected} bytes, got {actual}"
        )));
    }
    None
}

/// Read the response body under the configured cap and reject a short transfer.
async fn read_validated_body(
    config: &CrawlConfig,
    resp: reqwest::Response,
    expected_len: Option<usize>,
) -> Result<Vec<u8>, CrawlError> {
    let response_url = resp.url().clone();
    let (body_bytes, hit_cap) = read_body_bounded(resp, effective_max_body_size(config))
        .await
        .map_err(|error| classify_body_read_error(error.with_url(response_url)))?;

    if let Some(error) = content_length_shortfall_error(expected_len, body_bytes.len(), hit_cap) {
        return Err(error);
    }

    Ok(body_bytes)
}

/// Tell a truncated or failed body transfer apart from any other reqwest failure.
fn classify_body_read_error(e: reqwest::Error) -> CrawlError {
    if e.is_timeout() {
        return classify_reqwest_error(e);
    }
    let chain = error_chain_string(&e);
    let is_body_error = chain.contains("content-length")
        || chain.contains("truncate")
        || chain.contains("incomplete")
        || chain.contains("end of file")
        || chain.contains("body error")
        || chain.contains("body from connection")
        || chain.contains("decoding response body")
        || chain.contains("error decoding");
    #[cfg(not(target_arch = "wasm32"))]
    let is_body_error = is_body_error || e.is_body();

    if is_body_error {
        CrawlError::data_loss_with_source(e.to_string(), e)
    } else {
        classify_reqwest_error(e)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::net::ssrf::{HostMatcher, SsrfPolicy};
    use rustls::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::net::SocketAddr;
    use std::sync::{Arc, Once};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn truncated_body_reqwest_error() -> reqwest::Error {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener must bind");
        let address = listener.local_addr().expect("listener must have an address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 500000\r\n\r\ntruncated")
                    .await;
                let _ = socket.flush().await;
            }
        });

        reqwest::Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("response headers must arrive")
            .bytes()
            .await
            .expect_err("the truncated body must fail")
    }

    #[tokio::test]
    async fn a_plain_http_body_error_renders_one_prefix_and_keeps_its_source() {
        let raw_error = truncated_body_reqwest_error().await;
        let raw_message = raw_error.to_string();
        let error = classify_body_read_error(raw_error);

        assert_eq!(error.to_string(), format!("data_loss: {raw_message}"));

        use std::error::Error as _;
        let source = error.source().expect("data loss must expose its source");
        let original = source.source().expect("the source wrapper must expose reqwest's error");
        assert!(original.downcast_ref::<reqwest::Error>().is_some());
    }

    #[test]
    fn a_capped_or_unmeasured_plain_http_body_is_not_data_loss() {
        assert!(content_length_shortfall_error(Some(1000), 10, true).is_none());
        assert!(content_length_shortfall_error(None, 10, false).is_none());
    }

    #[test]
    fn a_plain_http_shortfall_within_tolerance_is_not_data_loss() {
        assert!(
            content_length_shortfall_error(Some(1000), 1000 - BODY_SHORTFALL_TOLERANCE_BYTES, false,).is_none(),
            "a shortfall of exactly the tolerance is accepted"
        );
        assert!(content_length_shortfall_error(Some(1000), 1000, false).is_none());
        assert!(
            content_length_shortfall_error(Some(1000), 1200, false).is_none(),
            "a body longer than content-length is not data loss"
        );
    }

    #[test]
    fn a_plain_http_shortfall_past_tolerance_renders_one_prefix_without_a_source() {
        let error = content_length_shortfall_error(Some(1000), 899, false).expect("data loss expected");

        assert_eq!(error.to_string(), "data_loss: expected 1000 bytes, got 899");

        use std::error::Error as _;
        assert!(error.source().is_none());
    }

    #[tokio::test]
    async fn http_fetch_enforces_config_timeout_without_client_default() {
        const REQUEST_TIMEOUT: Duration = Duration::from_millis(50);
        const RESPONSE_DELAY: Duration = Duration::from_millis(500);
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/slow"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(RESPONSE_DELAY)
                    .set_body_string("late"),
            )
            .mount(&mock)
            .await;
        let config = CrawlConfig {
            request_timeout: REQUEST_TIMEOUT,
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        // ~keep WASM has no client-level timeout; a bare native client reproduces that condition.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client must build");
        for endpoint in ["/slow", "/start"] {
            let result = http_fetch(&format!("{}{endpoint}", mock.uri()), &config, &HashMap::new(), &client).await;
            assert!(
                matches!(result, Err(CrawlError::Timeout { .. })),
                "expected timeout for {endpoint}"
            );
        }
    }

    /// `http_fetch` must populate `final_url` from the reqwest response URL.
    ///
    /// On native targets (Policy::none) this equals the request URL because
    /// redirects are not followed transparently.  The test verifies the field
    /// is set to a non-empty value matching the requested URL — confirming
    /// the plumbing that the wasm path relies on to capture the post-redirect
    /// URL is in place.
    ///
    /// Note: the wasm-specific transparent-redirect behaviour (browser `fetch`
    /// following 3xx and returning the final URL via `response.url()`) cannot
    /// be exercised under `cargo test` because it requires a wasm32 target and
    /// a real browser runtime.  The build-time check (`cargo build --target
    /// wasm32-unknown-unknown`) verifies the changed code path compiles
    /// correctly for wasm.
    #[tokio::test]
    async fn http_fetch_populates_final_url() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Hello</body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;

        let url = format!("{}/page", mock.uri());
        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client)
            .await
            .expect("http_fetch must succeed");

        assert!(
            !resp.final_url.is_empty(),
            "final_url must not be empty after a successful fetch"
        );
        assert!(
            resp.final_url.contains("/page"),
            "final_url must contain the requested path, got: {}",
            resp.final_url
        );
    }

    /// A 3xx whose `Location` has a scheme the crawler cannot fetch is the response, not an SSRF
    /// error, while a web `Location` on the same server is still followed.
    #[tokio::test]
    async fn http_fetch_returns_the_3xx_when_location_names_a_scheme_it_cannot_fetch() {
        let mock = MockServer::start().await;
        let locations = [
            "mailto:a@example.com",
            "data:,x",
            "file:///etc/passwd",
            "myapp://open",
            "/final",
        ];
        for (index, location) in locations.iter().enumerate() {
            Mock::given(method("GET"))
                .and(path(format!("/start{index}")))
                .respond_with(ResponseTemplate::new(302).append_header("location", *location))
                .mount(&mock)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(ResponseTemplate::new(200).set_body_string("final"))
            .mount(&mock)
            .await;
        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");

        for (index, location) in locations.iter().enumerate() {
            let url = format!("{}/start{index}", mock.uri());
            let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client)
                .await
                .unwrap_or_else(|e| panic!("{location}: http_fetch must not fail: {e}"));
            let expected = if *location == "/final" { 200 } else { 302 };
            assert_eq!(resp.status, expected, "{location}");
        }
    }

    /// Regression test: `http_fetch`'s internal redirect loop used to enforce
    /// `config.ssrf.max_redirects` (a `u8` with no public builder setter, default 5)
    /// instead of the builder-settable `config.max_redirects`, so `.max_redirects(N)`
    /// had no effect on this loop. This proves the builder value now bounds it.
    #[tokio::test]
    async fn http_fetch_stops_once_builder_max_redirects_is_exceeded() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hop0"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop1"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop2"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop2"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        config.max_redirects = 1;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/hop0", mock.uri());
        let result = http_fetch(&url, &config, &std::collections::HashMap::new(), &client).await;

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("must stop once the second hop exceeds max_redirects(1), got Ok"),
        };
        assert!(
            matches!(err, CrawlError::SsrfPolicyViolation { ref reason, .. } if reason == "too many redirects"),
            "expected a too-many-redirects SsrfPolicyViolation, got {err:?}"
        );
    }

    /// ~keep Regression: a refused `http://user:pass@host/` once leaked the credential into
    /// API error bodies, MCP payloads and tracing fields. The serial lock covers the unavoidable
    /// environment read in `CrawlConfig::default`; the explicit policy keeps that value from
    /// deciding whether the metadata address is admitted.
    #[tokio::test]
    #[serial_test::serial]
    async fn http_fetch_ssrf_rejection_does_not_leak_url_credentials() {
        let config = CrawlConfig {
            ssrf: SsrfPolicy::default(),
            ssrf_deny_private_explicit: Some(true),
            ..CrawlConfig::default()
        };
        let engine = crate::CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build");
        let url = "http://alice:hunter2@169.254.169.254/latest/meta-data/";

        let err = match engine.scrape(url).await {
            Err(e) => e,
            Ok(_) => panic!("the link-local metadata address must be refused by the pinned policy"),
        };

        let CrawlError::SsrfPolicyViolation {
            url: refused_url,
            reason,
            ..
        } = &err
        else {
            panic!("the metadata address must be refused before any request, got {err:?}");
        };
        assert_eq!(
            refused_url, "http://169.254.169.254/latest/meta-data/",
            "admission must remove userinfo before the SSRF policy names the metadata URL"
        );
        assert_eq!(
            reason, "denied by SSRF policy: link_local",
            "the metadata address must be refused as link-local before any request"
        );

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the refused URL's password must never reach the error, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the refused URL's username must never reach the error, got {rendered}"
        );
        assert!(
            rendered.contains("169.254.169.254"),
            "the host must survive redaction so the error stays actionable, got {rendered}"
        );
    }

    async fn http_fetch_refusal(url: &str) -> CrawlError {
        let config = CrawlConfig::default();
        let client = build_client(&config).expect("client must build");
        match http_fetch(url, &config, &std::collections::HashMap::new(), &client).await {
            Err(err @ CrawlError::SsrfPolicyViolation { .. }) => err,
            Err(other) => panic!("{url} must be refused by the SSRF policy, got {other:?}"),
            Ok(_) => panic!("{url} must be refused by the SSRF policy, got Ok"),
        }
    }

    async fn http_fetch_refusal_reason(url: &str) -> String {
        match http_fetch_refusal(url).await {
            CrawlError::SsrfPolicyViolation { reason, .. } => reason,
            other => unreachable!("http_fetch_refusal returns only SsrfPolicyViolation, got {other:?}"),
        }
    }

    /// ~keep The refusal's `url` field carries the address as written, so the secret must be
    /// absent from the whole rendered error, not only from `reason`. Each row is one class of
    /// credential-bearing address the refusal sees; the expected `url` field is the positive
    /// twin that proves the row reached the SSRF refusal at all.
    #[tokio::test]
    async fn http_fetch_ssrf_refusal_hides_the_credential_in_the_whole_error() {
        const HIDDEN: &str = "[address hidden: it may carry credentials]";
        let unrecognized = "disallowed scheme: unrecognized";
        let rows = [
            // Opaque: parses as scheme `user` with no host.
            ("user:token@host", &["token"][..], HIDDEN, unrecognized),
            ("KEY:@h:1", &["key"][..], HIDDEN, unrecognized),
            // Real userinfo under a scheme the policy does not recognise.
            (
                "foo://alice:hunter2@example.com/",
                &["alice", "hunter2"][..],
                "foo://***:***@example.com/",
                unrecognized,
            ),
            // No scheme at all: the address fails to parse.
            (
                "alice@example.com",
                &["alice"][..],
                HIDDEN,
                "invalid URL: relative URL without a base",
            ),
            // A percent-encoded `@` inside the password.
            ("user:hunt%40er2@host", &["hunt", "er2"][..], HIDDEN, unrecognized),
            (
                "foo://alice:hunt%40er2@example.com/",
                &["alice", "hunt", "er2"][..],
                "foo://***:***@example.com/",
                unrecognized,
            ),
        ];
        let mut failures = Vec::new();
        for (url, secrets, expected_url, expected_reason) in rows {
            let err = http_fetch_refusal(url).await;
            let rendered = format!("{err}\n{err:?}");
            let lowered = rendered.to_lowercase();
            let shown: Vec<&str> = secrets.iter().copied().filter(|s| lowered.contains(s)).collect();
            if !shown.is_empty() {
                failures.push(format!("{url}: shows {shown:?} in {rendered}"));
            }
            let CrawlError::SsrfPolicyViolation { url: field, reason, .. } = &err else {
                unreachable!("http_fetch_refusal returns only SsrfPolicyViolation, got {err:?}");
            };
            if field != expected_url || reason != expected_reason {
                failures.push(format!(
                    "{url}: expected url {expected_url:?} and reason {expected_reason:?}, got {field:?} and {reason:?}"
                ));
            }
        }
        assert!(failures.is_empty(), "credential rows failed:\n{}", failures.join("\n"));
    }

    #[tokio::test]
    async fn http_fetch_scheme_refusal_does_not_show_a_user_name_parsed_as_the_scheme() {
        for (url, parsed_scheme, secret) in [
            ("user:token@host", "user", "token"),
            ("KEY:@h:1", "key", "key"),
            ("localhost:3128", "localhost", "3128"),
        ] {
            let reason = http_fetch_refusal_reason(url).await;
            assert!(
                reason.contains("disallowed scheme"),
                "{url} must be refused for its scheme, got: {reason}"
            );
            let lowered = reason.to_lowercase();
            for shown in [parsed_scheme, secret] {
                assert!(
                    !lowered.contains(shown),
                    "the refusal of {url} shows {shown:?}: {reason}"
                );
            }
        }
    }

    #[tokio::test]
    async fn http_fetch_scheme_refusal_names_a_known_scheme() {
        for (url, named) in [
            ("ftp://x", "disallowed scheme: ftp"),
            ("file:///x", "disallowed scheme: file"),
        ] {
            let reason = http_fetch_refusal_reason(url).await;
            assert_eq!(reason, named, "the refusal of {url} must name its scheme");
        }
    }

    /// ~keep Regression coverage for #442: the plain fetch's `send_hop_request` is the one
    /// call site robots.txt (`helpers.rs`), sitemaps (`sitemap.rs`) and asset downloads
    /// (`assets.rs`) all fetch through, and it shares `classify_reqwest_error` with the
    /// page-fetch path `test_transport_error_credential_redaction.rs` already covers. That
    /// test never reaches this call site (page fetches go through `tower/service.rs`
    /// instead), so a change that reintroduced the raw URL here specifically would still
    /// pass every existing test. This drives `http_fetch` directly against a closed port.
    #[tokio::test]
    async fn http_fetch_transport_error_does_not_leak_url_credentials() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("must bind an ephemeral port");
        let port = listener.local_addr().expect("must read local addr").port();
        drop(listener);

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("http://alice:hunter2@127.0.0.1:{port}/robots.txt");

        let err = match http_fetch(&url, &config, &std::collections::HashMap::new(), &client).await {
            Err(e) => e,
            Ok(_) => panic!("a connection to a closed port must fail"),
        };

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the closed-port transport error must never carry the URL's password, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the closed-port transport error must never carry the URL's username, got {rendered}"
        );
        assert!(
            rendered.contains("127.0.0.1"),
            "the host must still be named so the error stays actionable, got {rendered}"
        );
    }

    /// ~keep Regression coverage for #463: `send_hop_request` only had connection-refused
    /// credential-redaction coverage (#442, the test above); the page-fetch path
    /// (`do_fetch`, `test_transport_error_shapes_credential_redaction.rs`) also has DNS,
    /// timeout and TLS-certificate shapes for #444. These three tests give the robots,
    /// sitemap and asset path (this call site) the same three shapes, so a change that
    /// reintroduced the raw URL in exactly one of them would not slip past unnoticed on
    /// this path the way it already couldn't on the page-fetch path.
    #[tokio::test]
    async fn http_fetch_transport_error_does_not_leak_url_credentials_from_a_dns_failure() {
        let host = "this-hostname-does-not-exist-crawlberg-test.invalid";
        let mut config = CrawlConfig::default();
        config.ssrf.allowlist.push(HostMatcher::exact(host));
        let client = build_client(&config).expect("client must build");
        let url = format!("http://alice:hunter2@{host}/robots.txt");

        let err = match http_fetch(&url, &config, &HashMap::new(), &client).await {
            Err(e) => e,
            Ok(_) => panic!("an unresolvable host must fail"),
        };

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the DNS-failure transport error must never carry the URL's password, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the DNS-failure transport error must never carry the URL's username, got {rendered}"
        );
        assert!(
            rendered.contains(host),
            "the host must still be named so the error stays actionable, got {rendered}"
        );
    }

    #[tokio::test]
    async fn http_fetch_transport_error_does_not_leak_url_credentials_from_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((_socket, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        config.request_timeout = Duration::from_millis(200);
        let client = build_client(&config).expect("client must build");
        let url = format!("http://alice:hunter2@{addr}/robots.txt");

        let err = match http_fetch(&url, &config, &HashMap::new(), &client).await {
            Err(e) => e,
            Ok(_) => panic!("a server that never answers must time out"),
        };

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the timeout transport error must never carry the URL's password, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the timeout transport error must never carry the URL's username, got {rendered}"
        );
        assert!(
            rendered.contains(&addr.ip().to_string()),
            "the host must still be named so the error stays actionable, got {rendered}"
        );
    }

    /// A throwaway self-signed certificate for `127.0.0.1`, the same static DER fixture
    /// `test_transport_error_shapes_credential_redaction.rs` uses for the page-fetch path's
    /// TLS-certificate test: no external process, no cross-OpenSSL-version drift between
    /// CI's Linux and macOS legs. Unit tests here and that integration test are separate
    /// compilation units, so the spawn helper is duplicated rather than shared, matching
    /// this crate's existing per-file test-helper convention (`build_engine`).
    static CERT_DER: &[u8] = include_bytes!("../tests/fixtures/self_signed/cert.der");
    static KEY_DER: &[u8] = include_bytes!("../tests/fixtures/self_signed/key.der");

    fn install_crypto_provider() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    async fn spawn_self_signed_tls_server() -> SocketAddr {
        install_crypto_provider();

        let cert = CertificateDer::from(CERT_DER.to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec()));
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("the self-signed server config must build");
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("must bind an ephemeral port");
        let addr = listener.local_addr().expect("must read local addr");

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = acceptor.accept(stream).await;
                });
            }
        });

        addr
    }

    #[tokio::test]
    async fn http_fetch_transport_error_does_not_leak_url_credentials_from_a_bad_certificate() {
        let addr = spawn_self_signed_tls_server().await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("https://alice:hunter2@{addr}/robots.txt");

        let err = match http_fetch(&url, &config, &HashMap::new(), &client).await {
            Err(e) => e,
            Ok(_) => panic!("an untrusted self-signed certificate must fail verification"),
        };

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the bad-certificate transport error must never carry the URL's password, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the bad-certificate transport error must never carry the URL's username, got {rendered}"
        );
        assert!(
            rendered.contains(&addr.ip().to_string()),
            "the host must still be named so the error stays actionable, got {rendered}"
        );
    }

    /// Same redirect chain as above, but with a builder value large enough to reach the
    /// end — proving `.max_redirects(N)` is actually honored (not just enforced too
    /// tightly) by the same loop.
    #[tokio::test]
    async fn http_fetch_follows_full_chain_when_builder_max_redirects_is_sufficient() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hop0"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        config.max_redirects = 1;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/hop0", mock.uri());
        let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client)
            .await
            .expect("one redirect hop must be followed when max_redirects == 1");

        assert_eq!(
            resp.body, "done",
            "final response body must be the hop1 body, got {:?}",
            resp.body
        );
        assert_eq!(resp.status, 200, "final status must be 200, got {}", resp.status);
    }
    /// Characterization for the status dispatch `fetch_one_hop` performs: every terminal
    /// status maps to one specific error carrying that status. ~keep
    ///
    /// ~keep 429 and 503 reach this table through `challenge::challenge_status_error`, which
    /// reads their body first; the bodyless responses below carry no fingerprint, so they fall
    /// through to the same `status_error` mapping as the rest.
    #[tokio::test]
    async fn http_fetch_maps_each_body_free_terminal_status_to_its_own_error() {
        /// (status, the variant it must raise, a fragment its message must carry).
        type StatusCase = (u16, fn(&CrawlError) -> bool, &'static str);

        let cases: &[StatusCase] = &[
            (401, |e| matches!(e, CrawlError::Unauthorized { .. }), "unauthorized"),
            (404, |e| matches!(e, CrawlError::NotFound { .. }), "not_found"),
            (408, |e| matches!(e, CrawlError::Timeout { .. }), "timeout"),
            (410, |e| matches!(e, CrawlError::Gone { .. }), "gone"),
            (429, |e| matches!(e, CrawlError::RateLimited { .. }), "rate_limited"),
            (500, |e| matches!(e, CrawlError::ServerError { .. }), "server_error"),
            (502, |e| matches!(e, CrawlError::BadGateway { .. }), "bad_gateway"),
            (
                503,
                |e| matches!(e, CrawlError::ServerError { .. }),
                "service unavailable",
            ),
            (504, |e| matches!(e, CrawlError::ServerError { .. }), "gateway timeout"),
        ];

        for (status, is_expected_variant, message_fragment) in cases {
            let error = fetch_status(*status, ResponseTemplate::new(*status)).await;
            assert!(
                is_expected_variant(&error),
                "status {status} produced the wrong error variant: {error:?}"
            );
            assert!(
                error.to_string().contains(message_fragment),
                "status {status} message must contain {message_fragment:?}, got: {error}"
            );
            assert_eq!(status::error_status(&error), Some(*status), "{error:?}");
        }
    }

    /// A custom retry policy decides on the status of the response that failed, so a plain 403
    /// and a fingerprinted block must both carry the status they were raised for (crawlberg#133).
    #[tokio::test]
    async fn http_fetch_carries_the_response_status_on_a_403_and_on_a_fingerprinted_block() {
        let plain = fetch_status(403, ResponseTemplate::new(403).set_body_string("nope")).await;
        assert_eq!(
            status::error_status(&plain),
            Some(403),
            "a plain 403 must carry its status: {plain:?}"
        );

        let fingerprinted = fetch_status(403, ResponseTemplate::new(403).set_body_string("cf-chl- challenge")).await;
        assert_eq!(
            status::error_status(&fingerprinted),
            Some(403),
            "a fingerprinted 403 must carry its status: {fingerprinted:?}"
        );

        for status in [429_u16, 503] {
            let blocked = fetch_status(
                status,
                ResponseTemplate::new(status)
                    .append_header("x-datadome", "blocked")
                    .set_body_string("<html>challenge</html>"),
            )
            .await;
            assert!(
                matches!(&blocked, CrawlError::WafBlocked { .. }),
                "status {status} must fingerprint as a block: {blocked:?}"
            );
            assert_eq!(
                status::error_status(&blocked),
                Some(status),
                "a block fingerprinted from a {status} must carry it: {blocked:?}"
            );
        }

        let refused = fetch_status(
            200,
            ResponseTemplate::new(200).set_body_string("<html>cf-chl- x</html>"),
        )
        .await;
        assert!(
            matches!(&refused, CrawlError::WafBlocked { .. }),
            "a 2xx interstitial must be refused as a block: {refused:?}"
        );
        assert_eq!(
            status::error_status(&refused),
            Some(200),
            "a block refused from a 2xx must carry its status: {refused:?}"
        );
    }

    /// A 403 page a browser rendered raises the error the HTTP fetch raises for it, with the
    /// same status attached, so a custom retry policy reads 403 whichever tier fetched the page.
    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[test]
    fn a_rendered_403_carries_its_status_as_the_http_fetch_does() {
        let rendered = |headers: &[(&str, &str)]| HttpResponse {
            status: 403,
            content_type: "text/html".to_owned(),
            body: "<p>nope</p>".to_owned(),
            body_bytes: b"<p>nope</p>".to_vec(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), vec![(*value).to_owned()]))
                .collect(),
            browser_extras: None,
            final_url: "https://example.com/page".to_owned(),
            screenshot: None,
        };
        let config = CrawlConfig::default();

        let plain = rendered_status_outcome(rendered(&[]), false, &config)
            .err()
            .expect("a rendered 403 must fail");
        assert!(matches!(&plain, CrawlError::Forbidden { .. }), "{plain:?}");
        assert_eq!(
            status::error_status(&plain),
            Some(403),
            "a plain rendered 403 must carry its status: {plain:?}"
        );

        let blocked = rendered_status_outcome(rendered(&[("x-datadome", "protected")]), false, &config)
            .err()
            .expect("a fingerprinted rendered 403 must fail");
        assert!(matches!(&blocked, CrawlError::WafBlocked { .. }), "{blocked:?}");
        assert_eq!(
            status::error_status(&blocked),
            Some(403),
            "a fingerprinted rendered 403 must carry its status: {blocked:?}"
        );
    }

    /// A 403 that carries no WAF fingerprint is a plain forbidden, not a WAF block.
    #[tokio::test]
    async fn http_fetch_reports_a_plain_403_as_forbidden() {
        let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("nope")).await;
        assert!(
            matches!(error, CrawlError::Forbidden { .. }),
            "expected Forbidden, got {error:?}"
        );
    }

    /// A 403 whose body fingerprints must name the vendor the corpus identifies. ~keep
    #[tokio::test]
    async fn http_fetch_reports_a_waf_block_when_a_403_body_fingerprints() {
        let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("cf-chl- challenge")).await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
            "expected a cloudflare WafBlocked, got {error:?}"
        );
        assert_eq!(error.to_string(), "forbidden: waf/blocked: detected: cloudflare");
    }

    /// A 503 stamped by a WAF vendor header is a challenge, not a server fault, so it must
    /// escalate rather than be retried blindly (crawlberg#169).
    #[tokio::test]
    async fn http_fetch_reports_a_waf_block_when_a_503_carries_a_vendor_header() {
        let error = fetch_status(
            503,
            ResponseTemplate::new(503)
                .append_header("x-datadome", "blocked")
                .set_body_string("<html>challenge</html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "datadome"),
            "expected a datadome WafBlocked, got {error:?}"
        );
        assert_eq!(error.to_string(), "forbidden: waf/blocked: detected on 503: datadome");
    }

    /// A 503 that only fingerprints once its body is read is still a challenge. This is the
    /// Cloudflare interstitial shape from crawlberg#169: `server: cloudflare` alone is not a
    /// block signal in the corpus, so the header check is inconclusive and the body decides.
    #[tokio::test]
    async fn http_fetch_reports_a_waf_block_when_a_503_body_fingerprints() {
        let error = fetch_status(
            503,
            ResponseTemplate::new(503)
                .append_header("server", "cloudflare")
                .set_body_string(
                    "<html><head><title>Just a moment...</title></head>\
                 <body><script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script>\
                 </body></html>",
                ),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
            "expected a cloudflare WafBlocked, got {error:?}"
        );
        assert_eq!(error.to_string(), "forbidden: waf/blocked: detected on 503: cloudflare");
        assert_eq!(
            status::error_status(&error),
            Some(503),
            "the response status must remain the source"
        );
    }

    /// A 429 challenge fingerprints exactly like a 503 one.
    #[tokio::test]
    async fn http_fetch_reports_a_waf_block_when_a_429_challenge_fingerprints() {
        let error = fetch_status(
            429,
            ResponseTemplate::new(429)
                .append_header("x-px-block", "1")
                .set_body_string("<html>px-captcha</html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "perimeterx"),
            "expected a perimeterx WafBlocked, got {error:?}"
        );
        assert_eq!(error.to_string(), "forbidden: waf/blocked: detected on 429: perimeterx");
    }

    /// A 503 or 429 carrying no WAF signal must come out of the new classification step
    /// untouched: the same variant, the same message, its status still attached, and still
    /// retryable for the `retry_codes` that list it (crawlberg#84).
    #[tokio::test]
    async fn http_fetch_keeps_a_challenge_status_without_a_waf_signal_retryable() {
        let cases: &[(u16, &str)] = &[(503, "service unavailable"), (429, "rate_limited")];
        for (status, message_fragment) in cases {
            let error = fetch_status(
                *status,
                ResponseTemplate::new(*status)
                    .append_header("content-type", "text/html")
                    .set_body_string("<html><body><h1>Service Unavailable</h1></body></html>"),
            )
            .await;
            assert!(
                matches!(&error, CrawlError::ServerError { .. } | CrawlError::RateLimited { .. }),
                "status {status} must stay an ordinary retryable error, got {error:?}"
            );
            assert!(
                error.to_string().contains(message_fragment),
                "status {status} message must contain {message_fragment:?}, got: {error}"
            );
            assert_eq!(
                status::error_status(&error),
                Some(*status),
                "status {status} must stay attached to its error: {error:?}"
            );
            assert!(
                should_retry_error(&error, &[*status]),
                "status {status} must still be retryable when retry_codes lists it: {error:?}"
            );
        }
    }

    /// A 2xx whose headers name the vendor is reported as a header block rather than treated
    /// as page content. ~keep
    ///
    /// ~keep Also the regression guard for crawlberg#169: this and the body-block test below
    /// pin the 2xx wording, which the challenge-status work must not reword. Both pass with
    /// and without that change, which is the point of a guard.
    ///
    /// ~keep The body carries DataDome's own script tag because a header-only fingerprint no
    /// longer decides a 2xx on its own (crawlberg#231). That narrowing reaches `x-datadome`,
    /// `x-px-block` and `x-amzn-waf-action` as well as the CDN-presence headers #231 is about:
    /// on a 2xx all four now need the interstitial to be visible in the body.
    #[tokio::test]
    async fn http_fetch_reports_a_header_waf_block_on_a_2xx() {
        let error = fetch_status(
            200,
            ResponseTemplate::new(200)
                .append_header("x-datadome", "protected")
                .set_body_string("<html><script src=\"https://js.datadome.co/tags.js\"></script></html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "datadome"),
            "expected a datadome WafBlocked, got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "forbidden: waf/blocked: detected on 2xx (header): datadome"
        );
    }

    /// A 2xx that only fingerprints once its body is read is reported as a body block.
    #[tokio::test]
    async fn http_fetch_reports_a_body_waf_block_on_a_2xx() {
        let error = fetch_status(
            200,
            ResponseTemplate::new(200).set_body_string("<html>cf-chl- x</html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
            "expected a cloudflare WafBlocked, got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "forbidden: waf/blocked: detected on 2xx (body): cloudflare"
        );
    }

    /// A 2xx whose only WAF evidence is a CDN-presence header is returned as content by
    /// `http_fetch`, the decision asset fetches get and sitemap fetches fall back to
    /// (crawlberg#231).
    #[tokio::test]
    async fn http_fetch_returns_a_2xx_with_only_a_cdn_presence_header_as_content() {
        for (name, value) in [("server", "AkamaiGHost"), ("x-sucuri-id", "18012")] {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/probe"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .append_header(name, value)
                        .set_body_string("<html><body><h1>Release notes</h1></body></html>"),
                )
                .mount(&mock)
                .await;

            let config = permissive_config();
            let client = build_client(&config).expect("client must build");
            let response = http_fetch(&format!("{}/probe", mock.uri()), &config, &HashMap::new(), &client)
                .await
                .unwrap_or_else(|error| panic!("a 200 with only `{name}: {value}` must succeed, got {error:?}"));
            assert!(
                response.body.contains("Release notes"),
                "the real page must reach the caller, got: {}",
                response.body
            );
        }
    }

    /// A 3xx whose `Location` does not resolve to a URL is returned as the response
    /// rather than followed or rejected. ~keep
    #[tokio::test]
    async fn http_fetch_returns_a_3xx_with_an_unresolvable_location_as_the_response() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/here"))
            .respond_with(
                ResponseTemplate::new(302)
                    .append_header("location", "http://")
                    .set_body_string("moved"),
            )
            .mount(&mock)
            .await;

        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        let response = http_fetch(&format!("{}/here", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .expect("an unresolvable Location must not fail the fetch");

        assert_eq!(response.status, 302, "the 3xx itself must be returned");
        assert_eq!(response.body, "moved", "its body must be read");
    }

    #[tokio::test]
    async fn a_followed_chain_stops_on_a_404_past_the_first_hop_as_the_crawl_does() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/here"))
            .respond_with(ResponseTemplate::new(301).append_header("location", "/missing"))
            .mount(&mock)
            .await;
        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        let fetch = |route: &str, refresh| {
            let url = format!("{}{route}", mock.uri());
            let (config, client) = (&config, &client);
            async move {
                http_fetch_with(&url, config, &HashMap::new(), client, refresh)
                    .await
                    .map(|page| page.response)
            }
        };

        let response = fetch("/here", RefreshRedirects::Follow)
            .await
            .expect("a 404 past the first hop must not fail a followed chain");
        assert_eq!(response.status, 404);
        assert_eq!(response.final_url, format!("{}/missing", mock.uri()));
        assert!(response.body.is_empty(), "the crawl's chain reports an empty 404");

        assert!(
            matches!(
                fetch("/missing", RefreshRedirects::Follow).await,
                Err(CrawlError::NotFound { .. })
            ),
            "a 404 on the first hop still fails, as in the crawl"
        );
        assert!(
            matches!(
                fetch("/here", RefreshRedirects::Ignore).await,
                Err(CrawlError::NotFound { .. })
            ),
            "a plain fetch keeps failing on a 404 anywhere in the chain"
        );
    }

    #[tokio::test]
    async fn a_url_with_userinfo_is_refused_before_the_network() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock)
            .await;
        let config = permissive_config();
        let client = build_client(&config).expect("client must build");

        let credentialed = mock.uri().replacen("http://", "http://user:FETCH-PW-4d1e@", 1) + "/in";
        let error = http_fetch(&credentialed, &config, &HashMap::new(), &client)
            .await
            .map(|_| ())
            .expect_err("a URL with userinfo must be refused");
        let text = error.to_string();
        assert!(
            !text.contains("FETCH-PW-4d1e"),
            "the error must not print the password: {text}"
        );
        assert!(text.contains("credentials"), "the error names the refusal: {text}");

        http_fetch(&format!("{}/out", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .expect("the same URL without userinfo must be fetched");
        let paths: Vec<String> = mock
            .received_requests()
            .await
            .expect("request recording is on")
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(paths, ["/out"], "only the URL without userinfo may reach the network");
    }

    #[tokio::test]
    async fn a_body_deadline_should_report_timeout_and_address() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let url = format!("http://{}/slow-body", listener.local_addr().expect("address"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request");
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.expect("read request") > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nstart")
                .await
                .expect("headers");
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let mut config = permissive_config();
        config.request_timeout = Duration::from_millis(100);
        let client = build_client(&config).expect("client");
        let error = http_fetch(&url, &config, &HashMap::new(), &client)
            .await
            .err()
            .expect("body deadline");
        assert!(matches!(error, CrawlError::Timeout { .. }), "{error}");
        assert!(error.to_string().contains(&url), "{error}");
        server.abort();
    }

    #[tokio::test]
    async fn deflate_should_decode_and_unknown_content_encoding_should_fail() {
        use std::io::Write as _;
        let mock = MockServer::start().await;
        let html = b"<p>page body words</p>";
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(html).expect("compression");
        let body = encoder.finish().expect("compressed body");
        for encoding in ["deflate", "unknown"] {
            Mock::given(path(format!("/{encoding}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Encoding", encoding)
                        .set_body_bytes(body.clone()),
                )
                .mount(&mock)
                .await;
        }
        let config = permissive_config();
        let client = build_client(&config).expect("client");
        let response = http_fetch(&format!("{}/deflate", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .expect("deflate decoded");
        assert_eq!(response.body_bytes, html);
        let error = http_fetch(&format!("{}/unknown", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .err()
            .expect("unsupported encoding");
        assert!(error.to_string().contains("unknown"), "{error}");
    }

    fn permissive_config() -> CrawlConfig {
        CrawlConfig {
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        }
    }

    /// Fetch a single mocked response and return the error it produced.
    async fn fetch_status(status: u16, template: ResponseTemplate) -> CrawlError {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(template)
            .mount(&mock)
            .await;

        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        http_fetch(&format!("{}/probe", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .map(|_| ())
            .expect_err(&format!("status {status} must produce an error"))
    }

    #[test]
    fn a_redirect_location_loses_its_userinfo() {
        let current = url::Url::parse("http://example.com/start").expect("test URL must parse");
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            reqwest::header::HeaderValue::from_static("http://user:s3cret@example.com/end"),
        );
        let Some(RedirectTarget::Follow(next)) = redirect_target(&current, &headers) else {
            panic!("the Location must be followed");
        };
        assert_eq!(next.as_str(), "http://example.com/end");
    }

    #[test]
    fn a_redirect_location_with_an_obs_text_byte_is_followed() {
        let current = url::Url::parse("http://example.com/start").expect("test URL must parse");
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            reqwest::header::HeaderValue::from_bytes(b"/caf\xe9").expect("an obs-text Location must be valid"),
        );

        let Some(RedirectTarget::Follow(next)) = redirect_target(&current, &headers) else {
            panic!("the non-ASCII Location must be followed");
        };

        assert_eq!(next.as_str(), "http://example.com/caf%C3%A9");
    }

    async fn read_request_path(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 512];
            let read = socket.read(&mut chunk).await.expect("request must be readable");
            assert_ne!(read, 0, "request headers ended before their terminator");
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&request)
            .lines()
            .next()
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .expect("request line must name a path")
            .to_owned()
    }

    async fn serve_obs_text_redirect_chain(listener: TcpListener) -> Vec<String> {
        let responses: [&[u8]; 3] = [
            b"HTTP/1.1 302 Found\r\nLocation: /caf\xe9\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nRefresh: 0; url=/cr\xe8me\r\nContent-Type: text/html\r\nContent-Length: 6\r\nConnection: close\r\n\r\nmiddle",
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfinal",
        ];
        let mut paths = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("request must arrive");
            paths.push(read_request_path(&mut socket).await);
            socket.write_all(response).await.expect("response must be writable");
            socket.flush().await.expect("response must flush");
        }
        paths
    }

    #[tokio::test]
    async fn http_fetch_follows_obs_text_location_and_refresh_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener must bind");
        let address = listener.local_addr().expect("listener must have an address");
        let server = tokio::spawn(serve_obs_text_redirect_chain(listener));

        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        let fetched = http_fetch_with(
            &format!("http://{address}/start"),
            &config,
            &HashMap::new(),
            &client,
            RefreshRedirects::Follow,
        )
        .await
        .expect("both non-ASCII redirect headers must be followed");

        assert_eq!(fetched.response.status, 200);
        assert_eq!(fetched.response.body, "final");
        assert_eq!(
            server.await.expect("server task must finish"),
            ["/start", "/caf%C3%A9", "/cr%C3%A8me"]
        );
    }

    /// crawlberg#423: a robots.txt, sitemap or asset fetch (the only callers of the plain fetch)
    /// must send the custom-header agent once, not append it alongside the configured one.
    #[tokio::test]
    async fn a_robots_or_asset_fetch_does_not_duplicate_a_custom_header_user_agent() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let seed = url::Url::parse(&mock.uri()).expect("mock URL must parse");
        let config = CrawlConfig {
            user_agent: Some("Configured".to_owned()),
            custom_headers: HashMap::from([("user-agent".to_owned(), "Custom".to_owned())]),
            credential_scope: crate::net::CredentialScope::for_seed(&seed, None),
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let client = build_client(&config).expect("client must build");
        http_fetch(&format!("{}/probe", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .expect("fetch must succeed");

        let requests = mock.received_requests().await.expect("request recording is on");
        let user_agent_values: Vec<&str> = requests[0]
            .headers
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap_or_default())
            .collect();
        assert_eq!(
            user_agent_values.len(),
            1,
            "a custom_headers user-agent must replace the configured default, not duplicate it: {user_agent_values:?}"
        );
        assert_eq!(user_agent_values, ["Custom"]);
    }

    /// A fetch that names its own `user-agent` (the wasm page fetch, pinning the agent its
    /// robots decision judged) must send that agent once, in place of the configured one.
    #[tokio::test]
    async fn an_extra_header_user_agent_replaces_the_configured_one() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let config = CrawlConfig {
            user_agent: Some("Configured".to_owned()),
            ..permissive_config()
        };
        let client = build_client(&config).expect("client must build");
        let extra_headers = HashMap::from([("user-agent".to_owned(), "Pinned".to_owned())]);
        http_fetch(&format!("{}/probe", mock.uri()), &config, &extra_headers, &client)
            .await
            .expect("fetch must succeed");

        let requests = mock.received_requests().await.expect("request recording is on");
        let user_agent_values: Vec<&str> = requests[0]
            .headers
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap_or_default())
            .collect();
        assert_eq!(
            user_agent_values,
            ["Pinned"],
            "an extra-header user-agent must replace the configured one, not add a second line"
        );
    }
}
