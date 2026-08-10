//! Regression tests for TOML validation gates and classify edge cases.
//!
//! Covers:
//!   A. MAX_FINGERPRINTS, MAX_PATTERN_LEN, MAX_SIGNALS_PER_FINGERPRINT gates
//!      (added in commit 5c7ac2a8b). A regression disabling any one gate would
//!      land silently without these tests.
//!   B. classify edge cases: empty body, non-UTF8 body bytes.

use std::collections::HashMap;

use crawlberg::http::HttpResponse;
use crawlberg::{TomlClassifier, WafClassifier, WafRulesError, waf_rules_from_str};

const MAX_FINGERPRINTS: usize = 1_000;
const MAX_PATTERN_LEN: usize = 4_096;
const MAX_SIGNALS_PER_FINGERPRINT: usize = 16;

fn make_response(status: u16, body: &str, body_bytes: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "text/html".into(),
        body: body.to_string(),
        body_bytes,
        headers: HashMap::new(),
        browser_extras: None,
        final_url: "https://example.com/".into(),
        screenshot: None,
    }
}

fn make_simple_response(status: u16, body: &str) -> HttpResponse {
    let body_bytes = body.as_bytes().to_vec();
    make_response(status, body, body_bytes)
}

/// Build a TOML string with `n` distinct fingerprints, each with one
/// `body_substring` signal.
fn build_fingerprints_toml(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&format!(
            "\n[[fingerprint]]\nid = \"fp_{i}\"\nvendor = \"test\"\nweight = 1.0\n\
             [[fingerprint.signals]]\nkind = \"body_substring\"\npattern = \"pat{i}\"\n"
        ));
    }
    out
}

/// Build a TOML string with one fingerprint that has `n` signals.
/// Uses `response_header` (no pattern field) to keep the TOML compact.
fn build_many_signals_toml(n: usize) -> String {
    let mut out = String::from("\n[[fingerprint]]\nid = \"fp_0\"\nvendor = \"test\"\nweight = 1.0\n");
    for i in 0..n {
        out.push_str(&format!(
            "\n[[fingerprint.signals]]\nkind = \"response_header\"\nname = \"x-hdr-{i}\"\n"
        ));
    }
    out
}

#[test]
fn load_from_str_rejects_more_than_max_fingerprints() {
    let toml = build_fingerprints_toml(MAX_FINGERPRINTS + 1);
    let result = waf_rules_from_str(&toml);
    match result {
        Err(WafRulesError::Validation { reason, .. }) => {
            assert!(
                reason.contains(&(MAX_FINGERPRINTS + 1).to_string())
                    || reason.contains("MAX_FINGERPRINTS")
                    || reason.contains("too many"),
                "reason should mention the count or limit, got: {reason}"
            );
        }
        other => panic!("expected Err(Validation), got: {other:?}"),
    }
}

#[test]
fn load_from_str_rejects_pattern_longer_than_max() {
    let long_pattern = "x".repeat(MAX_PATTERN_LEN + 1);
    let toml = format!(
        "\n[[fingerprint]]\nid = \"fp_long\"\nvendor = \"test\"\nweight = 1.0\n\
         [[fingerprint.signals]]\nkind = \"body_substring\"\npattern = \"{long_pattern}\"\n"
    );
    let result = waf_rules_from_str(&toml);
    match result {
        Err(WafRulesError::Validation { reason, .. }) => {
            assert!(
                reason.contains("MAX_PATTERN_LEN") || reason.contains("too long") || reason.contains("pattern"),
                "reason should describe the pattern length violation, got: {reason}"
            );
        }
        other => panic!("expected Err(Validation), got: {other:?}"),
    }
}

#[test]
fn load_from_str_rejects_more_than_max_signals_per_fingerprint() {
    let toml = build_many_signals_toml(MAX_SIGNALS_PER_FINGERPRINT + 1);
    let result = waf_rules_from_str(&toml);
    match result {
        Err(WafRulesError::Validation { reason, .. }) => {
            assert!(
                reason.contains("MAX_SIGNALS_PER_FINGERPRINT")
                    || reason.contains("too many signals")
                    || reason.contains("too many"),
                "reason should mention signal count violation, got: {reason}"
            );
        }
        other => panic!("expected Err(Validation), got: {other:?}"),
    }
}

#[test]
fn classify_returns_ok_none_for_empty_body() {
    let classifier = TomlClassifier::builtin();
    let response = make_simple_response(200, "");
    let result = classifier.classify(&response);
    assert!(
        matches!(result, Ok(None)),
        "empty body must return Ok(None), got: {result:?}"
    );
}

#[test]
fn classify_does_not_panic_on_non_utf8_body_bytes() {
    let classifier = TomlClassifier::builtin();
    let body_bytes: Vec<u8> = vec![0xFF, 0xFE, 0x80, 0x81, 0x82, 0x83, 0xC0, 0xC1, 0xFF];
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    let response = make_response(200, &body, body_bytes);
    let result = classifier.classify(&response);
    assert!(
        result.is_ok(),
        "classify must return Ok(_) for non-UTF8 bytes, got: {result:?}"
    );
}
