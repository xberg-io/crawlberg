//! Asynchronous job registry for crawl and batch scrape operations.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

use crate::types::{CrawlResult, ScrapeResult};

/// State of an asynchronous job.
#[derive(Debug, Clone)]
pub enum JobState {
    /// Job has been accepted but not yet started.
    Pending {
        /// When the job was created.
        created_at: Instant,
    },
    /// Job is actively processing pages.
    InProgress {
        /// Number of pages completed so far.
        pages_completed: usize,
        /// When the job was created.
        created_at: Instant,
    },
    /// Crawl job completed successfully.
    CrawlCompleted {
        /// The crawl result.
        result: Box<CrawlResult>,
        /// When the job was created.
        created_at: Instant,
    },
    /// Batch scrape job completed successfully.
    BatchCompleted {
        /// The batch results.
        results: Vec<(String, Result<ScrapeResult, String>)>,
        /// When the job was created.
        created_at: Instant,
    },
    /// Job encountered a fatal error.
    Failed {
        /// The error message.
        message: String,
        /// When the job was created.
        created_at: Instant,
    },
    /// Job was cancelled by the user.
    Cancelled {
        /// When the job was created.
        created_at: Instant,
    },
}

impl JobState {
    /// Returns the creation time of this job.
    pub fn created_at(&self) -> Instant {
        match self {
            JobState::Pending { created_at }
            | JobState::InProgress { created_at, .. }
            | JobState::CrawlCompleted { created_at, .. }
            | JobState::BatchCompleted { created_at, .. }
            | JobState::Failed { created_at, .. }
            | JobState::Cancelled { created_at } => *created_at,
        }
    }
}

/// Thread-safe registry of asynchronous jobs.
#[derive(Debug)]
pub struct JobRegistry {
    jobs: DashMap<Uuid, JobState>,
}

impl JobRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { jobs: DashMap::new() }
    }

    /// Register a new job and return its unique identifier.
    pub fn create_job(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.jobs.insert(
            id,
            JobState::Pending {
                created_at: Instant::now(),
            },
        );
        id
    }

    /// Retrieve the current state of a job, if it exists.
    pub fn get_status(&self, id: &Uuid) -> Option<JobState> {
        self.jobs.get(id).map(|entry| entry.value().clone())
    }

    /// Update the state of an existing job.
    ///
    /// Returns `true` if the job existed and was updated, `false` otherwise.
    pub fn update(&self, id: &Uuid, state: JobState) -> bool {
        if let Some(mut entry) = self.jobs.get_mut(id) {
            *entry = state;
            true
        } else {
            false
        }
    }

    /// Attempt to cancel a job.
    ///
    /// Returns `true` if the job existed and was marked as cancelled.
    /// Jobs that are already completed or failed cannot be cancelled.
    pub fn cancel(&self, id: &Uuid) -> bool {
        if let Some(mut entry) = self.jobs.get_mut(id) {
            match entry.value() {
                JobState::Pending { created_at } | JobState::InProgress { created_at, .. } => {
                    let created_at = *created_at;
                    *entry = JobState::Cancelled { created_at };
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    /// Remove all jobs older than `max_age`.
    pub fn evict_expired(&self, max_age: Duration) {
        self.jobs.retain(|_, state| state.created_at().elapsed() < max_age);
    }

    /// Count jobs that are still active (`Pending` or `InProgress`).
    ///
    /// Used to enforce a hard ceiling on concurrently running crawl/batch jobs
    /// before a new one is accepted.
    pub fn active_count(&self) -> usize {
        self.jobs
            .iter()
            .filter(|entry| matches!(entry.value(), JobState::Pending { .. } | JobState::InProgress { .. }))
            .count()
    }

    /// Spawn a background task that periodically evicts expired jobs.
    ///
    /// The task runs every 60 seconds and removes jobs older than `max_age`.
    pub fn spawn_eviction_task(self: &Arc<Self>, max_age: Duration) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                registry.evict_expired(max_age);
            }
        });
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_count_counts_only_pending_and_in_progress_jobs() {
        let registry = JobRegistry::new();
        let pending = registry.create_job();
        let in_progress = registry.create_job();
        let completed = registry.create_job();
        let failed = registry.create_job();

        registry.update(
            &in_progress,
            JobState::InProgress {
                pages_completed: 1,
                created_at: Instant::now(),
            },
        );
        registry.update(
            &completed,
            JobState::CrawlCompleted {
                result: Box::new(CrawlResult::default()),
                created_at: Instant::now(),
            },
        );
        registry.update(
            &failed,
            JobState::Failed {
                message: "boom".to_string(),
                created_at: Instant::now(),
            },
        );

        assert_eq!(
            registry.active_count(),
            2,
            "expected only the pending and in-progress jobs to count as active"
        );

        registry.cancel(&pending);
        assert_eq!(
            registry.active_count(),
            1,
            "cancelling the pending job must drop it from active_count"
        );
    }
}
