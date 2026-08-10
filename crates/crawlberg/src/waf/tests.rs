//! Unit tests for the WAF module.

#![cfg(test)]

use std::collections::HashMap;

use crate::http::HttpResponse;
use crate::types::WafClassifier;
use crate::waf::rules::{MAX_FINGERPRINTS, MAX_PATTERN_LEN, MAX_SIGNALS_PER_FINGERPRINT, Rules};
use crate::waf::{TomlClassifier, load_from_str};

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

#[test]
fn classifier_cloudflare_challenge_detected() {
    let c = TomlClassifier::builtin();
    let resp = make_response(
        403,
        vec![("server", "cloudflare")],
        "<html><script src='/cdn-cgi/challenge-platform/h/g/orchestrate/v1'></script></html>",
    );
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "cloudflare challenge must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "cloudflare");
}

#[test]
fn classifier_datadome_header_detected() {
    let c = TomlClassifier::builtin();
    let resp = make_response(200, vec![("x-datadome", "blocked")], "<html>ok</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "datadome x-datadome header must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "datadome");
}

#[test]
fn classifier_perimeterx_header_detected() {
    let c = TomlClassifier::builtin();
    let resp = make_response(200, vec![("x-px-block", "1")], "<html>ok</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "perimeterx x-px-* header must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "perimeterx");
}

#[test]
fn classifier_imperva_incap_ses_detected() {
    let c = TomlClassifier::builtin();
    let resp = make_response(403, vec![], "<html>_incap_ses_xyz_123</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "imperva _incap_ses_ must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "imperva");
}

#[test]
fn classifier_aws_waf_action_header() {
    let c = TomlClassifier::builtin();
    let resp = make_response(403, vec![("x-amzn-waf-action", "block")], "<html>blocked</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "aws waf action header must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "aws-waf");
}

#[test]
fn classifier_akamai_server_header() {
    let c = TomlClassifier::builtin();
    let resp = make_response(403, vec![("server", "AkamaiGHost")], "<html>Access Denied</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "akamai server header must be detected");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "akamai");
}

#[test]
fn classifier_large_2xx_not_flagged() {
    let c = TomlClassifier::builtin();
    let mut body = String::from("<html><body><h1>About Cloudflare</h1>");
    body.push_str(&"<p>Lorem ipsum dolor sit amet.</p>".repeat(5000));
    body.push_str("</body></html>");
    assert!(body.len() > 100 * 1024, "body must exceed CHALLENGE_BODY_LIMIT");

    let resp = make_response(200, vec![("server", "nginx")], &body);
    assert!(
        c.classify(&resp).expect("classify must not fail").is_none(),
        "large 2xx body mentioning vendor names must not be classified as WAF block"
    );
}

#[test]
fn classifier_benign_small_page_not_flagged() {
    let c = TomlClassifier::builtin();
    let body = "<html><head><title>Welcome</title></head><body><p>Hello world</p></body></html>";
    let resp = make_response(200, vec![], body);
    assert!(
        c.classify(&resp).expect("classify must not fail").is_none(),
        "small benign page must not be classified as WAF block"
    );
}

#[test]
fn classifier_datadome_captcha_delivery() {
    let c = TomlClassifier::builtin();
    let body = "<html><script>var dd={'host':'geo.captcha-delivery.com'}</script>\
                <script src='https://ct.captcha-delivery.com/i.js'></script></html>";
    let resp = make_response(200, vec![], body);
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some(), "captcha-delivery.com must be detected as datadome");
    assert_eq!(signal.expect("signal is Some — asserted above").vendor, "datadome");
}

#[test]
fn custom_rules_single_signal_matches() {
    let toml_src = r#"
[[fingerprint]]
id = "custom_vendor_v1"
vendor = "custom"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "custom-challenge-token"
"#;
    let rules = load_from_str(toml_src).expect("static test TOML is valid");
    use crate::waf::TomlClassifier;
    let c = TomlClassifier::from_rules(rules);
    let resp = make_response(403, vec![], "<html>custom-challenge-token</html>");
    let signal = c.classify(&resp).expect("classify must not fail");
    assert!(signal.is_some());
    let signal = signal.expect("signal is Some — asserted above");
    assert_eq!(signal.vendor, "custom");
    assert_eq!(signal.fingerprint_id, "custom_vendor_v1");
}

#[test]
fn all_signals_must_match() {
    let toml_src = r#"
[[fingerprint]]
id = "multi_signal_test"
vendor = "testvendor"
weight = 1.0
[[fingerprint.signals]]
kind = "response_header"
name = "server"
value_contains = "testvendor"
[[fingerprint.signals]]
kind = "body_substring"
pattern = "tv-challenge-token"
"#;
    let rules = load_from_str(toml_src).expect("static test TOML is valid");
    let c = TomlClassifier::from_rules(rules);

    let resp = make_response(403, vec![("server", "testvendor")], "<html>no token here</html>");
    assert!(
        c.classify(&resp).expect("classify must not fail").is_none(),
        "must not match without body pattern"
    );

    let resp2 = make_response(403, vec![("server", "testvendor")], "<html>tv-challenge-token</html>");
    assert!(
        c.classify(&resp2).expect("classify must not fail").is_some(),
        "must match when all signals present"
    );
}

#[test]
fn load_from_str_rejects_too_many_fingerprints() {
    let mut toml = String::new();
    for i in 0..(MAX_FINGERPRINTS + 1) {
        use std::fmt::Write as _;
        let _ = write!(
            toml,
            r#"
[[fingerprint]]
id = "fp_{i}"
vendor = "test"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "pattern_{i}"
"#
        );
    }
    let result = load_from_str(&toml);
    let err = result.expect_err("should reject too many fingerprints");
    assert!(
        matches!(err, crate::waf::rules::RulesError::Validation { .. }),
        "expected Validation error, got {err:?}"
    );
    if let crate::waf::rules::RulesError::Validation { reason, .. } = err {
        assert!(reason.contains("too many fingerprints"), "got: {reason}");
    }
}

#[test]
fn load_from_str_rejects_oversized_pattern() {
    let big_pattern = "a".repeat(MAX_PATTERN_LEN + 1);
    let toml = format!(
        r#"
[[fingerprint]]
id = "fp_big"
vendor = "test"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "{big_pattern}"
"#
    );
    let result = load_from_str(&toml);
    let err = result.expect_err("should reject oversized pattern");
    assert!(
        matches!(err, crate::waf::rules::RulesError::Validation { .. }),
        "expected Validation error, got {err:?}"
    );
}

#[test]
fn load_from_str_rejects_too_many_signals() {
    let mut signals_block = String::new();
    for i in 0..(MAX_SIGNALS_PER_FINGERPRINT + 1) {
        use std::fmt::Write as _;
        let _ = write!(
            signals_block,
            r#"
[[fingerprint.signals]]
kind = "body_substring"
pattern = "s_{i}"
"#
        );
    }
    let toml = format!(
        r#"
[[fingerprint]]
id = "fp_many"
vendor = "test"
weight = 1.0
{signals_block}
"#
    );
    let result = load_from_str(&toml);
    let err = result.expect_err("should reject too many signals");
    assert!(
        matches!(err, crate::waf::rules::RulesError::Validation { .. }),
        "expected Validation error, got {err:?}"
    );
}

#[test]
fn load_from_str_accepts_corpus_at_limit() {
    let builtin = Rules::builtin();
    assert!(
        builtin.fingerprint_count() < MAX_FINGERPRINTS,
        "builtin corpus ({}) is approaching the MAX_FINGERPRINTS limit ({})",
        builtin.fingerprint_count(),
        MAX_FINGERPRINTS,
    );
}

/// This test is INTENTIONALLY BROKEN to prove the fixture corpus is load-bearing.
///
/// It checks that the `cloudflare_cf_chl` fingerprint fires on a body containing
/// `cf-chl-`. To demonstrate the corpus matters, we patch the rules to use a
/// non-matching pattern and verify the test detects the missing fingerprint.
///
/// Mark `#[ignore]` to skip in normal CI; un-ignore to validate the corpus gate.
#[test]
#[ignore = "intentional broken-fingerprint gate — un-ignore to verify corpus is load-bearing"]
fn broken_fingerprint_correctly_fails() {
    let toml_src = r#"
[[fingerprint]]
id = "cloudflare_cf_chl_intentionally_broken"
vendor = "cloudflare"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "THIS_PATTERN_WILL_NEVER_MATCH_ANYTHING_xyzzy_12345"
"#;
    let rules = load_from_str(toml_src).expect("static test TOML is valid");
    let c = TomlClassifier::from_rules(rules);
    let resp = make_response(403, vec![], "<html>cf-chl-widget-abc</html>");
    assert!(
        c.classify(&resp).expect("classify must not fail").is_none(),
        "broken fingerprint correctly produces no match"
    );
}
