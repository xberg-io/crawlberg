//! `SimpleHttpProvider` — a YAML-config-driven `BypassProvider` implementation.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use crawlberg::{BypassProvider, BypassResponse, CrawlError};
use tracing::{Instrument, info_span};

use crate::config::{
    AuthScheme, CrawlErrorKind, HttpMethod, ProviderConfig, RequestBody, ResponseKind, UrlParamLocation,
};
use crate::error::ProviderError;
use crate::extract;

/// A `BypassProvider` driven entirely by a `ProviderConfig` loaded from YAML.
///
/// One instance should be created per vendor (it holds a reused `reqwest::Client`).
pub struct SimpleHttpProvider {
    config: ProviderConfig,
    client: reqwest::Client,
    /// Overrides `config.endpoint` in tests to point at a wiremock server.
    endpoint_override: Option<String>,
}

impl SimpleHttpProvider {
    /// Create a new provider from the given config, building a reused HTTP client.
    ///
    /// # Errors
    ///
    /// Returns `CrawlError::Other` if the HTTP client cannot be built.
    pub fn new(config: ProviderConfig) -> Result<Self, CrawlError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| CrawlError::other(ProviderError::ClientBuild(e.to_string()).to_string()))?;
        Ok(Self {
            config,
            client,
            endpoint_override: None,
        })
    }

    /// Override the endpoint URL — intended for use in tests with a wiremock server.
    #[must_use]
    pub fn with_endpoint_override(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint_override = Some(endpoint.into());
        self
    }

    fn effective_endpoint(&self) -> &str {
        self.endpoint_override.as_deref().unwrap_or(&self.config.endpoint)
    }
}

impl fmt::Debug for SimpleHttpProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimpleHttpProvider")
            .field("vendor", &self.config.vendor_name)
            .finish()
    }
}

#[async_trait]
impl BypassProvider for SimpleHttpProvider {
    fn vendor_name(&self) -> &'static str {
        // ~keep BypassProvider requires `&'static str`; leak one vendor string per long-lived provider instance.
        Box::leak(self.config.vendor_name.clone().into_boxed_str())
    }

    async fn fetch(&self, url: &str) -> Result<BypassResponse, CrawlError> {
        let vendor = self.config.vendor_name.clone();
        let span = info_span!("bypass.fetch", vendor = %vendor, url = %url);

        async move {
            let req = self.build_request(url)?;
            let resp = req.send().await.map_err(|e| {
                CrawlError::other(
                    ProviderError::Send {
                        vendor: vendor.clone(),
                        message: e.to_string(),
                    }
                    .to_string(),
                )
            })?;

            let status_u16 = resp.status().as_u16();
            let final_url = resp.url().to_string();
            let resp_headers = resp.headers().clone();

            if let Some(err) = self.map_status(status_u16, &vendor) {
                return Err(err);
            }

            match status_u16 {
                200..=299 => {}
                401..=403 => {
                    return Err(CrawlError::unauthorized(format!(
                        "{vendor} auth/quota failure: {status_u16}"
                    )));
                }
                429 => {
                    return Err(CrawlError::rate_limited(format!("{vendor} rate limited")));
                }
                500..=599 => {
                    return Err(CrawlError::server_error(format!("{vendor} upstream {status_u16}")));
                }
                other => {
                    return Err(CrawlError::other(format!("{vendor} unexpected status {other}")));
                }
            }

            let body_bytes = resp.bytes().await.map_err(|e| {
                CrawlError::other(
                    ProviderError::BodyRead {
                        vendor: vendor.clone(),
                        message: e.to_string(),
                    }
                    .to_string(),
                )
            })?;

            let raw_body = String::from_utf8_lossy(&body_bytes).into_owned();

            let (body, body_bytes_final) = self.decode_body(&raw_body, body_bytes.to_vec(), &vendor)?;

            let cost_usd = extract::cost(
                &resp_headers,
                &raw_body,
                &self.config.response.cost_extraction,
                self.config.response.fallback_cost_usd,
            );

            Ok(BypassResponse {
                status: status_u16,
                content_type: "text/html".into(),
                body,
                body_bytes: body_bytes_final,
                headers: HashMap::new(),
                final_url,
                cost_usd,
                vendor_request_id: None,
            })
        }
        .instrument(span)
        .await
    }
}

impl SimpleHttpProvider {
    /// Build the outbound reqwest request according to the provider config.
    fn build_request(&self, url: &str) -> Result<reqwest::RequestBuilder, CrawlError> {
        let endpoint = self.effective_endpoint();

        // ~keep Build query strings manually so this crate does not require reqwest's optional `query` feature.
        let full_url = self.build_url(endpoint, url);

        let mut req = match self.config.method {
            HttpMethod::Get => self.client.get(&full_url),
            HttpMethod::Post => {
                let mut r = self.client.post(&full_url);
                if let Some(RequestBody::Json { template }) = &self.config.request.body {
                    let body_str = template.replace("{{url}}", url);
                    r = r.header("Content-Type", "application/json").body(body_str);
                }
                r
            }
        };

        req = match &self.config.auth {
            AuthScheme::None => req,
            AuthScheme::Bearer { token } => req.bearer_auth(token),
            AuthScheme::BasicUsername { username } => req.basic_auth(username, Option::<&str>::None),
            AuthScheme::Header { name, value } => req.header(name.as_str(), value.as_str()),
            AuthScheme::QueryParam { .. } => req,
        };

        Ok(req)
    }

    /// Build the full request URL, embedding fixed query params and (for GET) the
    /// url_param, plus any AuthScheme::QueryParam values.
    fn build_url(&self, endpoint: &str, target_url: &str) -> String {
        let mut full = endpoint.to_owned();
        let mut sep = if full.contains('?') { '&' } else { '?' };

        for (k, v) in &self.config.request.query {
            full.push(sep);
            full.push_str(k);
            full.push('=');
            full.push_str(&urlencoding::encode(v));
            sep = '&';
        }

        if self.config.method == HttpMethod::Get
            && let UrlParamLocation::QueryParam { name } = &self.config.request.url_param
        {
            full.push(sep);
            full.push_str(name);
            full.push('=');
            full.push_str(&urlencoding::encode(target_url));
            sep = '&';
        }

        if let AuthScheme::QueryParam { name, value } = &self.config.auth {
            full.push(sep);
            full.push_str(name);
            full.push('=');
            full.push_str(&urlencoding::encode(value));
        }

        full
    }

    fn map_status(&self, status: u16, vendor: &str) -> Option<CrawlError> {
        for override_ in &self.config.status_mapping {
            if override_.http == status {
                let msg = override_
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("{vendor} status {status}"));
                return Some(match override_.error {
                    CrawlErrorKind::Unauthorized => CrawlError::unauthorized(msg),
                    CrawlErrorKind::RateLimited => CrawlError::rate_limited(msg),
                    CrawlErrorKind::ServerError => CrawlError::server_error(msg),
                    CrawlErrorKind::BadRequest => CrawlError::other(msg),
                });
            }
        }
        None
    }

    fn decode_body(&self, raw_body: &str, raw_bytes: Vec<u8>, vendor: &str) -> Result<(String, Vec<u8>), CrawlError> {
        match &self.config.response.kind {
            ResponseKind::RawBody => Ok((raw_body.to_owned(), raw_bytes)),
            ResponseKind::JsonField { html_field } => {
                let v: serde_json::Value = serde_json::from_str(raw_body).map_err(|e| {
                    CrawlError::other(
                        ProviderError::ResponseParse {
                            vendor: vendor.to_owned(),
                            message: e.to_string(),
                        }
                        .to_string(),
                    )
                })?;
                let html = v
                    .get(html_field.as_str())
                    .and_then(|f| f.as_str())
                    .ok_or_else(|| {
                        CrawlError::other(
                            ProviderError::ResponseParse {
                                vendor: vendor.to_owned(),
                                message: format!("missing or non-string field '{html_field}'"),
                            }
                            .to_string(),
                        )
                    })?
                    .to_owned();
                let bytes = html.as_bytes().to_vec();
                Ok((html, bytes))
            }
        }
    }
}
