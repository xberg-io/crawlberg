use std::sync::Arc;

use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::sync::{Semaphore, mpsc};
use tracing::Instrument;

use crate::error::CrawlError;
use crate::types::{CrawlEvent, CrawlResult};

use super::admission::admission_key;
use super::continuation::{Continuation, ContinuationStep, Waiting};
use super::cooperative::{ActivityGuard, CooperativeContext, SeedProgress};
use super::crawl_state::receiver_closed;
use super::host_turn::{HostGrant, HostJob, HostTurns, TurnError};
use super::{CrawlEngine, SeedUrl};

type CrawlOutcome = Result<CrawlResult, CrawlError>;
type CrawlAdvance = (usize, ContinuationStep<CrawlOutcome>, Option<Arc<ActivityGuard>>);
type SeedStart = (SeedJob, Result<Arc<ActivityGuard>, CrawlError>);

struct SeedJob {
    id: usize,
    admitted: Result<(CrawlEngine, SeedUrl), CrawlError>,
    host: Option<String>,
}

// ~keep Suspended parent futures are input-bounded, while shared seed leases cap active
// crawl schedules at C and each existing inner JoinSet remains independently bounded by C.
struct SeedSchedule {
    contexts: Vec<Arc<CooperativeContext>>,
    keys: Vec<String>,
    starts: FuturesUnordered<BoxFuture<'static, SeedStart>>,
    backlog: std::collections::BTreeMap<usize, SeedJob>,
    concurrency: usize,
    progress: Arc<SeedProgress>,
    active: FuturesUnordered<BoxFuture<'static, CrawlAdvance>>,
    turns: HostTurns<Waiting<CrawlOutcome>>,
    results: Vec<(String, CrawlOutcome)>,
    tx: Option<mpsc::Sender<CrawlEvent>>,
}

impl CrawlEngine {
    pub(super) async fn run_scheduled_crawls(
        &self,
        urls: &[&str],
        tx: Option<mpsc::Sender<CrawlEvent>>,
    ) -> Vec<(String, CrawlOutcome)> {
        let mut schedule = SeedSchedule::new(self, urls, tx);
        let mut cancelled = false;
        loop {
            let progress = schedule.progress.clone();
            let changed = progress.changed();
            if !cancelled && schedule.tx.as_ref().is_some_and(mpsc::Sender::is_closed) {
                cancelled = true;
                schedule.cancel();
            }
            if !cancelled {
                schedule.queue_starts();
            }
            if schedule.backlog.is_empty()
                && schedule.starts.is_empty()
                && schedule.active.is_empty()
                && schedule.turns.is_empty()
            {
                break;
            }
            tokio::select! {
                biased;
                () = receiver_closed(&schedule.tx), if !cancelled => {
                    cancelled = true;
                    schedule.cancel();
                }
                completed = schedule.active.next(), if !schedule.active.is_empty() => {
                    let (id, step, activity) = completed.expect("active seed continuation");
                    match step {
                        ContinuationStep::Waiting(waiting) if !cancelled => {
                            let limiter = waiting.limiter.clone();
                            schedule.turns.push_with_context_and_limiter(id, HostJob {
                                host: waiting.host.clone(), data: waiting,
                            }, schedule.contexts[id].clone(), limiter);
                        }
                        ContinuationStep::Waiting(waiting) => {
                            schedule.advance(id, waiting.cancel(), None);
                        }
                        ContinuationStep::Done(result) => schedule.finish(self, id, result).await,
                    }
                    drop(activity);
                }
                turn = schedule.turns.next(), if !schedule.turns.is_empty() && !cancelled => {
                    let (id, waiting, grant) = turn.expect("pending seed host turn");
                    schedule.resume(id, waiting, grant);
                }
                started = schedule.starts.next(), if !schedule.starts.is_empty() && !cancelled => {
                    let (job, activity) = started.expect("queued seed activity");
                    schedule.start(job, activity);
                }
                () = changed, if !schedule.backlog.is_empty() && !cancelled => {}
            }
        }
        schedule.results
    }
}

impl SeedSchedule {
    fn new(engine: &CrawlEngine, urls: &[&str], tx: Option<mpsc::Sender<CrawlEvent>>) -> Self {
        let concurrency = engine
            .config
            .max_concurrent
            .unwrap_or(super::batch::DEFAULT_MAX_CONCURRENT);
        let slots = Arc::new(Semaphore::new(concurrency));
        let progress = Arc::new(SeedProgress::default());
        let contexts = urls
            .iter()
            .map(|_| CooperativeContext::for_batch(slots.clone(), engine, progress.clone()))
            .collect::<Vec<_>>();
        let mut backlog = std::collections::BTreeMap::new();
        let mut keys = Vec::with_capacity(urls.len());
        for (id, url) in urls.iter().enumerate() {
            let admitted = engine.admit(url);
            keys.push(admission_key(url, admitted.as_ref().ok().map(|(_, seed)| seed)));
            let host = admitted
                .as_ref()
                .ok()
                .map(|(_, seed)| super::redirect::url_host(seed.as_str()));
            backlog.insert(id, SeedJob { id, admitted, host });
        }
        Self {
            contexts,
            keys,
            starts: Default::default(),
            backlog,
            concurrency,
            progress,
            active: Default::default(),
            turns: HostTurns::new(engine.scheduling_limiter()),
            results: Vec::with_capacity(urls.len()),
            tx,
        }
    }

    fn queue_starts(&mut self) {
        while self.starts.len() < self.concurrency {
            let ready = self
                .backlog
                .iter()
                .find(|(_, job)| job.host.as_ref().is_none_or(|host| !self.progress.unavailable(host)))
                .map(|(id, _)| *id);
            let Some(id) = ready else {
                break;
            };
            let job = self.backlog.remove(&id).expect("selected seed remains queued");
            let context = self.contexts[id].clone();
            self.starts.push(Box::pin(async move {
                let activity = context.activity().await;
                (job, activity)
            }));
        }
    }

    fn start(&mut self, job: SeedJob, activity: Result<Arc<ActivityGuard>, CrawlError>) {
        let id = job.id;
        let context = self.contexts[id].clone();
        if job.host.as_ref().is_some_and(|host| self.progress.unavailable(host))
            && let Ok(activity) = activity
        {
            context.rollback_tentative(activity);
            self.backlog.insert(id, job);
            return;
        }
        let tx = self.tx.clone();
        let outcome = activity.and_then(|activity| job.admitted.map(|admitted| (activity, admitted)));
        match outcome {
            Ok((activity, (mut engine, seed))) => {
                let span = if tx.is_none() {
                    tracing::info_span!("crawl.engine.crawl", url.full = %seed)
                } else {
                    tracing::Span::none()
                };
                let host = super::redirect::url_host(seed.as_str());
                let base = engine.scheduling_limiter();
                engine.crawl_schedule = Some(context.clone());
                let continuation = Continuation::ungranted(host, base, move |routing| {
                    async move {
                        let engine = routing.bind_engine(engine);
                        engine.for_one_crawl().crawl_with_sender(&seed, tx).await
                    }
                    .instrument(span)
                });
                self.advance(id, continuation, Some(activity));
            }
            Err(error) => {
                self.active
                    .push(Box::pin(async move { (id, ContinuationStep::Done(Err(error)), None) }));
            }
        }
    }

    fn advance(&mut self, id: usize, continuation: Continuation<CrawlOutcome>, activity: Option<Arc<ActivityGuard>>) {
        self.active.push(advance_seed(
            id,
            continuation,
            activity,
            self.contexts[id].clone(),
            self.keys[id].clone(),
        ));
    }

    fn resume(&mut self, id: usize, waiting: Waiting<CrawlOutcome>, grant: Result<HostGrant, TurnError>) {
        let context = self.contexts[id].clone();
        let url = self.keys[id].clone();
        let tx = self.tx.clone();
        self.active.push(Box::pin(async move {
            let (limiter, activity) = match grant {
                Ok(grant) => (Ok(grant.limiter), grant.activity),
                Err(error) => {
                    let activity = tokio::select! {
                        biased;
                        () = receiver_closed(&tx) => None,
                        activity = context.activity() => match activity {
                            Ok(activity) => Some(activity),
                            Err(error) => return (id, ContinuationStep::Done(Err(error)), None),
                        },
                    };
                    (Err(error), activity)
                }
            };
            let continuation = waiting.resume(limiter, &url).await;
            advance_seed(id, continuation, activity, context, url).await
        }));
    }

    fn cancel(&mut self) {
        self.backlog.clear();
        self.starts.clear();
        for context in &self.contexts {
            context.complete();
        }
        for (id, waiting) in self.turns.drain() {
            self.advance(id, waiting.cancel(), None);
        }
    }

    async fn finish(&mut self, engine: &CrawlEngine, id: usize, mut result: CrawlOutcome) {
        self.contexts[id].complete();
        if let (Some(tx), Err(error)) = (&self.tx, &result) {
            let event = CrawlEvent::Error {
                url: self.keys[id].clone(),
                error: error.to_string(),
            };
            let delivery = async {
                let _ = tx.send(event.clone()).await;
                if let Some(sink) = &engine.event_sink {
                    sink.emit(event).await;
                }
            };
            if std::panic::AssertUnwindSafe(delivery).catch_unwind().await.is_err() {
                result = Err(TurnError::Panicked.into_error(&self.keys[id]));
            }
        }
        self.results.push((self.keys[id].clone(), result));
    }
}

fn advance_seed(
    id: usize,
    continuation: Continuation<CrawlOutcome>,
    activity: Option<Arc<ActivityGuard>>,
    context: Arc<CooperativeContext>,
    url: String,
) -> BoxFuture<'static, CrawlAdvance> {
    Box::pin(async move {
        if let Some(activity) = &activity {
            context.attach_parent(activity);
        }
        let step = match std::panic::AssertUnwindSafe(continuation.advance())
            .catch_unwind()
            .await
        {
            Ok(step) => step,
            Err(_) => ContinuationStep::Done(Err(TurnError::Panicked.into_error(&url))),
        };
        (id, step, activity)
    })
}
