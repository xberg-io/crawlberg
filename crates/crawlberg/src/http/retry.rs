//! Retry policy: which failures are retryable, and the backoff between attempts.

use std::time::Duration;

use super::status::error_status;
use super::{HttpResponse, http_fetch};
use crate::defaults::dispatch::compute_backoff_ms;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// Decide whether `error` should trigger a retry, given the configured `retry_codes`.
///
/// Only `RateLimited`, `ServerError`, `BadGateway` and `Timeout` are retryable. An empty
/// `retry_codes` retries all of them. A non-empty `retry_codes` is an allowlist: an error
/// is retried only when the response status it carries (see [`error_status`]) is in the
/// list, so a timeout that never saw a response is not retried.
pub(crate) fn should_retry_error(error: &CrawlError, retry_codes: &[u16]) -> bool {
    let retryable = matches!(
        error,
        CrawlError::RateLimited { .. }
            | CrawlError::ServerError { .. }
            | CrawlError::BadGateway { .. }
            | CrawlError::Timeout { .. }
    );
    retryable && (retry_codes.is_empty() || error_status(error).is_some_and(|status| retry_codes.contains(&status)))
}

/// Fetch a URL with retry logic based on configuration.
///
/// Retries the errors [`should_retry_error`] admits for `config.retry_codes`. Uses the
/// crate-wide exponential backoff (see [`compute_backoff_ms`]), seeded from
/// `config.retry_initial_delay_ms` and capped at `config.retry_max_delay_ms`.
pub(crate) async fn fetch_with_retry(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &std::collections::HashMap<String, String>,
    client: &reqwest::Client,
) -> Result<HttpResponse, CrawlError> {
    let retries = config.retry_count;
    let retry_codes = config.retry_codes.clone();

    let mut last_err = None;
    for attempt in 0..=retries {
        match http_fetch(url, config, extra_headers, client).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let should_retry = should_retry_error(&e, &retry_codes);
                if should_retry && attempt < retries {
                    let attempt_u32 = u32::try_from(attempt).unwrap_or(u32::MAX);
                    let delay_ms =
                        compute_backoff_ms(attempt_u32, config.retry_initial_delay_ms, config.retry_max_delay_ms);
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| CrawlError::other("retry exhausted")))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::http::status::HttpStatus;
    use crate::http::status_error;

    fn from_status(status: u16) -> CrawlError {
        status_error(status, "https://example.com/x").expect("an error status")
    }

    #[test]
    fn each_listed_status_retries_its_own_error_only() {
        // ~keep 500, 503 and 504 raise the same `ServerError` variant, and 408 the same
        // `Timeout` a transport timeout raises. The negative cases are the ones a match on
        // the variant alone gets wrong; asserting only the positive direction would pass.
        let statuses = [408_u16, 429, 500, 502, 503, 504];
        for code in statuses {
            let error = from_status(code);
            assert!(
                should_retry_error(&error, &[code]),
                "{code} must retry its own error, got no retry for {error:?}"
            );
            for other in statuses.into_iter().filter(|other| *other != code) {
                assert!(
                    !should_retry_error(&error, &[other]),
                    "{error:?} must NOT retry when only {other} is listed"
                );
            }
        }
    }

    /// Guard, not a red-green test: a 403 and a WAF block carry their response status since
    /// crawlberg#133, and `should_retry_error` gates on the error variant before it reads that
    /// status — so listing 403, 429 or 503 in `retry_codes` must not make either retryable.
    #[test]
    fn a_forbidden_or_a_waf_block_is_not_retried_even_when_its_status_is_listed() {
        let listed = [403_u16, 429, 503];
        let cases = [
            (CrawlError::forbidden_with_source("forbidden", HttpStatus(403)), 403_u16),
            (
                CrawlError::waf_blocked_with_source("datadome", "waf/blocked on 429", HttpStatus(429)),
                429,
            ),
            (
                CrawlError::waf_blocked_with_source("cloudflare", "waf/blocked on 503", HttpStatus(503)),
                503,
            ),
        ];
        for (error, status) in cases {
            assert_eq!(
                error_status(&error),
                Some(status),
                "the error must carry its status: {error:?}"
            );
            assert!(
                !should_retry_error(&error, &listed),
                "{error:?} must not be retried even with retry_codes {listed:?}"
            );
        }
    }

    #[test]
    fn retry_codes_never_retry_an_unrelated_error() {
        for retry_codes in [&[][..], &[500, 502, 503, 504, 408, 429][..]] {
            assert!(
                !should_retry_error(&from_status(404), retry_codes),
                "a 404 must not retry with retry_codes {retry_codes:?}"
            );
        }
    }

    #[test]
    fn empty_retry_codes_retry_every_retryable_error() {
        let transport_timeout =
            CrawlError::timeout_with_source("[network:timeout] slow", std::io::Error::other("slow"));
        for error in [
            from_status(500),
            from_status(502),
            from_status(503),
            from_status(504),
            from_status(429),
            from_status(408),
            CrawlError::timeout("operation timed out"),
            transport_timeout,
        ] {
            assert!(
                should_retry_error(&error, &[]),
                "an empty retry_codes must retry {error:?}"
            );
        }
    }

    #[test]
    fn a_non_empty_retry_codes_never_retries_a_timeout_without_a_response() {
        // ~keep Neither timeout below saw a response. The one with no source error is the
        // shape `CrawlError::timeout` builds for MCP and custom retry policy callers, so it
        // must not pass for a 408 either.
        let every_code = [408, 429, 500, 502, 503, 504];
        let transport_timeout =
            CrawlError::timeout_with_source("[network:timeout] slow", std::io::Error::other("slow"));
        for error in [transport_timeout, CrawlError::timeout("operation timed out")] {
            assert!(
                !should_retry_error(&error, &every_code),
                "a timeout without a response has no status to list: {error:?}"
            );
        }
        assert!(
            should_retry_error(&from_status(408), &[408]),
            "a 408 response must retry when 408 is listed"
        );
    }
}
