//! In-memory frontier backed by a `VecDeque` and `AHashSet`.

use std::collections::VecDeque;
use std::sync::Mutex;

use ahash::AHashSet;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::{Frontier, FrontierEntry};

/// A simple in-memory URL frontier with deduplication.
///
/// FIFO: entries are popped in the order they were pushed, which makes the crawl
/// breadth-first. This is the engine's default frontier. For a depth-first crawl use
/// [`LifoFrontier`] instead — the frontier, not the [`CrawlStrategy`], decides global
/// traversal order.
///
/// [`CrawlStrategy`]: crate::traits::CrawlStrategy
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

/// An in-memory URL frontier with deduplication that pops the most recently pushed entry.
///
/// LIFO, which makes the crawl depth-first: the children of a page are visited before its
/// remaining siblings. Pair it with the engine to get a depth-first traversal —
/// [`DfsStrategy`] only reorders the engine's bounded selection window and cannot by itself
/// produce a globally depth-first crawl.
///
/// [`DfsStrategy`]: crate::defaults::DfsStrategy
#[derive(Debug)]
pub struct LifoFrontier {
    queue: Mutex<VecDeque<FrontierEntry>>,
    seen: Mutex<AHashSet<String>>,
}

impl LifoFrontier {
    /// Create a new empty `LifoFrontier`.
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            seen: Mutex::new(AHashSet::new()),
        }
    }
}

impl Default for LifoFrontier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Frontier for LifoFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.queue.lock().expect("lock poisoned").push_front(entry);
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
        Some(std::sync::Arc::new(LifoFrontier::new()))
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

#[cfg(test)]
mod lifo_tests {
    use super::{Frontier, FrontierEntry, InMemoryFrontier, LifoFrontier};

    fn entry(url: &str) -> FrontierEntry {
        FrontierEntry {
            url: url.to_owned(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        }
    }

    #[tokio::test]
    async fn should_pop_most_recently_pushed_entry_first() {
        let frontier = LifoFrontier::new();
        for url in ["a", "b", "c"] {
            frontier.push(entry(url)).await.expect("push must succeed");
        }

        let mut popped = Vec::new();
        while let Some(taken) = frontier.pop().await.expect("pop must succeed") {
            popped.push(taken.url);
        }

        assert_eq!(popped, vec!["c", "b", "a"]);
    }

    #[tokio::test]
    async fn should_pop_batch_in_lifo_order() {
        let frontier = LifoFrontier::new();
        for url in ["a", "b", "c"] {
            frontier.push(entry(url)).await.expect("push must succeed");
        }

        let batch = frontier.pop_batch(3).await.expect("pop_batch must succeed");

        assert_eq!(
            batch.iter().map(|e| e.url.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
    }

    #[tokio::test]
    async fn should_isolate_seen_state_per_crawl() {
        let frontier = LifoFrontier::new();
        frontier
            .mark_seen("https://example.com/")
            .await
            .expect("mark must succeed");

        let fresh = frontier.isolated().expect("LifoFrontier must isolate per call");

        assert!(
            !fresh
                .is_seen("https://example.com/")
                .await
                .expect("is_seen must succeed"),
            "a fresh instance must not inherit the parent's seen set"
        );
    }

    /// A short batch means "empty right now", which is how the crawl loop detects a drained
    /// frontier without an extra async `len` call. It must never be an error.
    #[tokio::test]
    async fn should_return_fewer_entries_than_requested_when_frontier_is_short() {
        for frontier in [
            Box::new(InMemoryFrontier::new()) as Box<dyn Frontier>,
            Box::new(LifoFrontier::new()) as Box<dyn Frontier>,
        ] {
            frontier.push(entry("a")).await.expect("push must succeed");
            frontier.push(entry("b")).await.expect("push must succeed");

            let batch = frontier.pop_batch(10).await.expect("a short batch is not an error");

            assert_eq!(batch.len(), 2);
            assert!(frontier.pop_batch(10).await.expect("must succeed").is_empty());
        }
    }
}
