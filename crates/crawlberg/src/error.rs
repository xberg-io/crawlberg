//! Error types for the crawlberg crate.

use thiserror::Error;

/// Stable, language-agnostic classification for network-level errors.
///
/// The [`tag`](NetworkErrorKind::tag) method returns a lowercase ASCII string
/// that is stable across all language bindings. Cross-language e2e fixtures
/// assert that the error message contains the corresponding tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetworkErrorKind {
    /// TCP connection attempt refused or unreachable.
    Connection,
    /// DNS name resolution failed.
    Dns,
    /// TLS/SSL handshake or certificate error.
    Ssl,
    /// Request exceeded the configured deadline.
    Timeout,
    /// Error communicating with or configuring a proxy.
    Proxy,
    /// Unclassified network error.
    Other,
}

impl NetworkErrorKind {
    /// Returns the stable, lowercase tag string embedded in error messages.
    ///
    /// Each tag is a fixed ASCII keyword: `"connection"`, `"dns"`, `"ssl"`,
    /// `"timeout"`, `"proxy"`, or `"network"`. Cross-language e2e fixtures
    /// assert that `error.to_string()` contains this substring.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Dns => "dns",
            Self::Ssl => "ssl",
            Self::Timeout => "timeout",
            Self::Proxy => "proxy",
            Self::Other => "network",
        }
    }
}

/// Errors that can occur during crawling, scraping, or mapping operations.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum CrawlError {
    /// The requested page was not found (HTTP 404).
    #[error("not_found: {0}")]
    NotFound(String),
    /// The request was unauthorized (HTTP 401).
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The request was forbidden (HTTP 403).
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The request was blocked by a WAF or bot protection (HTTP 403 with WAF indicators).
    ///
    /// `vendor` is the lowercase identifier of the detected WAF (e.g. "cloudflare",
    /// "datadome"). When the engine cannot identify the vendor, it uses "unknown".
    /// `message` is the freeform description for logs and human readers.
    ///
    /// The stable error tag remains `forbidden: waf/blocked: MESSAGE` so existing
    /// log-grep patterns and cross-language bindings continue to work; vendor is
    /// surfaced separately for structured consumers.
    #[error("forbidden: waf/blocked: {message}")]
    WafBlocked {
        /// Lowercase WAF vendor identifier (e.g. "cloudflare").
        vendor: String,
        /// Freeform description / context for logs.
        message: String,
    },
    /// The request timed out.
    #[error("timeout: {0}")]
    Timeout(String),
    /// The request was rate-limited (HTTP 429).
    #[error("rate_limited: {0}")]
    RateLimited(String),
    /// A server error occurred (HTTP 5xx).
    #[error("server_error: {0}")]
    ServerError(String),
    /// A bad gateway error occurred (HTTP 502).
    #[error("bad_gateway: {0}")]
    BadGateway(String),
    /// The resource is permanently gone (HTTP 410).
    #[error("gone: {0}")]
    Gone(String),
    /// A connection error occurred.
    #[error("connection: {0}")]
    Connection(String),
    /// A DNS resolution error occurred.
    #[error("dns: {0}")]
    Dns(String),
    /// An SSL/TLS error occurred.
    #[error("ssl: {0}")]
    Ssl(String),
    /// Data was lost or truncated during transfer.
    #[error("data_loss: {0}")]
    DataLoss(String),
    /// The browser failed to launch, connect, or navigate.
    #[error("browser: {0}")]
    BrowserError(String),
    /// The browser page load or rendering timed out.
    #[error("browser_timeout: {0}")]
    BrowserTimeout(String),
    /// The provided configuration is invalid.
    #[error("invalid_config: {0}")]
    InvalidConfig(String),
    /// The requested capability is not supported by the active backend or build.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A URL was rejected by SSRF policy (private IP, metadata, disallowed scheme, etc).
    #[error("ssrf_policy_violation: {url} - {reason}")]
    SsrfPolicyViolation {
        /// The URL that was refused by the policy.
        url: String,
        /// Reason for rejection (e.g., "loopback", "private_network", "disallowed_scheme: ftp").
        reason: String,
    },
    /// An unclassified error occurred.
    #[error("other: {0}")]
    Other(String),
}

/// Pairs an [`crate::net::ssrf::SsrfError`] with the URL that was being validated when it
/// occurred, so the pairing survives `?` into a [`CrawlError`] that names the refused URL.
///
/// [`crate::net::ssrf::SsrfError`] never carries a URL on its own — `validate_url` takes
/// it as a parameter rather than embedding it in every variant — so the blanket
/// `From<SsrfError> for CrawlError` below has no URL to report and falls back to
/// `"unknown"`. Attach one with [`crate::net::ssrf::SsrfError::with_url`] instead:
///
/// ```
/// # async fn example(url: url::Url, policy: crawlberg::SsrfPolicy) -> Result<(), crawlberg::CrawlError> {
/// crawlberg::validate_url(&url, &policy)
///     .await
///     .map_err(|e| e.with_url(&url))?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Error)]
#[error("{source}")]
pub struct UrlSsrfError {
    /// The URL that was being validated when `source` occurred. Credentials embedded in
    /// the URL's userinfo (`user:pass@host`) are redacted before storage.
    pub url: String,
    /// The underlying SSRF policy error.
    #[source]
    pub source: crate::net::ssrf::SsrfError,
}

impl crate::net::ssrf::SsrfError {
    /// Attach the URL that was being validated, producing an error that survives `?`
    /// into [`CrawlError::SsrfPolicyViolation`] with `url` populated instead of
    /// `"unknown"`. Any userinfo credentials in `url` are redacted, since this value may
    /// flow into logs and error reports.
    #[must_use]
    pub fn with_url(self, url: &url::Url) -> UrlSsrfError {
        UrlSsrfError {
            url: crate::net::redact_url_credentials(url.as_str()),
            source: self,
        }
    }
}

impl From<UrlSsrfError> for CrawlError {
    fn from(err: UrlSsrfError) -> Self {
        CrawlError::SsrfPolicyViolation {
            url: err.url,
            reason: err.source.to_string(),
        }
    }
}

/// Converts an [`crate::net::ssrf::SsrfError`] with no URL context available.
///
/// Prefer [`crate::net::ssrf::SsrfError::with_url`] wherever the URL that was being
/// validated is in scope — every call site inside this crate does. This impl exists so
/// `SsrfError` still converts via a bare `?` for callers that genuinely have no URL to
/// attach; `url` is reported as the literal string `"unknown"` in that case.
impl From<crate::net::ssrf::SsrfError> for CrawlError {
    fn from(err: crate::net::ssrf::SsrfError) -> Self {
        CrawlError::SsrfPolicyViolation {
            url: "unknown".to_string(),
            reason: err.to_string(),
        }
    }
}

/// Collect the full error source chain into a single lowercase string for keyword matching.
pub(crate) fn error_chain_string(e: &reqwest::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut current: &dyn std::error::Error = e;
    while let Some(src) = current.source() {
        parts.push(src.to_string());
        current = src;
    }
    parts.join(" | ").to_lowercase()
}

/// Determine the [`NetworkErrorKind`] for a reqwest error (non-wasm).
///
/// Walks the full source chain to detect DNS, SSL/TLS, timeout, and connection
/// errors that reqwest may wrap inside generic connect errors.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn network_error_kind(e: &reqwest::Error) -> NetworkErrorKind {
    let chain = error_chain_string(e);
    if e.is_timeout() || chain.contains("timed out") || chain.contains("timeout") {
        NetworkErrorKind::Timeout
    } else if chain.contains("dns") || chain.contains("resolve") || chain.contains("lookup") {
        NetworkErrorKind::Dns
    } else if chain.contains("ssl")
        || chain.contains("tls")
        || chain.contains("certificate")
        || chain.contains("record overflow")
        || chain.contains("handshake")
        || chain.contains("corrupt message")
        || chain.contains("alertdescription")
        || chain.contains("invalidcontenttype")
    {
        NetworkErrorKind::Ssl
    } else if chain.contains("proxy") {
        NetworkErrorKind::Proxy
    } else if e.is_connect() || chain.contains("connection") || chain.contains("connect") {
        NetworkErrorKind::Connection
    } else {
        NetworkErrorKind::Other
    }
}

/// Determine the [`NetworkErrorKind`] for a reqwest error (wasm fallback).
///
/// On wasm32, reqwest does not expose `.is_timeout()`, `.is_connect()`, or `.is_body()`
/// methods, so we rely solely on the error chain string for classification.
#[cfg(target_arch = "wasm32")]
pub(crate) fn network_error_kind(e: &reqwest::Error) -> NetworkErrorKind {
    let chain = error_chain_string(e);
    if chain.contains("timed out") || chain.contains("timeout") {
        NetworkErrorKind::Timeout
    } else if chain.contains("dns") || chain.contains("resolve") || chain.contains("lookup") {
        NetworkErrorKind::Dns
    } else if chain.contains("ssl")
        || chain.contains("tls")
        || chain.contains("certificate")
        || chain.contains("handshake")
    {
        NetworkErrorKind::Ssl
    } else if chain.contains("proxy") {
        NetworkErrorKind::Proxy
    } else if chain.contains("connection") || chain.contains("connect") {
        NetworkErrorKind::Connection
    } else {
        NetworkErrorKind::Other
    }
}

/// Classify a `reqwest::Error` into the appropriate `CrawlError` variant (non-wasm).
///
/// The error message is prefixed with `[network:<kind>]` so that cross-language
/// e2e fixtures can assert on stable substrings regardless of the native error
/// message format each binding produces.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn classify_reqwest_error(e: &reqwest::Error) -> CrawlError {
    let chain = error_chain_string(e);
    let kind = network_error_kind(e);
    let tag = kind.tag();
    match kind {
        NetworkErrorKind::Timeout => CrawlError::Timeout(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Dns => CrawlError::Dns(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Ssl => CrawlError::Ssl(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Proxy | NetworkErrorKind::Connection => {
            CrawlError::Connection(format!("[network:{tag}] {e}"))
        }
        NetworkErrorKind::Other => {
            if e.is_body()
                || chain.contains("content-length")
                || chain.contains("truncate")
                || chain.contains("incomplete")
                || chain.contains("decoding response body")
                || chain.contains("error decoding")
            {
                CrawlError::DataLoss(format!("data_loss: {e}"))
            } else {
                CrawlError::Other(format!("other: {e}"))
            }
        }
    }
}

/// Classify a `reqwest::Error` into the appropriate `CrawlError` variant (wasm fallback).
///
/// The error message is prefixed with `[network:<kind>]` so that cross-language
/// e2e fixtures can assert on stable substrings regardless of the native error
/// message format each binding produces.
#[cfg(target_arch = "wasm32")]
pub(crate) fn classify_reqwest_error(e: &reqwest::Error) -> CrawlError {
    let chain = error_chain_string(e);
    let kind = network_error_kind(e);
    let tag = kind.tag();
    match kind {
        NetworkErrorKind::Timeout => CrawlError::Timeout(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Dns => CrawlError::Dns(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Ssl => CrawlError::Ssl(format!("[network:{tag}] {e}")),
        NetworkErrorKind::Proxy | NetworkErrorKind::Connection => {
            CrawlError::Connection(format!("[network:{tag}] {e}"))
        }
        NetworkErrorKind::Other => {
            if chain.contains("content-length") || chain.contains("truncate") || chain.contains("incomplete") {
                CrawlError::DataLoss(format!("data_loss: {e}"))
            } else {
                CrawlError::Other(format!("other: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_kind_tag_connection() {
        assert_eq!(NetworkErrorKind::Connection.tag(), "connection");
    }

    #[test]
    fn network_error_kind_tag_dns() {
        assert_eq!(NetworkErrorKind::Dns.tag(), "dns");
    }

    #[test]
    fn network_error_kind_tag_ssl() {
        assert_eq!(NetworkErrorKind::Ssl.tag(), "ssl");
    }

    #[test]
    fn network_error_kind_tag_timeout() {
        assert_eq!(NetworkErrorKind::Timeout.tag(), "timeout");
    }

    #[test]
    fn network_error_kind_tag_proxy() {
        assert_eq!(NetworkErrorKind::Proxy.tag(), "proxy");
    }

    #[test]
    fn network_error_kind_tag_other() {
        assert_eq!(NetworkErrorKind::Other.tag(), "network");
    }

    #[test]
    fn blanket_from_ssrf_error_reports_unknown_url() {
        // ~keep Documents the pre-existing, still-supported fallback: no URL context
        // means no URL to report. This is exactly the gap `SsrfError::with_url` closes.
        let err = crate::net::ssrf::SsrfError::DeniedByPolicy { reason: "loopback" };
        let crawl_err: CrawlError = err.into();
        match crawl_err {
            CrawlError::SsrfPolicyViolation { url, reason } => {
                assert_eq!(url, "unknown", "bare `?` conversion has no URL to report, got '{url}'");
                assert_eq!(
                    reason, "denied by SSRF policy: loopback",
                    "unexpected reason: '{reason}'"
                );
            }
            other => panic!("expected SsrfPolicyViolation, got {other:?}"),
        }
    }

    #[test]
    fn ssrf_error_with_url_preserves_the_refused_url() {
        let url = "http://169.254.169.254/latest/meta-data/".parse::<url::Url>().unwrap();
        let err = crate::net::ssrf::SsrfError::DeniedByPolicy { reason: "link_local" };
        let crawl_err: CrawlError = err.with_url(&url).into();
        match crawl_err {
            CrawlError::SsrfPolicyViolation { url: reported, reason } => {
                assert_eq!(
                    reported, "http://169.254.169.254/latest/meta-data/",
                    "the refused URL must be preserved, not 'unknown'"
                );
                assert_eq!(
                    reason, "denied by SSRF policy: link_local",
                    "unexpected reason: '{reason}'"
                );
            }
            other => panic!("expected SsrfPolicyViolation, got {other:?}"),
        }
    }

    #[test]
    fn ssrf_error_with_url_redacts_embedded_proxy_credentials() {
        // ~keep A crawl target or proxy-routed URL can itself carry userinfo
        // (http://user:pass@host/); that must never reach a rendered CrawlError.
        let url = "http://svc-account:hunter2@internal.example/admin"
            .parse::<url::Url>()
            .unwrap();
        let err = crate::net::ssrf::SsrfError::DeniedByPolicy {
            reason: "private_network",
        };
        let crawl_err: CrawlError = err.with_url(&url).into();
        let rendered = crawl_err.to_string();
        assert!(
            !rendered.contains("hunter2"),
            "rendered CrawlError must not contain the raw password, got '{rendered}'"
        );
        assert!(
            !rendered.contains("svc-account:hunter2"),
            "rendered CrawlError must not contain raw userinfo, got '{rendered}'"
        );
        assert!(
            rendered.contains("internal.example"),
            "rendered CrawlError should still name the host for debugging, got '{rendered}'"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod network_integration {
        use super::*;
        use std::time::Duration;
        use tokio::net::TcpListener;

        async fn scrape_url(url: &str) -> CrawlError {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(500))
                .danger_accept_invalid_certs(true)
                .build()
                .expect("client build must not fail");
            classify_reqwest_error(&client.get(url).send().await.expect_err("expected network error"))
        }

        #[tokio::test]
        async fn connection_refused_produces_connection_tag() {
            let err = scrape_url("http://127.0.0.1:1/").await;
            let msg = err.to_string();
            assert!(
                msg.contains("[network:connection]"),
                "expected [network:connection] in '{msg}'"
            );
            assert!(msg.contains("connection"), "expected 'connection' in '{msg}'");
        }

        #[tokio::test]
        async fn dns_failure_produces_dns_tag() {
            let err = scrape_url("http://this-hostname-does-not-exist-crawlberg-test.invalid/").await;
            let msg = err.to_string();
            assert!(msg.contains("[network:dns]"), "expected [network:dns] in '{msg}'");
            assert!(msg.contains("dns"), "expected 'dns' in '{msg}'");
        }

        #[tokio::test]
        async fn timeout_produces_timeout_tag() {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                if let Ok((_socket, _)) = listener.accept().await {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });

            let err = scrape_url(&format!("http://{addr}/")).await;
            let msg = err.to_string();
            assert!(
                msg.contains("[network:timeout]"),
                "expected [network:timeout] in '{msg}'"
            );
            assert!(msg.contains("timeout"), "expected 'timeout' in '{msg}'");
        }

        #[tokio::test]
        async fn invalid_proxy_produces_connection_tag() {
            let client = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:1").expect("proxy parse"))
                .timeout(Duration::from_millis(500))
                .build()
                .expect("client build");
            let raw_err = client
                .get("http://example.com/")
                .send()
                .await
                .expect_err("expected proxy error");
            let err = classify_reqwest_error(&raw_err);
            let msg = err.to_string();
            assert!(
                msg.contains("[network:connection]") || msg.contains("[network:proxy]"),
                "expected [network:connection] or [network:proxy] in '{msg}'"
            );
            assert!(
                msg.contains("connection") || msg.contains("proxy"),
                "expected 'connection' or 'proxy' in '{msg}'"
            );
        }
    }
}
