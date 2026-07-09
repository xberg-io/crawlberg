//! Dataset download and management utilities.
//!
//! Handles fetching the scrape-evals dataset (or any compatible dataset) from
//! remote storage and unpacking it into the local fixture directory.

use crate::{
    Result,
    error::Error,
    fixture::{HfResponse, ScrapeFixtureRaw},
    types::ScrapeFixture,
};
use std::path::Path;
use std::time::Duration;

const DATASET_API_BASE: &str = "https://datasets-server.huggingface.co/rows?dataset=firecrawl/scrape-content-dataset-v1&config=default&split=train";
const PAGE_SIZE: u64 = 100;
const OUTPUT_FILENAME: &str = "scrape-evals.json";
const PARTIAL_FILENAME: &str = "scrape-evals.partial.json";
const HTTP_TIMEOUT_SECS: u64 = 30;
const MAX_PAGE_RETRIES: u32 = 3;

/// Fetch `url` with up to `max_retries` retry attempts on transient errors.
///
/// Uses exponential backoff: 1 s, 2 s, 4 s between attempts.
///
/// # Errors
///
/// Returns [`Error::Dataset`] if all attempts fail.
async fn fetch_with_retry(client: &reqwest::Client, url: &str, max_retries: u32) -> Result<String> {
    let mut last_error: Option<String> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay = Duration::from_secs(1 << (attempt - 1));
            eprintln!(
                "  retrying in {}s (attempt {}/{})",
                delay.as_secs(),
                attempt + 1,
                max_retries + 1
            );
            tokio::time::sleep(delay).await;
        }
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp
                    .text()
                    .await
                    .map_err(|e| Error::Dataset(format!("failed to read response body: {e}")));
            }
            Ok(resp) => {
                let status = resp.status();
                eprintln!("  page fetch returned {status}");
                last_error = Some(format!("HTTP {status}"));
            }
            Err(e) => {
                eprintln!("  page fetch failed: {e}");
                last_error = Some(format!("{e}"));
            }
        }
    }
    Err(Error::Dataset(format!(
        "failed after {} attempts: {}",
        max_retries + 1,
        last_error.unwrap_or_default()
    )))
}

/// Convert a raw HuggingFace fixture row into a [`ScrapeFixture`], defaulting
/// `split` to `"train"` when absent (as all downloaded rows belong to the
/// train split).
fn raw_to_fixture(raw: ScrapeFixtureRaw) -> ScrapeFixture {
    let mut fixture = ScrapeFixture::from(raw);
    if fixture.split.is_none() {
        fixture.split = Some(String::from("train"));
    }
    fixture
}

/// Download the scrape-evals dataset from the HuggingFace datasets-server API.
///
/// Fetches all rows in pages of 100 and converts them to [`ScrapeFixture`]
/// objects. The result is saved as a flat JSON array to
/// `{output_dir}/scrape-evals.json`.
///
/// If the output file already exists and `force` is `false`, the download is
/// skipped and the count is read from the existing file.
///
/// Progress is reported to stderr.
///
/// # Errors
///
/// Returns [`Error::Dataset`] on HTTP or I/O failures, and [`Error::Json`] on
/// JSON parse errors.
pub async fn download_scrape_evals(output_dir: &Path, force: bool) -> Result<usize> {
    let output_path = output_dir.join(OUTPUT_FILENAME);

    if output_path.exists() && !force {
        eprintln!(
            "scrape-evals: {} already exists, skipping download (use --force to re-download)",
            output_path.display()
        );
        let existing = std::fs::read_to_string(&output_path).map_err(|error| {
            Error::Dataset(format!(
                "failed to read existing dataset file {}: {error}",
                output_path.display()
            ))
        })?;
        let fixtures: Vec<ScrapeFixture> = serde_json::from_str(&existing).map_err(|error| {
            Error::Dataset(format!(
                "existing dataset file at {} is malformed (possibly from an interrupted download); \
                 re-run with --force to re-download: {error}",
                output_path.display()
            ))
        })?;
        return Ok(fixtures.len());
    }

    let client = reqwest::Client::builder()
        .user_agent("crawlberg-benchmark-harness/1.0")
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|error| Error::Dataset(format!("failed to build HTTP client: {error}")))?;

    std::fs::create_dir_all(output_dir).map_err(|error| {
        Error::Dataset(format!(
            "failed to create output directory {}: {error}",
            output_dir.display()
        ))
    })?;

    let partial_path = output_dir.join(PARTIAL_FILENAME);

    let mut all_fixtures: Vec<ScrapeFixture> = Vec::new();
    let mut offset: u64 = 0;
    let mut total_rows: Option<u64> = None;

    loop {
        let url = format!("{DATASET_API_BASE}&offset={offset}&length={PAGE_SIZE}");

        eprintln!("scrape-evals: fetching rows {offset}–{} ...", offset + PAGE_SIZE - 1);

        let body = fetch_with_retry(&client, &url, MAX_PAGE_RETRIES).await?;
        let page: HfResponse = serde_json::from_str(&body).map_err(|error| {
            Error::Dataset(format!(
                "failed to deserialize HuggingFace API response at offset {offset}: {error}"
            ))
        })?;

        if total_rows.is_none()
            && let Some(n) = page.num_rows_total
        {
            total_rows = Some(n);
            eprintln!("scrape-evals: {n} rows total");
        }

        let fetched = page.rows.len() as u64;
        all_fixtures.extend(page.rows.into_iter().map(|wrapper| raw_to_fixture(wrapper.row)));

        let partial_json = serde_json::to_string_pretty(&all_fixtures)?;
        std::fs::write(&partial_path, partial_json).map_err(|error| {
            Error::Dataset(format!(
                "failed to write partial dataset to {}: {error}",
                partial_path.display()
            ))
        })?;

        offset += fetched;

        if fetched == 0 || offset >= total_rows.unwrap_or(u64::MAX) {
            break;
        }
    }

    if all_fixtures.is_empty() {
        let _ = std::fs::remove_file(&partial_path);
        return Err(Error::Dataset("no fixtures downloaded from HuggingFace API".to_owned()));
    }

    eprintln!(
        "scrape-evals: downloaded {} fixtures, saving to {}",
        all_fixtures.len(),
        output_path.display()
    );

    std::fs::rename(&partial_path, &output_path).map_err(|error| {
        Error::Dataset(format!(
            "failed to finalize dataset file at {}: {error}",
            output_path.display()
        ))
    })?;

    Ok(all_fixtures.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_integer_id_converts_to_string_and_defaults_split() {
        let raw = ScrapeFixtureRaw {
            id: serde_json::Value::Number(serde_json::Number::from(99)),
            url: String::from("https://example.com"),
            truth_text: Some(String::from("hello")),
            lie_text: None,
            error: None,
            split: None,
            tags: Vec::new(),
            expected_status: None,
            verify_selectors: Vec::new(),
            verify_text: Vec::new(),
            category: None,
        };
        let fixture = raw_to_fixture(raw);
        assert_eq!(fixture.id, "99");
        assert_eq!(fixture.split.as_deref(), Some("train"));
    }

    #[test]
    fn test_raw_string_id_is_preserved() {
        let raw = ScrapeFixtureRaw {
            id: serde_json::Value::String(String::from("abc-123")),
            url: String::from("https://example.com"),
            truth_text: None,
            lie_text: None,
            error: None,
            split: None,
            tags: Vec::new(),
            expected_status: None,
            verify_selectors: Vec::new(),
            verify_text: Vec::new(),
            category: None,
        };
        let fixture = raw_to_fixture(raw);
        assert_eq!(fixture.id, "abc-123");
    }

    #[tokio::test]
    async fn test_skip_download_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join(OUTPUT_FILENAME);

        let fixtures = vec![ScrapeFixture {
            id: String::from("1"),
            url: String::from("https://example.com"),
            truth_text: None,
            lie_text: None,
            error: None,
            split: Some(String::from("train")),
            tags: Vec::new(),
            expected_status: None,
            verify_selectors: Vec::new(),
            verify_text: Vec::new(),
            category: None,
        }];
        std::fs::write(&output_path, serde_json::to_string(&fixtures).unwrap()).unwrap();

        let count = download_scrape_evals(dir.path(), false).await.unwrap();
        assert_eq!(count, 1);
    }
}
