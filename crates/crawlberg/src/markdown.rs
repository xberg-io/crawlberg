//! HTML-to-Markdown conversion -- always active.

use url::Url;

use crate::types::{ContentConfig, MarkdownResult};

/// Perform the actual HTML-to-Markdown conversion (synchronous).
///
/// ~keep html-to-markdown-rs 3.14 has no base URL option and writes each address as found, so
/// ~keep relative addresses are made absolute, and image and media `data:` addresses emptied, in
/// ~keep the HTML first; see `resolve_link_targets`.
fn convert_html_to_markdown(html: &str, document_url: &Url, config: &ContentConfig) -> Option<MarkdownResult> {
    let html = crate::html::resolve_link_targets(html, document_url);
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
        max_depth: config.max_depth,
        wrap: config.wrap,
        wrap_width: config.wrap_width,
        extract_metadata: config.extract_metadata,
        // ~keep Every option crawlberg has no opinion on stays at the library's default on
        // ~keep purpose, with one option that must never be picked up by accident: from 3.15 on,
        // ~keep html-to-markdown-rs has a `base_url` that resolves relative addresses the way
        // ~keep `resolve_link_targets` above already does. Setting it is only safe after #123 --
        // ~keep it resolves the empty `src=""` left by a dropped inline-data payload to the page
        // ~keep itself, and rewrites the fragment-only hrefs this crate leaves as written. See
        // ~keep #190; `html_to_markdown_has_no_base_url_option` below fails when 3.15 arrives.
        ..Default::default()
    };

    match html_to_markdown_rs::convert(&html, Some(options)) {
        Ok(result) => {
            let content = result.content.unwrap_or_default();
            let document_structure = result.document.and_then(|d| serde_json::to_value(d).ok());
            let tables = result
                .tables
                .iter()
                .filter_map(|t| serde_json::to_value(t).ok())
                .collect();
            let warnings = result.warnings.iter().map(|w| format!("{:?}", w)).collect();

            let citation_result = crate::citations::generate_citations(&content);
            let citations = !citation_result.references.is_empty();
            let fit_content = Some(crate::pruning::generate_fit_markdown(&content));

            Some(MarkdownResult {
                content,
                document_structure,
                tables,
                warnings,
                citations,
                fit_content,
            })
        }
        Err(_) => None,
    }
}

/// Convert an HTML string to the configured output format, returning a rich result.
///
/// Relative addresses in the output resolve against `document_url` (or the page's
/// `<base href>`), so pass the URL the content was actually served from.
///
/// On native targets, delegates to a blocking task so the conversion
/// does not block the async runtime. On wasm, runs synchronously.
pub(crate) async fn convert_to_markdown(
    html: &str,
    document_url: &Url,
    config: &ContentConfig,
) -> Option<MarkdownResult> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let html = html.to_owned();
        let document_url = document_url.clone();
        let config = config.clone();
        tokio::task::spawn_blocking(move || convert_html_to_markdown(&html, &document_url, &config))
            .await
            .ok()
            .flatten()
    }

    #[cfg(target_arch = "wasm32")]
    {
        convert_html_to_markdown(html, document_url, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("https://example.com/").expect("valid page URL")
    }

    #[tokio::test]
    async fn converts_heading() {
        let result = convert_to_markdown("<h1>Hello</h1>", &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("# Hello"),
            "expected '# Hello' in markdown, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_paragraph() {
        let result = convert_to_markdown("<p>Some text.</p>", &page(), &ContentConfig::default()).await;
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
        let result = convert_to_markdown(html, &page(), &ContentConfig::default()).await;
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
    async fn empty_html_returns_some() {
        let result = convert_to_markdown("", &page(), &ContentConfig::default()).await;
        assert!(result.is_some(), "empty html should still return Some");
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
        let result = convert_to_markdown(html, &page(), &ContentConfig::default()).await;
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
        let result = convert_to_markdown(html, &page(), &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert_eq!(
            result.content,
            "# Hello World\n\nThis is a paragraph.\n\n[Click here](https://example.com/link)\n"
        );
    }

    async fn markdown_at(html: &str, document_url: &str) -> String {
        let url = Url::parse(document_url).expect("valid document URL");
        convert_to_markdown(html, &url, &ContentConfig::default())
            .await
            .expect("should produce markdown")
            .content
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
    async fn leaves_absolute_fragment_and_non_http_targets_as_written() {
        let md = markdown_at(
            r##"<p><a href="https://other.example/x">abs</a> <a href="#section">frag</a> <a href="mailto:me@example.com">mail</a> <a href="tel:+15551234">tel</a> <a href="javascript:void(0)">js</a> <a href="data:text/plain,hi">data</a></p>"##,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(
            md,
            "[abs](https://other.example/x) [frag](#section) [mail](mailto:me@example.com) [tel](tel:+15551234) [js](javascript:void(0)) [data](data:text/plain,hi)\n"
        );
    }

    #[tokio::test]
    async fn an_opaque_script_base_href_leaves_relative_links_as_written() {
        let md = markdown_at(
            r#"<html><head><base href="javascript:alert(1)"></head><body><p><a href="leaf.html">leaf</a></p></body></html>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert!(
            md.ends_with("\n[leaf](leaf.html)\n"),
            "a javascript: base cannot be joined to, got: {md}"
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

    /// The inline SVG icon from issue #97, base64-encoded.
    const ICON_PAYLOAD: &str = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMCIgaGVpZ2h0PSIyMCI+\
        PHBhdGggZD0iTTEwIDEwIDIwIDIwIDMwIDMwIDQwIDQwIDUwIDUwIDYwIDYwIDcwIDcwIDgwIDgwIDkwIDkwIiAvPjxjaXJjbGUgY3g9IjUwIiBjeT0iNTAiIHI9IjQwIiAvPjwvc3ZnPg==";

    #[tokio::test]
    async fn an_inline_data_image_keeps_its_alt_text_and_drops_the_payload() {
        let html = format!(
            r#"<html><body><p>Real text before the icon.</p><img src="data:image/svg+xml;base64,{ICON_PAYLOAD}" alt="icon"><p>Real text after the icon.</p></body></html>"#
        );
        let md = markdown_at(&html, "https://example.com/").await;
        assert!(
            !md.contains("data:"),
            "the encoded image must not reach the markdown, got: {md}"
        );
        assert!(
            md.len() < 100,
            "{} bytes of markdown for two sentences and an icon: {md}",
            md.len()
        );
        assert_eq!(
            md,
            "Real text before the icon.\n\n![icon](<>)\n\nReal text after the icon.\n"
        );
    }

    #[tokio::test]
    async fn drops_an_unencoded_svg_data_address() {
        let md = markdown_at(
            r#"<p><img src='data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="9"></svg>' alt="logo"></p>"#,
            "https://example.com/",
        )
        .await;
        assert_eq!(md, "![logo](<>)\n");
    }

    #[tokio::test]
    async fn drops_inline_data_candidates_from_an_image_srcset() {
        for attr in ["data-srcset", "srcset"] {
            let html = format!(r#"<p><img {attr}="data:image/gif;base64,R0lGOD 2x" alt="a"></p>"#);
            let md = markdown_at(&html, "https://example.com/docs/index.html").await;
            assert_eq!(md, "![a](<>)\n", "attribute {attr}");
        }
    }

    #[tokio::test]
    async fn media_and_iframes_drop_an_inline_data_address() {
        for html in [
            format!(r#"<p>before</p><video src="data:video/mp4;base64,{ICON_PAYLOAD}"></video><p>after</p>"#),
            format!(r#"<p>before</p><audio src="data:audio/mpeg;base64,{ICON_PAYLOAD}"></audio><p>after</p>"#),
            format!(r#"<p>before</p><iframe src="data:text/html;base64,{ICON_PAYLOAD}"></iframe><p>after</p>"#),
            format!(r#"<p>before</p><video><source src="data:video/mp4;base64,{ICON_PAYLOAD}"></video><p>after</p>"#),
        ] {
            let md = markdown_at(&html, "https://example.com/").await;
            assert_eq!(md, "before\n\nafter\n", "for {html}");
        }
    }

    #[tokio::test]
    async fn a_video_falls_through_an_inline_data_address_to_its_source() {
        let md = markdown_at(
            r#"<video src="data:video/mp4;base64,AAAA"><source src="clip.mp4"></video>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(
            md,
            "[https://example.com/docs/clip.mp4](https://example.com/docs/clip.mp4)\n"
        );
    }

    #[tokio::test]
    async fn a_lazy_data_attribute_does_not_replace_a_real_srcset() {
        let md = markdown_at(
            r#"<p><img data-src="data:image/gif;base64,R0lGOD" srcset="big.jpg 2x" alt="a"></p>"#,
            "https://example.com/docs/index.html",
        )
        .await;
        assert_eq!(md, "![a](https://example.com/docs/big.jpg)\n");
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

    /// The converter option whose arrival makes this crate's link pre-pass redundant.
    const BASE_URL_OPTION: &str = "base_url";

    /// ~keep A canary on the dependency, not a behaviour test. The `html-to-markdown-rs`
    /// ~keep requirement is a caret range, so 3.15 -- the first version with a `base_url`
    /// ~keep conversion option -- arrives on a routine `cargo update` with nothing to compile
    /// ~keep against it and nothing to fail. From that point crawlberg carries two relative-link
    /// ~keep resolvers, and this is the only thing that says so. Read #190 before deleting it;
    /// ~keep do not silence it by setting `base_url`, which is unsafe until #123.
    #[test]
    fn html_to_markdown_has_no_base_url_option() {
        let options = html_to_markdown_rs::options::ConversionOptions::default();
        let serialized = serde_json::to_value(&options).expect("conversion options should serialize");
        let fields = serialized
            .as_object()
            .expect("conversion options should be a JSON object");

        assert!(
            !fields.contains_key(BASE_URL_OPTION),
            "html-to-markdown-rs now has a `{BASE_URL_OPTION}` conversion option, so crawlberg has two \
             relative-link resolvers: this one and `crate::html::resolve_link_targets`. Reconcile them \
             before landing this dependency bump -- see issue #190. Do not just set `base_url`: it is \
             only safe after #123, because it resolves the empty `src=\"\"` that marks a dropped \
             inline-data payload to the page URL, and it rewrites the fragment-only hrefs that \
             `resolve_link_targets` deliberately leaves as written."
        );
    }
}
