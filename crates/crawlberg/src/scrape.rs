//! Single-page scrape operation.

use tl::ParserOptions;
use url::Url;

use crate::assets;
use crate::browser_detect;
use crate::error::CrawlError;
use crate::helpers::{RobotsOutcome, default_robots_user_agent, fetch_robots_outcome};
use crate::html::{
    detect_charset, extract_page_data, is_binary_content_type, is_binary_url, is_html_content, is_pdf_content,
    mask_raw_text_markup, robots_meta_contents,
};
use crate::http::build_client;
use crate::robots::is_path_allowed;
use crate::types::{CrawlConfig, ScrapeResult};

/// The `X-Robots-Tag` header value and the directives it carries, returned together because
/// the raw value is reported on `ScrapeResult` while the directives gate link following.
fn header_robots_directives(
    headers: &std::collections::HashMap<String, Vec<String>>,
    user_agent: &str,
) -> (Option<String>, RobotsDirectives) {
    let value = x_robots_tag(headers);
    let directives = RobotsDirectives::from_header_values(headers.get("x-robots-tag"), user_agent);
    (value, directives)
}

/// Build a `ScrapeResult` from a Tower [`CrawlResponse`](crate::tower::CrawlResponse).
///
/// Runs the extraction pipeline (metadata, links, images, feeds, JSON-LD, assets)
/// on the response body returned by the Tower service stack.
/// `document_filter` is the engine's byte-aware document predicate, threaded through so a
/// `scrape()` and the wasm crawl loop (which takes its document from here) honour it too; the
/// native crawl loop builds its own document record in `engine::page_result`.
pub(crate) async fn scrape_from_crawl_response(
    url: &str,
    resp: &crate::tower::CrawlResponse,
    config: &CrawlConfig,
    document_filter: Option<&crate::document::DocumentFilter>,
) -> Result<ScrapeResult, CrawlError> {
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let auth_header_sent = config.auth.is_some();

    let robots = resolve_robots_status(url, &parsed_url, config, &client).await;
    let response_meta = crate::http::extract_response_meta_from_hashmap(&resp.headers);
    let content_type = resp.content_type.clone();
    let decoded = decode_response_body(resp, &content_type, &parsed_url, config);

    let (x_robots_tag, header_robots) = header_robots_directives(&resp.headers, default_robots_user_agent(config));

    let downloaded_document = crate::document::build_downloaded_document_with_filter(
        url,
        &parsed_url,
        crate::document::DocumentInput {
            content_type: &content_type,
            body_bytes: &resp.body_bytes,
            is_document: decoded.was_skipped,
        },
        config,
        document_filter,
    )
    .await;

    let body = extract_from_body(&decoded, &parsed_url, config, &header_robots)?;
    let extraction = body.extraction;

    let word_count = extraction.metadata.word_count.unwrap_or(0);
    let js_render_hint = decoded.is_html && browser_detect::detect_js_render_needed(&decoded.body, word_count);
    let downloaded_assets = download_discovered_assets(body.asset_refs, config, &client).await;
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
        noindex_detected: body.page_robots.noindex,
        nofollow_detected: body.page_robots.nofollow,
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

/// Everything read out of the response body's single parse.
struct BodyExtraction {
    extraction: crate::html::HtmlExtraction,
    asset_refs: Vec<crate::assets::AssetRef>,
    page_robots: RobotsDirectives,
}

/// Everything the extraction pipeline reads out of the response body.
///
/// ~keep The document is parsed exactly once here: `extract_page_data`, the meta-robots probes
/// ~keep and asset discovery all read the same `VDom`. The `VDom` borrows the masked source, so
/// ~keep both stay local to this function and only owned values cross back out.
fn extract_from_body(
    decoded: &DecodedBody,
    parsed_url: &Url,
    config: &CrawlConfig,
    header_robots: &RobotsDirectives,
) -> Result<BodyExtraction, CrawlError> {
    // ~keep Parse the masked source, never `decoded.body`: `tl` reads the contents of
    // ~keep raw-text elements as markup, which both invents tags and hides real ones.
    let parsed_html = mask_raw_text_markup(&decoded.body);
    let doc = tl::parse(&parsed_html, ParserOptions::default())
        .map_err(|e| CrawlError::other(format!("HTML parse error: {e:?}")))?;
    let page_robots = header_robots.with_meta_tags(&doc, default_robots_user_agent(config));
    let extraction = extract_page_data(&doc, &parsed_html, parsed_url, decoded.is_html, true);
    let asset_refs = discover_page_assets(&doc, parsed_url, decoded.is_html, config);
    Ok(BodyExtraction {
        extraction,
        asset_refs,
        page_robots,
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

/// Every `X-Robots-Tag` header on a response, joined with `, `, or `None` when it has none.
///
/// ~keep A response may send the header more than once, and a directive in any of them
/// applies, so reading only the first one missed a `nofollow` in the second.
pub(crate) fn x_robots_tag(headers: &std::collections::HashMap<String, Vec<String>>) -> Option<String> {
    headers
        .get("x-robots-tag")
        .filter(|values| !values.is_empty())
        .map(|values| values.join(", "))
}

/// `noindex` / `nofollow` as signalled by an `X-Robots-Tag` header or by the
/// document's own meta tags.
#[derive(Clone, Copy)]
pub(crate) struct RobotsDirectives {
    pub(crate) noindex: bool,
    pub(crate) nofollow: bool,
}

/// Directive keys that carry their own `key: value` argument, so a leading one in an
/// `X-Robots-Tag` value is a directive and not a crawler name.
const VALUE_BEARING_DIRECTIVES: [&str; 4] = [
    "unavailable_after",
    "max-snippet",
    "max-image-preview",
    "max-video-preview",
];

impl RobotsDirectives {
    /// Parse the `X-Robots-Tag` values a response sent, dropping any addressed to another crawler.
    ///
    /// ~keep Each header value is parsed on its own rather than from the `, `-joined string
    /// `x_robots_tag` reports: joining loses the header boundaries, so a `googlebot: noindex` in
    /// one header would swallow the next header's unscoped directives into googlebot's scope.
    pub(crate) fn from_header_values(values: Option<&Vec<String>>, user_agent: &str) -> Self {
        let ua_lower = user_agent.to_lowercase();
        let mut directives = Self {
            noindex: false,
            nofollow: false,
        };
        for value in values.into_iter().flatten() {
            if let Some(unscoped) = strip_crawler_scope(value, &ua_lower) {
                directives.apply_directives(unscoped);
            }
        }
        directives
    }

    /// Add the directives from the document's robots meta tags.
    pub(crate) fn with_meta_tags(mut self, doc: &tl::VDom<'_>, user_agent: &str) -> Self {
        for content in robots_meta_contents(doc, user_agent) {
            self.apply_directives(&content);
        }
        self
    }

    /// Fold one directive list into `self`.
    ///
    /// ~keep Split on whitespace as well as commas: a page that writes `content="noindex nofollow"`
    /// without the comma is read by every other crawler, and was read here too while this parsed by
    /// substring search.
    fn apply_directives(&mut self, value: &str) {
        for token in value.split(|c: char| c == ',' || c.is_whitespace()) {
            match token.to_lowercase().as_str() {
                // ~keep `none` is defined as `noindex, nofollow`, and a page that says only
                // `none` was previously read as neither.
                "none" => {
                    self.noindex = true;
                    self.nofollow = true;
                }
                "noindex" => self.noindex = true,
                "nofollow" => self.nofollow = true,
                _ => {}
            }
        }
    }
}

/// Strip a leading `crawler:` scope from an `X-Robots-Tag` value.
///
/// Returns the directives that bind us: `value` unchanged when it names no crawler, the
/// remainder when it names ours, and `None` when it names another crawler.
fn strip_crawler_scope<'a>(value: &'a str, ua_lower: &str) -> Option<&'a str> {
    let Some((head, rest)) = value.split_once(':') else {
        return Some(value);
    };
    let head_lower = head.trim().to_lowercase();
    // ~keep A crawler name is a bare product token and comes first, so anything carrying a comma
    // or whitespace -- `nofollow, unavailable_after: <date>` -- is a directive list, not a scope.
    // Dropping such a value as another crawler's would discard directives addressed to everyone.
    let is_product_token = !head_lower.contains(',') && !head_lower.contains(char::is_whitespace);
    if !is_product_token || VALUE_BEARING_DIRECTIVES.contains(&head_lower.as_str()) {
        return Some(value);
    }
    if crate::robots::product_token_addresses_us(&head_lower, ua_lower) {
        return Some(rest);
    }
    None
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "X-Robots-Tag: noindex must be honoured");
        assert!(result.nofollow_detected, "X-Robots-Tag: nofollow must be honoured");
        assert_eq!(result.x_robots_tag.as_deref(), Some("NoIndex, NoFollow"));
    }

    #[tokio::test]
    async fn scrape_reads_nofollow_from_a_second_x_robots_tag_header() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["noarchive".to_owned(), "nofollow".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.nofollow_detected, "every X-Robots-Tag header must be read");
        assert_eq!(result.x_robots_tag.as_deref(), Some("noarchive, nofollow"));
    }

    /// ~keep A guard, not evidence the fix works: the pre-fix substring parse read this too. It
    /// exists to catch the plausible mis-implementation of splitting the directive list on commas
    /// alone, which would stop reading a comma-less `content` every other crawler honours.
    #[tokio::test]
    async fn scrape_reads_a_space_separated_directive_list() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name="robots" content="noindex nofollow"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "a space-separated list must still be read");
        assert!(result.nofollow_detected, "a space-separated list must still be read");
    }

    #[tokio::test]
    async fn scrape_reads_none_as_both_noindex_and_nofollow() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name="robots" content="None"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "`none` must be read as noindex");
        assert!(result.nofollow_detected, "`none` must be read as nofollow");
    }

    #[tokio::test]
    async fn scrape_ignores_an_x_robots_tag_addressed_to_another_crawler() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["googlebot: noindex, nofollow".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(!result.noindex_detected, "googlebot's noindex does not bind us");
        assert!(!result.nofollow_detected, "googlebot's nofollow does not bind us");
        assert_eq!(
            result.x_robots_tag.as_deref(),
            Some("googlebot: noindex, nofollow"),
            "the raw header is still reported verbatim"
        );
    }

    #[tokio::test]
    async fn scrape_reads_directives_from_a_header_beside_one_scoped_to_another_crawler() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["googlebot: noindex".to_owned(), "nofollow".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            !result.noindex_detected,
            "the scoped header binds googlebot only, and its scope must not reach the next header"
        );
        assert!(result.nofollow_detected, "the unscoped header binds every crawler");
    }

    /// ~keep A guard, not evidence the fix works: it passes with the pre-fix substring parse too.
    /// It exists to catch the plausible mis-implementation of reading any leading `key:` as a
    /// crawler name, which would discard the whole value's directives.
    #[tokio::test]
    async fn scrape_reads_a_directive_beside_a_value_bearing_one() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["nofollow, unavailable_after: 25 Jun 2010 15:00:00 PST".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            result.nofollow_detected,
            "`unavailable_after` names a directive, not a crawler, so the value still binds us"
        );
    }

    #[tokio::test]
    async fn scrape_reads_noindex_from_the_document_when_the_header_is_absent() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name="robots" content="noindex"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, &config, None)
            .await
            .expect("scrape should succeed");

        assert_eq!(result.body_size, 64, "body_size must reflect the truncated body");
        assert_eq!(result.html.len(), 64);
    }

    #[tokio::test]
    async fn scrape_folds_remove_tags_into_the_markdown_exclude_selectors() {
        let html = "<html><body><p>keep this</p><aside>drop this</aside></body></html>";
        let resp = response("text/html", html);

        let baseline = scrape_from_crawl_response("https://example.com/page", &resp, &offline_config(), None)
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, &config, None)
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
        let result = scrape_from_crawl_response("https://example.com/doc.pdf", &resp, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.is_pdf, "a PDF content type must be recognised");
        assert!(result.was_skipped, "a PDF must be flagged as skipped for extraction");
    }

    #[tokio::test]
    async fn scrape_rejects_an_unparseable_url() {
        let resp = response("text/html", "<html></html>");
        let error = scrape_from_crawl_response("not a url", &resp, &offline_config(), None)
            .await
            .expect_err("an unparseable URL must be rejected");

        assert!(
            error.to_string().contains("invalid URL"),
            "expected an invalid-URL error, got: {error}"
        );
    }
}
