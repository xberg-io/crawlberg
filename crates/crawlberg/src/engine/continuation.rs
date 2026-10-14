use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use crate::error::CrawlError;
use crate::traits::RateLimiter;

use super::host_turn::{HostGrant, TurnError};

type AdmissionReply = tokio::sync::oneshot::Sender<Result<(), CrawlError>>;

struct AcquireRequest {
    host: String,
    limiter: Arc<dyn RateLimiter>,
    reply: AdmissionReply,
}

#[derive(Default)]
struct Suspension {
    request: Mutex<Option<AcquireRequest>>,
    waker: futures::task::AtomicWaker,
}

// ~keep A parked hop owns its original future, so retries keep their dispatch state,
// fallback responses and cleanup guards without duplicating the fetch state machine.
pub(super) struct Continuation<T> {
    future: Pin<Box<dyn Future<Output = T> + Send>>,
    suspension: Arc<Suspension>,
}

pub(super) enum ContinuationStep<T> {
    Done(T),
    Waiting(Waiting<T>),
}

pub(super) struct Waiting<T> {
    pub(super) host: String,
    pub(super) limiter: Arc<dyn RateLimiter>,
    reply: AdmissionReply,
    continuation: Continuation<T>,
}

pub(super) struct LimiterRouting {
    host: String,
    base: Arc<dyn RateLimiter>,
    first: Arc<Mutex<Option<Arc<dyn RateLimiter>>>>,
    suspension: Arc<Suspension>,
}

impl LimiterRouting {
    #[cfg(test)]
    pub(super) fn primary(&self) -> Arc<dyn RateLimiter> {
        self.wrap(self.base.clone())
    }

    fn wrap(&self, inner: Arc<dyn RateLimiter>) -> Arc<dyn RateLimiter> {
        let first = if Arc::ptr_eq(&inner, &self.base) {
            self.first.clone()
        } else {
            Arc::new(Mutex::new(None))
        };
        Arc::new(YieldingLimiter {
            host: self.host.clone(),
            first,
            inner,
            suspension: self.suspension.clone(),
        })
    }

    pub(super) fn bind_engine(&self, mut engine: super::CrawlEngine) -> super::CrawlEngine {
        engine.rate_limiter = self.wrap(engine.rate_limiter.clone());
        #[cfg(any(feature = "browser", feature = "browser-native"))]
        {
            engine.browser_rate_limiter = self.wrap(engine.browser_rate_limiter.clone());
        }
        engine
    }
}

impl<T: Send + 'static> Continuation<T> {
    pub(super) fn start<F, Fut>(
        host: String,
        grant: Arc<dyn RateLimiter>,
        base: Arc<dyn RateLimiter>,
        operation: F,
    ) -> Self
    where
        F: FnOnce(LimiterRouting) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        let suspension = Arc::new(Suspension::default());
        let routing = LimiterRouting {
            host,
            base,
            first: Arc::new(Mutex::new(Some(grant))),
            suspension: Arc::clone(&suspension),
        };
        Self {
            future: Box::pin(operation(routing)),
            suspension,
        }
    }

    pub(super) fn ungranted<F, Fut>(host: String, base: Arc<dyn RateLimiter>, operation: F) -> Self
    where
        F: FnOnce(LimiterRouting) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        let suspension = Arc::new(Suspension::default());
        let routing = LimiterRouting {
            host,
            first: Arc::new(Mutex::new(None)),
            base,
            suspension: suspension.clone(),
        };
        Self {
            future: Box::pin(operation(routing)),
            suspension,
        }
    }

    pub(super) async fn advance(self) -> ContinuationStep<T> {
        let mut owned = Some(self);
        futures::future::poll_fn(move |context| {
            let current = owned.as_mut().expect("continuation is polled until its first yield");
            current.suspension.waker.register(context.waker());
            if let Poll::Ready(result) = current.future.as_mut().poll(context) {
                return Poll::Ready(ContinuationStep::Done(result));
            }
            let request = current
                .suspension
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            match request {
                Some(request) => Poll::Ready(ContinuationStep::Waiting(Waiting {
                    host: request.host,
                    limiter: request.limiter,
                    reply: request.reply,
                    continuation: owned.take().expect("the pending continuation is owned"),
                })),
                None => Poll::Pending,
            }
        })
        .await
    }
}

impl<T> Waiting<T> {
    pub(super) fn cancel(self) -> Continuation<T> {
        drop(self.reply);
        self.continuation
    }

    pub(super) async fn resume(self, grant: Result<Arc<dyn RateLimiter>, TurnError>, url: &str) -> Continuation<T> {
        let result = match grant {
            Ok(limiter) => limiter.acquire(&self.host).await,
            Err(error) => Err(error.into_error(url)),
        };
        let _ = self.reply.send(result);
        self.continuation
    }
}

struct YieldingLimiter {
    host: String,
    first: Arc<Mutex<Option<Arc<dyn RateLimiter>>>>,
    inner: Arc<dyn RateLimiter>,
    suspension: Arc<Suspension>,
}

#[async_trait::async_trait]
impl RateLimiter for YieldingLimiter {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        let first = if domain == self.host {
            self.first
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        } else {
            None
        };
        if let Some(first) = first {
            return first.acquire(domain).await;
        }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        {
            let mut request = self
                .suspension
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if request.is_some() {
                return Err(CrawlError::other("one hop cannot acquire concurrent host turns"));
            }
            *request = Some(AcquireRequest {
                host: domain.to_owned(),
                limiter: self.inner.clone(),
                reply,
            });
        }
        self.suspension.waker.wake();
        receiver
            .await
            .map_err(|_| CrawlError::other("host admission continuation was cancelled"))?
    }

    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
        self.inner.record_response(domain, status).await
    }

    async fn set_crawl_delay(&self, domain: &str, delay: std::time::Duration) -> Result<(), CrawlError> {
        self.inner.set_crawl_delay(domain, delay).await
    }
}

struct ResumableJob<J, T> {
    context: String,
    state: ResumableState<J, T>,
}

enum ResumableState<J, T> {
    Queued { host: String, data: J },
    Waiting(Waiting<Result<super::host_turn::HostStep<J, T>, CrawlError>>),
}

/// Run `jobs` with at most `concurrency` operations in flight, one host turn each.
///
/// `jobs` is pulled lazily: a job is admitted into the host backlog only when a slot is free
/// and no admitted host is ready, so the first operation starts after the first job, not
/// after the last one, and the admission of a large batch overlaps with its operations.
/// A job whose host waits parks in the backlog without a slot, and the pull goes on past it,
/// so a late ready host still starts before the second turn of a delayed host.
///
/// `context(&job)` names a job in its error messages and keys its result. The results are
/// in completion order.
pub(super) async fn run_resumable_jobs<J, T, F, Fut, C>(
    jobs: impl IntoIterator<Item = super::host_turn::HostJob<J>>,
    concurrency: usize,
    limiter: Arc<dyn RateLimiter>,
    context: C,
    operation: F,
) -> Vec<(String, Result<T, CrawlError>)>
where
    J: Send + 'static,
    T: Send + 'static,
    F: Fn(J, LimiterRouting) -> Fut + Send + Sync + 'static,
    C: Fn(&J) -> String + Send,
    Fut: Future<Output = Result<super::host_turn::HostStep<J, T>, CrawlError>> + Send + 'static,
{
    use super::host_turn::{HostJob, HostStep, HostTurns};
    use futures::FutureExt;
    use tracing::{Instrument, instrument::WithSubscriber};
    let mut jobs = jobs.into_iter().enumerate().peekable();
    let mut turns: HostTurns<ResumableJob<J, T>> = HostTurns::new(limiter);
    let operation = Arc::new(operation);
    let mut active = tokio::task::JoinSet::new();
    let mut task_inputs = std::collections::HashMap::new();
    // ~keep Sized once: a result is about 2 KB, and a growing list copies every result again.
    let mut results = Vec::with_capacity(jobs.size_hint().1.unwrap_or(0));
    while !active.is_empty() || !turns.is_empty() || jobs.peek().is_some() {
        tokio::select! {
            biased;
            turn = turns.next(), if active.len() < concurrency && !turns.is_empty() => {
                let (id, job, grant) = turn.expect("a pending continuation host turn");
                let operation = Arc::clone(&operation);
                let context = job.context.clone();
                let task = active.spawn(async move {
                    let context = job.context.clone();
                    let result = std::panic::AssertUnwindSafe(advance_job(job, grant, operation)).catch_unwind().await;
                    result.unwrap_or_else(|_| Err(TurnError::Panicked.into_error(&context)))
                }.in_current_span().with_current_subscriber());
                task_inputs.insert(task.id(), (id, context));
            }
            completed = active.join_next_with_id(), if !active.is_empty() => {
                let completed = completed.expect("an active continuation task");
                let task_id = match &completed { Ok((task_id, _)) => *task_id, Err(error) => error.id() };
                let (id, context) = task_inputs.remove(&task_id).expect("each spawned continuation retains its input");
                let result = completed.map(|(_, result)| result).unwrap_or_else(|error| {
                    Err(CrawlError::other_with_source(format!("batch task failed while processing {context}"), error))
                });
                match result {
                    Ok(HostStep::Continue(job)) => {
                        let limiter = match &job.data.state {
                            ResumableState::Waiting(waiting) => Some(waiting.limiter.clone()),
                            ResumableState::Queued { .. } => None,
                        };
                        if let Some(limiter) = limiter {
                            turns.push_with_limiter(id, job, limiter);
                        } else {
                            turns.push(id, job);
                        }
                    }
                    Ok(HostStep::Done(result)) => results.push((context, Ok(result))),
                    Err(error) => results.push((context, Err(error))),
                }
            }
            // ~keep Last in the biased order: a job is admitted only when no admitted host is
            // ready and no operation has completed, so the backlog grows past parked hosts
            // and no further, and the pull never delays a ready turn.
            () = std::future::ready(()), if active.len() < concurrency && jobs.peek().is_some() => {
                let (id, job) = jobs.next().expect("a peeked job");
                turns.push(
                    id,
                    HostJob {
                        host: job.host.clone(),
                        data: ResumableJob {
                            context: context(&job.data),
                            state: ResumableState::Queued {
                                host: job.host,
                                data: job.data,
                            },
                        },
                    },
                );
            }
        }
    }
    results
}

async fn advance_job<J, T, F, Fut>(
    job: ResumableJob<J, T>,
    grant: Result<HostGrant, TurnError>,
    operation: Arc<F>,
) -> Result<super::host_turn::HostStep<ResumableJob<J, T>, T>, CrawlError>
where
    J: Send + 'static,
    T: Send + 'static,
    F: Fn(J, LimiterRouting) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<super::host_turn::HostStep<J, T>, CrawlError>> + Send + 'static,
{
    use super::host_turn::{HostJob, HostStep};
    let (grant, base, _activity) = match grant {
        Ok(grant) => (Ok(grant.limiter), Some(grant.base), grant.activity),
        Err(error) => (Err(error), None, None),
    };
    let context = job.context;
    let continuation = match job.state {
        ResumableState::Queued { host, data } => {
            let grant = grant.map_err(|error| error.into_error(&context))?;
            let base = base.expect("a granted turn retains its actual limiter");
            Continuation::start(host, grant, base, move |routing| operation(data, routing))
        }
        ResumableState::Waiting(waiting) => waiting.resume(grant, &context).await,
    };
    match continuation.advance().await {
        ContinuationStep::Done(result) => match result? {
            HostStep::Done(result) => Ok(HostStep::Done(result)),
            HostStep::Continue(job) => Ok(HostStep::Continue(HostJob {
                host: job.host.clone(),
                data: ResumableJob {
                    context,
                    state: ResumableState::Queued {
                        host: job.host,
                        data: job.data,
                    },
                },
            })),
        },
        ContinuationStep::Waiting(waiting) => Ok(HostStep::Continue(HostJob {
            host: waiting.host.clone(),
            data: ResumableJob {
                context,
                state: ResumableState::Waiting(waiting),
            },
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct BlockedHosts {
        active: Arc<AtomicUsize>,
        acquired: AtomicUsize,
    }

    struct ActiveAdmission(Arc<AtomicUsize>);

    impl Drop for ActiveAdmission {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl RateLimiter for Arc<BlockedHosts> {
        async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
            self.acquired.fetch_add(1, Ordering::SeqCst);
            if host != "free" {
                self.active.fetch_add(1, Ordering::SeqCst);
                let _guard = ActiveAdmission(self.active.clone());
                std::future::pending::<()>().await;
            }
            Ok(())
        }
        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_reach_a_late_free_host_and_cancel_all_earlier_admission_waiters() {
        const BLOCKED_HOSTS: usize = 1_000;
        let limiter = Arc::new(BlockedHosts::default());
        let free = Arc::new(tokio::sync::Notify::new());
        let done = free.clone();
        let jobs = (0..=BLOCKED_HOSTS)
            .map(|id| super::super::host_turn::HostJob {
                host: if id == BLOCKED_HOSTS {
                    "free".into()
                } else {
                    format!("blocked{id}")
                },
                data: id,
            })
            .collect::<Vec<_>>();
        let running = tokio::spawn(run_resumable_jobs(
            jobs,
            1,
            Arc::new(limiter.clone()),
            |id| format!("input{id}"),
            move |id, routing| {
                let done = done.clone();
                async move {
                    routing.primary().acquire("free").await?;
                    assert_eq!(id, BLOCKED_HOSTS);
                    done.notify_one();
                    Ok(super::super::host_turn::HostStep::Done(id))
                }
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), free.notified())
            .await
            .expect("a parked initial backlog cannot hide the last free host");
        assert_eq!(limiter.acquired.load(Ordering::SeqCst), BLOCKED_HOSTS + 1);
        assert_eq!(limiter.active.load(Ordering::SeqCst), BLOCKED_HOSTS);
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        assert_eq!(limiter.active.load(Ordering::SeqCst), 0);
    }

    struct FailingSecondAdmission(AtomicUsize);

    #[async_trait::async_trait]
    impl RateLimiter for FailingSecondAdmission {
        async fn acquire(&self, _: &str) -> Result<(), CrawlError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(CrawlError::rate_limited("temporary admission failure"));
            }
            Ok(())
        }
        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }
        async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_resume_the_original_retry_policy_after_a_later_custom_admission_error() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        Mock::given(wiremock::matchers::path("/retry"))
            .respond_with(move |_: &wiremock::Request| {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429)
                } else {
                    ResponseTemplate::new(200).set_body_raw("<p>ready</p>", "text/html")
                }
            })
            .mount(&site)
            .await;
        let limiter = Arc::new(FailingSecondAdmission(AtomicUsize::new(0)));
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.retry_count = 2;
        config.retry_initial_delay_ms = 0;
        config.retry_max_delay_ms = 0;
        config.respect_robots_txt = false;
        let mut engine = super::super::CrawlEngine::builder().config(config).build().unwrap();
        engine.rate_limiter = limiter.clone();
        let url = format!("{}/retry", site.uri());
        let results = engine.batch_scrape(&[&url]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, url);
        assert_eq!(results[0].1.as_ref().unwrap().status_code, 200);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(limiter.0.load(Ordering::SeqCst), 3);
    }
}
