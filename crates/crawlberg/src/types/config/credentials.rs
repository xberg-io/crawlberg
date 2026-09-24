//! Credential-bearing config: proxy and HTTP auth, each with a redacting `Debug`.

use serde::{Deserialize, Serialize};

/// Proxy configuration for HTTP requests.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Proxy URL (e.g. "http://proxy:8080", "socks5://proxy:1080").
    pub url: String,
    /// Optional username for proxy authentication.
    pub username: Option<String>,
    /// Optional password for proxy authentication.
    pub password: Option<String>,
}

impl std::fmt::Debug for ProxyConfig {
    /// Redacted: the derived `Debug` would print `password` verbatim, and `url` may
    /// itself carry `user:pass@` userinfo. Any `tracing::debug!(?proxy, ...)` or
    /// `{:?}` capture would leak it into logs. Shows the redacted URL and the username,
    /// but only whether a password is set — never the password itself.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface. The derived Debug this replaced emitted no method at all.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &crate::net::redact_url_credentials(&self.url))
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Authentication configuration.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
pub enum AuthConfig {
    /// HTTP Basic authentication.
    #[serde(rename = "basic")]
    Basic {
        /// Username sent in the `Authorization: Basic` header.
        username: String,
        /// Password sent in the `Authorization: Basic` header.
        password: String,
    },
    /// Bearer token authentication.
    #[serde(rename = "bearer")]
    Bearer {
        /// Token sent in the `Authorization: Bearer` header.
        token: String,
    },
    /// Custom authentication header.
    #[serde(rename = "header")]
    Header {
        /// HTTP header name to set on each request.
        name: String,
        /// HTTP header value to send.
        value: String,
    },
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::Basic {
            username: String::new(),
            password: String::new(),
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    /// Redacted: the derived `Debug` would print `password`, `token`, and `value` (the
    /// header value carrying the secret) verbatim, and any `tracing::debug!(?auth, ...)`
    /// or `{:?}` capture would leak it into logs. Shows which variant is configured and
    /// whether its secret field is non-empty, never the secret's contents.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface. The derived Debug this replaced emitted no method at all.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, password } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &(!password.is_empty()).then_some("***"))
                .finish(),
            Self::Bearer { token } => f
                .debug_struct("Bearer")
                .field("token", &(!token.is_empty()).then_some("***"))
                .finish(),
            Self::Header { name, value } => f
                .debug_struct("Header")
                .field("name", name)
                .field("value", &(!value.is_empty()).then_some("***"))
                .finish(),
        }
    }
}
