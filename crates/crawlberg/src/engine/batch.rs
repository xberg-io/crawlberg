//! Batch and streaming crawl operations.
#![allow(dead_code)]

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument as _;

use crate::error::CrawlError;
use crate::telemetry::attributes::CRAWL_SEED_COUNT;
use crate::types::*;

use super::CrawlEngine;

/// Default concurrency limit when `max_concurrent` is not set.
const DEFAULT_MAX_CONCURRENT: usize = 10;

/// Channel buffer multiplier relative to max_concurrent for streaming operations.
const STREAM_BUFFER_MULTIPLIER: usize = 16;

impl CrawlEngine {
    /// Crawl a website starting from `url`, following links up to the configured depth.
    ///
    /// Uses the engine's [`CrawlStrategy`](crate::traits::CrawlStrategy) and
    /// [`Frontier`](crate::traits::Frontier) traits to control URL selection order
    /// and deduplication.
    pub async fn crawl(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        self.crawl_with_sender(url, None).await
    }

    /// Crawl a website and return a stream of events as pages are processed.
    ///
    /// Uses the engine's trait implementations (strategy, frontier, etc.) for the crawl.
    pub fn crawl_stream(&self, url: &str) -> ReceiverStream<CrawlEvent> {
        let url = url.to_owned();
        let engine = self.clone();

        let channel_size = self.config.max_concurrent.unwrap_or(4) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(async move {
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
        });

        ReceiverStream::new(rx)
    }

    /// Scrape multiple URLs concurrently.
    ///
    /// Unlike the standalone `batch::batch_scrape`, this method routes each URL
    /// through the engine's middleware chain, rate limiter, and cache.
    pub async fn batch_scrape(&self, urls: &[&str]) -> Vec<(String, Result<ScrapeResult, CrawlError>)> {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut join_set = JoinSet::new();

        for url in urls {
            let url_owned = url.to_string();
            let engine = self.clone();
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    break;
                }
            };

            join_set.spawn(async move {
                let _permit = permit;
                let result = engine.scrape(&url_owned).await;
                (url_owned, result)
            });
        }

        let mut results = Vec::with_capacity(urls.len());
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(result) => results.push(result),
                Err(e) => results.push((String::new(), Err(CrawlError::Other(format!("task panicked: {e}"))))),
            }
        }
        results
    }

    /// Crawl multiple seed URLs, each following links to configured depth.
    /// Returns results paired with seed URLs as they complete.
    pub async fn batch_crawl(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        let seed_count = urls.len();
        let span = tracing::info_span!("crawl.engine.batch", { CRAWL_SEED_COUNT } = seed_count as i64,);

        self.batch_crawl_inner(urls).instrument(span).await
    }

    async fn batch_crawl_inner(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        let max_concurrent = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut join_set = JoinSet::new();

        for url in urls {
            let url_owned = url.to_string();
            let engine = self.clone();
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };

            join_set.spawn(async move {
                let _permit = permit;
                let result = engine.crawl(&url_owned).await;
                (url_owned, result)
            });
        }

        let mut results = Vec::with_capacity(urls.len());
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push((String::new(), Err(CrawlError::Other(format!("task panicked: {e}")))));
                }
            }
        }
        results
    }

    /// Crawl multiple seed URLs and stream events from all crawls.
    pub fn batch_crawl_stream(&self, urls: &[&str]) -> ReceiverStream<CrawlEvent> {
        let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
        let engine = self.clone();
        let channel_size = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(async move {
            let max_concurrent = engine.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
            let semaphore = Arc::new(Semaphore::new(max_concurrent));
            let mut join_set = JoinSet::new();

            for url in urls {
                let engine = engine.clone();
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
        });

        ReceiverStream::new(rx)
    }
}
