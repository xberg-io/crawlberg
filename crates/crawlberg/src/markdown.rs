//! HTML-to-Markdown conversion -- always active.

use html_to_markdown_rs::error::ConversionError;
use url::Url;

use crate::error::CrawlError;
use crate::html::PageScan;
use crate::types::{ContentConfig, MarkdownResult};

fn sanitize_document_structure(document: &mut html_to_markdown_rs::types::DocumentStructure, base_url: &Url) {
    use html_to_markdown_rs::types::{AnnotationKind, NodeContent};

    for node in &mut document.nodes {
        for annotation in &mut node.annotations {
            if let AnnotationKind::Link { url, .. } = &mut annotation.kind
                && !url.is_empty()
                && let Some(resolved) = crate::net::userinfo::resolve(base_url, url)
            {
                *url = resolved.into();
            }
        }
        if let NodeContent::Image { src: Some(src), .. } = &mut node.content
            && !src.is_empty()
            && let Some(resolved) = crate::net::userinfo::resolve(base_url, src)
        {
            *src = resolved.into();
        }
    }
}

/// Perform the actual HTML-to-Markdown conversion (synchronous).
fn convert_html_to_markdown(
    html: &str,
    page_scan: Option<PageScan>,
    document_url: &Url,
    config: &ContentConfig,
) -> Result<MarkdownResult, ConversionError> {
    let (html, effective_base) = crate::html::sanitize_url_attributes(html, page_scan, document_url);
    let structure_base = effective_base.as_ref().unwrap_or(document_url);
    let preset = html_to_markdown_rs::options::PreprocessingPreset::parse(&config.preprocessing_preset);

    let output_format = match config.output_format.as_str() {
        "plain" | "plaintext" | "text" => html_to_markdown_rs::options::OutputFormat::Plain,
        "djot" => html_to_markdown_rs::options::OutputFormat::Djot,
        _ => html_to_markdown_rs::options::OutputFormat::Markdown,
    };

    let options = html_to_markdown_rs::options::ConversionOptions {
        output_format,
        include_document_structure: config.include_document_structure,
        preprocessing: html_to_markdown_rs::options::PreprocessingOptions {
            enabled: true,
            preset,
            remove_navigation: config.remove_navigation,
            remove_forms: config.remove_forms,
        },
        strip_tags: config.strip_tags.clone(),
        preserve_tags: config.preserve_tags.clone(),
        exclude_selectors: config.exclude_selectors.clone(),
        skip_images: config.skip_images,
        inline_data_media: html_to_markdown_rs::options::InlineDataMedia::AltTextOnly,
        max_depth: config.max_depth,
        wrap: config.wrap,
        wrap_width: config.wrap_width,
        extract_metadata: config.extract_metadata,
        base_url: Some(document_url.as_str().to_owned()),
        // ~keep Every option crawlberg has no opinion on stays at the library's default on purpose.
        ..Default::default()
    };

    let mut result = html_to_markdown_rs::convert(&html, Some(options))?;
    let content = result.content.unwrap_or_default();
    let document_structure = result.document.as_mut().and_then(|document| {
        sanitize_document_structure(document, structure_base);
        serde_json::to_value(document).ok()
    });
    let tables = result
        .tables
        .iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .collect();
    let warnings = result.warnings.iter().map(|w| format!("{:?}", w)).collect();

    let citation_result = crate::citations::generate_citations(&content);
    let citations = !citation_result.references.is_empty();
    let fit_content = Some(crate::pruning::generate_fit_markdown(&content));

    Ok(MarkdownResult {
        content,
        document_structure,
        tables,
        warnings,
        citations,
        fit_content,
    })
}

/// Convert an HTML string to the configured output format, returning a rich result.
///
/// Relative addresses in the output resolve against `document_url` (or the page's
/// `<base href>`), so pass the URL the content was actually served from.
/// `page_scan` is the extraction's read of `html`; pass it so the page is read once.
///
/// On native targets, delegates to a blocking task so the conversion
/// does not block the async runtime. On wasm, runs synchronously.
///
/// # Errors
///
/// A page the converter refuses, or whose conversion stops, is an error that names the page
/// and the cause. A page is never reported as converted without its result.
pub(crate) async fn convert_to_markdown(
    html: &str,
    page_scan: Option<PageScan>,
    document_url: &Url,
    config: &ContentConfig,
) -> Result<MarkdownResult, CrawlError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let html = html.to_owned();
        let page_url = document_url.clone();
        let config = config.clone();
        convert_on_blocking_task(document_url, move || {
            convert_html_to_markdown(&html, page_scan, &page_url, &config)
        })
        .await
    }

    #[cfg(target_arch = "wasm32")]
    {
        convert_html_to_markdown(html, page_scan, document_url, config)
            .map_err(|refused| refusal(document_url, refused))
    }
}

/// The Markdown of a response that was not skipped as binary or PDF.
///
/// ~keep The one place that says what a failed conversion means, for a scrape and for a crawl. It
/// ~keep is the error of the response only when the response is a page (`is_page_content`) and
/// ~keep was not kept as a downloaded document. A response that is not a page, or one the caller
/// ~keep downloads as a document, was not asked for as Markdown: when the converter refuses it,
/// ~keep the response has no Markdown and keeps the rest of its result, the document included.
///
/// # Errors
///
/// The error of [`convert_to_markdown`], for a page that is not kept as a document.
pub(crate) async fn convert_response_to_markdown(
    body: &str,
    page_scan: Option<PageScan>,
    document_url: &Url,
    config: &ContentConfig,
    content_type: &str,
    kept_as_document: bool,
) -> Result<Option<MarkdownResult>, CrawlError> {
    let is_page = !kept_as_document && crate::html::is_page_content(content_type, body);
    match convert_to_markdown(body, page_scan, document_url, config).await {
        Ok(markdown) => Ok(Some(markdown)),
        Err(error) if is_page => Err(error),
        Err(error) => {
            tracing::debug!(%error, content_type, kept_as_document, "a response that is not a page has no Markdown");
            Ok(None)
        }
    }
}

/// What the error says when the conversion ended without a result and without a refusal.
#[cfg(not(target_arch = "wasm32"))]
const CONVERSION_STOPPED: &str = "the conversion stopped before it finished";

/// Run `conversion` on a blocking task and report its failure as the page's error.
///
/// ~keep A conversion that panics ends its task, and the join error is all that is left of
/// ~keep it. It is the page's error exactly as a refusal by the converter is. Its text is the
/// ~keep runtime's (a task number, the panic message), so it stays in the chain and out of the message.
#[cfg(not(target_arch = "wasm32"))]
async fn convert_on_blocking_task(
    document_url: &Url,
    conversion: impl FnOnce() -> Result<MarkdownResult, ConversionError> + Send + 'static,
) -> Result<MarkdownResult, CrawlError> {
    match tokio::task::spawn_blocking(conversion).await {
        Ok(converted) => converted.map_err(|refused| refusal(document_url, refused)),
        Err(stopped) => Err(conversion_failure(document_url, CONVERSION_STOPPED, stopped)),
    }
}

/// The error for a page the converter refuses. The converter's own words say why.
fn refusal(document_url: &Url, refused: ConversionError) -> CrawlError {
    conversion_failure(document_url, &refused.to_string(), refused)
}

/// The error for a page that has no Markdown because its conversion failed.
fn conversion_failure(
    document_url: &Url,
    why: &str,
    cause: impl std::error::Error + Send + Sync + 'static,
) -> CrawlError {
    let page = crate::net::redact_url_credentials(document_url.as_str());
    CrawlError::conversion_failed_with_source(format!("could not convert {page} to Markdown: {why}"), cause)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("https://example.com/").expect("valid page URL")
    }

    #[tokio::test]
    async fn converts_heading() {
        let result = convert_to_markdown("<h1>Hello</h1>", None, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("# Hello"),
            "expected '# Hello' in markdown, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_paragraph() {
        let result = convert_to_markdown("<p>Some text.</p>", None, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("Some text."),
            "expected 'Some text.' in markdown, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_link() {
        let result = convert_to_markdown(
            r#"<a href="https://example.com">Click</a>"#,
            None,
            &page(),
            &ContentConfig::default(),
        )
        .await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("[Click](https://example.com)"),
            "expected markdown link, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_full_page() {
        let html = r#"<html><head><title>Test</title></head><body>
            <h1>Hello World</h1>
            <p>This is a paragraph.</p>
            <a href="/link">Click here</a>
        </body></html>"#;
        let result = convert_to_markdown(html, None, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("# Hello World"),
            "missing heading: {}",
            result.content
        );
        assert!(
            result.content.contains("This is a paragraph."),
            "missing paragraph: {}",
            result.content
        );
        assert!(
            result.content.contains("[Click here]"),
            "missing link text: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn an_empty_page_is_converted() {
        let result = convert_to_markdown("", None, &page(), &ContentConfig::default()).await;
        assert!(result.is_ok(), "an empty page is a valid page, got: {result:?}");
    }

    /// The converter refuses this input by its own check, for its zip signature.
    const REFUSED_PAGE: &str = "PK\u{3}\u{4}<p>not a page</p>";

    #[tokio::test]
    async fn a_refused_page_is_an_error_that_names_the_page_and_the_cause() {
        let with_credentials = Url::parse("https://reader:hunter2@example.com/docs/page").expect("valid page URL");

        let error = convert_to_markdown(REFUSED_PAGE, None, &with_credentials, &ContentConfig::default())
            .await
            .expect_err("a page the converter refuses has no Markdown");

        assert!(matches!(error, CrawlError::ConversionFailed { .. }), "got: {error:?}");
        let message = error.to_string();
        assert!(
            message.contains("example.com/docs/page to Markdown") && message.contains("zip archive"),
            "the error names the page and the cause, got: {message}"
        );
        assert!(
            !message.contains("hunter2") && !message.contains("reader"),
            "the error hides the credentials of the page URL, got: {message}"
        );
        let cause = std::error::Error::source(&error)
            .and_then(|wrapper| wrapper.source())
            .expect("the converter's error stays in the chain");
        assert!(
            cause.downcast_ref::<ConversionError>().is_some(),
            "the cause is the converter's own error, got: {cause:?}"
        );
    }

    /// What `convert_response_to_markdown` gives for [`REFUSED_PAGE`] served as `content_type`.
    async fn refused_response(
        content_type: &str,
        kept_as_document: bool,
    ) -> Result<Option<MarkdownResult>, CrawlError> {
        convert_response_to_markdown(
            REFUSED_PAGE,
            None,
            &page(),
            &ContentConfig::default(),
            content_type,
            kept_as_document,
        )
        .await
    }

    #[tokio::test]
    async fn a_failed_conversion_is_an_error_only_for_a_page_that_is_not_kept_as_a_document() {
        for page_type in [
            "text/html",
            "text/plain",
            "application/xhtml+xml",
            "APPLICATION/XHTML+XML",
            "Application/Xhtml+Xml; Charset=UTF-8",
            "Text/HTML; Charset=UTF-8",
            " text/html ",
            "TEXT/PLAIN",
        ] {
            let error = refused_response(page_type, false)
                .await
                .expect_err("a page that cannot be converted is an error");
            assert!(
                matches!(error, CrawlError::ConversionFailed { .. }),
                "{page_type}: {error:?}"
            );
        }

        for (content_type, kept_as_document) in [
            ("application/java-archive", false),
            ("font/woff2", false),
            ("application/java-archive", true),
            ("text/html", true),
        ] {
            let markdown = refused_response(content_type, kept_as_document)
                .await
                .unwrap_or_else(|error| panic!("{content_type}, kept {kept_as_document}: {error}"));
            assert!(
                markdown.is_none(),
                "{content_type}, kept {kept_as_document}: no Markdown"
            );
        }
    }

    #[tokio::test]
    async fn a_response_that_is_not_a_page_is_still_converted_when_the_converter_accepts_it() {
        let markdown = convert_response_to_markdown(
            "# Title",
            None,
            &page(),
            &ContentConfig::default(),
            "application/json",
            false,
        )
        .await
        .expect("an accepted body is not an error")
        .expect("an accepted body has Markdown");
        assert!(markdown.content.contains("Title"), "got: {}", markdown.content);
    }

    #[tokio::test]
    async fn a_conversion_that_panics_is_an_error_for_the_page() {
        let error = convert_on_blocking_task(&page(), || panic!("the converter stopped"))
            .await
            .expect_err("a conversion that panics has no Markdown");

        assert!(matches!(error, CrawlError::ConversionFailed { .. }), "got: {error:?}");
        assert_eq!(
            error.to_string(),
            "conversion_failed: could not convert https://example.com/ to Markdown: \
             the conversion stopped before it finished",
            "the error names the page and says in plain words that the conversion stopped"
        );
        assert!(
            std::error::Error::source(&error).is_some(),
            "the cause stays in the chain for a caller that wants it"
        );
    }

    #[tokio::test]
    async fn drops_noscript_fallback_content_by_default() {
        let html = r#"<html><body>
            <p>Real content.</p>
            <noscript>
                <p>Please enable JavaScript to view this site.</p>
                <img src="https://track.example.com/pixel.gif" alt="">
                <iframe src="https://www.googletagmanager.com/ns.html?id=GTM-XXXX"></iframe>
            </noscript>
            <p>More content.</p>
        </body></html>"#;
        let result = convert_to_markdown(html, None, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert_eq!(result.content, "Real content.\n\nMore content.\n");
    }

    #[tokio::test]
    async fn preserves_content_when_no_noscript_present() {
        let html = r#"<html><body>
            <h1>Hello World</h1>
            <p>This is a paragraph.</p>
            <a href="/link">Click here</a>
        </body></html>"#;
        let result = convert_to_markdown(html, None, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert_eq!(
            result.content,
            "# Hello World\n\nThis is a paragraph.\n\n[Click here](https://example.com/link)\n"
        );
    }

    async fn result_at(html: &str, document_url: &str, page_scan: Option<PageScan>) -> MarkdownResult {
        let url = Url::parse(document_url).expect("valid document URL");
        convert_to_markdown(html, page_scan, &url, &ContentConfig::default())
            .await
            .expect("should produce markdown")
    }

    async fn markdown_at(html: &str, document_url: &str) -> String {
        let page_scan = crate::html::mask_raw_text_markup(html).detach();
        result_at(html, document_url, Some(page_scan)).await.content
    }

    fn structure_with_empty_link_and_image_urls() -> html_to_markdown_rs::types::DocumentStructure {
        use html_to_markdown_rs::types::{
            AnnotationKind, DocumentNode, DocumentStructure, NodeContent, TextAnnotation,
        };

        DocumentStructure {
            nodes: vec![
                DocumentNode {
                    id: "link".to_owned(),
                    content: NodeContent::Paragraph {
                        text: "link".to_owned(),
                    },
                    parent: None,
                    children: Vec::new(),
                    annotations: vec![TextAnnotation {
                        start: 0,
                        end: 4,
                        kind: AnnotationKind::Link {
                            url: String::new(),
                            title: None,
                        },
                    }],
                    attributes: None,
                },
                DocumentNode {
                    id: "image".to_owned(),
                    content: NodeContent::Image {
                        description: Some("image".to_owned()),
                        src: Some(String::new()),
                        image_index: None,
                    },
                    parent: None,
                    children: Vec::new(),
                    annotations: Vec::new(),
                    attributes: None,
                },
            ],
            source_format: Some("html".to_owned()),
        }
    }

    #[test]
    fn structured_empty_link_and_image_urls_stay_empty() {
        use html_to_markdown_rs::types::{AnnotationKind, NodeContent};

        let mut document = structure_with_empty_link_and_image_urls();
        let base = Url::parse("https://example.com/page").expect("valid base URL");

        sanitize_document_structure(&mut document, &base);

        let AnnotationKind::Link { url, .. } = &document.nodes[0].annotations[0].kind else {
            panic!("expected link annotation");
        };
        assert_eq!(url, "");
        let NodeContent::Image { src, .. } = &document.nodes[1].content else {
            panic!("expected image node");
        };
        assert_eq!(src.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn resolves_a_relative_link_against_the_page_url() {
        let md = markdown_at(
            r#"<p><a href="rel/child.html">child</a></p>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "[child](https://example.com/docs/rel/child.html)\n");
    }

    #[tokio::test]
    async fn resolves_root_relative_query_scheme_relative_and_parent_links() {
        let md = markdown_at(
            r#"<p><a href="/top.html">a</a> <a href="?page=2">b</a> <a href="//cdn.example.org/x">c</a> <a href="../up.html">d</a></p>"#,
            "https://example.com/docs/guide/index.html",
        )
        .await;
        assert_eq!(
            md,
            "[a](https://example.com/top.html) [b](https://example.com/docs/guide/index.html?page=2) \
             [c](https://cdn.example.org/x) [d](https://example.com/docs/up.html)\n"
        );
    }

    #[tokio::test]
    async fn resolves_against_an_absolute_base_href() {
        let md = markdown_at(
            r#"<html><head><base href="https://mirror.example.net/v2/"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(
            md.ends_with("\n[leaf](https://mirror.example.net/v2/leaf.html)\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn resolves_a_relative_base_href_against_the_page_url() {
        let md = markdown_at(
            r#"<html><head><base href="/other/"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "http://127.0.0.1:8000/",
        )
        .await;
        assert!(
            md.ends_with("\n[leaf](http://127.0.0.1:8000/other/leaf.html)\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn only_the_first_base_href_counts() {
        let md = markdown_at(
            r#"<html><head><base href="/first/"><base href="/second/"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "https://example.com/",
        )
        .await;
        assert!(
            md.ends_with("\n[leaf](https://example.com/first/leaf.html)\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn resolves_a_relative_image_source() {
        let md = markdown_at(
            r#"<p><img src="img/logo.png" alt="logo"></p>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "![logo](https://example.com/docs/img/logo.png)\n");
    }

    #[tokio::test]
    async fn leaves_absolute_and_non_data_targets_as_written_and_resolves_fragments() {
        let md = markdown_at(
            r##"<p><a href="https://other.example/x">abs</a> <a href="#section">frag</a> <a href="mailto:me@example.com">mail</a> <a href="tel:+15551234">tel</a> <a href="javascript:void(0)">js</a> <img src="data:image/gif;base64,R0lGOD" alt="px"></p>"##,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(
            md,
            concat!(
                "[abs](https://other.example/x) [frag](https://example.com/docs/index.html#section) ",
                "[mail](mailto:me@example.com) [tel](tel:+15551234) [js](javascript:void(0)) px\n"
            )
        );
    }

    #[tokio::test]
    async fn replaces_an_inline_image_payload_with_its_alt_text() {
        let payload = "A".repeat(8_192);
        let html = format!(r#"<p>before</p><img src="data:image/svg+xml;base64,{payload}" alt="icon"><p>after</p>"#);
        let md = markdown_at(&html, "https://example.com/").await;
        assert_eq!(md, "before\n\nicon\n\nafter\n");
        assert!(!md.contains(&payload), "inline payload leaked into markdown");
    }

    #[tokio::test]
    async fn replaces_inline_data_destinations_for_links_and_embedded_media() {
        let html = r#"
            <p><a href="data:text/plain;base64,LINK_PAYLOAD">download</a></p>
            <video src="data:video/mp4;base64,VIDEO_PAYLOAD">video fallback</video>
            <audio src="data:audio/mpeg;base64,AUDIO_PAYLOAD">audio fallback</audio>
            <iframe src="data:text/html;base64,IFRAME_PAYLOAD"></iframe>
            <p><svg width="1" height="1"><title>logo</title><rect width="1" height="1"/></svg></p>
        "#;
        let md = markdown_at(html, "https://example.com/").await;
        let visible_lines: Vec<_> = md.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(visible_lines, ["download", "video fallback", "audio fallback", "logo"]);
        assert!(
            !md.contains("data:"),
            "inline data destination leaked into markdown: {md}"
        );
    }

    #[tokio::test]
    async fn a_script_base_href_resolves_relative_links_against_the_page_url() {
        let md = markdown_at(
            r#"<html><head><base href="javascript:alert(1)"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(
            md.ends_with("\n[leaf](https://example.com/docs/leaf.html)\n"),
            "a javascript: base falls back to the page URL, got: {md}"
        );
    }

    #[tokio::test]
    async fn the_front_matter_shows_the_resolved_base_address() {
        let md = markdown_at(
            r#"<html><head><base href="/other/"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "http://127.0.0.1:8000/",
        )
        .await;
        assert!(
            md.starts_with("---\nbase: http://127.0.0.1:8000/other/\n---\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn the_front_matter_shows_the_first_base_when_there_are_several() {
        let md = markdown_at(
            r#"<html><head><base href="/first/"><base href="/second/"></head><body><p>x</p></body></html>"#,
            "https://example.com/",
        )
        .await;
        assert!(
            md.starts_with("---\nbase: https://example.com/first/\n---\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn protocol_relative_base_credentials_are_removed() {
        let md = markdown_at(
            r#"<html><head><base href="//user:secret@example.com/root/"></head><body><p>x</p></body></html>"#,
            "https://origin.example/docs/page.html",
        )
        .await;
        assert!(
            md.starts_with("---\nbase: https://example.com/root/\n---\n"),
            "got: {md}"
        );
        assert!(!md.contains("user:secret"));
    }

    #[tokio::test]
    async fn decodes_character_references_before_resolving() {
        let md = markdown_at(
            r#"<p><a href="&#x2F;app&#x2F;list?a=1">esapi</a> <a href="https&#58;//other.example/y">abs</a> <a href="&#47;root.html">root</a></p>"#,
            "https://example.com/dir/page.html",
        )
        .await;
        assert_eq!(
            md,
            "[esapi](https://example.com/app/list?a=1) [abs](https://other.example/y) [root](https://example.com/root.html)\n"
        );
    }

    #[tokio::test]
    async fn resolves_lazy_image_attributes_behind_a_placeholder_src() {
        for attr in ["data-src", "data-lazy-src", "data-original"] {
            let html = format!(r#"<p><img src="data:image/gif;base64,R0lGOD" {attr}="lazy/photo.jpg" alt="a"></p>"#);
            let md = markdown_at(&html, "https://example.com/docs/index.html").await;
            assert_eq!(
                md, "![a](https://example.com/docs/lazy/photo.jpg)\n",
                "attribute {attr}"
            );
        }
    }

    #[tokio::test]
    async fn resolves_srcset_candidates() {
        for attr in ["data-srcset", "srcset"] {
            let html = format!(r#"<p><img {attr}="small.jpg 300w, large.jpg 800w" alt="a"></p>"#);
            let md = markdown_at(&html, "https://example.com/docs/index.html").await;
            assert_eq!(md, "![a](https://example.com/docs/large.jpg)\n", "attribute {attr}");
        }
    }

    #[tokio::test]
    async fn strips_userinfo_from_the_selected_srcset_candidate() {
        let md = markdown_at(
            concat!(
                r#"<p><img src="data:image/gif;base64,R0lGOD" "#,
                r#"srcset="//small:secret@cdn.example/s.png 1x, "#,
                r#"//large:secret@cdn.example/l.png 2x" alt="a"></p>"#
            ),
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "![a](https://cdn.example/l.png)\n");
        assert!(!md.contains("secret"));
    }

    #[tokio::test]
    async fn resolves_embedded_media_sources() {
        for html in [
            r#"<iframe src="embed/v.html"></iframe>"#,
            r#"<video src="embed/v.html"></video>"#,
            r#"<audio src="embed/v.html"></audio>"#,
            r#"<video><source src="embed/v.html"></video>"#,
        ] {
            let md = markdown_at(html, "https://example.com/docs/index.html").await;
            assert!(
                md.contains("(https://example.com/docs/embed/v.html)"),
                "{html} gave: {md}"
            );
            assert!(!md.contains("(embed/"), "{html} gave: {md}");
        }
    }

    #[tokio::test]
    async fn resolves_a_blockquote_citation() {
        let md = markdown_at(
            r#"<blockquote cite="sources/c.html"><p>quoted</p></blockquote>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(md.contains("<https://example.com/docs/sources/c.html>"), "got: {md}");
    }

    #[tokio::test]
    async fn resolves_a_graphic_address() {
        for attr in ["url", "href", "xlink:href", "src"] {
            let html = format!(r#"<p><graphic {attr}="fig/g.png" alt="g"></graphic></p>"#);
            let md = markdown_at(&html, "https://example.com/docs/index.html").await;
            assert!(
                md.contains("(https://example.com/docs/fig/g.png)"),
                "attribute {attr} gave: {md}"
            );
        }
    }

    #[tokio::test]
    async fn leaves_an_empty_image_or_media_source_empty() {
        let mut wrong = Vec::new();
        for (html, expected) in [
            (r#"<p><img src="" alt="a"></p>"#, "![a](<>)\n"),
            (r#"<p><img src="" srcset="" alt="a"></p>"#, "![a](<>)\n"),
            (r#"<video src="">clip</video>"#, "clip\n"),
        ] {
            let md = markdown_at(html, "https://example.com/docs/index.html").await;
            if md != expected {
                wrong.push(format!("{html} gave: {md:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[tokio::test]
    async fn resolves_a_fragment_only_link_against_the_full_page_url() {
        let md = markdown_at(
            r##"<p><a href="#section">frag</a> <a href="#">top</a></p>"##,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(
            md,
            "[frag](https://example.com/docs/index.html#section) [top](https://example.com/docs/index.html#)\n"
        );
    }

    #[tokio::test]
    async fn resolves_a_fragment_only_link_against_the_effective_base_url() {
        let md = markdown_at(
            r##"<base href="/assets/"><p><a href="#section">frag</a></p>"##,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(
            md.ends_with("[frag](https://example.com/assets/#section)\n"),
            "got: {md}"
        );
    }

    #[tokio::test]
    async fn drops_a_credential_bearing_url_past_the_attribute_limit() {
        let attributes = (0..crate::html::ATTRIBUTE_LIMIT)
            .map(|index| format!("data-{index}=x"))
            .collect::<Vec<_>>()
            .join(" ");
        let html = format!(r#"<a {attributes} href="https://user:secret@example.com/private">safe</a>"#);

        let md = markdown_at(&html, "https://example.com/docs/index.html").await;

        assert_eq!(md, "safe\n");
        assert!(!md.contains("user:secret"));
    }

    #[tokio::test]
    async fn sanitizes_and_resolves_structured_link_and_image_urls() {
        let html = concat!(
            r#"<html><head><base href="/root/"></head><body>"#,
            r#"<p><a href="child?q=1#part">link</a></p>"#,
            r#"<img src="//image:secret@cdn.example/p.png?x=2#end" alt="image">"#,
            "</body></html>"
        );
        let result = result_at(html, "https://origin.example/docs/page.html", None).await;
        let structure = result.document_structure.expect("document structure");
        let nodes = structure["nodes"].as_array().expect("nodes array");
        let link_urls = nodes
            .iter()
            .flat_map(|node| node["annotations"].as_array().into_iter().flatten())
            .filter(|annotation| annotation["kind"]["annotation_type"] == "link")
            .map(|annotation| annotation["kind"]["url"].as_str().expect("link URL"))
            .collect::<Vec<_>>();
        let image_sources = nodes
            .iter()
            .filter(|node| node["content"]["node_type"] == "image")
            .map(|node| node["content"]["src"].as_str().expect("image source"))
            .collect::<Vec<_>>();
        assert_eq!(link_urls, ["https://origin.example/root/child?q=1#part"]);
        assert_eq!(image_sources, ["https://cdn.example/p.png?x=2#end"]);
    }

    #[tokio::test]
    async fn strips_userinfo_from_a_link_without_changing_its_target() {
        let md = markdown_at(
            concat!(
                r#"<p><a href="http://page:pw@example.com/b?q=1#part">b</a> "#,
                r#"<a href="//user:s3cret@example.com/a?x=2#end">a</a> "#,
                r#"<a href="ftp://ftp:pw@example.com/f">f</a></p>"#
            ),
            "https://example.com/",
        )
        .await;
        assert_eq!(
            md,
            "[b](http://example.com/b?q=1#part) [a](https://example.com/a?x=2#end) [f](ftp://example.com/f)\n"
        );
        assert!(!md.contains("page:pw"));
        assert!(!md.contains("user:s3cret"));
        assert!(!md.contains("ftp:pw"));
    }

    #[tokio::test]
    async fn userinfo_sanitization_does_not_rewrite_prose_code_or_titles() {
        let md = markdown_at(
            concat!(
                r#"<p>http://text:secret@example.com/prose "#,
                r#"<code>http://code:secret@example.com/x</code> "#,
                r#"<a href="https://link:secret@example.com/y" "#,
                r#"title="http://title:secret@example.com/z">x</a></p>"#
            ),
            "https://example.com/",
        )
        .await;
        assert_eq!(
            md,
            concat!(
                "http://text:secret@example.com/prose `http://code:secret@example.com/x` ",
                "[x](https://example.com/y \"http://title:secret@example.com/z\")\n"
            )
        );
    }

    #[tokio::test]
    async fn missing_page_scan_falls_back_to_attribute_sanitization() {
        let result = result_at(
            r#"<a href="//user:secret@example.com/a">a</a>"#,
            "https://origin.example/page",
            None,
        )
        .await;
        assert_eq!(result.content, "[a](https://example.com/a)\n");
    }

    #[tokio::test]
    async fn a_base_href_in_a_comment_or_raw_text_does_not_count() {
        let mut wrong = Vec::new();
        for head in [
            r#"<!-- <base href="/comment/"> -->"#,
            r#"<title><base href="/title/"></title>"#,
            r#"<script>document.write('<base href="/script/">')</script>"#,
            r#"<style>/* <base href="/style/"> */</style>"#,
        ] {
            let html = format!(r#"<html><head>{head}</head><body><p><a href="leaf.html">leaf</a></p></body></html>"#);
            let md = markdown_at(&html, "https://example.com/docs/index.html").await;
            if !md.ends_with("[leaf](https://example.com/docs/leaf.html)\n") {
                wrong.push(format!("{head} gave: {md}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[tokio::test]
    async fn the_base_href_after_a_decoy_in_raw_text_counts() {
        let md = markdown_at(
            r#"<html><head><title><base href="/title/"></title><base href="/real/"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(
            md.ends_with("[leaf](https://example.com/real/leaf.html)\n"),
            "got: {md}"
        );
    }

    /// Link markup inside a `<textarea>` is text, so its address stays as written (#102). ~keep
    #[tokio::test]
    async fn link_markup_inside_a_textarea_stays_as_written() {
        let md = markdown_at(
            r#"<textarea>see <a href="x.html">here</a></textarea><p>x</p>"#,
            "https://example.com/dir/page.html",
        )
        .await;
        assert_eq!(md, "see [here](x.html)\n\nx\n");
    }

    #[tokio::test]
    async fn parenthesised_srcset_descriptors_keep_embedded_commas() {
        let md = markdown_at(
            r#"<p><img srcset="a.png 1x (x, y.png 9x ), b.png 2x" alt="a"></p>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "![a](https://example.com/docs/b.png)\n");
    }

    #[tokio::test]
    async fn resolves_a_srcset_url_ending_in_a_comma_before_a_vertical_tab() {
        let md = markdown_at(
            "<p><img srcset=\"a,\x0b 1x\" alt=\"a\"></p>",
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "![a](https://example.com/docs/a,)\n");
    }
}
