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

pub(super) async fn run_resumable_jobs<J, T, F, Fut, C>(
    jobs: Vec<super::host_turn::HostJob<J>>,
    concurrency: usize,
    limiter: Arc<dyn RateLimiter>,
    context: C,
    operation: F,
) -> Vec<(usize, Result<T, CrawlError>)>
where
    J: Send + 'static,
    T: Send + 'static,
    F: Fn(J, LimiterRouting) -> Fut + Send + Sync + 'static,
    C: Fn(usize) -> String + Send,
    Fut: Future<Output = Result<super::host_turn::HostStep<J, T>, CrawlError>> + Send + 'static,
{
    use super::host_turn::{HostJob, HostStep, HostTurns};
    use futures::FutureExt;
    use tracing::{Instrument, instrument::WithSubscriber};
    let mut turns = HostTurns::new(limiter);
    for (id, job) in jobs.into_iter().enumerate() {
        turns.push(
            id,
            HostJob {
                host: job.host.clone(),
                data: ResumableJob {
                    context: context(id),
                    state: ResumableState::Queued {
                        host: job.host,
                        data: job.data,
                    },
                },
            },
        );
    }
    let operation = Arc::new(operation);
    let mut active = tokio::task::JoinSet::new();
    let mut task_inputs = std::collections::HashMap::new();
    let mut results = Vec::new();
    while !active.is_empty() || !turns.is_empty() {
        tokio::select! {
            turn = turns.next(), if active.len() < concurrency && !turns.is_empty() => {
                let (id, job, grant) = turn.expect("a pending continuation host turn");
                let operation = Arc::clone(&operation);
                let task = active.spawn(async move {
                    let context = job.context.clone();
                    let result = std::panic::AssertUnwindSafe(advance_job(job, grant, operation)).catch_unwind().await;
                    result.unwrap_or_else(|_| Err(TurnError::Panicked.into_error(&context)))
                }.in_current_span().with_current_subscriber());
                task_inputs.insert(task.id(), id);
            }
            completed = active.join_next_with_id(), if !active.is_empty() => {
                let completed = completed.expect("an active continuation task");
                let task_id = match &completed { Ok((task_id, _)) => *task_id, Err(error) => error.id() };
                let id = task_inputs.remove(&task_id).expect("each spawned continuation retains its input");
                let result = completed.map(|(_, result)| result).unwrap_or_else(|error| {
                    Err(CrawlError::other_with_source(format!("batch task failed while processing {}", context(id)), error))
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
                    Ok(HostStep::Done(result)) => results.push((id, Ok(result))),
                    Err(error) => results.push((id, Err(error))),
                }
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
