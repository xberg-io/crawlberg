//! Quality scoring module for benchmark results.
//!
//! Computes TF1-based quality metrics by comparing extracted text against ground truth.
//! Uses token-level (bag-of-words) multiset precision and recall.
//!
//! # Scoring weights
//!
//! Text-only scoring uses a **0.6 / 0.4 text / numeric split**:
//!
//! ```text
//! quality_score = 0.6 * f1_text + 0.4 * f1_numeric
//! ```
//!
//! Numeric tokens receive disproportionate weight (40% despite typically being
//! a small fraction of the token count) because web scraped content often
//! contains prices, counts, and other numeric data where a single wrong digit
//! invalidates a table row or data point.
//!
//! # Tokenization
//!
//! Tokenization is intentionally simple: lowercase, split on whitespace,
//! strip non-alphanumeric characters except periods and commas embedded between
//! alphanumeric characters (preserving decimal numbers like "3.14" and European
//! format "3,14"). This preserves punctuation that is semantically meaningful
//! while ignoring decorative punctuation.

use std::sync::LazyLock;

use ahash::AHashMap;

use regex::Regex;

use crate::types::ScrapeQualityMetrics;

/// Regex to strip markdown image syntax `![alt](url)` → `alt`
static MD_IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\([^)]*\)").expect("invalid regex"));

/// Regex to strip markdown link syntax `[text](url)` → `text`
static MD_LINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("invalid regex"));

/// Strip markdown link and image syntax so URL components do not become tokens.
/// `![alt](url)` → `alt`, `[text](url)` → `text`.
fn strip_markdown_links(text: &str) -> String {
    let text = MD_IMAGE_RE.replace_all(text, "$1");
    MD_LINK_RE.replace_all(&text, "$1").into_owned()
}

/// Internal result of [`compute_quality`], exposing precision and recall.
struct QualityResult {
    f1_text: f64,
    f1_numeric: f64,
    quality_score: f64,
    precision: f64,
    recall: f64,
    missing_tokens: Vec<(String, usize)>,
    extra_tokens: Vec<(String, usize)>,
    correct: bool,
}

/// Compute TF1 quality metrics comparing extracted text against ground truth.
///
/// Algorithm:
/// 1. Tokenize both texts: lowercase, split on whitespace, strip non-alphanumeric chars except
///    periods and commas embedded between alphanumeric chars (e.g. "3.14", "3,14")
/// 2. Build token multisets (bag of words with counts)
/// 3. Compute precision = |intersection| / |extracted tokens|
/// 4. Compute recall = |intersection| / |ground truth tokens|
/// 5. F1 = 2 * precision * recall / (precision + recall)
///    - If both token sets are empty, F1 = 1.0 (vacuously perfect match)
/// 6. Separate F1 for all tokens vs numeric-only tokens
/// 7. quality_score = 0.6 * f1_text + 0.4 * f1_numeric
fn compute_quality(extracted: &str, ground_truth: &str) -> QualityResult {
    let extracted_tokens = tokenize(extracted);
    let truth_tokens = tokenize(ground_truth);

    let f1_text = compute_f1(&extracted_tokens, &truth_tokens);

    let extracted_numeric = filter_numeric(&extracted_tokens);
    let truth_numeric = filter_numeric(&truth_tokens);
    let f1_numeric = compute_f1(&extracted_numeric, &truth_numeric);

    let quality_score = if extracted_numeric.is_empty() && truth_numeric.is_empty() {
        f1_text
    } else {
        0.6 * f1_text + 0.4 * f1_numeric
    };

    let (precision, recall) = if extracted_tokens.is_empty() && truth_tokens.is_empty() {
        (1.0, 1.0)
    } else if extracted_tokens.is_empty() || truth_tokens.is_empty() {
        (0.0, 0.0)
    } else {
        let extracted_counts = build_counts(&extracted_tokens);
        let truth_counts = build_counts(&truth_tokens);
        let intersection: usize = truth_counts
            .iter()
            .map(|(token, &count)| {
                let ext_count = extracted_counts.get(token).copied().unwrap_or(0);
                ext_count.min(count)
            })
            .sum();
        (
            intersection as f64 / extracted_tokens.len() as f64,
            intersection as f64 / truth_tokens.len() as f64,
        )
    };

    let (missing_tokens, extra_tokens) = compute_token_diff(&extracted_tokens, &truth_tokens);

    let correct = quality_score >= 0.95;

    QualityResult {
        f1_text,
        f1_numeric,
        quality_score,
        precision,
        recall,
        missing_tokens,
        extra_tokens,
        correct,
    }
}

/// Compute quality for a web scrape result.
///
/// Uses TF1 (multiset token F1) comparing extracted text against `truth_text`.
/// When `lie_text` is provided, computes a noise penalty (fraction of lie tokens
/// found in extracted text).
///
/// Returns `None` if `truth_text` is `None` or empty after tokenization — there
/// is no scoreable signal without ground truth.
///
/// # Examples
///
/// ```
/// use benchmark_harness::quality::compute_scrape_quality;
///
/// let metrics = compute_scrape_quality(
///     "The quick brown fox jumps over the lazy dog",
///     Some("quick brown fox"),
///     Some("completely unrelated noise"),
/// )
/// .expect("ground truth provided");
///
/// assert!(metrics.f1_text > 0.0);
/// assert!(metrics.noise_penalty < 0.5);
/// ```
pub fn compute_scrape_quality(
    extracted: &str,
    truth_text: Option<&str>,
    lie_text: Option<&str>,
) -> Option<ScrapeQualityMetrics> {
    let truth = truth_text?;
    if truth.trim().is_empty() {
        return None;
    }

    let quality = compute_quality(extracted, truth);

    let noise_penalty = lie_text.map_or(0.0, |lie| {
        if lie.trim().is_empty() {
            return 0.0;
        }
        let lie_tokens = tokenize(lie);
        if lie_tokens.is_empty() {
            return 0.0;
        }
        let extracted_tokens = tokenize(extracted);
        let ext_counts = build_counts(&extracted_tokens);
        let found: usize = lie_tokens
            .iter()
            .filter(|t| ext_counts.contains_key(t.as_str()))
            .count();
        found as f64 / lie_tokens.len() as f64
    });

    Some(ScrapeQualityMetrics {
        f1_text: quality.f1_text,
        f1_numeric: quality.f1_numeric,
        quality_score: quality.quality_score,
        precision: quality.precision,
        recall: quality.recall,
        noise_penalty,
        missing_tokens: quality.missing_tokens,
        extra_tokens: quality.extra_tokens,
        correct: quality.correct,
    })
}

/// Tokenize text: lowercase, split on whitespace, strip non-alphanumeric characters
/// (preserving `.` and `,` only when embedded between alphanumeric chars, e.g. "3.14", "3,14")
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let text = strip_markdown_links(text);
    text.to_lowercase()
        .split_whitespace()
        .map(|w| {
            let kept: String = w
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '.' || *c == ',')
                .collect();
            kept.trim_matches(|c: char| c == '.' || c == ',').to_string()
        })
        .filter(|w| !w.is_empty())
        .map(|token| {
            let digit_count = token.chars().filter(|c| c.is_ascii_digit()).count();
            if digit_count <= 15 {
                if let Ok(num) = token.parse::<f64>() {
                    let normalized = format!("{num}");
                    if normalized != token { normalized } else { token }
                } else {
                    token
                }
            } else {
                token
            }
        })
        .collect()
}

/// Filter tokens to only those containing numeric characters (Unicode-aware).
fn filter_numeric(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .filter(|t| t.chars().any(|c| c.is_numeric()))
        .cloned()
        .collect()
}

/// Compute F1 score between two token bags using multiset intersection.
pub fn compute_f1(extracted: &[String], truth: &[String]) -> f64 {
    if extracted.is_empty() && truth.is_empty() {
        return 1.0;
    }
    if extracted.is_empty() || truth.is_empty() {
        return 0.0;
    }

    let extracted_counts = build_counts(extracted);
    let truth_counts = build_counts(truth);

    let intersection: usize = truth_counts
        .iter()
        .map(|(token, &count)| {
            let ext_count = extracted_counts.get(token).copied().unwrap_or(0);
            ext_count.min(count)
        })
        .sum();

    let precision = intersection as f64 / extracted.len() as f64;
    let recall = intersection as f64 / truth.len() as f64;

    if precision + recall == 0.0 {
        return 0.0;
    }

    2.0 * precision * recall / (precision + recall)
}

/// Build a token frequency map.
pub(crate) fn build_counts(tokens: &[String]) -> AHashMap<&str, usize> {
    let mut counts = AHashMap::new();
    for token in tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    counts
}

/// Compute token-level diff between extracted and ground truth token bags.
///
/// Returns `(missing_tokens, extra_tokens)` where:
/// - `missing_tokens`: tokens in GT with higher count than in extraction (recall misses)
/// - `extra_tokens`: tokens in extraction with higher count than in GT (precision misses)
///
/// Both are sorted by deficit/surplus count descending.
pub type TokenDiff = (Vec<(String, usize)>, Vec<(String, usize)>);

pub fn compute_token_diff(extracted: &[String], truth: &[String]) -> TokenDiff {
    let extracted_counts = build_counts(extracted);
    let truth_counts = build_counts(truth);

    let mut missing: Vec<(String, usize)> = truth_counts
        .iter()
        .filter_map(|(&token, &gt_count)| {
            let ext_count = extracted_counts.get(token).copied().unwrap_or(0);
            if gt_count > ext_count {
                Some((token.to_string(), gt_count - ext_count))
            } else {
                None
            }
        })
        .collect();
    missing.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut extra: Vec<(String, usize)> = extracted_counts
        .iter()
        .filter_map(|(&token, &ext_count)| {
            let gt_count = truth_counts.get(token).copied().unwrap_or(0);
            if ext_count > gt_count {
                Some((token.to_string(), ext_count - gt_count))
            } else {
                None
            }
        })
        .collect();
    extra.sort_by_key(|b| std::cmp::Reverse(b.1));

    (missing, extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_text() {
        let text = "Hello world this is a test";
        let result = compute_quality(text, text);
        assert!((result.f1_text - 1.0).abs() < 0.001);
        assert!((result.quality_score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_completely_different() {
        let result = compute_quality("alpha beta gamma", "one two three");
        assert_eq!(result.f1_text, 0.0);
    }

    #[test]
    fn test_partial_overlap() {
        let result = compute_quality("hello world foo", "hello world bar");
        assert!((result.f1_text - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_numeric_scoring() {
        let result = compute_quality("page 42 section 7", "page 42 section 7");
        assert!((result.f1_numeric - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_empty_inputs() {
        let result = compute_quality("", "");
        assert!((result.f1_text - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_empty_extracted() {
        let result = compute_quality("", "some ground truth");
        assert_eq!(result.f1_text, 0.0);
    }

    #[test]
    fn test_punctuation_stripped() {
        let result = compute_quality("hello, world!", "hello world");
        assert!((result.f1_text - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_case_insensitive() {
        let result = compute_quality("Hello World", "hello world");
        assert!((result.f1_text - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_tokenize_number_normalization() {
        let tokens_a = tokenize("15.0");
        let tokens_b = tokenize("15");
        assert_eq!(tokens_a, tokens_b, "15.0 and 15 should normalize to the same token");
        assert_eq!(tokens_a, vec!["15"]);

        assert_eq!(tokenize("100.00"), vec!["100"]);
    }

    #[test]
    fn test_compute_f1_number_equivalence() {
        let extracted = tokenize("price 15.0 dollars");
        let truth = tokenize("price 15 dollars");
        let f1 = compute_f1(&extracted, &truth);
        assert!(
            (f1 - 1.0).abs() < 0.001,
            "F1 should be 1.0 for semantically equivalent numeric tokens, got {f1}"
        );
    }

    #[test]
    fn test_tokenize_preserves_decimals() {
        assert_eq!(tokenize("3.14"), vec!["3.14"]);
        assert_eq!(tokenize("0.5"), vec!["0.5"]);
        assert_eq!(tokenize("12.345"), vec!["12.345"]);
    }

    #[test]
    fn test_no_numbers_no_boost() {
        let result = compute_quality("hello world foo", "hello world bar");
        let expected_text_f1 = 2.0 / 3.0;
        assert!(
            (result.f1_text - expected_text_f1).abs() < 0.001,
            "text F1 should be 2/3, got {}",
            result.f1_text
        );
        assert!(
            (result.quality_score - expected_text_f1).abs() < 0.001,
            "quality_score should equal text F1 ({expected_text_f1}) when no numbers, got {}",
            result.quality_score
        );
    }

    #[test]
    fn test_url_stripped_from_tokens() {
        let tokens = tokenize("[link text](https://example.com)");
        assert_eq!(tokens, vec!["link", "text"]);

        let tokens = tokenize("![alt text](https://example.com/image.png)");
        assert_eq!(tokens, vec!["alt", "text"]);

        let tokens = tokenize("See [docs](https://example.com/docs) for details");
        assert_eq!(tokens, vec!["see", "docs", "for", "details"]);
    }

    #[test]
    fn test_large_number_preserved() {
        let tokens = tokenize("10000000000000001");
        assert_eq!(
            tokens,
            vec!["10000000000000001"],
            "17-digit number should be preserved as-is, not rounded by f64"
        );

        let tokens = tokenize("12345678901234.0");
        assert_eq!(
            tokens,
            vec!["12345678901234"],
            "15-digit number with trailing .0 should still normalize"
        );
    }

    #[test]
    fn test_returns_none_when_no_truth() {
        assert!(compute_scrape_quality("some content", None, None).is_none());
        assert!(compute_scrape_quality("some content", None, Some("noise")).is_none());
    }

    #[test]
    fn test_returns_none_when_truth_empty() {
        assert!(compute_scrape_quality("some content", Some(""), None).is_none());
        assert!(compute_scrape_quality("some content", Some("   "), None).is_none());
    }

    #[test]
    fn test_full_truth_match_no_lie() {
        let metrics = compute_scrape_quality("quick brown fox", Some("quick brown fox"), None).unwrap();

        assert!((metrics.f1_text - 1.0).abs() < 0.001);
        assert!((metrics.quality_score - 1.0).abs() < 0.01);
        assert_eq!(metrics.noise_penalty, 0.0);
        assert!(metrics.correct);
    }

    #[test]
    fn test_no_truth_match() {
        let metrics = compute_scrape_quality("completely different text", Some("quick brown fox"), None).unwrap();

        assert_eq!(metrics.f1_text, 0.0);
        assert_eq!(metrics.quality_score, 0.0);
        assert!(!metrics.correct);
    }

    #[test]
    fn test_precision_and_recall_exposed() {
        let metrics = compute_scrape_quality("hello world foo", Some("hello world bar"), None).unwrap();

        assert!((metrics.precision - 2.0 / 3.0).abs() < 0.001);
        assert!((metrics.recall - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_noise_penalty_zero_when_no_lie() {
        let metrics = compute_scrape_quality("hello world", Some("hello world"), None).unwrap();
        assert_eq!(metrics.noise_penalty, 0.0);
    }

    #[test]
    fn test_noise_penalty_zero_when_lie_absent_from_extraction() {
        let metrics =
            compute_scrape_quality("good content here", Some("good content"), Some("spam garbage noise")).unwrap();

        assert_eq!(metrics.noise_penalty, 0.0);
    }

    #[test]
    fn test_noise_penalty_one_when_all_lie_present() {
        let metrics = compute_scrape_quality(
            "good content spam garbage noise",
            Some("good content"),
            Some("spam garbage noise"),
        )
        .unwrap();

        assert!((metrics.noise_penalty - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_noise_penalty_partial() {
        let metrics = compute_scrape_quality("good content spam", Some("good content"), Some("spam garbage")).unwrap();

        assert!((metrics.noise_penalty - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_noise_penalty_empty_lie_text() {
        let metrics = compute_scrape_quality("hello world", Some("hello world"), Some("")).unwrap();
        assert_eq!(metrics.noise_penalty, 0.0);
    }

    #[test]
    fn test_noise_penalty_case_insensitive() {
        let metrics = compute_scrape_quality("good content spam", Some("good content"), Some("SPAM")).unwrap();

        assert!((metrics.noise_penalty - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_missing_and_extra_tokens_populated() {
        let metrics = compute_scrape_quality("hello world foo", Some("hello world bar"), None).unwrap();

        assert!(metrics.missing_tokens.iter().any(|(t, _)| t == "bar"));
        assert!(metrics.extra_tokens.iter().any(|(t, _)| t == "foo"));
    }

    #[test]
    fn test_correct_flag_threshold() {
        let metrics = compute_scrape_quality("hello world", Some("hello world"), None).unwrap();
        assert!(metrics.correct);

        let metrics = compute_scrape_quality("alpha beta", Some("one two three"), None).unwrap();
        assert!(!metrics.correct);
    }
}
