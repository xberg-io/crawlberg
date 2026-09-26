//! WAF classification: the shared classifier and the vendor lookups the fetch path uses.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::HttpResponse;
use crate::error::CrawlError;
use crate::types::WafClassifier;
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

/// The [`CrawlError::WafBlocked`] a 2xx is refused with, or `None` when it is ordinary content.
///
/// ~keep A fingerprint matching on response headers alone proves only that a WAF or CDN is in
/// the request path, which every page that product proxies carries — and unlike a 403 or a 503,
/// a 2xx carries no status evidence to go with it. So a header-only match refuses the response
/// only when the body shows the interstitial too, and an ordinary page served through Akamai,
/// Imperva or F5 is returned as content instead of failing the fetch with the real page in hand
/// (crawlberg#231).
pub(crate) fn waf_2xx_error(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<CrawlError> {
    let (vendor, evidence) = confirmed_2xx_waf(status, body_bytes, body, headers_map)?;
    Some(CrawlError::WafBlocked {
        message: format!("waf/blocked detected on 2xx ({}): {vendor}", evidence.label()),
        vendor,
    })
}

/// The vendor a 2xx is refused for, with the evidence class that decided it.
fn confirmed_2xx_waf(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<(String, WafEvidence)> {
    let vendor = classify_vendor(&build_partial_response_with_bytes(
        status,
        body_bytes,
        body,
        headers_map,
    ))?;

    // ~keep An empty body reproduces exactly the header-only subset of a classification:
    // `Rules::classify` evaluates its header-only fingerprints first and returns before it scans
    // the body, and a `body_substring` signal cannot match an empty body. So a `None` here means
    // a body signal took part in the match above and the match needs no further corroboration.
    if waf_vendor_from_body(status, "", headers_map).is_none() {
        return Some((vendor, WafEvidence::Body));
    }

    // ~keep Corroboration has to be asked of the body on its own: re-classifying with the
    // headers would short-circuit on the same header-only fingerprint and never reach the body.
    // Dropping them loses nothing, because the only header+body fingerprints in the corpus are
    // Cloudflare's and `server: cloudflare` is not a header-only match, so none of them can be
    // what reached this branch.
    classify_vendor(&build_partial_response_with_bytes(
        status,
        body_bytes,
        body,
        &HashMap::new(),
    ))
    .map(|_| (vendor, WafEvidence::Headers))
}

fn classify_vendor(response: &HttpResponse) -> Option<String> {
    WAF_CLASSIFIER
        .classify(response)
        .ok()
        .flatten()
        .map(|signal| signal.vendor)
}
