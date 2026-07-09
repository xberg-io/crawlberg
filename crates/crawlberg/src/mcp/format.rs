//! MCP response formatting helpers.
//!
//! This module provides utilities for formatting crawl/scrape results
//! as text content for MCP tool responses.

use crate::types::{CrawlResult, MapResult, ScrapeResult};

/// Format a scrape result as a markdown string suitable for MCP text output.
///
/// Includes the page title, URL metadata, and the markdown-converted content
/// when available. Falls back to a truncated HTML preview otherwise.
pub(super) fn format_as_markdown(result: &ScrapeResult) -> String {
    let mut out = String::new();

    if let Some(ref title) = result.metadata.title {
        out.push_str(&format!("# {title}\n\n"));
    }

    out.push_str(&format!("**Status:** {}\n", result.status_code));
    out.push_str(&format!("**Content-Type:** {}\n", result.content_type));
    out.push_str(&format!("**Size:** {} bytes\n", result.body_size));

    if let Some(ref desc) = result.metadata.description {
        out.push_str(&format!("**Description:** {desc}\n"));
    }

    out.push('\n');

    if let Some(ref md) = result.markdown {
        out.push_str(&md.content);
    } else if !result.html.is_empty() {
        let preview_len = result.html.len().min(5000);
        out.push_str("```html\n");
        out.push_str(&result.html[..preview_len]);
        if result.html.len() > preview_len {
            out.push_str("\n... (truncated)");
        }
        out.push_str("\n```\n");
    }

    out
}

/// Format a scrape result as a pretty-printed JSON string.
pub fn format_as_json(result: &ScrapeResult) -> String {
    serde_json::to_string_pretty(result)
        .unwrap_or_else(|e| format!("{{\"error\": \"Failed to serialize result: {e}\"}}"))
}

/// Format a crawl result as a markdown summary.
///
/// Lists each crawled page with its URL, status, depth, and content preview.
pub(super) fn format_crawl_as_markdown(result: &CrawlResult) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "# Crawl Result\n\n**Pages crawled:** {}\n**Final URL:** {}\n\n",
        result.pages.len(),
        result.final_url
    ));

    for (i, page) in result.pages.iter().enumerate() {
        out.push_str(&format!(
            "## Page {} — {}\n\n**Status:** {} | **Depth:** {} | **Size:** {} bytes\n\n",
            i + 1,
            page.url,
            page.status_code,
            page.depth,
            page.body_size,
        ));

        if let Some(ref md) = page.markdown {
            let content_preview = if md.content.len() > 2000 {
                format!("{}...\n\n*(truncated)*", &md.content[..2000])
            } else {
                md.content.clone()
            };
            out.push_str(&content_preview);
            out.push_str("\n\n---\n\n");
        }
    }

    out
}

/// Format a crawl result as a pretty-printed JSON string.
pub(super) fn format_crawl_as_json(result: &CrawlResult) -> String {
    serde_json::to_string_pretty(result)
        .unwrap_or_else(|e| format!("{{\"error\": \"Failed to serialize crawl result: {e}\"}}"))
}

/// Format a map result as a text list of discovered URLs.
pub(super) fn format_map_result(result: &MapResult) -> String {
    let mut out = String::new();

    out.push_str(&format!("# Site Map\n\n**URLs discovered:** {}\n\n", result.urls.len()));

    for entry in &result.urls {
        out.push_str(&format!("- {}", entry.url));
        if let Some(ref lastmod) = entry.lastmod {
            out.push_str(&format!(" (modified: {lastmod})"));
        }
        if let Some(ref priority) = entry.priority {
            out.push_str(&format!(" [priority: {priority}]"));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MarkdownResult, PageMetadata};

    #[test]
    fn test_format_as_markdown_with_title() {
        let result = ScrapeResult {
            status_code: 200,
            content_type: "text/html".to_string(),
            body_size: 1024,
            metadata: PageMetadata {
                title: Some("Test Page".to_string()),
                description: Some("A test page".to_string()),
                ..Default::default()
            },
            markdown: Some(MarkdownResult {
                content: "Hello world".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let formatted = format_as_markdown(&result);
        assert!(formatted.contains("# Test Page"));
        assert!(formatted.contains("**Status:** 200"));
        assert!(formatted.contains("Hello world"));
        assert!(formatted.contains("A test page"));
    }

    #[test]
    fn test_format_as_markdown_falls_back_to_html() {
        let result = ScrapeResult {
            status_code: 200,
            content_type: "text/html".to_string(),
            html: "<p>Hello</p>".to_string(),
            body_size: 13,
            ..Default::default()
        };

        let formatted = format_as_markdown(&result);
        assert!(formatted.contains("<p>Hello</p>"));
        assert!(formatted.contains("```html"));
    }

    #[test]
    fn test_format_as_json_is_valid() {
        let result = ScrapeResult {
            status_code: 200,
            content_type: "text/html".to_string(),
            ..Default::default()
        };

        let formatted = format_as_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).expect("Should be valid JSON");
        assert_eq!(parsed["status_code"], 200);
    }

    #[test]
    fn test_format_map_result() {
        let result = MapResult {
            urls: vec![
                crate::types::SitemapUrl {
                    url: "https://example.com/".to_string(),
                    lastmod: Some("2025-01-01".to_string()),
                    priority: Some("1.0".to_string()),
                    ..Default::default()
                },
                crate::types::SitemapUrl {
                    url: "https://example.com/about".to_string(),
                    ..Default::default()
                },
            ],
        };

        let formatted = format_map_result(&result);
        assert!(formatted.contains("**URLs discovered:** 2"));
        assert!(formatted.contains("https://example.com/"));
        assert!(formatted.contains("modified: 2025-01-01"));
        assert!(formatted.contains("priority: 1.0"));
        assert!(formatted.contains("https://example.com/about"));
    }

    #[test]
    fn test_format_crawl_as_markdown() {
        let result = CrawlResult::default();
        let formatted = format_crawl_as_markdown(&result);
        assert!(formatted.contains("Crawl Result"));
        assert!(formatted.contains("**Pages crawled:** 0"));
    }
}
