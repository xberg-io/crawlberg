//! YAML-driven configuration loader.
//!
//! Parses per-vendor YAML files into `ProviderConfig` with `${ENV_VAR}`
//! interpolation. Uses `saphyr` directly (no serde integration) so the AST
//! walk is explicit and validation errors point at the offending field.

use std::collections::HashMap;
use std::path::Path;

use saphyr::{LoadableYamlNode, YamlOwned};

use crate::config::{
    AuthScheme, CostCurrency, CostExtraction, CrawlErrorKind, HttpMethod, ProviderConfig, RequestBody, RequestShape,
    ResponseKind, ResponseShape, StatusOverride, UrlParamLocation,
};
use crate::error::ConfigError;

/// Load a `ProviderConfig` from a YAML file, substituting `${VAR}` placeholders
/// with values from `env_vars`.
///
/// # Errors
///
/// Returns `ConfigError::MissingEnvVar` if any placeholder's variable is absent
/// from `env_vars`, or `ConfigError::MissingField` / `ConfigError::UnknownValue`
/// for schema violations.
pub fn load_with_env(path: &Path, env_vars: &HashMap<String, String>) -> Result<ProviderConfig, ConfigError> {
    let raw = std::fs::read_to_string(path)?;
    parse_yaml(&raw, env_vars)
}

/// Load a `ProviderConfig` from a YAML file, substituting `${VAR}` placeholders
/// with values from the process environment.
pub fn load_with_process_env(path: &Path) -> Result<ProviderConfig, ConfigError> {
    let env_vars: HashMap<String, String> = std::env::vars().collect();
    load_with_env(path, &env_vars)
}

fn parse_yaml(raw: &str, env_vars: &HashMap<String, String>) -> Result<ProviderConfig, ConfigError> {
    let docs = YamlOwned::load_from_str(raw).map_err(|e| ConfigError::Parse(format!("{e}")))?;
    let doc = docs
        .into_iter()
        .next()
        .ok_or_else(|| ConfigError::Parse("empty YAML document".into()))?;

    let vendor_name = interp(req_str(&doc, "vendor_name")?, env_vars)?;
    let endpoint = interp(req_str(&doc, "endpoint")?, env_vars)?;
    let method = parse_method(req_str(&doc, "method")?)?;

    let auth_node = req_child(&doc, "auth")?;
    let auth = parse_auth(auth_node, env_vars)?;

    let request_node = req_child(&doc, "request")?;
    let request = parse_request(request_node, env_vars)?;

    let response_node = req_child(&doc, "response")?;
    let response = parse_response(response_node, env_vars)?;

    let status_mapping = match doc.as_mapping_get("status_mapping") {
        None => Vec::new(),
        Some(node) => parse_status_mapping(node)?,
    };

    Ok(ProviderConfig {
        vendor_name,
        endpoint,
        method,
        auth,
        request,
        response,
        status_mapping,
    })
}

fn parse_method(s: &str) -> Result<HttpMethod, ConfigError> {
    match s.to_uppercase().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        other => Err(ConfigError::UnknownValue {
            field: "method".into(),
            value: other.into(),
        }),
    }
}

fn parse_auth(node: &YamlOwned, env_vars: &HashMap<String, String>) -> Result<AuthScheme, ConfigError> {
    let kind = req_str(node, "kind")?;
    match kind {
        "none" => Ok(AuthScheme::None),
        "bearer" => {
            let token = interp(req_str(node, "token")?, env_vars)?;
            Ok(AuthScheme::Bearer { token })
        }
        "basic_username" => {
            let username = interp(req_str(node, "username")?, env_vars)?;
            Ok(AuthScheme::BasicUsername { username })
        }
        "header" => {
            let name = interp(req_str(node, "name")?, env_vars)?;
            let value = interp(req_str(node, "value")?, env_vars)?;
            Ok(AuthScheme::Header { name, value })
        }
        "query_param" => {
            let name = interp(req_str(node, "name")?, env_vars)?;
            let value = interp(req_str(node, "value")?, env_vars)?;
            Ok(AuthScheme::QueryParam { name, value })
        }
        other => Err(ConfigError::UnknownValue {
            field: "auth.kind".into(),
            value: other.into(),
        }),
    }
}

fn parse_request(node: &YamlOwned, env_vars: &HashMap<String, String>) -> Result<RequestShape, ConfigError> {
    let body = match node.as_mapping_get("body") {
        None => None,
        Some(body_node) => {
            let body_kind = req_str(body_node, "kind")?;
            match body_kind {
                "json" => {
                    let template = interp(req_str(body_node, "template")?, env_vars)?;
                    Some(RequestBody::Json { template })
                }
                other => {
                    return Err(ConfigError::UnknownValue {
                        field: "request.body.kind".into(),
                        value: other.into(),
                    });
                }
            }
        }
    };

    let query = match node.as_mapping_get("query") {
        None => Vec::new(),
        Some(query_node) => {
            let items = query_node.as_vec().ok_or_else(|| ConfigError::WrongType {
                field: "request.query".into(),
                expected: "array".into(),
            })?;
            let mut pairs = Vec::new();
            for item in items {
                let name = req_str(item, "name")?;
                let value = interp(req_str(item, "value")?, env_vars)?;
                pairs.push((name.to_owned(), value));
            }
            pairs
        }
    };

    let url_param_node = req_child(node, "url_param")?;
    let url_param = parse_url_param(url_param_node)?;

    Ok(RequestShape { body, query, url_param })
}

fn parse_url_param(node: &YamlOwned) -> Result<UrlParamLocation, ConfigError> {
    let kind = req_str(node, "kind")?;
    match kind {
        "query_param" => {
            let name = req_str(node, "name")?;
            Ok(UrlParamLocation::QueryParam { name: name.to_owned() })
        }
        "body_field" => Ok(UrlParamLocation::BodyField),
        other => Err(ConfigError::UnknownValue {
            field: "request.url_param.kind".into(),
            value: other.into(),
        }),
    }
}

fn parse_response(node: &YamlOwned, env_vars: &HashMap<String, String>) -> Result<ResponseShape, ConfigError> {
    let kind_node = req_child(node, "kind")?;
    let kind = parse_response_kind(kind_node)?;
    let cost_extraction_node = req_child(node, "cost_extraction")?;
    let cost_extraction = parse_cost_extraction(cost_extraction_node, env_vars)?;
    let fallback_cost_usd = match node.as_mapping_get("fallback_cost_usd") {
        None => None,
        Some(v) => {
            if let Some(f) = v.as_floating_point() {
                Some(f)
            } else if let Some(i) = v.as_integer() {
                Some(i as f64)
            } else {
                return Err(ConfigError::WrongType {
                    field: "response.fallback_cost_usd".into(),
                    expected: "float".into(),
                });
            }
        }
    };
    Ok(ResponseShape {
        kind,
        cost_extraction,
        fallback_cost_usd,
    })
}

fn parse_response_kind(node: &YamlOwned) -> Result<ResponseKind, ConfigError> {
    let kind = req_str(node, "kind")?;
    match kind {
        "raw_body" => Ok(ResponseKind::RawBody),
        "json_field" => {
            let html_field = req_str(node, "html_field")?;
            Ok(ResponseKind::JsonField {
                html_field: html_field.to_owned(),
            })
        }
        other => Err(ConfigError::UnknownValue {
            field: "response.kind.kind".into(),
            value: other.into(),
        }),
    }
}

fn parse_cost_extraction(node: &YamlOwned, env_vars: &HashMap<String, String>) -> Result<CostExtraction, ConfigError> {
    let kind = req_str(node, "kind")?;
    match kind {
        "none" => Ok(CostExtraction::None),
        "static" => Ok(CostExtraction::Static),
        "header" => {
            let name = interp(req_str(node, "name")?, env_vars)?;
            let currency_node = req_child(node, "currency")?;
            let currency = parse_currency(currency_node)?;
            Ok(CostExtraction::Header { name, currency })
        }
        "json_field" => {
            let field = req_str(node, "field")?.to_owned();
            Ok(CostExtraction::JsonField { field })
        }
        other => Err(ConfigError::UnknownValue {
            field: "response.cost_extraction.kind".into(),
            value: other.into(),
        }),
    }
}

fn parse_currency(node: &YamlOwned) -> Result<CostCurrency, ConfigError> {
    let kind = req_str(node, "kind")?;
    match kind {
        "usd" => Ok(CostCurrency::Usd),
        "credits" => {
            let rate_node = node
                .as_mapping_get("conversion_rate_to_usd")
                .ok_or_else(|| ConfigError::MissingField("conversion_rate_to_usd".into()))?;
            let rate = if let Some(f) = rate_node.as_floating_point() {
                f
            } else if let Some(i) = rate_node.as_integer() {
                i as f64
            } else {
                return Err(ConfigError::WrongType {
                    field: "conversion_rate_to_usd".into(),
                    expected: "float".into(),
                });
            };
            Ok(CostCurrency::Credits {
                conversion_rate_to_usd: rate,
            })
        }
        other => Err(ConfigError::UnknownValue {
            field: "response.cost_extraction.currency.kind".into(),
            value: other.into(),
        }),
    }
}

fn parse_status_mapping(node: &YamlOwned) -> Result<Vec<StatusOverride>, ConfigError> {
    let items = node.as_vec().ok_or_else(|| ConfigError::WrongType {
        field: "status_mapping".into(),
        expected: "array".into(),
    })?;
    let mut result = Vec::new();
    for item in items {
        let http = item
            .as_mapping_get("http")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| ConfigError::MissingField("status_mapping[].http".into()))? as u16;
        let error_str = req_str(item, "error")?;
        let error = match error_str {
            "unauthorized" => CrawlErrorKind::Unauthorized,
            "rate_limited" => CrawlErrorKind::RateLimited,
            "server_error" => CrawlErrorKind::ServerError,
            "bad_request" => CrawlErrorKind::BadRequest,
            other => {
                return Err(ConfigError::UnknownValue {
                    field: "status_mapping[].error".into(),
                    value: other.into(),
                });
            }
        };
        let message = item
            .as_mapping_get("message")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        result.push(StatusOverride { http, error, message });
    }
    Ok(result)
}

/// Get a required mapping child by key, returning `MissingField` if absent.
fn req_child<'a>(node: &'a YamlOwned, key: &str) -> Result<&'a YamlOwned, ConfigError> {
    node.as_mapping_get(key)
        .ok_or_else(|| ConfigError::MissingField(key.into()))
}

/// Get a required string field value from a mapping node.
fn req_str<'a>(node: &'a YamlOwned, key: &str) -> Result<&'a str, ConfigError> {
    match node.as_mapping_get(key) {
        Some(v) => v.as_str().ok_or_else(|| ConfigError::WrongType {
            field: key.into(),
            expected: "string".into(),
        }),
        None => Err(ConfigError::MissingField(key.into())),
    }
}

/// Substitute `${VAR_NAME}` placeholders in `s` using `env_vars`.
pub(crate) fn interp(s: &str, env_vars: &HashMap<String, String>) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;
    while let Some(start) = remaining.find("${") {
        let prefix = &remaining[..start];
        result.push_str(prefix);
        let rest = &remaining[start + 2..];
        let end = rest
            .find('}')
            .ok_or_else(|| ConfigError::Parse(format!("unclosed '${{' in: {s}")))?;
        let var_name = &rest[..end];
        let value = env_vars
            .get(var_name)
            .ok_or_else(|| ConfigError::MissingEnvVar(var_name.to_owned()))?;
        result.push_str(value);
        remaining = &rest[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bright_data_yaml(token: &str) -> String {
        format!(
            r#"
vendor_name: bright_data
endpoint: "https://api.brightdata.com/request"
method: POST
auth:
  kind: bearer
  token: "{token}"
request:
  body:
    kind: json
    template: '{{"url": "{{{{url}}}}", "format": "raw"}}'
  url_param:
    kind: body_field
response:
  kind:
    kind: raw_body
  cost_extraction:
    kind: static
  fallback_cost_usd: 0.003
status_mapping:
  - http: 401
    error: unauthorized
  - http: 402
    error: unauthorized
  - http: 429
    error: rate_limited
"#,
            token = token
        )
    }

    #[test]
    fn parse_resolves_env_var() {
        let yaml = make_bright_data_yaml("${MY_TOKEN}");
        let mut env = HashMap::new();
        env.insert("MY_TOKEN".into(), "secret-abc".into());
        let config = parse_yaml(&yaml, &env).unwrap();
        assert_eq!(config.vendor_name, "bright_data");
        assert!(
            matches!(config.auth, AuthScheme::Bearer { ref token } if token == "secret-abc"),
            "unexpected auth: {:?}",
            config.auth
        );
    }

    #[test]
    fn parse_error_on_missing_env_var() {
        let yaml = make_bright_data_yaml("${MISSING_VAR}");
        let env = HashMap::new();
        let err = parse_yaml(&yaml, &env).unwrap_err();
        assert!(
            matches!(err, ConfigError::MissingEnvVar(ref v) if v == "MISSING_VAR"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_error_on_missing_required_field() {
        let yaml = r#"
vendor_name: test
method: GET
auth:
  kind: none
request:
  url_param:
    kind: query_param
    name: url
response:
  kind:
    kind: raw_body
  cost_extraction:
    kind: static
"#;
        let env = HashMap::new();
        let err = parse_yaml(yaml, &env).unwrap_err();
        assert!(
            matches!(err, ConfigError::MissingField(ref f) if f == "endpoint"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_error_on_unknown_auth_kind() {
        let yaml = r#"
vendor_name: test
endpoint: "https://example.com"
method: GET
auth:
  kind: magic_token
request:
  url_param:
    kind: query_param
    name: url
response:
  kind:
    kind: raw_body
  cost_extraction:
    kind: static
"#;
        let env = HashMap::new();
        let err = parse_yaml(yaml, &env).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownValue { ref field, .. } if field.contains("auth.kind")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_error_on_unknown_cost_extraction_kind() {
        let yaml = r#"
vendor_name: test
endpoint: "https://example.com"
method: GET
auth:
  kind: none
request:
  url_param:
    kind: query_param
    name: url
response:
  kind:
    kind: raw_body
  cost_extraction:
    kind: magic_cost
"#;
        let env = HashMap::new();
        let err = parse_yaml(yaml, &env).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownValue { ref field, .. } if field.contains("cost_extraction")),
            "unexpected error: {err}"
        );
    }
}
