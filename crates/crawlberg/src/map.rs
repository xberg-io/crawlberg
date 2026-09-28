//! Site mapping operation that discovers URLs via sitemaps and link extraction.

use std::collections::HashSet;

use regex::Regex;
use url::Url;

use crate::error::CrawlError;
use crate::html::{effective_base_url, extract_links, is_html_content, mask_raw_text_markup};
use crate::http::{build_client, fetch_with_retry, http_fetch};
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
    let Ok(sitemap_resp) = http_fetch(&sitemap_url, config, &std::collections::HashMap::new(), client).await else {
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
        let urls = collect_urlset_entries(&resp.final_url, &decompressed, context, config.map_limit);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_xml {
        if is_sitemap_index(&resp.body) {
            return fetch_sitemap_tree(url, context, config.map_limit).await;
        }
        let urls = collect_urlset_entries(&resp.final_url, &resp.body, context, config.map_limit);
        if !urls.is_empty() {
            return urls;
        }
    }

    if is_html_content(&resp.content_type, &resp.body) {
        let parsed_html = mask_raw_text_markup(&resp.body);
        if let Ok(doc) = crate::html::parse_html(&parsed_html) {
            return links_as_sitemap_urls(&doc, &parsed_html, parsed_url);
        }
    }

    Vec::new()
}

/// Gzip member header magic (RFC 1952 §2.3.1), used to sniff a `.gz` sitemap whose
/// content type does not declare the encoding.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Turn a page's extracted links into sitemap entries, deduplicated on the
/// normalized URL. Anchor-only links are not URLs of their own and are skipped.
fn links_as_sitemap_urls(doc: &tl::VDom<'_>, html: &str, parsed_url: &Url) -> Vec<SitemapUrl> {
    let links = extract_links(html, &effective_base_url(doc, parsed_url));
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
            "a <loc> that is only a query or a fragment, or the sitemap's own address, is not a page"
        );
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
}
