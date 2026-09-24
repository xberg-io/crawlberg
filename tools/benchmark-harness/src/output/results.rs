//! Writing benchmark results and per-fixture extraction output to disk.

use std::path::Path;

use ahash::AHashMap;

use crate::Result;
use crate::adapter::ScrapeOutput;
use crate::types::{BenchmarkOutput, ScrapeBenchmarkResult, ScrapeFixture};

/// Write the full benchmark output to `{output_dir}/results.json` as
/// pretty-printed JSON.
///
/// Creates `output_dir` if it does not exist.
///
/// # Errors
///
/// Returns [`crate::Error`] if the directory cannot be created or the file
/// cannot be written.
pub fn write_results(output_dir: &Path, output: &BenchmarkOutput) -> Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join("results.json");
    let json = serde_json::to_string_pretty(output)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Sanitize a fixture ID for use as a filename component.
///
/// Replaces characters that are unsafe in filesystem paths with `_`.
fn sanitize_fixture_id(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Write per-fixture extraction outputs to `{output_dir}/fixtures/`.
///
/// For each fixture that has a successful [`ScrapeOutput`], creates:
/// - `{fixture_id}.md` — extracted markdown (or a note when unavailable)
/// - `{fixture_id}.html` — raw HTML response
/// - `{fixture_id}.meta.json` — metadata (URL, status, quality scores, token
///   analysis, duration)
///
/// Fixtures whose output is `None` (failed scrapes) are skipped entirely.
///
/// This enables manual inspection of extracted content vs ground truth to
/// support debugging quality gaps and reaching 100% extraction coverage.
///
/// # Errors
///
/// Returns [`crate::Error`] if the directory cannot be created or any file
/// cannot be written.
pub fn write_fixture_outputs(
    output_dir: &Path,
    results: &[ScrapeBenchmarkResult],
    outputs: &[(String, Option<ScrapeOutput>)],
    fixtures: &[ScrapeFixture],
) -> Result<()> {
    let fixtures_dir = output_dir.join("fixtures");
    std::fs::create_dir_all(&fixtures_dir)?;

    let result_map: AHashMap<&str, &ScrapeBenchmarkResult> =
        results.iter().map(|r| (r.fixture_id.as_str(), r)).collect();
    let fixture_map: AHashMap<&str, &ScrapeFixture> = fixtures.iter().map(|f| (f.id.as_str(), f)).collect();

    for (fixture_id, maybe_output) in outputs {
        let Some(output) = maybe_output else {
            continue;
        };

        let safe_id = sanitize_fixture_id(fixture_id);
        let result = result_map.get(fixture_id.as_str()).copied();
        let fixture = fixture_map.get(fixture_id.as_str()).copied();

        let content_path = fixtures_dir.join(format!("{safe_id}.md"));
        let content_str = output
            .content
            .as_deref()
            .unwrap_or("<!-- content extraction was not available for this fixture -->\n");
        std::fs::write(&content_path, content_str)?;

        let html_path = fixtures_dir.join(format!("{safe_id}.html"));
        std::fs::write(&html_path, &output.html)?;

        let url = result.map(|r| r.url.as_str()).unwrap_or("");
        let status_code = output.status_code;
        let browser_used = output.browser_used;
        let content_size = output.content_size;
        let duration_ms = result.map(|r| r.duration_ms).unwrap_or(0.0);
        let truth_text = fixture.and_then(|f| f.truth_text.as_deref()).unwrap_or("");
        let lie_text = fixture.and_then(|f| f.lie_text.as_deref()).unwrap_or("");

        let quality_json = result
            .and_then(|r| r.quality.as_ref())
            .map(|q| {
                serde_json::json!({
                    "f1_text": q.f1_text,
                    "f1_numeric": q.f1_numeric,
                    "quality_score": q.quality_score,
                    "precision": q.precision,
                    "recall": q.recall,
                    "noise_penalty": q.noise_penalty,
                    "correct": q.correct,
                    "missing_tokens": q.missing_tokens,
                    "extra_tokens": q.extra_tokens,
                })
            })
            .unwrap_or(serde_json::Value::Null);

        let meta = serde_json::json!({
            "fixture_id": fixture_id,
            "url": url,
            "status_code": status_code,
            "browser_used": browser_used,
            "content_size": content_size,
            "truth_text": truth_text,
            "lie_text": lie_text,
            "quality": quality_json,
            "duration_ms": duration_ms,
        });

        let meta_path = fixtures_dir.join(format!("{safe_id}.meta.json"));
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BenchmarkConfig;
    use crate::output::test_support::make_result;

    #[test]
    fn test_write_results_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = BenchmarkConfig::default();
        let results: Vec<ScrapeBenchmarkResult> = vec![];
        let output = crate::output::aggregate_results(&results, &[], &config, "test-adapter");
        write_results(dir.path(), &output).unwrap();
        assert!(dir.path().join("results.json").exists());
    }

    #[test]
    fn test_write_results_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let config = BenchmarkConfig::default();
        let results = vec![make_result(true, 150.0, None)];
        let output = crate::output::aggregate_results(&results, &[], &config, "test-adapter");
        write_results(dir.path(), &output).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("results.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("results").is_some());
        assert!(parsed.get("metadata").is_some());
        assert!(parsed.get("performance_report").is_some());
    }
}
