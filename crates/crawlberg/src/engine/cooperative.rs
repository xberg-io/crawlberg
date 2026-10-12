use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::error::CrawlError;

#[derive(Default)]
pub(super) struct SeedProgress {
    hosts: Mutex<std::collections::HashMap<String, HostEvidence>>,
    ready: Notify,
}

#[derive(Default)]
struct HostEvidence {
    probes: usize,
    pending: usize,
}

enum EvidenceKind {
    Probe,
    Pending,
}

impl SeedProgress {
    pub(super) fn unavailable(&self, host: &str) -> bool {
        self.hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(host)
    }

    pub(super) fn changed(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.ready.notified()
    }

    fn wake(&self) {
        self.ready.notify_one();
    }

    fn evidence(self: &Arc<Self>, host: &str, kind: EvidenceKind) -> HostEvidenceGuard {
        {
            let mut hosts = self.hosts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let evidence = hosts.entry(host.to_owned()).or_default();
            match kind {
                EvidenceKind::Probe => evidence.probes += 1,
                EvidenceKind::Pending => evidence.pending += 1,
            }
        }
        self.wake();
        HostEvidenceGuard {
            progress: self.clone(),
            host: host.to_owned(),
            kind,
        }
    }
}

struct HostEvidenceGuard {
    progress: Arc<SeedProgress>,
    host: String,
    kind: EvidenceKind,
}

impl Drop for HostEvidenceGuard {
    fn drop(&mut self) {
        {
            let mut hosts = self
                .progress
                .hosts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let evidence = hosts.get_mut(&self.host).expect("host evidence owns its registration");
            match self.kind {
                EvidenceKind::Probe => evidence.probes -= 1,
                EvidenceKind::Pending => evidence.pending -= 1,
            }
            if evidence.probes == 0 && evidence.pending == 0 {
                hosts.remove(&self.host);
            }
        }
        self.progress.wake();
    }
}

pub(super) struct CooperativeContext {
    seeds: Arc<Semaphore>,
    state: Mutex<ContextState>,
    changed: Notify,
    active: AtomicUsize,
    pub(super) base_limiter: Option<Arc<dyn crate::traits::RateLimiter>>,
    #[cfg(any(feature = "browser", feature = "browser-native"))]
    pub(super) browser_base_limiter: Option<Arc<dyn crate::traits::RateLimiter>>,
    progress: Option<Arc<SeedProgress>>,
}

#[derive(Default)]
struct ContextState {
    lease: Option<OwnedSemaphorePermit>,
    acquiring: bool,
    waiting: usize,
    completed: bool,
    parent: std::sync::Weak<ActivityGuard>,
}

impl CooperativeContext {
    #[cfg(test)]
    pub(super) fn new(seeds: Arc<Semaphore>) -> Arc<Self> {
        Self::create(
            seeds,
            None,
            None,
            #[cfg(any(feature = "browser", feature = "browser-native"))]
            None,
        )
    }

    pub(super) fn for_batch(
        seeds: Arc<Semaphore>,
        engine: &super::CrawlEngine,
        progress: Arc<SeedProgress>,
    ) -> Arc<Self> {
        Self::create(
            seeds,
            Some(engine.rate_limiter.clone()),
            Some(progress),
            #[cfg(any(feature = "browser", feature = "browser-native"))]
            Some(engine.browser_rate_limiter.clone()),
        )
    }

    fn create(
        seeds: Arc<Semaphore>,
        base_limiter: Option<Arc<dyn crate::traits::RateLimiter>>,
        progress: Option<Arc<SeedProgress>>,
        #[cfg(any(feature = "browser", feature = "browser-native"))] browser_base_limiter: Option<
            Arc<dyn crate::traits::RateLimiter>,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            seeds,
            state: Mutex::new(ContextState::default()),
            changed: Notify::new(),
            active: AtomicUsize::new(0),
            base_limiter,
            #[cfg(any(feature = "browser", feature = "browser-native"))]
            browser_base_limiter,
            progress,
        })
    }

    fn wake_progress(&self) {
        if let Some(progress) = &self.progress {
            progress.wake();
        }
    }

    pub(super) fn attach_parent(&self, activity: &Arc<ActivityGuard>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parent = Arc::downgrade(activity);
    }

    pub(super) fn finish_parent(&self) {
        let parent = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parent
            .upgrade();
        if let Some(parent) = parent {
            parent.finish();
        }
    }

    pub(super) fn rollback_tentative(self: &Arc<Self>, activity: Arc<ActivityGuard>) {
        let _parking = self.waiting();
        activity.finish();
        drop(activity);
    }

    pub(super) async fn activity(self: &Arc<Self>) -> Result<Arc<ActivityGuard>, CrawlError> {
        loop {
            let changed = self.changed.notified();
            let acquisition = {
                let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.completed {
                    return Err(CrawlError::other("crawl schedule completed"));
                }
                if state.lease.is_some() {
                    return Ok(self.new_activity());
                }
                if state.acquiring {
                    None
                } else {
                    state.acquiring = true;
                    Some(AcquisitionGuard {
                        context: self.clone(),
                        completed: false,
                    })
                }
            };
            if let Some(mut acquisition) = acquisition {
                let permit = tokio::select! {
                    biased;
                    _ = changed => continue,
                    permit = self.seeds.clone().acquire_owned() => {
                        permit.map_err(|_| CrawlError::other("seed slots closed"))?
                    }
                };
                let activity = {
                    let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.completed {
                        return Err(CrawlError::other("crawl schedule completed"));
                    }
                    state.lease = Some(permit);
                    state.acquiring = false;
                    self.new_activity()
                };
                acquisition.completed = true;
                self.changed.notify_waiters();
                return Ok(activity);
            }
            changed.await;
        }
    }

    pub(super) fn complete(&self) {
        let permit = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.completed = true;
            if self.active.load(Ordering::SeqCst) == 0 {
                state.lease.take()
            } else {
                None
            }
        };
        drop(permit);
        self.changed.notify_waiters();
        self.wake_progress();
    }

    pub(super) fn waiting(self: &Arc<Self>) -> HostWait {
        let permit = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.waiting += 1;
            if self.active.load(Ordering::SeqCst) == 0 {
                state.lease.take()
            } else {
                None
            }
        };
        let released = permit.is_some();
        drop(permit);
        if released {
            self.wake_progress();
        }
        HostWait(self.clone())
    }

    fn new_activity(self: &Arc<Self>) -> Arc<ActivityGuard> {
        self.active.fetch_add(1, Ordering::SeqCst);
        Arc::new(ActivityGuard {
            context: self.clone(),
            active: AtomicBool::new(true),
        })
    }
}

pub(super) struct HostWait(Arc<CooperativeContext>);

impl Drop for HostWait {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting -= 1;
    }
}

struct AcquisitionGuard {
    context: Arc<CooperativeContext>,
    completed: bool,
}

impl Drop for AcquisitionGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.context
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .acquiring = false;
            self.context.changed.notify_waiters();
        }
    }
}

pub(super) struct ActivityGuard {
    context: Arc<CooperativeContext>,
    active: AtomicBool,
}

impl ActivityGuard {
    fn finish(&self) {
        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        let permit = {
            let mut state = self
                .context
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.context.active.fetch_sub(1, Ordering::SeqCst) == 1 && (state.waiting > 0 || state.completed) {
                state.lease.take()
            } else {
                None
            }
        };
        let released = permit.is_some();
        drop(permit);
        if released {
            self.context.wake_progress();
        }
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

type ActivityFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Arc<ActivityGuard>, CrawlError>> + Send>>;

struct HostAdmission {
    context: Arc<CooperativeContext>,
    acquisition: Option<ActivityFuture>,
    guard: Option<Arc<ActivityGuard>>,
    host: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CrawlError>> + Send>>,
    hostname: String,
    probe: Option<HostEvidenceGuard>,
    pending: Option<HostEvidenceGuard>,
}

impl std::future::Future for HostAdmission {
    type Output = Result<Arc<ActivityGuard>, CrawlError>;

    fn poll(self: std::pin::Pin<&mut Self>, context: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        let this = self.get_mut();
        if this.guard.is_none() {
            let acquisition = this.acquisition.get_or_insert_with(|| {
                let context = this.context.clone();
                Box::pin(async move { context.activity().await })
            });
            match acquisition.as_mut().poll(context) {
                Poll::Pending => {
                    // ~keep Classify an existing first probe before allowing a tentative same-host seed
                    // to keep the permit that probe needs; queued/unpolled turns provide no delay evidence.
                    if this.probe.is_none() && this.pending.is_none() {
                        this.probe = this
                            .context
                            .progress
                            .as_ref()
                            .map(|progress| progress.evidence(&this.hostname, EvidenceKind::Probe));
                    }
                    return Poll::Pending;
                }
                Poll::Ready(result) => {
                    this.acquisition = None;
                    this.guard = Some(result?);
                }
            }
        }
        match this.host.as_mut().poll(context) {
            Poll::Ready(Ok(())) => {
                this.probe.take();
                this.pending.take();
                Poll::Ready(Ok(this.guard.take().expect("a ready host owns its activity")))
            }
            Poll::Ready(Err(error)) => {
                this.guard.take();
                this.probe.take();
                this.pending.take();
                Poll::Ready(Err(error))
            }
            Poll::Pending => {
                if this.pending.is_none() {
                    this.pending = this
                        .context
                        .progress
                        .as_ref()
                        .map(|progress| progress.evidence(&this.hostname, EvidenceKind::Pending));
                }
                this.probe.take();
                this.guard.take();
                Poll::Pending
            }
        }
    }
}

pub(super) async fn admit_host(
    context: Arc<CooperativeContext>,
    limiter: Arc<dyn crate::traits::RateLimiter>,
    host: String,
) -> Result<Arc<ActivityGuard>, CrawlError> {
    let _waiting = context.waiting();
    HostAdmission {
        context,
        acquisition: None,
        guard: None,
        hostname: host.clone(),
        probe: None,
        pending: None,
        host: Box::pin(async move { limiter.acquire(&host).await }),
    }
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn should_release_the_parent_and_context_when_their_external_owners_are_dropped() {
        let slots = Arc::new(Semaphore::new(1));
        let context = CooperativeContext::new(slots.clone());
        let parent = context.activity().await.unwrap();
        context.attach_parent(&parent);
        let weak_parent = Arc::downgrade(&parent);
        let weak_context = Arc::downgrade(&context);
        let waiting = context.waiting();
        drop(parent);
        assert!(weak_parent.upgrade().is_none());
        assert_eq!(slots.available_permits(), 1);
        drop(waiting);
        drop(context);
        assert!(weak_context.upgrade().is_none());
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn should_release_a_tentative_seed_lease_without_completing_its_reusable_context() {
        let slots = Arc::new(Semaphore::new(1));
        let context = CooperativeContext::new(slots.clone());
        let first = context.activity().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        context.rollback_tentative(first);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(context.active.load(Ordering::SeqCst), 0);
        let second = context.activity().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        assert_eq!(context.active.load(Ordering::SeqCst), 1);
        context.complete();
        drop(second);
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn should_release_a_seed_slot_only_when_its_last_inner_activity_parks() {
        let seeds = Arc::new(Semaphore::new(2));
        let first = CooperativeContext::new(seeds.clone());
        let second = CooperativeContext::new(seeds.clone());
        let third = CooperativeContext::new(seeds.clone());
        let initial = first.activity().await.unwrap();
        let child_one = first.activity().await.unwrap();
        let child_two = first.activity().await.unwrap();
        let other = second.activity().await.unwrap();
        assert_eq!(seeds.available_permits(), 0);
        assert_eq!(first.active.load(Ordering::SeqCst), 3);
        let _waiting = first.waiting();
        drop(initial);
        drop(child_one);
        assert_eq!(seeds.available_permits(), 0);
        let admission = third.activity();
        tokio::pin!(admission);
        assert!(futures::poll!(&mut admission).is_pending());
        drop(child_two);
        assert_eq!(first.active.load(Ordering::SeqCst), 0);
        let free = tokio::time::timeout(std::time::Duration::from_secs(1), admission)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seeds.available_permits(), 0);
        second.complete();
        third.complete();
        drop(other);
        drop(free);
        assert_eq!(seeds.available_permits(), 2);
    }

    #[tokio::test]
    async fn should_require_a_new_seed_slot_before_a_parked_crawl_resumes() {
        let seeds = Arc::new(Semaphore::new(1));
        let first = CooperativeContext::new(seeds.clone());
        let second = CooperativeContext::new(seeds.clone());
        let running = first.activity().await.unwrap();
        let _waiting = first.waiting();
        drop(running);
        let free = tokio::time::timeout(std::time::Duration::from_secs(1), second.activity())
            .await
            .unwrap()
            .unwrap();
        let resumed = first.activity();
        tokio::pin!(resumed);
        assert!(futures::poll!(&mut resumed).is_pending());
        assert_eq!(first.active.load(Ordering::SeqCst), 0);
        second.complete();
        drop(free);
        let resumed = resumed.await.unwrap();
        assert_eq!(first.active.load(Ordering::SeqCst), 1);
        assert_eq!(seeds.available_permits(), 0);
        first.complete();
        drop(resumed);
        assert_eq!(seeds.available_permits(), 1);
    }
    struct HeldRedirects {
        blocked: AtomicBool,
        target_waiters: AtomicUsize,
        acquisitions: AtomicUsize,
        targets_started: Notify,
        release: Notify,
    }

    #[async_trait::async_trait]
    impl crate::traits::RateLimiter for HeldRedirects {
        async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
            self.acquisitions.fetch_add(1, Ordering::SeqCst);
            if host == "localhost" && self.blocked.load(Ordering::Acquire) {
                self.target_waiters.fetch_add(1, Ordering::SeqCst);
                self.targets_started.notify_waiters();
                self.release.notified().await;
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

    async fn trial_cursor(
        context: Arc<CooperativeContext>,
        engine: super::super::CrawlEngine,
        url: String,
        limiter: Arc<dyn crate::traits::RateLimiter>,
    ) -> Result<String, CrawlError> {
        use super::super::redirect::{Hop, RedirectCursor, RedirectResolution, RedirectStep, url_host};
        let mut cursor = RedirectCursor::new(&url, 10);
        loop {
            let host = url_host(cursor.url());
            let _activity = admit_host(context.clone(), limiter.clone(), host.clone()).await?;
            let mut engine = engine.clone();
            engine.rate_limiter = Arc::new(super::super::host_turn::GrantedTurn::new(host, limiter.clone()));
            match cursor.step(&engine, None, None, Hop::Fetch).await? {
                RedirectStep::Continue(next) => cursor = next,
                RedirectStep::Done(RedirectResolution::Fetched(outcome)) => return Ok(outcome.final_url),
                RedirectStep::Done(RedirectResolution::Refused { refusal, .. }) => return Err(refusal.into_error()),
            }
        }
    }

    #[tokio::test]
    async fn should_admit_a_free_seed_after_two_real_redirect_host_waits_park() {
        use futures::{StreamExt, stream::FuturesUnordered};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        let target = site.uri().replace("127.0.0.1", "localhost");
        for index in 0..2 {
            Mock::given(wiremock::matchers::path(format!("/source/{index}")))
                .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{target}/target/{index}")))
                .mount(&site)
                .await;
            Mock::given(wiremock::matchers::path(format!("/target/{index}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw("<p>target</p>", "text/html"))
                .mount(&site)
                .await;
        }
        Mock::given(wiremock::matchers::path("/free"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
            .mount(&site)
            .await;
        let seeds = Arc::new(Semaphore::new(2));
        let limiter = Arc::new(HeldRedirects {
            blocked: AtomicBool::new(true),
            target_waiters: AtomicUsize::new(0),
            acquisitions: AtomicUsize::new(0),
            targets_started: Notify::new(),
            release: Notify::new(),
        });
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        let engine = super::super::CrawlEngine::builder().config(config).build().unwrap();
        let jobs = FuturesUnordered::new();
        for index in 0..3 {
            let context = CooperativeContext::new(seeds.clone());
            let engine = engine.clone();
            let limiter = limiter.clone();
            let url = if index < 2 {
                format!("{}/source/{index}", site.uri())
            } else {
                format!("{}/free", site.uri())
            };
            jobs.push(async move {
                if index == 2 {
                    loop {
                        let started = limiter.targets_started.notified();
                        if limiter.target_waiters.load(Ordering::SeqCst) == 2 {
                            break;
                        }
                        started.await;
                    }
                }
                trial_cursor(context, engine, url, limiter).await
            });
        }
        let running = tokio::spawn(async move { jobs.collect::<Vec<_>>().await });
        let free = tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
            assert_eq!(limiter.target_waiters.load(Ordering::SeqCst), 2);
            assert_eq!(site.received_requests().await.unwrap().len(), 2);
            panic!("two real host waits held both seed activity slots");
        }
        assert_eq!(site.received_requests().await.unwrap().len(), 3);
        assert_eq!(limiter.target_waiters.load(Ordering::SeqCst), 2);
        limiter.blocked.store(false, Ordering::Release);
        limiter.release.notify_waiters();
        let results = tokio::time::timeout(std::time::Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(site.received_requests().await.unwrap().len(), 5);
        assert_eq!(limiter.acquisitions.load(Ordering::SeqCst), 5);
        assert_eq!(seeds.available_permits(), 2);
    }
    #[tokio::test]
    async fn should_preserve_four_inner_wires_with_two_active_crawl_contexts() {
        use futures::{StreamExt, stream::FuturesUnordered};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::path_regex("^/child/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<p>child</p>", "text/html")
                    .set_delay(std::time::Duration::from_secs(10)),
            )
            .mount(&site)
            .await;
        let seeds = Arc::new(Semaphore::new(2));
        let contexts = (0..3)
            .map(|_| CooperativeContext::new(seeds.clone()))
            .collect::<Vec<_>>();
        let inner = (0..3).map(|_| Arc::new(Semaphore::new(2))).collect::<Vec<_>>();
        let limiter = Arc::new(HeldRedirects {
            blocked: AtomicBool::new(false),
            target_waiters: AtomicUsize::new(0),
            acquisitions: AtomicUsize::new(0),
            targets_started: Notify::new(),
            release: Notify::new(),
        });
        let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        let engine = super::super::CrawlEngine::builder().config(config).build().unwrap();
        let jobs = FuturesUnordered::new();
        for (family, child) in [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)] {
            let context = contexts[family].clone();
            let inner = inner[family].clone();
            let engine = engine.clone();
            let limiter = limiter.clone();
            let url = format!("{}/child/{family}/{child}", site.uri());
            jobs.push(async move {
                let _inner = inner.acquire_owned().await.unwrap();
                trial_cursor(context, engine, url, limiter).await
            });
        }
        let running = tokio::spawn(async move { jobs.collect::<Vec<_>>().await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while site.received_requests().await.unwrap().len() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("two active contexts must each keep two independent inner wire slots");
        let paths = site
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 4);
        for path in ["/child/0/0", "/child/0/1", "/child/1/0", "/child/1/1"] {
            assert!(paths.contains(&path.to_owned()));
        }
        assert_eq!(contexts[0].active.load(Ordering::SeqCst), 2);
        assert_eq!(contexts[1].active.load(Ordering::SeqCst), 2);
        assert_eq!(contexts[2].active.load(Ordering::SeqCst), 0);
        assert_eq!(limiter.acquisitions.load(Ordering::SeqCst), 4);
        assert_eq!(seeds.available_permits(), 0);
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        for context in &contexts {
            context.complete();
        }
        assert_eq!(seeds.available_permits(), 2);
        assert!(
            contexts
                .iter()
                .all(|context| context.active.load(Ordering::SeqCst) == 0)
        );
        assert_eq!(site.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn should_recover_an_activity_acquisition_when_its_first_waiter_is_cancelled() {
        let seeds = Arc::new(Semaphore::new(1));
        let occupied = CooperativeContext::new(seeds.clone());
        let waiting = CooperativeContext::new(seeds.clone());
        let active = occupied.activity().await.unwrap();
        let mut first = Box::pin(waiting.activity());
        let mut second = Box::pin(waiting.activity());
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        drop(first);
        occupied.complete();
        drop(active);
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waiting.active.load(Ordering::SeqCst), 1);
        waiting.complete();
        admitted.finish();
        admitted.finish();
        assert_eq!(waiting.active.load(Ordering::SeqCst), 0);
        assert_eq!(seeds.available_permits(), 1);
        drop(admitted);
        assert_eq!(seeds.available_permits(), 1);
    }
    #[tokio::test]
    async fn should_retain_seed_budget_for_ready_processing_without_a_parked_host_wait() {
        let seeds = Arc::new(Semaphore::new(1));
        let first = CooperativeContext::new(seeds.clone());
        let second = CooperativeContext::new(seeds.clone());
        let work = first.activity().await.unwrap();
        drop(work);
        let mut other = Box::pin(second.activity());
        assert!(
            futures::poll!(&mut other).is_pending(),
            "a ready crawl must retain its active schedule slot until it parks or completes"
        );
        first.complete();
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(1), other)
            .await
            .unwrap()
            .unwrap();
        second.complete();
        drop(admitted);
        assert_eq!(seeds.available_permits(), 1);
    }

    #[tokio::test]
    async fn should_cancel_all_activity_waiters_when_their_crawl_completes() {
        let seeds = Arc::new(Semaphore::new(1));
        let occupied = CooperativeContext::new(seeds.clone());
        let waiting = CooperativeContext::new(seeds.clone());
        let live = occupied.activity().await.unwrap();
        let mut first = Box::pin(waiting.activity());
        let mut second = Box::pin(waiting.activity());
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        waiting.complete();
        let cancelled = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            let (first, second) = futures::join!(first, second);
            assert!(matches!(first, Err(error) if error.to_string().contains("crawl schedule completed")));
            assert!(matches!(second, Err(error) if error.to_string().contains("crawl schedule completed")));
        })
        .await;
        assert!(
            cancelled.is_ok(),
            "completed crawls must not wait for occupied seed slots"
        );
        assert_eq!(waiting.active.load(Ordering::SeqCst), 0);
        assert_eq!(seeds.available_permits(), 0);
        assert!(
            matches!(waiting.activity().await, Err(error) if error.to_string().contains("crawl schedule completed"))
        );
        occupied.complete();
        drop(live);
        assert_eq!(seeds.available_permits(), 1);
    }
}
