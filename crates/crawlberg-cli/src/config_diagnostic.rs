use std::fmt;

use crawlberg::CrawlConfig;
use serde::Deserialize;
use serde_json::Value;
use serde_value::{DeserializerError, ValueDeserializer};

#[derive(Debug)]
pub(super) struct ConfigDiagnostic(String);

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigDiagnostic {}

pub(super) fn deserialize(value: Value) -> Result<CrawlConfig, ConfigDiagnostic> {
    let value = serde_value::Value::deserialize(value).map_err(|error| ConfigDiagnostic(error.to_string()))?;
    serde_path_to_error::deserialize(ValueDeserializer::<DeserializerError>::new(value)).map_err(|error| {
        let path = error.path().to_string();
        let message = match error.inner() {
            DeserializerError::UnknownField(_, _) => "unknown key".to_owned(),
            DeserializerError::InvalidType(_, expected) | DeserializerError::InvalidValue(_, expected) => {
                format!("expected {}", json_expected(expected))
            }
            other => other.to_string(),
        };
        ConfigDiagnostic(format!("{path}: {message}"))
    })
}

// ~keep These are Serde Expected visitor descriptors, not parsed rendered error messages.
fn json_expected(expected: &str) -> &str {
    match expected {
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "f32"
        | "f64" => "a JSON number",
        "a boolean" => "a JSON boolean",
        "a string" | "a borrowed string" | "a character" => "a JSON string",
        "a sequence" => "a JSON array",
        "a map" => "a JSON object",
        "string or map" => "a JSON string or object",
        "unit" => "JSON null",
        descriptor if descriptor.starts_with("struct ") => "a JSON object",
        descriptor => descriptor,
    }
}

#[cfg(test)]
mod tests {
    use super::deserialize;
    use crawlberg::CrawlConfig;
    use serde_json::json;

    #[test]
    fn should_preserve_valid_config_deserialization() {
        let cases = [
            json!({}),
            json!({"browser": {"mode": "always", "wait": "network_idle", "timeout": 321}}),
            json!({"proxy": {"url": "http://proxy.example", "username": null, "password": "secret"}}),
            json!({"content": {"strip_tags": ["span"], "max_depth": 5}}),
            json!({"ssrf": {"allowlist": [{"type": "exact", "value": "example.com"}]}}),
            json!({"auth": {"type": "basic", "username": "name", "password": "secret"}}),
            json!({"custom_headers": {"X-Config": "value"}, "proxy": null}),
        ];
        for value in cases {
            let original: CrawlConfig = serde_json::from_value(value.clone()).expect("valid legacy config");
            let actual = deserialize(value).expect("valid config remains valid");
            assert_eq!(
                serde_json::to_value(actual).expect("config JSON"),
                serde_json::to_value(original).expect("config JSON")
            );
        }
    }

    #[test]
    fn should_report_types_inside_optional_fields_and_arrays() {
        for (value, expected) in [
            (json!({"proxy": {"url": 12}}), "proxy.url: expected a JSON string"),
            (
                json!({"browser": {"endpoint": 12}}),
                "browser.endpoint: expected a JSON string",
            ),
            (
                json!({"ssrf": {"allowlist": [{"type": "exact", "value": 12}]}}),
                "ssrf.allowlist[0].value: expected a JSON string",
            ),
        ] {
            assert_eq!(deserialize(value).expect_err("invalid type").to_string(), expected);
        }
    }
}
