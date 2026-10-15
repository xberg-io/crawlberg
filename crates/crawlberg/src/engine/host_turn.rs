use std::future::Future;
use std::sync::Arc;

use super::cooperative::{ActivityGuard, CooperativeContext, HostWait, admit_host};
use crate::error::CrawlError;
use crate::traits::RateLimiter;

pub(super) struct HostJob<J> {
    pub(super) host: String,
    pub(super) data: J,
}

pub(super) enum HostStep<J, T> {
    Continue(HostJob<J>),
    Done(T),
}

pub(super) enum TurnError {
    Failed(CrawlError),
    Panicked,
}

impl TurnError {
    pub(super) fn into_error(self, url: &str) -> CrawlError {
        match self {
            Self::Failed(error) => error,
            Self::Panicked => CrawlError::other(format!("task panicked while processing {url}")),
        }
    }
}

pub(super) struct HostGrant {
    pub(super) limiter: Arc<dyn RateLimiter>,
    pub(super) base: Arc<dyn RateLimiter>,
    pub(super) activity: Option<Arc<ActivityGuard>>,
}

struct QueuedTurn<J> {
    id: usize,
    data: J,
    waiting: Option<HostWait>,
    context: Option<Arc<CooperativeContext>>,
    limiter: Arc<dyn RateLimiter>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct HostKey {
    host: String,
    limiter: usize,
}

struct CompletedTurn {
    key: HostKey,
    result: Result<Option<Arc<ActivityGuard>>, TurnError>,
}

type TurnFuture = std::pin::Pin<Box<dyn Future<Output = CompletedTurn> + Send>>;

// ~keep Host waits live in this backlog, not in one spawned URL task each. There is one
// acquire future per pending host and transport limiter; the caller separately bounds active hop futures.
// Batch backlog is bounded by its input count; crawl backlog carries the existing page budget.
pub(super) struct HostTurns<J> {
    queues: std::collections::HashMap<HostKey, std::collections::VecDeque<QueuedTurn<J>>>,
    // ~keep Head data stays outside cancellable limiter futures so shutdown can recover parked continuations.
    heads: std::collections::HashMap<HostKey, QueuedTurn<J>>,
    turns: futures::stream::FuturesUnordered<TurnFuture>,
    limiter: Arc<dyn RateLimiter>,
    context: Option<Arc<CooperativeContext>>,
}

impl<J: Send + 'static> HostTurns<J> {
    pub(super) fn new(limiter: Arc<dyn RateLimiter>) -> Self {
        Self {
            queues: Default::default(),
            heads: Default::default(),
            turns: Default::default(),
            limiter,
            context: None,
        }
    }

    pub(super) fn with_context(limiter: Arc<dyn RateLimiter>, context: Option<Arc<CooperativeContext>>) -> Self {
        Self {
            context,
            ..Self::new(limiter)
        }
    }

    pub(super) fn push(&mut self, id: usize, job: HostJob<J>) {
        self.enqueue(id, job, self.context.clone(), self.limiter.clone());
    }

    #[cfg(test)]
    pub(super) fn push_with_context(&mut self, id: usize, job: HostJob<J>, context: Arc<CooperativeContext>) {
        self.enqueue(id, job, Some(context), self.limiter.clone());
    }

    pub(super) fn push_with_limiter(&mut self, id: usize, job: HostJob<J>, limiter: Arc<dyn RateLimiter>) {
        self.enqueue(id, job, self.context.clone(), limiter);
    }

    pub(super) fn push_with_context_and_limiter(
        &mut self,
        id: usize,
        job: HostJob<J>,
        context: Arc<CooperativeContext>,
        limiter: Arc<dyn RateLimiter>,
    ) {
        self.enqueue(id, job, Some(context), limiter);
    }

    fn enqueue(
        &mut self,
        id: usize,
        job: HostJob<J>,
        context: Option<Arc<CooperativeContext>>,
        limiter: Arc<dyn RateLimiter>,
    ) {
        // ~keep Non-head jobs must park their own context before the shared host waiter reaches them.
        let waiting = context.as_ref().map(|context| context.waiting());
        // ~keep Queued/head entries retain this Arc until its identity leaves every keyed map.
        let key = HostKey {
            host: job.host,
            limiter: Arc::as_ptr(&limiter) as *const () as usize,
        };
        let queued = QueuedTurn {
            id,
            data: job.data,
            waiting,
            context,
            limiter,
        };
        if self.heads.contains_key(&key) {
            self.queues.entry(key).or_default().push_back(queued);
        } else {
            self.start_waiter(key, queued);
        }
    }

    pub(super) fn blocked_hosts(&self) -> Vec<String> {
        self.heads.keys().map(|key| key.host.clone()).collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    fn start_waiter(&mut self, key: HostKey, queued: QueuedTurn<J>) {
        let limiter = queued.limiter.clone();
        let context = queued.context.clone();
        self.heads.insert(key.clone(), queued);
        self.turns.push(Box::pin(async move {
            use futures::FutureExt;
            let result = match std::panic::AssertUnwindSafe(async {
                match context {
                    Some(context) => admit_host(context, limiter, key.host.clone()).await.map(Some),
                    None => limiter.acquire(&key.host).await.map(|()| None),
                }
            })
            .catch_unwind()
            .await
            {
                Ok(result) => result.map_err(TurnError::Failed),
                Err(_) => Err(TurnError::Panicked),
            };
            CompletedTurn { key, result }
        }));
    }

    pub(super) async fn next(&mut self) -> Option<(usize, J, Result<HostGrant, TurnError>)> {
        use futures::StreamExt;
        let CompletedTurn { key, result } = self.turns.next().await?;
        let queued = self.heads.remove(&key).expect("pending host owns its queue head");
        let QueuedTurn {
            id,
            data,
            waiting,
            limiter,
            ..
        } = queued;
        if let Some(queue) = self.queues.get_mut(&key) {
            let next = queue.pop_front().expect("a queued host retains its next turn");
            if queue.is_empty() {
                self.queues.remove(&key);
            }
            self.start_waiter(key.clone(), next);
        }
        let grant = result.map(|activity| HostGrant {
            limiter: Arc::new(GrantedTurn::new(key.host, limiter.clone())),
            base: limiter,
            activity,
        });
        drop(waiting);
        Some((id, data, grant))
    }

    pub(super) fn drain(&mut self) -> Vec<(usize, J)> {
        self.turns = Default::default();
        self.heads
            .drain()
            .map(|(_, queued)| queued)
            .chain(self.queues.drain().flat_map(|(_, queued)| queued))
            .map(|queued| (queued.id, queued.data))
            .collect()
    }
}

pub(super) struct GrantedTurn {
    host: String,
    available: std::sync::atomic::AtomicBool,
    inner: Arc<dyn RateLimiter>,
}

impl GrantedTurn {
    pub(super) fn new(host: String, inner: Arc<dyn RateLimiter>) -> Self {
        Self {
            host,
            available: std::sync::atomic::AtomicBool::new(true),
            inner,
        }
    }
}

#[async_trait::async_trait]
impl RateLimiter for GrantedTurn {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        if domain == self.host && self.available.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.acquire(domain).await
    }

    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
        self.inner.record_response(domain, status).await
    }

    async fn set_crawl_delay(&self, domain: &str, delay: std::time::Duration) -> Result<(), CrawlError> {
        self.inner.set_crawl_delay(domain, delay).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct RecordingLimiter {
        acquisitions: std::sync::Mutex<Vec<String>>,
        responses: std::sync::Mutex<Vec<(String, u16)>>,
    }

    #[async_trait::async_trait]
    impl RateLimiter for RecordingLimiter {
        async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
            self.acquisitions.lock().unwrap().push(domain.into());
            if domain == "refused" {
                return Err(CrawlError::other("custom admission refused"));
            }
            Ok(())
        }
        async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
            self.responses.lock().unwrap().push((domain.into(), status));
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    type BenchmarkPayload = (usize, super::super::CrawlEngine, super::super::redirect::RedirectCursor);

    fn host_benchmark_jobs(
        engine: &super::super::CrawlEngine,
        hosts: usize,
        repeats: usize,
    ) -> (Arc<RecordingLimiter>, Vec<HostJob<BenchmarkPayload>>) {
        let jobs = (0..hosts * repeats)
            .map(|id| {
                let host = format!("host{}.example.test", id % hosts);
                let url = format!("http://{host}/{id}");
                HostJob {
                    host,
                    data: (
                        id,
                        engine.clone(),
                        super::super::redirect::RedirectCursor::new(&url, engine.config.max_redirects),
                    ),
                }
            })
            .collect::<Vec<_>>();
        (Arc::new(RecordingLimiter::default()), jobs)
    }

    async fn drive_host_benchmark(
        limiter: Arc<RecordingLimiter>,
        jobs: Vec<HostJob<BenchmarkPayload>>,
        expected: usize,
    ) {
        let mut turns = HostTurns::new(limiter.clone());
        for (id, job) in jobs.into_iter().enumerate() {
            turns.push(id, job);
        }
        let mut seen = vec![false; expected];
        while let Some((id, data, grant)) = turns.next().await {
            assert_eq!(id, data.0);
            assert!(!seen[id]);
            seen[id] = true;
            assert!(grant.is_ok());
            std::hint::black_box(data);
        }
        assert_eq!(seen.into_iter().filter(|seen| *seen).count(), expected);
        assert_eq!(limiter.acquisitions.lock().unwrap().len(), expected);
    }

    #[test]
    #[ignore = "manual Criterion comparison of host scheduling independently of HTTP transport"]
    fn measure_host_turn_backlog() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let engine = super::super::CrawlEngine::builder().build().unwrap();
        let output = std::env::var("HOST_BENCH_OUTPUT").expect("benchmark output directory is required");
        assert!(
            !output.trim().is_empty(),
            "benchmark output directory must not be empty"
        );
        let mut criterion = criterion::Criterion::default()
            .sample_size(20)
            .warm_up_time(Duration::from_millis(100))
            .measurement_time(Duration::from_millis(500))
            .output_directory(std::path::Path::new(&output));
        let baseline = std::env::var("HOST_BENCH_BASELINE").unwrap();
        criterion = if baseline == "b24" {
            criterion.save_baseline(baseline)
        } else {
            criterion.retain_baseline("b24".into(), true)
        };
        let mut group = criterion.benchmark_group("host_turn_dispatch");
        for hosts in [250, 500, 1_000, 2_000] {
            for repeats in [1, 3] {
                group.bench_function(format!("{hosts}x{repeats}"), |bench| {
                    bench.iter_batched(
                        || host_benchmark_jobs(&engine, hosts, repeats),
                        |(limiter, jobs)| runtime.block_on(drive_host_benchmark(limiter, jobs, hosts * repeats)),
                        criterion::BatchSize::PerIteration,
                    );
                });
            }
        }
        group.finish();
        criterion.final_summary();
    }

    #[tokio::test]
    async fn should_forward_granted_turn_feedback_to_the_original_custom_limiter() {
        let limiter = Arc::new(RecordingLimiter::default());
        let mut turns = HostTurns::new(limiter.clone());
        turns.push(
            0,
            HostJob {
                host: "free".into(),
                data: (),
            },
        );
        let (_, _, grant) = turns.next().await.unwrap();
        let grant = grant.ok().expect("custom admission succeeds");
        grant.limiter.acquire("free").await.unwrap();
        grant.limiter.record_response("free", 429).await.unwrap();
        assert_eq!(*limiter.acquisitions.lock().unwrap(), ["free"]);
        assert_eq!(*limiter.responses.lock().unwrap(), [("free".into(), 429)]);
    }

    #[tokio::test]
    async fn should_apply_custom_admission_once_and_forward_response_recording() {
        let limiter = Arc::new(RecordingLimiter::default());
        let jobs = ["refused", "free"]
            .into_iter()
            .map(|host| HostJob {
                host: host.into(),
                data: host,
            })
            .collect::<Vec<_>>();
        let mut results = super::super::continuation::run_resumable_jobs(
            jobs,
            2,
            limiter.clone(),
            |id| format!("job {id}"),
            |host, limiter| async move {
                let limiter = limiter.primary();
                limiter.acquire(host).await?;
                limiter.record_response(host, 200).await?;
                Ok(HostStep::Done(host))
            },
        )
        .await;
        results.sort_by_key(|(context, _)| ["job refused", "job free"].iter().position(|c| c == context));
        assert_eq!(results.len(), 2);
        assert!(
            matches!(results[0].1.as_ref().unwrap_err(), CrawlError::Other { message, .. } if message == "custom admission refused")
        );
        assert_eq!(*results[1].1.as_ref().unwrap(), "free");
        let mut acquired = limiter.acquisitions.lock().unwrap().clone();
        acquired.sort();
        assert_eq!(acquired, ["free", "refused"]);
        assert_eq!(*limiter.responses.lock().unwrap(), [("free".into(), 200)]);
    }

    #[tokio::test]
    async fn should_return_every_input_after_admission_errors_operation_errors_and_panics() {
        const INPUTS: usize = 1_000;
        let limiter = Arc::new(RecordingLimiter::default());
        let jobs = (0..INPUTS)
            .map(|id| HostJob {
                host: if id == 0 { "refused".into() } else { format!("free{id}") },
                data: id,
            })
            .collect::<Vec<_>>();
        let mut results = super::super::continuation::run_resumable_jobs(
            jobs,
            4,
            limiter.clone(),
            |id| format!("input{id}"),
            |id, routing| async move {
                routing.primary().acquire(&format!("free{id}")).await?;
                match id {
                    500 => Err(CrawlError::other("page operation refused")),
                    501 => panic!("page operation panic"),
                    _ => Ok(HostStep::Done(id)),
                }
            },
        )
        .await;
        results.sort_by_key(|(context, _)| context.trim_start_matches("input").parse::<usize>().unwrap());
        assert_eq!(results.len(), INPUTS);
        for (expected, (context, result)) in results.iter().enumerate() {
            assert_eq!(*context, format!("input{expected}"));
            let error = match expected {
                0 => Some("other: custom admission refused"),
                500 => Some("other: page operation refused"),
                501 => Some("other: task panicked while processing input501"),
                _ => None,
            };
            if let Some(error) = error {
                assert_eq!(result.as_ref().unwrap_err().to_string(), error);
            } else {
                assert_eq!(*result.as_ref().unwrap(), expected);
            }
        }
        assert_eq!(limiter.acquisitions.lock().unwrap().len(), INPUTS);
    }

    #[tokio::test]
    async fn should_preserve_each_hosts_fifo_order_across_a_large_mixed_backlog() {
        const HOSTS: usize = 1_000;
        const TURNS_PER_HOST: usize = 3;
        let limiter = Arc::new(RecordingLimiter::default());
        let mut turns = HostTurns::new(limiter.clone());
        for id in 0..HOSTS * TURNS_PER_HOST {
            turns.push(
                id,
                HostJob {
                    host: format!("host{}", id % HOSTS),
                    data: id,
                },
            );
        }
        let mut next = vec![0; HOSTS];
        for _ in 0..HOSTS * TURNS_PER_HOST {
            let (id, data, grant) = turns.next().await.expect("every input retains its host turn");
            let host = id % HOSTS;
            assert_eq!(data, id);
            assert_eq!(id / HOSTS, next[host], "one host retains FIFO admission");
            next[host] += 1;
            grant
                .ok()
                .expect("admission succeeds")
                .limiter
                .acquire(&format!("host{host}"))
                .await
                .unwrap();
        }
        assert_eq!(next, vec![TURNS_PER_HOST; HOSTS]);
        assert_eq!(limiter.acquisitions.lock().unwrap().len(), HOSTS * TURNS_PER_HOST);
        assert!(turns.next().await.is_none());
    }

    #[tokio::test]
    async fn should_park_each_queued_seed_context_before_its_host_reaches_the_queue_head() {
        let slots = Arc::new(tokio::sync::Semaphore::new(2));
        let contexts = (0..2)
            .map(|_| CooperativeContext::new(slots.clone()))
            .collect::<Vec<_>>();
        for context in &contexts {
            drop(context.activity().await.unwrap());
        }
        assert_eq!(slots.available_permits(), 0);
        let limiter = Arc::new(RecordingLimiter::default());
        let mut turns = HostTurns::new(limiter.clone());
        for (id, context) in contexts.iter().enumerate() {
            turns.push_with_context(
                id,
                HostJob {
                    host: "same".into(),
                    data: id,
                },
                context.clone(),
            );
        }
        assert_eq!(
            slots.available_permits(),
            2,
            "non-head waits must also park their seed context"
        );
        let mut grants = Vec::new();
        for id in 0..2 {
            let (actual_id, data, grant) = turns.next().await.unwrap();
            assert_eq!((actual_id, data), (id, id));
            let grant = grant.ok().expect("custom host admission succeeds");
            assert!(grant.activity.is_some());
            grant.limiter.acquire("same").await.unwrap();
            grants.push(grant);
        }
        assert_eq!(slots.available_permits(), 0);
        assert_eq!(*limiter.acquisitions.lock().unwrap(), ["same", "same"]);
        for context in &contexts {
            context.complete();
        }
        drop(grants);
        assert_eq!(slots.available_permits(), 2);
        assert!(turns.is_empty());
    }

    #[tokio::test]
    async fn should_not_classify_unpolled_or_zero_delay_turns_as_unavailable_hosts() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let progress = Arc::new(super::super::cooperative::SeedProgress::default());
        let limiter = Arc::new(RecordingLimiter::default());
        let mut engine = super::super::CrawlEngine::builder().build().unwrap();
        engine.rate_limiter = limiter.clone();
        let context = CooperativeContext::for_batch(slots.clone(), &engine, progress.clone());
        let mut turns = HostTurns::with_context(limiter.clone(), Some(context.clone()));
        turns.push(
            0,
            HostJob {
                host: "same".into(),
                data: "original",
            },
        );
        assert!(!progress.unavailable("same"));
        let (id, data, grant) = turns.next().await.unwrap();
        let grant = grant.ok().expect("zero-delay host is admitted");
        assert_eq!((id, data), (0, "original"));
        assert!(!progress.unavailable("same"));
        grant.limiter.acquire("same").await.unwrap();
        assert_eq!(*limiter.acquisitions.lock().unwrap(), ["same"]);
        context.complete();
        drop(grant);
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn should_defer_tentative_startup_until_an_existing_first_host_probe_gets_its_seed_slot() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let progress = Arc::new(super::super::cooperative::SeedProgress::default());
        let limiter = Arc::new(RecordingLimiter::default());
        let mut engine = super::super::CrawlEngine::builder().build().unwrap();
        engine.rate_limiter = limiter.clone();
        let startup = CooperativeContext::for_batch(slots.clone(), &engine, progress.clone());
        let existing = CooperativeContext::for_batch(slots.clone(), &engine, progress.clone());
        let tentative = startup.activity().await.unwrap();
        let mut probe = Box::pin(admit_host(existing.clone(), limiter.clone(), "same".into()));
        assert!(futures::poll!(&mut probe).is_pending());
        assert!(progress.unavailable("same"));
        assert_eq!(limiter.acquisitions.lock().unwrap().len(), 0);
        startup.rollback_tentative(tentative);
        let resumed = probe.await.unwrap();
        assert!(!progress.unavailable("same"));
        assert_eq!(*limiter.acquisitions.lock().unwrap(), ["same"]);
        existing.complete();
        drop(resumed);
        assert_eq!(slots.available_permits(), 1);
        let requeued = startup.activity().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        startup.complete();
        drop(requeued);
        assert_eq!(slots.available_permits(), 1);
    }

    pub(super) struct HeldTarget {
        pub(super) blocked: std::sync::atomic::AtomicBool,
        pub(super) release: tokio::sync::Notify,
    }

    #[derive(Default)]
    struct PendingLimiter {
        active: std::sync::atomic::AtomicUsize,
        attempts: std::sync::atomic::AtomicUsize,
    }

    struct PendingAcquire<'a>(&'a std::sync::atomic::AtomicUsize);

    impl Drop for PendingAcquire<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl RateLimiter for PendingLimiter {
        async fn acquire(&self, _: &str) -> Result<(), CrawlError> {
            self.attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _pending = PendingAcquire(&self.active);
            std::future::pending().await
        }
        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_admit_a_browser_turn_while_the_same_hosts_http_limiter_is_pending() {
        use std::sync::atomic::Ordering;
        let http = Arc::new(PendingLimiter::default());
        let browser = Arc::new(RecordingLimiter::default());
        let mut turns = HostTurns::new(http.clone());
        turns.push(
            0,
            HostJob {
                host: "same".into(),
                data: "http",
            },
        );
        turns.push_with_limiter(
            1,
            HostJob {
                host: "same".into(),
                data: "browser",
            },
            browser.clone(),
        );
        let (id, data, grant) = tokio::time::timeout(Duration::from_secs(1), turns.next())
            .await
            .expect("browser admission must not share the pending HTTP queue")
            .unwrap();
        assert_eq!((id, data), (1, "browser"));
        let grant = grant.ok().expect("browser admission succeeds");
        grant.limiter.acquire("same").await.unwrap();
        assert_eq!(*browser.acquisitions.lock().unwrap(), ["same"]);
        let mut pending = Box::pin(turns.next());
        assert!(futures::poll!(&mut pending).is_pending());
        assert_eq!(http.active.load(Ordering::SeqCst), 1);
        assert_eq!(http.attempts.load(Ordering::SeqCst), 1);
        drop(pending);
        assert_eq!(turns.drain(), [(0, "http")]);
        assert_eq!(http.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn should_cancel_host_waiters_and_recover_all_queued_data_when_drained() {
        use std::sync::atomic::Ordering;
        let slots = Arc::new(tokio::sync::Semaphore::new(2));
        let contexts = (0..2)
            .map(|_| CooperativeContext::new(slots.clone()))
            .collect::<Vec<_>>();
        let limiter = Arc::new(PendingLimiter::default());
        let mut turns = HostTurns::new(limiter.clone());
        let originals = ["https://HOST.test/Raw%2fFirst", "https://HOST.test/Raw%2fSecond"];
        for (id, context) in contexts.iter().enumerate() {
            drop(context.activity().await.unwrap());
            turns.push_with_context(
                id,
                HostJob {
                    host: "host.test".into(),
                    data: originals[id].to_owned(),
                },
                context.clone(),
            );
        }
        let mut pending = Box::pin(turns.next());
        assert!(futures::poll!(&mut pending).is_pending());
        assert_eq!(limiter.active.load(Ordering::SeqCst), 1);
        assert_eq!(limiter.attempts.load(Ordering::SeqCst), 1);
        drop(pending);
        let mut returned = turns.drain();
        assert_eq!(limiter.active.load(Ordering::SeqCst), 0);
        assert_eq!(limiter.attempts.load(Ordering::SeqCst), 1);
        returned.sort_by_key(|(id, _)| *id);
        assert_eq!(returned, [(0, originals[0].into()), (1, originals[1].into())]);
        assert!(turns.is_empty());
        assert_eq!(turns.blocked_hosts(), Vec::<String>::new());
        assert_eq!(slots.available_permits(), 2);
    }

    #[async_trait::async_trait]
    impl RateLimiter for HeldTarget {
        async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
            if domain == "localhost" && self.blocked.load(std::sync::atomic::Ordering::Acquire) {
                self.release.notified().await;
            }
            Ok(())
        }
        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_request_a_free_host_when_all_active_chains_redirect_to_a_waiting_host() {
        use super::super::redirect::{Hop, RedirectCursor, RedirectResolution, RedirectStep, url_host};
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let target = server.uri().replace("127.0.0.1", "localhost");
        for index in 0..2 {
            Mock::given(path(format!("/source/{index}")))
                .respond_with(
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{target}/target/{index}"))
                        .set_delay(Duration::from_millis(100)),
                )
                .mount(&server)
                .await;
            Mock::given(path(format!("/target/{index}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target</p>", "text/html"))
                .mount(&server)
                .await;
        }
        Mock::given(path("/free"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
            .mount(&server)
            .await;
        let limiter = Arc::new(HeldTarget {
            blocked: true.into(),
            release: Default::default(),
        });
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        config.max_concurrent = Some(2);
        let engine = super::super::CrawlEngine::builder().config(config).build().unwrap();
        let urls = [
            format!("{}/source/0", server.uri()),
            format!("{}/source/1", server.uri()),
            format!("{}/free", server.uri()),
        ];
        let jobs = urls
            .iter()
            .map(|url| HostJob {
                host: url_host(url),
                data: RedirectCursor::new(url, 5),
            })
            .collect::<Vec<_>>();
        let active = tokio::spawn(super::super::continuation::run_resumable_jobs(
            jobs,
            2,
            limiter.clone(),
            |cursor| cursor.url().to_owned(),
            move |cursor, grant| {
                let mut engine = engine.clone();
                engine.rate_limiter = grant.primary();
                async move {
                    match cursor.step(&engine, None, None, Hop::Fetch).await? {
                        RedirectStep::Continue(cursor) => Ok(HostStep::Continue(HostJob {
                            host: url_host(cursor.url()),
                            data: cursor,
                        })),
                        RedirectStep::Done(RedirectResolution::Fetched(outcome)) => {
                            Ok(HostStep::Done(outcome.final_url))
                        }
                        RedirectStep::Done(RedirectResolution::Refused { refusal, .. }) => Err(refusal.into_error()),
                    }
                }
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let requests = server.received_requests().await.unwrap();
                if requests.iter().any(|request| request.url.path() == "/free") {
                    assert_eq!(
                        requests
                            .iter()
                            .filter(|request| request.url.path().starts_with("/source/"))
                            .count(),
                        2
                    );
                    assert_eq!(
                        requests
                            .iter()
                            .filter(|request| request.url.path().starts_with("/target/"))
                            .count(),
                        0
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("free host must be requested while the redirect target's turn is still held");
        limiter.blocked.store(false, std::sync::atomic::Ordering::Release);
        limiter.release.notify_waiters();
        let results = tokio::time::timeout(Duration::from_secs(2), active)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn should_schedule_public_batch_scrapes_when_all_active_chains_redirect_to_a_waiting_host() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let target = server.uri().replace("127.0.0.1", "localhost");
        for index in 0..2 {
            Mock::given(path(format!("/source/{index}")))
                .respond_with(
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{target}/target/{index}"))
                        .set_delay(Duration::from_millis(100)),
                )
                .mount(&server)
                .await;
            Mock::given(path(format!("/target/{index}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target</p>", "text/html"))
                .mount(&server)
                .await;
        }
        Mock::given(path("/free"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
            .mount(&server)
            .await;
        let limiter = Arc::new(HeldTarget {
            blocked: true.into(),
            release: Default::default(),
        });
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        config.max_concurrent = Some(2);
        let engine = super::super::CrawlEngine::builder().config(config).build().unwrap();
        let urls = [
            format!("{}/source/0", server.uri()),
            format!("{}/source/1", server.uri()),
            format!("{}/free", server.uri()),
        ];
        let mut engine = engine;
        engine.rate_limiter = limiter.clone();
        let active = tokio::spawn(async move {
            let urls = urls.iter().map(String::as_str).collect::<Vec<_>>();
            engine.batch_scrape(&urls).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let requests = server.received_requests().await.unwrap();
                if requests.iter().any(|request| request.url.path() == "/free") {
                    assert_eq!(
                        requests
                            .iter()
                            .filter(|request| request.url.path().starts_with("/source/"))
                            .count(),
                        2
                    );
                    assert_eq!(
                        requests
                            .iter()
                            .filter(|request| request.url.path().starts_with("/target/"))
                            .count(),
                        0
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("free host must be requested while the redirect target's turn is still held");
        limiter.blocked.store(false, std::sync::atomic::Ordering::Release);
        limiter.release.notify_waiters();
        let results = tokio::time::timeout(Duration::from_secs(2), active)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn should_use_both_slots_for_a_zero_delay_host() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let jobs = (0..2)
            .map(|id| HostJob {
                host: "same".into(),
                data: id,
            })
            .collect::<Vec<_>>();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            super::super::continuation::run_resumable_jobs(
                jobs,
                2,
                Arc::new(crate::defaults::NoopRateLimiter),
                |id| format!("job {id}"),
                move |id, limiter| {
                    let barrier = Arc::clone(&barrier);
                    let limiter = limiter.primary();
                    async move {
                        limiter.acquire("same").await?;
                        barrier.wait().await;
                        Ok(HostStep::Done(id))
                    }
                },
            ),
        )
        .await
        .expect("zero-delay turns must permit simultaneous work on the same host");
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|(_, result)| result.is_ok()));
    }

    #[tokio::test(start_paused = true)]
    async fn should_run_a_free_host_before_the_second_delayed_host_turn() {
        let limiter = Arc::new(crate::defaults::PerDomainThrottle::new(Duration::from_secs(60)));
        let starts = Arc::new(Mutex::new(Vec::new()));
        let jobs = ["slow", "slow", "slow", "slow", "free"]
            .into_iter()
            .enumerate()
            .map(|(id, host)| HostJob {
                host: host.into(),
                data: (id, host),
            })
            .collect::<Vec<_>>();
        let observed = Arc::clone(&starts);
        let results = super::super::continuation::run_resumable_jobs(
            jobs,
            2,
            limiter,
            |(id, _)| format!("job {id}"),
            move |(id, host), limiter| {
                let observed = Arc::clone(&observed);
                let limiter = limiter.primary();
                async move {
                    limiter.acquire(host).await?;
                    observed.lock().await.push(id);
                    Ok(HostStep::Done(id))
                }
            },
        )
        .await;
        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        let starts = starts.lock().await;
        assert!(
            starts.iter().position(|id| *id == 4).unwrap() < starts.iter().position(|id| *id == 1).unwrap(),
            "starts: {starts:?}"
        );
    }
}

#[cfg(test)]
mod residual_tests {
    use super::super::CrawlEngine;
    use super::tests::HeldTarget;
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    enum BatchKind {
        Crawl,
        Stream,
    }

    #[derive(Clone, Copy)]
    enum WaitPhase {
        SeedRedirect,
        DiscoveredRedirect,
    }

    async fn assert_public_crawl_redirect_fairness(kind: BatchKind, phase: WaitPhase) {
        let site = MockServer::start().await;
        let target = site.uri().replace("127.0.0.1", "localhost");
        for id in 0..2 {
            let location = format!("{target}/target/{id}");
            let source = match phase {
                WaitPhase::SeedRedirect => ResponseTemplate::new(302).insert_header("location", &location),
                WaitPhase::DiscoveredRedirect => {
                    ResponseTemplate::new(200).set_body_raw(format!("<a href='/child/{id}'>child</a>"), "text/html")
                }
            };
            Mock::given(path(format!("/source/{id}")))
                .respond_with(source.set_delay(Duration::from_millis(100)))
                .mount(&site)
                .await;
            if matches!(phase, WaitPhase::DiscoveredRedirect) {
                Mock::given(path(format!("/child/{id}")))
                    .respond_with(ResponseTemplate::new(302).insert_header("location", &location))
                    .mount(&site)
                    .await;
            }
            Mock::given(path(format!("/target/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target</p>", "text/html"))
                .mount(&site)
                .await;
        }
        Mock::given(path("/free"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
            .mount(&site)
            .await;
        let limiter = Arc::new(HeldTarget {
            blocked: true.into(),
            release: Default::default(),
        });
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.max_concurrent = Some(2);
        config.respect_robots_txt = false;
        let mut engine = CrawlEngine::builder().config(config).build().unwrap();
        engine.rate_limiter = limiter.clone();
        let urls = [
            format!("{}/source/0", site.uri()),
            format!("{}/source/1", site.uri()),
            format!("{}/free", site.uri()),
        ];
        let running = tokio::spawn(async move {
            let urls = urls.iter().map(String::as_str).collect::<Vec<_>>();
            match kind {
                BatchKind::Crawl => engine.batch_crawl(&urls).await.len(),
                BatchKind::Stream => {
                    use futures::StreamExt;
                    let mut stream = engine.batch_crawl_stream(&urls);
                    let mut count = 0;
                    while let Some(event) = stream.next().await {
                        if matches!(event, crate::types::CrawlEvent::Complete { .. }) {
                            count += 1;
                        }
                    }
                    count
                }
            }
        });
        let free = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if site
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|request| request.url.path() == "/free")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        if free.is_err() {
            running.abort();
            let requests = site.received_requests().await.unwrap();
            let source = requests
                .iter()
                .filter(|request| request.url.path().starts_with("/source/"))
                .count();
            let target = requests
                .iter()
                .filter(|request| request.url.path().starts_with("/target/"))
                .count();
            assert_eq!(source, 2, "both logical slots must have started a real source request");
            assert_eq!(target, 0, "redirect target remains held by its rate limiter");
            if matches!(phase, WaitPhase::DiscoveredRedirect) {
                assert_eq!(
                    requests
                        .iter()
                        .filter(|request| request.url.path().starts_with("/child/"))
                        .count(),
                    2,
                    "both active crawls already reached their discovered child redirect"
                );
            }
            panic!("free queued seed did not start: sources={source}, targets={target}, free=0");
        }
        limiter.blocked.store(false, std::sync::atomic::Ordering::Release);
        limiter.release.notify_waiters();
        let count = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 3);
        let expected = match phase {
            WaitPhase::SeedRedirect => 5,
            WaitPhase::DiscoveredRedirect => 7,
        };
        assert_eq!(site.received_requests().await.unwrap().len(), expected);
    }

    #[tokio::test]
    async fn should_observe_four_inner_wire_requests_with_two_logical_crawl_slots() {
        let site = MockServer::start().await;
        for id in 0..2 {
            Mock::given(path(format!("/seed/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    format!("<a href='/child/{id}/0'>one</a><a href='/child/{id}/1'>two</a>"),
                    "text/html",
                ))
                .mount(&site)
                .await;
            for child in 0..2 {
                Mock::given(path(format!("/child/{id}/{child}")))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_raw("<p>child</p>", "text/html")
                            .set_delay(Duration::from_secs(10)),
                    )
                    .mount(&site)
                    .await;
            }
        }
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.max_concurrent = Some(2);
        config.max_pages = Some(3);
        config.rate_limit_ms = Some(0);
        config.respect_robots_txt = false;
        let engine = CrawlEngine::builder().config(config).build().unwrap();
        let urls = [format!("{}/seed/0", site.uri()), format!("{}/seed/1", site.uri())];
        let running = tokio::spawn(async move {
            let urls = urls.iter().map(String::as_str).collect::<Vec<_>>();
            engine.batch_crawl(&urls).await
        });
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let count = site
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .filter(|request| request.url.path().starts_with("/child/"))
                    .count();
                if count == 4 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        running.abort();
        observed.expect("two logical crawls each admit two simultaneous child requests while all child responses are delayed ten seconds");
        assert_eq!(site.received_requests().await.unwrap().len(), 6);
    }

    #[tokio::test]
    async fn should_schedule_public_batch_crawls_when_both_logical_crawls_wait_at_redirects() {
        assert_public_crawl_redirect_fairness(BatchKind::Crawl, WaitPhase::SeedRedirect).await;
    }

    #[tokio::test]
    async fn should_schedule_public_batch_crawl_stream_when_both_logical_crawls_wait_at_redirects() {
        assert_public_crawl_redirect_fairness(BatchKind::Stream, WaitPhase::SeedRedirect).await;
    }
    #[tokio::test]
    async fn should_schedule_another_batch_seed_when_both_inner_crawls_wait_at_child_redirects() {
        assert_public_crawl_redirect_fairness(BatchKind::Crawl, WaitPhase::DiscoveredRedirect).await;
    }

    #[tokio::test]
    async fn should_schedule_another_stream_seed_when_both_inner_crawls_wait_at_child_redirects() {
        assert_public_crawl_redirect_fairness(BatchKind::Stream, WaitPhase::DiscoveredRedirect).await;
    }

    #[derive(Clone, Default)]
    struct StreamCounter {
        pages: Arc<std::sync::atomic::AtomicUsize>,
        errors: Arc<std::sync::atomic::AtomicUsize>,
        completed: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::traits::EventEmitter for StreamCounter {
        async fn on_page(&self, _: &crate::traits::PageEvent) {
            self.pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        async fn on_error(&self, _: &crate::traits::ErrorEvent) {
            self.errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        async fn on_complete(&self, _: &crate::traits::CompleteEvent) {
            self.completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        async fn on_discovered(&self, _: &str, _: usize) {}
    }

    struct PendingAcquire(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for PendingAcquire {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct StreamHoldLimiter {
        pending: Arc<std::sync::atomic::AtomicUsize>,
        admissions: Arc<std::sync::atomic::AtomicUsize>,
        hold_after_first: bool,
    }
    #[async_trait::async_trait]
    impl crate::traits::RateLimiter for StreamHoldLimiter {
        async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
            let attempt = self.admissions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if host == "localhost" || (self.hold_after_first && attempt > 0) {
                self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _pending = PendingAcquire(self.pending.clone());
                std::future::pending::<()>().await;
            }
            Ok(())
        }
        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_complete_only_started_seeds_when_a_batch_stream_drops_during_a_redirect_wait() {
        use std::sync::atomic::Ordering;
        let site = MockServer::start().await;
        let target = site.uri().replace("127.0.0.1", "localhost");
        for index in 0..2 {
            Mock::given(path(format!("/source/{index}")))
                .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{target}/target/{index}")))
                .mount(&site)
                .await;
        }
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = StreamCounter::default();
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        config.max_concurrent = Some(2);
        let engine = CrawlEngine::builder()
            .config(config)
            .rate_limiter(StreamHoldLimiter {
                pending: pending.clone(),
                admissions: admissions.clone(),
                hold_after_first: false,
            })
            .event_emitter(counter.clone())
            .build()
            .unwrap();
        let urls = [format!("{}/source/0", site.uri()), format!("{}/source/1", site.uri())];
        let stream = engine.batch_crawl_stream(&urls.iter().map(String::as_str).collect::<Vec<_>>());
        tokio::time::timeout(Duration::from_secs(2), async {
            while site.received_requests().await.unwrap().len() != 2 || pending.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both seeds must start real source requests and a target acquire must actually wait");
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), async {
            while counter.completed.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("started seeds still complete after receiver drop");
        assert_eq!(counter.completed.load(Ordering::SeqCst), 2);
        assert_eq!(counter.pages.load(Ordering::SeqCst), 0);
        assert_eq!(counter.errors.load(Ordering::SeqCst), 0);
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert_eq!(admissions.load(Ordering::SeqCst), 3);
        assert_eq!(site.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn should_not_start_queued_seeds_after_stream_drop_while_both_wire_slots_are_live() {
        use std::sync::atomic::Ordering;
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::path_regex("^/held/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<p>held</p>", "text/html")
                    .set_delay(Duration::from_secs(10)),
            )
            .mount(&site)
            .await;
        let admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = StreamCounter::default();
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        config.max_concurrent = Some(2);
        let engine = CrawlEngine::builder()
            .config(config)
            .rate_limiter(StreamHoldLimiter {
                pending: Default::default(),
                admissions: admissions.clone(),
                hold_after_first: false,
            })
            .event_emitter(counter.clone())
            .build()
            .unwrap();
        let urls = [
            format!("{}/held/0", site.uri()),
            format!("{}/held/1", site.uri()),
            format!("{}/unstarted", site.uri()),
            "invalid url".to_owned(),
        ];
        let stream = engine.batch_crawl_stream(&urls.iter().map(String::as_str).collect::<Vec<_>>());
        tokio::time::timeout(Duration::from_secs(2), async {
            while site.received_requests().await.unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(site.received_requests().await.unwrap().len(), 2);
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), async {
            while counter.completed.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.completed.load(Ordering::SeqCst), 2);
        assert_eq!(counter.pages.load(Ordering::SeqCst), 0);
        assert_eq!(counter.errors.load(Ordering::SeqCst), 0);
        assert_eq!(admissions.load(Ordering::SeqCst), 2);
        assert_eq!(site.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn should_keep_same_host_queued_seeds_from_reading_robots_or_emitting_callbacks_before_startup() {
        use std::sync::atomic::Ordering;
        let site = MockServer::start().await;
        Mock::given(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("User-agent: *\nAllow: /\n", "text/plain"))
            .mount(&site)
            .await;
        Mock::given(path("/seed/0"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<a href='/child'>child</a>", "text/html"))
            .mount(&site)
            .await;
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = StreamCounter::default();
        let mut config = crate::CrawlConfig::builder()
            .allow_private_networks(true)
            .max_concurrent(1)
            .max_depth(1)
            .build();
        config.respect_robots_txt = true;
        let engine = CrawlEngine::builder()
            .config(config)
            .rate_limiter(StreamHoldLimiter {
                pending: pending.clone(),
                admissions: admissions.clone(),
                hold_after_first: true,
            })
            .event_emitter(counter.clone())
            .build()
            .unwrap();
        let urls = (0..6).map(|id| format!("{}/seed/{id}", site.uri())).collect::<Vec<_>>();
        let stream = engine.batch_crawl_stream(&urls.iter().map(String::as_str).collect::<Vec<_>>());
        tokio::time::timeout(Duration::from_secs(2), async {
            while counter.pages.load(Ordering::SeqCst) != 1 || pending.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), async {
            while counter.completed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.completed.load(Ordering::SeqCst), 1);
        assert_eq!(counter.pages.load(Ordering::SeqCst), 1);
        assert_eq!(counter.errors.load(Ordering::SeqCst), 0);
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert_eq!(admissions.load(Ordering::SeqCst), 2);
        let paths = site
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["/robots.txt", "/seed/0"]);
    }
}

#[cfg(test)]
#[path = "scheduling_panic_tests.rs"]
mod scheduling_panic_tests;
