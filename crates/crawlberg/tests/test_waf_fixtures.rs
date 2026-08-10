#![allow(clippy::print_stdout)]
//! Fixture-replay integration tests for WAF fingerprint detection.
//!
//! Each `.http` file under `tests/fixtures/waf/` is parsed into an
//! [`HttpResponse`] and run through [`TomlClassifier::builtin`]. The test
//! asserts that:
//!
//! 1. The classifier returns `Some(WafSignal)`.
//! 2. `signal.vendor` matches the vendor derived from the filename prefix.
//!
//! Filename convention: `<vendor>_<scenario>_<n>.http`
//! The vendor prefix is everything before the first non-vendor `_`.
//! Multi-word vendors use `_` as separator in filenames, with a canonical
//! mapping to TOML vendor strings defined in `VENDOR_MAP` below.
//!
//! See `tests/fixtures/waf/README.md` for format details.

use std::collections::HashMap;
use std::path::Path;

use crawlberg::{TomlClassifier, WafClassifier};

use crawlberg::http::HttpResponse;

/// Maps filename vendor prefix → canonical TOML vendor string.
///
/// Filename: `cloudflare_*` → vendor `cloudflare`
/// Filename: `datadome_*` → vendor `datadome`
/// Filename: `perimeterx_*` → vendor `perimeterx`
/// Filename: `imperva_*` → vendor `imperva`
/// Filename: `incapsula_*` → vendor `imperva`  (same vendor as Imperva)
/// Filename: `aws_waf_*` → vendor `aws-waf`
/// Filename: `generic_*` → vendor `generic`
fn expected_vendor(filename_stem: &str) -> &'static str {
    if filename_stem.starts_with("aws_waf_") {
        return "aws-waf";
    }
    let prefix = filename_stem.split('_').next().unwrap_or("");
    match prefix {
        "cloudflare" => "cloudflare",
        "datadome" => "datadome",
        "perimeterx" => "perimeterx",
        "imperva" | "incapsula" => "imperva",
        "generic" => "generic",
        other => panic!("unknown vendor prefix '{other}' in fixture file name '{filename_stem}'"),
    }
}

fn parse_http_fixture(content: &str) -> HttpResponse {
    let mut lines = content.lines();

    let status_line = lines.next().expect("fixture must have a status line");
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    assert!(parts.len() >= 2, "malformed status line: '{status_line}'");
    let status: u16 = parts[1]
        .parse()
        .unwrap_or_else(|_| panic!("bad status code in '{status_line}'"));

    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers_map
                .entry(name.trim().to_lowercase())
                .or_default()
                .push(value.trim().to_string());
        }
    }

    let body: String = lines.collect::<Vec<_>>().join("\n");
    let body_bytes = body.as_bytes().to_vec();

    HttpResponse {
        status,
        content_type: headers_map
            .get("content-type")
            .and_then(|v| v.first().cloned())
            .unwrap_or_default(),
        body,
        body_bytes,
        headers: headers_map,
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    }
}

#[test]
fn waf_fixtures_all_match() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/waf");
    let classifier = TomlClassifier::builtin();

    let mut fixture_count = 0;
    let mut failures: Vec<String> = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(&fixtures_dir)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {e}", fixtures_dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "http").unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let content = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        let response = parse_http_fixture(&content);
        let signal = classifier.classify(&response).unwrap_or_else(|e| {
            panic!("classifier returned error for {stem}: {e}");
        });

        fixture_count += 1;
        let vendor = expected_vendor(&stem);

        match signal {
            None => {
                failures.push(format!("{stem}: no signal (expected vendor={vendor})"));
            }
            Some(s) if s.vendor != vendor => {
                failures.push(format!("{stem}: wrong vendor (expected={vendor}, got={})", s.vendor));
            }
            Some(_) => {}
        }
    }

    assert!(
        fixture_count > 0,
        "no .http fixtures found under {}",
        fixtures_dir.display()
    );

    if !failures.is_empty() {
        panic!(
            "{}/{fixture_count} fixtures failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    println!("waf_fixtures_all_match: {fixture_count} fixtures verified");
}
