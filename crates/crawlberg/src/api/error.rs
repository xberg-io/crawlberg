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
}

impl ApiError {
    /// Create a new API error from a status code, error code, and message.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
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
        let body = ApiResponse::<()>::err(self.code, &self.message);
        (self.status, Json(body)).into_response()
    }
}

impl From<CrawlError> for ApiError {
    fn from(error: CrawlError) -> Self {
        let (status, code) = match &error {
            CrawlError::NotFound { .. } => (StatusCode::NOT_FOUND, "NOT_FOUND"),
            CrawlError::Unauthorized { .. } => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED"),
            CrawlError::Forbidden { .. } => (StatusCode::FORBIDDEN, "FORBIDDEN"),
            CrawlError::WafBlocked { .. } => (StatusCode::FORBIDDEN, "WAF_BLOCKED"),
            CrawlError::Timeout { .. } | CrawlError::BrowserTimeout { .. } => (StatusCode::GATEWAY_TIMEOUT, "TIMEOUT"),
            CrawlError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
            CrawlError::ServerError { .. } => (StatusCode::BAD_GATEWAY, "SERVER_ERROR"),
            CrawlError::BadGateway { .. } => (StatusCode::BAD_GATEWAY, "BAD_GATEWAY"),
            CrawlError::Gone { .. } => (StatusCode::GONE, "GONE"),
            CrawlError::InvalidConfig { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_CONFIG"),
            CrawlError::Connection { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "CONNECTION_ERROR"),
            CrawlError::Dns { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "DNS_ERROR"),
            CrawlError::Ssl { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "SSL_ERROR"),
            CrawlError::DataLoss { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "DATA_LOSS"),
            CrawlError::BrowserError { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "BROWSER_ERROR"),
            CrawlError::Unsupported { .. } => (StatusCode::NOT_IMPLEMENTED, "UNSUPPORTED"),
            CrawlError::SsrfPolicyViolation { .. } => (StatusCode::FORBIDDEN, "SSRF_POLICY_VIOLATION"),
            CrawlError::Other { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"),
        };

        Self {
            status,
            code,
            message: error.to_string(),
        }
    }
}
