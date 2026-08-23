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
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, Event};
use url::Url;

use crate::http::http_fetch;
use crate::map::MapFilter;
use crate::normalize::{resolve_redirect, rewrite_url_host};
use crate::types::{CrawlConfig, SitemapUrl};

/// Which text-bearing child of a `<url>` entry the reader is currently inside.
#[derive(Clone, Copy)]
enum UrlField {
    Loc,
    LastMod,
    ChangeFreq,
    Priority,
}

/// Append the text an entity reference stands for to `buffer`.
///
/// ~keep quick-xml reports entity references as standalone `Event::GeneralRef` events
/// rather than folding them into the surrounding `Event::Text`, so a reference both
/// splits an element's character data and carries a character of its own. Dropping
/// these events loses that character: `?a=1&amp;b=2` would rejoin as `?a=1b=2`.
/// An entity this parser cannot resolve — a document may declare its own in a DTD —
/// contributes nothing, matching how the surrounding parser skips what it cannot read.
fn push_entity_ref(buffer: &mut String, entity: &BytesRef<'_>) {
    match entity.resolve_char_ref() {
        Ok(Some(character)) => buffer.push(character),
        Ok(None) => {
            if let Some(text) = resolve_predefined_entity(entity.as_ref()) {
                buffer.push_str(text);
            }
        }
        Err(_) => {}
    }
}

/// Parse a sitemap XML document and extract URL entries.
pub fn parse_sitemap_xml(body: &str) -> Vec<SitemapUrl> {
    let mut urls = Vec::new();

    let mut reader = Reader::from_str(body);
    let mut buf = Vec::new();
    let mut in_url = false;
    let mut current_field: Option<UrlField> = None;
    // ~keep Character data arrives in several events whenever entity references appear
    // ~keep inside an element, so text is accumulated here and only committed to a field
    // ~keep on the closing tag.
    let mut text = String::new();
    let mut current_loc = String::new();
    let mut current_lastmod: Option<String> = None;
    let mut current_changefreq: Option<String> = None;
    let mut current_priority: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let field = match e.name().as_ref() {
                    "url" => {
                        in_url = true;
                        current_loc.clear();
                        current_lastmod = None;
                        current_changefreq = None;
                        current_priority = None;
                        None
                    }
                    "loc" if in_url => Some(UrlField::Loc),
                    "lastmod" if in_url => Some(UrlField::LastMod),
                    "changefreq" if in_url => Some(UrlField::ChangeFreq),
                    "priority" if in_url => Some(UrlField::Priority),
                    _ => None,
                };
                if field.is_some() {
                    text.clear();
                    current_field = field;
                }
            }
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "url" => {
                    if in_url && !current_loc.is_empty() {
                        urls.push(SitemapUrl {
                            url: current_loc.clone(),
                            lastmod: current_lastmod.clone(),
                            changefreq: current_changefreq.clone(),
                            priority: current_priority.clone(),
                        });
                    }
                    in_url = false;
                    current_field = None;
                }
                "loc" | "lastmod" | "changefreq" | "priority" => {
                    let value = text.trim();
                    if !value.is_empty() {
                        match current_field {
                            Some(UrlField::Loc) => current_loc = value.to_owned(),
                            Some(UrlField::LastMod) => current_lastmod = Some(value.to_owned()),
                            Some(UrlField::ChangeFreq) => current_changefreq = Some(value.to_owned()),
                            Some(UrlField::Priority) => current_priority = Some(value.to_owned()),
                            None => {}
                        }
                    }
                    current_field = None;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref e)) if current_field.is_some() => {
                text.push_str(&e.xml_content(XmlVersion::default()));
            }
            Ok(Event::GeneralRef(ref e)) if current_field.is_some() => push_entity_ref(&mut text, e),
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
    let mut text = String::new();
    let mut current_loc = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                "sitemap" => {
                    in_sitemap = true;
                    current_loc.clear();
                }
                "loc" if in_sitemap => {
                    in_loc = true;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "sitemap" => {
                    if in_sitemap && !current_loc.is_empty() {
                        child_urls.push(current_loc.clone());
                    }
                    in_sitemap = false;
                }
                "loc" => {
                    if in_loc {
                        current_loc = text.trim().to_owned();
                    }
                    in_loc = false;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref e)) if in_loc => {
                text.push_str(&e.xml_content(XmlVersion::default()));
            }
            Ok(Event::GeneralRef(ref e)) if in_loc => push_entity_ref(&mut text, e),
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

/// Maximum sitemap-index nesting depth followed before giving up on a branch.
///
/// Bounds worst-case fetch work independently of the visited-set cycle guard,
/// since a long acyclic chain of indexes would otherwise pass the cycle check
/// while still growing unbounded.
const MAX_SITEMAP_INDEX_DEPTH: u32 = 10;

/// Maximum number of child sitemaps followed from a single index document.
const MAX_SITEMAP_INDEX_CHILDREN: usize = 100;

/// Maximum number of distinct sitemap documents fetched across a whole tree walk.
///
/// ~keep Depth and per-tier breadth bound the tree's *shape*, not its size: 100 distinct
/// children at each of 10 tiers is 100^10 fetches, and the visited set stops only repeats,
/// never distinct URLs. `map_limit` does not help — it bounds the URLs *returned*, so an
/// index tree whose leaves are all empty or all filtered out never reaches it and keeps
/// fetching. This is the only bound on total fetch work, so it counts documents the walk
/// commits to fetching rather than the ones it successfully parses.
const MAX_SITEMAP_DOCUMENTS: usize = 1_000;

/// Process an already-fetched sitemap response body, following sitemap index
/// references if needed. Avoids re-fetching a URL that was already retrieved.
///
/// `filter` and `limit` bound the result incrementally: entries that do not
/// match `filter` are discarded as they are parsed, and both child-sitemap
/// fetching and per-child parsing stop once `limit` matching URLs are collected.
///
/// Nested sitemap indexes (an index that itself points at another index) are
/// followed recursively, bounded by [`MAX_SITEMAP_INDEX_DEPTH`] and a visited
/// set of already-fetched URLs so a self-referential sitemap cannot loop
/// forever.
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
    let mut visited = std::collections::HashSet::new();
    visited.insert(sitemap_url.to_owned());
    process_sitemap_response_inner(
        sitemap_url,
        body,
        body_bytes,
        content_type,
        config,
        client,
        filter,
        limit,
        0,
        &mut visited,
    )
    .await
}

/// Recursive worker behind [`process_sitemap_response`]. See that function's
/// docs for the general contract; `depth` and `visited` are the recursion
/// guards threaded through nested sitemap-index fetches.
#[allow(clippy::too_many_arguments)]
async fn process_sitemap_response_inner(
    sitemap_url: &str,
    body: &str,
    body_bytes: &[u8],
    content_type: &str,
    config: &CrawlConfig,
    client: &reqwest::Client,
    filter: &MapFilter,
    limit: Option<usize>,
    depth: u32,
    visited: &mut std::collections::HashSet<String>,
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

    if !is_sitemap_index(xml_body) {
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
        return urls;
    }

    if depth >= MAX_SITEMAP_INDEX_DEPTH {
        tracing::warn!(
            sitemap_url = %sitemap_url,
            depth,
            max_depth = MAX_SITEMAP_INDEX_DEPTH,
            "skipping sitemap index tier: max nesting depth exceeded"
        );
        return Vec::new();
    }

    let child_urls = parse_sitemap_index(xml_body);
    let base = Url::parse(sitemap_url).ok();
    let mut all_urls = Vec::new();
    for child_url in child_urls.iter().take(MAX_SITEMAP_INDEX_CHILDREN) {
        if reached_limit(all_urls.len()) {
            break;
        }
        // ~keep `visited` holds every document the walk has committed to fetching, root
        // included, and is shared across the whole recursion — so its length is the
        // running total this cap is expressed in.
        if visited.len() >= MAX_SITEMAP_DOCUMENTS {
            tracing::warn!(
                sitemap_url = %sitemap_url,
                fetched = visited.len(),
                max_documents = MAX_SITEMAP_DOCUMENTS,
                "stopping sitemap walk: fetched the maximum number of sitemap documents"
            );
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

        if !visited.insert(resolved.clone()) {
            tracing::warn!(
                sitemap_url = %resolved,
                "skipping sitemap index tier: cycle detected (already visited)"
            );
            continue;
        }

        let child_resp = match http_fetch(&resolved, config, &std::collections::HashMap::new(), client).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let remaining = limit.map(|limit| limit.saturating_sub(all_urls.len()));

        let child_entries = Box::pin(process_sitemap_response_inner(
            &resolved,
            &child_resp.body,
            &child_resp.body_bytes,
            &child_resp.content_type,
            config,
            client,
            filter,
            remaining,
            depth + 1,
            visited,
        ))
        .await;

        for entry in child_entries {
            all_urls.push(entry);
            if reached_limit(all_urls.len()) {
                break;
            }
        }
    }
    all_urls
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

    fn sitemap_index_xml(child_locs: &[&str]) -> String {
        let mut body =
            String::from(r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
        for loc in child_locs {
            body.push_str(&format!("<sitemap><loc>{loc}</loc></sitemap>"));
        }
        body.push_str("</sitemapindex>");
        body
    }

    async fn mount_xml(mock: &MockServer, route: &str, body: String) {
        let response = ResponseTemplate::new(200)
            .set_body_string(body)
            .append_header("content-type", "application/xml");
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(mock)
            .await;
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_follows_nested_sitemap_index() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep root index -> child index -> grandchild urlset (two tiers of nesting).
        mount_xml(&mock, "/root.xml", sitemap_index_xml(&[&format!("{base}/child.xml")])).await;
        mount_xml(
            &mock,
            "/child.xml",
            sitemap_index_xml(&[&format!("{base}/grandchild.xml")]),
        )
        .await;
        mount_xml(&mock, "/grandchild.xml", urlset(2)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/root.xml"), &config, &client, &filter, None).await;

        assert_eq!(
            urls.len(),
            2,
            "URLs from a doubly-nested sitemap index (index -> index -> urlset) must not be dropped, got {urls:?}"
        );
        assert!(
            urls.iter().all(|u| u.url.contains("example.com/page-")),
            "expected grandchild urlset entries, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_terminates_on_self_referential_cycle() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_xml(&mock, "/cycle.xml", sitemap_index_xml(&[&format!("{base}/cycle.xml")])).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/cycle.xml"), &config, &client, &filter, None).await;

        assert!(
            urls.is_empty(),
            "a self-referential sitemap index must terminate with no URLs, not loop forever, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_terminates_on_mutual_cycle() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        mount_xml(&mock, "/a.xml", sitemap_index_xml(&[&format!("{base}/b.xml")])).await;
        mount_xml(&mock, "/b.xml", sitemap_index_xml(&[&format!("{base}/a.xml")])).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/a.xml"), &config, &client, &filter, None).await;

        assert!(
            urls.is_empty(),
            "a mutual two-hop sitemap index cycle must terminate with no URLs, not loop forever, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_stops_at_max_index_depth() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep Chain of MAX_SITEMAP_INDEX_DEPTH + 2 distinct index tiers ending in a
        // ~keep real urlset leaf, so the leaf is only reachable by exceeding the cap.
        let chain_len = MAX_SITEMAP_INDEX_DEPTH as usize + 2;
        for level in 0..chain_len {
            let next = if level + 1 < chain_len {
                format!("{base}/level{}.xml", level + 1)
            } else {
                format!("{base}/leaf.xml")
            };
            mount_xml(&mock, &format!("/level{level}.xml"), sitemap_index_xml(&[&next])).await;
        }
        mount_xml(&mock, "/leaf.xml", urlset(1)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/level0.xml"), &config, &client, &filter, None).await;

        assert!(
            urls.is_empty(),
            "a sitemap-index chain deeper than MAX_SITEMAP_INDEX_DEPTH must be cut off \
             before reaching the leaf, got {urls:?}"
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_stops_at_max_sitemap_documents() {
        let mock = MockServer::start().await;
        let base = mock.uri();

        // ~keep A tree that is shallow (depth 2, well under MAX_SITEMAP_INDEX_DEPTH) and
        // ~keep narrow per tier (100 children, exactly the per-tier cap), so neither
        // ~keep existing bound fires. Only the aggregate document cap can stop it.
        let tier_one: Vec<String> = (0..MAX_SITEMAP_INDEX_CHILDREN)
            .map(|i| format!("{base}/tier1-{i}.xml"))
            .collect();
        mount_xml(
            &mock,
            "/root.xml",
            sitemap_index_xml(&tier_one.iter().map(String::as_str).collect::<Vec<_>>()),
        )
        .await;

        // ~keep Every tier-1 index but the last points at 100 children that are never
        // ~keep mounted: they resolve, get counted, fail to fetch, and burn budget. The
        // ~keep single real leaf hangs off the LAST tier-1 index, so it is reachable only
        // ~keep if the walk is still fetching after ~9,900 dead children.
        for (index, _) in tier_one.iter().enumerate() {
            let is_last = index + 1 == MAX_SITEMAP_INDEX_CHILDREN;
            let children: Vec<String> = if is_last {
                vec![format!("{base}/leaf.xml")]
            } else {
                (0..MAX_SITEMAP_INDEX_CHILDREN)
                    .map(|child| format!("{base}/dead-{index}-{child}.xml"))
                    .collect()
            };
            mount_xml(
                &mock,
                &format!("/tier1-{index}.xml"),
                sitemap_index_xml(&children.iter().map(String::as_str).collect::<Vec<_>>()),
            )
            .await;
        }
        mount_xml(&mock, "/leaf.xml", urlset(1)).await;

        let config = local_test_config();
        let client = reqwest::Client::new();
        let filter = MapFilter::from_config(&config).unwrap();

        let urls = fetch_sitemap_tree(&format!("{base}/root.xml"), &config, &client, &filter, None).await;

        assert!(
            urls.is_empty(),
            "the walk must stop after MAX_SITEMAP_DOCUMENTS fetches, long before reaching \
             the leaf behind ~9,900 dead children, got {urls:?}"
        );
        assert!(
            mock.received_requests()
                .await
                .is_some_and(|requests| requests.len() <= MAX_SITEMAP_DOCUMENTS),
            "the walk must issue at most MAX_SITEMAP_DOCUMENTS ({MAX_SITEMAP_DOCUMENTS}) fetches"
        );
    }

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
