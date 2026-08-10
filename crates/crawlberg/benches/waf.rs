use crawlberg::http::HttpResponse;
use crawlberg::{TomlClassifier, WafClassifier};
use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;

/// Create a minimal HttpResponse for testing.
fn make_test_response(status: u16, body: String) -> HttpResponse {
    let body_bytes = body.as_bytes().to_vec();
    HttpResponse {
        status,
        content_type: "text/html; charset=utf-8".to_string(),
        body,
        body_bytes,
        headers: HashMap::new(),
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    }
}

/// Generate clean HTML with no WAF fingerprint markers.
/// Produces N bytes of varied HTML content.
fn generate_clean_html(target_bytes: usize) -> String {
    let fragment = "<p>This is clean content {}</p>\n";
    let mut result = String::with_capacity(target_bytes);
    let mut counter = 0;
    while result.len() < target_bytes {
        result.push_str(&fragment.replace("{}", &counter.to_string()));
        counter += 1;
    }
    result.truncate(target_bytes);
    result
}

fn bench_ac_build(c: &mut Criterion) {
    c.bench_function("waf_ac_build_builtin", |b| {
        b.iter(|| {
            let classifier = TomlClassifier::builtin();
            black_box(classifier);
        });
    });
}

fn bench_classify_clean(c: &mut Criterion) {
    let body = generate_clean_html(100_000);
    let response = make_test_response(200, body);
    let classifier = TomlClassifier::builtin();

    c.bench_function("waf_classify_clean_100kb", |b| {
        b.iter(|| {
            let _ = black_box(classifier.classify(black_box(&response)));
        });
    });
}

fn bench_classify_match_early(c: &mut Criterion) {
    let body = format!("<html><body>cf-chl-{}</body></html>", "x".repeat(99_000));
    let response = make_test_response(200, body);
    let classifier = TomlClassifier::builtin();

    c.bench_function("waf_classify_match_early_100kb", |b| {
        b.iter(|| {
            let _ = black_box(classifier.classify(black_box(&response)));
        });
    });
}

fn bench_classify_match_late(c: &mut Criterion) {
    let body = format!("{}cf-chl-</html>", "x".repeat(99_000));
    let response = make_test_response(200, body);
    let classifier = TomlClassifier::builtin();

    c.bench_function("waf_classify_match_late_100kb", |b| {
        b.iter(|| {
            let _ = black_box(classifier.classify(black_box(&response)));
        });
    });
}

criterion_group!(
    benches,
    bench_ac_build,
    bench_classify_clean,
    bench_classify_match_early,
    bench_classify_match_late
);
criterion_main!(benches);
