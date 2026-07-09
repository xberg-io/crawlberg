//! HTML-to-Markdown conversion -- always active.

use crate::types::{ContentConfig, MarkdownResult};

/// Perform the actual HTML-to-Markdown conversion (synchronous).
fn convert_html_to_markdown(html: &str, config: &ContentConfig) -> Option<MarkdownResult> {
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
        ..Default::default()
    };

    match html_to_markdown_rs::convert(html, Some(options)) {
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
/// On native targets, delegates to a blocking task so the conversion
/// does not block the async runtime. On wasm, runs synchronously.
pub(crate) async fn convert_to_markdown(html: &str, config: &ContentConfig) -> Option<MarkdownResult> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let html = html.to_owned();
        let config = config.clone();
        tokio::task::spawn_blocking(move || convert_html_to_markdown(&html, &config))
            .await
            .ok()
            .flatten()
    }

    #[cfg(target_arch = "wasm32")]
    {
        convert_html_to_markdown(html, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn converts_heading() {
        let result = convert_to_markdown("<h1>Hello</h1>", &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("# Hello"),
            "expected '# Hello' in markdown, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_paragraph() {
        let result = convert_to_markdown("<p>Some text.</p>", &ContentConfig::default()).await;
        let result = result.expect("should produce markdown");
        assert!(
            result.content.contains("Some text."),
            "expected 'Some text.' in markdown, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn converts_link() {
        let result = convert_to_markdown(r#"<a href="https://example.com">Click</a>"#, &ContentConfig::default()).await;
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
        let result = convert_to_markdown(html, &ContentConfig::default()).await;
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
        let result = convert_to_markdown("", &ContentConfig::default()).await;
        assert!(result.is_some(), "empty html should still return Some");
    }
}
