//! Batch and streaming crawl operations.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument as _;

use crate::error::CrawlError;
use crate::telemetry::attributes::{CRAWL_SEED_COUNT, URL_FULL};
use crate::types::*;

use super::CrawlEngine;

/// Default concurrency limit when `max_concurrent` is not set.
const DEFAULT_MAX_CONCURRENT: usize = 10;

/// Channel buffer multiplier relative to max_concurrent for streaming operations.
const STREAM_BUFFER_MULTIPLIER: usize = 16;

impl CrawlEngine {
    /// Return a clone of this engine whose frontier is isolated to a single crawl call.
    ///
    /// `CrawlEngine` is designed to be constructed once and reused for many `crawl()`
    /// calls, and `CrawlEngine::clone()` shares the same `Arc<dyn Frontier>` across all
    /// clones. Without per-call isolation, a frontier's `seen` state (e.g. the default
    /// `InMemoryFrontier`) would leak across calls on a reused engine and race under
    /// concurrent `batch_crawl` calls sharing the same `seen` set. Every entry point that
    /// invokes `crawl_with_sender` must route through this helper.
    fn with_isolated_frontier(&self) -> Self {
        let mut engine = self.clone();
        if let Some(fresh) = self.frontier.isolated() {
            engine.frontier = fresh;
        }
        engine
    }

    /// Crawl a website starting from `url`, following links up to the configured depth.
    ///
    /// Uses the engine's [`CrawlStrategy`](crate::traits::CrawlStrategy) and
    /// [`Frontier`](crate::traits::Frontier) traits to control URL selection order
    /// and deduplication.
    #[tracing::instrument(name = "crawl.engine.crawl", skip(self), fields(url.full = tracing::field::Empty))]
    pub async fn crawl(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        let redacted_url = crate::net::redact_url_credentials(url);
        tracing::Span::current().record(URL_FULL, tracing::field::display(&redacted_url));
        self.with_isolated_frontier().crawl_with_sender(url, None).await
    }

    /// Crawl a website and return a stream of events as pages are processed.
    ///
    /// Uses the engine's trait implementations (strategy, frontier, etc.) for the crawl.
    pub fn crawl_stream(&self, url: &str) -> ReceiverStream<CrawlEvent> {
        let redacted_url = crate::net::redact_url_credentials(url);
        let span = tracing::info_span!("crawl.engine.crawl_stream", { URL_FULL } = %redacted_url);
        let url = url.to_owned();
        let engine = self.with_isolated_frontier();

        let channel_size = self.config.max_concurrent.unwrap_or(4) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(
            async move {
                match engine.crawl_with_sender(&url, Some(tx.clone())).await {
                    Ok(_result) => {}
                    Err(e) => {
                        let error_event = CrawlEvent::Error {
                            url: url.clone(),
                            error: e.to_string(),
                        };
                        let _ = tx.send(error_event.clone()).await;
                        if let Some(ref sink) = engine.event_sink {
                            sink.emit(error_event).await;
                        }
                        let complete_event = CrawlEvent::Complete { pages_crawled: 0 };
                        let _ = tx.send(complete_event.clone()).await;
                        if let Some(ref sink) = engine.event_sink {
                            sink.emit(complete_event).await;
                        }
                    }
                }
            }
            .instrument(span),
        );

        ReceiverStream::new(rx)
    }

    /// Run `operation(engine, url)` for each of `urls` with bounded concurrency, pairing
    /// every result with its originating URL — including when a spawned task panics.
    ///
    /// ~keep `JoinError` carries a task id but not the URL the task was processing, so a
    /// naive panic handler loses the URL entirely (returns `String::new()`). Task ids are
    /// recorded at spawn time and used to recover the URL on panic instead.
    async fn run_batch<T, F, Fut>(&self, urls: &[&str], operation: F) -> Vec<(String, Result<T, CrawlError>)>
    where
        F: Fn(CrawlEngine, String) -> Fut,
        Fut: Future<Output = Result<T, CrawlError>> + Send + 'static,
        T: Send + 'static,
    {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut join_set: JoinSet<Result<T, CrawlError>> = JoinSet::new();
        let mut url_by_task: HashMap<tokio::task::Id, String> = HashMap::with_capacity(urls.len());

        for url in urls {
            let url_owned = url.to_string();
            let engine = self.clone();
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };
            let task = operation(engine, url_owned.clone());
            let abort_handle = join_set.spawn(async move {
                let _permit = permit;
                task.await
            });
            url_by_task.insert(abort_handle.id(), url_owned);
        }

        let mut results = Vec::with_capacity(urls.len());
        while let Some(joined) = join_set.join_next_with_id().await {
            let (task_id, outcome) = match joined {
                Ok((id, out)) => (id, out),
                Err(join_error) => {
                    let id = join_error.id();
                    let url = url_by_task.remove(&id).unwrap_or_default();
                    results.push((
                        url.clone(),
                        Err(CrawlError::other(format!(
                            "task panicked while processing {url}: {join_error}"
                        ))),
                    ));
                    continue;
                }
            };
            let url = url_by_task.remove(&task_id).unwrap_or_default();
            results.push((url, outcome));
        }
        results
    }

    /// Scrape multiple URLs concurrently.
    ///
    /// Unlike the standalone `batch::batch_scrape`, this method routes each URL
    /// through the engine's middleware chain, rate limiter, and cache.
    #[tracing::instrument(name = "crawl.engine.batch_scrape", skip(self, urls), fields(url_count = urls.len()))]
    pub async fn batch_scrape(&self, urls: &[&str]) -> Vec<(String, Result<ScrapeResult, CrawlError>)> {
        self.run_batch(urls, |engine, url| async move { engine.scrape(&url).await })
            .await
    }

    /// Crawl multiple seed URLs, each following links to configured depth.
    /// Returns results paired with seed URLs as they complete.
    pub async fn batch_crawl(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        let seed_count = urls.len();
        let span = tracing::info_span!("crawl.engine.batch", { CRAWL_SEED_COUNT } = seed_count as i64,);

        self.batch_crawl_inner(urls).instrument(span).await
    }

    async fn batch_crawl_inner(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        self.run_batch(urls, |engine, url| async move { engine.crawl(&url).await })
            .await
    }

    /// Crawl multiple seed URLs and stream events from all crawls.
    pub fn batch_crawl_stream(&self, urls: &[&str]) -> ReceiverStream<CrawlEvent> {
        let span = tracing::info_span!("crawl.engine.batch_crawl_stream", url_count = urls.len());
        let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
        let engine = self.clone();
        let channel_size = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(
            async move {
                let max_concurrent = engine.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
                let semaphore = Arc::new(Semaphore::new(max_concurrent));
                let mut join_set = JoinSet::new();

                for url in urls {
                    let engine = engine.with_isolated_frontier();
                    let tx = tx.clone();
                    let permit = match semaphore.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };

                    join_set.spawn(async move {
                        let _permit = permit;
                        match engine.crawl_with_sender(&url, Some(tx.clone())).await {
                            Ok(_result) => {}
                            Err(e) => {
                                let error_event = CrawlEvent::Error {
                                    url: url.clone(),
                                    error: e.to_string(),
                                };
                                let _ = tx.send(error_event.clone()).await;
                                if let Some(ref sink) = engine.event_sink {
                                    sink.emit(error_event).await;
                                }
                            }
                        }
                    });
                }

                while join_set.join_next().await.is_some() {}
            }
            .instrument(span),
        );

        ReceiverStream::new(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: a panicking task must not lose the URL it was processing.
    /// Previously, `join_set.join_next()` returning `Err(JoinError)` on panic pushed
    /// `(String::new(), Err(...))`, discarding the URL entirely.
    #[tokio::test]
    async fn run_batch_preserves_url_when_task_panics() {
        let engine = CrawlEngine::builder().build().expect("engine build must not fail");

        let results = engine
            .run_batch(
                &["https://ok.example/", "https://panics.example/"],
                |_engine, url| async move {
                    if url == "https://panics.example/" {
                        panic!("simulated task panic");
                    }
                    Ok::<u32, CrawlError>(42)
                },
            )
            .await;

        assert_eq!(results.len(), 2, "expected one result per URL, got {results:?}");

        let ok_result = results
            .iter()
            .find(|(url, _)| url == "https://ok.example/")
            .expect("ok URL must be present in results");
        assert_eq!(ok_result.1.as_ref().copied().ok(), Some(42));

        let panic_result = results
            .iter()
            .find(|(url, _)| url == "https://panics.example/")
            .unwrap_or_else(|| panic!("panicking task must still report its URL, got: {results:?}"));
        let error_msg = panic_result
            .1
            .as_ref()
            .expect_err("panicking task must return Err")
            .to_string();
        assert!(
            error_msg.contains("https://panics.example/"),
            "panic error must mention the URL that was being processed, got: {error_msg}"
        );
    }
}
