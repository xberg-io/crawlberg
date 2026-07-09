//! Tests for the YAML config loader.

use std::collections::HashMap;
use std::io::Write;

use crawlberg_bypass::config::AuthScheme;
use crawlberg_bypass::error::ConfigError;
use crawlberg_bypass::loader::load_with_env;

fn write_temp_yaml(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("temp file");
    f.write_all(content.as_bytes()).expect("write");
    f
}

fn minimal_yaml_with_auth(auth_block: &str) -> String {
    format!(
        r#"
vendor_name: test_vendor
endpoint: "https://example.com/api"
method: POST
{auth_block}
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
  fallback_cost_usd: 0.001
"#
    )
}

#[test]
fn load_parses_bearer_auth_and_resolves_env_var() {
    let yaml = minimal_yaml_with_auth(
        r#"auth:
  kind: bearer
  token: "${MY_TEST_TOKEN}""#,
    );
    let file = write_temp_yaml(&yaml);
    let mut env = HashMap::new();
    env.insert("MY_TEST_TOKEN".into(), "resolved-token".into());

    let config = load_with_env(file.path(), &env).unwrap();
    assert_eq!(config.vendor_name, "test_vendor");
    assert!(
        matches!(&config.auth, AuthScheme::Bearer { token } if token == "resolved-token"),
        "unexpected auth: {:?}",
        config.auth
    );
    assert_eq!(config.response.fallback_cost_usd, Some(0.001));
}

#[test]
fn load_parses_basic_username_auth() {
    let yaml = minimal_yaml_with_auth(
        r#"auth:
  kind: basic_username
  username: "${API_KEY}""#,
    );
    let file = write_temp_yaml(&yaml);
    let mut env = HashMap::new();
    env.insert("API_KEY".into(), "my-api-key".into());

    let config = load_with_env(file.path(), &env).unwrap();
    assert!(
        matches!(&config.auth, AuthScheme::BasicUsername { username } if username == "my-api-key"),
        "unexpected auth: {:?}",
        config.auth
    );
}

#[test]
fn load_parses_header_auth() {
    let yaml = minimal_yaml_with_auth(
        r#"auth:
  kind: header
  name: "X-Api-Key"
  value: "${HDR_KEY}""#,
    );
    let file = write_temp_yaml(&yaml);
    let mut env = HashMap::new();
    env.insert("HDR_KEY".into(), "hdr-secret".into());

    let config = load_with_env(file.path(), &env).unwrap();
    assert!(
        matches!(&config.auth, AuthScheme::Header { name, value }
            if name == "X-Api-Key" && value == "hdr-secret"),
        "unexpected auth: {:?}",
        config.auth
    );
}

#[test]
fn load_parses_status_mapping() {
    let yaml = r#"
vendor_name: test_vendor
endpoint: "https://example.com/api"
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
status_mapping:
  - http: 401
    error: unauthorized
  - http: 429
    error: rate_limited
    message: "custom message"
"#;
    let file = write_temp_yaml(yaml);
    let env = HashMap::new();

    let config = load_with_env(file.path(), &env).unwrap();
    assert_eq!(config.status_mapping.len(), 2);
    assert_eq!(config.status_mapping[0].http, 401);
    assert_eq!(config.status_mapping[1].http, 429);
    assert_eq!(config.status_mapping[1].message.as_deref(), Some("custom message"));
}

#[test]
fn error_on_missing_env_var() {
    let yaml = minimal_yaml_with_auth(
        r#"auth:
  kind: bearer
  token: "${ABSENT_VAR}""#,
    );
    let file = write_temp_yaml(&yaml);
    let env = HashMap::new();

    let err = load_with_env(file.path(), &env).unwrap_err();
    assert!(
        matches!(err, ConfigError::MissingEnvVar(ref v) if v == "ABSENT_VAR"),
        "unexpected error: {err}"
    );
}

#[test]
fn error_on_missing_required_field() {
    let yaml = r#"
vendor_name: test_vendor
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
    let file = write_temp_yaml(yaml);
    let env = HashMap::new();

    let err = load_with_env(file.path(), &env).unwrap_err();
    assert!(
        matches!(err, ConfigError::MissingField(ref f) if f == "endpoint"),
        "unexpected error: {err}"
    );
}

#[test]
fn error_on_unknown_auth_kind() {
    let yaml = minimal_yaml_with_auth(
        r#"auth:
  kind: magic_unicorn"#,
    );
    let file = write_temp_yaml(&yaml);
    let env = HashMap::new();

    let err = load_with_env(file.path(), &env).unwrap_err();
    assert!(
        matches!(err, ConfigError::UnknownValue { ref field, .. } if field.contains("auth.kind")),
        "unexpected error: {err}"
    );
}

#[test]
fn error_on_malformed_cost_extraction() {
    let yaml = r#"
vendor_name: test_vendor
endpoint: "https://example.com/api"
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
    kind: unknown_cost_kind
"#;
    let file = write_temp_yaml(yaml);
    let env = HashMap::new();

    let err = load_with_env(file.path(), &env).unwrap_err();
    assert!(
        matches!(err, ConfigError::UnknownValue { ref field, .. } if field.contains("cost_extraction")),
        "unexpected error: {err}"
    );
}
