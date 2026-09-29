//! WAF classification: the shared classifier, the vendor lookups the fetch path uses, and the
//! one 2xx decision the fetch paths and the engine's classifier hooks share.

use std::collections::HashMap;
use std::sync::LazyLock;

use opentelemetry::KeyValue;

use super::HttpResponse;
use crate::error::CrawlError;
use crate::types::{WafClassifier, WafClassifyError, WafSignal};
use crate::waf::TomlClassifier;

/// Process-wide WAF classifier built once from the embedded fingerprint corpus.
///
/// ~keep `TomlClassifier::builtin()` re-parses `waf_fingerprints.toml` (via
/// `include_str!`) and rebuilds the Aho-Corasick matcher set on every call — it was
/// previously constructed fresh per response on the `http_fetch` hot path (robots.txt,
/// every asset download, every sitemap fetch, and every page fetch), so this cache
/// turns a per-response parse+compile into a one-time process-wide cost. `classify`
/// only needs `&self`, so a shared immutable instance is safe across concurrent fetches.
static WAF_CLASSIFIER: LazyLock<TomlClassifier> = LazyLock::new(TomlClassifier::builtin);

/// Build a partial [`HttpResponse`] from a pre-built header map + body string.
///
/// Used in the early-exit detection paths where we need to pass a response
/// to [`crate::types::WafClassifier::classify`] before the full
/// [`HttpResponse`] struct is assembled.
fn build_partial_response(status: u16, body: &str, headers_map: &HashMap<String, Vec<String>>) -> HttpResponse {
    let body_bytes = body.as_bytes().to_vec();
    build_partial_response_with_bytes(status, &body_bytes, body, headers_map)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
thread_local! {
    /// Test-only: bytes handed to [`build_partial_response_with_bytes`] on the current test
    /// thread. The standard library gives each `#[test]` its own thread, so this isolates one
    /// test's count from every other test's calls into the same function, with no need for
    /// `--test-threads=1`.
    static PARTIAL_RESPONSE_BYTES_COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Build a partial [`HttpResponse`] with a pre-computed byte vec and a pre-built header map.
///
/// ~keep Takes an already-built `headers_map` (rather than a `reqwest::HeaderMap` it
/// rebuilds internally) so callers checking WAF signals at multiple points for the same
/// response — `http_fetch`'s header-only and body checks — can build the map once and
/// share it instead of re-walking `HeaderMap` and re-lowercasing every header name per check.
fn build_partial_response_with_bytes(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> HttpResponse {
    #[cfg(all(test, not(target_arch = "wasm32")))]
    PARTIAL_RESPONSE_BYTES_COPIED.with(|c| c.set(c.get() + body_bytes.len()));
    HttpResponse {
        status,
        content_type: String::new(),
        body: body.to_string(),
        body_bytes: body_bytes.to_vec(),
        headers: headers_map.clone(),
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    }
}

/// The WAF vendor `classify` reports for a response assembled from `body` and
/// `headers_map`, or `None` when nothing in it fingerprints.
pub(super) fn waf_vendor_from_body(
    status: u16,
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<String> {
    classify_vendor(&build_partial_response(status, body, headers_map))
}

/// The WAF vendor `headers` alone fingerprint for `status`, without reading any body.
///
/// ~keep Passing an empty body is not a shortcut. `Rules::classify` evaluates its header-only
/// fingerprints and returns before it scans the body, and a `body_substring` signal cannot
/// match an empty body, so this is exactly the header-only subset of a full classification and
/// reports the same vendor a full one would.
pub(super) fn header_waf_vendor(status: u16, headers: &HashMap<String, Vec<String>>) -> Option<String> {
    waf_vendor_from_body(status, "", headers)
}

/// Count one response refused as a WAF block for `vendor` in `crawl_waf_blocks_total`.
///
/// ~keep This is the only place that counter is incremented. The fetch path calls it through
/// [`waf_block`] when it refuses a response, and the engine calls it when its antibot strategy or
/// retry policy refuses a response the fetch path returned. The two never see the same response,
/// so each refused response counts once.
pub(crate) fn record_waf_block(vendor: &str) {
    crate::telemetry::metrics::registry()
        .waf_blocks_total
        .add(1, &[KeyValue::new("vendor", vendor.to_owned())]);
}

/// The [`CrawlError::WafBlocked`] the fetch path refuses a response with, counted once.
pub(super) fn waf_block(vendor: String, message: String) -> CrawlError {
    record_waf_block(&vendor);
    CrawlError::WafBlocked { message, vendor }
}

/// Largest 2xx body that can still be refused as a WAF interstitial rather than returned as content.
///
/// ~keep A challenge page is a few kilobytes. A larger 2xx is a real page, and refusing one
/// because it names a vendor in prose or says "blocked" on a Cloudflare-served site loses the
/// page the crawl came for.
const WAF_2XX_MAX_BODY_LEN: usize = 5000;

/// The evidence that decided a 2xx response carries a WAF interstitial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WafEvidence {
    /// The response headers named the vendor and the body corroborated it.
    Headers,
    /// A body signal took part in the match.
    Body,
}

impl WafEvidence {
    /// The word this evidence class is named by in a block message.
    fn label(self) -> &'static str {
        match self {
            Self::Headers => "header",
            Self::Body => "body",
        }
    }
}

/// The [`CrawlError::WafBlocked`] the fetch path refuses a response with under the 2xx decision,
/// or `None` when the response is not a 2xx or is ordinary content.
///
/// ~keep Both fetch paths call this for every response they are about to return, the plain fetch
/// from `fetch_one_hop` and the engine's Tower fetch from `do_fetch`, so they refuse exactly the
/// same responses.
pub(crate) fn waf_2xx_error(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<CrawlError> {
    refuse_2xx_with(status, body_bytes.len(), || {
        build_partial_response_with_bytes(status, body_bytes, body, headers_map)
    })
}

/// [`waf_2xx_error`] for a response that `response` builds on demand.
///
/// ~keep The status and size are checked before `response` runs, so a response the decision
/// cannot refuse, which is every non-2xx and every real page, is never copied to be classified.
fn refuse_2xx_with(status: u16, body_len: usize, response: impl FnOnce() -> HttpResponse) -> Option<CrawlError> {
    if !in_2xx_decision(status, body_len, Some(WAF_2XX_MAX_BODY_LEN)) {
        return None;
    }
    block_page_error(&response(), Some(WAF_2XX_MAX_BODY_LEN))
}

/// [`waf_2xx_error`] for a robots.txt fetch: `None` when the body is the site's robots.txt, else
/// the refusal a 2xx block page gets.
///
/// ~keep A body that reads as robots.txt is rules, as RFC 9309 reads any 2xx: a fingerprint
/// such as `server: cloudflare` with "blocked" in the body also matches a comment written for a
/// human reader (crawlberg#507). Any other body gets the 2xx decision without
/// [`WAF_2XX_MAX_BODY_LEN`], only the classifier's own body limit: a block page served as
/// robots.txt is an interstitial at any size, and reading one as rules hands a WAF-protected
/// site an unrestricted crawl.
pub(super) fn robots_2xx_error(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<CrawlError> {
    if crate::robots::reads_as_robots_txt(body) {
        return None;
    }
    block_page_error(
        &build_partial_response_with_bytes(status, body_bytes, body, headers_map),
        None,
    )
}

/// [`waf_2xx_error`] for a sitemap fetch: `None` when the body is a sitemap document, else the
/// refusal a page gets.
///
/// ~keep A sitemap is read whatever its `<loc>`s say: a fingerprint such as `server: cloudflare`
/// with "blocked" in the body also matches a URL like "/blog/why-we-blocked-the-old-api"
/// (crawlberg#515). Any other body keeps the page decision and its [`WAF_2XX_MAX_BODY_LEN`]
/// limit. The body is read as a sitemap only inside that limit, where the page decision could
/// refuse it.
pub(super) fn sitemap_2xx_error(
    status: u16,
    content_type: &str,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<CrawlError> {
    if in_2xx_decision(status, body_bytes.len(), Some(WAF_2XX_MAX_BODY_LEN))
        && crate::sitemap::reads_as_sitemap(content_type, body_bytes, body)
    {
        return None;
    }
    waf_2xx_error(status, body_bytes, body, headers_map)
}

/// The counted refusal for a 2xx `response` the built-in classifier confirms as a block page.
fn block_page_error(response: &HttpResponse, max_body_len: Option<usize>) -> Option<CrawlError> {
    let (signal, evidence) = confirmed_2xx_waf(&*WAF_CLASSIFIER, response, max_body_len).ok()??;
    let message = format!("waf/blocked detected on 2xx ({}): {}", evidence.label(), signal.vendor);
    Some(waf_block(signal.vendor, message))
}

/// The WAF signal the engine hands its antibot strategy and retry policy for `response`.
///
/// ~keep A 2xx gets the same decision the fetch paths refuse with, asked of the engine's own
/// classifier, so a hook never refuses a 2xx the fetch paths would return as content. Any
/// other status gets the classifier's answer unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn engine_waf_signal(
    classifier: &dyn WafClassifier,
    response: &HttpResponse,
) -> Result<Option<WafSignal>, WafClassifyError> {
    if !is_2xx(response.status) {
        return classifier.classify(response);
    }
    Ok(confirmed_2xx_waf(classifier, response, Some(WAF_2XX_MAX_BODY_LEN))?.map(|(signal, _)| signal))
}

fn is_2xx(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Whether the 2xx decision applies to `status` with a body of `body_len` bytes, when a body of
/// `max_body_len` bytes or more is content whatever it holds.
fn in_2xx_decision(status: u16, body_len: usize, max_body_len: Option<usize>) -> bool {
    is_2xx(status) && max_body_len.is_none_or(|max| body_len < max)
}

/// The one decision on whether a 2xx `response` is a WAF interstitial, with the evidence class.
///
/// ~keep Three conditions, all required. The status is a 2xx; any other status is not this
/// decision's to make. The body is under `max_body_len` ([`WAF_2XX_MAX_BODY_LEN`] for page
/// content). And the match is not a header-only one: a fingerprint matching on headers alone
/// proves only that a WAF or CDN is in the request path, which every page that product proxies
/// carries, and a 2xx has no status evidence to go with it. So a header-only match refuses the
/// response only when the body shows the interstitial too, and an ordinary page served through
/// Akamai, Imperva, F5 or Sucuri is returned as content (crawlberg#231).
fn confirmed_2xx_waf(
    classifier: &dyn WafClassifier,
    response: &HttpResponse,
    max_body_len: Option<usize>,
) -> Result<Option<(WafSignal, WafEvidence)>, WafClassifyError> {
    if !in_2xx_decision(response.status, response.body_bytes.len(), max_body_len) {
        return Ok(None);
    }
    let Some(signal) = classifier.classify(response)? else {
        return Ok(None);
    };

    // ~keep An empty body cannot match a body signal, so this asks for a header-only match. None
    // means a body signal took part in the match above, which needs no further corroboration.
    if classifier.classify(&headers_only(response))?.is_none() {
        return Ok(Some((signal, WafEvidence::Body)));
    }

    // ~keep Re-classifying the whole response would stop at the same header-only fingerprint and
    // never reach the body, so the headers that match on their own are set aside first. The
    // others stay: a fingerprint that needs a header and a body signal together, such as
    // Cloudflare's `server: cloudflare` with a block phrase, still corroborates when another
    // vendor's header-only fingerprint matches the same response.
    Ok(classifier
        .classify(&without_header_only_matches(classifier, response)?)?
        .map(|_| (signal, WafEvidence::Headers)))
}

/// A copy of `response` with its headers and an empty body.
fn headers_only(response: &HttpResponse) -> HttpResponse {
    HttpResponse {
        headers: response.headers.clone(),
        ..bare(response.status)
    }
}

/// A copy of `response` without the headers that fingerprint on their own, so that any match
/// it makes has a body signal in it.
///
/// ~keep Each header is asked on its own, so two headers that fingerprint only together are both
/// kept. When the kept headers still match without a body, every header is set aside and the
/// body is asked alone.
fn without_header_only_matches(
    classifier: &dyn WafClassifier,
    response: &HttpResponse,
) -> Result<HttpResponse, WafClassifyError> {
    let mut rest = bare(response.status);
    for (name, values) in &response.headers {
        let alone = HttpResponse {
            headers: HashMap::from([(name.clone(), values.clone())]),
            ..bare(response.status)
        };
        if classifier.classify(&alone)?.is_none() {
            rest.headers.insert(name.clone(), values.clone());
        }
    }
    if classifier.classify(&rest)?.is_some() {
        rest.headers.clear();
    }
    rest.body = response.body.clone();
    rest.body_bytes = response.body_bytes.clone();
    Ok(rest)
}

/// A response carrying only `status`.
fn bare(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        content_type: String::new(),
        body: String::new(),
        body_bytes: Vec::new(),
        headers: HashMap::new(),
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    }
}

fn classify_vendor(response: &HttpResponse) -> Option<String> {
    WAF_CLASSIFIER
        .classify(response)
        .ok()
        .flatten()
        .map(|signal| signal.vendor)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::error::CrawlError;
    use crate::types::{BrowserMode, CrawlConfig, DispatchProfile};

    const DATADOME_TAG: &str = "<script src=\"https://js.datadome.co/tags.js\"></script>";

    /// An HTML page of exactly `len` bytes that carries `marker` once and pads the rest.
    fn page_of_len(marker: &str, len: usize) -> String {
        let frame = "<html><!----></html>".len() + marker.len();
        let pad = len.checked_sub(frame).expect("len fits the marker");
        let page = format!("<html>{marker}<!--{}--></html>", "x".repeat(pad));
        assert_eq!(page.len(), len, "the fixture must be exactly {len} bytes");
        page
    }

    /// A page of exactly `len` raw bytes carrying `marker` and ten bytes that are not UTF-8.
    ///
    /// ~keep Each invalid byte decodes to a three-byte replacement character, so the decoded text
    /// is 20 bytes longer than the body: the size limit must be measured on the bytes received.
    fn not_utf8_page(marker: &str, len: usize) -> Vec<u8> {
        let mut page = page_of_len(marker, len - 10).into_bytes();
        let close = page.len() - "--></html>".len();
        page.splice(close..close, [0xFF_u8; 10]);
        assert_eq!(page.len(), len, "the fixture must be exactly {len} bytes");
        assert!(
            String::from_utf8_lossy(&page).len() >= 5000,
            "the decoded text must cross the limit"
        );
        page
    }

    fn config() -> CrawlConfig {
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.browser.mode = BrowserMode::Never;
        config.retry_count = 0;
        config
    }

    /// The engine configured with the built-in classifier and antibot strategy as hooks.
    fn hook_config() -> CrawlConfig {
        let mut config = config();
        config.dispatch = Some(DispatchProfile {
            waf_classifier: Some(Arc::new(crate::waf::TomlClassifier::builtin())),
            antibot_strategy: Some(Arc::new(crate::types::antibot::DefaultAntibotStrategy::new())),
            ..DispatchProfile::default()
        });
        config
    }

    /// `Ok` for content, or the vendor a response was refused for.
    fn verdict<T>(result: Result<T, CrawlError>) -> Result<(), String> {
        match result {
            Ok(_) => Ok(()),
            Err(CrawlError::WafBlocked { vendor, .. }) => Err(vendor),
            Err(other) => Err(format!("not a WAF block: {other}")),
        }
    }

    /// What the plain fetch, the engine and the engine with classifier hooks make of `template`.
    async fn three_paths(template: ResponseTemplate) -> [Result<(), String>; 3] {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(template)
            .mount(&mock)
            .await;
        let url = format!("{}/page", mock.uri());

        let plain_config = config();
        let client = crate::http::build_client(&plain_config).expect("client must build");
        let plain = crate::http::http_fetch(&url, &plain_config, &std::collections::HashMap::new(), &client).await;
        let engine = crate::create_engine(Some(config())).expect("engine must build");
        let hooked = crate::create_engine(Some(hook_config())).expect("engine must build");
        [
            verdict(plain),
            verdict(crate::scrape(&engine, &url).await),
            verdict(crate::scrape(&hooked, &url).await),
        ]
    }

    fn html(status: u16, body: String) -> ResponseTemplate {
        ResponseTemplate::new(status)
            .append_header("content-type", "text/html")
            .set_body_string(body)
    }

    /// The plain fetch (assets, and sitemaps that do not read as one), the engine's crawl fetch
    /// and the engine's classifier hooks give every 2xx the same answer (crawlberg#231).
    ///
    /// ~keep Every row runs before the assertion so one disagreement cannot hide the others.
    #[tokio::test]
    async fn every_fetch_path_gives_a_2xx_the_same_waf_verdict() {
        let content = Ok(());
        let datadome = Err("datadome".to_owned());
        let ordinary = "<html><body><h1>Release notes</h1></body></html>".to_owned();
        let article = format!(
            "<html><body><h1>Why we blocked the old API</h1>{}</body></html>",
            "<p>The old endpoint is retired.</p>".repeat(600)
        );
        let cases: Vec<(&str, ResponseTemplate, Result<(), String>)> = vec![
            (
                "200, bare x-sucuri-id, ordinary page",
                html(200, ordinary.clone()).append_header("x-sucuri-id", "18012"),
                content.clone(),
            ),
            (
                "200, server AkamaiGHost, 7 KB page",
                html(200, page_of_len("<h1>Release notes</h1>", 7000)).append_header("server", "AkamaiGHost"),
                content.clone(),
            ),
            (
                "200, x-datadome and the DataDome tag",
                html(200, page_of_len(DATADOME_TAG, 400)).append_header("x-datadome", "protected"),
                datadome.clone(),
            ),
            (
                "200, Cloudflare challenge body",
                html(200, "<html>cf-chl- x</html>".to_owned()),
                Err("cloudflare".to_owned()),
            ),
            (
                "203, x-datadome and the DataDome tag",
                html(203, page_of_len(DATADOME_TAG, 400)).append_header("x-datadome", "protected"),
                datadome.clone(),
            ),
            (
                "200, the DataDome tag in a 4999-byte body",
                html(200, page_of_len(DATADOME_TAG, 4999)).append_header("x-datadome", "protected"),
                datadome.clone(),
            ),
            (
                "200, the DataDome tag in a 5000-byte body",
                html(200, page_of_len(DATADOME_TAG, 5000)).append_header("x-datadome", "protected"),
                content.clone(),
            ),
            (
                "200, the DataDome tag in 4990 bytes, ten of them not UTF-8",
                ResponseTemplate::new(200)
                    .append_header("content-type", "text/html")
                    .append_header("x-datadome", "protected")
                    .set_body_bytes(not_utf8_page(DATADOME_TAG, 4990)),
                datadome.clone(),
            ),
            (
                "200, the DataDome tag alone in a 6 KB body",
                html(200, page_of_len(DATADOME_TAG, 6000)),
                content.clone(),
            ),
            (
                "200, server cloudflare, Cloudflare block page",
                html(200, "<html><h1>Access blocked</h1></html>".to_owned()).append_header("server", "cloudflare"),
                Err("cloudflare".to_owned()),
            ),
            (
                "200, server cloudflare and x-sucuri-id, Cloudflare block page",
                html(200, "<html><h1>Access blocked</h1></html>".to_owned())
                    .append_header("server", "cloudflare")
                    .append_header("x-sucuri-id", "18012"),
                Err("imperva".to_owned()),
            ),
            (
                "200, server cloudflare, robots.txt text that says blocked, fetched as a page",
                ResponseTemplate::new(200)
                    .append_header("content-type", "text/plain")
                    .append_header("server", "cloudflare")
                    .set_body_string("# AI crawlers are blocked below\nUser-agent: *\nDisallow: /private\n"),
                Err("cloudflare".to_owned()),
            ),
            (
                "200, server cloudflare, a sitemap that lists a URL saying blocked, fetched as a page",
                cloudflare("application/xml", SITEMAP_WITH_A_BLOCKED_URL),
                Err("cloudflare".to_owned()),
            ),
            (
                "200, server cloudflare, a short page that says blocked",
                html(
                    200,
                    "<html><body><p>We blocked the old API.</p></body></html>".to_owned(),
                )
                .append_header("server", "cloudflare"),
                Err("cloudflare".to_owned()),
            ),
            (
                "200, server cloudflare, 20 KB article that says blocked",
                html(200, article).append_header("server", "cloudflare"),
                content.clone(),
            ),
        ];

        let mut disagreements = Vec::new();
        for (label, template, expected) in cases {
            let verdicts = three_paths(template).await;
            for (path_name, verdict) in ["plain fetch", "engine", "engine hooks"].iter().zip(&verdicts) {
                if *verdict != expected {
                    disagreements.push(format!("{label}: {path_name} gave {verdict:?}, expected {expected:?}"));
                }
            }
        }
        assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
    }

    /// The 184-byte sitemap from crawlberg#515: one `<loc>` whose path says "blocked".
    const SITEMAP_WITH_A_BLOCKED_URL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n<url><loc>https://example.com/blog/why-we-blocked-the-old-api</loc></url>\n</urlset>\n";

    /// The `<loc>` [`SITEMAP_WITH_A_BLOCKED_URL`] lists.
    const BLOCKED_LOC: &str = "https://example.com/blog/why-we-blocked-the-old-api";

    /// A 200 `content_type` response served by Cloudflare with `body`.
    fn cloudflare(content_type: &str, body: impl Into<Vec<u8>>) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .append_header("content-type", content_type)
            .append_header("server", "cloudflare")
            .set_body_bytes(body)
    }

    /// `body` as a gzip file whose deflate blocks are stored, so its bytes carry the text as is.
    fn stored_gzip(body: &str) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        encoder.write_all(body.as_bytes()).expect("gzip must encode");
        let gzip = encoder.finish().expect("gzip must finish");
        assert!(
            String::from_utf8_lossy(&gzip).contains("blocked"),
            "the gzip bytes must carry the text the fingerprint matches"
        );
        gzip
    }

    /// What a mock server serves: each path with its response.
    type Mounts<'a> = Vec<(&'a str, ResponseTemplate)>;

    /// The `<loc>`s `map` returned, or the vendor it was refused for.
    fn map_verdict(result: Result<crate::MapResult, CrawlError>) -> Result<Vec<String>, String> {
        result
            .map(|map| map.urls.into_iter().map(|entry| entry.url).collect())
            .map_err(|error| match error {
                CrawlError::WafBlocked { vendor, .. } => vendor,
                other => format!("not a WAF block: {other}"),
            })
    }

    /// What the sitemap walk of `walked` and the engine's map of `mapped`, with and without
    /// classifier hooks, read on a server that serves `mounts`: the `<loc>`s, or the vendor of
    /// the refusal.
    ///
    /// ~keep The walk swallows a failed fetch as an empty list, so its column cannot name a vendor.
    async fn sitemap_paths(mounts: Mounts<'_>, walked: &str, mapped: &str) -> [Result<Vec<String>, String>; 3] {
        let mock = MockServer::start().await;
        for (served, template) in mounts {
            Mock::given(method("GET"))
                .and(path(served))
                .respond_with(template)
                .mount(&mock)
                .await;
        }
        let walked = format!("{}{walked}", mock.uri());
        let mapped = format!("{}{mapped}", mock.uri());

        let plain_config = config();
        let client = crate::http::build_client(&plain_config).expect("client must build");
        let filter = crate::map::MapFilter::from_config(&plain_config).expect("filter must build");
        let context = crate::sitemap::SitemapWalkContext::new(&plain_config, &client, &filter);
        let walk = crate::sitemap::fetch_sitemap_tree(&walked, &context, None).await;
        let engine = crate::create_engine(Some(config())).expect("engine must build");
        let hooked = crate::create_engine(Some(hook_config())).expect("engine must build");
        [
            Ok(walk.into_iter().map(|entry| entry.url).collect()),
            map_verdict(crate::map_urls(&engine, &mapped).await),
            map_verdict(crate::map_urls(&hooked, &mapped).await),
        ]
    }

    /// A sitemap that lists a URL saying "blocked" is read behind Cloudflare, wherever map finds
    /// it: at /sitemap.xml, at the URL mapped, as an index child, and gzipped (crawlberg#515).
    ///
    /// ~keep Every row runs before the assertion so one disagreement cannot hide the others.
    #[tokio::test]
    async fn a_sitemap_that_lists_a_blocked_url_is_read_behind_cloudflare() {
        assert_eq!(
            SITEMAP_WITH_A_BLOCKED_URL.len(),
            184,
            "the fixture must be the issue's 184-byte body"
        );
        let index = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n<sitemap><loc>/sitemaps/why-we-blocked.xml</loc></sitemap>\n</sitemapindex>\n";
        let xml = || cloudflare("application/xml", SITEMAP_WITH_A_BLOCKED_URL);
        let cases: Vec<(&str, Mounts<'_>, &str, &str)> = vec![
            (
                "the issue's sitemap at /sitemap.xml",
                vec![("/sitemap.xml", xml())],
                "/sitemap.xml",
                "/",
            ),
            (
                "the issue's sitemap at the URL mapped",
                vec![("/feed/posts.xml", xml())],
                "/feed/posts.xml",
                "/feed/posts.xml",
            ),
            (
                "an index whose child URL says blocked",
                vec![
                    ("/sitemap_index.xml", cloudflare("application/xml", index)),
                    ("/sitemaps/why-we-blocked.xml", xml()),
                ],
                "/sitemap_index.xml",
                "/sitemap_index.xml",
            ),
            (
                "the issue's sitemap gzipped",
                vec![(
                    "/sitemap.xml.gz",
                    cloudflare("application/x-gzip", stored_gzip(SITEMAP_WITH_A_BLOCKED_URL)),
                )],
                "/sitemap.xml.gz",
                "/sitemap.xml.gz",
            ),
        ];

        let mut disagreements = Vec::new();
        for (label, mounts, walked, mapped) in cases {
            let verdicts = sitemap_paths(mounts, walked, mapped).await;
            for (path_name, verdict) in ["sitemap walk", "engine map", "engine map with hooks"]
                .iter()
                .zip(&verdicts)
            {
                if *verdict != Ok(vec![BLOCKED_LOC.to_owned()]) {
                    disagreements.push(format!("{label}: {path_name} must read the sitemap, got {verdict:?}"));
                }
            }
        }
        assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
    }

    /// A block page served by Cloudflare at a sitemap URL is still refused as a WAF block, whatever
    /// sitemap text it carries: HTML or text that shows sitemap XML, a CDN's XML error, JSON, and
    /// a sitemap followed by or holding block text.
    #[tokio::test]
    async fn a_block_page_served_at_a_sitemap_url_is_still_refused_behind_cloudflare() {
        let cases = [
            (
                "an HTML block page",
                "<html><head><title>Attention Required</title></head><body><h1>Sorry, you have been blocked</h1></body></html>".to_owned(),
            ),
            (
                "an HTML block page that shows sitemap XML",
                format!("<html><body><pre>{SITEMAP_WITH_A_BLOCKED_URL}</pre><h1>Access blocked</h1></body></html>"),
            ),
            (
                "an XHTML block page",
                "<?xml version=\"1.0\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h1>Access blocked</h1></body></html>".to_owned(),
            ),
            (
                "a sitemap followed by a block page",
                format!("{SITEMAP_WITH_A_BLOCKED_URL}<html><body><h1>Access blocked</h1></body></html>"),
            ),
            ("a text block page", "Status: blocked\nReason: automated traffic\n".to_owned()),
            (
                "a text block page that shows sitemap XML",
                format!("Access blocked for this request:\n{SITEMAP_WITH_A_BLOCKED_URL}"),
            ),
            (
                "a CDN's XML error",
                "<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>Request blocked</Message></Error>"
                    .to_owned(),
            ),
            (
                "an HTML block page with a loc element",
                format!("<html><body><loc>{BLOCKED_LOC}</loc><h1>Access blocked</h1></body></html>"),
            ),
            ("a JSON block page", "{\"error\":\"blocked\",\"urlset\":[]}".to_owned()),
            (
                "a urlset that holds block text",
                "<urlset><url><loc>/a</loc></url>Sorry, you have been blocked</urlset>".to_owned(),
            ),
        ];
        let mut disagreements = Vec::new();
        for (label, body) in cases {
            let verdicts = sitemap_paths(
                vec![("/sitemap.xml", cloudflare("application/xml", body))],
                "/sitemap.xml",
                "/sitemap.xml",
            )
            .await;
            let [walk, engine, hooked] = &verdicts;
            let refused = |verdict: &Result<Vec<String>, String>| {
                verdict
                    .as_ref()
                    .is_err_and(|vendor| !vendor.starts_with("not a WAF block"))
            };
            if *walk != Ok(Vec::new()) || !refused(engine) || !refused(hooked) {
                disagreements.push(format!("{label}: must be refused as a WAF block, got {verdicts:?}"));
            }
        }
        assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
    }

    /// The 2xx decision copies a response to classify it only when it may refuse it: a non-2xx or
    /// a body at or over the size limit is never built.
    #[test]
    fn the_2xx_decision_copies_no_response_it_cannot_refuse() {
        for (status, len) in [(418_u16, 400_usize), (304, 0), (200, 5000), (206, 2_000_000)] {
            let refusal = super::refuse_2xx_with(status, len, || {
                panic!("a {status} with {len} body bytes must not be copied to be classified")
            });
            assert!(
                refusal.is_none(),
                "a {status} with {len} body bytes must not be refused"
            );
        }

        let body = page_of_len(DATADOME_TAG, 400);
        let headers = std::collections::HashMap::from([("x-datadome".to_owned(), vec!["protected".to_owned()])]);
        let refusal = super::refuse_2xx_with(200, body.len(), || {
            super::build_partial_response_with_bytes(200, body.as_bytes(), &body, &headers)
        });
        assert!(
            matches!(refusal, Some(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "datadome"),
            "a small DataDome 200 must be built and refused, got {refusal:?}"
        );
    }

    /// `waf_2xx_error`, the call site both fetch paths share, must not build the response it
    /// hands to the classifier when its own gate already refuses the input: a non-2xx status, or
    /// a body at or over the size limit, is never a page the 2xx decision may act on.
    #[test]
    fn waf_2xx_error_at_the_call_site_copies_no_response_its_gate_would_refuse() {
        for (status, len) in [(418_u16, 400_usize), (304, 0), (200, 5000), (206, 2_000_000)] {
            super::PARTIAL_RESPONSE_BYTES_COPIED.with(|c| c.set(0));
            let body = if len == 0 {
                String::new()
            } else {
                page_of_len("<h1>Release notes</h1>", len)
            };
            let headers = std::collections::HashMap::new();

            let refusal = super::waf_2xx_error(status, body.as_bytes(), &body, &headers);

            assert!(
                refusal.is_none(),
                "a {status} with {len} body bytes must not be refused"
            );
            assert_eq!(
                super::PARTIAL_RESPONSE_BYTES_COPIED.with(|c| c.get()),
                0,
                "a {status} with {len} body bytes must not be copied to be classified"
            );
        }
    }

    /// Two headers that fingerprint only together are both set aside by corroboration, so they
    /// cannot corroborate their own match on an ordinary 2xx.
    #[test]
    fn headers_that_fingerprint_only_together_cannot_corroborate_themselves() {
        let rules = crate::waf::rules::load_from_str(
            r#"
[[fingerprint]]
id = "pair"
vendor = "pair"
weight = 1.0
[[fingerprint.signals]]
kind = "response_header"
name = "x-a"
[[fingerprint.signals]]
kind = "response_header"
name = "x-b"
"#,
        )
        .expect("valid rules");
        let classifier = crate::waf::TomlClassifier::from_rules(rules);
        let body = "<html><body><h1>Release notes</h1></body></html>";
        let headers = std::collections::HashMap::from([
            ("x-a".to_owned(), vec!["1".to_owned()]),
            ("x-b".to_owned(), vec!["1".to_owned()]),
        ]);
        let response = super::build_partial_response_with_bytes(200, body.as_bytes(), body, &headers);
        let decision = super::confirmed_2xx_waf(&classifier, &response, Some(super::WAF_2XX_MAX_BODY_LEN))
            .expect("classify must not fail");
        assert_eq!(
            decision, None,
            "a header-only pair must not corroborate an ordinary page"
        );
    }

    /// A 2xx refusal is decided for 2xx statuses only: a 418 carrying DataDome's interstitial is
    /// returned as content by both fetch paths, as every status without a rule of its own is.
    #[tokio::test]
    async fn a_non_2xx_status_is_not_refused_by_the_2xx_decision() {
        let [plain, engine, _] =
            three_paths(html(418, page_of_len(DATADOME_TAG, 400)).append_header("x-datadome", "protected")).await;
        assert_eq!(
            plain,
            Ok(()),
            "a 418 must not be refused by the plain fetch's 2xx check"
        );
        assert_eq!(engine, Ok(()), "a 418 must not be refused by the engine's 2xx check");
    }
}
