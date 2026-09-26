//! Single-page scrape operation.

use tl::ParserOptions;
use url::Url;

use crate::assets;
use crate::browser_detect;
use crate::error::CrawlError;
use crate::helpers::{RobotsOutcome, fetch_robots_outcome};
use crate::html::{
    detect_charset, detect_nofollow, detect_noindex, extract_page_data, is_binary_content_type, is_binary_url,
    is_html_content, is_pdf_content,
};
use crate::http::build_client;
use crate::robots::is_path_allowed;
use crate::types::{CrawlConfig, ScrapeResult};

/// Build a `ScrapeResult` from a Tower [`CrawlResponse`](crate::tower::CrawlResponse).
///
/// Runs the extraction pipeline (metadata, links, images, feeds, JSON-LD, assets)
/// on the response body returned by the Tower service stack.
pub(crate) async fn scrape_from_crawl_response(
    url: &str,
    resp: &crate::tower::CrawlResponse,
    config: &CrawlConfig,
) -> Result<ScrapeResult, CrawlError> {
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let auth_header_sent = config.auth.is_some();

    let robots = resolve_robots_status(url, &parsed_url, config, &client).await;
    let response_meta = crate::http::extract_response_meta_from_hashmap(&resp.headers);
    let content_type = resp.content_type.clone();
    let decoded = decode_response_body(resp, &content_type, &parsed_url, config);

    let x_robots_tag = resp.headers.get("x-robots-tag").and_then(|v| v.first().cloned());
    let header_robots = RobotsDirectives::from_header(x_robots_tag.as_deref());

    let downloaded_document = crate::document::build_downloaded_document(
        url,
        &parsed_url,
        &content_type,
        &resp.body_bytes,
        decoded.was_skipped,
        config,
    )
    .await;

    // ~keep The document is parsed exactly once: `extract_page_data`, the meta-robots
    // ~keep probes and asset discovery all read the same `VDom`, and its type is not
    // ~keep nameable outside `crate::html` on wasm, so this stays an inline block.
    let (extraction, asset_refs, page_robots) = {
        let doc = tl::parse(&decoded.body, ParserOptions::default())
            .map_err(|e| CrawlError::other(format!("HTML parse error: {e:?}")))?;
        let page_robots = RobotsDirectives {
            noindex: header_robots.noindex || detect_noindex(&doc),
            nofollow: header_robots.nofollow || detect_nofollow(&doc),
        };
        let extraction = extract_page_data(&doc, &decoded.body, &parsed_url, decoded.is_html, true);
        let asset_refs = discover_page_assets(&doc, &parsed_url, decoded.is_html, config);
        (extraction, asset_refs, page_robots)
    };

    let word_count = extraction.metadata.word_count.unwrap_or(0);
    let js_render_hint = decoded.is_html && browser_detect::detect_js_render_needed(&decoded.body, word_count);
    let downloaded_assets = download_discovered_assets(asset_refs, config, &client).await;
    let markdown =
        crate::markdown::convert_to_markdown(&decoded.body, &parsed_url, &merged_content_config(config)).await;

    Ok(ScrapeResult {
        status_code: resp.status,
        final_url: url.to_owned(),
        content_type,
        html: decoded.body,
        body_size: decoded.body_size,
        metadata: extraction.metadata,
        links: extraction.links,
        images: extraction.images,
        feeds: extraction.feeds,
        json_ld: extraction.json_ld,
        is_allowed: robots.is_allowed,
        crawl_delay: robots.crawl_delay,
        noindex_detected: page_robots.noindex,
        nofollow_detected: page_robots.nofollow,
        x_robots_tag,
        is_pdf: decoded.is_pdf,
        was_skipped: decoded.was_skipped,
        detected_charset: decoded.detected_charset,
        auth_header_sent,
        response_meta: Some(response_meta),
        assets: downloaded_assets,
        js_render_hint,
        browser_used: false,
        markdown,
        extracted_data: None,
        extraction_meta: None,
        screenshot: None,
        screenshot_base64: None,
        downloaded_document,
        browser: None,
    })
}

/// What the site's robots.txt says about this URL, as reported (not enforced) by
/// `scrape()`.
struct RobotsStatus {
    is_allowed: bool,
    crawl_delay: Option<u64>,
}

/// Fetch and evaluate robots.txt for `url`, or report the permissive default when
/// `respect_robots_txt` is off.
async fn resolve_robots_status(
    url: &str,
    parsed_url: &Url,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> RobotsStatus {
    if !config.respect_robots_txt {
        return RobotsStatus {
            is_allowed: true,
            crawl_delay: None,
        };
    }

    // ~keep This used to swallow every fetch error and report `is_allowed: true`, and it
    // never checked the status, so an HTTP error page (451, 405, ...) had its *body*
    // parsed as though it were a robots.txt. `scrape()` reports robots status rather than
    // enforcing it -- the page is fetched by the caller either way -- so failing closed
    // here means reporting `is_allowed: false`, which is the honest answer when the
    // site's policy could not be read.
    // ~keep The `"*"` user-agent is preserved from the previous behaviour; see
    // `helpers::default_robots_user_agent` for why unifying it is deferred.
    let ua = config.user_agent.as_deref().unwrap_or("*");
    match fetch_robots_outcome(url, config, client, ua).await {
        RobotsOutcome::Rules(rules) => RobotsStatus {
            is_allowed: is_path_allowed(parsed_url.path(), &rules),
            crawl_delay: rules.crawl_delay,
        },
        RobotsOutcome::AllowAll => RobotsStatus {
            is_allowed: true,
            crawl_delay: None,
        },
        RobotsOutcome::DisallowAll { reason, .. } => {
            tracing::warn!(
                url = %url,
                reason = %reason,
                "robots.txt unreachable; reporting is_allowed=false"
            );
            RobotsStatus {
                is_allowed: false,
                crawl_delay: None,
            }
        }
    }
}

/// The response body as the extraction pipeline sees it: charset-decoded, then
/// truncated to `max_body_size`, together with the content verdicts derived from it.
struct DecodedBody {
    body: String,
    body_size: usize,
    detected_charset: Option<String>,
    is_pdf: bool,
    is_html: bool,
    was_skipped: bool,
}

/// Decode and size-bound the response body, and classify what it holds.
///
/// ~keep `is_pdf` is sniffed from the decoded but *untruncated* body: a
/// `max_body_size` small enough to cut the PDF header would otherwise flip the
/// verdict, and `was_skipped` is derived from it.
fn decode_response_body(
    resp: &crate::tower::CrawlResponse,
    content_type: &str,
    parsed_url: &Url,
    config: &CrawlConfig,
) -> DecodedBody {
    let mut body = resp.body.clone();
    let detected_charset = detect_charset(content_type, &resp.body_bytes);

    if let Some(ref charset) = detected_charset
        && let Some(decoded) = crate::http::redecode_with_charset(charset, &resp.body_bytes)
    {
        body = decoded;
    }
    let is_pdf = is_pdf_content(content_type, &body);

    let mut body_size = body.len();
    if let Some(max_size) = config.max_body_size {
        crate::http::truncate_body_at_char_boundary(&mut body, max_size);
        body_size = body.len();
    }

    let was_skipped = is_binary_content_type(content_type) || is_binary_url(parsed_url.as_str()) || is_pdf;
    let is_html = is_html_content(content_type, &body);

    DecodedBody {
        body,
        body_size,
        detected_charset,
        is_pdf,
        is_html,
        was_skipped,
    }
}

/// `noindex` / `nofollow` as signalled by an `X-Robots-Tag` header or by the
/// document's own meta tags.
#[derive(Clone, Copy)]
struct RobotsDirectives {
    noindex: bool,
    nofollow: bool,
}

impl RobotsDirectives {
    fn from_header(x_robots_tag: Option<&str>) -> Self {
        let Some(value) = x_robots_tag else {
            return Self {
                noindex: false,
                nofollow: false,
            };
        };
        let lower = value.to_lowercase();
        Self {
            noindex: lower.contains("noindex"),
            nofollow: lower.contains("nofollow"),
        }
    }
}

/// Asset references to download for this page, if asset downloading is enabled.
fn discover_page_assets(
    doc: &tl::VDom<'_>,
    parsed_url: &Url,
    is_html: bool,
    config: &CrawlConfig,
) -> Vec<crate::assets::AssetRef> {
    if config.download_assets && is_html {
        assets::discover_assets(doc, parsed_url)
    } else {
        Vec::new()
    }
}

/// Download the discovered assets, skipping the whole pass when there are none.
async fn download_discovered_assets(
    asset_refs: Vec<crate::assets::AssetRef>,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> Vec<crate::types::DownloadedAsset> {
    if asset_refs.is_empty() {
        return Vec::new();
    }
    assets::download_assets(asset_refs, config, client).await
}

/// The content config to convert markdown with: `config.content`, with any
/// `remove_tags` folded in as additional exclude selectors.
///
/// Shared by every markdown-producing path (`scrape()` and the native `crawl()` engine) so
/// `remove_tags` is honoured consistently; do not duplicate this decision elsewhere.
pub(crate) fn merged_content_config(config: &CrawlConfig) -> std::borrow::Cow<'_, crate::types::ContentConfig> {
    if config.remove_tags.is_empty() {
        return std::borrow::Cow::Borrowed(&config.content);
    }
    let mut merged = config.content.clone();
    merged.exclude_selectors.extend(config.remove_tags.iter().cloned());
    std::borrow::Cow::Owned(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A config that performs no network I/O during a scrape: robots.txt is not
    /// consulted, so the whole pipeline runs off the supplied response alone.
    fn offline_config() -> CrawlConfig {
        CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::default()
        }
    }

    fn response(content_type: &str, body: &str) -> crate::tower::CrawlResponse {
        crate::tower::CrawlResponse {
            status: 200,
            content_type: content_type.to_owned(),
            body: body.to_owned(),
            body_bytes: body.as_bytes().to_vec(),
            headers: HashMap::new(),
            landed_url: None,
        }
    }

    fn response_with_bytes(content_type: &str, body_bytes: Vec<u8>) -> crate::tower::CrawlResponse {
        crate::tower::CrawlResponse {
            status: 200,
            content_type: content_type.to_owned(),
            body: String::from_utf8_lossy(&body_bytes).into_owned(),
            body_bytes,
            headers: HashMap::new(),
            landed_url: None,
        }
    }

    #[tokio::test]
    async fn scrape_reports_allowed_without_network_when_robots_is_not_respected() {
        let resp = response(
            "text/html",
            "<html><head><title>Hi</title></head><body>hello</body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config())
            .await
            .expect("scrape should succeed");

        assert!(result.is_allowed, "robots.txt is not consulted, so the page is allowed");
        assert_eq!(result.crawl_delay, None);
        assert_eq!(result.status_code, 200);
        assert_eq!(result.metadata.title.as_deref(), Some("Hi"));
        assert!(!result.was_skipped);
        assert!(!result.is_pdf);
    }

    #[tokio::test]
    async fn scrape_reads_noindex_and_nofollow_from_the_x_robots_tag_header() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers
            .insert("x-robots-tag".to_owned(), vec!["NoIndex, NoFollow".to_owned()]);

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config())
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "X-Robots-Tag: noindex must be honoured");
        assert!(result.nofollow_detected, "X-Robots-Tag: nofollow must be honoured");
        assert_eq!(result.x_robots_tag.as_deref(), Some("NoIndex, NoFollow"));
    }

    #[tokio::test]
    async fn scrape_reads_noindex_from_the_document_when_the_header_is_absent() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name="robots" content="noindex"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config())
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "a meta robots noindex must be detected");
        assert!(!result.nofollow_detected);
        assert_eq!(result.x_robots_tag, None);
    }

    #[tokio::test]
    async fn scrape_redecodes_the_body_using_the_detected_charset() {
        // ~keep windows-1252 0xE9 is `é`; read as UTF-8 it is invalid and lossy-decodes to
        // U+FFFD, so a result containing `é` proves the redecode ran.
        let mut body_bytes = b"<html><head><meta charset=\"windows-1252\"></head><body>caf".to_vec();
        body_bytes.push(0xE9);
        body_bytes.extend_from_slice(b"</body></html>");

        let resp = response_with_bytes("text/html", body_bytes);
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config())
            .await
            .expect("scrape should succeed");

        assert_eq!(result.detected_charset.as_deref(), Some("windows-1252"));
        assert!(
            result.html.contains("café"),
            "the body must be redecoded with the detected charset, got: {}",
            result.html
        );
    }

    #[tokio::test]
    async fn scrape_truncates_the_body_at_max_body_size() {
        let body = format!("<html><body>{}</body></html>", "x".repeat(500));
        let resp = response("text/html", &body);
        let config = CrawlConfig {
            max_body_size: Some(64),
            ..offline_config()
        };

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &config)
            .await
            .expect("scrape should succeed");

        assert_eq!(result.body_size, 64, "body_size must reflect the truncated body");
        assert_eq!(result.html.len(), 64);
    }

    #[tokio::test]
    async fn scrape_folds_remove_tags_into_the_markdown_exclude_selectors() {
        let html = "<html><body><p>keep this</p><aside>drop this</aside></body></html>";
        let resp = response("text/html", html);

        let baseline = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config())
            .await
            .expect("scrape should succeed");
        let baseline_markdown = baseline.markdown.expect("markdown").content;
        assert!(
            baseline_markdown.contains("drop this"),
            "without remove_tags the aside must survive, got: {baseline_markdown}"
        );

        let config = CrawlConfig {
            remove_tags: vec!["aside".to_owned()],
            ..offline_config()
        };
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &config)
            .await
            .expect("scrape should succeed");
        let markdown = result.markdown.expect("markdown").content;

        assert!(
            markdown.contains("keep this"),
            "unremoved content must survive, got: {markdown}"
        );
        assert!(
            !markdown.contains("drop this"),
            "remove_tags must reach the markdown converter as an exclude selector, got: {markdown}"
        );
    }

    #[tokio::test]
    async fn scrape_marks_a_pdf_response_as_skipped() {
        let resp = response("application/pdf", "%PDF-1.7 not really a pdf");
        let result = scrape_from_crawl_response("https://example.com/doc.pdf", &resp, &offline_config())
            .await
            .expect("scrape should succeed");

        assert!(result.is_pdf, "a PDF content type must be recognised");
        assert!(result.was_skipped, "a PDF must be flagged as skipped for extraction");
    }

    #[tokio::test]
    async fn scrape_rejects_an_unparseable_url() {
        let resp = response("text/html", "<html></html>");
        let error = scrape_from_crawl_response("not a url", &resp, &offline_config())
            .await
            .expect_err("an unparseable URL must be rejected");

        assert!(
            error.to_string().contains("invalid URL"),
            "expected an invalid-URL error, got: {error}"
        );
    }
}
