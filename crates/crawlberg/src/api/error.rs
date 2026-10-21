//! API error handling.

use crate::error::CrawlError;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::types::ApiResponse;

/// API-specific error wrapper.
#[derive(Debug)]
pub struct ApiError {
    /// HTTP status code.
    pub status: StatusCode,
    /// Machine-readable error code.
    pub code: &'static str,
    /// Human-readable error message.
    pub message: String,
    /// WAF vendor associated with this error.
    pub vendor: Option<String>,
    /// Original upstream status associated with a WAF block.
    pub origin_status: Option<u16>,
    /// Bounded prefix of an upstream block page.
    pub body_prefix: Option<String>,
}

impl ApiError {
    /// Create a new API error from a status code, error code, and message.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            vendor: None,
            origin_status: None,
            body_prefix: None,
        }
    }

    /// Create a 400 Bad Request error.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }

    /// Create a 404 Not Found error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }

    /// Create a 401 Unauthorized error, for requests missing or presenting an
    /// invalid bearer token when authentication is enabled.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", message)
    }

    /// Create a 429 Too Many Requests error, for requests rejected by a
    /// server-side resource ceiling (e.g. the concurrent job limit).
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_REQUESTS", message)
    }

    /// Create a 500 Internal Server Error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = ApiResponse::<()>::err(self.code, &self.message);
        if let Some(error) = body.error.as_mut() {
            error.vendor = self.vendor;
            error.status = self.origin_status;
            error.body_prefix = self.body_prefix;
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<CrawlError> for ApiError {
    fn from(error: CrawlError) -> Self {
        let (status, code) = match &error {
            CrawlError::NotFound { .. } => (StatusCode::NOT_FOUND, "NOT_FOUND"),
            CrawlError::Unauthorized { .. } => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED"),
            CrawlError::Forbidden { .. } => (StatusCode::FORBIDDEN, "FORBIDDEN"),
            CrawlError::WafBlocked { status, .. } => (
                status
                    .filter(|status| (400..600).contains(status))
                    .and_then(|status| StatusCode::from_u16(status).ok())
                    .unwrap_or(StatusCode::FORBIDDEN),
                "WAF_BLOCKED",
            ),
            CrawlError::Timeout { .. } | CrawlError::BrowserTimeout { .. } => (StatusCode::GATEWAY_TIMEOUT, "TIMEOUT"),
            CrawlError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
            CrawlError::ServerError { .. } => (StatusCode::BAD_GATEWAY, "SERVER_ERROR"),
            CrawlError::BadGateway { .. } => (StatusCode::BAD_GATEWAY, "BAD_GATEWAY"),
            CrawlError::TooManyRedirects { .. } => (StatusCode::BAD_GATEWAY, "TOO_MANY_REDIRECTS"),
            CrawlError::Gone { .. } => (StatusCode::GONE, "GONE"),
            CrawlError::InvalidConfig { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_CONFIG"),
            CrawlError::Connection { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "CONNECTION_ERROR"),
            CrawlError::Dns { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "DNS_ERROR"),
            CrawlError::Ssl { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "SSL_ERROR"),
            CrawlError::DataLoss { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "DATA_LOSS"),
            CrawlError::ConversionFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "CONVERSION_FAILED"),
            CrawlError::BrowserError { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "BROWSER_ERROR"),
            CrawlError::Unsupported { .. } => (StatusCode::NOT_IMPLEMENTED, "UNSUPPORTED"),
            CrawlError::SsrfPolicyViolation { .. } => (StatusCode::FORBIDDEN, "SSRF_POLICY_VIOLATION"),
            CrawlError::Other { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"),
        };

        let message = error.to_string();
        let mut result = Self::new(status, code, message);
        if let CrawlError::WafBlocked {
            vendor, status, body, ..
        } = error
        {
            result.vendor = Some(vendor);
            result.origin_status = status;
            result.body_prefix = body;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_exhaustion_is_an_upstream_failure_not_an_ssrf_refusal() {
        let error = ApiError::from(CrawlError::too_many_redirects("redirect loop"));
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "TOO_MANY_REDIRECTS");
        assert_eq!(error.message, "too_many_redirects: redirect loop");
    }

    #[test]
    fn a_failed_conversion_has_its_own_code_and_keeps_its_text() {
        let error = CrawlError::conversion_failed("could not convert https://example.com/ to Markdown: refused");
        let text = error.to_string();

        let api_error = ApiError::from(error);

        assert_eq!(api_error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(api_error.code, "CONVERSION_FAILED");
        assert_eq!(api_error.message, text);
    }
    #[tokio::test]
    async fn waf_response_preserves_origin_status_and_structured_details() {
        for status in [200_u16, 403, 429, 503] {
            let error = ApiError::from(CrawlError::waf_blocked_response(
                "datadome",
                "blocked",
                status,
                Some("challenge body"),
            ));
            let response = error.into_response();
            assert_eq!(response.status().as_u16(), if status == 200 { 403 } else { status });
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .expect("response body must be readable");
            let body: serde_json::Value = serde_json::from_slice(&bytes).expect("response must be JSON");
            assert_eq!(body["error"]["code"], "WAF_BLOCKED");
            assert_eq!(body["error"]["vendor"], "datadome");
            assert_eq!(body["error"]["status"], status);
            assert_eq!(body["error"]["body_prefix"], "challenge body");
        }
    }
}
