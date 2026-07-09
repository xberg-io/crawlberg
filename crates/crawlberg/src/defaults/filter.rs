//! Content filters for post-extraction page filtering.
#![allow(dead_code)]

use ahash::AHashMap;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::ContentFilter;
use crate::types::CrawlPageResult;

/// A content filter that passes everything through without modification.
#[derive(Debug, Clone, Default)]
pub struct NoopFilter;

#[async_trait]
impl ContentFilter for NoopFilter {
    async fn filter(&self, page: CrawlPageResult) -> Result<Option<CrawlPageResult>, CrawlError> {
        Ok(Some(page))
    }
}

/// A BM25-based content filter that scores pages against a query.
///
/// Pages with a BM25 score below the threshold are filtered out.
/// Uses the page's HTML text content for scoring.
#[derive(Debug, Clone)]
pub struct Bm25Filter {
    query_terms: Vec<String>,
    threshold: f64,
    k1: f64,
    b: f64,
}

impl Bm25Filter {
    /// Create a new BM25 filter with the given query and score threshold.
    ///
    /// - `query`: space-separated search terms
    /// - `threshold`: minimum BM25 score (pages below this are filtered out)
    pub fn new(query: &str, threshold: f64) -> Self {
        let query_terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        Self {
            query_terms,
            threshold,
            k1: 1.5,
            b: 0.75,
        }
    }
}

/// Score a page's HTML content against query terms using BM25 TF-saturation.
///
/// Since we operate on individual pages without corpus statistics, IDF is set to
/// 1.0 and `avgdl` equals the current document length.  This simplifies BM25 to
/// a per-term TF-saturation score normalised by the number of query terms.
///
/// Avoids allocating a `Vec<String>` for tokens by streaming over the lowercased
/// text and counting term frequencies inline.
fn score_page(html: &str, query_terms: &[String], k1: f64, _b: f64) -> f64 {
    if query_terms.is_empty() {
        return 1.0;
    }

    let lower = html.to_lowercase();

    let mut tf: AHashMap<&str, usize> = AHashMap::new();
    let mut doc_len: usize = 0;

    for token in lower.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()) {
        doc_len += 1;
        for qt in query_terms {
            if token == qt.as_str() {
                *tf.entry(qt.as_str()).or_insert(0) += 1;
            }
        }
    }

    if doc_len == 0 {
        return 0.0;
    }

    let mut score = 0.0;
    for qt in query_terms {
        let freq = *tf.get(qt.as_str()).unwrap_or(&0) as f64;
        if freq > 0.0 {
            let tf_score = (freq * (k1 + 1.0)) / (freq + k1);
            score += tf_score;
        }
    }

    score / query_terms.len() as f64
}

#[async_trait]
impl ContentFilter for Bm25Filter {
    async fn filter(&self, page: CrawlPageResult) -> Result<Option<CrawlPageResult>, CrawlError> {
        if self.query_terms.is_empty() {
            return Ok(Some(page));
        }

        let score = score_page(&page.html, &self.query_terms, self.k1, self.b);
        if score >= self.threshold {
            Ok(Some(page))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_empty_query() {
        assert_eq!(score_page("some text", &[], 1.5, 0.75), 1.0);
    }

    #[test]
    fn test_score_empty_doc() {
        let terms = vec!["rust".to_string()];
        assert_eq!(score_page("", &terms, 1.5, 0.75), 0.0);
    }

    #[test]
    fn test_score_matching_term() {
        let terms = vec!["rust".to_string()];
        let score = score_page("Rust is a great language. Rust is fast.", &terms, 1.5, 0.75);
        assert!(score > 0.0);
    }

    #[test]
    fn test_score_no_match() {
        let terms = vec!["python".to_string()];
        let score = score_page("Rust is a great language.", &terms, 1.5, 0.75);
        assert_eq!(score, 0.0);
    }

    #[tokio::test]
    async fn test_bm25_filter_passes_relevant() {
        let filter = Bm25Filter::new("rust language", 0.1);
        let page = CrawlPageResult {
            html: "Rust is a great systems programming language".to_string(),
            ..Default::default()
        };
        let result = filter.filter(page).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_bm25_filter_rejects_irrelevant() {
        let filter = Bm25Filter::new("quantum physics", 0.5);
        let page = CrawlPageResult {
            html: "Rust is a great systems programming language".to_string(),
            ..Default::default()
        };
        let result = filter.filter(page).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_bm25_filter_empty_query_passes_all() {
        let filter = Bm25Filter::new("", 0.5);
        let page = CrawlPageResult {
            html: "anything".to_string(),
            ..Default::default()
        };
        let result = filter.filter(page).await.unwrap();
        assert!(result.is_some());
    }
}
