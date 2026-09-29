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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, HeaderEntry, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::{EventFrameNavigated, FrameId};
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, EventTargetCreated, EventTargetDestroyed, GetTargetsParams, TargetId,
};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesUnordered, SelectAll, StreamExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::CrawlError;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::LOGGED_REFUSALS;
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// How long closing a watched page and its popups may take before the watch ends anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// How long interception stays on after the last refusal once nothing is watched.
///
/// ~keep Chrome reports a page destroyed while a request the page issued just before can
/// ~keep still be on its way to the check. Turned off at once, interception would let that
/// ~keep request through; kept on until refusals stop, it refuses it.
const DISABLE_DRAIN: Duration = Duration::from_millis(100);

/// The longest an action waits for the requests it started to be judged.
const ACTION_SETTLE_LIMIT: Duration = Duration::from_secs(1);

/// The most refused requests a page keeps for attribution to an action, and the most refused
/// URLs it reports.
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
    /// The first main-frame document request the SSRF policy blocked, as `(url, reason)`.
    /// The page then shows Chrome's error page, not a document from the server.
    pub(crate) blocked_navigation: Option<(String, String)>,
    /// Main-frame responses that were not a redirect, keyed by their network request id. For
    /// a navigation that id is the loader id of the document the response commits. A new record
    /// drops every other response but the committed document's.
    pub(crate) documents: HashMap<String, DocumentResponse>,
    /// The loader id of the document the main frame committed last, from `Page.frameNavigated`.
    committed_loader: Option<String>,
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

/// The status and headers of a main-frame document response.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct DocumentResponse {
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
}

/// The SSRF check of one chromiumoxide [`Browser`]: a single listener on the browser session
/// that answers every paused request of every target in that browser. Pages register with
/// [`FirewallHandle::watch`]. Each request is judged by the policy of the watched page it
/// belongs to: the page itself, a frame in it, or a popup it opened, directly or through
/// another popup. Interception is on while at least one page is watched, and on a browser that
/// is killed at the end, from the first watch until the kill.
///
/// A request that belongs to another client's page of an external browser is continued
/// untouched. Any other request that belongs to no watched page is refused: on a browser
/// crawlberg launched every page is crawlberg's, and a frame that cannot be placed at all
/// is refused on either kind.
///
/// ~keep CDP Fetch interception is per session. Enabled on a page's session it pauses only
/// ~keep that page's requests, and chromiumoxide attaches a popup's target without pausing it,
/// ~keep so the popup's first request would leave before a page-level interception could be
/// ~keep enabled on it. Enabled on the browser session, it pauses every target's requests.
/// ~keep A browser serves several pages at once (a `BrowserPool` hands out one tab per
/// ~keep concurrent fetch), and a second Fetch listener on the same session would answer
/// ~keep the same paused requests and turn interception off under the others, so one
/// ~keep listener per browser serves them all.
pub(crate) struct BrowserFirewall {
    handle: FirewallHandle,
    listener: tokio::task::JoinHandle<()>,
    stopped: bool,
}

/// Whether crawlberg launched the browser, and kills it at the end, or connected to one through
/// `browser.endpoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserOrigin {
    /// crawlberg started the process, so every page in it is crawlberg's.
    Launched,
    /// crawlberg started the process and kills it when done with it. Interception is never
    /// turned off, so a page or popup still open when the check stops keeps its requests paused
    /// until the process is gone.
    Killed,
    /// Another program owns the browser, and its other pages are that program's. A stop keeps
    /// interception on until Chrome has destroyed every target of a page the check closed.
    External,
}

impl BrowserOrigin {
    /// The origin of a browser reached through `endpoint`, if one is configured.
    pub(crate) fn of_endpoint(endpoint: Option<&str>) -> Self {
        if endpoint.is_some() {
            Self::External
        } else {
            Self::Launched
        }
    }

    /// The origin of a browser that serves one fetch or session: one crawlberg launches for it
    /// with a throwaway profile is killed at its end. A saved profile is closed, so Chrome
    /// writes it out.
    pub(crate) fn of_session(endpoint: Option<&str>, throwaway_profile: bool) -> Self {
        match Self::of_endpoint(endpoint) {
            Self::Launched if throwaway_profile => Self::Killed,
            origin => origin,
        }
    }
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
    /// Every URL the SSRF policy refused for the page, credential-redacted, each once.
    refused_urls: Mutex<Vec<String>>,
    /// How many requests of the page the policy refused, for the bound on the warnings.
    refused_count: AtomicUsize,
    /// Set when the watch ends: from then on every request of the page is refused.
    ending: AtomicBool,
    /// Set when the watch ends with the page closed. On an external browser the stop waits until
    /// Chrome has destroyed the targets of these watches, not a parked page's.
    closing: AtomicBool,
    /// Requests of the page that are paused and whose answer has not been sent yet.
    in_flight: AtomicUsize,
}

/// The targets and frames each watched page owns.
#[derive(Default)]
struct Registry {
    pages: Vec<Arc<WatchedPage>>,
    /// Every live target a watched page owns, in the order they were created: its own and the
    /// popups it opened. A target of an ended watch stays until Chrome destroys it, its
    /// requests refused, and interception stays on until then.
    targets: Vec<(TargetId, Arc<WatchedPage>)>,
    /// Every live target no watched page owns: another client's page on an external browser,
    /// or a browser's own tab.
    others: HashSet<TargetId>,
    /// Frames, keyed by frame id: an in-process frame as a request names it, and a frame Chrome
    /// hosts in a target of its own as that target is created. The value is the watched page
    /// that owns the frame, or `None` for a frame of another target. A page's frames go when
    /// its watch is released.
    frames: HashMap<FrameId, Option<Arc<WatchedPage>>>,
}

/// Who a paused request belongs to.
enum Owner {
    Watched(Arc<WatchedPage>),
    Other,
}

impl Registry {
    fn owner_of_target(&self, target: &str) -> Option<Arc<WatchedPage>> {
        self.targets
            .iter()
            .find(|(id, _)| id.inner() == target)
            .map(|(_, page)| Arc::clone(page))
    }

    fn owner_of_frame(&self, frame: &FrameId) -> Option<Owner> {
        if let Some(page) = self.owner_of_target(frame.inner()) {
            return Some(Owner::Watched(page));
        }
        if self.others.iter().any(|id| id.inner() == frame.inner()) {
            return Some(Owner::Other);
        }
        self.frames.get(frame).map(|owner| {
            owner
                .as_ref()
                .map_or(Owner::Other, |page| Owner::Watched(Arc::clone(page)))
        })
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
    origin: BrowserOrigin,
    /// When the check last refused a request.
    last_refused: Mutex<Option<Instant>>,
    #[cfg(test)]
    delays: TestDelays,
}

/// Delays a unit test injects to widen a race window deterministically, and the probe that
/// reports the idle disable.
#[cfg(test)]
#[derive(Clone, Default)]
struct TestDelays {
    /// Before interception is turned on.
    enable: Duration,
    /// Before a request's SSRF verdict, as a slow DNS lookup would take.
    verdict: Duration,
    /// Between a request's verdict and the answer that delivers it to Chrome.
    deliver: Duration,
    /// Before the listener takes in a paused request, as a busy host holds it.
    receive: Duration,
    /// Told the moment the idle disable turns interception off, with the last refusal then.
    disabled: Option<mpsc::UnboundedSender<(Instant, Option<Instant>)>>,
    /// Before a watch's end closes a target, as Chrome under load is slow to destroy one.
    close: Duration,
}

enum Command {
    /// Watch the page, recording the documents its main frame commits from its navigation events.
    Watch(
        Arc<WatchedPage>,
        chromiumoxide::listeners::EventStream<EventFrameNavigated>,
        oneshot::Sender<Result<(), String>>,
    ),
    /// Close the page's popups, and the page itself when `close_page` is set, then stop
    /// watching it.
    End {
        page: Arc<WatchedPage>,
        close_page: bool,
        done: Option<oneshot::Sender<()>>,
    },
    /// Turn interception off once every answer and every watch end already started has
    /// finished, and on an external browser once every target of a closed page is destroyed,
    /// unless the browser is to be killed, then stop the listener. `done` is told when the
    /// listener is done.
    Stop(Option<oneshot::Sender<()>>),
}

/// What a finished task of the listener reports back to it.
enum Done {
    Answered,
    Closed,
    Ended(Arc<WatchedPage>, bool, Option<oneshot::Sender<()>>),
}

impl BrowserFirewall {
    /// Start the listener on `browser`'s session. Interception stays off until a page is watched.
    pub(crate) async fn start(browser: Arc<Browser>, origin: BrowserOrigin) -> Result<Self, CrawlError> {
        Self::start_with(
            browser,
            origin,
            #[cfg(test)]
            TestDelays::default(),
        )
        .await
    }

    async fn start_with(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        #[cfg(test)] delays: TestDelays,
    ) -> Result<Self, CrawlError> {
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
        // ~keep Targets that exist before the listener starts, such as another client's tabs on
        // ~keep an external browser, are seeded here; later ones arrive as creation events.
        let existing = browser
            .execute(GetTargetsParams::default())
            .await
            .map(|response| response.result.target_infos)
            .unwrap_or_default();
        let shared = Shared {
            registry: Mutex::new(Registry {
                others: existing.into_iter().map(|info| info.target_id).collect(),
                ..Registry::default()
            }),
            destroyed: Notify::new(),
            origin,
            last_refused: Mutex::new(None),
            #[cfg(test)]
            delays,
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        let listener = tokio::spawn(serve(
            browser,
            shared,
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
            stopped: false,
        })
    }

    pub(crate) fn handle(&self) -> FirewallHandle {
        self.handle.clone()
    }

    /// Turn interception off once every answer the check had started when asked to stop is
    /// delivered, stop the listener, and release its reference to the browser, so the owner can
    /// close it. On a [`BrowserOrigin::Killed`] browser interception is left on. On a
    /// [`BrowserOrigin::External`] browser the stop first closes every target still open of a
    /// watch that closed its page and waits, with no time limit, until Chrome has destroyed them. A
    /// request Chrome pauses after the stop is not waited for, and the disable can let it through.
    /// Call it once no page of the browser needs the check any more.
    ///
    /// ~keep The disable is not left to the listener's own idle disable. `End` acks from
    /// ~keep `serve`'s `Done::Ended` arm, before the loop head next evaluates `idle`, and a refusal
    /// ~keep within `DISABLE_DRAIN` holds that disable back further still, so a stop that only
    /// ~keep ended the listener left interception on with nothing answering: every request of
    /// ~keep every target in the browser then stays paused for good -- on a `browser.endpoint`
    /// ~keep Chrome, that is the user's own tabs, permanently.
    /// ~keep Nor is it sent from here: a disable that lands between a refusal's verdict and its
    /// ~keep delivery lets the refused request through. The listener sends it once the answers it
    /// ~keep has started are delivered.
    pub(crate) async fn stop(mut self) {
        let (done, stopped) = oneshot::channel();
        if self.handle.commands.send(Command::Stop(Some(done))).is_ok() {
            let _ = stopped.await;
        }
        self.stopped = true;
        let _ = (&mut self.listener).await;
    }
}

impl Drop for BrowserFirewall {
    // ~keep The stop repeats `stop`'s for the path that never reaches it: a cancelled fetch
    // ~keep future drops the firewall without stopping it, and interception left on with no
    // ~keep listener pauses the whole browser. The listener keeps answering until it has turned
    // ~keep interception off, then ends and lets go of the browser. A `Killed` browser keeps it on:
    // ~keep chromiumoxide kills the process it launched when the last reference goes.
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.handle.commands.send(Command::Stop(None));
        }
    }
}

/// Turn CDP Fetch interception off for the whole browser.
async fn disable_fetch(browser: &Browser) {
    if let Err(error) = browser.execute(FetchDisableParams::default()).await {
        tracing::warn!(%error, "failed to turn browser request interception off");
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
        let main_frame = require_main_frame(page.mainframe().await.map_err(|e| e.to_string()))?;
        let navigations = page
            .event_listener::<EventFrameNavigated>()
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to register navigation listener: {e}")))?;
        let watched = Arc::new(WatchedPage {
            root: page.target_id().clone(),
            main_frame,
            policy: policy.clone(),
            redirect_limit,
            outcome: Mutex::new(InterceptOutcome::default()),
            refusals: Mutex::new(Vec::new()),
            refused_urls: Mutex::new(Vec::new()),
            refused_count: AtomicUsize::new(0),
            ending: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        });
        let (ack, enabled) = oneshot::channel();
        self.commands
            .send(Command::Watch(Arc::clone(&watched), navigations, ack))
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

    /// The status and headers of the main-frame document that `loader_id` committed, or `None`
    /// when no response was recorded for it (a `data:` URL, or Chrome's error page for a request
    /// that got no response).
    ///
    /// ~keep Keyed by the committed loader, not taken from the last response: Chrome commits
    /// ~keep no document for a 204 or a 2xx download, so the page keeps showing the previous one.
    #[cfg(feature = "browser")]
    pub(crate) fn document(&self, loader_id: &str) -> Option<DocumentResponse> {
        lock(&self.page.outcome).documents.get(loader_id).cloned()
    }

    /// The first main-frame document request the check refused since the watch began.
    pub(crate) fn blocked_navigation(&self) -> Option<(String, String)> {
        lock(&self.page.outcome).blocked_navigation.clone()
    }

    /// Wait until every request of the page the check has taken is judged, for at most
    /// `ACTION_SETTLE_LIMIT`.
    pub(crate) async fn settle(&self) {
        let deadline = Instant::now() + ACTION_SETTLE_LIMIT;
        while self.page.in_flight.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Every URL the SSRF policy refused for the page so far, once the requests the check has
    /// taken are judged: credential-redacted, each once, in the order they were refused.
    pub(crate) async fn refused_urls(&self) -> Vec<String> {
        self.settle().await;
        lock(&self.page.refused_urls).clone()
    }

    /// The first request refused among those the page sent from `started` until `grace`
    /// after the action that began then returned, once they are all judged.
    ///
    /// ~keep A request counts by the time the listener received its pause, not the time its
    /// ~keep refusal was recorded after a DNS lookup. A pause the listener receives after the
    /// ~keep cutoff, as on a busy host, counts for the next action. Refusals up to the cutoff are
    /// ~keep dropped, so each is reported once.
    pub(crate) async fn refusal_during(&self, started: Instant, grace: Duration) -> Option<(String, String)> {
        let cutoff = Instant::now() + grace;
        tokio::time::sleep_until(cutoff.into()).await;
        self.settle().await;
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
    #[cfg(any(feature = "browser", test))]
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
/// and no paused request is left unanswered, or on a stop, once every answer and every watch
/// end already started has finished, and on an external browser once every target of a closed
/// page is destroyed. On a [`BrowserOrigin::Killed`] browser it is never turned off.
async fn serve(
    browser: Arc<Browser>,
    shared: Shared,
    mut events: Events,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let browser = &*browser;
    let shared = &shared;
    let mut running: FuturesUnordered<BoxFuture<'_, Done>> = FuturesUnordered::new();
    // ~keep What a stop waits for: the tasks running when it arrived. Requests paused after it
    // ~keep are still answered, but not waited for, so a page that keeps sending cannot hold
    // ~keep the stop off.
    let mut draining: FuturesUnordered<BoxFuture<'_, Done>> = FuturesUnordered::new();
    let mut stopping: Option<Vec<oneshot::Sender<()>>> = None;
    let mut commands_open = true;
    let mut navigations: SelectAll<BoxStream<'static, Committed>> = SelectAll::new();
    let mut enabled = false;
    let mut unanswered = 0usize;
    loop {
        if draining.is_empty()
            && let Some(stopped) = stopping.take()
        {
            if shared.origin != BrowserOrigin::Killed {
                disable_fetch(browser).await;
            }
            for done in stopped {
                let _ = done.send(());
            }
            break;
        }
        let idle =
            enabled && shared.origin != BrowserOrigin::Killed && unanswered == 0 && lock(&shared.registry).is_idle();
        let drained = lock(&shared.last_refused).is_none_or(|at| at.elapsed() >= DISABLE_DRAIN);
        if idle && drained {
            #[cfg(test)]
            if let Some(probe) = &shared.delays.disabled {
                let _ = probe.send((Instant::now(), *lock(&shared.last_refused)));
            }
            disable_fetch(browser).await;
            enabled = false;
        }
        tokio::select! {
            () = tokio::time::sleep(DISABLE_DRAIN), if idle => {}
            command = commands.recv(), if commands_open => match command {
                Some(Command::Watch(_, _, ack)) if stopping.is_some() => {
                    let _ = ack.send(Err("request interception stopped".to_owned()));
                }
                Some(Command::Watch(page, navigated, ack)) => {
                    navigations.push(commits_of(&page, navigated));
                    {
                        let mut registry = lock(&shared.registry);
                        registry.pages.push(Arc::clone(&page));
                        registry.targets.push((page.root.clone(), Arc::clone(&page)));
                    }
                    if !enabled {
                        #[cfg(test)]
                        tokio::time::sleep(shared.delays.enable).await;
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
                    // ~keep Set here, not in `end_watch`: a Stop sent right after this End is
                    // ~keep taken in before `end_watch` is first polled, and `close_ended_targets`
                    // ~keep reads the flag then.
                    if close_page {
                        page.closing.store(true, Ordering::Release);
                    }
                    running.push(Box::pin(end_watch(browser, shared, page, close_page, done)));
                }
                Some(Command::Stop(done)) => {
                    let stopped = stopping.get_or_insert_with(|| {
                        draining.extend(std::mem::take(&mut running));
                        if shared.origin == BrowserOrigin::External {
                            draining.push(Box::pin(close_ended_targets(browser, shared)));
                        }
                        Vec::new()
                    });
                    stopped.extend(done);
                }
                // ~keep Only after a stop: the firewall sends one before its handle goes.
                None => commands_open = false,
            },
            Some((page, navigated)) = navigations.next(), if !navigations.is_empty() => {
                record_commit(&page, &navigated);
            }
            event = events.paused.next() => match event {
                Some(event) => {
                    // ~keep Chrome sends a commit before any later paused response, and chromiumoxide
                    // ~keep queues both in that order, so the commits already queued are recorded
                    // ~keep first and the committed loader is current when a response is recorded.
                    while let Some(Some((page, navigated))) = navigations.next().now_or_never() {
                        record_commit(&page, &navigated);
                    }
                    #[cfg(test)]
                    tokio::time::sleep(shared.delays.receive).await;
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
                    let mut registry = lock(&shared.registry);
                    registry.targets.retain(|(id, _)| *id != event.target_id);
                    registry.others.remove(&event.target_id);
                    drop(registry);
                    shared.destroyed.notify_waiters();
                }
            }
            Some(done) = running.next(), if !running.is_empty() => settle(shared, done, &mut unanswered),
            Some(done) = draining.next(), if !draining.is_empty() => settle(shared, done, &mut unanswered),
        }
    }
}

/// Take in what a finished task of the listener reports.
fn settle(shared: &Shared, done: Done, unanswered: &mut usize) {
    match done {
        Done::Answered => *unanswered -= 1,
        Done::Closed => {}
        Done::Ended(page, keep_root, done) => {
            release(shared, &page, keep_root);
            if let Some(done) = done {
                let _ = done.send(());
            }
        }
    }
}

/// Record a new target that belongs to a watched page. A popup it opened, directly, through
/// another popup or from one of its frames, is one of its targets; a frame of it that Chrome
/// hosts in a target of its own is one of its frames. Returns a popup to close at once when its
/// page's watch is ending.
///
/// ~keep A frame's target is not closed on its own: Chrome closes the whole page for a
/// ~keep `Target.closeTarget` on it (measured on Chrome 154), so a park that closed the page's
/// ~keep other targets closed the page it was keeping. Recorded as a frame, it is attributed
/// ~keep like an in-process frame and neither closed nor waited for; it goes with its page.
/// ~keep A popup opened from inside that frame names the page's target as its opener and the
/// ~keep frame only as `openerFrameId` (measured on Chrome 154), so the opener is found among
/// ~keep the targets as before.
fn adopt_target(shared: &Shared, event: &EventTargetCreated) -> Option<TargetId> {
    let info = &event.target_info;
    let mut registry = lock(&shared.registry);
    let (owner, frame) = match (&info.opener_id, &info.parent_frame_id) {
        (Some(opener), _) => (registry.owner_of_target(opener.inner()), false),
        (None, Some(parent)) => match registry.owner_of_frame(parent) {
            Some(Owner::Watched(page)) => (Some(page), true),
            _ => (None, false),
        },
        (None, None) => (None, false),
    };
    let Some(owner) = owner else {
        registry.others.insert(info.target_id.clone());
        return None;
    };
    if frame {
        registry
            .frames
            .insert(FrameId::new(info.target_id.inner()), Some(owner));
        return None;
    }
    let ending = owner.ending.load(Ordering::Acquire);
    registry.targets.push((info.target_id.clone(), owner));
    ending.then(|| info.target_id.clone())
}

/// A main-frame commit of a watched page: the page and the navigation event.
type Committed = (Arc<WatchedPage>, Arc<EventFrameNavigated>);

/// The navigation events of `page`'s own target, until its watch ends.
fn commits_of(
    page: &Arc<WatchedPage>,
    navigated: chromiumoxide::listeners::EventStream<EventFrameNavigated>,
) -> BoxStream<'static, Committed> {
    let page = Arc::clone(page);
    navigated
        .take_while({
            let page = Arc::clone(&page);
            move |_| std::future::ready(!page.ending.load(Ordering::Acquire))
        })
        .map(move |event| (Arc::clone(&page), event))
        .boxed()
}

/// Record the loader of a document `page`'s main frame committed.
fn record_commit(page: &WatchedPage, navigated: &EventFrameNavigated) {
    if navigated.frame.id == page.main_frame {
        lock(&page.outcome).committed_loader = Some(navigated.frame.loader_id.clone().into());
    }
}

/// Stop watching `page` once its watch has ended. Its parked page is let go; a target it
/// owns that Chrome has not destroyed yet stays, so its requests are still refused.
fn release(shared: &Shared, page: &Arc<WatchedPage>, keep_root: bool) {
    let mut registry = lock(&shared.registry);
    registry.pages.retain(|watched| !Arc::ptr_eq(watched, page));
    registry
        .frames
        .retain(|_, owner| !owner.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, page)));
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
    registry
        .frames
        .retain(|_, owner| !owner.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, page)));
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
            #[cfg(test)]
            tokio::time::sleep(shared.delays.close).await;
            // ~keep A popup opened meanwhile is closed as it appears.
            for target in &open {
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
    let refused = page.refused_count.load(Ordering::Acquire);
    if refused > LOGGED_REFUSALS {
        tracing::warn!(
            refused,
            logged = LOGGED_REFUSALS,
            "the SSRF policy refused more requests the page sent; only the first were logged"
        );
    }
    Done::Ended(page, keep_root, done)
}

/// Close every target still open of a watch that ended with its page closed, and wait, with no
/// time limit, until Chrome reports each one destroyed. Their requests are refused throughout.
///
/// ~keep crawlberg cannot kill a browser it does not own, and a watch's end gives up after
/// ~keep `CLOSE_TIMEOUT`, which runs out under load with the page or a popup still open and
/// ~keep sending. Interception turned off then lets that target's later requests out
/// ~keep (xberg-io/crawlberg#484). If the connection ends, the listener's event streams end and
/// ~keep it returns without waiting.
/// ~keep A parked page's targets are left alone: the page stays open for reuse, a frame of it
/// ~keep cannot be destroyed without the page, and the parked page is unguarded after the park
/// ~keep anyway, so waiting would hold a pool shutdown for good.
async fn close_ended_targets(browser: &Browser, shared: &Shared) -> Done {
    loop {
        let destroyed = shared.destroyed.notified();
        let open: Vec<TargetId> = lock(&shared.registry)
            .targets
            .iter()
            .filter(|(_, owner)| owner.closing.load(Ordering::Acquire))
            .map(|(id, _)| id.clone())
            .collect();
        if open.is_empty() {
            return Done::Closed;
        }
        for target in open {
            let _ = browser.execute(CloseTargetParams::new(target)).await;
        }
        destroyed.await;
    }
}

/// Who a request belongs to, found through the frame that sent it. A frame not known yet is
/// looked up in the frame trees of the live pages. `None` when it cannot be placed, and the
/// request is then refused.
async fn attribute(browser: &Browser, shared: &Shared, frame: &FrameId) -> Option<Owner> {
    if let Some(owner) = lock(&shared.registry).owner_of_frame(frame) {
        return Some(owner);
    }
    let live: Vec<(TargetId, Option<Arc<WatchedPage>>)> = {
        let registry = lock(&shared.registry);
        let owned = registry
            .targets
            .iter()
            .map(|(id, page)| (id.clone(), Some(Arc::clone(page))));
        owned
            .chain(registry.others.iter().map(|id| (id.clone(), None)))
            .collect()
    };
    for (target, owner) in live {
        let Ok(page) = browser.get_page(target).await else {
            continue;
        };
        let Ok(frames) = page.frames().await else {
            continue;
        };
        if frames.contains(frame) {
            lock(&shared.registry).frames.insert(frame.clone(), owner.clone());
            return Some(owner.map_or(Owner::Other, Owner::Watched));
        }
    }
    None
}

/// Answer one paused request. It is judged by the policy of the watched page it belongs to,
/// and refused when its page's watch is ending. A request of another client's page on an
/// external browser is continued untouched; any other request is refused. A document response
/// of a watched page's main frame is judged by [`main_frame_verdict`].
async fn answer(browser: &Browser, shared: &Shared, event: &EventRequestPaused, paused_at: Instant) {
    // ~keep `_in_flight` lives to the end of this function, so the page counts the request until
    // ~keep its answer has been sent: a watch ending on a zero count has nothing still paused.
    let (allow, _in_flight) = match attribute(browser, shared, &event.frame_id).await {
        None => (false, None),
        Some(Owner::Other) => (shared.origin == BrowserOrigin::External, None),
        Some(Owner::Watched(page)) => {
            let in_flight = InFlight::enter(page);
            let page = &in_flight.0;
            let allow = judge(shared, page, event, paused_at).await && !page.ending.load(Ordering::Acquire);
            (allow, Some(in_flight))
        }
    };
    if !allow {
        *lock(&shared.last_refused) = Some(Instant::now());
    }
    #[cfg(test)]
    tokio::time::sleep(shared.delays.deliver).await;
    let request_id = event.request_id.clone();
    // ~keep For a response-stage pause, `Fetch.continueResponse` is the contract-correct call;
    // ~keep `continueRequest` is the request-stage one, and Chrome accepts it here. Switching was
    // ~keep tried and reverted. Measured on the macos-latest CI leg, which runs the preinstalled
    // ~keep Chrome (the Setup Chrome step in ci-rust.yaml is Linux-only), so it is neither
    // ~keep pinned nor reproducible locally:
    // ~keep   2d2089793, continueResponse: 5 passed, 2 failed, 60.17s
    // ~keep   7422fd541, continueRequest:  6 passed, 1 failed, 16.35s
    // ~keep `a_redirect_after_a_script_navigation_is_not_counted` fails either
    // ~keep way, so it is INDEPENDENT of this call and pre-existing.
    // ~keep `a_javascript_navigation_after_load_is_not_counted_as_a_redirect`
    // ~keep differed, but that is one run each way and could be flake. Pin Chrome
    // ~keep on that leg before drawing a conclusion or revisiting the call.
    // ~keep Left as `continueRequest` only to keep this change minimal.
    let _ = if allow {
        browser.execute(ContinueRequestParams::new(request_id)).await.map(drop)
    } else {
        browser
            .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
            .await
            .map(drop)
    };
}

/// A paused request of a watched page, counted in the page's `in_flight` while it lives.
struct InFlight(Arc<WatchedPage>);

impl InFlight {
    fn enter(page: Arc<WatchedPage>) -> Self {
        page.in_flight.fetch_add(1, Ordering::AcqRel);
        Self(page)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn judge(shared: &Shared, page: &WatchedPage, event: &EventRequestPaused, paused_at: Instant) -> bool {
    if page.ending.load(Ordering::Acquire) {
        return false;
    }
    if is_response_stage(event) {
        return main_frame_verdict(event, &page.main_frame, page.redirect_limit, &page.outcome);
    }
    #[cfg(test)]
    tokio::time::sleep(shared.delays.verdict).await;
    #[cfg(not(test))]
    let _ = shared;
    let Err(reason) = ssrf_verdict(&event.request.url, &page.policy).await else {
        return true;
    };
    let url = event.request.url.clone();
    let redacted = crate::net::redact_url_credentials(&url);
    // ~keep The page decides how many requests it sends, so it must not decide the log volume:
    // ~keep the first refusals are logged one by one, and the watch's end reports the count.
    if page.refused_count.fetch_add(1, Ordering::AcqRel) < LOGGED_REFUSALS {
        tracing::warn!(url = %redacted, %reason, "the SSRF policy refused a request the page sent");
    }
    {
        let mut refused = lock(&page.refused_urls);
        if refused.len() < MAX_REFUSALS && !refused.contains(&redacted) {
            refused.push(redacted);
        }
    }
    {
        let mut outcome = lock(&page.outcome);
        if event.frame_id == page.main_frame
            && event.resource_type == ResourceType::Document
            && outcome.blocked_navigation.is_none()
        {
            outcome.blocked_navigation = Some((url.clone(), reason.clone()));
        }
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

/// The main frame the redirect limit is counted against, or an error naming why it is unknown.
///
/// ~keep The limit cannot be applied without it, so an unknown main frame has to be loud.
/// ~keep Treating it as "no frame to compare against" instead turns "count the main frame's
/// ~keep redirects" into "count every document frame's", so an iframe redirect would spend the
/// ~keep seed's redirect budget or end the fetch on a redirect response with no body.
fn require_main_frame(resolved: Result<Option<FrameId>, String>) -> Result<FrameId, CrawlError> {
    match resolved {
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => Err(CrawlError::browser_error(
            "cannot apply the redirect limit: the page reports no main frame".to_owned(),
        )),
        Err(error) => Err(CrawlError::browser_error(format!(
            "cannot apply the redirect limit: failed to read the page's main frame: {error}"
        ))),
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

/// Whether a paused document response may proceed, recording the status and headers of each
/// main-frame document response. Only the response of the committed document is kept beside the
/// new one. A main-frame redirect of the requested navigation is counted while it is within
/// `limit`; the one past it is recorded and must be failed. A main-frame response Chrome does not
/// commit is also recorded and failed.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started, and it is not counted.
/// ~keep Chrome commits no document for a 204, 205 or 304, so no load event fires and
/// ~keep chromiumoxide's `goto` waits for the browser timeout. Failing the response makes
/// ~keep Chrome commit its error page, which ends `goto` at once.
fn main_frame_verdict(
    event: &EventRequestPaused,
    main_frame: &FrameId,
    limit: usize,
    state: &Mutex<InterceptOutcome>,
) -> bool {
    if *main_frame != event.frame_id {
        return true;
    }
    let mut state = lock(state);
    let headers = event.response_headers.as_deref().unwrap_or_default();
    let status = event.response_status_code.and_then(|code| u16::try_from(code).ok());
    let is_redirect = status.is_some_and(|code| REDIRECT_STATUSES.contains(&code))
        && headers.iter().any(|h| h.name.eq_ignore_ascii_case("location"));
    if !is_redirect
        && let Some(status) = status
        && let Some(network_id) = &event.network_id
    {
        // ~keep A newer main-frame response cancels a navigation that has not committed, so only
        // ~keep the committed document and this response can still be the one the page shows.
        // ~keep Without the pruning a page that keeps navigating to a 204 grows the map.
        if let Some(committed) = state.committed_loader.clone() {
            state.documents.retain(|id, _| *id == committed);
        }
        state.documents.insert(
            network_id.as_ref().to_owned(),
            DocumentResponse {
                status,
                headers: header_map(headers),
            },
        );
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
    state.stopped_response = Some(StoppedResponse {
        url: event.request.url.clone(),
        status,
        headers: header_map(headers),
    });
    false
}

/// CDP response headers keyed by lowercase name, as the HTTP fetch path keys them.
fn header_map(headers: &[HeaderEntry]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for header in headers {
        map.entry(header.name.to_ascii_lowercase())
            .or_default()
            .push(header.value.clone());
    }
    map
}

#[cfg(test)]
mod tests {
    //! Unit tests for the per-request SSRF decision applied by browser-tier
    //! Fetch interception. These cover the security-critical verdict (the CDP
    //! plumbing around it is thin glue) and stay hermetic by using literal-IP
    //! and scheme rejections that require no DNS resolution or network.
    use std::sync::Mutex;

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::{
        BrowserOrigin, EventRequestPaused, EventTargetCreated, FrameId, InterceptOutcome, Owner, Registry, Shared,
        TargetId, TestDelays, WatchedPage, adopt_target, lock, main_frame_verdict, release, require_main_frame,
        ssrf_verdict,
    };
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

    /// A watched page whose own target is `root`.
    fn watched(root: &str) -> Arc<WatchedPage> {
        Arc::new(WatchedPage {
            root: TargetId::new(root),
            main_frame: FrameId::new(root),
            policy: deny_policy(),
            redirect_limit: 0,
            outcome: Mutex::new(InterceptOutcome::default()),
            refusals: Mutex::new(Vec::new()),
            refused_urls: Mutex::new(Vec::new()),
            refused_count: AtomicUsize::new(0),
            ending: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        })
    }

    /// The listener's state on an external browser with `page` watched and another client's
    /// target `other` open.
    fn shared_with(page: &Arc<WatchedPage>, other: &str) -> Shared {
        Shared {
            registry: Mutex::new(Registry {
                pages: vec![Arc::clone(page)],
                targets: vec![(page.root.clone(), Arc::clone(page))],
                others: std::iter::once(TargetId::new(other)).collect(),
                frames: Default::default(),
            }),
            destroyed: Notify::new(),
            origin: BrowserOrigin::External,
            last_refused: Mutex::new(None),
            delays: TestDelays::default(),
        }
    }

    /// Chrome's report of a new target `id` of type `kind`: a frame of `parent`, or a popup that
    /// `opener` opened from its frame `opener_frame`.
    ///
    /// ~keep A popup opened from inside a frame names the page's target as `openerId` and the
    /// ~keep frame as `openerFrameId` (measured on Chrome 154); the fixtures use that shape.
    fn target_created(
        id: &str,
        kind: &str,
        opener: Option<&str>,
        opener_frame: Option<&str>,
        parent: Option<&str>,
    ) -> EventTargetCreated {
        serde_json::from_value(serde_json::json!({
            "targetInfo": {
                "targetId": id,
                "type": kind,
                "title": "",
                "url": "http://a.localhost/frame",
                "attached": false,
                "canAccessOpener": false,
                "openerId": opener,
                "openerFrameId": opener_frame,
                "parentFrameId": parent,
            }
        }))
        .expect("a target created event")
    }

    /// The ids of the targets the registry holds for watched pages, in order.
    fn owned(registry: &Registry) -> Vec<&str> {
        registry.targets.iter().map(|(id, _)| id.inner().as_str()).collect()
    }

    /// A frame of a watched page that Chrome hosts in a target of its own is one of the page's
    /// frames, not a target to close; a popup opened from inside that frame is the page's popup,
    /// kept while the page is watched and closed at once once the page is ending; and the frame
    /// goes with the page's watch.
    #[test]
    fn a_frame_target_of_a_page_is_its_frame_and_not_a_target_to_close() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let frame = adopt_target(&shared, &target_created("FRAME", "iframe", None, None, Some("ROOT")));
        let popup = adopt_target(
            &shared,
            &target_created("POPUP", "page", Some("ROOT"), Some("FRAME"), None),
        );
        page.ending.store(true, Ordering::Release);
        let late = adopt_target(
            &shared,
            &target_created("LATE", "page", Some("ROOT"), Some("FRAME"), None),
        );
        let (owned_now, others_now, frame_owner_is_page) = {
            let registry = lock(&shared.registry);
            (
                owned(&registry).into_iter().map(String::from).collect::<Vec<_>>(),
                registry.others.iter().map(|id| id.inner().clone()).collect::<Vec<_>>(),
                matches!(registry.owner_of_frame(&FrameId::new("FRAME")), Some(Owner::Watched(owner)) if Arc::ptr_eq(&owner, &page)),
            )
        };
        release(&shared, &page, true);
        let frame_after_release = lock(&shared.registry).frames.contains_key(&FrameId::new("FRAME"));

        assert!(frame.is_none(), "a frame target is never closed on its own");
        assert!(frame_owner_is_page, "the frame's requests are judged as the page's");
        assert_eq!(
            owned_now,
            ["ROOT", "POPUP", "LATE"],
            "the page's targets are its own and the popups opened from its frame, never the frame"
        );
        assert_eq!(
            others_now,
            ["OTHER"],
            "nothing of the page passes as another client's target"
        );
        assert!(popup.is_none(), "a popup of a page still watched is kept");
        assert_eq!(
            late.as_ref().map(|id| id.inner().as_str()),
            Some("LATE"),
            "a popup opened once the page is ending is closed at once"
        );
        assert!(!frame_after_release, "the frame goes with the page's watch");
    }

    /// A frame target and a popup of another client's page stay that client's: neither is adopted.
    #[test]
    fn a_frame_and_a_popup_of_another_client_s_page_stay_that_client_s() {
        let page = watched("ROOT");
        let shared = shared_with(&page, "OTHER");
        let frame = adopt_target(
            &shared,
            &target_created("OTHER-FRAME", "iframe", None, None, Some("OTHER")),
        );
        let popup = adopt_target(
            &shared,
            &target_created("OTHER-POPUP", "page", Some("OTHER"), Some("OTHER-FRAME"), None),
        );
        let registry = lock(&shared.registry);

        assert!(
            frame.is_none() && popup.is_none(),
            "nothing of another client is closed"
        );
        assert_eq!(owned(&registry), ["ROOT"], "nothing of another client is adopted");
        assert!(
            matches!(
                registry.owner_of_frame(&FrameId::new("OTHER-FRAME")),
                Some(Owner::Other)
            ),
            "the other client's frame is judged as that client's"
        );
        assert!(
            matches!(
                registry.owner_of_frame(&FrameId::new("OTHER-POPUP")),
                Some(Owner::Other)
            ),
            "the other client's popup is judged as that client's"
        );
    }

    #[test]
    fn a_resolved_main_frame_is_the_frame_the_limit_counts_against() {
        let frame = require_main_frame(Ok(Some(FrameId::new("FRAME-1")))).expect("a resolved frame must be returned");
        assert_eq!(frame, FrameId::new("FRAME-1"));
    }

    #[test]
    fn should_refuse_the_limit_when_the_page_reports_no_main_frame() {
        let error =
            require_main_frame(Ok(None)).expect_err("without a main frame the limit would count every document frame");
        assert_eq!(
            error.to_string(),
            "browser: cannot apply the redirect limit: the page reports no main frame"
        );
    }

    #[test]
    fn should_refuse_the_limit_when_the_main_frame_cannot_be_read() {
        let error = require_main_frame(Err("channel closed".to_owned()))
            .expect_err("a failed main-frame read must not silently widen the count");
        assert_eq!(
            error.to_string(),
            "browser: cannot apply the redirect limit: failed to read the page's main frame: channel closed"
        );
    }

    /// A paused main-frame document response with `status`, from the navigation `network_id`.
    fn main_frame_response(network_id: &str, status: u16) -> EventRequestPaused {
        serde_json::from_value(serde_json::json!({
            "requestId": format!("interception-{network_id}"),
            "request": {
                "url": format!("http://example.com/{network_id}"),
                "method": "GET",
                "headers": {},
                "initialPriority": "VeryHigh",
                "referrerPolicy": "no-referrer",
            },
            "frameId": "MAIN",
            "resourceType": "Document",
            "responseStatusCode": status,
            "responseHeaders": [{"name": "X-Navigation", "value": network_id}],
            "networkId": network_id,
        }))
        .expect("a paused response event")
    }

    #[test]
    fn keeps_only_the_committed_document_and_the_newest_response() {
        let main_frame = FrameId::new("MAIN");
        let state = Mutex::new(InterceptOutcome::default());
        for (network_id, status, committed) in [("A", 200, None), ("B", 204, Some("A")), ("C", 204, Some("A"))] {
            state.lock().expect("state lock").committed_loader = committed.map(str::to_owned);
            assert!(main_frame_verdict(
                &main_frame_response(network_id, status),
                &main_frame,
                0,
                &state
            ));
        }
        let state = state.into_inner().expect("state lock");
        let mut kept: Vec<(&str, u16)> = state
            .documents
            .iter()
            .map(|(id, document)| (id.as_str(), document.status))
            .collect();
        kept.sort_unstable();
        assert_eq!(kept, [("A", 200), ("C", 204)]);
        assert_eq!(
            state.documents["A"].headers.get("x-navigation"),
            Some(&vec!["A".to_owned()]),
            "headers are recorded with the status, keyed by lowercase name"
        );
    }
}

#[cfg(test)]
mod race_tests {
    //! Races the listener closes by construction, made deterministic with injected delays.
    //! These launch a real Chrome and skip when none is found.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use chromiumoxide::Browser;
    use chromiumoxide::cdp::browser_protocol::target::{GetTargetsParams, TargetId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_stream::StreamExt;

    use super::{
        ACTION_GRACE, BrowserFirewall, BrowserOrigin, CLOSE_TIMEOUT, DISABLE_DRAIN, FetchDisableParams, TestDelays,
    };
    use crate::net::ssrf::{HostMatcher, SsrfPolicy};

    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
    )]
    async fn launch(test_name: &str) -> Option<Arc<Browser>> {
        let dir = std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id()));
        let builder = chromiumoxide::browser::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(dir);
        let launched = match crate::browser_pool::apply_default_args(builder).build() {
            Ok(config) => Browser::launch(config).await,
            Err(error) => {
                eprintln!("skipping {test_name}: no usable Chrome: {error}");
                return None;
            }
        };
        match launched {
            Ok((browser, mut handler)) => {
                tokio::spawn(async move { while handler.next().await.is_some() {} });
                Some(Arc::new(browser))
            }
            Err(error) => {
                eprintln!("skipping {test_name}: no usable Chrome: {error}");
                None
            }
        }
    }

    /// Allow `localhost`, where the test pages are served, and refuse the loopback address.
    fn policy() -> SsrfPolicy {
        crate::types::CrawlConfig::builder()
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
            .build()
            .ssrf
    }

    /// Navigate `page` to an empty page served on `localhost`, so it has a real origin.
    async fn open_blank_site(page: &chromiumoxide::Page) {
        let (url, _) = denied_listener().await;
        let url = url.replace("127.0.0.1", "localhost");
        page.goto(url).await.expect("the test page must load");
    }

    /// A loopback server, counting the connections it accepts.
    async fn denied_listener() -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!(
            "http://127.0.0.1:{}/secret",
            listener.local_addr().expect("addr").port()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .await;
                });
            }
        });
        (url, hits)
    }

    /// Whether `hits` records a connection within five seconds.
    async fn served(hits: &Arc<AtomicUsize>) -> bool {
        for _ in 0..50 {
            if hits.load(Ordering::SeqCst) > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Stopping the check must turn interception off. The listener is the only thing answering
    /// paused requests, so a stop that only aborts it leaves interception on with nothing behind
    /// it: every request it covers stays paused for good, and on a browser that outlives the stop
    /// -- one reached through `browser.endpoint`, or one a surviving reference kept from being
    /// closed -- that is permanent.
    ///
    /// ~keep The page is parked rather than closed, so it is still open to be measured, and it is
    /// ~keep the page the check demonstrably covers: Chrome does not pause a target created after
    /// ~keep interception was turned on, and another target's requests are only partly paused, so
    /// ~keep neither can tell a frozen browser from a working one.
    ///
    /// ~keep The probe is a `fetch`, not a navigation. Chrome pre-connects for a navigation, so
    /// ~keep the listener accepts a connection even while the request itself is paused -- measured:
    /// ~keep 2 connections while `goto` hung for its full 3 s timeout. Counting connections would
    /// ~keep have reported a frozen browser as a working one.
    ///
    /// ~keep The freeze also needs a refusal newer than `DISABLE_DRAIN` at the moment the watch
    /// ~keep ends: only then is the listener's own idle disable still pending for the abort to
    /// ~keep beat. The injected verdict delay puts one there deterministically -- the watch waits
    /// ~keep for the requests in flight, so the last slow refusal lands just before it ends. That
    /// ~keep is a slow DNS lookup on a page being released, which is when this happens for real.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_parked_page_still_reaches_the_network_after_the_check_is_stopped() {
        let test_name = "a_parked_page_still_reaches_the_network_after_the_check_is_stopped";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            enable: Duration::ZERO,
            verdict: Duration::from_millis(300),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "window.__probe = setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 10); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let refused_while_watched = denied_hits.load(Ordering::SeqCst);
        let _ = page.evaluate("clearInterval(window.__probe); 1").await;
        watch.park().await;
        firewall.stop().await;

        let (probe, probe_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "setInterval(() => fetch({probe:?}, {{ mode: 'no-cors' }}).catch(() => 0), 50); 1"
            ))
            .await;
        let reached = served(&probe_hits).await;
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }

        assert_eq!(
            refused_while_watched, 0,
            "{test_name}: the watched page's requests must have been refused while it was watched, \
             or the drain window this test needs was never opened"
        );
        assert!(
            reached,
            "{test_name}: the parked page must still reach the network after the check is stopped; \
             interception was left on with no listener answering, so its requests are paused for good"
        );
    }

    /// On a browser that is killed when done, a page still sending after its watch ended and the
    /// check stopped stays refused: interception stays on with nothing answering, so its requests
    /// wait paused until the browser is gone. Turning interception off afterwards lets them out,
    /// which shows the page was sending all along.
    ///
    /// ~keep The parked page stands in for a page or popup Chrome has not destroyed yet when the
    /// ~keep session ends, as under load (xberg-io/crawlberg#468). The page is quiet for a second
    /// ~keep after its first refused request, so with no page watched and no recent refusal the
    /// ~keep listener's idle disable would fire before it starts sending again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_killed_browser_keeps_refusing_a_page_still_sending_after_the_check_stops() {
        let test_name = "a_killed_browser_keeps_refusing_a_page_still_sending_after_the_check_stops";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start(Arc::clone(&browser), BrowserOrigin::Killed)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); \
                 setTimeout(() => setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 10), 1000); 1"
            ))
            .await;
        let mut refused = false;
        for _ in 0..50 {
            refused = !super::lock(&watch.page.refusals).is_empty();
            if refused {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        watch.park().await;
        tokio::time::sleep(DISABLE_DRAIN * 3).await;
        firewall.stop().await;
        let reached_before_kill = served(&denied_hits).await;

        let mut browser = Arc::into_inner(browser).expect("the stopped check lets go of the browser");
        let _ = browser.execute(FetchDisableParams::default()).await;
        let reached_once_off = served(&denied_hits).await;
        let _ = browser.kill().await;

        assert!(
            refused,
            "{test_name}: the watched page's requests must be refused before the park"
        );
        assert!(
            !reached_before_kill,
            "{test_name}: a page still sending after the check stopped before a kill must not reach \
             the denied address, got {} requests",
            denied_hits.load(Ordering::SeqCst)
        );
        assert!(
            reached_once_off,
            "{test_name}: the page must reach the denied address once interception is off, or it \
             was never sending and the first assertion proves nothing"
        );
    }

    /// The ids of the targets `browser` has open.
    async fn open_targets(browser: &Browser) -> Vec<TargetId> {
        browser
            .execute(GetTargetsParams::default())
            .await
            .expect("the browser must answer")
            .result
            .target_infos
            .into_iter()
            .map(|info| info.target_id)
            .collect()
    }

    /// On an external browser, a session page still open and sending when its watch gives up
    /// stays refused through the stop: the stop closes it and keeps interception on until Chrome
    /// has destroyed it. The browser, and a tab another client had open before the check
    /// started, keep working.
    ///
    /// ~keep The injected close delay outlasts `CLOSE_TIMEOUT`, so the watch ends with the page
    /// ~keep still open and sending, as when Chrome under load is slow to destroy a page or popup
    /// ~keep (xberg-io/crawlberg#484).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_external_browser_keeps_refusing_a_session_page_until_chrome_destroys_it() {
        let test_name = "an_external_browser_keeps_refusing_a_session_page_until_chrome_destroys_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let other = browser.new_page("about:blank").await.expect("the other client's tab");
        open_blank_site(&other).await;
        let delays = TestDelays {
            close: CLOSE_TIMEOUT + Duration::from_secs(1),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::External, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let session_target = page.target_id().clone();
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 10); 1"
            ))
            .await;
        let mut refused = false;
        for _ in 0..50 {
            refused = !super::lock(&watch.page.refusals).is_empty();
            if refused {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        watch.close().await;
        let open_at_stop = open_targets(&browser).await.contains(&session_target);
        firewall.stop().await;
        let open_after_stop = open_targets(&browser).await.contains(&session_target);
        let reached = served(&denied_hits).await;

        let (reachable, other_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!("fetch({reachable:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let other_works = served(&other_hits).await;
        let mut browser = Arc::into_inner(browser).expect("the stopped check lets go of the browser");
        let _ = browser.kill().await;

        assert!(
            refused,
            "{test_name}: the watched page's requests must be refused while it is watched"
        );
        assert!(
            open_at_stop,
            "{test_name}: the page must still be open when the watch ends, or the stop has nothing to wait for"
        );
        assert!(
            !reached,
            "{test_name}: a session page still sending when the check stops must not reach the denied \
             address, got {} requests",
            denied_hits.load(Ordering::SeqCst)
        );
        assert!(
            !open_after_stop,
            "{test_name}: the stop must close the session page before it returns"
        );
        assert!(
            other_works,
            "{test_name}: the other client's tab must still reach the network after the stop"
        );
    }

    /// The popup `root` opened, if `browser` has one.
    async fn popup_of(browser: &Browser, root: &TargetId) -> Option<TargetId> {
        browser
            .execute(GetTargetsParams::default())
            .await
            .ok()?
            .result
            .target_infos
            .into_iter()
            .find(|info| info.opener_id.as_ref() == Some(root))
            .map(|info| info.target_id)
    }

    /// On an external browser, a stop that arrives while a page is being parked leaves the parked
    /// page open: the stop waits for the pages the check closed, not for one it keeps for reuse.
    ///
    /// ~keep The injected close delay slows the park while it closes the page's popup, so the stop
    /// ~keep finds the page still registered and ending. Closed there, it would be handed to the
    /// ~keep next fetch dead (xberg-io/crawlberg#484).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_during_a_park_leaves_the_parked_page_open_on_an_external_browser() {
        let test_name = "a_stop_during_a_park_leaves_the_parked_page_open_on_an_external_browser";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            close: Duration::from_millis(1500),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::External, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let root = page.target_id().clone();
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let _ = page.evaluate("window.open('about:blank'); 1").await;
        let mut popup = None;
        for _ in 0..50 {
            popup = popup_of(&browser, &root).await;
            if popup.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let (_, stopped) = tokio::join!(
            watch.park(),
            tokio::time::timeout(Duration::from_secs(15), firewall.stop())
        );
        let open = open_targets(&browser).await;
        let root_open = open.contains(&root);
        let popup_open = popup.as_ref().is_some_and(|popup| open.contains(popup));
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.kill().await;
        }

        assert!(
            popup.is_some(),
            "{test_name}: the page must open a popup, or the park has nothing to close"
        );
        assert!(
            stopped.is_ok(),
            "{test_name}: the stop must return once the park has ended"
        );
        assert!(!popup_open, "{test_name}: the park must close the popup");
        assert!(root_open, "{test_name}: the stop must leave the parked page open");
    }

    /// The id of the frame target whose URL starts with `prefix`, if `browser` has one.
    async fn frame_target_of(browser: &Browser, prefix: &str) -> Option<TargetId> {
        browser
            .execute(GetTargetsParams::default())
            .await
            .ok()?
            .result
            .target_infos
            .into_iter()
            .find(|info| info.r#type == "iframe" && info.url.starts_with(prefix))
            .map(|info| info.target_id)
    }

    /// A loopback server on `a.localhost` answering every request with `body` as HTML, counting
    /// the requests for `/ok`.
    async fn frame_site(body: String) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!(
            "http://a.localhost:{}/frame",
            listener.local_addr().expect("addr").port()
        );
        let ok_hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ok_hits);
        let body = Arc::new(body);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let counter = Arc::clone(&counter);
                let body = Arc::clone(&body);
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                    if buffer.starts_with(b"GET /ok") {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (url, ok_hits)
    }

    /// A page keeps a cross-site frame Chrome hosts in a target of its own: the frame's requests
    /// are judged by the page's policy, the park closes the page's popups and not the frame, and
    /// a stop of an external browser does not wait for it. The parked page is still open after
    /// both.
    ///
    /// ~keep The frame is on `a.localhost`, a different site from the page's `localhost`, so
    /// ~keep Chrome gives it a target of its own. Closing that target closes the page (measured on
    /// ~keep Chrome 154), which is what the park did to the page it was keeping.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_parked_page_keeps_its_cross_site_frame_and_stays_open() {
        let test_name = "a_parked_page_keeps_its_cross_site_frame_and_stays_open";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start(Arc::clone(&browser), BrowserOrigin::External)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let root = page.target_id().clone();
        let mut allowing = policy();
        allowing.allowlist.push(HostMatcher::exact("a.localhost"));
        let watch = firewall
            .handle()
            .watch(&page, &allowing, 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let (denied, denied_hits) = denied_listener().await;
        let (frame_url, ok_hits) = frame_site(format!(
            "<script>setInterval(() => {{ fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); \
             fetch('/ok?' + Math.random()).catch(() => 0); }}, 50);</script>"
        ))
        .await;
        let _ = page
            .evaluate(format!(
                "const frame = document.createElement('iframe'); frame.src = {frame_url:?}; \
                 document.body.appendChild(frame); 1"
            ))
            .await;
        let mut frame_target = None;
        for _ in 0..50 {
            frame_target = frame_target_of(&browser, "http://a.localhost").await;
            if frame_target.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let frame_allowed = served(&ok_hits).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let frame_refused = denied_hits.load(Ordering::SeqCst) == 0;
        watch.park().await;
        let open_after_park = open_targets(&browser).await.contains(&root);
        let stopped = tokio::time::timeout(Duration::from_secs(15), firewall.stop())
            .await
            .is_ok();
        let open_after_stop = open_targets(&browser).await.contains(&root);
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.kill().await;
        }

        assert!(
            frame_target.is_some(),
            "{test_name}: the cross-site frame must get a target of its own"
        );
        assert!(
            frame_allowed,
            "{test_name}: the frame's request to its own site must be allowed through"
        );
        assert!(
            frame_refused,
            "{test_name}: the frame's request to the denied address must be refused"
        );
        assert!(open_after_park, "{test_name}: parking must leave the page open");
        assert!(stopped, "{test_name}: the stop must not wait for a parked page's frame");
        assert!(open_after_stop, "{test_name}: the stop must leave the parked page open");
    }

    /// A page watched while interception is still being turned on for another page waits until
    /// it is on, so its first request is checked.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_watched_while_interception_turns_on_waits_until_it_is_on() {
        let test_name = "a_page_watched_while_interception_turns_on_waits_until_it_is_on";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            enable: Duration::from_millis(500),
            verdict: Duration::ZERO,
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, delays.clone())
            .await
            .expect("the listener must start");
        let first = browser.new_page("about:blank").await.expect("page");
        let second = browser.new_page("about:blank").await.expect("page");
        let (url, hits) = denied_listener().await;
        let handle = firewall.handle();
        let enabling = Instant::now();
        let first_watch = tokio::spawn({
            let handle = handle.clone();
            async move { handle.watch(&first, &policy(), 0).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second_watch = handle
            .watch(&second, &policy(), 0)
            .await
            .expect("the second watch must start");
        let waited = enabling.elapsed();
        open_blank_site(&second).await;
        let _ = second
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = first_watch.await;
        second_watch.close().await;
        firewall.stop().await;
        assert!(
            waited >= delays.enable,
            "{test_name}: the second watch returned after {waited:?}, before interception was on"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "{test_name}: the early request must be checked"
        );
    }

    /// A request refused after a slow DNS lookup still counts for the action that sent it:
    /// it is timed when Chrome paused it, and the action waits while it is judged.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_after_a_slow_lookup_counts_for_the_action_that_sent_it() {
        let test_name = "a_refusal_after_a_slow_lookup_counts_for_the_action_that_sent_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            enable: Duration::ZERO,
            verdict: Duration::from_millis(300),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let (url, _hits) = denied_listener().await;
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        let started = Instant::now();
        let _ = page
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let refused = watch.refusal_during(started, ACTION_GRACE).await;
        watch.close().await;
        firewall.stop().await;
        assert!(
            refused.is_some_and(|(refused_url, _)| refused_url == url),
            "{test_name}: the refusal must count for the action"
        );
    }

    /// Start a check with `delays`, watch a fresh page under it, and give the page a real origin.
    async fn watched_page(
        browser: &Arc<Browser>,
        delays: TestDelays,
    ) -> (BrowserFirewall, chromiumoxide::Page, super::Watch) {
        let firewall = BrowserFirewall::start_with(Arc::clone(browser), BrowserOrigin::Launched, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        (firewall, page, watch)
    }

    /// A watch that ends has failed every request of its page it refused: the page sees the
    /// refused `fetch` rejected by the time `park` returns.
    ///
    /// ~keep The injected delivery delay holds the refusal between its verdict and the
    /// ~keep `Fetch.failRequest` that delivers it, where a slow CDP round trip would hold it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_request_is_failed_before_its_watch_ends() {
        let test_name = "a_refused_request_is_failed_before_its_watch_ends";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            deliver: Duration::from_millis(1000),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "window.__probe = 'pending'; fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => \
                 window.__probe = 'sent', () => window.__probe = 'refused'); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        watch.park().await;
        let mut probe = String::new();
        for _ in 0..30 {
            probe = page
                .evaluate("window.__probe")
                .await
                .ok()
                .and_then(|value| value.into_value::<String>().ok())
                .unwrap_or_default();
            if probe != "pending" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        firewall.stop().await;
        drop(page);
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }
        assert_eq!(
            probe, "refused",
            "{test_name}: the refused request must be failed when the watch has ended"
        );
        assert_eq!(
            denied_hits.load(Ordering::SeqCst),
            0,
            "{test_name}: the denied address must receive nothing"
        );
    }

    /// Stopping the check waits until a refusal it is delivering has reached Chrome, so the
    /// refused request is failed rather than let through when interception turns off.
    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_the_check_still_fails_a_refused_request() {
        let test_name = "stopping_the_check_still_fails_a_refused_request";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            deliver: Duration::from_millis(500),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        tokio::time::timeout(Duration::from_secs(10), firewall.stop())
            .await
            .expect("stopping the check must finish");
        let reached = served(&denied_hits).await;
        let (after, after_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({after:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let unchecked_after_stop = served(&after_hits).await;
        drop(watch);
        drop(page);
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }
        assert!(
            !reached,
            "{test_name}: a request refused while the check stopped must not reach the denied address"
        );
        assert!(
            unchecked_after_stop,
            "{test_name}: once the check has stopped, the page's requests must reach the network again"
        );
    }

    /// Dropping the check without stopping it still delivers the refusal it is sending, then
    /// turns interception off, so the browser is not left paused with nothing answering.
    ///
    /// ~keep Both watches go: one parked before, one dropped right after the check, so the
    /// ~keep listener's commands close while it is still delivering. The parked page is the
    /// ~keep probe: its requests belong to no watch, so they are refused until interception is
    /// ~keep off, and reach the network after.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_check_delivers_its_refusals_and_turns_interception_off() {
        let test_name = "dropping_the_check_delivers_its_refusals_and_turns_interception_off";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            deliver: Duration::from_millis(500),
            ..TestDelays::default()
        };
        let firewall = BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, delays)
            .await
            .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let parked = browser.new_page("about:blank").await.expect("page");
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        let parked_watch = firewall
            .handle()
            .watch(&parked, &policy(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        open_blank_site(&parked).await;
        parked_watch.park().await;
        let (probe, probe_hits) = denied_listener().await;
        let _ = parked
            .evaluate(format!(
                "setInterval(() => fetch({probe:?}, {{ mode: 'no-cors' }}).catch(() => 0), 50); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reached_while_on = probe_hits.load(Ordering::SeqCst);
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(firewall);
        drop(watch);
        let reached_after_drop = served(&probe_hits).await;
        drop((page, parked));
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }
        assert_eq!(
            reached_while_on, 0,
            "{test_name}: a parked page's requests must be refused while interception is on"
        );
        assert_eq!(
            denied_hits.load(Ordering::SeqCst),
            0,
            "{test_name}: the request refused while the check was dropped must not reach the denied address"
        );
        assert!(
            reached_after_drop,
            "{test_name}: a dropped check must turn interception off, or the browser's requests stay paused"
        );
    }

    /// A server on `localhost` that answers `/nc...` with a 204 and anything else with a page.
    async fn no_content_site() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 2048];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]);
                    let response: &[u8] = if request.starts_with("GET /nc") {
                        b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n"
                    } else {
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 13\r\nconnection: close\r\n\r\n<p>page</p>\r\n"
                    };
                    let _ = stream.write_all(response).await;
                });
            }
        });
        format!("http://localhost:{port}/")
    }

    /// The watch records a commit before it judges a later response, so a main-frame response
    /// Chrome does not commit drops every older record but the committed document's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_watch_keeps_only_the_committed_document_and_the_newest_response() {
        use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;

        let test_name = "a_watch_keeps_only_the_committed_document_and_the_newest_response";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall =
            BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, TestDelays::default())
                .await
                .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let watch = firewall
            .handle()
            .watch(&page, &policy(), 0)
            .await
            .expect("the watch must start");
        let site = no_content_site().await;
        page.goto(site).await.expect("the test page must load");
        for target in ["/nc1", "/nc2"] {
            let _ = page.evaluate(format!("location.href = {target:?}; 1")).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let committed: String = page
            .execute(GetFrameTreeParams::default())
            .await
            .expect("the frame tree must be read")
            .result
            .frame_tree
            .frame
            .loader_id
            .into();
        let mut kept: Vec<(String, u16)> = super::lock(&watch.page.outcome)
            .documents
            .iter()
            .map(|(id, document)| (id.clone(), document.status))
            .collect();
        kept.sort_unstable_by_key(|(_, status)| *status);
        watch.close().await;
        firewall.stop().await;
        assert_eq!(
            kept.len(),
            2,
            "{test_name}: only the committed document and the newest response must be kept, got {kept:?}"
        );
        assert_eq!(
            kept[0],
            (committed, 200),
            "{test_name}: the committed document must be kept"
        );
        assert_eq!(kept[1].1, 204, "{test_name}: the newest response must be kept");
    }

    /// A page watched while the check is stopping is refused, not left unchecked once
    /// interception turns off.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_watched_while_the_check_stops_is_refused() {
        let test_name = "a_page_watched_while_the_check_stops_is_refused";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            deliver: Duration::from_millis(500),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, _hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let handle = firewall.handle();
        let stopping = tokio::spawn(firewall.stop());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let late = browser.new_page("about:blank").await.expect("page");
        let watched = handle.watch(&late, &policy(), 0).await;
        let _ = stopping.await;
        drop((watch, page, late));
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }
        assert!(
            watched.is_err(),
            "{test_name}: a watch that starts while the check stops must fail"
        );
    }

    /// A page whose main frame cannot be read is not watched: the redirect limit could not be
    /// applied to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_without_a_main_frame_is_not_watched() {
        let test_name = "a_page_without_a_main_frame_is_not_watched";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall =
            BrowserFirewall::start_with(Arc::clone(&browser), BrowserOrigin::Launched, TestDelays::default())
                .await
                .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let closed = page.clone();
        let _ = page.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let watched = firewall.handle().watch(&closed, &policy(), 0).await;
        firewall.stop().await;
        let error = watched.err().map(|error| error.to_string()).unwrap_or_default();
        assert!(
            error.contains("cannot apply the redirect limit"),
            "{test_name}: a page with no readable main frame must be refused, got {error:?}"
        );
    }

    /// Close `browser` once the test is done with it.
    async fn close(browser: Arc<Browser>) {
        if let Some(mut browser) = Arc::into_inner(browser) {
            let _ = browser.close().await;
            let _ = browser.wait().await;
        }
    }

    /// Whether `condition` holds within five seconds.
    async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..2500 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        false
    }

    /// Once nothing is watched, interception stays on for `DISABLE_DRAIN` after the last
    /// refusal, so a request of a page just parked that is still on its way to the check is
    /// refused, and turns off once that time has passed.
    ///
    /// ~keep The test times the listener's drain, not Chrome: the probe reports the moment the
    /// ~keep idle disable fires and the listener's own stamp of the last refusal, so no CDP round
    /// ~keep trip enters the measurement. A page that keeps sending cannot show the drain, since
    /// ~keep Chrome's turnaround between a refusal and the page's next request exceeds the drain
    /// ~keep on a busy host (measured: 5 of 40 runs at load 100 or more), and the disable then
    /// ~keep fires by design. The injected verdict delay holds the page's request in judgement
    /// ~keep until the park, so the refusal lands as the park returns, as a slow DNS lookup on a
    /// ~keep page being released does for real. Without the drain the disable fires in the first
    /// ~keep moment nothing is watched and nothing is unanswered.
    #[tokio::test(flavor = "multi_thread")]
    async fn interception_stays_on_for_the_drain_after_the_last_refusal() {
        let test_name = "interception_stays_on_for_the_drain_after_the_last_refusal";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (probe, mut disables) = tokio::sync::mpsc::unbounded_channel();
        let delays = TestDelays {
            verdict: Duration::from_millis(500),
            disabled: Some(probe),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let taken = wait_until(|| watch.page.in_flight.load(Ordering::Acquire) > 0).await;
        watch.park().await;
        let disabled = tokio::time::timeout(Duration::from_secs(5), disables.recv()).await;
        let (after, after_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({after:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let reached_after = served(&after_hits).await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            taken,
            "{test_name}: the check must take the page's request before the park"
        );
        let Ok(Some((disabled, last_refused))) = disabled else {
            panic!("{test_name}: the idle disable must fire once the page is parked");
        };
        let Some(last_refused) = last_refused else {
            panic!("{test_name}: the refusal of the parked page's request must be recorded before the disable");
        };
        let held = disabled.saturating_duration_since(last_refused);
        assert!(
            held >= DISABLE_DRAIN,
            "{test_name}: the idle disable must wait for the drain after the last refusal, fired {held:?} after it"
        );
        assert_eq!(
            denied_hits.load(Ordering::SeqCst),
            0,
            "{test_name}: the request refused as the page was parked must reach nothing"
        );
        assert!(
            reached_after,
            "{test_name}: once the drain has passed, interception must be off"
        );
    }

    /// A watch dropped without being closed, as by a cancelled fetch, closes its page.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_watch_closes_its_page() {
        use chromiumoxide::cdp::browser_protocol::target::GetTargetsParams;

        let test_name = "a_dropped_watch_closes_its_page";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch) = watched_page(&browser, TestDelays::default()).await;
        let target = page.target_id().clone();
        let open = |browser: Arc<Browser>, target: chromiumoxide::cdp::browser_protocol::target::TargetId| async move {
            browser
                .execute(GetTargetsParams::default())
                .await
                .map(|response| response.result.target_infos.iter().any(|info| info.target_id == target))
                .unwrap_or(false)
        };
        let open_while_watched = open(Arc::clone(&browser), target.clone()).await;
        drop(watch);
        let mut still_open = true;
        for _ in 0..50 {
            still_open = open(Arc::clone(&browser), target.clone()).await;
            if !still_open {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            open_while_watched,
            "{test_name}: the page must be open while it is watched"
        );
        assert!(!still_open, "{test_name}: a dropped watch must close its page");
    }

    /// A stop waits only for the answers it found started, so a page that keeps sending cannot
    /// hold it off.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_that_keeps_sending_does_not_hold_the_stop_off() {
        let test_name = "a_page_that_keeps_sending_does_not_hold_the_stop_off";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            deliver: Duration::from_millis(300),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, _hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 5); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stopped = tokio::time::timeout(Duration::from_secs(5), firewall.stop()).await;
        drop((watch, page));
        close(browser).await;
        assert!(
            stopped.is_ok(),
            "{test_name}: the stop must finish while the page keeps sending"
        );
    }

    /// A refusal counts by the time the listener receives the pause. When the listener takes a
    /// pause in only after the action's grace, as on a busy host, the refusal counts for the
    /// next action, not the one that sent the request.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pause_the_listener_receives_late_counts_for_the_next_action() {
        let test_name = "a_pause_the_listener_receives_late_counts_for_the_next_action";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            receive: Duration::from_millis(300),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, _hits) = denied_listener().await;
        let sent = Instant::now();
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let for_the_sender = watch.refusal_during(sent, ACTION_GRACE).await;
        let next = Instant::now();
        let for_the_next = watch.refusal_during(next, Duration::from_millis(600)).await;
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert_eq!(
            for_the_sender, None,
            "{test_name}: a pause received after the grace must not count for the action that sent it"
        );
        assert!(
            for_the_next.is_some_and(|(url, _)| url == denied),
            "{test_name}: the late pause must count for the next action"
        );
    }

    /// The refused URLs are read once the requests the check has taken are judged, so a request
    /// whose DNS lookup is still running is listed.
    #[cfg(feature = "browser")]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_refused_urls_include_a_request_still_being_judged() {
        let test_name = "the_refused_urls_include_a_request_still_being_judged";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let delays = TestDelays {
            verdict: Duration::from_millis(400),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        let (denied, _hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let refused = watch.refused_urls().await;
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert_eq!(
            refused,
            [denied],
            "{test_name}: the request being judged must be listed"
        );
    }
}
