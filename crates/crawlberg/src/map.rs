//! Site mapping operation that discovers URLs via sitemaps and link extraction.

use std::collections::HashSet;

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::html::{MaskedHtml, PageScan, effective_base_url, extract_links, is_html_content, mask_raw_text_markup};
use crate::http::{Fetched, RefreshRedirects, build_client, fetch_with_retry, http_fetch_sitemap};
use crate::normalize::{normalize_url, resolve_redirect, strip_fragment};
use crate::sitemap::{
    SitemapDocument, SitemapWalkContext, collect_urlset_entries, decompress_gzip, fetch_sitemap_tree, is_sitemap_index,
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
pub async fn map(seed: &crate::engine::SeedUrl, config: &CrawlConfig) -> Result<MapResult, CrawlError> {
    let url = seed.as_str();
    let parsed_url = seed.url().clone();
    let client = build_client(config)?;
    let filter = MapFilter::from_config(config)?;
    let context = SitemapWalkContext::new(config, &client, &filter);

    if config.respect_robots_txt {
        let urls = sitemap_urls_from_robots(url, config, &client, &context).await;
        if !urls.is_empty() {
            return Ok(filter_map_result(urls, &filter, config.map_limit));
        }
    }

    let urls = sitemap_urls_from_well_known(&parsed_url, config, &client, &context).await;
    if !urls.is_empty() {
        return Ok(filter_map_result(urls, &filter, config.map_limit));
    }

    // ~keep The direct fetch follows a refresh as the crawl does, so a page that forwards through
    // ~keep one is mapped from the page it lands on (#502). The mapped URL is often a sitemap
    // ~keep itself, so a body that reads as one is read whatever its URLs say; any other body gets
    // ~keep the page decision.
    let page = fetch_with_retry(
        url,
        config,
        &std::collections::HashMap::new(),
        &client,
        RefreshRedirects::Follow,
        Fetched::Sitemap,
    )
    .await?;
    let urls = urls_from_direct_response(url, &parsed_url, &page.response, page.page_scan, config, &context).await;
    Ok(filter_map_result(urls, &filter, config.map_limit))
}

/// Collect URLs from every `Sitemap:` directive advertised in the site's robots.txt.
///
/// Returns an empty vector when robots.txt is unreadable or advertises no sitemaps,
/// which the caller treats as "no hints" and falls through to `/sitemap.xml`.
async fn sitemap_urls_from_robots(
    url: &str,
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
    let (crate::helpers::RobotsOutcome::Rules(rules), Some(robots_url)) =
        crate::helpers::fetch_robots_document(url, config, client, ua).await
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
        let Some(resolved) = resolve_sitemap_directive(&robots_url, sitemap_ref) else {
            continue;
        };
        let remaining = config.map_limit.map(|limit| limit.saturating_sub(all_urls.len()));
        all_urls.extend(fetch_sitemap_tree(&resolved, context, remaining).await);
    }
    all_urls
}

/// The URL to fetch for one robots.txt `Sitemap:` directive, resolved against `robots_url`, the
/// address that served robots.txt after redirects. `None` when `sitemap_ref` cannot be resolved
/// against it at all, which the caller skips rather than fetching as raw text.
///
/// ~keep A directive on another host is fetched from that host: the sitemaps.org protocol lets
/// ~keep robots.txt name a sitemap on another host. The SSRF policy gates the fetch, and seed
/// ~keep credentials go only to the seed host.
fn resolve_sitemap_directive(robots_url: &str, sitemap_ref: &str) -> Option<String> {
    let Some(resolved) = resolve_redirect(robots_url, sitemap_ref) else {
        tracing::debug!(
            url = %crate::net::redact_url_credentials(robots_url),
            target_len = sitemap_ref.len(),
            "robots.txt Sitemap: directive failed to parse; skipping it"
        );
        return None;
    };
    Some(resolved.into())
}

/// Collect URLs from the conventional `/sitemap.xml`, if the origin serves one.
async fn sitemap_urls_from_well_known(
    parsed_url: &Url,
    config: &CrawlConfig,
    client: &reqwest::Client,
    context: &SitemapWalkContext<'_>,
) -> Vec<SitemapUrl> {
    let sitemap_url = format!("{}://{}/sitemap.xml", parsed_url.scheme(), parsed_url.authority());
    let Ok(sitemap_resp) = http_fetch_sitemap(&sitemap_url, config, client).await else {
        return Vec::new();
    };
    if !(sitemap_resp.body.contains("<urlset") || sitemap_resp.body.contains("<sitemapindex")) {
        return Vec::new();
    }
    process_sitemap_response(
        &SitemapDocument {
            url: &sitemap_url,
            final_url: &sitemap_resp.final_url,
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
/// or index sitemap, or an HTML page whose links stand in for a sitemap. `page_scan` is the
/// refresh check's read of `resp`'s body, when it made one.
async fn urls_from_direct_response(
    url: &str,
    parsed_url: &Url,
    resp: &crate::http::HttpResponse,
    page_scan: Option<PageScan>,
    config: &CrawlConfig,
    context: &SitemapWalkContext<'_>,
) -> Vec<SitemapUrl> {
    let is_xml = resp.content_type.contains("xml") || resp.body.trim_start().starts_with("<?xml");

    let is_gzip = resp.content_type.contains("gzip")
        || resp.content_type.contains("x-gzip")
        || url.to_lowercase().ends_with(".gz")
        || (resp.body_bytes.len() >= 2 && resp.body_bytes[0] == GZIP_MAGIC[0] && resp.body_bytes[1] == GZIP_MAGIC[1]);
    if is_gzip && let Ok(decompressed) = decompress_gzip(&resp.body_bytes) {
        let urls = collect_urlset_entries(&resp.final_url, &decompressed, context, config.map_limit);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_xml {
        if is_sitemap_index(&resp.body) {
            // ~keep Walked from the URL that served it: a refresh that led here is not followed
            // ~keep again by the sitemap fetch, so the requested URL would give back the page.
            return fetch_sitemap_tree(&resp.final_url, context, config.map_limit).await;
        }
        let urls = collect_urlset_entries(&resp.final_url, &resp.body, context, config.map_limit);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_html_content(&resp.content_type, &resp.body) {
        // ~keep The page's links resolve against the URL that served it, not the one
        // ~keep requested, matching the crawl engine (`crawl_loop.rs`'s `url_for_extract`)
        // ~keep and the gzip, urlset and sitemap index branches above.
        let base_url = Url::parse(&resp.final_url).unwrap_or_else(|_| parsed_url.clone());
        let page = match page_scan {
            Some(page_scan) => page_scan.attach(&resp.body),
            None => mask_raw_text_markup(&resp.body),
        };
        return links_as_sitemap_urls(&page, &base_url);
    }

    Vec::new()
}

/// Gzip member header magic (RFC 1952 §2.3.1), used to sniff a `.gz` sitemap whose
/// content type does not declare the encoding.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Turn a page's extracted links into sitemap entries, deduplicated on the
/// normalized URL. Anchor-only links are not URLs of their own and are skipped.
fn links_as_sitemap_urls(page: &MaskedHtml<'_>, parsed_url: &Url) -> Vec<SitemapUrl> {
    let links = extract_links(page, &effective_base_url(page.base_href.as_deref(), parsed_url));
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
/// The term and each address are compared in their [`crate::normalize::search_key`] form, which
/// joins canonically equivalent spellings and folds case without a locale. The Turkish dotted
/// and dotless `i` therefore never match their Turkish case partners.
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
        let search = config.map_search.as_deref().map(crate::normalize::search_key);
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
    ///
    /// `map_search` matches either the address as `map()` returns it (percent-encoded,
    /// punycode host) or its decoded, human-readable form, so a caller's non-ASCII term
    /// still finds an address whose path is percent-encoded or whose host is punycode.
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
            && !crate::normalize::search_key(&crate::normalize::decoded_for_search(url)).contains(search)
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
    use crate::tracing_capture::{assert_logged_without_secret, capture_events};
    use crate::types::CrawlConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Map an already-clean test URL, as the engine does after admission.
    async fn map(url: &str, config: &CrawlConfig) -> Result<MapResult, CrawlError> {
        super::map(&crate::engine::SeedUrl::for_test(url), config).await
    }

    /// A `CrawlConfig` that allows fetching the wiremock server on `127.0.0.1`
    /// without tripping SSRF private-network protections.
    fn local_test_config() -> CrawlConfig {
        CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn an_unparseable_sitemap_directive_is_refused_not_followed_raw() {
        let resolved = resolve_sitemap_directive("https://example.com/", "https://ex ample.com/bad.xml");

        assert!(
            resolved.is_none(),
            "a robots.txt Sitemap: directive that fails to parse must not be followed as raw \
             text, got {resolved:?}"
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn an_unparseable_sitemap_directive_with_credentials_is_never_logged() {
        let (resolved, fields) = capture_events(|| {
            resolve_sitemap_directive(
                "https://example.com/robots.txt",
                "https://user:hunter2@ex ample.com/bad.xml",
            )
        });

        assert!(
            resolved.is_none(),
            "an unparseable Sitemap: directive must not be followed"
        );
        assert_logged_without_secret(&fields, "hunter2", "example.com/robots.txt");
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
    async fn map_ignores_links_that_only_appear_inside_raw_text_on_an_html_page() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_body(
            &mock,
            "/",
            "text/html",
            "<html><head><title>Sitemapless <a href=\"/from-title\">t</a></title></head><body>\
             <script>document.write('<a href=\"/from-script\">s</a>');</script>\
             <textarea><a href=\"/from-textarea\">x</a></textarea>\
             <a href=\"/real\">real</a>\
             </body></html>"
                .to_owned(),
        )
        .await;

        let result = map(&base, &local_test_config()).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec![format!("{base}/real")],
            "a mapped HTML page must contribute only the links a browser sees, not addresses \
             written inside title, script or textarea text"
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
    async fn map_search_matches_a_non_ascii_term_against_a_percent_encoded_path() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://example.com/café".to_owned(),
            "https://example.com/other".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            map_search: Some("café".to_owned()),
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec!["https://example.com/caf%C3%A9".to_owned()],
            "map_search=\"café\" must match the entry map() stores percent-encoded, got {:?}",
            result.urls
        );
    }

    #[tokio::test]
    async fn map_search_matches_a_non_ascii_term_against_a_punycode_host() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        let locs = vec![
            "https://bücher.example/x".to_owned(),
            "https://example.com/other".to_owned(),
        ];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;

        let config = CrawlConfig {
            map_search: Some("bücher".to_owned()),
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.iter().map(|u| u.url.clone()).collect::<Vec<_>>(),
            vec!["https://xn--bcher-kva.example/x".to_owned()],
            "map_search=\"bücher\" must match the entry map() stores as punycode, got {:?}",
            result.urls
        );
    }

    /// The addresses `map()` returns when a sitemap lists `loc` and an unrelated page, and
    /// `map_search` is `term`.
    async fn map_search_results(term: &str, loc: &str) -> Vec<String> {
        let mock = MockServer::start().await;
        let locs = vec![loc.to_owned(), "https://example.com/other".to_owned()];
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;
        let config = CrawlConfig {
            map_search: Some(term.to_owned()),
            ..local_test_config()
        };
        map_urls(&mock.uri(), &config).await
    }

    #[tokio::test]
    async fn map_search_matches_a_decomposed_term_against_a_punycode_host() {
        let urls = map_search_results("bu\u{308}cher", "https://bücher.example/x").await;

        assert_eq!(urls, vec!["https://xn--bcher-kva.example/x".to_owned()]);
    }

    #[tokio::test]
    async fn map_search_matches_a_composed_term_against_a_decomposed_path() {
        let urls = map_search_results("café", "https://example.com/cafe\u{301}").await;

        assert_eq!(urls, vec!["https://example.com/cafe%CC%81".to_owned()]);
    }

    #[tokio::test]
    async fn map_search_matches_a_decomposed_term_against_a_composed_path() {
        let urls = map_search_results("cafe\u{301}", "https://example.com/caf\u{e9}").await;

        assert_eq!(urls, vec!["https://example.com/caf%C3%A9".to_owned()]);
    }

    #[tokio::test]
    async fn map_search_folds_sharp_s_to_ss() {
        let urls = map_search_results("STRASSE", "https://example.com/Straße").await;

        assert_eq!(urls, vec!["https://example.com/Stra%C3%9Fe".to_owned()]);
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
        let engine = crate::CrawlEngine::builder()
            .config(local_test_config())
            .build()
            .expect("engine must build");
        let error = engine
            .map("not a url")
            .await
            .expect_err("an unparseable URL must be rejected");

        assert!(
            error.to_string().contains("invalid URL"),
            "expected an invalid-URL error, got: {error}"
        );
    }

    /// Serve `locs` as the well-known `/sitemap.xml` and return the addresses `map()` reports.
    async fn map_well_known_urlset(locs: &[&str], config: &CrawlConfig) -> (String, Vec<String>) {
        let mock = MockServer::start().await;
        let base = mock.uri();
        let locs: Vec<String> = locs.iter().map(|loc| (*loc).to_owned()).collect();
        mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)).await;
        let result = map(&base, config).await.expect("map should succeed");
        (base, result.urls.into_iter().map(|u| u.url).collect())
    }

    #[tokio::test]
    async fn map_lower_cases_the_scheme_and_host_of_a_urlset_loc() {
        let (_, urls) = map_well_known_urlset(&["HTTPS://EXAMPLE.COM/Page"], &local_test_config()).await;

        assert_eq!(
            urls,
            vec!["https://example.com/Page".to_owned()],
            "a urlset <loc> must be returned in the parser's normalized form, with the path's case kept"
        );
    }

    #[tokio::test]
    async fn map_drops_the_default_port_of_a_urlset_loc() {
        let (_, urls) = map_well_known_urlset(&["https://example.com:443/a"], &local_test_config()).await;

        assert_eq!(urls, vec!["https://example.com/a".to_owned()]);
    }

    #[tokio::test]
    async fn map_strips_stray_whitespace_inside_a_urlset_loc() {
        let (_, urls) = map_well_known_urlset(&["https://example.com/a\n\tb"], &local_test_config()).await;

        assert_eq!(
            urls,
            vec!["https://example.com/ab".to_owned()],
            "an embedded newline or tab must be removed by the URL parser, not returned"
        );
    }

    #[tokio::test]
    async fn map_resolves_a_relative_urlset_loc_against_the_sitemap_url() {
        let (base, urls) = map_well_known_urlset(&["/relative/page"], &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/relative/page")]);
    }

    #[tokio::test]
    async fn map_drops_a_urlset_loc_that_names_the_sitemap_itself() {
        let locs = ["?q=1", "#frag", "sitemap.xml", "/sitemap.xml#top", "page"];

        let (base, urls) = map_well_known_urlset(&locs, &local_test_config()).await;

        assert_eq!(
            urls,
            vec![format!("{base}/page")],
            "a <loc> that is only a query is dropped for being query-only; one that is only a \
             fragment, or that names the sitemap's own address, is dropped for naming the sitemap itself"
        );
    }

    #[tokio::test]
    async fn map_drops_a_query_only_loc_even_when_it_resolves_to_a_different_page_than_the_sitemap() {
        // ~keep A urlset served at `/` resolves `?page=2` to `/?page=2`, a page distinct from the
        // ~keep sitemap's own address; the query-only rule drops it anyway, not a same-address match.
        let seed = MockServer::start().await;
        let base = seed.uri();
        mount_body(
            &seed,
            "/",
            "application/xml",
            urlset(&["?page=2".to_owned(), "/about".to_owned()]),
        )
        .await;

        let urls = map_urls(&base, &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/about")]);
    }

    #[tokio::test]
    async fn map_applies_exclude_paths_to_a_relative_urlset_loc() {
        let config = CrawlConfig {
            exclude_paths: vec!["^/admin".to_owned()],
            ..local_test_config()
        };
        let (base, urls) = map_well_known_urlset(&["/admin/secret", "/blog/one"], &config).await;

        assert_eq!(
            urls,
            vec![format!("{base}/blog/one")],
            "a relative <loc> resolves to an address, so exclude_paths must apply to it"
        );
    }

    #[tokio::test]
    async fn map_returns_two_spellings_of_one_urlset_page_once() {
        let (_, urls) = map_well_known_urlset(
            &[
                "https://example.com/a",
                "HTTPS://example.com:443/a",
                "https://example.com/b",
            ],
            &local_test_config(),
        )
        .await;

        assert_eq!(
            urls,
            vec!["https://example.com/a".to_owned(), "https://example.com/b".to_owned()]
        );
    }

    #[tokio::test]
    async fn map_limit_is_not_consumed_by_a_duplicate_urlset_loc() {
        let config = CrawlConfig {
            map_limit: Some(2),
            ..local_test_config()
        };
        let (_, urls) = map_well_known_urlset(
            &[
                "https://example.com/a",
                "https://EXAMPLE.com/a",
                "https://example.com/b",
                "https://example.com/c",
            ],
            &config,
        )
        .await;

        assert_eq!(
            urls,
            vec!["https://example.com/a".to_owned(), "https://example.com/b".to_owned()],
            "a duplicate must be dropped before it counts toward map_limit"
        );
    }

    #[tokio::test]
    async fn map_returns_a_page_listed_by_two_child_sitemaps_once() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(
            &mock,
            "/sitemap.xml",
            "application/xml",
            format!(
                r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><sitemap><loc>{base}/one.xml</loc></sitemap><sitemap><loc>{base}/two.xml</loc></sitemap></sitemapindex>"#
            ),
        )
        .await;
        let one = vec!["https://example.com/a".to_owned(), "https://example.com/b".to_owned()];
        let two = vec![
            "https://example.com:443/a".to_owned(),
            "https://example.com/c".to_owned(),
        ];
        mount_body(&mock, "/one.xml", "application/xml", urlset(&one)).await;
        mount_body(&mock, "/two.xml", "application/xml", urlset(&two)).await;

        let result = map(&base, &local_test_config()).await.expect("map should succeed");

        assert_eq!(
            result.urls.into_iter().map(|u| u.url).collect::<Vec<_>>(),
            vec![
                "https://example.com/a".to_owned(),
                "https://example.com/b".to_owned(),
                "https://example.com/c".to_owned()
            ],
            "a page listed by two child sitemaps of one index must be returned once"
        );
    }

    #[tokio::test]
    async fn map_returns_a_page_listed_by_two_robots_sitemap_directives_once() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(
            &mock,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {base}/one.xml\nSitemap: {base}/two.xml\n"),
        )
        .await;
        let one = vec!["https://example.com/a".to_owned()];
        let two = vec!["HTTPS://example.com/a".to_owned(), "https://example.com/b".to_owned()];
        mount_body(&mock, "/one.xml", "application/xml", urlset(&one)).await;
        mount_body(&mock, "/two.xml", "application/xml", urlset(&two)).await;

        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };
        let result = map(&base, &config).await.expect("map should succeed");

        assert_eq!(
            result.urls.into_iter().map(|u| u.url).collect::<Vec<_>>(),
            vec!["https://example.com/a".to_owned(), "https://example.com/b".to_owned()],
            "a page listed by two robots.txt Sitemap: directives must be returned once"
        );
    }

    #[tokio::test]
    async fn map_normalizes_and_dedupes_a_urlset_fetched_directly() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        let locs = vec![
            "https://example.com/a".to_owned(),
            "HTTPS://example.com:443/a".to_owned(),
            "/relative".to_owned(),
        ];
        mount_body(&mock, "/feed.xml", "application/xml", urlset(&locs)).await;

        let result = map(&format!("{base}/feed.xml"), &local_test_config())
            .await
            .expect("map should succeed");

        assert_eq!(
            result.urls.into_iter().map(|u| u.url).collect::<Vec<_>>(),
            vec!["https://example.com/a".to_owned(), format!("{base}/relative")]
        );
    }

    #[tokio::test]
    async fn map_normalizes_and_dedupes_a_gzip_urlset_fetched_directly() {
        use std::io::Write as _;

        let mock = MockServer::start().await;
        let base = mock.uri();
        let locs = vec![
            "https://example.com/a".to_owned(),
            "HTTPS://example.com:443/a".to_owned(),
            "/relative".to_owned(),
        ];
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(urlset(&locs).as_bytes()).expect("gzip write");
        let gzipped = encoder.finish().expect("gzip finish");
        mount_bytes(&mock, "/sitemap.xml.gz", "application/octet-stream", gzipped).await;

        let result = map(&format!("{base}/sitemap.xml.gz"), &local_test_config())
            .await
            .expect("map should succeed");

        assert_eq!(
            result.urls.into_iter().map(|u| u.url).collect::<Vec<_>>(),
            vec!["https://example.com/a".to_owned(), format!("{base}/relative")]
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn map_drops_an_unparseable_urlset_loc_and_never_logs_its_credentials() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime must build");
        let mock = runtime.block_on(MockServer::start());
        let base = mock.uri();
        let bad_loc = "https://user:hunter2@ex ample.com/bad";
        let locs = vec![bad_loc.to_owned(), "https://example.com/kept".to_owned()];
        runtime.block_on(mount_body(&mock, "/sitemap.xml", "application/xml", urlset(&locs)));

        let (result, fields) = capture_events(|| runtime.block_on(map(&base, &local_test_config())));

        assert_eq!(
            result
                .expect("map should succeed")
                .urls
                .into_iter()
                .map(|u| u.url)
                .collect::<Vec<_>>(),
            vec!["https://example.com/kept".to_owned()],
            "a <loc> that does not parse must be dropped, not returned as text"
        );
        assert_logged_without_secret(&fields, "hunter2", "urlset entry");
        assert!(
            fields
                .iter()
                .any(|(name, value)| name == "target_len" && *value == bad_loc.len().to_string()),
            "the dropped <loc> must be logged by length, got {fields:?}"
        );
    }

    #[test]
    #[serial_test::serial(dropped_target_log)]
    fn a_credentialed_sitemap_url_is_redacted_when_logging_an_unparseable_urlset_loc() {
        // ~keep The engine admits no seed with userinfo, so `map()` never hands the walk a
        // ~keep credentialed document URL; this calls the urlset reader directly to pin
        // ~keep `log_unparseable_loc`'s `source_url_parses` branch, which redacts the address.
        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).expect("filter");
        let context = SitemapWalkContext::new(&config, &client, &filter);
        let bad_loc = "https://ex ample.com/bad";
        let locs = vec![bad_loc.to_owned(), "https://example.com/kept".to_owned()];
        let body = urlset(&locs);

        let (urls, fields) = capture_events(|| {
            collect_urlset_entries("https://user:hunter2@example.com/sitemap.xml", &body, &context, None)
        });

        assert_eq!(
            urls.into_iter().map(|u| u.url).collect::<Vec<_>>(),
            vec!["https://example.com/kept".to_owned()],
            "a <loc> that does not parse must still be dropped when the sitemap URL carries credentials"
        );
        assert_logged_without_secret(&fields, "hunter2", "example.com/sitemap.xml");
    }

    async fn mount_redirect(mock: &MockServer, route: &str, location: &str) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(301).append_header("location", location))
            .mount(mock)
            .await;
    }

    fn sitemap_index(child_locs: &[&str]) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for loc in child_locs {
            body.push_str(&format!("<sitemap><loc>{loc}</loc></sitemap>"));
        }
        body.push_str("</sitemapindex>");
        body
    }

    /// The addresses `map()` returns for `url`.
    async fn map_urls(url: &str, config: &CrawlConfig) -> Vec<String> {
        let result = map(url, config).await.expect("map should succeed");
        result.urls.into_iter().map(|u| u.url).collect()
    }

    /// An HTML page whose only content is a `<meta http-equiv="refresh">` with `content`.
    fn meta_refresh_page(content: &str) -> String {
        format!(r#"<html><head><meta http-equiv="refresh" content="{content}"></head><body></body></html>"#)
    }

    /// An HTML page with one link to `href`.
    fn page_linking_to(href: &str) -> String {
        format!(r#"<html><body><a href="{href}">next</a></body></html>"#)
    }

    async fn request_count(mock: &MockServer, route: &str) -> usize {
        mock.received_requests()
            .await
            .expect("wiremock records requests")
            .iter()
            .filter(|request| request.url.path() == route)
            .count()
    }

    #[tokio::test]
    async fn map_follows_a_meta_refresh_and_resolves_links_against_the_page_it_lands_on() {
        // ~keep The crawl has no delay cap: it follows a refresh whatever its delay, so map does too.
        for delay in ["0", "300"] {
            let mock = MockServer::start().await;
            let base = mock.uri();
            let refresh = meta_refresh_page(&format!("{delay}; url=/dir/page.html"));
            mount_body(&mock, "/start", "text/html", refresh).await;
            mount_body(&mock, "/dir/page.html", "text/html", page_linking_to("x.html")).await;

            let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

            assert_eq!(
                urls,
                vec![format!("{base}/dir/x.html")],
                "a meta refresh with delay {delay} must be followed, and the link resolved against the page it lands on"
            );
        }
    }

    #[tokio::test]
    async fn a_map_through_a_meta_refresh_reads_each_page_once_with_the_html_parser() {
        use crate::html::reads;
        let mock = MockServer::start().await;
        let base = mock.uri();
        let start = "map-refresh-start-8c1f";
        let landing = "map-refresh-landing-8c1f";
        let refresh = format!(
            r#"{}<p>{start}</p><meta http-equiv="refresh" content="0; url=/dir/page.html">"#,
            reads::MARKER
        );
        let page = format!(r#"{}<p>{landing}</p><a href="x.html">next</a>"#, reads::MARKER);
        mount_body(&mock, "/start", "text/html", refresh).await;
        mount_body(&mock, "/dir/page.html", "text/html", page).await;

        let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/dir/x.html")]);
        assert_eq!(reads::count(start), 1, "the page that refreshes is read once");
        assert_eq!(reads::count(landing), 1, "the page the refresh lands on is read once");
    }

    #[tokio::test]
    async fn map_follows_a_refresh_header_the_way_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body></body></html>")
                    .append_header("content-type", "text/html")
                    .append_header("refresh", "0; url=/dir/page.html"),
            )
            .mount(&mock)
            .await;
        mount_body(&mock, "/dir/page.html", "text/html", page_linking_to("x.html")).await;

        let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/dir/x.html")]);
    }

    #[tokio::test]
    async fn map_stops_a_meta_refresh_loop_at_the_first_page_it_would_revisit() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        // ~keep /a -> /b -> /c -> /b: the loop returns to a page reached by a refresh, not to
        // ~keep the one requested first, so every hop must be remembered, not only the first.
        mount_body(&mock, "/a", "text/html", meta_refresh_page("0; url=/b")).await;
        mount_body(&mock, "/b", "text/html", meta_refresh_page("0; url=/c")).await;
        let c = r#"<html><head><meta http-equiv="refresh" content="0; url=/b"></head>
            <body><a href="/from-c">c</a></body></html>"#;
        mount_body(&mock, "/c", "text/html", c.to_owned()).await;

        let urls = map_urls(&format!("{base}/a"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/from-c")]);
        assert_eq!(request_count(&mock, "/b").await, 1, "the loop must not return to /b");
        assert_eq!(request_count(&mock, "/c").await, 1);
    }

    #[tokio::test]
    async fn map_stops_a_refresh_chain_at_the_redirect_limit_counting_http_redirects_too() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        // ~keep /r0 -meta-> /r1 -301-> /r2 -meta-> /r3 -meta-> /r4: with a limit of 3 the chain
        // ~keep takes the 301 as one of its hops, as the crawl does, and stops on /r3.
        mount_body(&mock, "/r0", "text/html", meta_refresh_page("0; url=/r1")).await;
        mount_redirect(&mock, "/r1", "/r2").await;
        for (page, next) in [("/r2", "/r3"), ("/r3", "/r4"), ("/r4", "/r5")] {
            let body = format!(
                r#"<html><head><meta http-equiv="refresh" content="0; url={next}"></head>
                <body><a href="/link{page}">l</a></body></html>"#
            );
            mount_body(&mock, page, "text/html", body).await;
        }
        let config = CrawlConfig {
            max_redirects: 3,
            ..local_test_config()
        };

        let urls = map_urls(&format!("{base}/r0"), &config).await;

        assert_eq!(urls, vec![format!("{base}/link/r3")]);
        assert_eq!(request_count(&mock, "/r3").await, 1);
        assert_eq!(
            request_count(&mock, "/r4").await,
            0,
            "a hop past the limit must not be requested"
        );
    }

    #[tokio::test]
    async fn map_refuses_a_meta_refresh_to_an_address_the_ssrf_policy_denies() {
        let seed = MockServer::start().await;
        let denied = MockServer::start().await;
        // ~keep Only the host name `localhost` is allowlisted, so the literal `127.0.0.1` target
        // ~keep is refused before any connection.
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        let refresh = meta_refresh_page(&format!("0; url={}/page.html", denied.uri()));
        mount_body(&seed, "/start", "text/html", refresh).await;
        mount_body(&denied, "/page.html", "text/html", page_linking_to("x.html")).await;
        let config = CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder()
                .ssrf_allowlist_host(crate::HostMatcher::exact("localhost"))
                .build()
        };

        let result = map(&format!("{seed_base}/start"), &config).await;

        assert!(
            matches!(result, Err(CrawlError::SsrfPolicyViolation { .. })),
            "a refresh to a denied address must be refused, got {result:?}"
        );
        assert_eq!(request_count(&seed, "/start").await, 1, "the seed itself is fetched");
        assert_eq!(
            request_count(&denied, "/page.html").await,
            0,
            "the denied target is never requested"
        );
    }

    #[tokio::test]
    async fn map_does_not_follow_a_meta_refresh_the_crawl_would_not_follow() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        // ~keep GUARD: a later 0-second reload with no target replaces the 5-second refresh, as in
        // ~keep a browser, so the crawl stays on the page; map must stay too.
        let start = r#"<html><head>
            <meta http-equiv="refresh" content="5; url=/dir/page.html">
            <meta http-equiv="refresh" content="0">
            </head><body><a href="/stay">stay</a></body></html>"#;
        mount_body(&mock, "/start", "text/html", start.to_owned()).await;
        mount_body(&mock, "/dir/page.html", "text/html", page_linking_to("x.html")).await;

        let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/stay")]);
        assert_eq!(request_count(&mock, "/dir/page.html").await, 0);
    }

    #[tokio::test]
    async fn map_sends_the_seed_credential_only_to_the_seed_host_along_a_meta_refresh() {
        let seed = MockServer::start().await;
        let other = MockServer::start().await;
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        let refresh = meta_refresh_page(&format!("0; url={}/page.html", other.uri()));
        mount_body(&seed, "/start", "text/html", refresh).await;
        mount_body(&other, "/page.html", "text/html", page_linking_to("x.html")).await;
        let seed_url = Url::parse(&seed_base).expect("mock URL must parse");
        let config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(
                &seed_url,
                Some(("user".to_owned(), "hunter2".to_owned())),
            ),
            ..local_test_config()
        };

        let urls = map_urls(&format!("{seed_base}/start"), &config).await;

        assert_eq!(urls, vec![format!("{}/x.html", other.uri())]);
        let authorized = |requests: Vec<wiremock::Request>, route: &str| {
            requests
                .iter()
                .filter(|r| r.url.path() == route && r.headers.contains_key("authorization"))
                .count()
        };
        let seed_requests = seed.received_requests().await.expect("wiremock records requests");
        let other_requests = other.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            authorized(seed_requests, "/start"),
            1,
            "the seed page gets the credential"
        );
        assert_eq!(
            authorized(other_requests, "/page.html"),
            0,
            "the refresh target on another host must not"
        );
        assert_eq!(
            request_count(&other, "/page.html").await,
            1,
            "the refresh target is fetched"
        );
    }

    #[tokio::test]
    async fn map_walks_a_sitemap_index_it_reaches_through_a_meta_refresh() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(&mock, "/start", "text/html", meta_refresh_page("0; url=/dir/index.xml")).await;
        mount_body(
            &mock,
            "/dir/index.xml",
            "application/xml",
            sitemap_index(&["child.xml"]),
        )
        .await;
        mount_body(
            &mock,
            "/dir/child.xml",
            "application/xml",
            urlset(&["https://example.com/from-child".to_owned()]),
        )
        .await;

        let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

        assert_eq!(urls, vec!["https://example.com/from-child".to_owned()]);
    }

    /// Where the crawl's own redirect chain stops for `url`: the final URL without `base`, and
    /// its status. The crawl never fails on these chains, so neither may map.
    async fn crawl_stop(base: &str, url: &str, config: &CrawlConfig) -> (String, u16) {
        let engine = crate::CrawlEngine::builder()
            .config(config.clone())
            .build()
            .expect("engine builds");
        let page = engine.scrape(url).await.expect("the crawl does not fail on this chain");
        (page.final_url.replace(base, ""), page.status_code)
    }

    #[tokio::test]
    async fn map_stops_on_a_refresh_target_that_is_not_found_as_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        let start = r#"<html><head><meta http-equiv="refresh" content="0; url=/missing"></head>
            <body><a href="/fallback">f</a></body></html>"#;
        mount_body(&mock, "/start", "text/html", start.to_owned()).await;
        let config = local_test_config();

        let result = map(&format!("{base}/start"), &config).await;

        assert!(
            matches!(&result, Ok(mapped) if mapped.urls.is_empty()),
            "map must stop on the missing page without failing, got {result:?}"
        );
        assert_eq!(
            crawl_stop(&base, &format!("{base}/start"), &config).await,
            ("/missing".to_owned(), 404)
        );
    }

    #[tokio::test]
    async fn map_stops_on_a_redirect_past_the_limit_after_a_refresh_as_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        // ~keep /r0 -meta-> /r1 -meta-> /r2 -301-> /r3 with a limit of 2: both refresh hops are
        // ~keep taken, so the 301 is the page the chain stops on.
        mount_body(&mock, "/r0", "text/html", meta_refresh_page("0; url=/r1")).await;
        let r1 = r#"<html><head><meta http-equiv="refresh" content="0; url=/r2"></head>
            <body><a href="/from-r1">l</a></body></html>"#;
        mount_body(&mock, "/r1", "text/html", r1.to_owned()).await;
        mount_redirect(&mock, "/r2", "/r3").await;
        mount_body(&mock, "/r3", "text/html", page_linking_to("/from-r3")).await;
        let config = CrawlConfig {
            max_redirects: 2,
            ..local_test_config()
        };

        let result = map(&format!("{base}/r0"), &config).await;

        assert!(
            matches!(&result, Ok(mapped) if mapped.urls.is_empty()),
            "map must stop on the 301 without failing, got {result:?}"
        );
        assert_eq!(
            request_count(&mock, "/r3").await,
            0,
            "a hop past the limit is not requested"
        );
        assert_eq!(
            crawl_stop(&base, &format!("{base}/r0"), &config).await,
            ("/r2".to_owned(), 301)
        );
    }

    #[tokio::test]
    async fn map_does_not_request_a_url_again_when_a_redirect_leads_back_to_it() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        // ~keep /a -meta-> /b -301-> /a: the 301 leads back to a URL already requested, so the
        // ~keep chain stops on /b as the crawl does.
        let a = r#"<html><head><meta http-equiv="refresh" content="0; url=/b"></head>
            <body><a href="/from-a">a</a></body></html>"#;
        mount_body(&mock, "/a", "text/html", a.to_owned()).await;
        mount_redirect(&mock, "/b", "/a").await;
        let config = local_test_config();

        let urls = map_urls(&format!("{base}/a"), &config).await;

        assert_eq!(request_count(&mock, "/a").await, 1, "/a must be requested once");
        assert_eq!(urls, Vec::<String>::new(), "map stops on /b, which has no links");
        assert_eq!(
            crawl_stop(&base, &format!("{base}/a"), &config).await,
            ("/b".to_owned(), 301)
        );
    }

    #[tokio::test]
    async fn map_does_not_request_the_start_url_again_when_a_refresh_leads_back_to_it() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(&mock, "/a", "text/html", meta_refresh_page("0; url=/b")).await;
        let b = r#"<html><head><meta http-equiv="refresh" content="0; url=/a"></head>
            <body><a href="/from-b">b</a></body></html>"#;
        mount_body(&mock, "/b", "text/html", b.to_owned()).await;

        let urls = map_urls(&format!("{base}/a"), &local_test_config()).await;

        assert_eq!(
            request_count(&mock, "/a").await,
            1,
            "the start URL must be requested once"
        );
        assert_eq!(urls, vec![format!("{base}/from-b")]);
    }

    #[tokio::test]
    async fn map_follows_the_refresh_header_before_a_meta_refresh_as_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        Mock::given(method("GET"))
            .and(path("/s"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(meta_refresh_page("0; url=/m"))
                    .append_header("content-type", "text/html")
                    .append_header("refresh", "0; url=/h"),
            )
            .mount(&mock)
            .await;
        mount_body(&mock, "/h", "text/html", page_linking_to("/from-h")).await;
        mount_body(&mock, "/m", "text/html", page_linking_to("/from-m")).await;
        let config = local_test_config();

        let urls = map_urls(&format!("{base}/s"), &config).await;

        assert_eq!(urls, vec![format!("{base}/from-h")]);
        assert_eq!(
            crawl_stop(&base, &format!("{base}/s"), &config).await,
            ("/h".to_owned(), 200)
        );
    }

    #[tokio::test]
    async fn map_sends_custom_headers_only_to_the_seed_host_along_a_meta_refresh() {
        let seed = MockServer::start().await;
        let other = MockServer::start().await;
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        mount_body(&seed, "/start", "text/html", meta_refresh_page("0; url=/mid")).await;
        let to_other = meta_refresh_page(&format!("0; url={}/page.html", other.uri()));
        mount_body(&seed, "/mid", "text/html", to_other).await;
        mount_body(&other, "/page.html", "text/html", page_linking_to("x.html")).await;
        let seed_url = Url::parse(&seed_base).expect("mock URL must parse");
        let config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(
                &seed_url,
                Some(("user".to_owned(), "hunter2".to_owned())),
            ),
            custom_headers: std::collections::HashMap::from([("x-seed-secret".to_owned(), "s3cr3t".to_owned())]),
            ..local_test_config()
        };

        let urls = map_urls(&format!("{seed_base}/start"), &config).await;

        assert_eq!(urls, vec![format!("{}/x.html", other.uri())]);
        let with_secret = |requests: Vec<wiremock::Request>, route: &str| {
            requests
                .iter()
                .filter(|r| r.url.path() == route && r.headers.contains_key("x-seed-secret"))
                .count()
        };
        let seed_requests = seed.received_requests().await.expect("wiremock records requests");
        let other_requests = other.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            with_secret(seed_requests, "/mid"),
            1,
            "a refresh hop on the seed host gets the custom header"
        );
        assert_eq!(
            with_secret(other_requests, "/page.html"),
            0,
            "the refresh target on another host must not"
        );
    }

    #[tokio::test]
    async fn map_stops_a_redirect_chain_at_the_limit_as_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/r0", "/r1").await;
        mount_redirect(&mock, "/r1", "/r2").await;
        mount_body(&mock, "/r2", "text/html", page_linking_to("/from-r2")).await;
        let config = CrawlConfig {
            max_redirects: 1,
            ..local_test_config()
        };

        let result = map(&format!("{base}/r0"), &config).await;

        assert!(
            matches!(&result, Ok(mapped) if mapped.urls.is_empty()),
            "map must stop on the second 301 without failing, got {result:?}"
        );
        assert_eq!(
            request_count(&mock, "/r2").await,
            0,
            "a hop past the limit is not requested"
        );
        assert_eq!(
            crawl_stop(&base, &format!("{base}/r0"), &config).await,
            ("/r1".to_owned(), 301)
        );
    }

    #[tokio::test]
    async fn map_does_not_follow_a_location_on_a_non_redirect_3xx_as_the_crawl_does() {
        // ~keep 300, 304 and 305 name a Location, but the crawl's REDIRECT_STATUSES only
        // ~keep follows 301, 302, 303, 307 and 308; map must stop on these the same way.
        for status in [300u16, 304, 305] {
            let mock = MockServer::start().await;
            let base = mock.uri();
            Mock::given(method("GET"))
                .and(path("/s"))
                .respond_with(ResponseTemplate::new(status).append_header("location", "/t"))
                .mount(&mock)
                .await;
            mount_body(&mock, "/t", "text/html", page_linking_to("/from-t")).await;
            let config = local_test_config();

            let result = map(&format!("{base}/s"), &config).await;

            assert!(
                matches!(&result, Ok(mapped) if mapped.urls.is_empty()),
                "status {status}: map must stop on the response without following its Location, got {result:?}"
            );
            assert_eq!(
                request_count(&mock, "/t").await,
                0,
                "status {status}: a non-redirect 3xx's Location is not requested"
            );
            assert_eq!(
                crawl_stop(&base, &format!("{base}/s"), &config).await,
                ("/s".to_owned(), status),
                "status {status}: the crawl stops on the seed's own response"
            );
        }
    }

    #[tokio::test]
    async fn map_stops_on_a_redirect_target_that_is_not_found_as_the_crawl_does() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/r0", "/missing").await;
        let config = local_test_config();

        let result = map(&format!("{base}/r0"), &config).await;

        assert!(
            matches!(&result, Ok(mapped) if mapped.urls.is_empty()),
            "map must stop on the missing page without failing, got {result:?}"
        );
        assert_eq!(
            crawl_stop(&base, &format!("{base}/r0"), &config).await,
            ("/missing".to_owned(), 404)
        );
    }

    #[tokio::test]
    async fn map_resolves_a_relative_urlset_loc_against_the_url_after_a_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/sitemap.xml", "/nested/sitemap.xml").await;
        mount_body(
            &mock,
            "/nested/sitemap.xml",
            "application/xml",
            urlset(&["page".to_owned()]),
        )
        .await;

        let urls = map_urls(&base, &local_test_config()).await;

        assert_eq!(
            urls,
            vec![format!("{base}/nested/page")],
            "a relative <loc> must resolve against the URL that served the sitemap, not the one requested"
        );
    }

    #[tokio::test]
    async fn map_resolves_a_relative_index_child_against_the_url_after_a_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/sitemap.xml", "/nested/index.xml").await;
        mount_body(
            &mock,
            "/nested/index.xml",
            "application/xml",
            sitemap_index(&["child.xml"]),
        )
        .await;
        mount_body(
            &mock,
            "/nested/child.xml",
            "application/xml",
            urlset(&["https://example.com/from-child".to_owned()]),
        )
        .await;
        mount_body(&mock, "/", "text/html", "<html><body></body></html>".to_owned()).await;

        let urls = map_urls(&base, &local_test_config()).await;

        assert_eq!(
            urls,
            vec!["https://example.com/from-child".to_owned()],
            "a relative index child must resolve against the URL that served the index"
        );
    }

    #[tokio::test]
    async fn map_fetches_an_absolute_index_child_from_its_own_host_after_the_index_redirected() {
        let requested = MockServer::start().await;
        let serving = MockServer::start().await;
        let requested_base = requested.uri();
        // ~keep A second host name for the same loopback address, so the redirect crosses hosts.
        let serving_base = serving.uri().replace("127.0.0.1", "localhost");
        mount_redirect(&requested, "/sitemap.xml", &format!("{serving_base}/index.xml")).await;
        mount_body(
            &serving,
            "/index.xml",
            "application/xml",
            sitemap_index(&[&format!("{requested_base}/child.xml")]),
        )
        .await;
        mount_body(
            &serving,
            "/child.xml",
            "application/xml",
            urlset(&["https://example.com/from-serving-host".to_owned()]),
        )
        .await;
        mount_body(
            &requested,
            "/child.xml",
            "application/xml",
            urlset(&["https://example.com/from-requested-host".to_owned()]),
        )
        .await;

        let urls = map_urls(&requested_base, &local_test_config()).await;

        assert_eq!(
            urls,
            vec!["https://example.com/from-requested-host".to_owned()],
            "an absolute index child must be fetched from its own host, not the host that served the index"
        );
    }

    #[test]
    #[serial_test::serial(sitemap_redaction_log)]
    fn map_fetches_index_children_on_three_other_hosts_from_their_own_hosts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime must build");
        let index_server = runtime.block_on(MockServer::start());
        let children: Vec<MockServer> = (0..3).map(|_| runtime.block_on(MockServer::start())).collect();
        // ~keep The index host is `localhost`, each child a distinct `127.0.0.1` port, so every
        // ~keep child is on another host than the index.
        let index_base = index_server.uri().replace("127.0.0.1", "localhost");
        let child_locs: Vec<String> = children.iter().map(|c| format!("{}/sitemap.xml", c.uri())).collect();
        let child_refs: Vec<&str> = child_locs.iter().map(String::as_str).collect();
        runtime.block_on(mount_body(
            &index_server,
            "/sitemap.xml",
            "application/xml",
            sitemap_index(&child_refs),
        ));
        runtime.block_on(mount_body(&index_server, "/", "text/html", "<html></html>".to_owned()));
        let mut expected = Vec::new();
        for (i, child) in children.iter().enumerate() {
            let page = format!("https://example.com/from-child-{i}");
            runtime.block_on(mount_body(
                child,
                "/sitemap.xml",
                "application/xml",
                urlset(std::slice::from_ref(&page)),
            ));
            expected.push(page);
        }

        let (result, fields) = capture_events(|| runtime.block_on(map(&index_base, &local_test_config())));

        let urls: Vec<String> = result
            .expect("map should succeed")
            .urls
            .into_iter()
            .map(|u| u.url)
            .collect();
        assert_eq!(
            urls, expected,
            "each cross-host index child must be fetched from its own host"
        );
        for (i, child) in children.iter().enumerate() {
            let hits = runtime
                .block_on(child.received_requests())
                .expect("wiremock records requests")
                .len();
            assert_eq!(hits, 1, "child server {i} must get exactly one GET, got {hits}");
        }
        let cycles: Vec<&(String, String)> = fields.iter().filter(|(_, v)| v.contains("cycle detected")).collect();
        assert!(cycles.is_empty(), "no child may be skipped as a cycle, got {cycles:?}");
    }

    #[tokio::test]
    async fn map_refuses_a_cross_host_index_child_the_ssrf_policy_denies_and_fetches_its_allowed_sibling() {
        let index_server = MockServer::start().await;
        let denied = MockServer::start().await;
        let allowed = MockServer::start().await;
        // ~keep Only the host name `localhost` is allowlisted, so the literal `127.0.0.1` child is
        // ~keep refused before any connection while the `localhost` sibling is fetched.
        let index_base = index_server.uri().replace("127.0.0.1", "localhost");
        let allowed_base = allowed.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &index_server,
            "/sitemap.xml",
            "application/xml",
            sitemap_index(&[
                &format!("{}/sitemap.xml", denied.uri()),
                &format!("{allowed_base}/sitemap.xml"),
            ]),
        )
        .await;
        for (server, page) in [(&denied, "from-denied"), (&allowed, "from-allowed")] {
            mount_body(
                server,
                "/sitemap.xml",
                "application/xml",
                urlset(&[format!("https://example.com/{page}")]),
            )
            .await;
        }
        let config = CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder()
                .ssrf_allowlist_host(crate::HostMatcher::exact("localhost"))
                .build()
        };

        let urls = map_urls(&index_base, &config).await;

        // ~keep GUARD: green before the cross-host change too, where the denied child was moved
        // ~keep onto the index host; it pins that the SSRF policy still gates each child fetch.
        assert_eq!(urls, vec!["https://example.com/from-allowed".to_owned()]);
        let denied_hits = denied
            .received_requests()
            .await
            .expect("wiremock records requests")
            .len();
        assert_eq!(denied_hits, 0, "a child the SSRF policy denies must never be requested");
    }

    #[tokio::test]
    async fn map_refuses_a_robots_sitemap_line_on_a_denied_host_and_fetches_its_allowed_sibling() {
        let seed = MockServer::start().await;
        let denied = MockServer::start().await;
        let allowed = MockServer::start().await;
        // ~keep Only the host name `localhost` is allowlisted, so the robots.txt Sitemap: line on
        // ~keep the literal `127.0.0.1` denied host is refused before any connection while the
        // ~keep `localhost` sibling line is fetched.
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        let allowed_base = allowed.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &seed,
            "/robots.txt",
            "text/plain",
            format!(
                "User-agent: *\nSitemap: {}/s.xml\nSitemap: {allowed_base}/s.xml\n",
                denied.uri()
            ),
        )
        .await;
        mount_body(
            &denied,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-denied".to_owned()]),
        )
        .await;
        mount_body(
            &allowed,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-allowed".to_owned()]),
        )
        .await;
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..CrawlConfig::builder()
                .ssrf_allowlist_host(crate::HostMatcher::exact("localhost"))
                .build()
        };

        let urls = map_urls(&seed_base, &config).await;

        assert_eq!(urls, vec!["https://example.com/from-allowed".to_owned()]);
        let denied_hits = denied
            .received_requests()
            .await
            .expect("wiremock records requests")
            .len();
        assert_eq!(
            denied_hits, 0,
            "a robots.txt Sitemap: line on a host the SSRF policy denies must never be requested"
        );
    }

    #[tokio::test]
    async fn map_refuses_a_robots_sitemap_redirect_to_a_denied_host() {
        let seed = MockServer::start().await;
        let hop = MockServer::start().await;
        let denied = MockServer::start().await;
        // ~keep The hop is on the allowlisted host `localhost`; the redirect it sends points at
        // ~keep the literal `127.0.0.1` host, which the SSRF policy denies.
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        let hop_base = hop.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &seed,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {hop_base}/s.xml\n"),
        )
        .await;
        mount_redirect(&hop, "/s.xml", &format!("{}/s.xml", denied.uri())).await;
        mount_body(
            &denied,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-denied".to_owned()]),
        )
        .await;
        mount_body(&seed, "/", "text/html", "<html></html>".to_owned()).await;
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..CrawlConfig::builder()
                .ssrf_allowlist_host(crate::HostMatcher::exact("localhost"))
                .build()
        };

        let urls = map_urls(&seed_base, &config).await;

        assert!(
            !urls.contains(&"https://example.com/from-denied".to_owned()),
            "got {urls:?}"
        );
        let hop_hits = hop.received_requests().await.expect("wiremock records requests").len();
        assert_eq!(hop_hits, 1, "the allowlisted redirect hop must still be requested");
        let denied_hits = denied
            .received_requests()
            .await
            .expect("wiremock records requests")
            .len();
        assert_eq!(
            denied_hits, 0,
            "a robots.txt Sitemap: redirect to a host the SSRF policy denies must never be followed"
        );
    }

    #[tokio::test]
    async fn map_sends_seed_credentials_to_the_index_host_but_not_to_a_cross_host_child() {
        let index_server = MockServer::start().await;
        let child = MockServer::start().await;
        let index_base = index_server.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &index_server,
            "/sitemap.xml",
            "application/xml",
            sitemap_index(&[&format!("{}/sitemap.xml", child.uri())]),
        )
        .await;
        mount_body(
            &child,
            "/sitemap.xml",
            "application/xml",
            urlset(&["https://example.com/from-child".to_owned()]),
        )
        .await;
        let seed = Url::parse(&index_base).expect("mock URL must parse");
        let config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(
                &seed,
                Some(("user".to_owned(), "hunter2".to_owned())),
            ),
            ..local_test_config()
        };

        let urls = map_urls(&index_base, &config).await;

        assert_eq!(urls, vec!["https://example.com/from-child".to_owned()]);
        let authorized = |requests: Vec<wiremock::Request>| {
            requests
                .iter()
                .filter(|request| request.headers.contains_key("authorization"))
                .count()
        };
        let index_requests = index_server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(
            authorized(index_requests),
            1,
            "the index host must get the seed credentials"
        );
        // ~keep GUARD: the seed-host scope keeps credentials off every other host; it passed before
        // ~keep the cross-host change too, where no request left the seed host.
        let child_requests = child.received_requests().await.expect("wiremock records requests");
        assert_eq!(child_requests.len(), 1, "the child must be fetched once");
        assert_eq!(
            authorized(child_requests),
            0,
            "a cross-host child must not get the seed credentials"
        );
    }

    #[tokio::test]
    async fn map_sends_seed_credentials_to_the_seed_host_but_not_to_a_cross_host_robots_sitemap() {
        let seed = MockServer::start().await;
        let other = MockServer::start().await;
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &seed,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {}/s.xml\n", other.uri()),
        )
        .await;
        mount_body(
            &other,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-other".to_owned()]),
        )
        .await;
        let seed_url = Url::parse(&seed_base).expect("mock URL must parse");
        let mut config = CrawlConfig {
            respect_robots_txt: true,
            credential_scope: crate::net::CredentialScope::for_seed(
                &seed_url,
                Some(("user".to_owned(), "hunter2".to_owned())),
            ),
            ..local_test_config()
        };
        config
            .custom_headers
            .insert("x-test-secret".to_owned(), "s3".to_owned());

        let urls = map_urls(&seed_base, &config).await;

        assert_eq!(urls, vec!["https://example.com/from-other".to_owned()]);
        let header_hits = |requests: &[wiremock::Request], name: &str| {
            requests.iter().filter(|r| r.headers.contains_key(name)).count()
        };
        let seed_requests = seed.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            header_hits(&seed_requests, "authorization"),
            1,
            "robots.txt must get the seed credential"
        );
        assert_eq!(
            header_hits(&seed_requests, "x-test-secret"),
            1,
            "robots.txt must get the custom header"
        );
        let other_requests = other.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            header_hits(&other_requests, "authorization"),
            0,
            "a credential must not leak to a robots.txt Sitemap: line on another host"
        );
        assert_eq!(
            header_hits(&other_requests, "x-test-secret"),
            0,
            "a custom header must not leak to a robots.txt Sitemap: line on another host"
        );
    }

    #[tokio::test]
    async fn map_sends_a_custom_header_to_the_index_host_but_not_to_a_cross_host_child() {
        let index_server = MockServer::start().await;
        let child = MockServer::start().await;
        let index_base = index_server.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &index_server,
            "/sitemap.xml",
            "application/xml",
            sitemap_index(&[&format!("{}/sitemap.xml", child.uri())]),
        )
        .await;
        mount_body(
            &child,
            "/sitemap.xml",
            "application/xml",
            urlset(&["https://example.com/from-child".to_owned()]),
        )
        .await;
        let index_url = Url::parse(&index_base).expect("mock URL must parse");
        let mut config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(&index_url, None),
            ..local_test_config()
        };
        config
            .custom_headers
            .insert("x-test-secret".to_owned(), "s3".to_owned());

        let urls = map_urls(&index_base, &config).await;

        assert_eq!(urls, vec!["https://example.com/from-child".to_owned()]);
        let header_hits = |requests: &[wiremock::Request], name: &str| {
            requests.iter().filter(|r| r.headers.contains_key(name)).count()
        };
        let index_requests = index_server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(
            header_hits(&index_requests, "x-test-secret"),
            1,
            "the index host must get the custom header"
        );
        let child_requests = child.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            header_hits(&child_requests, "x-test-secret"),
            0,
            "a custom header must not leak to a cross-host index child"
        );
    }

    #[tokio::test]
    async fn map_drops_seed_credentials_when_a_same_host_child_redirects_to_another_host() {
        let index_server = MockServer::start().await;
        let other = MockServer::start().await;
        let index_base = index_server.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &index_server,
            "/sitemap.xml",
            "application/xml",
            sitemap_index(&[&format!("{index_base}/child.xml")]),
        )
        .await;
        mount_redirect(&index_server, "/child.xml", &format!("{}/s.xml", other.uri())).await;
        mount_body(
            &other,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-other".to_owned()]),
        )
        .await;
        let seed_url = Url::parse(&index_base).expect("mock URL must parse");
        let config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(
                &seed_url,
                Some(("user".to_owned(), "hunter2".to_owned())),
            ),
            ..local_test_config()
        };

        let urls = map_urls(&index_base, &config).await;

        assert_eq!(urls, vec!["https://example.com/from-other".to_owned()]);
        let authorized = |requests: &[wiremock::Request]| {
            requests
                .iter()
                .filter(|r| r.headers.contains_key("authorization"))
                .count()
        };
        let index_requests = index_server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(
            authorized(&index_requests),
            2,
            "the index and the same-host redirect hop must both get the seed credential"
        );
        let other_requests = other.received_requests().await.expect("wiremock records requests");
        assert_eq!(
            authorized(&other_requests),
            0,
            "a credential must not follow a redirect off the seed host"
        );
    }

    #[tokio::test]
    async fn map_resolves_a_redirected_robots_sitemap_against_the_url_after_the_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(
            &mock,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {base}/custom.xml\n"),
        )
        .await;
        mount_redirect(&mock, "/custom.xml", "/nested/custom.xml").await;
        mount_body(
            &mock,
            "/nested/custom.xml",
            "application/xml",
            urlset(&["page".to_owned()]),
        )
        .await;
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };

        let urls = map_urls(&base, &config).await;

        assert_eq!(urls, vec![format!("{base}/nested/page")]);
    }

    #[tokio::test]
    async fn map_resolves_a_relative_robots_sitemap_against_the_robots_url_after_a_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/robots.txt", "/moved/robots.txt").await;
        mount_body(
            &mock,
            "/moved/robots.txt",
            "text/plain",
            "User-agent: *\nSitemap: s.xml\n".to_owned(),
        )
        .await;
        for (route, page) in [("/moved/s.xml", "from-moved"), ("/s.xml", "from-seed-path")] {
            mount_body(
                &mock,
                route,
                "application/xml",
                urlset(&[format!("https://example.com/{page}")]),
            )
            .await;
        }
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };

        let urls = map_urls(&base, &config).await;

        assert_eq!(
            urls,
            vec!["https://example.com/from-moved".to_owned()],
            "a relative Sitemap: line must resolve against the robots.txt address after its redirect"
        );
    }

    #[tokio::test]
    async fn map_fetches_a_robots_sitemap_on_another_host_from_that_host() {
        let seed = MockServer::start().await;
        let other = MockServer::start().await;
        // ~keep The seed host is `localhost` and the sitemap is on `127.0.0.1`, another host.
        let seed_base = seed.uri().replace("127.0.0.1", "localhost");
        mount_body(
            &seed,
            "/robots.txt",
            "text/plain",
            format!("User-agent: *\nSitemap: {}/s.xml#top\n", other.uri()),
        )
        .await;
        mount_body(&seed, "/", "text/html", "<html></html>".to_owned()).await;
        mount_body(
            &other,
            "/s.xml",
            "application/xml",
            urlset(&["https://example.com/from-other-host".to_owned()]),
        )
        .await;
        let config = CrawlConfig {
            respect_robots_txt: true,
            ..local_test_config()
        };

        let urls = map_urls(&seed_base, &config).await;

        assert_eq!(
            urls,
            vec!["https://example.com/from-other-host".to_owned()],
            "a robots.txt Sitemap: line on another host must be fetched from that host"
        );
        let requests = other.received_requests().await.expect("wiremock records requests");
        assert_eq!(requests.len(), 1, "the sitemap must be fetched once");
    }

    #[tokio::test]
    async fn map_fetches_a_redirected_index_that_lists_itself_once() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/sitemap.xml", "/nested/index.xml").await;
        mount_body(
            &mock,
            "/nested/index.xml",
            "application/xml",
            sitemap_index(&["index.xml", "child.xml"]),
        )
        .await;
        mount_body(
            &mock,
            "/nested/child.xml",
            "application/xml",
            urlset(&["page".to_owned()]),
        )
        .await;

        let urls = map_urls(&base, &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/nested/page")]);
        let requests = mock.received_requests().await.expect("wiremock records requests");
        let index_gets = requests.iter().filter(|r| r.url.path() == "/nested/index.xml").count();
        assert_eq!(index_gets, 1, "the index that served the redirect must be fetched once");
    }

    #[tokio::test]
    async fn map_resolves_a_redirected_index_child_against_the_url_after_the_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_body(&mock, "/sitemap.xml", "application/xml", sitemap_index(&["/child.xml"])).await;
        mount_redirect(&mock, "/child.xml", "/nested/child.xml").await;
        mount_body(
            &mock,
            "/nested/child.xml",
            "application/xml",
            urlset(&["page".to_owned()]),
        )
        .await;

        let urls = map_urls(&base, &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/nested/page")]);
    }

    #[tokio::test]
    async fn map_resolves_a_directly_fetched_urlset_against_the_url_after_a_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/feed", "/nested/feed.xml").await;
        mount_body(
            &mock,
            "/nested/feed.xml",
            "application/xml",
            urlset(&["page".to_owned()]),
        )
        .await;

        let urls = map_urls(&format!("{base}/feed"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/nested/page")]);
    }

    #[tokio::test]
    async fn map_resolves_a_directly_fetched_gzip_urlset_against_the_url_after_a_redirect() {
        use std::io::Write as _;

        let mock = MockServer::start().await;
        let base = mock.uri();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(urlset(&["page".to_owned()]).as_bytes())
            .expect("gzip write");
        let gzipped = encoder.finish().expect("gzip finish");
        mount_redirect(&mock, "/feed", "/nested/feed.xml.gz").await;
        mount_bytes(&mock, "/nested/feed.xml.gz", "application/octet-stream", gzipped).await;

        let urls = map_urls(&format!("{base}/feed"), &local_test_config()).await;

        assert_eq!(urls, vec![format!("{base}/nested/page")]);
    }

    #[tokio::test]
    async fn map_resolves_a_directly_fetched_html_pages_link_against_the_url_after_a_redirect() {
        let mock = MockServer::start().await;
        let base = mock.uri();
        mount_redirect(&mock, "/start", "/dir/page.html").await;
        mount_body(
            &mock,
            "/dir/page.html",
            "text/html",
            "<html><body><a href=\"x.html\">next</a></body></html>".to_owned(),
        )
        .await;

        let urls = map_urls(&format!("{base}/start"), &local_test_config()).await;

        assert_eq!(
            urls,
            vec![format!("{base}/dir/x.html")],
            "a relative href must resolve against the URL that served the page, not the one requested"
        );
    }

    /// A native fetch always returns a parseable `final_url`, so this calls
    /// `urls_from_direct_response` directly to force the branch `map()` cannot reach
    /// through a real HTTP round trip.
    #[tokio::test]
    async fn a_directly_fetched_html_pages_link_falls_back_to_the_requested_url_when_the_final_url_does_not_parse() {
        let config = local_test_config();
        let client = build_client(&config).expect("build_client should succeed");
        let filter = MapFilter::from_config(&config).expect("MapFilter::from_config should succeed");
        let context = SitemapWalkContext::new(&config, &client, &filter);
        let requested = "http://example.test/dir/start";
        let parsed_url = Url::parse(requested).expect("the requested URL must parse");
        let body = "<html><body><a href=\"x.html\">next</a></body></html>";

        for final_url in ["", "not a url", "http://[::1"] {
            let resp = crate::http::HttpResponse {
                status: 200,
                content_type: "text/html".to_owned(),
                body: body.to_owned(),
                body_bytes: body.as_bytes().to_vec(),
                headers: Default::default(),
                browser_extras: None,
                final_url: final_url.to_owned(),
                screenshot: None,
            };

            let urls: Vec<String> = urls_from_direct_response(requested, &parsed_url, &resp, None, &config, &context)
                .await
                .into_iter()
                .map(|u| u.url)
                .collect();

            assert_eq!(
                urls,
                vec!["http://example.test/dir/x.html".to_owned()],
                "an unparseable final URL {final_url:?} must fall back to the requested URL, not a fixed default"
            );
        }
    }
}
