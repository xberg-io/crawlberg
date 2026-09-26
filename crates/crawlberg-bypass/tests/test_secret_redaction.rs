//! Secrets in a provider config must not reach `Debug` output or loader errors.

use std::collections::HashMap;

use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, HttpMethod, ProviderConfig, RequestShape, ResponseKind, ResponseShape, UrlParamLocation,
};
use crawlberg_bypass::load_with_env;

const SECRET: &str = "sk-live-9f8e7d6c5b4a";

fn every_auth_scheme() -> Vec<AuthScheme> {
    vec![
        AuthScheme::None,
        AuthScheme::Bearer { token: SECRET.into() },
        AuthScheme::BasicUsername {
            username: SECRET.into(),
        },
        AuthScheme::Header {
            name: "X-Api-Key".into(),
            value: SECRET.into(),
        },
        AuthScheme::QueryParam {
            name: "api_key".into(),
            value: SECRET.into(),
        },
    ]
}

fn config_with(auth: AuthScheme, endpoint: &str) -> ProviderConfig {
    ProviderConfig {
        vendor_name: "vendor".into(),
        endpoint: endpoint.into(),
        method: HttpMethod::Get,
        auth,
        request: RequestShape {
            body: None,
            query: Vec::new(),
            url_param: UrlParamLocation::QueryParam { name: "url".into() },
        },
        response: ResponseShape {
            kind: ResponseKind::RawBody,
            cost_extraction: CostExtraction::Static,
            fallback_cost_usd: None,
        },
        status_mapping: Vec::new(),
    }
}

#[test]
fn debug_output_of_every_auth_scheme_hides_the_secret() {
    for auth in every_auth_scheme() {
        let variant = format!("{auth:?}");
        let config = config_with(auth.clone(), "https://api.example.com/");
        for rendered in [
            format!("{auth:?}"),
            format!("{auth:#?}"),
            format!("{config:?}"),
            format!("{config:#?}"),
        ] {
            assert!(!rendered.contains(SECRET), "secret printed by {variant}: {rendered}");
        }
    }
}

#[test]
fn debug_output_keeps_the_non_secret_fields() {
    let rendered = format!(
        "{:?}",
        AuthScheme::Header {
            name: "X-Api-Key".into(),
            value: SECRET.into(),
        }
    );
    assert_eq!(rendered, r#"Header { name: "X-Api-Key", value: Some("***") }"#);
    let rendered = format!("{:?}", AuthScheme::Bearer { token: String::new() });
    assert_eq!(rendered, "Bearer { token: None }");
}

#[test]
fn unclosed_placeholder_error_names_the_field_and_hides_the_value() {
    let yaml = format!(
        r#"
vendor_name: test
endpoint: "https://api.example.com"
method: GET
auth:
  kind: bearer
  token: "{SECRET}${{UNCLOSED"
request:
  url_param:
    kind: query_param
    name: url
response:
  kind:
    kind: raw_body
  cost_extraction:
    kind: static
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("provider.yaml");
    std::fs::write(&path, yaml).unwrap();

    let err = load_with_env(&path, &HashMap::new()).unwrap_err();
    let rendered = format!("{err} {err:?}");
    assert!(!rendered.contains(SECRET), "secret printed in loader error: {rendered}");
    assert_eq!(
        err.to_string(),
        format!(
            "config parse failed: unclosed '${{' in field 'auth.token' at byte {}",
            SECRET.len()
        )
    );
}
