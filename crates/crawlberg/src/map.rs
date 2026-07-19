//! Site mapping operation that discovers URLs via sitemaps and link extraction.

use std::collections::HashSet;

use regex::Regex;
use tl::ParserOptions;
use url::Url;

use crate::error::CrawlError;
use crate::html::{extract_links, is_html_content};
use crate::http::{build_client, fetch_with_retry, http_fetch};
use crate::normalize::{normalize_url, resolve_redirect, rewrite_url_host, robots_url, strip_fragment};
use crate::robots::parse_robots_txt;
use crate::sitemap::{
    decompress_gzip, fetch_sitemap_tree, is_sitemap_index, parse_sitemap_xml, process_sitemap_response,
};
use crate::types::{CrawlConfig, LinkType, MapResult, SitemapUrl};

/// Map a website to discover its URLs.
///
/// Tries the following strategies in order:
/// 1. Robots.txt sitemap directives (if `respect_robots_txt` is enabled)
/// 2. `/sitemap.xml` fallback
/// 3. Direct fetch of the URL (handles XML sitemaps, gzip, or HTML link extraction)
///
/// Applies `exclude_paths`, `map_search`, and `map_limit` filters to the result.
///
/// `map_limit` bounds both the returned length and the work performed: it is
/// threaded into the sitemap fetch loop so a large sitemap-index tree is not
/// fully materialized before truncation. Peak memory is bounded to roughly the
/// limit plus a single child sitemap.
pub async fn map(url: &str, config: &CrawlConfig) -> Result<MapResult, CrawlError> {
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::Other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let filter = MapFilter::from_config(config)?;

    if config.respect_robots_txt {
        let robots = robots_url(&parsed_url);
        if let Ok(robots_resp) = http_fetch(&robots, config, &std::collections::HashMap::new(), &client).await {
            let ua = config.user_agent.as_deref().unwrap_or("*");
            let rules = parse_robots_txt(&robots_resp.body, ua);
            if !rules.sitemaps.is_empty() {
                let mut all_urls = Vec::new();
                for sitemap_ref in &rules.sitemaps {
                    if let Some(limit) = config.map_limit
                        && all_urls.len() >= limit
                    {
                        break;
                    }
                    let sitemap_url = resolve_redirect(url, sitemap_ref);
                    let resolved = rewrite_url_host(&sitemap_url, &parsed_url);
                    let remaining = config.map_limit.map(|limit| limit.saturating_sub(all_urls.len()));
                    all_urls.extend(fetch_sitemap_tree(&resolved, config, &client, &filter, remaining).await);
                }
                if !all_urls.is_empty() {
                    return Ok(filter_map_result(all_urls, &filter, config.map_limit));
                }
            }
        }
    }

    let sitemap_url = format!("{}://{}/sitemap.xml", parsed_url.scheme(), parsed_url.authority());
    if let Ok(sitemap_resp) = http_fetch(&sitemap_url, config, &std::collections::HashMap::new(), &client).await
        && (sitemap_resp.body.contains("<urlset") || sitemap_resp.body.contains("<sitemapindex"))
    {
        let urls = process_sitemap_response(
            &sitemap_url,
            &sitemap_resp.body,
            &sitemap_resp.body_bytes,
            &sitemap_resp.content_type,
            config,
            &client,
            &filter,
            config.map_limit,
        )
        .await;
        if !urls.is_empty() {
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
    }

    let resp = fetch_with_retry(url, config, &std::collections::HashMap::new(), &client).await?;

    let is_xml = resp.content_type.contains("xml") || resp.body.trim_start().starts_with("<?xml");

    let is_gzip = resp.content_type.contains("gzip")
        || resp.content_type.contains("x-gzip")
        || url.to_lowercase().ends_with(".gz")
        || (resp.body_bytes.len() >= 2 && resp.body_bytes[0] == 0x1f && resp.body_bytes[1] == 0x8b);
    if is_gzip && let Ok(decompressed) = decompress_gzip(&resp.body_bytes) {
        let urls = parse_sitemap_xml(&decompressed);
        if !urls.is_empty() {
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
    }

    if is_xml {
        if is_sitemap_index(&resp.body) {
            let urls = fetch_sitemap_tree(url, config, &client, &filter, config.map_limit).await;
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
        let urls = parse_sitemap_xml(&resp.body);
        if !urls.is_empty() {
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
    }

    if is_html_content(&resp.content_type, &resp.body)
        && let Ok(doc) = tl::parse(&resp.body, ParserOptions::default())
    {
        let links = extract_links(&doc, &parsed_url);
        let mut url_set: Vec<SitemapUrl> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for link in &links {
            if link.link_type == LinkType::Internal || link.link_type == LinkType::Document {
                let norm = normalize_url(&link.url);
                if seen.insert(norm.clone()) {
                    let clean = strip_fragment(&link.url);
                    url_set.push(SitemapUrl {
                        url: clean,
                        lastmod: None,
                        changefreq: None,
                        priority: None,
                    });
                }
            } else if link.link_type == LinkType::External {
                let norm = normalize_url(&link.url);
                if seen.insert(norm) {
                    url_set.push(SitemapUrl {
                        url: link.url.clone(),
                        lastmod: None,
                        changefreq: None,
                        priority: None,
                    });
                }
            }
        }
        return Ok(filter_map_result(url_set, &filter, config.map_limit));
    }

    Ok(MapResult { urls: Vec::new() })
}

/// Compiled URL filter for map results: `exclude_paths` regexes plus an optional
/// case-insensitive `map_search` substring.
///
/// Built once per [`map`] call so the same predicate can be applied incrementally
/// while sitemaps are fetched — this bounds peak memory instead of materializing
/// the entire sitemap tree before filtering.
pub(crate) struct MapFilter {
    exclude_paths: Vec<Regex>,
    search: Option<String>,
}

impl MapFilter {
    /// Compile the filter from config.
    ///
    /// Returns an error if any `exclude_paths` pattern is not a valid regex.
    pub(crate) fn from_config(config: &CrawlConfig) -> Result<Self, CrawlError> {
        let mut exclude_paths = Vec::with_capacity(config.exclude_paths.len());
        for pat in &config.exclude_paths {
            let re = Regex::new(pat)
                .map_err(|e| CrawlError::Other(format!("invalid exclude_paths regex pattern '{pat}': {e}")))?;
            exclude_paths.push(re);
        }
        let search = config.map_search.as_ref().map(|s| s.to_lowercase());
        Ok(Self { exclude_paths, search })
    }

    /// Whether a discovered URL passes the exclude-path and search filters.
    ///
    /// A URL that fails to parse is not subject to `exclude_paths` (there is no
    /// path to match against) but is still subject to `map_search`.
    pub(crate) fn matches(&self, url: &str) -> bool {
        if !self.exclude_paths.is_empty()
            && let Ok(parsed) = Url::parse(url)
        {
            let path = parsed.path();
            if self.exclude_paths.iter().any(|re| re.is_match(path)) {
                return false;
            }
        }
        if let Some(ref search) = self.search
            && !url.to_lowercase().contains(search)
        {
            return false;
        }
        true
    }
}

/// Apply the compiled filter and `map_limit` to the collected URLs.
pub(crate) fn filter_map_result(mut urls: Vec<SitemapUrl>, filter: &MapFilter, limit: Option<usize>) -> MapResult {
    urls.retain(|su| filter.matches(&su.url));
    if let Some(limit) = limit {
        urls.truncate(limit);
    }
    MapResult { urls }
}
