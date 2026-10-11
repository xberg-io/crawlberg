//! Single-page scrape operation.

use url::Url;

use crate::assets;
use crate::browser_detect;
use crate::error::CrawlError;
use crate::helpers::{RobotsOutcome, default_robots_user_agent, fetch_robots_outcome};
use crate::html::{
    MaskedHtml, PageScan, decode_page, effective_base_url, extract_page_data, is_binary_content_type, is_binary_url,
    is_html_content, is_pdf_content, mask_raw_text_markup, robots_meta_contents,
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
/// `page_scan` is the redirect check's read of `resp`'s body, when it made one.
pub(crate) async fn scrape_from_crawl_response(
    url: &str,
    resp: &crate::tower::CrawlResponse,
    page_scan: Option<PageScan>,
    config: &CrawlConfig,
    document_filter: Option<&crate::document::DocumentFilter>,
) -> Result<ScrapeResult, CrawlError> {
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let auth_header_sent = config.auth.is_some();

    // ~keep The agent this response's request actually sent, when the UA rotation layer chose
    // one; without rotation this is exactly `default_robots_user_agent(config)`, so a
    // non-rotating scrape() sees no change (crawlberg#423).
    let sent_user_agent = resp
        .sent_user_agent
        .as_deref()
        .unwrap_or_else(|| default_robots_user_agent(config));
    let robots = resolve_robots_status(url, &parsed_url, config, &client, sent_user_agent).await;
    let response_meta = crate::http::extract_response_meta_from_hashmap(&resp.headers);
    let content_type = resp.content_type.clone();
    let mut decoded = decode_response_body(resp, page_scan, &content_type, &parsed_url, config);

    let (x_robots_tag, header_robots) = header_robots_directives(&resp.headers, sent_user_agent);

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

    let page_scan = decoded.page_scan.take();
    let body = extract_from_body(
        &decoded,
        page_scan,
        &parsed_url,
        config,
        &header_robots,
        sent_user_agent,
    )?;
    let extraction = body.extraction;
    let js_render_hint = body.js_render_hint;
    let downloaded_assets = download_discovered_assets(body.asset_refs, config, &client).await;
    // ~keep A binary or PDF body is not a page to convert, as in the crawl (`engine::page_result`).
    let markdown = if decoded.was_skipped {
        None
    } else {
        crate::markdown::convert_response_to_markdown(
            &decoded.body,
            Some(body.page_scan),
            &parsed_url,
            &merged_content_config(config),
            &content_type,
            downloaded_document.is_some(),
        )
        .await?
    };

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
        ssrf_refused_urls: resp
            .landed
            .as_ref()
            .map(|landed| landed.refused.clone())
            .unwrap_or_default(),
    })
}

/// Everything read out of the response body's single parse.
struct BodyExtraction {
    extraction: crate::html::HtmlExtraction,
    asset_refs: Vec<crate::assets::AssetRef>,
    page_robots: RobotsDirectives,
    js_render_hint: bool,
    /// The read of the page, kept for the markdown's link pre-pass.
    page_scan: PageScan,
}

/// Everything the extraction pipeline reads out of the response body.
///
/// ~keep The document is parsed exactly once here: `extract_page_data`, the meta-robots probes,
/// ~keep asset discovery and the render hint all read the same `VDom`. The `VDom` borrows the
/// ~keep masked source, so both stay local to this function and only owned values cross back out.
fn extract_from_body(
    decoded: &DecodedBody,
    page_scan: Option<PageScan>,
    parsed_url: &Url,
    config: &CrawlConfig,
    header_robots: &RobotsDirectives,
    sent_user_agent: &str,
) -> Result<BodyExtraction, CrawlError> {
    // ~keep Parse the masked source, never `decoded.body`: `tl` reads the contents of
    // ~keep raw-text elements as markup, which both invents tags and hides real ones.
    let parsed_html = match page_scan {
        Some(page_scan) => page_scan.attach(&decoded.body),
        None => mask_raw_text_markup(&decoded.body),
    };
    let doc = crate::html::parse_html(&parsed_html.text)
        .map_err(|e| CrawlError::other(format!("HTML parse error: {e:?}")))?;
    let page_robots = header_robots.with_meta_tags(&doc, sent_user_agent);
    let extraction = extract_page_data(&doc, &parsed_html, parsed_url, decoded.is_html, true);
    let asset_refs = discover_page_assets(&doc, &parsed_html, parsed_url, decoded.is_html, config);
    let word_count = extraction.metadata.word_count.unwrap_or(0);
    let js_render_hint = decoded.is_html && browser_detect::detect_js_render_needed(&doc, word_count);
    // ~keep The tree borrows the masked page, so it goes before the page is detached.
    drop(doc);
    Ok(BodyExtraction {
        extraction,
        asset_refs,
        page_robots,
        js_render_hint,
        page_scan: parsed_html.detach(),
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
    sent_user_agent: &str,
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
    // ~keep Without rotation this is the previous behaviour: the configured agent, or `"*"`
    // when none is set; see `helpers::default_robots_user_agent` for why unifying that `"*"`
    // default is deferred. A configured rotation list changes what actually goes out on the
    // wire per request, though, and robots.txt group selection must match that
    // (crawlberg#423), so it overrides both.
    let ua = if config.user_agents.is_empty() {
        config.user_agent.as_deref().unwrap_or("*")
    } else {
        sent_user_agent
    };
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
    /// The redirect check's read of the response body, dropped when the decode with the page's
    /// character set replaces the body. A body cut to `max_body_size` is read again (see [`PageScan::attach`]).
    page_scan: Option<PageScan>,
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
    mut page_scan: Option<PageScan>,
    content_type: &str,
    parsed_url: &Url,
    config: &CrawlConfig,
) -> DecodedBody {
    let (text, detected_charset) = decode_page(resp.body_text(), content_type, parsed_url.as_str(), &resp.body_bytes);
    let mut body = match text {
        Some(text) => {
            page_scan = None;
            text
        }
        None => resp.body.clone(),
    };
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
        page_scan,
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
    page: &MaskedHtml<'_>,
    parsed_url: &Url,
    is_html: bool,
    config: &CrawlConfig,
) -> Vec<crate::assets::AssetRef> {
    if config.download_assets && is_html {
        assets::discover_assets(doc, &effective_base_url(page.base_href.as_deref(), parsed_url))
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
        crate::tower::CrawlResponse::new(
            200,
            content_type.to_owned(),
            HashMap::new(),
            crate::tower::ResponseBody::Bytes(body.as_bytes().to_vec()),
        )
    }

    #[tokio::test]
    async fn scrape_reads_the_render_hint_from_the_masked_page() {
        // ~keep Between 20 and 50 words, so only the SPA mount decides the hint.
        let prose = "<p>This server-rendered article has real prose in it, enough words that the \
                     sparse-content rule cannot decide the page on its own.</p>";
        let in_script =
            format!(r#"<html><body>{prose}<script>var s = '<div id="root"></div>';</script></body></html>"#);
        let real = format!(r#"<html><body>{prose}<div id="root"></div></body></html>"#);
        let hint = |html: String| async move {
            scrape_from_crawl_response(
                "https://example.com/page",
                &response("text/html", &html),
                None,
                &offline_config(),
                None,
            )
            .await
            .expect("scrape succeeds")
            .js_render_hint
        };
        assert!(hint(real).await, "an empty SPA mount on the page asks for a browser");
        assert!(
            !hint(in_script).await,
            "an SPA mount written inside script text is not an element a browser sees"
        );
    }

    /// A page with 50 or more words of prose needs no browser, even with an empty SPA mount.
    #[tokio::test]
    async fn scrape_gives_no_render_hint_for_a_page_with_enough_words() {
        let prose = "word ".repeat(60);
        let html = format!(r#"<html><body><p>{prose}</p><div id="root"></div></body></html>"#);
        let result = scrape_from_crawl_response(
            "https://example.com/page",
            &response("text/html", &html),
            None,
            &offline_config(),
            None,
        )
        .await
        .expect("scrape succeeds");
        assert!(
            !result.js_render_hint,
            "a page of 60 words is rendered content, whatever its SPA mount holds"
        );
    }

    fn response_with_bytes(content_type: &str, body_bytes: Vec<u8>) -> crate::tower::CrawlResponse {
        crate::tower::CrawlResponse::new(
            200,
            content_type.to_owned(),
            HashMap::new(),
            crate::tower::ResponseBody::Bytes(body_bytes),
        )
    }

    #[tokio::test]
    async fn scrape_reports_allowed_without_network_when_robots_is_not_respected() {
        let resp = response(
            "text/html",
            "<html><head><title>Hi</title></head><body>hello</body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            !result.noindex_detected,
            "the scoped header binds googlebot only, and its scope must not reach the next header"
        );
        assert!(result.nofollow_detected, "the unscoped header binds every crawler");
    }

    /// ~keep A guard, not evidence the fix works: it passes with the pre-fix substring parse too.
    /// It exists to catch the plausible mis-implementation of reading the text before the first
    /// `:` as a crawler name even when it holds a comma, which would discard the whole value's
    /// directives. The next test covers a value-bearing key in first place.
    #[tokio::test]
    async fn scrape_reads_a_directive_beside_a_value_bearing_one() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["nofollow, unavailable_after: 25 Jun 2010 15:00:00 PST".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            result.nofollow_detected,
            "`unavailable_after` names a directive, not a crawler, so the value still binds us"
        );
    }

    #[tokio::test]
    async fn scrape_reads_a_directive_after_a_leading_value_bearing_one() {
        let mut resp = response("text/html", "<html><body>plain</body></html>");
        resp.headers.insert(
            "x-robots-tag".to_owned(),
            vec!["unavailable_after: 25 Jun 2010 15:00:00 PST, nofollow".to_owned()],
        );

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            result.nofollow_detected,
            "a leading `unavailable_after:` is a directive, not a crawler name, so the trailing nofollow binds us"
        );
    }

    #[tokio::test]
    async fn scrape_reads_noindex_from_the_document_when_the_header_is_absent() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name="robots" content="noindex"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "a meta robots noindex must be detected");
        assert!(!result.nofollow_detected);
        assert_eq!(result.x_robots_tag, None);
    }

    #[tokio::test]
    async fn scrape_reads_a_meta_tag_named_for_our_own_user_agent() {
        // ~keep The generic `<meta name="robots">` case above passes the product-token check
        // ~keep unconditionally (`name_lower == ROBOTS_META_NAME`), so it never observes the
        // ~keep user agent `scrape_from_crawl_response` passes to `with_meta_tags`. This test
        // ~keep uses a name scoped to crawlberg's own product token instead.
        let resp = response(
            "text/html",
            r#"<html><head><meta name="crawlberg" content="noindex"></head><body>x</body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            result.noindex_detected,
            "a meta tag naming our own product token must be honoured"
        );
    }

    #[tokio::test]
    async fn scrape_matches_a_meta_tag_against_the_agent_the_request_actually_sent() {
        // ~keep crawlberg#423: the UA rotation layer chose "AgentB" for this request, which the
        // ~keep response reports back on `sent_user_agent`; the configured agent is "AgentA",
        // ~keep which this specific request never sent. A meta tag naming the configured agent
        // ~keep must not bind this page, and one naming the sent agent must.
        let mut config = offline_config();
        config.user_agent = Some("AgentA".to_owned());
        let mut resp = response(
            "text/html",
            r#"<html><head><meta name="AgentA" content="noindex"></head><body>x</body></html>"#,
        );
        resp.sent_user_agent = Some("AgentB".to_owned());

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &config, None)
            .await
            .expect("scrape should succeed");
        assert!(
            !result.noindex_detected,
            "a meta tag naming the configured agent must not bind a request that sent a different one"
        );

        let mut resp = response(
            "text/html",
            r#"<html><head><meta name="AgentB" content="noindex"></head><body>x</body></html>"#,
        );
        resp.sent_user_agent = Some("AgentB".to_owned());
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &config, None)
            .await
            .expect("scrape should succeed");
        assert!(
            result.noindex_detected,
            "a meta tag naming the agent this request actually sent must bind it"
        );
    }

    #[tokio::test]
    async fn scrape_without_rotation_matches_the_configured_agent_unchanged() {
        // ~keep Characterization: `sent_user_agent: None` (no UA rotation layer reached this
        // ~keep response) must fall back to exactly `default_robots_user_agent(config)`, the
        // ~keep pre-existing behaviour, so a non-rotating scrape() sees no change (crawlberg#423).
        let mut config = offline_config();
        config.user_agent = Some("AgentA".to_owned());
        let resp = response(
            "text/html",
            r#"<html><head><meta name="AgentA" content="noindex"></head><body>x</body></html>"#,
        );
        assert_eq!(resp.sent_user_agent, None);

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &config, None)
            .await
            .expect("scrape should succeed");
        assert!(
            result.noindex_detected,
            "without rotation, the configured agent must still bind the page as before"
        );
    }

    /// crawlberg#423: `resolve_robots_status`'s own robots.txt fetch, inside `scrape()`, must
    /// pick the agent that request actually sent when rotation is configured, exactly like
    /// the crawl loop's `RedirectPolicy::admits`. A robots.txt group naming only the sent
    /// agent must block the page.
    #[tokio::test]
    async fn scrape_matches_robots_txt_against_the_agent_actually_sent_when_rotating() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("User-agent: AgentB\nDisallow: /\n")
                    .append_header("content-type", "text/plain"),
            )
            .mount(&mock)
            .await;

        let mut config = CrawlConfig {
            respect_robots_txt: true,
            user_agent: Some("AgentA".to_owned()),
            user_agents: vec!["AgentB".to_owned()],
            ..CrawlConfig::default()
        };
        config.ssrf.deny_private = false;
        let mut resp = response("text/html", "<html><body>x</body></html>");
        resp.sent_user_agent = Some("AgentB".to_owned());

        let url = format!("{}/page", mock.uri());
        let result = scrape_from_crawl_response(&url, &resp, None, &config, None)
            .await
            .expect("scrape should succeed");

        assert!(
            !result.is_allowed,
            "a robots.txt group naming the agent this request actually sent must block it"
        );
    }

    /// Without rotation, `scrape()` must match robots.txt against the configured agent, not
    /// the `*` group: the group naming that agent decides both `is_allowed` and `crawl_delay`.
    #[tokio::test]
    async fn scrape_without_rotation_matches_robots_txt_against_the_configured_agent() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let robots = "User-agent: *\nCrawl-delay: 5\nAllow: /\n\nUser-agent: AgentA\nCrawl-delay: 2\nDisallow: /\n";
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(robots)
                    .append_header("content-type", "text/plain"),
            )
            .mount(&mock)
            .await;

        let mut config = CrawlConfig {
            respect_robots_txt: true,
            user_agent: Some("AgentA".to_owned()),
            ..CrawlConfig::default()
        };
        config.ssrf.deny_private = false;
        let resp = response("text/html", "<html><body>x</body></html>");

        let url = format!("{}/page", mock.uri());
        let result = scrape_from_crawl_response(&url, &resp, None, &config, None)
            .await
            .expect("scrape should succeed");

        assert!(
            !result.is_allowed,
            "the robots.txt group naming the configured agent must block the page"
        );
        assert_eq!(
            result.crawl_delay,
            Some(2),
            "the crawl delay must come from the group naming the configured agent, not `*`"
        );
    }

    #[tokio::test]
    async fn scrape_redecodes_the_body_using_the_detected_charset() {
        // ~keep windows-1252 0xE9 is `é`; read as UTF-8 it is invalid and lossy-decodes to
        // U+FFFD, so a result containing `é` proves the redecode ran.
        let mut body_bytes = b"<html><head><meta charset=\"windows-1252\"></head><body>caf".to_vec();
        body_bytes.push(0xE9);
        body_bytes.extend_from_slice(b"</body></html>");

        let resp = response_with_bytes("text/html", body_bytes);
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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

        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &config, None)
            .await
            .expect("scrape should succeed");

        assert_eq!(result.body_size, 64, "body_size must reflect the truncated body");
        assert_eq!(result.html.len(), 64);
    }

    #[tokio::test]
    async fn scrape_folds_remove_tags_into_the_markdown_exclude_selectors() {
        let html = "<html><body><p>keep this</p><aside>drop this</aside></body></html>";
        let resp = response("text/html", html);

        let baseline = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
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
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &config, None)
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
        let result = scrape_from_crawl_response("https://example.com/doc.pdf", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.is_pdf, "a PDF content type must be recognised");
        assert!(result.was_skipped, "a PDF must be flagged as skipped for extraction");
        assert!(result.markdown.is_none(), "a skipped page is not converted");
    }

    #[tokio::test]
    async fn scrape_of_a_page_the_converter_refuses_is_an_error() {
        let resp = response("text/html", "PK\u{3}\u{4}<p>not a page</p>");
        let error = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect_err("a page that cannot be converted is not a result");

        let message = error.to_string();
        assert!(
            message.contains("could not convert https://example.com/page to Markdown"),
            "the error names the page, got: {message}"
        );
    }

    fn urls<T>(items: &[T], url: impl Fn(&T) -> &str) -> Vec<String> {
        items.iter().map(|item| url(item).to_owned()).collect()
    }

    #[tokio::test]
    async fn scrape_decodes_character_references_in_addresses() {
        let resp = response(
            "text/html",
            r#"<html><body><a href="list?a=1&amp;b=2">q</a> <a href="&#47;root.html">r</a>
            <img src="i.png?a=1&amp;b=2" alt="Tom &amp; Jerry"></body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/dir/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(
            urls(&result.links, |l| &l.url),
            ["https://example.com/dir/list?a=1&b=2", "https://example.com/root.html"]
        );
        assert_eq!(
            urls(&result.images, |i| &i.url),
            ["https://example.com/dir/i.png?a=1&b=2"]
        );
        assert_eq!(result.images[0].alt.as_deref(), Some("Tom & Jerry"));
    }

    #[tokio::test]
    async fn scrape_reads_markup_written_in_uppercase() {
        let resp = response(
            "text/html",
            r#"<HTML LANG="en"><HEAD><TITLE>Upper</TITLE><META NAME="robots" CONTENT="noindex">
            <LINK REL="canonical" HREF="/canon"></HEAD>
            <BODY><A HREF="up.html">x</A><IMG SRC="u.png"></BODY></HTML>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/dir/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.links, |l| &l.url), ["https://example.com/dir/up.html"]);
        assert_eq!(urls(&result.images, |i| &i.url), ["https://example.com/dir/u.png"]);
        assert_eq!(result.metadata.title.as_deref(), Some("Upper"));
        assert_eq!(result.metadata.html_lang.as_deref(), Some("en"));
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.com/canon")
        );
        assert!(result.noindex_detected, "an uppercase robots meta tag must be read");
    }

    #[tokio::test]
    async fn scrape_resolves_images_against_the_base_href() {
        let resp = response(
            "text/html",
            r#"<html><head><base href="/assets/"><meta property="og:image" content="og.png"></head>
            <body><a href="leaf.html">l</a><img src="logo.png">
            <picture><source srcset="wide.png 2x"></picture></body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/dir/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(
            urls(&result.links, |l| &l.url),
            ["https://example.com/assets/leaf.html"]
        );
        assert_eq!(
            urls(&result.images, |i| &i.url),
            [
                "https://example.com/assets/logo.png",
                "https://example.com/assets/wide.png",
                "https://example.com/assets/og.png"
            ]
        );
    }

    #[tokio::test]
    async fn scrape_matches_attribute_values_in_any_case() {
        let resp = response(
            "text/html",
            r#"<html><head><META NAME="ROBOTS" CONTENT="noindex, nofollow">
            <link rel="Canonical" href="https://example.com/canon">
            <link rel="Alternate" type="application/RSS+xml" href="https://example.com/feed.xml">
            <link rel="ALTERNATE" hreflang="de" href="https://example.com/de/">
            <link rel="Shortcut Icon" href="https://example.com/a.ico"><link rel="ICON" href="https://example.com/b.ico">
            <meta property="OG:IMAGE" content="https://example.com/og.png">
            <meta name="Twitter:Image" content="https://example.com/tw.png">
            <script type="application/LD+JSON">{"@type":"Thing","name":"t"}</script></head>
            <body><a href="https://other.example/" rel="External NoFollow">x</a></body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/dir/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(result.noindex_detected, "an uppercase robots name must be read");
        assert!(result.nofollow_detected, "an uppercase robots name must be read");
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.com/canon")
        );
        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/feed.xml"]);
        let hreflangs = result.metadata.hreflangs.as_deref().unwrap_or_default();
        assert_eq!(urls(hreflangs, |h| &h.url), ["https://example.com/de/"]);
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(
            urls(favicons, |f| &f.url),
            ["https://example.com/a.ico", "https://example.com/b.ico"]
        );
        assert_eq!(
            urls(&result.images, |i| &i.url),
            ["https://example.com/og.png", "https://example.com/tw.png"]
        );
        assert_eq!(result.json_ld.len(), 1, "got {:?}", result.json_ld);
        assert!(result.links[0].nofollow, "rel is a token list compared in any case");
    }

    #[tokio::test]
    async fn scrape_reads_nofollow_from_a_comma_separated_rel() {
        let resp = response(
            "text/html",
            r#"<html><body><a href="/a" rel="ugc,nofollow">a</a><a href="/b" rel="nofollow,ugc">b</a>
            <a href="/c" rel="UGC , NoFollow">c</a><a href="/d" rel="ugc,sponsored">d</a></body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        let nofollow: Vec<(&str, bool)> = result.links.iter().map(|l| (l.text.as_str(), l.nofollow)).collect();
        assert_eq!(
            nofollow,
            [("a", true), ("b", true), ("c", true), ("d", false)],
            "a comma separates the link qualifiers"
        );
    }

    #[tokio::test]
    async fn scrape_resolves_head_links_against_the_base_href() {
        let resp = response(
            "text/html",
            r#"<html><head><base href="/other/">
            <link rel="alternate" type="application/rss+xml" href="feed.xml">
            <link rel="icon" href="fav.ico"><link rel="canonical" href="c.html"></head></html>"#,
        );
        let result = scrape_from_crawl_response(
            "https://example.com/dir/page.html",
            &resp,
            None,
            &offline_config(),
            None,
        )
        .await
        .expect("scrape should succeed");

        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/other/feed.xml"]);
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(urls(favicons, |f| &f.url), ["https://example.com/other/fav.ico"]);
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.com/other/c.html")
        );
    }

    #[tokio::test]
    async fn scrape_uses_the_page_url_as_the_base_for_a_data_or_javascript_base_href() {
        for (base, dir) in [
            (" DATA:text/html,x ", "https://example.com/dir/"),
            (" JavaScript:alert(1)// ", "https://example.com/dir/"),
            ("JAVASCRIPT://example.org/", "https://example.com/dir/"),
            ("/other/", "https://example.com/other/"),
            ("https://cdn.example/", "https://cdn.example/"),
        ] {
            let resp = response(
                "text/html",
                &format!(
                    r#"<html><head><base href="{base}">
                    <link rel="alternate" type="application/rss+xml" href="feed.xml">
                    <link rel="icon" href="fav.ico"><link rel="canonical" href="c.html"></head>
                    <body><p><a href="leaf.html">leaf</a><img src="logo.png"></p></body></html>"#
                ),
            );
            let result = scrape_from_crawl_response(
                "https://example.com/dir/page.html",
                &resp,
                None,
                &offline_config(),
                None,
            )
            .await
            .expect("scrape should succeed");

            assert_eq!(
                urls(&result.links, |l| &l.url),
                [format!("{dir}leaf.html")],
                "for {base:?}"
            );
            assert_eq!(
                urls(&result.images, |i| &i.url),
                [format!("{dir}logo.png")],
                "for {base:?}"
            );
            assert_eq!(
                urls(&result.feeds, |f| &f.url),
                [format!("{dir}feed.xml")],
                "for {base:?}"
            );
            let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
            assert_eq!(urls(favicons, |f| &f.url), [format!("{dir}fav.ico")], "for {base:?}");
            assert_eq!(
                result.metadata.canonical_url,
                Some(format!("{dir}c.html")),
                "for {base:?}"
            );
            let markdown = result.markdown.expect("markdown").content;
            assert!(
                markdown.contains(&format!("[leaf]({dir}leaf.html)")),
                "for {base:?}, got: {markdown}"
            );
        }
    }

    #[tokio::test]
    async fn scrape_trims_attribute_values_and_reads_a_type_by_its_mime_essence() {
        let resp = response(
            "text/html",
            r#"<html><head><meta name=" robots " content="noindex">
            <meta name=" Description " content="d">
            <link rel="alternate" type=" application/rss+xml; charset=utf-8 " href="/feed.xml">
            <script type="application/LD+JSON; charset=utf-8">{"@type":"Thing","name":"t"}</script>
            </head></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert!(
            result.noindex_detected,
            "a robots name with spaces around it must be read"
        );
        assert_eq!(result.metadata.robots.as_deref(), Some("noindex"));
        assert_eq!(result.metadata.description.as_deref(), Some("d"));
        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/feed.xml"]);
        assert_eq!(result.json_ld.len(), 1, "got {:?}", result.json_ld);
    }

    #[tokio::test]
    async fn scrape_reports_no_canonical_url_for_a_blank_href() {
        for head in [
            r#"<link rel="canonical" href="">"#,
            "<link rel=\"canonical\" href=\" \t\n\">",
        ] {
            let resp = response("text/html", &format!("<html><head>{head}</head></html>"));
            let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
                .await
                .expect("scrape should succeed");
            assert_eq!(result.metadata.canonical_url, None, "for {head}");
        }
    }

    #[tokio::test]
    async fn scrape_resolves_hreflang_addresses_against_the_base_href() {
        let resp = response(
            "text/html",
            r#"<html><head><base href="/other/">
            <link rel="alternate" hreflang="de" href="de.html">
            <link rel="alternate" hreflang="fr" href="https://example.org/fr/">
            <link rel="alternate" hreflang="es" href=" ">
            <link rel="alternate" hreflang=" en-GB " href="en.html">
            <link rel="alternate" hreflang=" " href="blank.html"></head></html>"#,
        );
        let result = scrape_from_crawl_response(
            "https://example.com/dir/page.html",
            &resp,
            None,
            &offline_config(),
            None,
        )
        .await
        .expect("scrape should succeed");

        let hreflangs = result.metadata.hreflangs.as_deref().unwrap_or_default();
        assert_eq!(
            urls(hreflangs, |h| &h.url),
            [
                "https://example.com/other/de.html",
                "https://example.org/fr/",
                "https://example.com/other/en.html"
            ]
        );
        assert_eq!(urls(hreflangs, |h| &h.lang), ["de", "fr", "en-GB"]);
    }

    async fn scrape_head(head: &str) -> ScrapeResult {
        let resp = response("text/html", &format!("<html><head>{head}</head><body></body></html>"));
        scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed")
    }

    #[tokio::test]
    async fn scrape_skips_feeds_with_an_unfetchable_address() {
        let result = scrape_head(
            "<link rel=\"alternate\" type=\"application/rss+xml\" href=\"JavaScript:alert(1)\">\
             <link rel=\"alternate\" type=\"application/atom+xml\" href=\"VBScript:msgbox(1)\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"java&#9;script:x\">\
             <link rel=\"alternate\" type=\"application/feed+json\" href=\"DATA:application/json,{}\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"file:///etc/passwd\">\
             <link rel=\"alternate\" type=\"application/atom+xml\" href=\"blob:https://example.com/x\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"feed.xml\">",
        )
        .await;
        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/feed.xml"]);
    }

    #[tokio::test]
    async fn scrape_skips_hreflangs_with_an_unfetchable_address() {
        let result = scrape_head(
            "<link rel=\"alternate\" hreflang=\"de\" href=\"javascript:alert(1)\">\
             <link rel=\"alternate\" hreflang=\"fr\" href=\"VBSCRIPT:x\">\
             <link rel=\"alternate\" hreflang=\"es\" href=\"data:text/html,x\">\
             <link rel=\"alternate\" hreflang=\"pt\" href=\"file:///etc/passwd\">\
             <link rel=\"alternate\" hreflang=\"it\" href=\"blob:https://example.com/x\">\
             <link rel=\"alternate\" hreflang=\"en\" href=\"en.html\">",
        )
        .await;
        let hreflangs = result.metadata.hreflangs.as_deref().unwrap_or_default();
        assert_eq!(urls(hreflangs, |h| &h.url), ["https://example.com/en.html"]);
    }

    #[tokio::test]
    async fn scrape_skips_script_favicons_and_keeps_a_data_favicon() {
        let result = scrape_head(
            "<link rel=\"icon\" href=\"javascript:alert(1)\">\
             <link rel=\"shortcut icon\" href=\"VBScript:msgbox(1)\">\
             <link rel=\"apple-touch-icon\" href=\"JAVASCRIPT:x\">\
             <link rel=\"icon\" href=\"data:image/png;base64,iVBORw0KGgo=\">\
             <link rel=\"icon\" href=\"file:///etc/passwd\">\
             <link rel=\"icon\" href=\"blob:https://example.com/x\">\
             <link rel=\"icon\" href=\"fav.ico\">",
        )
        .await;
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(
            urls(favicons, |f| &f.url),
            ["data:image/png;base64,iVBORw0KGgo=", "https://example.com/fav.ico"],
            "a data: icon is a real, usable icon and stays; a file: or blob: icon names \
             something the crawler can never fetch and is dropped"
        );
    }

    #[tokio::test]
    async fn scrape_reports_no_canonical_url_for_an_unfetchable_address() {
        for href in [
            "javascript:alert(1)",
            "VBScript:msgbox(1)",
            "Data:text/html,x",
            "file:///etc/passwd",
            "blob:https://example.com/x",
        ] {
            let result = scrape_head(&format!("<link rel=\"canonical\" href=\"{href}\">")).await;
            assert_eq!(result.metadata.canonical_url, None, "for {href}");
        }
        let result = scrape_head("<link rel=\"canonical\" href=\"c.html\">").await;
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.com/c.html")
        );
    }

    #[tokio::test]
    async fn scrape_takes_the_canonical_url_from_the_first_canonical_link_only() {
        for href in ["javascript:alert(1)", "VBScript:msgbox(1)", "Data:text/html,x"] {
            let result = scrape_head(&format!(
                "<link rel=\"canonical\" href=\"{href}\"><link rel=\"canonical\" href=\"c.html\">"
            ))
            .await;
            assert_eq!(result.metadata.canonical_url, None, "for {href} first");
            let result = scrape_head(&format!(
                "<link rel=\"canonical\" href=\"c.html\"><link rel=\"canonical\" href=\"{href}\">"
            ))
            .await;
            assert_eq!(
                result.metadata.canonical_url.as_deref(),
                Some("https://example.com/c.html"),
                "for {href} second"
            );
        }
    }

    #[tokio::test]
    async fn scrape_resolves_relative_head_links_to_the_page_under_a_script_base() {
        let result = scrape_head(
            "<base href=\"javascript:alert(1)//\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"#feed\">\
             <link rel=\"alternate\" hreflang=\"de\" href=\"#de\">\
             <link rel=\"icon\" href=\"#icon\">\
             <link rel=\"canonical\" href=\"#top\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"https://example.com/feed.xml\">\
             <link rel=\"alternate\" hreflang=\"en\" href=\"https://example.com/en/\">\
             <link rel=\"icon\" href=\"https://example.com/fav.ico\">\
             <link rel=\"alternate\" type=\"application/atom+xml\" href=\"javascript:alert(2)\">\
             <link rel=\"alternate\" hreflang=\"fr\" href=\"data:text/html,x\">\
             <link rel=\"icon\" href=\"VBScript:msgbox(1)\">",
        )
        .await;
        // A script base is ignored (the HTML frozen base URL steps), so a relative address
        // resolves against the page; an absolute script or data address still names itself
        // and is still dropped, even under the same base.
        assert_eq!(
            urls(&result.feeds, |f| &f.url),
            ["https://example.com/page#feed", "https://example.com/feed.xml"]
        );
        let hreflangs = result.metadata.hreflangs.as_deref().unwrap_or_default();
        assert_eq!(
            urls(hreflangs, |h| &h.url),
            ["https://example.com/page#de", "https://example.com/en/"]
        );
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(
            urls(favicons, |f| &f.url),
            ["https://example.com/page#icon", "https://example.com/fav.ico"]
        );
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.com/page#top")
        );
    }

    #[tokio::test]
    async fn scrape_resolves_relative_images_to_the_page_under_a_script_or_data_base() {
        // A script or data base is ignored (the HTML frozen base URL steps), so a relative
        // image address resolves against the page and is kept. `vbscript:` is not one of the
        // frozen-base schemes, so its base still stands and a relative address under it still
        // names an absolute script address and is still dropped, same as before #450. The last
        // case is a literal absolute script address: it names itself under any base and stays
        // dropped.
        for (base, image, resolved) in [
            ("javascript:alert(1)//", "#x", Some("https://example.com/page#x")),
            ("JavaScript://host/", "x.png", Some("https://example.com/x.png")),
            ("vbscript://host/", "x.png", None),
            ("data:text/html,x", "#x", Some("https://example.com/page#x")),
            ("javascript:alert(1)//", "javascript:evil()", None),
        ] {
            let resp = response(
                "text/html",
                &format!(
                    "<html><head><base href=\"{base}\">\
                     <meta property=\"og:image\" content=\"{image}\">\
                     <meta name=\"twitter:image\" content=\"{image}\">\
                     <meta property=\"og:image\" content=\"https://example.com/og.png\">\
                     <meta name=\"twitter:image\" content=\"https://example.com/tw.png\"></head><body>\
                     <img src=\"{image}\"><img src=\"https://example.com/i.png\">\
                     <picture><source srcset=\"{image} 1x\"></picture>\
                     <picture><source srcset=\"https://example.com/s.png 1x\"></picture></body></html>"
                ),
            );
            let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
                .await
                .expect("scrape should succeed");
            let expected: Vec<&str> = match resolved {
                Some(resolved) => vec![
                    resolved,
                    "https://example.com/i.png",
                    resolved,
                    "https://example.com/s.png",
                    resolved,
                    "https://example.com/og.png",
                    resolved,
                    "https://example.com/tw.png",
                ],
                None => vec![
                    "https://example.com/i.png",
                    "https://example.com/s.png",
                    "https://example.com/og.png",
                    "https://example.com/tw.png",
                ],
            };
            assert_eq!(
                urls(&result.images, |i| &i.url),
                expected,
                "for {image:?} against {base:?}"
            );
        }
    }

    #[tokio::test]
    async fn scrape_normalizes_newlines_and_nul_in_attribute_values() {
        let resp = response(
            "text/html",
            "<html><body><a href=\"a.html\" rel=\"nofollow\r\nexternal\">a</a>\
             <img src=\"i.png\" alt=\"one\rtwo\0three\"><img src=\"j.png\" alt=\"a\0b\"></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(result.links[0].rel.as_deref(), Some("nofollow\nexternal"));
        assert_eq!(result.images[0].alt.as_deref(), Some("one\ntwo\u{FFFD}three"));
        assert_eq!(result.images[1].alt.as_deref(), Some("a\u{FFFD}b"));
    }

    #[tokio::test]
    async fn scrape_skips_feed_and_icon_links_with_a_blank_href() {
        let resp = response(
            "text/html",
            "<html><head>\
             <link rel=\"alternate\" type=\"application/rss+xml\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"\">\
             <link rel=\"alternate\" type=\"application/atom+xml\" href=\" \t\n\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"feed.xml\">\
             <link rel=\"icon\" href=\" \">\
             <link rel=\"icon\" href=\"fav.ico\"></head></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/feed.xml"]);
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(urls(favicons, |f| &f.url), ["https://example.com/fav.ico"]);
    }

    #[tokio::test]
    async fn scrape_reads_a_type_with_form_feeds_around_it() {
        let resp = response(
            "text/html",
            "<html><head>\
             <link rel=\"alternate\" type=\"\x0Capplication/atom+xml\x0C\" href=\"/atom.xml\">\
             <script type=\"\x0Capplication/ld+json\x0C\">{\"@type\":\"Thing\",\"name\":\"t\"}</script>\
             </head></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/atom.xml"]);
        assert!(
            matches!(result.feeds[0].feed_type, crate::types::FeedType::Atom),
            "got {:?}",
            result.feeds
        );
        assert_eq!(result.json_ld.len(), 1, "got {:?}", result.json_ld);
    }

    #[tokio::test]
    async fn scrape_decodes_character_references_in_image_and_icon_addresses() {
        let resp = response(
            "text/html",
            r#"<html><head>
            <link rel="icon" href="&#32;&#32;"><link rel="icon" href="&#102;av.ico">
            <meta property="og:image" content="&#32;"><meta property="og:image" content="og&#46;png">
            <meta name="twitter:image" content="&#x74;w.png"></head><body>
            <img src="&#32;&#9;"><img src="i&amp;j.png">
            <picture><source srcset="&#32;&#32;"></picture>
            <picture><source srcset="s&#46;png 2x"></picture></body></html>"#,
        );
        let result = scrape_from_crawl_response("https://example.com/dir/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(urls(favicons, |f| &f.url), ["https://example.com/dir/fav.ico"]);
        assert_eq!(
            urls(&result.images, |i| &i.url),
            [
                "https://example.com/dir/i&j.png",
                "https://example.com/dir/s.png",
                "https://example.com/dir/og.png",
                "https://example.com/dir/tw.png"
            ]
        );
    }

    #[tokio::test]
    async fn scrape_skips_blank_or_inline_image_sources_and_splits_srcset_as_a_browser_does() {
        let resp = response(
            "text/html",
            "<html><head>\
             <meta property=\"og:image\" content=\"  \">\
             <meta name=\"twitter:image\" content=\"\t\u{1}\"></head><body>\
             <img src=\" \"><img src=\"\u{B}\"><img src=\"i.png\">\
             <picture><source srcset=\"a\u{A0}b.png 2x, c.png 1x\"></picture>\
             <picture><source srcset=\"\u{1} 1x, d.png 2x\"></picture>\
             <picture><source srcset=\"data:image/png;base64,AA 1x\"></picture></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(
            urls(&result.images, |i| &i.url),
            ["https://example.com/i.png", "https://example.com/a%C2%A0b.png"]
        );
    }

    #[tokio::test]
    async fn scrape_skips_inline_data_images_in_any_case() {
        let resp = response(
            "text/html",
            "<html><body>\
             <img src=\"DATA:image/png;base64,AA\"><img src=\"i.png\">\
             <picture><source srcset=\"Data:image/png;base64,AA 1x\"></picture></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.images, |i| &i.url), ["https://example.com/i.png"]);
    }

    #[tokio::test]
    async fn scrape_skips_vbscript_links_in_any_case() {
        let resp = response(
            "text/html",
            "<html><body>\
             <a href=\"vbscript:msgbox(1)\">a</a><a href=\"VBScript:msgbox(1)\">b</a>\
             <a href=\"next.html\">c</a></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.links, |l| &l.url), ["https://example.com/next.html"]);
    }

    #[tokio::test]
    async fn scrape_skips_script_image_sources_in_any_case() {
        let resp = response(
            "text/html",
            "<html><head>\
             <meta property=\"og:image\" content=\"JavaScript:alert(1)\">\
             <meta name=\"twitter:image\" content=\"vbscript:x\"></head><body>\
             <img src=\"javascript:alert(1)\"><img src=\"VBScript:msgbox(1)\"><img src=\"i.png\">\
             <picture><source srcset=\"JAVASCRIPT:alert(1) 1x\"></picture></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.images, |i| &i.url), ["https://example.com/i.png"]);
    }

    #[tokio::test]
    async fn scrape_skips_inline_data_meta_images_in_any_case() {
        let resp = response(
            "text/html",
            "<html><head>\
             <meta property=\"og:image\" content=\"DATA:image/png;base64,AA\">\
             <meta name=\"twitter:image\" content=\"data:image/png;base64,AA\">\
             <meta property=\"og:image\" content=\"og.png\"></head><body></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.images, |i| &i.url), ["https://example.com/og.png"]);
    }

    #[tokio::test]
    async fn scrape_skips_a_data_image_address_the_url_parser_cannot_read_at_every_image_site() {
        let resp = response(
            "text/html",
            "<html><head><meta property=\"og:image\" content=\"DATA://h:99999\">\
             <meta name=\"twitter:image\" content=\"data://[c\"></head><body>\
             <img src=\"data://[a\"><picture><source srcset=\"data://[b 1x\"></picture>\
             <img src=\"ok.png\"></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(urls(&result.images, |i| &i.url), ["https://example.com/ok.png"]);
    }

    #[tokio::test]
    async fn scrape_treats_an_address_of_only_c0_controls_as_blank() {
        let resp = response(
            "text/html",
            "<html><head>\
             <link rel=\"canonical\" href=\"\u{B}\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"\u{1}\u{B}\u{1F}\">\
             <link rel=\"alternate\" type=\"application/rss+xml\" href=\"\u{1}feed.xml\u{1F}\">\
             <link rel=\"icon\" href=\"\u{1C}\"><link rel=\"icon\" href=\"\u{1C}f.ico\"></head><body>\
             <a href=\"\u{1}\">a</a><a href=\"java\tscript:alert(1)\">j</a>\
             <a href=\"\u{B}next.html\">b</a></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/page", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(result.metadata.canonical_url, None);
        assert_eq!(urls(&result.feeds, |f| &f.url), ["https://example.com/feed.xml"]);
        let favicons = result.metadata.favicons.as_deref().unwrap_or_default();
        assert_eq!(urls(favicons, |f| &f.url), ["https://example.com/f.ico"]);
        assert_eq!(urls(&result.links, |l| &l.url), ["https://example.com/next.html"]);
    }

    #[tokio::test]
    async fn scrape_keeps_unicode_spaces_at_the_ends_of_a_link_address() {
        let resp = response(
            "text/html",
            "<html><body>\
             <a href=\" \t\u{A0}nbsp.html\u{3000}\n\">a</a>\
             <a href=\"\u{2003}em.html\u{85}\">b</a>\
             <a href=\"\u{A0}\">c</a>\
             <a href=\" \t\">d</a></body></html>",
        );
        let result = scrape_from_crawl_response("https://example.com/", &resp, None, &offline_config(), None)
            .await
            .expect("scrape should succeed");

        assert_eq!(
            urls(&result.links, |l| &l.url),
            [
                "https://example.com/%C2%A0nbsp.html%E3%80%80",
                "https://example.com/%E2%80%83em.html%C2%85",
                "https://example.com/%C2%A0",
            ]
        );
    }

    #[tokio::test]
    async fn scrape_rejects_an_unparseable_url() {
        let resp = response("text/html", "<html></html>");
        let error = scrape_from_crawl_response("not a url", &resp, None, &offline_config(), None)
            .await
            .expect_err("an unparseable URL must be rejected");

        assert!(
            error.to_string().contains("invalid URL"),
            "expected an invalid-URL error, got: {error}"
        );
    }

    type MetaAddressGetter = fn(&ScrapeResult) -> Option<&str>;

    const META_ADDRESS_FIELDS: [(&str, &str, MetaAddressGetter); 5] = [
        ("property", "og:url", |r| r.metadata.og_url.as_deref()),
        ("property", "og:video", |r| r.metadata.og_video.as_deref()),
        ("property", "og:audio", |r| r.metadata.og_audio.as_deref()),
        ("property", "og:image", |r| r.metadata.og_image.as_deref()),
        ("name", "twitter:image", |r| r.metadata.twitter_image.as_deref()),
    ];

    #[tokio::test]
    async fn scrape_skips_og_and_twitter_addresses_the_crawler_cannot_fetch() {
        for (attr, name, get) in META_ADDRESS_FIELDS {
            for address in [
                "javascript:alert(1)",
                "JavaScript:alert(1)",
                "VBScript:msgbox(1)",
                "Data:text/html,x",
                "DATA:text/html,x",
                "file:///etc/passwd",
                "FILE:///etc/passwd",
                "blob:https://example.com/x",
                "ftp://example.com/x",
                "mailto:a@example.com",
                "tel:+15550100",
                "file://[bad/x",
                "http://[bad/x",
            ] {
                let result = scrape_head(&format!(r#"<meta {attr}="{name}" content="{address}">"#)).await;
                assert_eq!(get(&result), None, "for {name} with {address}");
            }
            let result = scrape_head(&format!(r#"<meta {attr}="{name}" content="x.png">"#)).await;
            assert_eq!(
                get(&result),
                Some("https://example.com/x.png"),
                "for {name} against the page base"
            );
        }
    }

    #[tokio::test]
    async fn scrape_treats_a_whitespace_only_og_or_twitter_address_as_absent() {
        let result = scrape_head(
            "<meta property=\"og:url\" content=\"  \">\
             <meta property=\"og:video\" content=\"\t\n\">\
             <meta property=\"og:audio\" content=\" \">\
             <meta property=\"og:image\" content=\" \">\
             <meta name=\"twitter:image\" content=\" \">",
        )
        .await;
        for (_, name, get) in META_ADDRESS_FIELDS {
            assert_eq!(get(&result), None, "for {name}");
        }
    }

    #[tokio::test]
    async fn scrape_keeps_a_valid_og_or_twitter_address_when_another_tag_cannot_be_fetched() {
        let result = scrape_head(
            "<meta property=\"og:url\" content=\"javascript:alert(1)\">\
             <meta property=\"og:url\" content=\"page.html\">\
             <meta property=\"og:video\" content=\"video.mp4\">\
             <meta property=\"og:video\" content=\"file:///etc/passwd\">",
        )
        .await;
        assert_eq!(
            result.metadata.og_url.as_deref(),
            Some("https://example.com/page.html"),
            "an address the crawler cannot fetch does not clear a valid one seen before or after it"
        );
        assert_eq!(
            result.metadata.og_video.as_deref(),
            Some("https://example.com/video.mp4")
        );
    }

    #[tokio::test]
    async fn scrape_resolves_og_and_twitter_addresses_against_the_base_href() {
        let result = scrape_head(
            "<base href=\"/dir/\">\
             <meta property=\"og:url\" content=\"x.png\">\
             <meta property=\"og:video\" content=\"x.png\">\
             <meta property=\"og:audio\" content=\"x.png\">\
             <meta property=\"og:image\" content=\"x.png\">\
             <meta name=\"twitter:image\" content=\"x.png\">",
        )
        .await;
        for (_, name, get) in META_ADDRESS_FIELDS {
            assert_eq!(get(&result), Some("https://example.com/dir/x.png"), "for {name}");
        }
    }

    #[tokio::test]
    async fn scrape_skips_a_relative_og_or_twitter_address_under_a_base_that_cannot_take_one() {
        let result = scrape_head(
            "<base href=\"blob:https://example.com/b\">\
             <meta property=\"og:url\" content=\"x.png\">\
             <meta property=\"og:video\" content=\"x.png\">\
             <meta property=\"og:audio\" content=\"x.png\">\
             <meta property=\"og:image\" content=\"x.png\">\
             <meta name=\"twitter:image\" content=\"x.png\">",
        )
        .await;
        for (_, name, get) in META_ADDRESS_FIELDS {
            assert_eq!(get(&result), None, "for {name}");
        }
    }
}
