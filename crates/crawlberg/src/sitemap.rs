//! Sitemap XML parsing and recursive fetching.
//!
//! Substrate-level surface for sitemap.xml — usable without the full crawl
//! engine. The synchronous parsers ([`parse_sitemap_xml`],
//! [`parse_sitemap_index`], [`is_sitemap_index`]) are public so OSS users
//! can build their own fetcher on top. The async recursive fetch helpers
//! (`fetch_sitemap_tree`, `process_sitemap_response`) remain engine-internal
//! because they depend on the engine's HTTP layer and config; substrate users
//! supply their own HTTP and call the parsers directly.
//!
//! ```
//! use crawlberg::sitemap::{parse_sitemap_xml, is_sitemap_index};
//!
//! let body = r#"<?xml version="1.0"?>
//! <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
//!   <url><loc>https://example.com/a</loc></url>
//!   <url><loc>https://example.com/b</loc></url>
//! </urlset>"#;
//! assert!(!is_sitemap_index(body));
//! let urls = parse_sitemap_xml(body);
//! assert_eq!(urls.len(), 2);
//! ```

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use url::Url;

use crate::http::http_fetch;
use crate::map::MapFilter;
use crate::normalize::{resolve_redirect, rewrite_url_host};
use crate::types::{CrawlConfig, SitemapUrl};

/// Parse a sitemap XML document and extract URL entries.
pub fn parse_sitemap_xml(body: &str) -> Vec<SitemapUrl> {
    let mut urls = Vec::new();

    let mut reader = Reader::from_str(body);
    let mut buf = Vec::new();
    let mut in_url = false;
    let mut in_loc = false;
    let mut in_lastmod = false;
    let mut in_changefreq = false;
    let mut in_priority = false;
    let mut current_loc = String::new();
    let mut current_lastmod: Option<String> = None;
    let mut current_changefreq: Option<String> = None;
    let mut current_priority: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                b"url" => {
                    in_url = true;
                    current_loc.clear();
                    current_lastmod = None;
                    current_changefreq = None;
                    current_priority = None;
                }
                b"loc" if in_url => in_loc = true,
                b"lastmod" if in_url => in_lastmod = true,
                b"changefreq" if in_url => in_changefreq = true,
                b"priority" if in_url => in_priority = true,
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"url" => {
                    if in_url && !current_loc.is_empty() {
                        urls.push(SitemapUrl {
                            url: current_loc.clone(),
                            lastmod: current_lastmod.clone(),
                            changefreq: current_changefreq.clone(),
                            priority: current_priority.clone(),
                        });
                    }
                    in_url = false;
                }
                b"loc" => in_loc = false,
                b"lastmod" => in_lastmod = false,
                b"changefreq" => in_changefreq = false,
                b"priority" => in_priority = false,
                _ => {}
            },
            Ok(Event::Text(ref e)) => {
                if let Ok(text) = e.xml_content(XmlVersion::default()) {
                    let text = text.trim().to_owned();
                    if in_loc {
                        current_loc = text;
                    } else if in_lastmod {
                        current_lastmod = Some(text);
                    } else if in_changefreq {
                        current_changefreq = Some(text);
                    } else if in_priority {
                        current_priority = Some(text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    urls
}

/// Parse a sitemap index XML document and return the child sitemap URLs.
pub fn parse_sitemap_index(body: &str) -> Vec<String> {
    let mut child_urls = Vec::new();
    let mut reader = Reader::from_str(body);
    let mut buf = Vec::new();
    let mut in_sitemap = false;
    let mut in_loc = false;
    let mut current_loc = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"sitemap" => {
                    in_sitemap = true;
                    current_loc.clear();
                }
                b"loc" if in_sitemap => in_loc = true,
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"sitemap" => {
                    if in_sitemap && !current_loc.is_empty() {
                        child_urls.push(current_loc.clone());
                    }
                    in_sitemap = false;
                }
                b"loc" => in_loc = false,
                _ => {}
            },
            Ok(Event::Text(ref e)) => {
                if in_loc && let Ok(text) = e.xml_content(XmlVersion::default()) {
                    current_loc = text.trim().to_owned();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    child_urls
}

/// Check whether the body looks like a sitemap index (contains `<sitemapindex`).
pub fn is_sitemap_index(body: &str) -> bool {
    body.contains("<sitemapindex") || body.contains("<sitemapindex>")
}

/// Recursively fetch a sitemap tree, following sitemap index references.
///
/// If the URL points to a sitemap index, fetches each child sitemap and
/// collects all URL entries. Handles gzip-compressed sitemaps.
///
/// `filter` and `limit` are applied incrementally as entries are parsed: only
/// matching URLs are retained, and fetching stops once `limit` matching URLs
/// have been collected. This bounds both peak memory and network work on large
/// sitemap-index trees.
pub(crate) async fn fetch_sitemap_tree(
    sitemap_url: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    filter: &MapFilter,
    limit: Option<usize>,
) -> Vec<SitemapUrl> {
    let resp = match http_fetch(sitemap_url, config, &std::collections::HashMap::new(), client).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    process_sitemap_response(
        sitemap_url,
        &resp.body,
        &resp.body_bytes,
        &resp.content_type,
        config,
        client,
        filter,
        limit,
    )
    .await
}

/// Process an already-fetched sitemap response body, following sitemap index
/// references if needed. Avoids re-fetching a URL that was already retrieved.
///
/// `filter` and `limit` bound the result incrementally: entries that do not
/// match `filter` are discarded as they are parsed, and both child-sitemap
/// fetching and per-child parsing stop once `limit` matching URLs are collected.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_sitemap_response(
    sitemap_url: &str,
    body: &str,
    body_bytes: &[u8],
    content_type: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    filter: &MapFilter,
    limit: Option<usize>,
) -> Vec<SitemapUrl> {
    let decompressed;
    let xml_body = if content_type.contains("gzip") || content_type.contains("x-gzip") {
        match decompress_gzip(body_bytes) {
            Ok(d) => {
                decompressed = d;
                &decompressed
            }
            Err(_) => body,
        }
    } else {
        body
    };

    let reached_limit = |len: usize| limit.is_some_and(|limit| len >= limit);

    if is_sitemap_index(xml_body) {
        let child_urls = parse_sitemap_index(xml_body);
        let base = Url::parse(sitemap_url).ok();
        let mut all_urls = Vec::new();
        let max_children = 100;
        for child_url in child_urls.iter().take(max_children) {
            if reached_limit(all_urls.len()) {
                break;
            }
            let resolved = if let Some(ref base_parsed) = base {
                if Url::parse(child_url).is_ok() {
                    rewrite_url_host(child_url, base_parsed)
                } else {
                    resolve_redirect(sitemap_url, child_url)
                }
            } else {
                child_url.clone()
            };
            let child_resp = match http_fetch(&resolved, config, &std::collections::HashMap::new(), client).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            let child_body = &child_resp.body;
            let child_decompressed;
            let child_xml = if child_resp.content_type.contains("gzip") {
                match decompress_gzip(&child_resp.body_bytes) {
                    Ok(d) => {
                        child_decompressed = d;
                        &child_decompressed
                    }
                    Err(_) => child_body,
                }
            } else {
                child_body
            };
            for entry in parse_sitemap_xml(child_xml) {
                if !filter.matches(&entry.url) {
                    continue;
                }
                all_urls.push(entry);
                if reached_limit(all_urls.len()) {
                    break;
                }
            }
        }
        all_urls
    } else {
        let mut urls = Vec::new();
        for entry in parse_sitemap_xml(xml_body) {
            if !filter.matches(&entry.url) {
                continue;
            }
            urls.push(entry);
            if reached_limit(urls.len()) {
                break;
            }
        }
        urls
    }
}

/// Decompress gzip-encoded data into a UTF-8 string.
///
/// Limits decompressed output to 50 MB to prevent gzip bomb attacks.
pub(crate) fn decompress_gzip(data: &[u8]) -> Result<String, std::io::Error> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    const MAX_DECOMPRESSED_SIZE: u64 = 50 * 1024 * 1024;

    let decoder = GzDecoder::new(data);
    let mut limited = decoder.take(MAX_DECOMPRESSED_SIZE);
    let mut result = String::new();
    limited.read_to_string(&mut result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::MapFilter;
    use crate::types::CrawlConfig;

    fn urlset(count: usize) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for i in 0..count {
            body.push_str(&format!("<url><loc>https://example.com/page-{i}</loc></url>"));
        }
        body.push_str("</urlset>");
        body
    }

    #[tokio::test]
    async fn process_sitemap_response_stops_at_limit_on_single_sitemap() {
        let config = CrawlConfig::default();
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = urlset(1000);

        let urls = process_sitemap_response(
            "https://example.com/sitemap.xml",
            &body,
            body.as_bytes(),
            "application/xml",
            &config,
            &client,
            &filter,
            Some(10),
        )
        .await;

        assert_eq!(
            urls.len(),
            10,
            "map_limit must cap the parsed URLs, not just the returned slice"
        );
    }

    #[tokio::test]
    async fn process_sitemap_response_returns_all_when_unlimited() {
        let config = CrawlConfig::default();
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = urlset(25);

        let urls = process_sitemap_response(
            "https://example.com/sitemap.xml",
            &body,
            body.as_bytes(),
            "application/xml",
            &config,
            &client,
            &filter,
            None,
        )
        .await;

        assert_eq!(urls.len(), 25);
    }

    #[tokio::test]
    async fn process_sitemap_response_applies_filter_before_limit() {
        let config = CrawlConfig {
            map_search: Some("keep".to_string()),
            ..CrawlConfig::default()
        };
        let filter = MapFilter::from_config(&config).unwrap();
        let client = reqwest::Client::new();
        let body = concat!(
            r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#,
            "<url><loc>https://example.com/keep-1</loc></url>",
            "<url><loc>https://example.com/drop-1</loc></url>",
            "<url><loc>https://example.com/keep-2</loc></url>",
            "<url><loc>https://example.com/drop-2</loc></url>",
            "</urlset>",
        );

        let urls = process_sitemap_response(
            "https://example.com/sitemap.xml",
            body,
            body.as_bytes(),
            "application/xml",
            &config,
            &client,
            &filter,
            None,
        )
        .await;

        assert_eq!(urls.len(), 2);
        assert!(urls.iter().all(|entry| entry.url.contains("keep")));
    }
}
