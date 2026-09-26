//! Classification of the statuses a WAF or CDN challenge is commonly served with.
//!
//! A challenge status has to be fingerprinted *before* [`super::status::status_error`] maps
//! it, because that mapping is what the retry policy reads.

use std::collections::HashMap;

use super::body::read_text_bounded;
use super::status::status_error;
use super::waf;
use crate::error::CrawlError;

/// HTTP 403, the one challenge status [`status_error`] deliberately leaves unmapped.
const FORBIDDEN_STATUS: u16 = 403;

/// The statuses whose response is fingerprinted for a WAF before it is reduced to the error
/// the status carries on its own.
///
/// ~keep 429 and 503 are here because `status_error` maps them to `RateLimited`/`ServerError`,
/// which `SimpleRetryPolicy` answers with `RetryDirective::Retry`. A Cloudflare or Akamai
/// challenge served with 503 was therefore re-requested by the same JS-less client that
/// provoked it and never reached the browser tier (crawlberg#169). The corpus in
/// `rules/waf_fingerprints.toml` stays the single source of truth for *what* is a WAF; this
/// list only decides *which statuses get asked*.
///
/// ~keep Check what a new status would be decided by before adding it: seven corpus
/// fingerprints match on a response header alone. Three of them (`akamai_server_ghost`,
/// `imperva_server_incapsula`, `f5_bigip_server`) prove only that a CDN served the response,
/// which every page behind that CDN does, so the corpus restricts each to `statuses = [403]`
/// and a genuine origin 429 or 503 behind one of them is retried rather than escalated
/// (crawlberg#197). Of the four that stay unrestricted, `x-datadome`, `x-px-block` and
/// `x-amzn-waf-action` name a WAF action; `x-sucuri-id` is a proxy stamp and is the remaining
/// CDN-presence case. Narrowing belongs in the corpus, never in a header list here.
const CHALLENGE_STATUSES: [u16; 3] = [FORBIDDEN_STATUS, 429, 503];

/// Whether `status` is one whose response is fingerprinted for a WAF challenge.
pub(crate) fn is_challenge_status(status: u16) -> bool {
    CHALLENGE_STATUSES.contains(&status)
}

/// The WAF vendor `headers` alone fingerprint for `status`, without reading any body.
///
/// ~keep Passing an empty body is not a shortcut. `Rules::classify` evaluates its header-only
/// fingerprints and returns before it scans the body, and a `body_substring` signal cannot
/// match an empty body, so this is exactly the header-only subset of a full classification and
/// reports the same vendor a full one would.
pub(super) fn header_waf_vendor(status: u16, headers: &HashMap<String, Vec<String>>) -> Option<String> {
    waf::waf_vendor_from_body(status, "", headers)
}

/// Classify a challenge status: a [`CrawlError::WafBlocked`] when the response fingerprints,
/// otherwise the plain error the status carries on its own.
///
/// `resp`'s body is read only when `headers` did not already identify a vendor, so a challenge
/// stamped by a response header costs no body read at all. The read that does happen is capped
/// by `max_body_size`, the same cap the ordinary content path uses.
pub(crate) async fn challenge_status_error(
    status: u16,
    url: &str,
    headers: &HashMap<String, Vec<String>>,
    resp: reqwest::Response,
    max_body_size: Option<usize>,
) -> CrawlError {
    if let Some(vendor) = header_waf_vendor(status, headers) {
        return waf_blocked(status, vendor);
    }

    let body = read_text_bounded(resp, max_body_size).await;
    if let Some(vendor) = waf::waf_vendor_from_body(status, &body, headers) {
        return waf_blocked(status, vendor);
    }

    // ~keep 403 is the only member of `CHALLENGE_STATUSES` that `status_error` does not map,
    // because telling a WAF block from a plain forbidden needs exactly the body just read and
    // rejected above.
    status_error(status, url).unwrap_or_else(|| CrawlError::forbidden("forbidden"))
}

/// The WAF block error for a fingerprinted challenge `status`.
fn waf_blocked(status: u16, vendor: String) -> CrawlError {
    tracing::debug!(
        status,
        vendor = %vendor,
        "challenge status fingerprinted as a WAF block; escalating rather than retrying"
    );
    CrawlError::WafBlocked {
        message: challenge_message(status, &vendor),
        vendor,
    }
}

/// The freeform part of a WAF block's message.
///
/// ~keep 403 keeps its original wording verbatim: `waf/blocked detected: VENDOR` is what
/// existing log-grep patterns match. The statuses added by crawlberg#169 name themselves, so a
/// challenge served with 429 or 503 is distinguishable in a log from a plain 403 block.
fn challenge_message(status: u16, vendor: &str) -> String {
    if status == FORBIDDEN_STATUS {
        format!("waf/blocked detected: {vendor}")
    } else {
        format!("waf/blocked detected on {status}: {vendor}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_statuses_a_challenge_is_served_with_are_fingerprinted() {
        for status in [403_u16, 429, 503] {
            assert!(is_challenge_status(status), "{status} must be fingerprinted");
        }
        // ~keep 500/502/504 are deliberately excluded: no WAF in the corpus serves a challenge
        // with them, and admitting them would buy a body read on every origin failure.
        for status in [200_u16, 204, 301, 400, 401, 404, 408, 410, 500, 502, 504] {
            assert!(!is_challenge_status(status), "{status} must not be fingerprinted");
        }
    }

    #[test]
    fn a_403_block_message_is_unchanged_while_the_new_statuses_name_themselves() {
        assert_eq!(challenge_message(403, "cloudflare"), "waf/blocked detected: cloudflare");
        assert_eq!(
            challenge_message(503, "cloudflare"),
            "waf/blocked detected on 503: cloudflare"
        );
        assert_eq!(
            challenge_message(429, "datadome"),
            "waf/blocked detected on 429: datadome"
        );
    }

    #[test]
    fn a_header_only_fingerprint_is_found_without_a_body_on_every_challenge_status() {
        let headers = HashMap::from([("x-datadome".to_string(), vec!["blocked".to_string()])]);
        for status in [403_u16, 429, 503] {
            assert_eq!(
                header_waf_vendor(status, &headers).as_deref(),
                Some("datadome"),
                "status {status} must fingerprint from headers alone"
            );
        }
    }

    #[test]
    fn a_cdn_presence_header_fingerprints_a_403_but_no_other_challenge_status() {
        for (server, vendor) in [("AkamaiGHost", "akamai"), ("Incapsula", "imperva"), ("BIG-IP", "f5")] {
            let headers = HashMap::from([("server".to_string(), vec![server.to_string()])]);
            assert_eq!(
                header_waf_vendor(FORBIDDEN_STATUS, &headers).as_deref(),
                Some(vendor),
                "a 403 behind {server} is near-certainly a block"
            );
            for status in [429_u16, 503] {
                assert_eq!(
                    header_waf_vendor(status, &headers),
                    None,
                    "a {status} behind {server} is the origin, not an interstitial"
                );
            }
        }
    }

    #[test]
    fn a_body_only_fingerprint_is_not_reported_from_headers_alone() {
        let headers = HashMap::from([("server".to_string(), vec!["cloudflare".to_string()])]);
        assert_eq!(
            header_waf_vendor(503, &headers),
            None,
            "a fingerprint needing a body signal must not fire on headers alone"
        );
    }
}
