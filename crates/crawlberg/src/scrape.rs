//! Single-page scrape operation.

use tl::ParserOptions;
use url::Url;

use crate::assets;
use crate::browser_detect;
use crate::error::CrawlError;
use crate::html::{
    detect_charset, detect_nofollow, detect_noindex, extract_page_data, is_binary_content_type, is_binary_url,
    is_html_content, is_pdf_content,
};
use crate::http::{build_client, http_fetch};
use crate::normalize::robots_url;
use crate::robots::{is_path_allowed, parse_robots_txt};
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
    let parsed_url = Url::parse(url).map_err(|e| CrawlError::Other(format!("invalid URL: {e}")))?;
    let client = build_client(config)?;
    let auth_header_sent = config.auth.is_some();

    let mut is_allowed = true;
    let mut crawl_delay = None;
    if config.respect_robots_txt {
        let robots = robots_url(&parsed_url);
        if let Ok(robots_resp) = http_fetch(&robots, config, &std::collections::HashMap::new(), &client).await {
            let ua = config.user_agent.as_deref().unwrap_or("*");
            let rules = parse_robots_txt(&robots_resp.body, ua);
            is_allowed = is_path_allowed(parsed_url.path(), &rules);
            crawl_delay = rules.crawl_delay;
        }
    }

    let response_meta = crate::http::extract_response_meta_from_hashmap(&resp.headers);

    let status_code = resp.status;
    let content_type = resp.content_type.clone();
    let mut body = resp.body.clone();

    let detected_charset = detect_charset(&content_type, &resp.body_bytes);

    if let Some(ref charset) = detected_charset
        && let Some(decoded) = crate::http::redecode_with_charset(charset, &resp.body_bytes)
    {
        body = decoded;
    }
    let is_pdf = is_pdf_content(&content_type, &body);

    let mut body_size = body.len();
    if let Some(max_size) = config.max_body_size {
        crate::http::truncate_body_at_char_boundary(&mut body, max_size);
        body_size = body.len();
    }

    let x_robots_tag = resp.headers.get("x-robots-tag").and_then(|v| v.first().cloned());

    let mut noindex_detected = false;
    let mut nofollow_detected = false;

    if let Some(ref xrt) = x_robots_tag {
        let lower = xrt.to_lowercase();
        if lower.contains("noindex") {
            noindex_detected = true;
        }
        if lower.contains("nofollow") {
            nofollow_detected = true;
        }
    }

    let was_skipped = is_binary_content_type(&content_type) || is_binary_url(parsed_url.as_str()) || is_pdf;

    let downloaded_document = crate::document::build_downloaded_document(
        url,
        &parsed_url,
        &content_type,
        &resp.body_bytes,
        was_skipped,
        config,
    )
    .await;

    let is_html = is_html_content(&content_type, &body);

    let (extraction, asset_refs) = {
        let doc = tl::parse(&body, ParserOptions::default())
            .map_err(|e| CrawlError::Other(format!("HTML parse error: {e:?}")))?;

        if !noindex_detected {
            noindex_detected = detect_noindex(&doc);
        }
        if !nofollow_detected {
            nofollow_detected = detect_nofollow(&doc);
        }

        let extraction = extract_page_data(&doc, &body, &parsed_url, is_html, true);

        let asset_refs = if config.download_assets && is_html {
            assets::discover_assets(&doc, &parsed_url)
        } else {
            Vec::new()
        };

        (extraction, asset_refs)
    };

    let word_count = extraction.metadata.word_count.unwrap_or(0);
    let js_render_hint = is_html && browser_detect::detect_js_render_needed(&body, word_count);

    let downloaded_assets = if !asset_refs.is_empty() {
        assets::download_assets(asset_refs, config, &client).await
    } else {
        Vec::new()
    };

    let content_config = if config.remove_tags.is_empty() {
        std::borrow::Cow::Borrowed(&config.content)
    } else {
        let mut merged = config.content.clone();
        merged.exclude_selectors.extend(config.remove_tags.iter().cloned());
        std::borrow::Cow::Owned(merged)
    };
    let markdown = crate::markdown::convert_to_markdown(&body, &content_config).await;

    Ok(ScrapeResult {
        status_code,
        final_url: url.to_owned(),
        content_type,
        html: body,
        body_size,
        metadata: extraction.metadata,
        links: extraction.links,
        images: extraction.images,
        feeds: extraction.feeds,
        json_ld: extraction.json_ld,
        is_allowed,
        crawl_delay,
        noindex_detected,
        nofollow_detected,
        x_robots_tag,
        is_pdf,
        was_skipped,
        detected_charset,
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
