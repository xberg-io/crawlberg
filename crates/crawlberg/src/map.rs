//! Site mapping operation that discovers URLs via sitemaps and link extraction.

use std::collections::HashSet;

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::html::{effective_base_url, extract_links, is_html_content, mask_raw_text_markup};
use crate::http::{build_client, fetch_with_retry, http_fetch};
use crate::normalize::{normalize_url, resolve_redirect, rewrite_url_host, strip_fragment};
use crate::sitemap::{
    SitemapDocument, SitemapWalkContext, decompress_gzip, fetch_sitemap_tree, is_sitemap_index, parse_sitemap_xml,
    process_sitemap_response,
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
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let filter = MapFilter::from_config(config)?;
    let context = SitemapWalkContext {
        config,
        client: &client,
        filter: &filter,
    };

    if config.respect_robots_txt {
        let urls = sitemap_urls_from_robots(url, &parsed_url, config, &client, &context).await;
        if !urls.is_empty() {
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
    }

    let urls = sitemap_urls_from_well_known(&parsed_url, config, &client, &context).await;
    if !urls.is_empty() {
        return Ok(filter_map_result(urls, &filter, config.map_limit));
    }

    let resp = fetch_with_retry(url, config, &std::collections::HashMap::new(), &client).await?;
    let urls = urls_from_direct_response(url, &parsed_url, &resp, config, &context).await;
    Ok(filter_map_result(urls, &filter, config.map_limit))
}

/// Collect URLs from every `Sitemap:` directive advertised in the site's robots.txt.
///
/// Returns an empty vector when robots.txt is unreadable or advertises no sitemaps,
/// which the caller treats as "no hints" and falls through to `/sitemap.xml`.
async fn sitemap_urls_from_robots(
    url: &str,
    parsed_url: &Url,
    config: &CrawlConfig,
    client: &reqwest::Client,
    context: &SitemapWalkContext<'_>,
) -> Vec<SitemapUrl> {
    // ~keep `map()` deliberately stays fail-open where the crawl path now fails closed.
    // It reads robots.txt only to discover `Sitemap:` directives -- it never calls
    // `is_path_allowed`, so there is no access decision to fail closed on -- and
    // `MapResult` is `{ urls }` with no `error` or `was_skipped` field, so a fail-closed
    // result would be an empty list with no way to say why. Both `AllowAll` and
    // `DisallowAll` therefore mean "no sitemap hints", falling through to /sitemap.xml.
    // ~keep The `"*"` user-agent is preserved; see `helpers::default_robots_user_agent`.
    let ua = config.user_agent.as_deref().unwrap_or("*");
    let crate::helpers::RobotsOutcome::Rules(rules) =
        crate::helpers::fetch_robots_outcome(url, config, client, ua).await
    else {
        return Vec::new();
    };
    if rules.sitemaps.is_empty() {
        return Vec::new();
    }

    let mut all_urls = Vec::new();
    for sitemap_ref in &rules.sitemaps {
        if let Some(limit) = config.map_limit
            && all_urls.len() >= limit
        {
            break;
        }
        let sitemap_url = resolve_redirect(url, sitemap_ref);
        let resolved = rewrite_url_host(&sitemap_url, parsed_url);
        let remaining = config.map_limit.map(|limit| limit.saturating_sub(all_urls.len()));
        all_urls.extend(fetch_sitemap_tree(&resolved, context, remaining).await);
    }
    all_urls
}

/// Collect URLs from the conventional `/sitemap.xml`, if the origin serves one.
async fn sitemap_urls_from_well_known(
    parsed_url: &Url,
    config: &CrawlConfig,
    client: &reqwest::Client,
    context: &SitemapWalkContext<'_>,
) -> Vec<SitemapUrl> {
    let sitemap_url = format!("{}://{}/sitemap.xml", parsed_url.scheme(), parsed_url.authority());
    let Ok(sitemap_resp) = http_fetch(&sitemap_url, config, &std::collections::HashMap::new(), client).await else {
        return Vec::new();
    };
    if !(sitemap_resp.body.contains("<urlset") || sitemap_resp.body.contains("<sitemapindex")) {
        return Vec::new();
    }
    process_sitemap_response(
        &SitemapDocument {
            url: &sitemap_url,
            body: &sitemap_resp.body,
            body_bytes: &sitemap_resp.body_bytes,
            content_type: &sitemap_resp.content_type,
        },
        context,
        config.map_limit,
    )
    .await
}

/// Interpret a direct fetch of the mapped URL itself: a gzipped sitemap, a plain
/// or index sitemap, or an HTML page whose links stand in for a sitemap.
async fn urls_from_direct_response(
    url: &str,
    parsed_url: &Url,
    resp: &crate::http::HttpResponse,
    config: &CrawlConfig,
    context: &SitemapWalkContext<'_>,
) -> Vec<SitemapUrl> {
    let is_xml = resp.content_type.contains("xml") || resp.body.trim_start().starts_with("<?xml");

    let is_gzip = resp.content_type.contains("gzip")
        || resp.content_type.contains("x-gzip")
        || url.to_lowercase().ends_with(".gz")
        || (resp.body_bytes.len() >= 2 && resp.body_bytes[0] == GZIP_MAGIC[0] && resp.body_bytes[1] == GZIP_MAGIC[1]);
    if is_gzip && let Ok(decompressed) = decompress_gzip(&resp.body_bytes) {
        let urls = parse_sitemap_xml(&decompressed);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_xml {
        if is_sitemap_index(&resp.body) {
            return fetch_sitemap_tree(url, context, config.map_limit).await;
        }
        let urls = parse_sitemap_xml(&resp.body);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_html_content(&resp.content_type, &resp.body) {
        let parsed_html = mask_raw_text_markup(&resp.body);
        if let Ok(doc) = crate::html::parse_html(&parsed_html) {
            return links_as_sitemap_urls(&doc, parsed_url);
        }
    }

    Vec::new()
}

/// Gzip member header magic (RFC 1952 §2.3.1), used to sniff a `.gz` sitemap whose
/// content type does not declare the encoding.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Turn a page's extracted links into sitemap entries, deduplicated on the
/// normalized URL. Anchor-only links are not URLs of their own and are skipped.
fn links_as_sitemap_urls(doc: &tl::VDom<'_>, parsed_url: &Url) -> Vec<SitemapUrl> {
    let links = extract_links(doc, &effective_base_url(doc, parsed_url));
    let mut url_set: Vec<SitemapUrl> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for link in &links {
        // ~keep Internal and Document links are recorded without their fragment, external
        // ones verbatim; both dedupe on the *normalized* URL, not on the recorded form.
        let recorded = match link.link_type {
            LinkType::Internal | LinkType::Document => strip_fragment(&link.url),
            LinkType::External => link.url.clone(),
            LinkType::Anchor => continue,
        };
        if seen.insert(normalize_url(&link.url)) {
            url_set.push(SitemapUrl {
                url: recorded,
                lastmod: None,
                changefreq: None,
                priority: None,
            });
        }
    }
    url_set
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
    match_query: bool,
}

impl MapFilter {
    /// Compile the filter from config.
    ///
    /// Returns an error if any `exclude_paths` pattern is not a valid regex.
    pub(crate) fn from_config(config: &CrawlConfig) -> Result<Self, CrawlError> {
        let exclude_paths = crate::helpers::compile_regexes(&config.exclude_paths)?;
        let search = config.map_search.as_ref().map(|s| s.to_lowercase());
        Ok(Self {
            exclude_paths,
            search,
            match_query: config.path_patterns_match_query,
        })
    }

    /// Whether a discovered URL passes the exclude-path and search filters.
    ///
    /// A URL that fails to parse is not subject to `exclude_paths` (there is no
    /// path to match against) but is still subject to `map_search`.
    pub(crate) fn matches(&self, url: &str) -> bool {
        if !self.exclude_paths.is_empty()
            && let Ok(parsed) = Url::parse(url)
        {
            let mut urls_filtered = 0usize;
            if !crate::helpers::passes_path_patterns(
                &parsed,
                &self.exclude_paths,
                &[],
                false,
                self.match_query,
                &mut urls_filtered,
            ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CrawlConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A `CrawlConfig` that allows fetching the wiremock server on `127.0.0.1`
    /// without tripping SSRF private-network protections.
    fn local_test_config() -> CrawlConfig {
        CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }
    }

    fn urlset(locs: &[String]) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for loc in locs {
            body.push_str(&format!("<url><loc>{loc}</loc></url>"));
        }
        body.push_str("</urlset>");
        body
    }

    async fn mount_body(mock: &MockServer, route: &str, content_type: &str, body: String) {
        let response = ResponseTemplate::new(200)
            .set_body_string(body)
            .append_header("content-type", content_type);
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(mock)
            .await;
    }

    async fn mount_bytes(mock: &MockServer, route: &str, content_type: &str, body: Vec<u8>) {
        let response = ResponseTemplate::new(200)
            .set_body_bytes(body)
            .append_header("content-type", content_type);
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(mock)
            .await;
    }

    fn page_urls(base: &str, count: usize) -> Vec<String> {
        (0..count).map(|i| format!("{base}/page-{i}")).collect()
    }

    #[tokio::test]
    async fn map_uses_sitemap_directives_advertised_by_robots_txt() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {base}/custom-sitemap.xml\n"),
        )
        .await;
        mount_body(
            &mock,
            "/custom-sitemap.xml",
            "application/xml",
            urlset(&page_urls("https://example.com", 3)),
        )
        .await;

        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            page_urls("https://example.com", 3),
            "the robots.txt Sitemap: directive must be preferred over /sitemap.xml"
        );
    }

    #[tokio::test]
    async fn map_falls_back_to_well_known_sitemap_xml() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/sitemap.xml",
            "application/xml",
            urlset(&page_urls("https://example.com", 2)),
        )
        .await;

        let result = map(&base, &local_test_config()).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            page_urls("https://example.com", 2),
            "with no robots.txt hints, /sitemap.xml must be used"
        );
    }

    #[tokio::test]
    async fn map_extracts_links_from_an_html_page_when_no_sitemap_exists() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/",
            "text/html",
            format!(
                "<html><body>\
                 <a href=\"/internal#section\">internal</a>\
                 <a href=\"https://external.example.com/away\">external</a>\
                 <a href=\"#top\">anchor</a>\
                 </body></html>\
                 <!-- {base} -->"
            ),
        )
        .await;

        let result = map(&base, &local_test_config()).await.expect("map should succeed");
        let urls: Vec<String> = result.urls.iter().map(|u| u.url.clone()).collect();

        assert_eq!(
            urls,
            vec![
                format!("{base}/internal"),
                "https://external.example.com/away".to_owned()
            ],
            "internal links must be recorded without their fragment, external ones verbatim, \
             and fragment-only anchors skipped entirely"
        );
    }

    #[tokio::test]
    async fn map_resolves_html_links_against_the_page_base_href() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/",
            "text/html",
            "<html><head><base href=\"/other/\"></head>\
             <body><a href=\"page\">page</a></body></html>"
                .to_owned(),
        )
        .await;

        let result = map(&base, &local_test_config()).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec![format!("{base}/other/page")],
            "a relative link must resolve against the page's <base href>, not the document URL"
        );
    }

    #[tokio::test]
    async fn map_parses_a_gzip_encoded_sitemap_fetched_directly() {
        use std::io::Write as _;

        let mock = MockServer::start().await;
        let base = mock.uri();

        let plain = urlset(&page_urls("https://example.com", 2));
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(plain.as_bytes()).expect("gzip write");
        let gzipped = encoder.finish().expect("gzip finish");

        mount_bytes(&mock, "/sitemap.xml.gz", "application/octet-stream", gzipped).await;

        let result = map(&format!("{base}/sitemap.xml.gz"), &local_test_config())
            .await
            .expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            page_urls("https://example.com", 2),
            "a gzipped sitemap must be sniffed by magic bytes and inflated"
        );
    }

    #[tokio::test]
    async fn map_applies_search_filter_and_limit_to_the_result() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://example.com/keep-1".to_owned(),
            "https://example.com/drop-1".to_owned(),
            "https://example.com/keep-2".to_owned(),
            "https://example.com/keep-3".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            map_search: Some("KEEP".to_owned()),
            map_limit: Some(2),
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec![
                "https://example.com/keep-1".to_owned(),
                "https://example.com/keep-2".to_owned()
            ],
            "map_search must match case-insensitively and map_limit must cap the result"
        );
    }

    #[tokio::test]
    async fn map_applies_exclude_paths_to_discovered_urls() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://example.com/blog/one".to_owned(),
            "https://example.com/admin/secret".to_owned(),
            "https://example.com/blog/two".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            exclude_paths: vec!["^/admin".to_owned()],
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec![
                "https://example.com/blog/one".to_owned(),
                "https://example.com/blog/two".to_owned()
            ],
            "exclude_paths regexes must be matched against the URL path"
        );
    }

    #[tokio::test]
    async fn map_exclude_paths_ignores_query_by_default() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://example.com/blog?p=42".to_owned(),
            "https://example.com/blog/two".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            exclude_paths: vec![r"\?p=\d+".to_owned()],
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            locs,
            "path-only matching must not see the query string, so /blog?p=42 must not be excluded"
        );
    }

    #[tokio::test]
    async fn map_exclude_paths_matches_query_when_path_patterns_match_query_is_set() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://example.com/blog?p=42".to_owned(),
            "https://example.com/blog/two".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            exclude_paths: vec![r"\?p=\d+".to_owned()],
            path_patterns_match_query: true,
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec!["https://example.com/blog/two".to_owned()],
            "with path_patterns_match_query on, /blog?p=42 must be excluded"
        );
    }

    #[tokio::test]
    async fn map_limit_truncates_links_extracted_from_html() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/",
            "text/html",
            "<html><body><a href=\"/a\">a</a><a href=\"/b\">b</a><a href=\"/c\">c</a></body></html>".to_owned(),
        )
        .await;

        let config = CrawlConfig {
            map_limit: Some(2),
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec![format!("{base}/a"), format!("{base}/b")],
            "map_limit must truncate HTML-extracted links, which are not bounded during extraction"
        );
    }

    #[tokio::test]
    async fn map_rejects_an_invalid_exclude_paths_regex() {
        let config = CrawlConfig {
            exclude_paths: vec!["[unclosed".to_owned()],
            ..local_test_config()
        };
        let error = map("https://example.com", &config)
            .await
            .expect_err("an invalid regex must be reported, not silently ignored");

        assert!(
            error.to_string().contains("invalid regex pattern \"[unclosed\""),
            "the error must name the offending pattern, got: {error}"
        );
    }

    #[tokio::test]
    async fn map_rejects_an_unparseable_url() {
        let error = map("not a url", &local_test_config())
            .await
            .expect_err("an unparseable URL must be rejected");

        assert!(
            error.to_string().contains("invalid URL"),
            "expected an invalid-URL error, got: {error}"
        );
    }
}
