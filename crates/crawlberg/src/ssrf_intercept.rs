//! CDP Fetch-domain interception that re-validates every browser-issued request
//! against the SSRF policy, closing the gap the pre-navigation seed check leaves
//! open: a browser follows redirects and client-side navigations internally, so
//! without per-request interception a redirect to a private/metadata address
//! would reach the network unchecked. The same interception counts the redirects
//! the main frame follows, so `max_redirects` can bound them.
//!
//! ~keep A top-level module rather than nested under `browser`, so every chromiumoxide
//! ~keep caller can reach it: the scrape/crawl fetch in `browser`, `browser_pool`, and
//! ~keep `interact::chromiumoxide::run_with_browser` (xberg-io/crawlberg#74). `browser.rs`
//! ~keep is gated on the wider `browser` feature (it pulls in `browser_profile`/
//! ~keep `browser_session_pool`, which are `browser`-gated too), but `interact/chromiumoxide.rs`
//! ~keep is gated on the narrower `browser-chromiumoxide`, so nesting this under `browser`
//! ~keep would make it unreachable from a `browser-chromiumoxide`-only build. This module has
//! ~keep no dependency on anything `browser`-gated, so it is gated on `browser-chromiumoxide`
//! ~keep alone in `lib.rs`, matching both callers' actual requirement.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, EventTargetCreated, EventTargetDestroyed, TargetId,
};
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::CrawlError;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// How long closing a watched page and its popups may take before the watch ends anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times a request whose frame is not known yet is looked up again, and the pause
/// between lookups, before it is refused.
const ATTRIBUTION_RETRIES: usize = 3;
const ATTRIBUTION_RETRY_DELAY: Duration = Duration::from_millis(15);

/// How long after an action returns a request it started still counts as its own: for a
/// script, a scroll or a wait, and for input (a click, a key press, typing) that can start a
/// navigation or submit a form.
///
/// ~keep Fetch.requestPaused carries no timestamp, so a request is timed when the listener
/// ~keep receives its pause. A `fetch()` a script starts is paused about 1 ms after the script
/// ~keep returns on Chrome 154; the navigation a click starts takes longer, and more under load
/// ~keep (over 25 ms with the test suite running in parallel).
pub(crate) const ACTION_GRACE: Duration = Duration::from_millis(25);
pub(crate) const INPUT_ACTION_GRACE: Duration = Duration::from_millis(150);

/// The longest an action waits for the requests it started to be judged.
const ACTION_SETTLE_LIMIT: Duration = Duration::from_secs(1);

/// The most refused requests a page keeps for attribution to an action.
const MAX_REFUSALS: usize = 256;

/// What the interception observed for one watched page.
#[derive(Debug, Default)]
pub(crate) struct InterceptOutcome {
    /// The first request the SSRF policy blocked, as `(url, reason)`.
    pub(crate) blocked: Option<(String, String)>,
    /// HTTP redirects the main frame followed before its first document arrived.
    pub(crate) redirects_followed: usize,
    /// The main-frame response the navigation ends on without a document: the redirect past
    /// the redirect limit, or a response Chrome does not commit (204, 205, 304).
    pub(crate) stopped_response: Option<StoppedResponse>,
    /// The status of the last main-frame response that was not a redirect: the document the
    /// page shows.
    document_status: Option<u16>,
    /// Whether the main frame has received a document that is not a redirect. Redirects
    /// after it belong to a navigation the page started itself.
    first_document_arrived: bool,
}

/// A main-frame response the navigation ends on without a document, reported as is.
///
/// ~keep The headers are read by `browser::navigation`, which needs the `browser` feature;
/// ~keep a `browser-chromiumoxide`-only build has just `interact`, which reads the URL and status.
#[derive(Debug)]
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct StoppedResponse {
    /// The URL that answered.
    pub(crate) url: String,
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
}

/// The SSRF check of one chromiumoxide [`Browser`]: a single listener on the browser session
/// that answers every paused request of every target in that browser. Pages register with
/// [`FirewallHandle::watch`]. Each request is judged by the policy of the watched page it
/// belongs to: the page itself, a frame in it, or a popup it opened, directly or through
/// another popup. A request that belongs to no watched page is refused. Interception is on
/// while at least one page is watched.
///
/// ~keep CDP Fetch interception is per session. Enabled on a page's session it pauses only
/// ~keep that page's requests, and chromiumoxide attaches a popup's target without pausing it,
/// ~keep so the popup's first request would leave before a page-level interception could be
/// ~keep enabled on it. Enabled on the browser session, it pauses every target's requests.
/// ~keep A browser serves several pages at once (a `BrowserPool` hands out one tab per
/// ~keep concurrent fetch), and a second Fetch listener on the same session would answer
/// ~keep the same paused requests and turn interception off under the others, so one
/// ~keep listener per browser serves them all. On a browser reached through
/// ~keep `browser.endpoint`, pages other clients opened belong to no watched page, so their
/// ~keep requests are refused while a page of ours is watched.
pub(crate) struct BrowserFirewall {
    handle: FirewallHandle,
    listener: tokio::task::JoinHandle<()>,
}

/// A cheap, cloneable reference to a [`BrowserFirewall`], used to watch pages.
#[derive(Clone)]
pub(crate) struct FirewallHandle {
    commands: mpsc::UnboundedSender<Command>,
}

/// A page under the check. [`Watch::close`] or [`Watch::park`] ends it; dropping it closes
/// the page like `close`.
pub(crate) struct Watch {
    commands: mpsc::UnboundedSender<Command>,
    page: Arc<WatchedPage>,
    ended: bool,
}

/// A request the check refused, stamped with the time Chrome paused it.
struct Refusal {
    paused_at: Instant,
    url: String,
    reason: String,
}

struct WatchedPage {
    /// The page's own target.
    root: TargetId,
    main_frame: FrameId,
    policy: SsrfPolicy,
    redirect_limit: usize,
    outcome: Mutex<InterceptOutcome>,
    refusals: Mutex<Vec<Refusal>>,
    /// Set when the watch ends: from then on every request of the page is refused.
    ending: AtomicBool,
    /// Requests of the page that are paused and not answered yet.
    in_flight: AtomicUsize,
}

/// The targets and frames each watched page owns.
#[derive(Default)]
struct Registry {
    pages: Vec<Arc<WatchedPage>>,
    /// Every live target a watched page owns, in the order they were created: its own, its
    /// out-of-process frames, and the popups it opened. A target of an ended watch stays
    /// until Chrome destroys it, its requests refused, and interception stays on until then.
    targets: Vec<(TargetId, Arc<WatchedPage>)>,
    /// The in-process frames of the owned pages, filled in as requests name them.
    frames: HashMap<FrameId, Arc<WatchedPage>>,
}

impl Registry {
    fn owner_of_target(&self, target: &str) -> Option<Arc<WatchedPage>> {
        self.targets
            .iter()
            .find(|(id, _)| id.inner() == target)
            .map(|(_, page)| Arc::clone(page))
    }

    fn owner_of_frame(&self, frame: &FrameId) -> Option<Arc<WatchedPage>> {
        self.owner_of_target(frame.inner())
            .or_else(|| self.frames.get(frame).map(Arc::clone))
    }

    /// No page is watched and no target of an ended watch is still alive.
    fn is_idle(&self) -> bool {
        self.pages.is_empty() && self.targets.is_empty()
    }

    fn owns_live_target(&self, page: &Arc<WatchedPage>, keep_root: bool) -> bool {
        self.targets
            .iter()
            .any(|(id, owner)| Arc::ptr_eq(owner, page) && !(keep_root && *id == page.root))
    }
}

struct Shared {
    registry: Mutex<Registry>,
    /// Notified whenever a target is destroyed.
    destroyed: Notify,
}

enum Command {
    Watch(Arc<WatchedPage>, oneshot::Sender<Result<(), String>>),
    /// Close the page's popups, and the page itself when `close_page` is set, then stop
    /// watching it.
    End {
        page: Arc<WatchedPage>,
        close_page: bool,
        done: Option<oneshot::Sender<()>>,
    },
}

/// What a finished task of the listener reports back to it.
enum Done {
    Answered,
    Closed,
    Ended(Arc<WatchedPage>, bool, Option<oneshot::Sender<()>>),
}

impl BrowserFirewall {
    /// Start the listener on `browser`'s session. Interception stays off until a page is watched.
    pub(crate) async fn start(browser: Arc<Browser>) -> Result<Self, CrawlError> {
        let listen_error = |e| CrawlError::browser_error(format!("failed to register intercept listener: {e}"));
        let paused = browser
            .event_listener::<EventRequestPaused>()
            .await
            .map_err(listen_error)?;
        let created = browser
            .event_listener::<EventTargetCreated>()
            .await
            .map_err(listen_error)?;
        let destroyed = browser
            .event_listener::<EventTargetDestroyed>()
            .await
            .map_err(listen_error)?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let listener = tokio::spawn(serve(
            browser,
            Events {
                paused,
                created,
                destroyed,
            },
            receiver,
        ));
        Ok(Self {
            handle: FirewallHandle { commands },
            listener,
        })
    }

    pub(crate) fn handle(&self) -> FirewallHandle {
        self.handle.clone()
    }

    /// Stop the listener and release its reference to the browser, so the owner can close it.
    /// Call it once no page of the browser needs the check any more.
    pub(crate) async fn stop(mut self) {
        self.listener.abort();
        let _ = (&mut self.listener).await;
    }
}

impl Drop for BrowserFirewall {
    // ~keep A detached listener would keep the browser alive through its reference.
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl FirewallHandle {
    /// Put `page` under the check with `policy`, counting its main-frame redirects against
    /// `redirect_limit`. Interception is on when this returns.
    pub(crate) async fn watch(
        &self,
        page: &chromiumoxide::Page,
        policy: &SsrfPolicy,
        redirect_limit: usize,
    ) -> Result<Watch, CrawlError> {
        // ~keep Chrome gives a page's main frame the id of its target, so the target id stands
        // ~keep in when the frame tree is not known yet.
        let main_frame = page
            .mainframe()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| FrameId::new(page.target_id().inner().clone()));
        let watched = Arc::new(WatchedPage {
            root: page.target_id().clone(),
            main_frame,
            policy: policy.clone(),
            redirect_limit,
            outcome: Mutex::new(InterceptOutcome::default()),
            refusals: Mutex::new(Vec::new()),
            ending: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        });
        let (ack, enabled) = oneshot::channel();
        self.commands
            .send(Command::Watch(Arc::clone(&watched), ack))
            .map_err(|_| CrawlError::browser_error("request interception stopped"))?;
        let watch = Watch {
            commands: self.commands.clone(),
            page: watched,
            ended: false,
        };
        match enabled.await {
            Ok(Ok(())) => Ok(watch),
            Ok(Err(e)) => Err(CrawlError::browser_error(format!(
                "failed to enable request interception: {e}"
            ))),
            Err(_) => Err(CrawlError::browser_error("request interception stopped")),
        }
    }
}

impl Watch {
    /// Return how the navigation went so far, and keep watching: the first blocked request,
    /// the redirects followed and the response the navigation stopped on. Redirects are not
    /// counted again: once the main frame has its first document, later navigations are free
    /// of the redirect limit.
    pub(crate) fn take_outcome(&self) -> InterceptOutcome {
        let mut state = lock(&self.page.outcome);
        InterceptOutcome {
            blocked: state.blocked.take(),
            redirects_followed: std::mem::take(&mut state.redirects_followed),
            stopped_response: state.stopped_response.take(),
            ..InterceptOutcome::default()
        }
    }

    /// The status of the main-frame document the page shows now.
    #[cfg(feature = "browser")]
    pub(crate) fn document_status(&self) -> Option<u16> {
        lock(&self.page.outcome).document_status
    }

    /// The first request refused among those the page sent from `started` until `grace`
    /// after the action that began then returned, once they are all judged.
    ///
    /// ~keep A request counts by the time it was paused, not the time its refusal was recorded
    /// ~keep after a DNS lookup. Refusals up to the cutoff are dropped, so each is reported once.
    pub(crate) async fn refusal_during(&self, started: Instant, grace: Duration) -> Option<(String, String)> {
        let cutoff = Instant::now() + grace;
        tokio::time::sleep_until(cutoff.into()).await;
        let deadline = cutoff + ACTION_SETTLE_LIMIT;
        while self.page.in_flight.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut refusals = lock(&self.page.refusals);
        let first = refusals
            .iter()
            .find(|refusal| refusal.paused_at >= started && refusal.paused_at <= cutoff)
            .map(|refusal| (refusal.url.clone(), refusal.reason.clone()));
        refusals.retain(|refusal| refusal.paused_at > cutoff);
        first
    }

    /// Close the page and every popup it opened, children first, and end the watch once
    /// Chrome has destroyed them. From now on the page's requests are refused.
    pub(crate) async fn close(self) {
        self.end(true).await;
    }

    /// Close the popups the page opened and end the watch, keeping the page open for reuse.
    #[cfg(feature = "browser")]
    pub(crate) async fn park(self) {
        self.end(false).await;
    }

    async fn end(mut self, close_page: bool) {
        self.ended = true;
        self.page.ending.store(true, Ordering::Release);
        let (done, ended) = oneshot::channel();
        let command = Command::End {
            page: Arc::clone(&self.page),
            close_page,
            done: Some(done),
        };
        if self.commands.send(command).is_ok() {
            let _ = ended.await;
        }
    }
}

impl Drop for Watch {
    // ~keep A watch dropped mid-fetch (a timeout, a cancelled future) still refuses the page's
    // ~keep requests and closes its pages: the listener runs the close.
    fn drop(&mut self) {
        if !self.ended {
            self.page.ending.store(true, Ordering::Release);
            let _ = self.commands.send(Command::End {
                page: Arc::clone(&self.page),
                close_page: true,
                done: None,
            });
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct Events {
    paused: chromiumoxide::listeners::EventStream<EventRequestPaused>,
    created: chromiumoxide::listeners::EventStream<EventTargetCreated>,
    destroyed: chromiumoxide::listeners::EventStream<EventTargetDestroyed>,
}

/// The listener. It runs the watch commands in order, tracks the targets each watched page
/// owns, and answers the paused requests concurrently, so a slow DNS lookup for one page
/// does not hold up the others. Interception is turned off only once no page is watched
/// and no paused request is left unanswered.
async fn serve(browser: Arc<Browser>, mut events: Events, mut commands: mpsc::UnboundedReceiver<Command>) {
    let browser = &*browser;
    let shared = Shared {
        registry: Mutex::new(Registry::default()),
        destroyed: Notify::new(),
    };
    let shared = &shared;
    let mut running: FuturesUnordered<BoxFuture<'_, Done>> = FuturesUnordered::new();
    let mut enabled = false;
    let mut unanswered = 0usize;
    loop {
        if enabled && unanswered == 0 && lock(&shared.registry).is_idle() {
            let _ = browser.execute(FetchDisableParams::default()).await;
            enabled = false;
        }
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Watch(page, ack)) => {
                    {
                        let mut registry = lock(&shared.registry);
                        registry.pages.push(Arc::clone(&page));
                        registry.targets.push((page.root.clone(), Arc::clone(&page)));
                    }
                    if !enabled {
                        match browser.execute(fetch_enable_params()).await {
                            Ok(_) => enabled = true,
                            Err(e) => {
                                forget(shared, &page);
                                let _ = ack.send(Err(e.to_string()));
                                continue;
                            }
                        }
                    }
                    let _ = ack.send(Ok(()));
                }
                Some(Command::End { page, close_page, done }) => {
                    running.push(Box::pin(end_watch(browser, shared, page, close_page, done)));
                }
                None => break,
            },
            event = events.paused.next() => match event {
                Some(event) => {
                    unanswered += 1;
                    let paused_at = Instant::now();
                    running.push(Box::pin(async move {
                        answer(browser, shared, &event, paused_at).await;
                        Done::Answered
                    }));
                }
                None => break,
            },
            event = events.created.next() => {
                if let Some(event) = event
                    && let Some(close) = adopt_target(shared, &event)
                {
                    running.push(Box::pin(async move {
                        let _ = browser.execute(CloseTargetParams::new(close)).await;
                        Done::Closed
                    }));
                }
            }
            event = events.destroyed.next() => {
                if let Some(event) = event {
                    lock(&shared.registry).targets.retain(|(id, _)| *id != event.target_id);
                    shared.destroyed.notify_waiters();
                }
            }
            Some(done) = running.next(), if !running.is_empty() => match done {
                Done::Answered => unanswered -= 1,
                Done::Closed => {}
                Done::Ended(page, keep_root, done) => {
                    release(shared, &page, keep_root);
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                }
            },
        }
    }
}

/// Record a new target that belongs to a watched page: a popup it opened, directly or through
/// another popup, or one of its out-of-process frames. Returns the target when its page's
/// watch is ending, so it is closed at once.
fn adopt_target(shared: &Shared, event: &EventTargetCreated) -> Option<TargetId> {
    let info = &event.target_info;
    let mut registry = lock(&shared.registry);
    let owner = match (&info.opener_id, &info.parent_frame_id) {
        (Some(opener), _) => registry.owner_of_target(opener.inner()),
        (None, Some(parent)) => registry.owner_of_frame(parent),
        (None, None) => None,
    }?;
    let ending = owner.ending.load(Ordering::Acquire);
    registry.targets.push((info.target_id.clone(), owner));
    ending.then(|| info.target_id.clone())
}

/// Stop watching `page` once its watch has ended. Its parked page is let go; a target it
/// owns that Chrome has not destroyed yet stays, so its requests are still refused.
fn release(shared: &Shared, page: &Arc<WatchedPage>, keep_root: bool) {
    let mut registry = lock(&shared.registry);
    registry.pages.retain(|watched| !Arc::ptr_eq(watched, page));
    registry.frames.retain(|_, owner| !Arc::ptr_eq(owner, page));
    if keep_root {
        registry
            .targets
            .retain(|(id, owner)| !(Arc::ptr_eq(owner, page) && *id == page.root));
    }
}

/// Drop every trace of `page` from the registry.
fn forget(shared: &Shared, page: &Arc<WatchedPage>) {
    let mut registry = lock(&shared.registry);
    registry.pages.retain(|watched| !Arc::ptr_eq(watched, page));
    registry.targets.retain(|(_, owner)| !Arc::ptr_eq(owner, page));
    registry.frames.retain(|_, owner| !Arc::ptr_eq(owner, page));
}

/// End the watch of `page`: close the popups it opened, children first, and the page itself
/// when `close_page` is set, wait until Chrome has destroyed them and every request the page
/// sent is answered, then report back. The page's requests are refused throughout.
async fn end_watch(
    browser: &Browser,
    shared: &Shared,
    page: Arc<WatchedPage>,
    close_page: bool,
    done: Option<oneshot::Sender<()>>,
) -> Done {
    page.ending.store(true, Ordering::Release);
    let keep_root = !close_page;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        loop {
            let destroyed = shared.destroyed.notified();
            let open: Vec<TargetId> = lock(&shared.registry)
                .targets
                .iter()
                .filter(|(id, owner)| Arc::ptr_eq(owner, &page) && !(keep_root && *id == page.root))
                .map(|(id, _)| id.clone())
                .collect();
            if open.is_empty() {
                break;
            }
            // ~keep Popups close before the page that opened them: closing the opener first let a
            // ~keep popup's request through the check in some runs. A popup opened meanwhile is
            // ~keep closed as it appears.
            for target in open.iter().rev() {
                let _ = browser.execute(CloseTargetParams::new(target.clone())).await;
            }
            if lock(&shared.registry).owns_live_target(&page, keep_root) {
                destroyed.await;
            }
        }
        while page.in_flight.load(Ordering::Acquire) > 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    Done::Ended(page, keep_root, done)
}

/// The watched page a request belongs to, found through the frame that sent it. A frame not
/// known yet is looked up in the frame trees of the owned pages, a few times, since Chrome
/// can pause a new frame's first request before its page reports the frame.
async fn attribute(browser: &Browser, shared: &Shared, frame: &FrameId) -> Option<Arc<WatchedPage>> {
    for attempt in 0..=ATTRIBUTION_RETRIES {
        if let Some(owner) = lock(&shared.registry).owner_of_frame(frame) {
            return Some(owner);
        }
        let owned: Vec<(TargetId, Arc<WatchedPage>)> = lock(&shared.registry).targets.clone();
        for (target, owner) in owned {
            let Ok(page) = browser.get_page(target).await else {
                continue;
            };
            let Ok(frames) = page.frames().await else {
                continue;
            };
            if frames.contains(frame) {
                lock(&shared.registry).frames.insert(frame.clone(), Arc::clone(&owner));
                return Some(owner);
            }
        }
        if attempt < ATTRIBUTION_RETRIES {
            tokio::time::sleep(ATTRIBUTION_RETRY_DELAY).await;
        }
    }
    None
}

/// Answer one paused request. It is judged by the policy of the watched page it belongs to,
/// and refused when it belongs to none or its page's watch is ending. A document response of
/// a watched page's main frame is judged by [`main_frame_verdict`].
async fn answer(browser: &Browser, shared: &Shared, event: &EventRequestPaused, paused_at: Instant) {
    let allow = match attribute(browser, shared, &event.frame_id).await {
        None => false,
        Some(page) => {
            page.in_flight.fetch_add(1, Ordering::AcqRel);
            let allow = judge(&page, event, paused_at).await && !page.ending.load(Ordering::Acquire);
            page.in_flight.fetch_sub(1, Ordering::AcqRel);
            allow
        }
    };
    let request_id = event.request_id.clone();
    let _ = if allow {
        browser.execute(ContinueRequestParams::new(request_id)).await.map(drop)
    } else {
        browser
            .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
            .await
            .map(drop)
    };
}

async fn judge(page: &WatchedPage, event: &EventRequestPaused, paused_at: Instant) -> bool {
    if page.ending.load(Ordering::Acquire) {
        return false;
    }
    if is_response_stage(event) {
        return event.frame_id != page.main_frame || main_frame_verdict(event, page.redirect_limit, &page.outcome);
    }
    let Err(reason) = ssrf_verdict(&event.request.url, &page.policy).await else {
        return true;
    };
    let url = event.request.url.clone();
    {
        let mut outcome = lock(&page.outcome);
        if outcome.blocked.is_none() {
            outcome.blocked = Some((url.clone(), reason.clone()));
        }
    }
    let mut refusals = lock(&page.refusals);
    if refusals.len() < MAX_REFUSALS {
        refusals.push(Refusal { paused_at, url, reason });
    }
    false
}

/// Decide whether an intercepted request URL is permitted by the SSRF policy.
/// Returns `Err(reason)` when the request must be failed at the CDP layer. This
/// is the per-request decision applied to every browser-issued request.
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<(), String> {
    match url::Url::parse(request_url) {
        Ok(parsed) => validate_url(&parsed, policy).await.map_err(|e| e.to_string()),
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

/// Every request at the request stage, plus document responses so redirects can be counted.
fn fetch_enable_params() -> FetchEnableParams {
    FetchEnableParams {
        patterns: Some(vec![
            RequestPattern {
                url_pattern: Some("*".to_owned()),
                resource_type: None,
                request_stage: Some(RequestStage::Request),
            },
            RequestPattern {
                url_pattern: Some("*".to_owned()),
                resource_type: Some(ResourceType::Document),
                request_stage: Some(RequestStage::Response),
            },
        ]),
        handle_auth_requests: None,
    }
}

/// CDP marks a paused response by setting its status or error reason.
fn is_response_stage(event: &EventRequestPaused) -> bool {
    event.response_status_code.is_some() || event.response_error_reason.is_some()
}

/// Whether a paused document response of a watched main frame may proceed, recording the
/// status of each document. A redirect of the requested navigation is counted while it is
/// within `limit`; the one past it is recorded and must be failed. A response Chrome does not
/// commit is also recorded and failed.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started, and it is not counted.
/// ~keep Chrome commits no document for a 204, 205 or 304, so no load event fires and
/// ~keep chromiumoxide's `goto` waits for the browser timeout. Failing the response makes
/// ~keep Chrome commit its error page, which ends `goto` at once.
fn main_frame_verdict(event: &EventRequestPaused, limit: usize, state: &Mutex<InterceptOutcome>) -> bool {
    let mut state = lock(state);
    let headers = event.response_headers.as_deref().unwrap_or_default();
    let status = event.response_status_code.and_then(|code| u16::try_from(code).ok());
    let is_redirect = status.is_some_and(|code| REDIRECT_STATUSES.contains(&code))
        && headers.iter().any(|h| h.name.eq_ignore_ascii_case("location"));
    if !is_redirect && status.is_some() {
        state.document_status = status;
    }
    if state.first_document_arrived {
        return true;
    }

    let Some(status) = status.filter(|code| is_redirect || NO_DOCUMENT_STATUSES.contains(code)) else {
        state.first_document_arrived = true;
        return true;
    };

    if is_redirect && state.redirects_followed < limit {
        state.redirects_followed += 1;
        return true;
    }
    let mut header_map: HashMap<String, Vec<String>> = HashMap::new();
    for header in headers {
        header_map
            .entry(header.name.to_ascii_lowercase())
            .or_default()
            .push(header.value.clone());
    }
    state.stopped_response = Some(StoppedResponse {
        url: event.request.url.clone(),
        status,
        headers: header_map,
    });
    false
}

#[cfg(test)]
mod tests {
    //! Unit tests for the per-request SSRF decision applied by browser-tier
    //! Fetch interception. These cover the security-critical verdict (the CDP
    //! plumbing around it is thin glue) and stay hermetic by using literal-IP
    //! and scheme rejections that require no DNS resolution or network.
    use super::ssrf_verdict;
    use crate::net::ssrf::SsrfPolicy;

    fn deny_policy() -> SsrfPolicy {
        SsrfPolicy::default()
    }

    fn allow_private_policy() -> SsrfPolicy {
        SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        }
    }

    #[tokio::test]
    async fn rejects_loopback_navigation() {
        let verdict = ssrf_verdict("http://127.0.0.1/admin", &deny_policy()).await;
        assert!(verdict.is_err(), "loopback must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_cloud_metadata_address() {
        let verdict = ssrf_verdict("http://169.254.169.254/latest/meta-data/", &deny_policy()).await;
        assert!(verdict.is_err(), "cloud metadata IP must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let verdict = ssrf_verdict("file:///etc/passwd", &deny_policy()).await;
        assert!(verdict.is_err(), "file:// scheme must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_malformed_url() {
        let verdict = ssrf_verdict("not a url", &deny_policy()).await;
        assert!(verdict.is_err(), "malformed URL must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn allows_loopback_when_private_networks_permitted() {
        let verdict = ssrf_verdict("http://127.0.0.1/", &allow_private_policy()).await;
        assert!(
            verdict.is_ok(),
            "loopback must pass when deny_private=false: {verdict:?}"
        );
    }
}
