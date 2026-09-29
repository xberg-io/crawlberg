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
    let response = build_partial_response_with_bytes(status, body_bytes, body, headers_map);
    let (signal, evidence) = confirmed_2xx_waf(&*WAF_CLASSIFIER, &response).ok()??;
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
    Ok(confirmed_2xx_waf(classifier, response)?.map(|(signal, _)| signal))
}

fn is_2xx(status: u16) -> bool {
    (200..300).contains(&status)
}

/// The one decision on whether a 2xx `response` is a WAF interstitial, with the evidence class.
///
/// ~keep Three conditions, all required. The status is a 2xx; any other status is not this
/// decision's to make. The body is under [`WAF_2XX_MAX_BODY_LEN`]. And the match is not a
/// header-only one: a fingerprint matching on headers alone proves only that a WAF or CDN is in
/// the request path, which every page that product proxies carries, and a 2xx has no status
/// evidence to go with it. So a header-only match refuses the response only when the body shows
/// the interstitial too, and an ordinary page served through Akamai, Imperva, F5 or Sucuri is
/// returned as content (crawlberg#231).
fn confirmed_2xx_waf(
    classifier: &dyn WafClassifier,
    response: &HttpResponse,
) -> Result<Option<(WafSignal, WafEvidence)>, WafClassifyError> {
    if !is_2xx(response.status) || response.body_bytes.len() >= WAF_2XX_MAX_BODY_LEN {
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

    // ~keep Corroboration has to be asked of the body on its own: re-classifying with the headers
    // would stop at the same header-only fingerprint and never reach the body. So a fingerprint
    // that needs a header and a body signal together cannot corroborate. In the built-in corpus
    // those are Cloudflare's, keyed on `server: cloudflare`, which no header-only fingerprint
    // matches (`waf::tests` pins both), so one of them is lost only when another vendor's
    // header-only fingerprint matches the same response and no body-only fingerprint does.
    Ok(classifier
        .classify(&body_only(response))?
        .map(|_| (signal, WafEvidence::Headers)))
}

/// A copy of `response` with its headers and an empty body.
fn headers_only(response: &HttpResponse) -> HttpResponse {
    HttpResponse {
        headers: response.headers.clone(),
        ..bare(response.status)
    }
}

/// A copy of `response` with its body and no headers.
fn body_only(response: &HttpResponse) -> HttpResponse {
    HttpResponse {
        body: response.body.clone(),
        body_bytes: response.body_bytes.clone(),
        ..bare(response.status)
    }
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

    /// The plain fetch (robots.txt, sitemaps, assets), the engine's crawl fetch and the engine's
    /// classifier hooks give every 2xx the same answer (crawlberg#231).
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
