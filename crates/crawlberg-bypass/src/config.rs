//! Provider configuration types.
//!
//! These types represent the schema loaded from per-vendor YAML files.
//! See `loader.rs` for the parsing logic and `configs/` for example files.

/// HTTP method for the vendor's extraction endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// Authentication scheme to apply to every outbound request.
#[derive(Clone)]
pub enum AuthScheme {
    /// No authentication.
    None,
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
    /// HTTP Basic Auth with the API key as the username and an empty password.
    /// Used by Zyte.
    BasicUsername { username: String },
    /// A custom header: `<name>: <value>`.
    Header { name: String, value: String },
    /// Append `?<name>=<value>` to the request URL.
    QueryParam { name: String, value: String },
}

impl std::fmt::Debug for AuthScheme {
    /// Redacted: shows which scheme is configured and whether its secret is non-empty,
    /// never the secret itself. `ProviderConfig`'s derived `Debug` prints through this.
    // ~keep `BasicUsername.username` is hidden although `crawlberg`'s `AuthConfig::Basic`
    // ~keep prints its username in clear. That is deliberate, not an inconsistency: this
    // ~keep field carries the vendor API key (Zyte sends the key as the Basic username),
    // ~keep whereas `AuthConfig::Basic.username` is an account name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = |secret: &String| (!secret.is_empty()).then_some("***");
        match self {
            Self::None => f.write_str("None"),
            Self::Bearer { token } => f.debug_struct("Bearer").field("token", &redacted(token)).finish(),
            Self::BasicUsername { username } => f
                .debug_struct("BasicUsername")
                .field("username", &redacted(username))
                .finish(),
            Self::Header { name, value } => f
                .debug_struct("Header")
                .field("name", name)
                .field("value", &redacted(value))
                .finish(),
            Self::QueryParam { name, value } => f
                .debug_struct("QueryParam")
                .field("name", name)
                .field("value", &redacted(value))
                .finish(),
        }
    }
}

/// Location where the target URL is injected into the outbound request.
#[derive(Debug, Clone)]
pub enum UrlParamLocation {
    /// Append `?<name>=<url-encoded-target>` as a query string parameter.
    QueryParam { name: String },
    /// Substitute `{{url}}` inside the JSON body template.
    BodyField,
}

/// Body shape for POST requests.
#[derive(Debug, Clone)]
pub enum RequestBody {
    /// JSON body; the literal `{{url}}` placeholder is replaced with the
    /// URL-encoded target before sending.
    Json { template: String },
}

/// Request construction parameters.
#[derive(Debug, Clone)]
pub struct RequestShape {
    /// POST body; `None` for GET requests.
    pub body: Option<RequestBody>,
    /// Fixed query parameters appended to every request (before `url_param`).
    pub query: Vec<(String, String)>,
    /// How and where the target URL is placed in the request.
    pub url_param: UrlParamLocation,
}

/// How to interpret the vendor's HTTP response body.
#[derive(Debug, Clone)]
pub enum ResponseKind {
    /// The response body is the raw HTML/content directly.
    RawBody,
    /// The response body is JSON; extract `html_field` as a top-level string.
    JsonField { html_field: String },
}

/// Unit for a cost value extracted from the response.
#[derive(Debug, Clone)]
pub enum CostCurrency {
    /// Value is already in USD.
    Usd,
    /// Value is in vendor credits; multiply by `conversion_rate_to_usd` to get USD.
    Credits { conversion_rate_to_usd: f64 },
}

/// How to extract the per-request cost from the vendor response.
#[derive(Debug, Clone)]
pub enum CostExtraction {
    /// The vendor does not report cost; use `fallback_cost_usd`.
    None,
    /// Use the configured `fallback_cost_usd` directly (static billing).
    Static,
    /// Read cost from a response header.
    Header { name: String, currency: CostCurrency },
    /// Read cost from a top-level JSON field in the response body.
    JsonField { field: String },
}

/// Maps an HTTP status code to a `CrawlError` variant.
#[derive(Debug, Clone)]
pub enum CrawlErrorKind {
    Unauthorized,
    RateLimited,
    ServerError,
    BadRequest,
}

/// A single status-to-error mapping entry.
#[derive(Debug, Clone)]
pub struct StatusOverride {
    /// The HTTP status code to match.
    pub http: u16,
    /// The error kind to raise.
    pub error: CrawlErrorKind,
    /// Optional human-readable message; the vendor name is prepended if `None`.
    pub message: Option<String>,
}

/// Response decoding and cost extraction parameters.
#[derive(Debug, Clone)]
pub struct ResponseShape {
    /// How to interpret the response body.
    pub kind: ResponseKind,
    /// How to extract the per-request cost.
    pub cost_extraction: CostExtraction,
    /// Fallback cost in USD when `cost_extraction` yields nothing.
    pub fallback_cost_usd: Option<f64>,
}

/// Top-level configuration for a single bypass provider vendor.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Stable, lowercase vendor identifier (matches `BypassProvider::vendor_name`).
    pub vendor_name: String,
    /// Base URL of the vendor's API endpoint.
    pub endpoint: String,
    /// HTTP method to use.
    pub method: HttpMethod,
    /// Authentication scheme.
    pub auth: AuthScheme,
    /// Request construction parameters.
    pub request: RequestShape,
    /// Response decoding and cost extraction.
    pub response: ResponseShape,
    /// Ordered list of HTTP status overrides; matched before the default mapping.
    pub status_mapping: Vec<StatusOverride>,
}
