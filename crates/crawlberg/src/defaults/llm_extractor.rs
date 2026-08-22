//! LLM-powered content extraction using liter-llm.
#![allow(dead_code)]
//!
//! Requires the `ai` feature flag.

#[cfg(feature = "ai")]
mod inner {
    use std::time::Duration;

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

    /// Default ceiling on provider requests in flight at once for one extractor.
    ///
    /// Chosen to keep a crawl's LLM fan-out well inside the per-minute request
    /// quotas the major providers apply to a single API key, while still
    /// overlapping enough calls to hide per-request latency. ~keep
    const DEFAULT_MAX_IN_FLIGHT: usize = 8;

    /// Default number of extraction responses retained by the response cache.
    const DEFAULT_CACHE_MAX_ENTRIES: usize = 256;

    /// Default lifetime of a cached extraction response.
    const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

    /// Response cache applied in front of the provider.
    ///
    /// Cache hits are served without consuming an in-flight permit, so a bounded
    /// extractor still answers repeat pages immediately while the bound is
    /// saturated by uncached work. ~keep
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LlmResponseCacheConfig {
        /// Maximum number of cached responses.
        pub max_entries: usize,
        /// How long a cached response stays valid.
        pub ttl: Duration,
    }

    impl Default for LlmResponseCacheConfig {
        fn default() -> Self {
            Self {
                max_entries: DEFAULT_CACHE_MAX_ENTRIES,
                ttl: DEFAULT_CACHE_TTL,
            }
        }
    }

    /// Crawlberg-owned configuration for [`LlmExtractor`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LlmExtractorConfig {
        /// Model identifier (e.g. `"openai/gpt-4o-mini"`).
        pub model: String,
        /// Optional JSON schema for structured extraction.
        pub schema: Option<Value>,
        /// Optional extraction instruction.
        pub instruction: Option<String>,
        /// Optional custom Jinja2 template for the prompt.
        pub prompt_template: Option<String>,
        /// Ceiling on provider requests in flight at once, applied globally per
        /// client rather than per call site.
        ///
        /// `None` means unlimited. `Some(0)` is rejected by
        /// [`LlmExtractor::with_config`] as invalid configuration.
        pub max_in_flight: Option<usize>,
        /// Response cache placed in front of the provider. `None` disables caching.
        pub response_cache: Option<LlmResponseCacheConfig>,
        /// Override for the provider base URL. `None` uses provider auto-detection.
        pub base_url: Option<String>,
    }

    impl LlmExtractorConfig {
        /// Build a config for `model` with the default in-flight bound and no cache.
        pub fn new(model: impl Into<String>) -> Self {
            Self {
                model: model.into(),
                schema: None,
                instruction: None,
                prompt_template: None,
                max_in_flight: Some(DEFAULT_MAX_IN_FLIGHT),
                response_cache: None,
                base_url: None,
            }
        }
    }

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
        client: liter_llm::ManagedClient,
        config: LlmExtractorConfig,
    }

    impl LlmExtractor {
        /// Create a new LLM extractor with the default in-flight bound.
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
            Self::with_config(
                api_key,
                LlmExtractorConfig {
                    schema,
                    instruction,
                    prompt_template,
                    ..LlmExtractorConfig::new(model)
                },
            )
        }

        /// Create an extractor from a full [`LlmExtractorConfig`].
        ///
        /// The configured `max_in_flight` bound is enforced by a queueing
        /// `InFlightLimitLayer` inside the managed client, so it caps provider
        /// requests globally for this extractor's client rather than per call.
        ///
        /// # Errors
        ///
        /// Returns [`CrawlError::InvalidConfig`] when `max_in_flight` is `Some(0)`:
        /// a bound of zero admits no request at all and would deadlock every
        /// extraction. Use `None` to disable the bound.
        pub fn with_config(api_key: &str, config: LlmExtractorConfig) -> Result<Self, CrawlError> {
            if config.max_in_flight == Some(0) {
                return Err(CrawlError::invalid_config(
                    "llm max_in_flight must be greater than zero; use None for an unlimited bound",
                ));
            }

            let mut client_config = liter_llm::ClientConfig::new(api_key);
            client_config.base_url = config.base_url.clone();
            client_config.in_flight_limit_config = Some(liter_llm::InFlightLimitConfig {
                max_in_flight: config.max_in_flight,
            });
            client_config.cache_config = config.response_cache.as_ref().map(|cache| liter_llm::CacheConfig {
                max_entries: cache.max_entries,
                ttl: cache.ttl,
                backend: liter_llm::CacheBackend::Memory,
            });

            let client = liter_llm::ManagedClient::new(client_config, Some(&config.model))
                .map_err(|e| CrawlError::other(format!("failed to create LLM client: {e}")))?;
            Ok(Self { client, config })
        }
    }

    #[async_trait]
    impl ContentFilter for LlmExtractor {
        async fn filter(&self, mut page: CrawlPageResult) -> Result<Option<CrawlPageResult>, CrawlError> {
            use liter_llm::LlmClient;

            let content = page.markdown.as_ref().map(|m| m.content.as_str()).unwrap_or(&page.html);

            let content = truncate_to_char_boundary(content, MAX_CONTENT_CHARS);

            let mut env = minijinja::Environment::new();
            let template_str = self
                .config
                .prompt_template
                .as_deref()
                .unwrap_or(DEFAULT_EXTRACTION_TEMPLATE);
            env.add_template("prompt", template_str)
                .map_err(|e| CrawlError::other(format!("template error: {e}")))?;
            let tmpl = env.get_template("prompt").expect("template was just added above");

            let rendered = tmpl
                .render(minijinja::context! {
                    content => content,
                    schema => self.config.schema.as_ref().map(|s| serde_json::to_string_pretty(s).unwrap_or_default()),
                    instruction => self.config.instruction.as_deref(),
                    url => &page.url,
                    title => page.metadata.title.as_deref(),
                })
                .map_err(|e| CrawlError::other(format!("template render error: {e}")))?;

            let request = liter_llm::ChatCompletionRequest {
                model: self.config.model.clone(),
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
                response_format: self.config.schema.as_ref().map(|s| liter_llm::ResponseFormat::JsonSchema {
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
                model: Some(self.config.model.clone()),
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

    #[cfg(all(test, not(target_arch = "wasm32")))]
    mod tests {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        use super::*;

        const PROVIDER_BODY: &str = concat!(
            r#"{"id":"chatcmpl-test","object":"chat.completion","created":1700000000,"#,
            r#""model":"gpt-4o-mini","choices":[{"index":0,"logprobs":null,"finish_reason":"stop","#,
            r#""message":{"role":"assistant","content":"{\"extracted\":true}"}}],"#,
            r#""usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        );

        /// Concurrency observed by the fake provider, sampled around the response delay.
        #[derive(Default)]
        struct ProviderStats {
            in_flight: AtomicUsize,
            peak: AtomicUsize,
            total: AtomicUsize,
        }

        /// Drain one complete HTTP/1.1 request so the client never sees a reset socket.
        async fn read_request(socket: &mut TcpStream) -> Option<()> {
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..read]);
                if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
            let content_length = headers
                .split("\r\n")
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                let read = socket.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..read]);
            }
            Some(())
        }

        /// Start a local OpenAI-compatible endpoint that holds every request open for `delay`.
        async fn spawn_fake_provider(delay: Duration) -> (String, Arc<ProviderStats>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake provider");
            let address = listener.local_addr().expect("fake provider address");
            let stats = Arc::new(ProviderStats::default());
            let server_stats = Arc::clone(&stats);

            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let stats = Arc::clone(&server_stats);
                    tokio::spawn(async move {
                        if read_request(&mut socket).await.is_none() {
                            return;
                        }
                        let active = stats.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        stats.peak.fetch_max(active, Ordering::SeqCst);
                        stats.total.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(delay).await;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PROVIDER_BODY}",
                            PROVIDER_BODY.len(),
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        stats.in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });

            (format!("http://{address}/v1"), stats)
        }

        fn page_with_content(content: &str) -> CrawlPageResult {
            CrawlPageResult {
                url: format!("https://example.com/{content}"),
                html: content.to_owned(),
                ..CrawlPageResult::default()
            }
        }

        fn extractor_for(base_url: String, max_in_flight: Option<usize>) -> LlmExtractor {
            LlmExtractor::with_config(
                "test-key",
                LlmExtractorConfig {
                    max_in_flight,
                    base_url: Some(base_url),
                    ..LlmExtractorConfig::new("gpt-4o-mini")
                },
            )
            .expect("extractor should build")
        }

        async fn extract_all(extractor: &LlmExtractor, count: usize) {
            let calls = (0..count).map(|index| extractor.filter(page_with_content(&format!("page-{index}"))));
            for outcome in futures::future::join_all(calls).await {
                outcome.expect("extraction should succeed");
            }
        }

        #[tokio::test]
        async fn should_cap_simultaneous_provider_requests_at_max_in_flight() {
            let (base_url, stats) = spawn_fake_provider(Duration::from_millis(150)).await;
            let extractor = extractor_for(base_url, Some(2));

            extract_all(&extractor, 6).await;

            assert_eq!(
                stats.total.load(Ordering::SeqCst),
                6,
                "every page should reach the provider"
            );
            assert_eq!(
                stats.peak.load(Ordering::SeqCst),
                2,
                "provider observed more concurrent requests than the configured bound",
            );
        }

        #[tokio::test]
        async fn should_leave_provider_requests_unbounded_when_max_in_flight_is_none() {
            let (base_url, stats) = spawn_fake_provider(Duration::from_millis(250)).await;
            let extractor = extractor_for(base_url, None);

            extract_all(&extractor, 6).await;

            assert_eq!(stats.total.load(Ordering::SeqCst), 6);
            assert_eq!(
                stats.peak.load(Ordering::SeqCst),
                6,
                "a `None` bound must not serialise provider requests",
            );
        }

        #[tokio::test]
        async fn should_serve_cache_hits_without_waiting_for_an_in_flight_permit() {
            let (base_url, stats) = spawn_fake_provider(Duration::from_millis(400)).await;
            let extractor = Arc::new(
                LlmExtractor::with_config(
                    "test-key",
                    LlmExtractorConfig {
                        max_in_flight: Some(1),
                        response_cache: Some(LlmResponseCacheConfig::default()),
                        base_url: Some(base_url),
                        ..LlmExtractorConfig::new("gpt-4o-mini")
                    },
                )
                .expect("extractor should build"),
            );

            extractor
                .filter(page_with_content("cached"))
                .await
                .expect("priming extraction should succeed");
            assert_eq!(stats.total.load(Ordering::SeqCst), 1);

            let blocker = tokio::spawn({
                let extractor = Arc::clone(&extractor);
                async move { extractor.filter(page_with_content("uncached-a")).await }
            });
            let queued = tokio::spawn({
                let extractor = Arc::clone(&extractor);
                async move { extractor.filter(page_with_content("uncached-b")).await }
            });
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(
                stats.in_flight.load(Ordering::SeqCst),
                1,
                "only one uncached extraction should hold the single permit",
            );
            assert_eq!(
                stats.total.load(Ordering::SeqCst),
                2,
                "the second uncached extraction should still be queued behind the limiter",
            );

            let started = Instant::now();
            extractor
                .filter(page_with_content("cached"))
                .await
                .expect("cached extraction should succeed");
            let cache_hit_latency = started.elapsed();

            assert!(
                cache_hit_latency < Duration::from_millis(150),
                "cache hit took {cache_hit_latency:?}; it queued behind the in-flight limiter",
            );
            assert_eq!(
                stats.total.load(Ordering::SeqCst),
                2,
                "a cache hit must not issue a provider request",
            );

            blocker
                .await
                .expect("blocker task should not panic")
                .expect("uncached extraction should succeed");
            queued
                .await
                .expect("queued task should not panic")
                .expect("queued extraction should succeed");
            assert_eq!(
                stats.total.load(Ordering::SeqCst),
                3,
                "both uncached extractions plus the priming call should reach the provider",
            );
            assert_eq!(
                stats.peak.load(Ordering::SeqCst),
                1,
                "the single permit must never be exceeded",
            );
        }

        #[test]
        fn should_reject_a_zero_max_in_flight_as_invalid_config() {
            let result = LlmExtractor::with_config(
                "test-key",
                LlmExtractorConfig {
                    max_in_flight: Some(0),
                    ..LlmExtractorConfig::new("gpt-4o-mini")
                },
            );

            let error = match result {
                Ok(_) => panic!("a zero bound must be rejected"),
                Err(error) => error,
            };
            assert!(
                matches!(error, CrawlError::InvalidConfig { .. }),
                "unexpected error: {error:?}"
            );
            assert!(
                error.to_string().contains("max_in_flight"),
                "unexpected message: {error}"
            );
        }

        #[test]
        fn should_reject_a_zero_bound_in_the_underlying_limiter() {
            let layer =
                liter_llm::tower::InFlightLimitLayer::new(liter_llm::InFlightLimitConfig { max_in_flight: Some(0) });

            assert!(layer.is_err(), "liter-llm must keep treating a zero bound as invalid");
        }
    }
}
