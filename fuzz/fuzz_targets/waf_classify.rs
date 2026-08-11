//! libFuzzer target: WAF classifier robustness against random HTTP responses.
#![no_main]

use crawlberg::{TomlClassifier, WafClassifier, http::HttpResponse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&status_byte, rest)) = data.split_first() else {
        return;
    };
    let status = 200u16 + (status_byte as u16 % 400);

    let body = String::from_utf8_lossy(rest).into_owned();
    let body_bytes = rest.to_vec();

    let response = HttpResponse {
        status,
        content_type: String::new(),
        body,
        body_bytes,
        headers: std::collections::HashMap::new(),
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    };

    let classifier = TomlClassifier::builtin();
    let _ = classifier.classify(&response);
});
