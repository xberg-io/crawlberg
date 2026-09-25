//! Retry policy: which failures are retryable, and the backoff between attempts.

use std::time::Duration;

use super::{HttpResponse, http_fetch};
use crate::defaults::dispatch::compute_backoff_ms;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// Substring appended to a `CrawlError::ServerError` message for a 503 response.
///
/// ~keep `CrawlError::ServerError` carries a single free-form `String` with no numeric
/// status field (see `error.rs`), so 500, 503, and 504 all raise the same enum variant.
/// `should_retry_status` matches on this substring to tell them apart when deciding
/// which `retry_codes` entry governs a given failure — without it, configuring a retry
/// for one of the three silently also retried (or failed to retry) the other two.
pub(super) const SERVICE_UNAVAILABLE_SUFFIX: &str = "service unavailable";

/// Substring appended to a `CrawlError::ServerError` message for a 504 response.
/// See [`SERVICE_UNAVAILABLE_SUFFIX`] for why this disambiguation is needed.
pub(super) const GATEWAY_TIMEOUT_SUFFIX: &str = "gateway timeout";

/// Decide whether `error` should trigger a retry, given the configured `retry_codes`.
///
/// Each configured status code retries exactly the failures that produced it: 500 only
/// retries a plain `ServerError`, 503 only a `ServerError` carrying
/// [`SERVICE_UNAVAILABLE_SUFFIX`], 504 only one carrying [`GATEWAY_TIMEOUT_SUFFIX`], 502
/// only `BadGateway`, 408 only `Timeout`, and 429 only `RateLimited`. Any other error
/// (including statuses not covered by `retry_codes`) never retries.
fn should_retry_status(error: &CrawlError, retry_codes: &[u16]) -> bool {
    match error {
        CrawlError::ServerError { message: msg, .. } if msg.contains(GATEWAY_TIMEOUT_SUFFIX) => {
            retry_codes.contains(&504)
        }
        CrawlError::ServerError { message: msg, .. } if msg.contains(SERVICE_UNAVAILABLE_SUFFIX) => {
            retry_codes.contains(&503)
        }
        CrawlError::ServerError { .. } => retry_codes.contains(&500),
        CrawlError::BadGateway { .. } => retry_codes.contains(&502),
        CrawlError::Timeout { .. } => retry_codes.contains(&408),
        CrawlError::RateLimited { .. } => retry_codes.contains(&429),
        _ => false,
    }
}

/// Fetch a URL with retry logic based on configuration.
///
/// Retries on server errors and rate limiting if the corresponding status codes
/// are included in `config.retry_codes`. Uses the crate-wide exponential backoff
/// (see [`compute_backoff_ms`]), seeded from `config.retry_initial_delay_ms` and
/// capped at `config.retry_max_delay_ms`.
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
                let should_retry = should_retry_status(&e, &retry_codes);
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

    fn server_error(suffix: &str) -> CrawlError {
        CrawlError::server_error(format!("server_error: {suffix}"))
    }

    #[test]
    fn retry_codes_distinguish_the_three_server_error_statuses() {
        // ~keep Regression: `CrawlError::ServerError` carries a free-form String with no
        // status field, so 500, 503 and 504 all raise the same variant. Matching on the
        // variant alone meant configuring a retry for one silently governed all three.
        // The negative cases below are the ones that fail against the old code — a test
        // asserting only the positive direction would pass either way.
        let only_500 = [500_u16];
        assert!(
            should_retry_status(&server_error("internal"), &only_500),
            "a plain 500 must retry when 500 is configured"
        );
        assert!(
            !should_retry_status(&server_error(SERVICE_UNAVAILABLE_SUFFIX), &only_500),
            "a 503 must NOT retry when only 500 is configured"
        );
        assert!(
            !should_retry_status(&server_error(GATEWAY_TIMEOUT_SUFFIX), &only_500),
            "a 504 must NOT retry when only 500 is configured"
        );

        let only_503 = [503_u16];
        assert!(
            should_retry_status(&server_error(SERVICE_UNAVAILABLE_SUFFIX), &only_503),
            "a 503 must retry when 503 is configured"
        );
        assert!(
            !should_retry_status(&server_error("internal"), &only_503),
            "a plain 500 must NOT retry when only 503 is configured"
        );

        let only_504 = [504_u16];
        assert!(
            should_retry_status(&server_error(GATEWAY_TIMEOUT_SUFFIX), &only_504),
            "a 504 must retry when 504 is configured"
        );
        assert!(
            !should_retry_status(&server_error(SERVICE_UNAVAILABLE_SUFFIX), &only_504),
            "a 503 must NOT retry when only 504 is configured"
        );
    }

    #[test]
    fn retry_codes_502_408_and_429_match_their_own_errors_only() {
        // ~keep Regression: [502, 504, 408] passed validate() but retried on nothing,
        // because the matcher never mapped those codes to a CrawlError variant at all.
        let cases: [(u16, CrawlError); 3] = [
            (502, CrawlError::bad_gateway("bad gateway")),
            (408, CrawlError::timeout("timeout")),
            (429, CrawlError::rate_limited("slow down")),
        ];

        for (code, error) in &cases {
            assert!(
                should_retry_status(error, &[*code]),
                "{code} must retry its own error, got no retry for {error:?}"
            );
            for (other, _) in &cases {
                if other != code {
                    assert!(
                        !should_retry_status(error, &[*other]),
                        "{error:?} must NOT retry when only {other} is configured"
                    );
                }
            }
        }
    }

    #[test]
    fn retry_codes_never_retry_an_unconfigured_or_unrelated_error() {
        assert!(
            !should_retry_status(&server_error("internal"), &[]),
            "an empty retry_codes list must never retry"
        );
        assert!(
            !should_retry_status(&CrawlError::not_found("missing"), &[500, 502, 503, 504, 408, 429]),
            "a 404 must not retry even with every retryable code configured"
        );
    }
}
