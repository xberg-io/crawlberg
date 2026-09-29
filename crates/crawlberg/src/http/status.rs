//! The one mapping from an HTTP response status to the error that ends the fetch.

use crate::error::CrawlError;

/// The status of the response an error was raised for, kept as that error's source.
///
/// ~keep `retry_codes` is matched against this, never against an error's variant or
/// message: `CrawlError::timeout` also reports timeouts that never saw a response, so only
/// an error built by [`status_error`] can say which status it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("HTTP status {0}")]
pub(crate) struct HttpStatus(pub(crate) u16);

/// The error a response status ends the fetch with, or `None` when the status is not an
/// error by itself. 403 is not here: telling a WAF block from a plain 403 needs the body.
pub(crate) fn status_error(status: u16, url: &str) -> Option<CrawlError> {
    let source = HttpStatus(status);
    Some(match status {
        401 => CrawlError::unauthorized_with_source("unauthorized", source),
        404 => CrawlError::not_found_with_source(format!("not_found: {url}"), source),
        408 => CrawlError::timeout_with_source("timeout", source),
        410 => CrawlError::gone_with_source("gone", source),
        429 => CrawlError::rate_limited_with_source("rate_limited", source),
        500 => CrawlError::server_error_with_source("server_error", source),
        502 => CrawlError::bad_gateway_with_source("bad_gateway", source),
        503 => CrawlError::server_error_with_source("service unavailable", source),
        504 => CrawlError::server_error_with_source("gateway timeout", source),
        _ => return None,
    })
}

/// The status of the response `error` was raised for, or `None` when no response caused it.
pub(crate) fn error_status(error: &CrawlError) -> Option<u16> {
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(HttpStatus(status)) = current.downcast_ref::<HttpStatus>() {
            return Some(*status);
        }
        cause = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_error_status_maps_to_its_own_variant() {
        let tag = |error: &CrawlError| match error {
            CrawlError::Unauthorized { .. } => "unauthorized",
            CrawlError::NotFound { .. } => "not_found",
            CrawlError::Timeout { .. } => "timeout",
            CrawlError::Gone { .. } => "gone",
            CrawlError::RateLimited { .. } => "rate_limited",
            CrawlError::ServerError { .. } => "server_error",
            CrawlError::BadGateway { .. } => "bad_gateway",
            other => panic!("unexpected error variant: {other:?}"),
        };
        let expected = [
            (401, "unauthorized"),
            (404, "not_found"),
            (408, "timeout"),
            (410, "gone"),
            (429, "rate_limited"),
            (500, "server_error"),
            (502, "bad_gateway"),
            (503, "server_error"),
            (504, "server_error"),
        ];
        for (status, variant) in expected {
            let error = status_error(status, "https://example.com/x").expect("an error status");
            assert_eq!(tag(&error), variant, "status {status} mapped to {error:?}");
        }
    }

    #[test]
    fn a_404_names_the_requested_url() {
        let error = status_error(404, "https://example.com/missing").expect("404 is an error");
        assert!(
            matches!(&error, CrawlError::NotFound { message, .. } if message == "not_found: https://example.com/missing"),
            "got {error:?}"
        );
    }

    #[test]
    fn every_status_error_carries_the_status_it_was_built_for() {
        for status in [401_u16, 404, 408, 410, 429, 500, 502, 503, 504] {
            let error = status_error(status, "https://example.com/x").expect("an error status");
            assert_eq!(error_status(&error), Some(status), "{error:?}");
        }
    }

    #[test]
    fn a_status_that_is_not_an_error_by_itself_maps_to_nothing() {
        for status in [200_u16, 204, 301, 400, 403, 405, 418, 451, 501, 520] {
            assert!(status_error(status, "https://example.com/x").is_none(), "{status}");
        }
    }

    #[test]
    fn an_error_built_without_a_response_has_no_status() {
        let transport_timeout =
            CrawlError::timeout_with_source("[network:timeout] slow", std::io::Error::other("slow"));
        for error in [
            CrawlError::timeout("operation timed out"),
            transport_timeout,
            CrawlError::server_error("service unavailable"),
        ] {
            assert_eq!(error_status(&error), None, "{error:?}");
        }
    }
}
