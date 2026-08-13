//! LLM-powered content extraction using liter-llm.
#![allow(dead_code)]
//!
//! Requires the `ai` feature flag.

#[cfg(feature = "ai")]
mod inner {
    use async_trait::async_trait;
    use serde_json::Value;

    use crate::error::CrawlError;
    use crate::traits::ContentFilter;
    use crate::types::{CrawlPageResult, ExtractionMeta};

    const DEFAULT_EXTRACTION_TEMPLATE: &str = r#"Extract structured data from the following content.
{% if instruction %}
{{ instruction }}
{% endif %}
{% if schema %}
Output must conform to this JSON schema:
```json
{{ schema }}
```
{% endif %}

Content:
{{ content }}"#;

    const MAX_CONTENT_CHARS: usize = 100_000;

    /// Truncate a string to at most `max_bytes` bytes on a valid char boundary.
    fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
        if s.len() <= max_bytes {
            return s;
        }
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }

    /// Extracts structured data from crawled pages using an LLM.
    pub struct LlmExtractor {
        client: liter_llm::DefaultClient,
        model: String,
        schema: Option<Value>,
        instruction: Option<String>,
        prompt_template: Option<String>,
    }

    impl LlmExtractor {
        /// Create a new LLM extractor.
        ///
        /// - `api_key`: API key for the LLM provider
        /// - `model`: Model identifier (e.g. `"openai/gpt-4o-mini"`, `"anthropic/claude-sonnet-4-20250514"`)
        /// - `schema`: Optional JSON schema for structured extraction
        /// - `instruction`: Optional extraction instruction
        /// - `prompt_template`: Optional custom Jinja2 template for the prompt
        pub fn new(
            api_key: &str,
            model: &str,
            schema: Option<Value>,
            instruction: Option<String>,
            prompt_template: Option<String>,
        ) -> Result<Self, CrawlError> {
            let config = liter_llm::ClientConfig::new(api_key);
            let client = liter_llm::DefaultClient::new(config, Some(model))
                .map_err(|e| CrawlError::other(format!("failed to create LLM client: {e}")))?;
            Ok(Self {
                client,
                model: model.to_owned(),
                schema,
                instruction,
                prompt_template,
            })
        }
    }

    #[async_trait]
    impl ContentFilter for LlmExtractor {
        async fn filter(&self, mut page: CrawlPageResult) -> Result<Option<CrawlPageResult>, CrawlError> {
            use liter_llm::LlmClient;

            let content = page.markdown.as_ref().map(|m| m.content.as_str()).unwrap_or(&page.html);

            let content = truncate_to_char_boundary(content, MAX_CONTENT_CHARS);

            let mut env = minijinja::Environment::new();
            let template_str = self.prompt_template.as_deref().unwrap_or(DEFAULT_EXTRACTION_TEMPLATE);
            env.add_template("prompt", template_str)
                .map_err(|e| CrawlError::other(format!("template error: {e}")))?;
            let tmpl = env.get_template("prompt").expect("template was just added above");

            let rendered = tmpl
                .render(minijinja::context! {
                    content => content,
                    schema => self.schema.as_ref().map(|s| serde_json::to_string_pretty(s).unwrap_or_default()),
                    instruction => self.instruction.as_deref(),
                    url => &page.url,
                    title => page.metadata.title.as_deref(),
                })
                .map_err(|e| CrawlError::other(format!("template render error: {e}")))?;

            let request = liter_llm::ChatCompletionRequest {
                model: self.model.clone(),
                messages: vec![
                    liter_llm::Message::System(liter_llm::SystemMessage {
                        content: "You are a data extraction assistant. Extract structured data from the provided content. Return valid JSON only.".into(),
                        name: None,
                    }),
                    liter_llm::Message::User(liter_llm::UserMessage {
                        content: liter_llm::UserContent::Text(rendered),
                        name: None,
                    }),
                ],
                response_format: self.schema.as_ref().map(|s| liter_llm::ResponseFormat::JsonSchema {
                    json_schema: liter_llm::JsonSchemaFormat {
                        name: "extraction".to_owned(),
                        description: None,
                        schema: s.clone(),
                        strict: Some(true),
                    },
                }),
                ..Default::default()
            };

            let response = self
                .client
                .chat(request)
                .await
                .map_err(|e| CrawlError::other(format!("LLM extraction failed: {e}")))?;

            let cost = response.estimated_cost();
            let usage = response.usage.as_ref();

            page.extraction_meta = Some(ExtractionMeta {
                cost,
                prompt_tokens: usage.map(|u| u.prompt_tokens),
                completion_tokens: usage.map(|u| u.completion_tokens),
                model: Some(self.model.clone()),
                chunks_processed: 1,
            });

            if let Some(choice) = response.choices.first()
                && let Some(text) = choice.message.content.as_ref().and_then(|content| content.as_text())
            {
                let extracted: Value = serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()));
                page.extracted_data = Some(extracted);
            }

            Ok(Some(page))
        }
    }
}
