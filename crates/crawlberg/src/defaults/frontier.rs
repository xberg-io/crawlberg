//! In-memory frontier backed by a `VecDeque` and `AHashSet`.

use std::collections::VecDeque;
use std::sync::Mutex;

use ahash::AHashSet;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::{Frontier, FrontierEntry};

/// A simple in-memory URL frontier with deduplication.
///
/// Uses BFS ordering (FIFO) by default.
#[derive(Debug)]
pub struct InMemoryFrontier {
    queue: Mutex<VecDeque<FrontierEntry>>,
    seen: Mutex<AHashSet<String>>,
}

impl InMemoryFrontier {
    /// Create a new empty `InMemoryFrontier`.
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            seen: Mutex::new(AHashSet::new()),
        }
    }
}

impl Default for InMemoryFrontier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Frontier for InMemoryFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.queue.lock().expect("lock poisoned").push_back(entry);
        Ok(())
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        Ok(self.queue.lock().expect("lock poisoned").pop_front())
    }

    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        let mut queue = self.queue.lock().expect("lock poisoned");
        let mut batch = Vec::with_capacity(n.min(queue.len()));
        for _ in 0..n {
            match queue.pop_front() {
                Some(entry) => batch.push(entry),
                None => break,
            }
        }
        Ok(batch)
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        Ok(self.seen.lock().expect("lock poisoned").contains(url))
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.seen.lock().expect("lock poisoned").insert(url.to_owned());
        Ok(())
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        Ok(self.queue.lock().expect("lock poisoned").len())
    }

    fn isolated(&self) -> Option<std::sync::Arc<dyn Frontier>> {
        Some(std::sync::Arc::new(InMemoryFrontier::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::FrontierEntry;

    #[tokio::test]
    async fn test_push_pop_fifo_order() {
        let f = InMemoryFrontier::new();
        f.push(FrontierEntry {
            url: "a".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();
        f.push(FrontierEntry {
            url: "b".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();
        f.push(FrontierEntry {
            url: "c".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();

        assert_eq!(f.pop().await.unwrap().unwrap().url, "a");
        assert_eq!(f.pop().await.unwrap().unwrap().url, "b");
        assert_eq!(f.pop().await.unwrap().unwrap().url, "c");
    }

    #[tokio::test]
    async fn test_pop_empty_returns_none() {
        let f = InMemoryFrontier::new();
        assert!(f.pop().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_is_seen_mark_seen() {
        let f = InMemoryFrontier::new();
        assert!(!f.is_seen("url1").await.unwrap());
        f.mark_seen("url1").await.unwrap();
        assert!(f.is_seen("url1").await.unwrap());
        assert!(!f.is_seen("url2").await.unwrap());
    }

    #[tokio::test]
    async fn test_len() {
        let f = InMemoryFrontier::new();
        assert_eq!(f.len().await.unwrap(), 0);
        f.push(FrontierEntry {
            url: "a".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();
        assert_eq!(f.len().await.unwrap(), 1);
        f.push(FrontierEntry {
            url: "b".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();
        assert_eq!(f.len().await.unwrap(), 2);
        f.pop().await.unwrap();
        assert_eq!(f.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_pop_batch() {
        let f = InMemoryFrontier::new();
        for i in 0..5 {
            f.push(FrontierEntry {
                url: format!("url{i}"),
                depth: 0,
                doc_depth: 0,
                priority: 1.0,
            })
            .await
            .unwrap();
        }
        let batch = f.pop_batch(3).await.unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].url, "url0");
        assert_eq!(batch[2].url, "url2");
        assert_eq!(f.len().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_isolated_returns_fresh_instance_with_empty_seen_set() {
        let f = InMemoryFrontier::new();
        f.mark_seen("https://example.com/seen").await.unwrap();
        assert!(f.is_seen("https://example.com/seen").await.unwrap());

        let fresh = f.isolated().expect("InMemoryFrontier must support isolation");
        assert!(
            !fresh.is_seen("https://example.com/seen").await.unwrap(),
            "a fresh isolated frontier must not inherit the parent's seen set"
        );

        fresh.mark_seen("https://example.com/only-in-fresh").await.unwrap();
        assert!(
            !f.is_seen("https://example.com/only-in-fresh").await.unwrap(),
            "marking the fresh frontier must not leak back into the parent"
        );
    }

    #[tokio::test]
    async fn test_is_empty() {
        let f = InMemoryFrontier::new();
        assert!(f.is_empty().await.unwrap());
        f.push(FrontierEntry {
            url: "a".into(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        })
        .await
        .unwrap();
        assert!(!f.is_empty().await.unwrap());
    }
}
