//! Provider configuration types.
//!
//! These types represent the schema loaded from per-vendor YAML files.
//! See `loader.rs` for the parsing logic and `configs/` for example files.

/// Placeholder every `Debug` impl in this module prints in place of a secret.
///
/// Must equal `crawlberg`'s own placeholder, so one redacted rendering is recognisable
/// wherever it comes from. `redaction_placeholder_matches_crawlbergs` pins that.
pub(crate) const REDACTED_PLACEHOLDER: &str = "***";

/// The marker in a JSON body template that the provider replaces with the target URL.
pub(crate) const URL_PLACEHOLDER: &str = "{{url}}";

/// HTTP method for the vendor's extraction endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// A vendor secret: an API key, a token or a query parameter value.
///
/// `Debug` and `Display` print [`REDACTED_PLACEHOLDER`], never the value, so a struct that
/// derives `Debug` over it cannot print the secret. An empty secret prints as `""`, which shows
/// that the value is missing without showing anything else. [`Secret::expose`] is the only way to
/// read the value, for the request builder.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The secret value, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl PartialEq<str> for Secret {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str("\"\"")
        } else {
            f.write_str(REDACTED_PLACEHOLDER)
        }
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            Ok(())
        } else {
            f.write_str(REDACTED_PLACEHOLDER)
        }
    }
}

/// Authentication scheme to apply to every outbound request.
#[derive(Debug, Clone)]
pub enum AuthScheme {
    /// No authentication.
    None,
    /// `Authorization: Bearer <token>`.
    Bearer { token: Secret },
    /// HTTP Basic Auth with the API key as the username and an empty password.
    /// Used by Zyte.
    // ~keep A secret although `crawlberg`'s `AuthConfig::Basic` prints its username: this one
    // ~keep carries the vendor API key, not an account name.
    BasicUsername { username: Secret },
    /// A custom header: `<name>: <value>`.
    Header { name: String, value: Secret },
    /// Append `?<name>=<value>` to the request URL.
    QueryParam { name: String, value: Secret },
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
#[derive(Clone)]
pub enum RequestBody {
    /// JSON body; the literal `{{url}}` placeholder is replaced with the
    /// URL-encoded target before sending.
    Json { template: String },
}

impl std::fmt::Debug for RequestBody {
    /// Redacted: a `${ENV}` value substituted into the template can be a vendor key, so the
    /// template prints as the placeholder with its length. Whether it holds the `{{url}}`
    /// marker prints too, because a template without it is the likeliest misconfiguration.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json { template } => f
                .debug_struct("Json")
                .field(
                    "template",
                    &format!("{REDACTED_PLACEHOLDER} ({} bytes)", template.len()),
                )
                .field("has_url_placeholder", &template.contains(URL_PLACEHOLDER))
                .finish(),
        }
    }
}

/// Request construction parameters.
#[derive(Debug, Clone)]
pub struct RequestShape {
    /// POST body; `None` for GET requests.
    pub body: Option<RequestBody>,
    /// Fixed query parameters appended to every request (before `url_param`). A value can be
    /// a vendor key, so it is a [`Secret`].
    pub query: Vec<(String, Secret)>,
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
#[derive(Clone)]
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

impl std::fmt::Debug for ProviderConfig {
    /// Redacted: the endpoint prints as its origin only, through the shared
    /// `crawlberg::net::redact::redact_url_to_origin`, because its userinfo, path, query and
    /// fragment can each carry a vendor key. The auth scheme and request shape redact their
    /// own secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            vendor_name,
            endpoint,
            method,
            auth,
            request,
            response,
            status_mapping,
        } = self;
        f.debug_struct("ProviderConfig")
            .field("vendor_name", vendor_name)
            .field("endpoint", &crawlberg::net::redact::redact_url_to_origin(endpoint))
            .field("method", method)
            .field("auth", auth)
            .field("request", request)
            .field("response", response)
            .field("status_mapping", status_mapping)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A redacted value must render identically whichever crate produced it.
    #[test]
    fn redaction_placeholder_matches_crawlbergs() {
        assert_eq!(
            REDACTED_PLACEHOLDER,
            crawlberg::net::redact::REDACTED_PLACEHOLDER,
            "this crate's placeholder has drifted from crawlberg's"
        );
    }

    /// A secret prints the placeholder through `Debug` and `Display`, and only `expose` reads it.
    #[test]
    fn secret_never_prints_its_value() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        let secret = Secret::from(SECRET);
        assert_eq!(format!("{secret:?}"), REDACTED_PLACEHOLDER);
        assert_eq!(format!("{secret}"), REDACTED_PLACEHOLDER);
        assert_eq!(secret.expose(), SECRET);
        let empty = Secret::from("");
        assert_eq!(format!("{empty:?}"), r#""""#);
        assert_eq!(format!("{empty}"), "");
    }

    /// Every auth scheme that holds a secret prints the placeholder, and keeps a header or
    /// query parameter name visible.
    #[test]
    fn auth_scheme_debug_never_prints_a_planted_secret() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        for (auth, visible) in [
            (AuthScheme::Bearer { token: SECRET.into() }, "Bearer"),
            (
                AuthScheme::BasicUsername {
                    username: SECRET.into(),
                },
                "BasicUsername",
            ),
            (
                AuthScheme::Header {
                    name: "X-Api-Key".into(),
                    value: SECRET.into(),
                },
                "X-Api-Key",
            ),
            (
                AuthScheme::QueryParam {
                    name: "api_key".into(),
                    value: SECRET.into(),
                },
                "api_key",
            ),
        ] {
            for rendered in [format!("{auth:?}"), format!("{auth:#?}")] {
                assert!(!rendered.contains(SECRET), "a secret printed: {rendered}");
                assert!(
                    rendered.contains(REDACTED_PLACEHOLDER),
                    "the placeholder is missing: {rendered}"
                );
                assert!(rendered.contains(visible), "{visible} must stay visible: {rendered}");
            }
        }
    }

    /// A body template prints its length and whether it holds `{{url}}`, never its text.
    #[test]
    fn request_body_debug_shows_the_length_and_the_url_placeholder() {
        for (template, has_placeholder) in [(r#"{"url":"{{url}}","key":"k"}"#, true), (r#"{"url":"url"}"#, false)] {
            let body = RequestBody::Json {
                template: template.into(),
            };
            assert_eq!(
                format!("{body:?}"),
                format!(
                    r#"Json {{ template: "*** ({} bytes)", has_url_placeholder: {has_placeholder} }}"#,
                    template.len()
                ),
            );
        }
    }

    /// Every secret-bearing field of a provider config prints the placeholder, not the value.
    #[test]
    fn provider_config_debug_prints_the_placeholder_for_every_secret() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        let config = ProviderConfig {
            vendor_name: "querykey".into(),
            endpoint: format!("https://{SECRET}.vendor.example:8443/v1/{SECRET}?key={SECRET}"),
            method: HttpMethod::Post,
            auth: AuthScheme::BasicUsername {
                username: SECRET.into(),
            },
            request: RequestShape {
                body: Some(RequestBody::Json {
                    template: format!(r#"{{"key":"{SECRET}","url":"{{{{url}}}}"}}"#),
                }),
                query: vec![("api_key".into(), SECRET.into())],
                url_param: UrlParamLocation::BodyField,
            },
            response: ResponseShape {
                kind: ResponseKind::RawBody,
                cost_extraction: CostExtraction::None,
                fallback_cost_usd: None,
            },
            status_mapping: vec![],
        };

        for rendered in [format!("{config:?}"), format!("{config:#?}")] {
            assert!(!rendered.contains(SECRET), "a secret printed: {rendered}");
            assert!(
                rendered.contains(REDACTED_PLACEHOLDER),
                "the placeholder is missing: {rendered}"
            );
            assert!(
                rendered.contains("https://***.vendor.example:8443"),
                "the endpoint origin must stay visible: {rendered}"
            );
            assert!(
                rendered.contains("api_key"),
                "a query parameter name must stay visible: {rendered}"
            );
        }
    }
}
