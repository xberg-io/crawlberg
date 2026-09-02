//! Error types for the crawlberg crate.

use std::sync::Arc;

use thiserror::Error;

/// A cloneable, downcastable wrapper around the error a [`CrawlError`] was built from.
///
/// ~keep `Arc<dyn Error>`, not `Box<dyn Error>`, because [`CrawlError`] derives `Clone` and
/// that derive is load-bearing — `engine/mod.rs` clones an error into `AttemptOutcome` on
/// every tier-escalation retry. `Box<dyn Error>` cannot derive `Clone`, and neither
/// `reqwest::Error` nor `SsrfError` is `Clone`, so an owned concrete source would break
/// both. `Arc<T: ?Sized>` is `Clone` regardless of `T`.
///
/// ~keep A single trait object rather than a concrete type per variant: `Other`,
/// `BrowserError`, and `InvalidConfig` each wrap several unrelated error types
/// (`io::Error`, `regex::Error`, `url::ParseError`, `serde_json::Error`, chromiumoxide's).
///
/// ~keep Every `source` field is `alef(skip)`ed, so this type is Rust-only and never
/// reaches a binding. alef can only flatten a trait object to its `Display` string, and
/// that string is already in the variant's `message` — every construction site formats it
/// in (`format!("[network:{tag}] {e}")`). What would not survive the flattening is the
/// only reason the field exists: `downcast_ref::<reqwest::Error>()` on the chain, to read
/// `is_connect()`/`url()` off the original. Exporting it would add 18 duplicate fields
/// across 14 languages and cost `alef verify` 18 `lossy_sanitized_surface` errors.
#[derive(Debug, Clone)]
pub struct ErrorSource(Arc<dyn std::error::Error + Send + Sync>);

impl std::fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ErrorSource {
    /// Returns the wrapped error itself, not its source.
    ///
    /// ~keep This is what makes the original recoverable: `err.source()` yields this
    /// wrapper, and one more `.source()` yields the concrete error, so
    /// `downcast_ref::<reqwest::Error>()` works and `is_timeout()`/`is_connect()`/`url()`
    /// come back. thiserror cannot put the concrete error directly in `#[source]` here,
    /// because the field must stay `Clone` (see the type docs) and `Arc<dyn Error>` does
    /// not itself implement `Error`. The cost is one extra link whose `Display` repeats
    /// the wrapped error's message.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

impl ErrorSource {
    /// Wrap a concrete error so it can be carried in a [`CrawlError`].
    pub fn new<E: std::error::Error + Send + Sync + 'static>(error: E) -> Self {
        Self(Arc::new(error))
    }
}

/// Wrap a concrete error as an optional [`ErrorSource`].
fn source_of<E: std::error::Error + Send + Sync + 'static>(error: E) -> Option<ErrorSource> {
    Some(ErrorSource::new(error))
}

/// Generate the paired constructors for a variant carrying `message` + `source`.
///
/// ~keep Every variant needs both a sourceless and a sourced constructor, and writing 34
/// near-identical functions by hand invites one of them to drift. The enum itself is
/// deliberately *not* macro-generated: its `#[error(...)]` strings are a stable public
/// contract that must stay greppable in the source.
macro_rules! message_constructors {
    ($( $variant:ident => ($plain:ident, $with_source:ident) ),* $(,)?) => {
        impl CrawlError {
            $(
                #[doc = concat!("Build a [`CrawlError::", stringify!($variant), "`] with no underlying error.")]
                pub fn $plain(message: impl Into<String>) -> Self {
                    Self::$variant { message: message.into(), source: None }
                }

                #[doc = concat!("Build a [`CrawlError::", stringify!($variant), "`] preserving `source` in the error chain.")]
                pub fn $with_source(
                    message: impl Into<String>,
                    source: impl std::error::Error + Send + Sync + 'static,
                ) -> Self {
                    Self::$variant { message: message.into(), source: source_of(source) }
                }
            )*
        }
    };
}

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
    #[error("not_found: {message}")]
    NotFound {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The request was unauthorized (HTTP 401).
    #[error("unauthorized: {message}")]
    Unauthorized {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The request was forbidden (HTTP 403).
    #[error("forbidden: {message}")]
    Forbidden {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
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
    #[error("timeout: {message}")]
    Timeout {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The request was rate-limited (HTTP 429).
    #[error("rate_limited: {message}")]
    RateLimited {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// A server error occurred (HTTP 5xx).
    #[error("server_error: {message}")]
    ServerError {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// A bad gateway error occurred (HTTP 502).
    #[error("bad_gateway: {message}")]
    BadGateway {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The resource is permanently gone (HTTP 410).
    #[error("gone: {message}")]
    Gone {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// A connection error occurred.
    #[error("connection: {message}")]
    Connection {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// A DNS resolution error occurred.
    #[error("dns: {message}")]
    Dns {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// An SSL/TLS error occurred.
    #[error("ssl: {message}")]
    Ssl {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// Data was lost or truncated during transfer.
    #[error("data_loss: {message}")]
    DataLoss {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The browser failed to launch, connect, or navigate.
    #[error("browser: {message}")]
    BrowserError {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The browser page load or rendering timed out.
    #[error("browser_timeout: {message}")]
    BrowserTimeout {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The provided configuration is invalid.
    #[error("invalid_config: {message}")]
    InvalidConfig {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// The requested capability is not supported by the active backend or build.
    #[error("unsupported: {message}")]
    Unsupported {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
    /// A URL was rejected by SSRF policy (private IP, metadata, disallowed scheme, etc).
    #[error("ssrf_policy_violation: {url} - {reason}")]
    SsrfPolicyViolation {
        /// The URL that was refused by the policy.
        url: String,
        /// Reason for rejection (e.g., "loopback", "private_network", "disallowed_scheme: ftp").
        reason: String,
        /// The policy error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<Arc<crate::net::ssrf::SsrfError>>,
    },
    /// An unclassified error occurred.
    #[error("other: {message}")]
    Other {
        /// Human-readable description of the failure.
        message: String,
        /// The error this was built from, when one was available.
        #[cfg_attr(alef, alef(skip))]
        #[source]
        source: Option<ErrorSource>,
    },
}

message_constructors! {
    NotFound => (not_found, not_found_with_source),
    Unauthorized => (unauthorized, unauthorized_with_source),
    Forbidden => (forbidden, forbidden_with_source),
    Timeout => (timeout, timeout_with_source),
    RateLimited => (rate_limited, rate_limited_with_source),
    ServerError => (server_error, server_error_with_source),
    BadGateway => (bad_gateway, bad_gateway_with_source),
    Gone => (gone, gone_with_source),
    Connection => (connection, connection_with_source),
    Dns => (dns, dns_with_source),
    Ssl => (ssl, ssl_with_source),
    DataLoss => (data_loss, data_loss_with_source),
    BrowserError => (browser_error, browser_error_with_source),
    BrowserTimeout => (browser_timeout, browser_timeout_with_source),
    InvalidConfig => (invalid_config, invalid_config_with_source),
    Unsupported => (unsupported, unsupported_with_source),
    Other => (other, other_with_source),
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

impl CrawlError {
    /// Build an [`CrawlError::SsrfPolicyViolation`] with `url` credential-redacted.
    ///
    /// ~keep Always construct this variant here rather than with a struct literal. A
    /// refused URL frequently carries userinfo (`http://user:pass@host/`), and this
    /// value reaches API error bodies, MCP error payloads and tracing fields — so a
    /// literal leaks the credential at exactly the moment the request was rejected.
    pub(crate) fn ssrf_violation(url: impl AsRef<str>, reason: impl Into<String>) -> Self {
        Self::SsrfPolicyViolation {
            url: crate::net::redact_url_credentials(url.as_ref()),
            reason: reason.into(),
            source: None,
        }
    }
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
            source: Some(Arc::new(err.source)),
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
            source: Some(Arc::new(err)),
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
///
/// ~keep The scan runs over the chain with the request URL removed. Every keyword below
/// (`dns`, `ssl`, `timeout`, `certificate`, `proxy`, `connect`, ...) is a word a URL path or
/// hostname can spell, and `reqwest::Error`'s `Display` embeds the request URL — so scanning
/// the raw chain let the page being fetched pick its own error class.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn network_error_kind(e: &reqwest::Error) -> NetworkErrorKind {
    let chain = chain_without_request_url(e, &error_chain_string(e));
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
///
/// ~keep With no structural predicates available here, stripping the request URL from the
/// chain is the only thing keeping the fetched path out of the classification.
#[cfg(target_arch = "wasm32")]
pub(crate) fn network_error_kind(e: &reqwest::Error) -> NetworkErrorKind {
    let chain = chain_without_request_url(e, &error_chain_string(e));
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
pub(crate) fn classify_reqwest_error(e: reqwest::Error) -> CrawlError {
    let chain = error_chain_string(&e);
    let kind = network_error_kind(&e);
    let tag = kind.tag();
    match kind {
        NetworkErrorKind::Timeout => CrawlError::timeout_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Dns => CrawlError::dns_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Ssl => CrawlError::ssl_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Proxy => CrawlError::connection_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Connection | NetworkErrorKind::Other if is_body_data_loss(&e, &chain) => {
            CrawlError::data_loss_with_source(format!("data_loss: {e}"), e)
        }
        NetworkErrorKind::Connection => CrawlError::connection_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Other => CrawlError::other_with_source(format!("[network:{tag}] {e}"), e),
    }
}

/// The error chain with the request URL removed, for keyword matching.
///
/// ~keep `reqwest::Error`'s own `Display` embeds the request URL, so the raw chain matches
/// keywords that came from the path being fetched rather than from the failure. Scraping
/// `/blog/dns-explained` must not classify as a DNS error.
fn chain_without_request_url(e: &reqwest::Error, chain: &str) -> String {
    match e.url() {
        Some(url) => chain.replace(&url.to_string().to_lowercase(), ""),
        None => chain.to_string(),
    }
}

/// Whether a reqwest error describes a body that arrived truncated or undecodable.
///
/// ~keep hyper renders the underlying `IncompleteMessage` as "connection closed before
/// message completed", so the generic `contains("connection")` arm of `network_error_kind`
/// claims every truncated-body error first. Checking this predicate for `Connection` as well
/// as `Other` is what keeps `CrawlError::DataLoss` reachable for the case it exists to name.
#[cfg(not(target_arch = "wasm32"))]
fn is_body_data_loss(e: &reqwest::Error, chain: &str) -> bool {
    if e.is_body() {
        return true;
    }
    let chain = chain_without_request_url(e, chain);
    chain.contains("content-length")
        || chain.contains("truncate")
        || chain.contains("incomplete")
        || chain.contains("decoding response body")
        || chain.contains("error decoding")
}

/// Classify a `reqwest::Error` into the appropriate `CrawlError` variant (wasm fallback).
///
/// The error message is prefixed with `[network:<kind>]` so that cross-language
/// e2e fixtures can assert on stable substrings regardless of the native error
/// message format each binding produces.
#[cfg(target_arch = "wasm32")]
pub(crate) fn classify_reqwest_error(e: reqwest::Error) -> CrawlError {
    let chain = error_chain_string(&e);
    let kind = network_error_kind(&e);
    let tag = kind.tag();
    match kind {
        NetworkErrorKind::Timeout => CrawlError::timeout_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Dns => CrawlError::dns_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Ssl => CrawlError::ssl_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Proxy => CrawlError::connection_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Connection | NetworkErrorKind::Other if is_body_data_loss(&e, &chain) => {
            CrawlError::data_loss_with_source(format!("data_loss: {e}"), e)
        }
        NetworkErrorKind::Connection => CrawlError::connection_with_source(format!("[network:{tag}] {e}"), e),
        NetworkErrorKind::Other => CrawlError::other_with_source(format!("[network:{tag}] {e}"), e),
    }
}

/// Whether a reqwest error describes a body that arrived truncated or undecodable (wasm).
///
/// ~keep wasm32 reqwest exposes no `is_body()`, so this relies on the source chain alone.
/// The `Connection` arm is checked for the same reason as the native build: the truncated
/// body surfaces as a closed connection and would otherwise never reach `DataLoss`.
#[cfg(target_arch = "wasm32")]
fn is_body_data_loss(e: &reqwest::Error, chain: &str) -> bool {
    let chain = chain_without_request_url(e, chain);
    chain.contains("content-length") || chain.contains("truncate") || chain.contains("incomplete")
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
            CrawlError::SsrfPolicyViolation { url, reason, .. } => {
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
            CrawlError::SsrfPolicyViolation {
                url: reported, reason, ..
            } => {
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
            classify_reqwest_error(client.get(url).send().await.expect_err("expected network error"))
        }

        /// The whole point of the `#[source]` wiring: the original `reqwest::Error` must
        /// come back out, not just its rendered message.
        ///
        /// ~keep This is the test that distinguishes the real change from a shape-only
        /// refactor. Converting the variants to carry a `source` field compiles, passes
        /// every pre-existing test, and still returns `None` from `source()` if no
        /// construction site actually attaches anything — which is exactly the state the
        /// migration passed through on its way here.
        #[tokio::test]
        async fn a_network_error_keeps_the_reqwest_error_recoverable() {
            use std::error::Error as _;

            let err = scrape_url("http://127.0.0.1:1/").await;

            let source = err.source().expect("a classified network error must expose its source");
            let original = source
                .source()
                .expect("the wrapper must expose the concrete error beneath it");
            let reqwest_error = original
                .downcast_ref::<reqwest::Error>()
                .expect("the concrete source must downcast back to reqwest::Error");

            assert!(
                reqwest_error.is_connect(),
                "recovering is_connect() is the capability this wiring exists to restore, got {reqwest_error:?}"
            );
            assert!(
                reqwest_error.url().is_some(),
                "the failing URL must be recoverable from the original error"
            );
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

        /// A body truncated relative to its declared `content-length` must classify as
        /// `DataLoss`, and the rendered message must carry the `data_loss:` prefix.
        ///
        /// ~keep This is the assertion string the `error_data_loss_truncated` e2e fixture
        /// checks for. The fixture's own id contains the bare substring `data_loss`, and the
        /// classified message embeds the request URL, so `contains("data_loss")` matched the
        /// URL rather than the classification. `data_loss:` cannot occur in a URL path
        /// segment, which is why the fixture asserts the prefix and why this test pins it.
        #[tokio::test]
        async fn truncated_body_produces_data_loss_prefix() {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                if let Ok((mut socket, _)) = listener.accept().await {
                    let mut discard = [0_u8; 1024];
                    let _ = socket.read(&mut discard).await;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 500000\r\n\r\n<html><body>Incomplete content",
                        )
                        .await;
                    let _ = socket.flush().await;
                }
            });

            let url = format!("http://{addr}/");
            assert!(
                !url.contains("data_loss"),
                "the probe URL must not contain the asserted substring, got '{url}'"
            );

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client build must not fail");
            let response = client.get(&url).send().await.expect("response headers must arrive");
            let raw_err = response.text().await.expect_err("truncated body read must fail");
            let msg = classify_reqwest_error(raw_err).to_string();

            assert!(msg.contains("data_loss:"), "expected 'data_loss:' in '{msg}'");
        }

        /// Every keyword the classifier scans for must be inert when it comes from the URL.
        ///
        /// ~keep `reqwest::Error`'s `Display` embeds the request URL, so before the chain was
        /// stripped, scraping `/blog/dns-explained` reported a DNS failure and
        /// `/fixtures/error_invalid_proxy` reported a proxy failure — both were plain refused
        /// TCP connections. This is table-driven because the exposure is per keyword: fixing
        /// one arm of `network_error_kind` and leaving the rest is the failure mode.
        #[tokio::test]
        async fn no_url_keyword_can_pick_the_network_error_kind() {
            for keyword_path in [
                "/blog/dns-explained",
                "/blog/how-to-resolve-a-hostname",
                "/docs/lookup-tables",
                "/blog/ssl-explained",
                "/reference/tls-primer",
                "/blog/certificate-pinning",
                "/guide/tcp-handshake",
                "/blog/timeout-tuning",
                "/fixtures/error_invalid_proxy",
                "/help/proxy-setup",
            ] {
                let url = format!("http://127.0.0.1:1{keyword_path}");
                let err = scrape_url(&url).await;
                let msg = err.to_string();
                assert!(
                    msg.starts_with("connection: [network:connection]"),
                    "a refused TCP connection to '{url}' must classify as a connection error, got '{msg}'"
                );
            }
        }

        /// A proxy that refuses the connection is a connection error, not a proxy error.
        ///
        /// ~keep A refused proxy CONNECT renders as `tcp connect error | connection refused`;
        /// nothing in the chain says "proxy". The `[network:proxy]` tag this used to carry came
        /// entirely from the word "proxy" in the request URL, which is why the assertion below
        /// is now a single expected tag rather than a disjunction that either arm could satisfy.
        #[tokio::test]
        async fn a_refused_proxy_is_tagged_as_a_connection_error() {
            let client = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:1").expect("proxy parse"))
                .timeout(Duration::from_millis(500))
                .build()
                .expect("client build");
            let raw_err = client
                .get("http://example.invalid/fixtures/error_invalid_proxy")
                .send()
                .await
                .expect_err("expected proxy error");
            let msg = classify_reqwest_error(raw_err).to_string();
            assert!(
                msg.contains("[network:connection]"),
                "expected [network:connection] in '{msg}'"
            );
        }

        /// A request path that happens to spell a classification keyword must not decide the
        /// classification.
        ///
        /// ~keep `reqwest::Error`'s `Display` embeds the request URL, so the keyword scan ran
        /// over the path being fetched. This URL is a plain connection refusal; only its path
        /// says "truncated".
        #[tokio::test]
        async fn a_url_spelling_truncated_is_not_a_data_loss() {
            let err = scrape_url("http://127.0.0.1:1/fixtures/error_data_loss_truncated").await;
            let msg = err.to_string();
            assert!(
                !msg.starts_with("data_loss:"),
                "a refused connection must not classify as data loss, got '{msg}'"
            );
            assert!(
                msg.contains("[network:connection]"),
                "expected [network:connection] in '{msg}'"
            );
        }

        /// A response that reqwest cannot classify as timeout, DNS, SSL, proxy, or
        /// connection falls to `NetworkErrorKind::Other`, whose rendered message must not
        /// double the `Other` variant's own `"other: {message}"` prefix.
        ///
        /// ~keep Regression test for the `other: other: ...` bug: `classify_reqwest_error`
        /// built the `Other` arm's message as `format!("other: {e}")` and handed it to
        /// `CrawlError::other_with_source`, whose `Display` is itself `"other: {message}"` —
        /// doubling the prefix and, unlike every sibling arm, omitting the `[network:{tag}]`
        /// marker the e2e fixtures match on. An invalid HTTP response (garbled status line)
        /// is a reliable way to reach this arm: it trips none of the DNS/SSL/proxy/connect/
        /// timeout keywords and `is_connect()`/`is_timeout()`/`is_body()` are all false.
        #[tokio::test]
        async fn unclassifiable_response_has_single_other_prefix_and_network_tag() {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt as _;
                if let Ok((mut socket, _)) = listener.accept().await {
                    let _ = socket.write_all(b"not even close to http\r\n\r\n").await;
                    let _ = socket.flush().await;
                    let _ = socket.shutdown().await;
                }
            });
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("client build must not fail");
            let raw_err = client
                .get(format!("http://{addr}/"))
                .send()
                .await
                .expect_err("garbage response must fail");
            assert!(!raw_err.is_connect() && !raw_err.is_timeout() && !raw_err.is_body());

            let classified = classify_reqwest_error(raw_err);
            assert!(
                matches!(classified, CrawlError::Other { .. }),
                "expected an unclassifiable response to fall to CrawlError::Other, got {classified:?}"
            );
            let msg = classified.to_string();
            assert!(
                msg.starts_with("other: [network:network] "),
                "expected a single 'other: ' prefix followed by the [network:network] tag, got '{msg}'"
            );
            assert!(
                !msg.contains("other: other:"),
                "the 'other: {{message}}' Display must not be doubled, got '{msg}'"
            );
        }
    }
}
