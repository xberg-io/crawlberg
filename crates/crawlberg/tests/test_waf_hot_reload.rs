//! Integration test: TomlClassifier::watch atomically swaps rules on file change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crawlberg::http::HttpResponse;
use crawlberg::{TomlClassifier, WafClassifier, waf_rules_from_path, waf_rules_from_str};

fn make_response(status: u16, headers: Vec<(&str, &str)>, body: &str) -> HttpResponse {
    let mut header_map: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in headers {
        header_map.entry(k.to_lowercase()).or_default().push(v.to_string());
    }
    let body_bytes = body.as_bytes().to_vec();
    HttpResponse {
        status,
        content_type: "text/html".into(),
        body: body.to_string(),
        body_bytes,
        headers: header_map,
        browser_extras: None,
        final_url: "https://example.com/".into(),
        screenshot: None,
    }
}

const OLD_RULES_TOML: &str = r#"
[[fingerprint]]
id = "old_vendor_fp"
vendor = "oldvendor"
weight = 1.0

[[fingerprint.signals]]
kind = "body_substring"
pattern = "oldmarker"
"#;

const NEW_RULES_TOML: &str = r#"
[[fingerprint]]
id = "new_vendor_fp"
vendor = "newvendor"
weight = 1.0

[[fingerprint.signals]]
kind = "body_substring"
pattern = "newmarker"
"#;

#[tokio::test]
async fn toml_classifier_watch_swaps_rules_on_file_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.toml");

    std::fs::write(&path, OLD_RULES_TOML).unwrap();

    let initial_rules = waf_rules_from_str(OLD_RULES_TOML).unwrap();
    let classifier = Arc::new(TomlClassifier::from_rules(initial_rules));

    let old_resp = make_response(403, vec![], "oldmarker is present here");
    assert!(
        matches!(classifier.classify(&old_resp), Ok(Some(ref sig)) if sig.vendor == "oldvendor"),
        "initial classify must detect oldvendor"
    );

    let _handle = classifier.watch(&path).unwrap();

    std::fs::write(&path, NEW_RULES_TOML).unwrap();

    let new_resp = make_response(403, vec![], "newmarker is present here");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(ref sig)) = classifier.classify(&new_resp)
            && sig.vendor == "newvendor"
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "classifier did not swap to newvendor within 10 seconds; \
                 current classify result: {:?}",
                classifier.classify(&new_resp)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        matches!(classifier.classify(&old_resp), Ok(None)),
        "after swap, oldvendor must no longer match"
    );

    drop(_handle);
}

/// Dropping the WatchHandle while no file events are pending must not hang.
#[tokio::test]
async fn toml_classifier_watch_handle_drop_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.toml");
    std::fs::write(&path, OLD_RULES_TOML).unwrap();

    let rules = waf_rules_from_str(OLD_RULES_TOML).unwrap();
    let classifier = Arc::new(TomlClassifier::from_rules(rules));
    let handle = classifier.watch(&path).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(handle);
}

/// waf_rules_from_path returns an error for a non-existent file.
#[test]
fn load_from_path_returns_error_for_missing_file() {
    let result = waf_rules_from_path(std::path::Path::new("/nonexistent/path/rules.toml"));
    assert!(result.is_err(), "must error on missing file");
}
