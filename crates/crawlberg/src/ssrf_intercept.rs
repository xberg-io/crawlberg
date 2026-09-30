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
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, HeaderEntry, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{
    Cookie, CookieParam, ErrorReason, Headers, ResourceType, TimeSinceEpoch,
};
use chromiumoxide::cdp::browser_protocol::page::{EventFrameNavigated, FrameId};
use chromiumoxide::cdp::browser_protocol::storage::{
    GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
};
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, CreateBrowserContextParams, CreateTargetParams, EventTargetCreated, EventTargetDestroyed,
    GetTargetsParams, TargetId,
};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesUnordered, SelectAll, StreamExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::CrawlError;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::LOGGED_REFUSALS;
use crate::net::credentials::seed_host_headers;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::net::userinfo;
use crate::types::CrawlConfig;

/// What an intercepted request is recorded as when it does not parse, so its text is never echoed.
const UNPARSEABLE_URL: &str = "(unparseable URL)";

/// How long closing a watched page and its popups may take before the watch ends anyway.
///
/// ~keep The wait ends at Chrome's destroy events, so the bound only ends the wait of a Chrome
/// ~keep that never sends them. It is longer than `ACTION_SETTLE_LIMIT`, which waits only for the
/// ~keep check's own verdicts, because this waits for Chrome's renderer to let the page go.
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
/// that answers every paused request of every target in that browser. Interception is on from
/// the start of the check to its stop. Pages are opened with [`FirewallHandle::new_page`], in
/// the context [`PageContext`] names, and put under the check with [`FirewallHandle::watch`].
/// Each request is judged by the policy of the watched page it belongs to: the page itself, a
/// frame in it, or a popup it opened, directly or through another popup.
///
/// A request that belongs to another client's page of an external browser is continued
/// untouched. Any other request that belongs to no watched page is refused: on a browser
/// crawlberg launched every page is crawlberg's, and a frame that cannot be placed at all
/// is refused on either kind.
///
/// ~keep CDP Fetch interception is per session. Enabled on a page's session it pauses only
/// ~keep that page's requests, and chromiumoxide 0.9.1 attaches a popup after it runs and
/// ~keep releases an out-of-process frame or a worker itself (`Runtime.runIfWaitingForDebugger`
/// ~keep in `handler/target.rs`), so their first request would leave before a page-level
/// ~keep interception could be enabled on them. Enabled on the browser session, it pauses every
/// ~keep target's requests. A browser serves several pages at once (a `BrowserPool` hands out
/// ~keep one tab per concurrent fetch), and a second Fetch listener on the same session would
/// ~keep answer the same paused requests, so one listener per browser serves them all.
/// ~keep
/// ~keep Chrome continues every paused request the moment interception is turned off, including
/// ~keep the ones the listener has not received yet, so interception is never turned off while
/// ~keep a page of the check can still send: measured on #189, a `Fetch.disable` 100 ms after
/// ~keep the last refusal let 2 to 31 requests of a torn-down page reach a denied address in 4
/// ~keep of 120 loaded runs (xberg-io/crawlberg#506). Instead each page of a browser that outlives
/// ~keep the check lives in its own browser context, and ending its watch disposes that context:
/// ~keep the page, its popups and their pending requests die with the context's network stack,
/// ~keep whatever has reached the listener (measured: a `Fetch.disable` with pauses outstanding
/// ~keep continued 9 to 71 of them in 3 of 3 runs; a page disposed with its context before the
/// ~keep stop leaked in 0 of 15). A `browser_profile` needs the browser's own context, whose
/// ~keep storage is the profile's; its browser is launched for the one session and closed after
/// ~keep the check, so there interception is never turned off at all (measured: pauses left
/// ~keep outstanding under interception until the browser closed reached nothing, 3 of 3).
pub(crate) struct BrowserFirewall {
    handle: FirewallHandle,
    listener: tokio::task::JoinHandle<()>,
    stopped: bool,
}

/// Whether crawlberg launched the browser or connected to one through `browser.endpoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserOrigin {
    /// crawlberg started the process, so every page in it is crawlberg's.
    Launched,
    /// Another program owns the browser, and its other pages are that program's.
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
}

/// The browser context the check opens its pages in.
///
/// ~keep A created context is an incognito-like jar: it starts empty and its storage dies with
/// ~keep it, and only its disposal takes a page's pending requests along (xberg-io/crawlberg#506).
/// ~keep A `browser.endpoint` Chrome has its owner's cookies, so those are copied in. A
/// ~keep `browser_profile` promises a session the profile's cookies and localStorage, and a copy
/// ~keep of the cookies is not that: it lost the localStorage and brought back a cookie the page
/// ~keep had deleted (4 of 4 runs each). A profile session runs on a Chrome
/// ~keep launched for it alone, so its page uses the browser's own context and the check leaves
/// ~keep interception on until that Chrome is closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PageContext {
    /// A context of its own that starts empty; the page's storage dies with it.
    Isolated,
    /// A context of its own that starts with the browser's cookies; the page's own die with it.
    Copied,
    /// The browser's own context: the page reads and writes the browser's storage. Interception
    /// stays on when the check stops; the browser is closed after it.
    Shared,
}

impl PageContext {
    /// The context a one-shot session with `config` opens its page in: an external browser's
    /// cookies reach the page, a named profile's storage is the page's, a launched browser
    /// without a profile gives it nothing.
    #[cfg(feature = "browser")]
    pub(crate) fn of(config: &CrawlConfig) -> Self {
        if config.browser.endpoint.is_some() {
            Self::Copied
        } else if config.browser_profile.is_some() {
            Self::Shared
        } else {
            Self::Isolated
        }
    }

    /// The context a browser reached through `endpoint`, if one is configured, opens its pages
    /// in: an external browser's cookies are its owner's and reach the page; a launched browser
    /// has none.
    pub(crate) fn of_endpoint(endpoint: Option<&str>) -> Self {
        if endpoint.is_some() {
            Self::Copied
        } else {
            Self::Isolated
        }
    }
}

/// A cheap, cloneable reference to a [`BrowserFirewall`], used to open and watch pages.
#[derive(Clone)]
pub(crate) struct FirewallHandle {
    commands: mpsc::UnboundedSender<Command>,
    /// The browser, held weakly so the owner's `Arc::into_inner` still finds it alone once the
    /// check has stopped.
    browser: Weak<Browser>,
    context: PageContext,
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
    /// The crawl's config: the SSRF policy every request is judged by, and the headers a
    /// request to the seed's host gets.
    config: CrawlConfig,
    redirect_limit: usize,
    outcome: Mutex<InterceptOutcome>,
    refusals: Mutex<Vec<Refusal>>,
    /// Every URL the SSRF policy refused for the page, credential-redacted, each once.
    refused_urls: Mutex<Vec<String>>,
    /// How many requests of the page the policy refused, for the bound on the warnings.
    refused_count: AtomicUsize,
    /// Set when the watch ends: from then on every request of the page is refused.
    ending: AtomicBool,
    /// Requests of the page that are paused and whose answer has not been sent yet.
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
    /// Every live target no watched page owns: another client's page on an external browser,
    /// or a browser's own tab.
    others: HashSet<TargetId>,
    /// In-process frames, filled in as requests name them: the watched page that owns the
    /// frame, or `None` for a frame of another target.
    frames: HashMap<FrameId, Option<Arc<WatchedPage>>>,
    /// Every page the check opened, by its own target, with the browser context it lives in when
    /// it has one of its own. The page goes when its watch ends with it, when Chrome destroys it,
    /// or when the check stops; a context of its own, and its popups, go with it.
    opened: HashMap<TargetId, Option<BrowserContextId>>,
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
    context: PageContext,
    #[cfg(test)]
    delays: TestDelays,
}

/// Delays a unit test injects to widen a race window deterministically.
#[cfg(test)]
#[derive(Clone, Default)]
struct TestDelays {
    /// Before a request's SSRF verdict, as a slow DNS lookup would take.
    verdict: Duration,
    /// Between a request's verdict and the answer that delivers it to Chrome.
    deliver: Duration,
    /// Before a page opened while the check stops is dropped, until the test gives the gate a
    /// permit.
    drop_late_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Between a request's verdict and its delivery, until the test gives the gate a permit.
    deliver_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Before the listener takes in a paused request, until the test gives the gate a permit.
    receive_gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Before a request's SSRF verdict, until the test gives the gate a permit.
    verdict_gate: Option<Arc<UrlGate>>,
    /// Between the verdict and the delivery of a request that belongs to no watched page, until
    /// the test gives the gate a permit. Such a request waits on this gate in place of the
    /// delivery gate.
    unwatched_deliver_gate: Option<Arc<UrlGate>>,
    /// The URLs of the paused requests the listener has taken in, recorded as each is taken in.
    received: Arc<Mutex<Vec<String>>>,
    /// The pages the listener has dropped, recorded as each drop starts.
    dropped: Arc<Mutex<Vec<TargetId>>>,
}

/// Holds each request until the test gives a permit, and lists the URLs it has held, so a test
/// knows which request the listener has taken in.
#[cfg(test)]
struct UrlGate {
    permits: tokio::sync::Semaphore,
    held: Mutex<Vec<String>>,
}

enum Command {
    /// The check opened a page: its own target, and its browser context when it has one of its
    /// own.
    Opened(TargetId, Option<BrowserContextId>),
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
    /// Drop every page of the check that is left, turn interception off (unless the pages
    /// share the browser's context) once that and every answer and every watch end already
    /// started have finished, then stop the listener. `done` is told when the listener stops.
    Stop(Option<oneshot::Sender<()>>),
}

/// What a finished task of the listener reports back to it.
enum Done {
    Answered,
    Closed,
    Dropped,
    Ended(Arc<WatchedPage>, bool, Option<oneshot::Sender<()>>),
}

impl BrowserFirewall {
    /// Turn interception on for `browser` and start the listener on its session. The check
    /// opens its pages in the context `context` names.
    pub(crate) async fn start(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
    ) -> Result<Self, CrawlError> {
        Self::start_with(
            browser,
            origin,
            context,
            #[cfg(test)]
            TestDelays::default(),
        )
        .await
    }

    async fn start_with(
        browser: Arc<Browser>,
        origin: BrowserOrigin,
        context: PageContext,
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
        browser
            .execute(fetch_enable_params())
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;
        let shared = Shared {
            registry: Mutex::new(Registry {
                others: existing.into_iter().map(|info| info.target_id).collect(),
                ..Registry::default()
            }),
            destroyed: Notify::new(),
            origin,
            context,
            #[cfg(test)]
            delays,
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        let handle = FirewallHandle {
            commands,
            browser: Arc::downgrade(&browser),
            context,
        };
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
            handle,
            listener,
            stopped: false,
        })
    }

    pub(crate) fn handle(&self) -> FirewallHandle {
        self.handle.clone()
    }

    /// Drop every page of the check that is left, so no page of it can still send, turn
    /// interception off once that and every answer the check had started when asked to stop are
    /// done, stop the listener, and release its reference to the browser, so the owner can close
    /// it. Call it once no page of the browser needs the check any more.
    ///
    /// ~keep A stop that only ended the listener would leave interception on with nothing
    /// ~keep answering: every request of every target in the browser then stays paused for good;
    /// ~keep on a `browser.endpoint` Chrome, that is the user's own tabs, permanently.
    /// ~keep Nor is the disable sent from here: a disable that lands between a refusal's verdict
    /// ~keep and its delivery lets the refused request through. The listener sends it once the
    /// ~keep answers it has started are delivered. On a `PageContext::Shared` check it is not
    /// ~keep sent at all: the pages used the browser's own context, so no disposal took their
    /// ~keep pending requests before it. The browser, launched for this one session, is closed
    /// ~keep instead. This is a precaution: a disable here was not seen to leak (0 of 13 runs).
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
    // ~keep interception off, then ends and lets go of the browser.
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

/// Drop the page `root` the check opened: dispose its browser context `context` when it has one
/// of its own, and with it every page in it and every request of theirs Chrome still holds;
/// otherwise close the page's target.
///
/// ~keep Closing a target under browser-wide interception lets none of its pending requests
/// ~keep out (measured: 854 to 6674 pauses outstanding at the close, 0 reached, 3 of 3 runs);
/// ~keep what takes them out is turning interception off, which a shared-context check never does.
async fn drop_page(browser: &Browser, root: TargetId, context: Option<BrowserContextId>) {
    match context {
        Some(context) => dispose_context(browser, context).await,
        None => {
            if let Err(error) = browser.execute(CloseTargetParams::new(root)).await {
                tracing::debug!(%error, "failed to close a page of the check");
            }
        }
    }
}

/// Dispose the browser context `context`, and with it every page in it and every request of
/// theirs Chrome still holds.
async fn dispose_context(browser: &Browser, context: BrowserContextId) {
    if let Err(error) = browser.dispose_browser_context(context).await {
        tracing::debug!(%error, "failed to dispose a page's browser context");
    }
}

/// Copy every cookie of the browser's own context into the browser context `context`. Returns
/// how many cookies were copied.
async fn copy_cookies(browser: &Browser, context: BrowserContextId) -> Result<usize, String> {
    let cookies = browser
        .execute(StorageGetCookiesParams {
            browser_context_id: None,
        })
        .await
        .map_err(|e| e.to_string())?
        .result
        .cookies;
    let count = cookies.len();
    if count == 0 {
        return Ok(0);
    }
    browser
        .execute(StorageSetCookiesParams {
            cookies: cookies.into_iter().map(cookie_param).collect(),
            browser_context_id: Some(context),
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(count)
}

/// A stored cookie as the parameter that sets it again. A session cookie has no expiry.
fn cookie_param(cookie: Cookie) -> CookieParam {
    CookieParam {
        name: cookie.name,
        value: cookie.value,
        url: None,
        domain: Some(cookie.domain),
        path: Some(cookie.path),
        secure: Some(cookie.secure),
        http_only: Some(cookie.http_only),
        same_site: cookie.same_site,
        expires: (!cookie.session).then(|| TimeSinceEpoch::new(cookie.expires)),
        priority: Some(cookie.priority),
        same_party: None,
        source_scheme: Some(cookie.source_scheme),
        source_port: Some(cookie.source_port),
        partition_key: cookie.partition_key,
    }
}

impl FirewallHandle {
    /// Open a blank page for [`Self::watch`], in a browser context of its own or in the
    /// browser's, as the check's [`PageContext`] says. A context of its own starts with the
    /// browser's cookies when the check copies them. The page goes, with its popups, when its
    /// watch ends with it, when Chrome destroys it, or when the check stops.
    ///
    /// ~keep A created context is disposed with the debugging session too, so a check that ends
    /// ~keep without stopping (a crashed process) leaves no context in a `browser.endpoint` Chrome.
    pub(crate) async fn new_page(&self) -> Result<chromiumoxide::Page, CrawlError> {
        let stopped = || CrawlError::browser_error("request interception stopped");
        let browser = self.browser.upgrade().ok_or_else(stopped)?;
        let failed = |e: &dyn std::fmt::Display| CrawlError::browser_error(format!("failed to create page: {e}"));
        let context = match self.context {
            PageContext::Shared => None,
            PageContext::Isolated | PageContext::Copied => Some(
                browser
                    .create_browser_context(CreateBrowserContextParams {
                        dispose_on_detach: Some(true),
                        ..CreateBrowserContextParams::default()
                    })
                    .await
                    .map_err(|e| failed(&e))?,
            ),
        };
        if self.context == PageContext::Copied
            && let Some(context) = &context
            && let Err(error) = copy_cookies(&browser, context.clone()).await
        {
            dispose_context(&browser, context.clone()).await;
            return Err(CrawlError::browser_error(format!(
                "failed to copy the browser's cookies into the page: {error}"
            )));
        }
        let mut params = CreateTargetParams::new("about:blank");
        params.browser_context_id = context.clone();
        let page = match browser.new_page(params).await {
            Ok(page) => page,
            Err(error) => {
                if let Some(context) = context {
                    dispose_context(&browser, context).await;
                }
                return Err(failed(&error));
            }
        };
        let root = page.target_id().clone();
        if self
            .commands
            .send(Command::Opened(root.clone(), context.clone()))
            .is_err()
        {
            drop_page(&browser, root, context).await;
            return Err(stopped());
        }
        Ok(page)
    }

    /// Put `page`, opened with [`Self::new_page`], under the check with `config`'s SSRF policy,
    /// counting its main-frame redirects against `redirect_limit`. A request of the page to the
    /// seed's host gets the seed-host headers. A page the check did not open is refused.
    pub(crate) async fn watch(
        &self,
        page: &chromiumoxide::Page,
        config: &CrawlConfig,
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
            config: config.clone(),
            redirect_limit,
            outcome: Mutex::new(InterceptOutcome::default()),
            refusals: Mutex::new(Vec::new()),
            refused_urls: Mutex::new(Vec::new()),
            refused_count: AtomicUsize::new(0),
            ending: AtomicBool::new(false),
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
            Ok(Err(e)) => Err(CrawlError::browser_error(e)),
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

    /// Close the page and every popup it opened: a page in a context of its own goes with the
    /// context, and every request of theirs Chrome still holds goes with it. End the watch once
    /// Chrome has destroyed them. From now on the page's requests are refused.
    pub(crate) async fn close(self) {
        self.end(true).await;
    }

    /// Close the popups the page opened and end the watch, keeping the page open for reuse. The
    /// page's requests are refused until it is watched again; the page, and a context of its
    /// own, go when Chrome destroys the page, or with the check.
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

/// The listener. It runs the watch commands in order, tracks the targets each watched page owns
/// and the pages the check opened, and answers the paused requests concurrently, so a slow DNS
/// lookup for one page does not hold up the others. Interception is turned off only on a stop,
/// once every page of the check is dropped and every answer and every watch end already
/// started has finished, and never when the pages share the browser's own context.
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
    loop {
        if draining.is_empty()
            && let Some(stopped) = stopping.take()
        {
            // ~keep Pages in the browser's own context had no context to dispose, so no disposal
            // ~keep took their pending requests before a disable. Interception stays on as a
            // ~keep precaution (a disable here was not seen to leak, 0 of 13 runs); the owner
            // ~keep closes the browser, which was launched for this one session, and the pauses
            // ~keep die with it (measured 0 reached in 3 of 3 runs).
            if shared.context != PageContext::Shared {
                disable_fetch(browser).await;
            }
            for done in stopped {
                let _ = done.send(());
            }
            break;
        }
        tokio::select! {
            command = commands.recv(), if commands_open => match command {
                // ~keep A page opened while the check stops is dropped at once: its watch is
                // ~keep refused below, and a page left behind would outlive the check. The drop
                // ~keep joins the drain, since the loop ends, and turns interception off, as
                // ~keep soon as the drain is empty, whatever is still running.
                Some(Command::Opened(root, context)) if stopping.is_some() => {
                    draining.push(Box::pin(async move {
                        #[cfg(test)]
                        if let Some(gate) = &shared.delays.drop_late_gate {
                            let _ = gate.acquire().await;
                        }
                        #[cfg(test)]
                        lock(&shared.delays.dropped).push(root.clone());
                        drop_page(browser, root, context).await;
                        Done::Dropped
                    }));
                }
                Some(Command::Opened(root, context)) => {
                    lock(&shared.registry).opened.insert(root, context);
                }
                Some(Command::Watch(_, _, ack)) if stopping.is_some() => {
                    let _ = ack.send(Err("request interception stopped".to_owned()));
                }
                Some(Command::Watch(page, navigated, ack)) => {
                    let mut registry = lock(&shared.registry);
                    if !registry.opened.contains_key(&page.root) {
                        drop(registry);
                        let _ = ack.send(Err(
                            "the SSRF check can only watch a page it opened; open the page with the check".to_owned(),
                        ));
                        continue;
                    }
                    registry.pages.push(Arc::clone(&page));
                    registry.targets.push((page.root.clone(), Arc::clone(&page)));
                    drop(registry);
                    navigations.push(commits_of(&page, navigated));
                    let _ = ack.send(Ok(()));
                }
                Some(Command::End { page, close_page, done }) => {
                    running.push(Box::pin(end_watch(browser, shared, page, close_page, done)));
                }
                Some(Command::Stop(done)) => {
                    let stopped = stopping.get_or_insert_with(|| {
                        draining.extend(std::mem::take(&mut running));
                        let left = std::mem::take(&mut lock(&shared.registry).opened);
                        for (root, context) in left {
                            draining.push(Box::pin(async move {
                                #[cfg(test)]
                                lock(&shared.delays.dropped).push(root.clone());
                                drop_page(browser, root, context).await;
                                Done::Dropped
                            }));
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
                    {
                        if let Some(gate) = &shared.delays.receive_gate {
                            let _ = gate.acquire().await;
                        }
                        lock(&shared.delays.received).push(event.request.url.clone());
                    }
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
                    let context = registry.opened.remove(&event.target_id).flatten();
                    drop(registry);
                    shared.destroyed.notify_waiters();
                    // ~keep A parked page the session pool evicts, or a pooled page dropped before
                    // ~keep its watch, is closed by its owner, not through a watch; its context
                    // ~keep goes here, and its popups with it.
                    if let Some(context) = context {
                        running.push(Box::pin(async move {
                            dispose_context(browser, context).await;
                            Done::Dropped
                        }));
                    }
                }
            }
            Some(done) = running.next(), if !running.is_empty() => settle(shared, done),
            Some(done) = draining.next(), if !draining.is_empty() => settle(shared, done),
        }
    }
}

/// Take in what a finished task of the listener reports.
fn settle(shared: &Shared, done: Done) {
    match done {
        Done::Answered | Done::Closed | Done::Dropped => {}
        Done::Ended(page, keep_root, done) => {
            release(shared, &page, keep_root);
            if let Some(done) = done {
                let _ = done.send(());
            }
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
        (None, Some(parent)) => match registry.owner_of_frame(parent) {
            Some(Owner::Watched(page)) => Some(page),
            _ => None,
        },
        (None, None) => None,
    };
    let Some(owner) = owner else {
        registry.others.insert(info.target_id.clone());
        return None;
    };
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

/// End the watch of `page`. With `close_page` set, dispose the page's browser context when it
/// has one of its own, which takes the page, its popups and their pending requests, or close the
/// page and its popups; otherwise close the popups it opened, children first, and keep the page.
/// Wait until Chrome has destroyed them and every request the page sent is answered, then report
/// back. The page's requests are refused throughout.
async fn end_watch(
    browser: &Browser,
    shared: &Shared,
    page: Arc<WatchedPage>,
    close_page: bool,
    done: Option<oneshot::Sender<()>>,
) -> Done {
    page.ending.store(true, Ordering::Release);
    let keep_root = !close_page;
    // ~keep Taken out of the registry first, so the destroy events do not dispose it again.
    let context = close_page
        .then(|| lock(&shared.registry).opened.remove(&page.root))
        .flatten()
        .flatten();
    let disposed = context.is_some();
    if let Some(context) = context {
        dispose_context(browser, context).await;
    }
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
            // ~keep A popup opened meanwhile is closed as it appears. A disposed context's
            // ~keep targets are already going; closing them again is refused and harmless.
            if !disposed {
                for target in &open {
                    let _ = browser.execute(CloseTargetParams::new(target.clone())).await;
                }
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
    let (verdict, _in_flight) = match attribute(browser, shared, &event.frame_id).await {
        None => (Verdict::Refuse, None),
        Some(Owner::Other) if shared.origin == BrowserOrigin::External => (Verdict::Continue(None), None),
        Some(Owner::Other) => (Verdict::Refuse, None),
        Some(Owner::Watched(page)) => {
            let in_flight = InFlight::enter(page);
            let page = &in_flight.0;
            let verdict = judge(shared, page, event, paused_at).await;
            let verdict = if page.ending.load(Ordering::Acquire) {
                Verdict::Refuse
            } else {
                verdict
            };
            (verdict, Some(in_flight))
        }
    };
    #[cfg(test)]
    {
        tokio::time::sleep(shared.delays.deliver).await;
        if _in_flight.is_none()
            && let Some(gate) = &shared.delays.unwatched_deliver_gate
        {
            lock(&gate.held).push(event.request.url.clone());
            let _ = gate.permits.acquire().await;
        } else if let Some(gate) = &shared.delays.deliver_gate {
            let _ = gate.acquire().await;
        }
    }
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
    let _ = match verdict {
        Verdict::Continue(headers) => {
            let mut params = ContinueRequestParams::new(request_id);
            params.headers = headers;
            browser.execute(params).await.map(drop)
        }
        Verdict::Refuse => browser
            .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
            .await
            .map(drop),
    };
}

/// How the check answers one paused request.
enum Verdict {
    /// Let it go out, with these headers in place of its own when set.
    Continue(Option<Vec<HeaderEntry>>),
    /// Fail it with `BlockedByClient`.
    Refuse,
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

async fn judge(shared: &Shared, page: &WatchedPage, event: &EventRequestPaused, paused_at: Instant) -> Verdict {
    if page.ending.load(Ordering::Acquire) {
        return Verdict::Refuse;
    }
    if is_response_stage(event) {
        return if main_frame_verdict(event, &page.main_frame, page.redirect_limit, &page.outcome) {
            Verdict::Continue(None)
        } else {
            Verdict::Refuse
        };
    }
    #[cfg(test)]
    {
        tokio::time::sleep(shared.delays.verdict).await;
        if let Some(gate) = &shared.delays.verdict_gate {
            lock(&gate.held).push(event.request.url.clone());
            let _ = gate.permits.acquire().await;
        }
    }
    #[cfg(not(test))]
    let _ = shared;
    let (url, reason) = match ssrf_verdict(&event.request.url, &page.config.ssrf).await {
        Ok(parsed) => {
            return Verdict::Continue(headers_with_seed_host_headers(
                &page.config,
                &parsed,
                &event.request.headers,
            ));
        }
        Err(refused) => refused,
    };
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
    Verdict::Refuse
}

/// Decide whether an intercepted request URL may go out.
///
/// Returns the parsed URL, or `Err((recorded_url, reason))` when the request must be failed
/// at the CDP layer. A URL with userinfo is refused, as the Fetch standard does for
/// subresources, and is recorded without it. This is the per-request decision applied to
/// every browser-issued request.
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<url::Url, (String, String)> {
    let parsed = url::Url::parse(request_url).map_err(|e| (UNPARSEABLE_URL.to_owned(), format!("invalid URL: {e}")))?;
    if userinfo::has_userinfo(&parsed) {
        let mut clean = parsed;
        userinfo::strip(&mut clean);
        return Err((clean.into(), "a URL with credentials in it is refused".to_owned()));
    }
    validate_url(&parsed, policy)
        .await
        .map_err(|e| (parsed.to_string(), e.to_string()))?;
    Ok(parsed)
}

/// The request's own headers plus the seed-host headers `url` gets, if it gets any: the
/// custom headers and the credential.
///
/// ~keep They go on this one request only, never through `Network.setExtraHTTPHeaders`,
/// ~keep which would give them to every host the page loads from. A redirect hop is paused
/// ~keep again and gets its own decision.
fn headers_with_seed_host_headers(config: &CrawlConfig, url: &url::Url, headers: &Headers) -> Option<Vec<HeaderEntry>> {
    let added = seed_host_headers(config, url);
    if added.is_empty() {
        return None;
    }
    let mut entries: Vec<HeaderEntry> = headers
        .inner()
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(existing, _)| !added.iter().any(|(name, _)| existing.eq_ignore_ascii_case(name)))
        .filter_map(|(existing, value)| value.as_str().map(|value| HeaderEntry::new(existing.clone(), value)))
        .collect();
    entries.extend(added.into_iter().map(|(name, value)| HeaderEntry::new(name, value)));
    Some(entries)
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

    use super::{EventRequestPaused, FrameId, InterceptOutcome, main_frame_verdict, require_main_frame, ssrf_verdict};
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

    #[tokio::test]
    async fn a_url_with_userinfo_is_refused_and_recorded_without_it() {
        let Err((recorded, reason)) = ssrf_verdict("http://user:s3cret@example.com/a", &deny_policy()).await else {
            panic!("a URL with userinfo must be refused");
        };
        assert_eq!(recorded, "http://example.com/a");
        assert!(reason.contains("credentials"), "{reason}");
    }

    #[tokio::test]
    async fn a_malformed_url_is_recorded_without_its_text() {
        let Err((recorded, reason)) = ssrf_verdict("http://user:s3cret@exa mple/", &deny_policy()).await else {
            panic!("a malformed URL must be refused");
        };
        assert_eq!(recorded, "(unparseable URL)");
        assert!(reason.contains("invalid URL"), "{reason}");
    }

    #[test]
    fn the_seed_host_headers_replace_page_headers_of_the_same_name_and_keep_the_rest() {
        use chromiumoxide::cdp::browser_protocol::network::Headers;

        use super::headers_with_seed_host_headers;
        use crate::types::{AuthConfig, CrawlConfig};

        let seed = url::Url::parse("http://example.com/").expect("test URL must parse");
        let config = CrawlConfig {
            auth: Some(AuthConfig::Bearer {
                token: "tok".to_owned(),
            }),
            custom_headers: std::collections::HashMap::from([("X-Custom".to_owned(), "configured".to_owned())]),
            credential_scope: crate::net::CredentialScope::for_seed(&seed, None),
            ..CrawlConfig::default()
        };
        let headers = Headers::new(serde_json::json!({
            "Cookie": "a=b", "authorization": "page-value", "x-custom": "page-value"
        }));

        let entries = headers_with_seed_host_headers(&config, &seed, &headers).expect("the seed host gets the headers");
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.value.as_str()))
            .collect();
        assert!(
            pairs.contains(&("Cookie", "a=b")),
            "the page's headers are kept: {pairs:?}"
        );
        let authorization: Vec<&(&str, &str)> = pairs
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .collect();
        assert_eq!(authorization, vec![&("Authorization", "Bearer tok")], "{pairs:?}");
        let custom: Vec<&(&str, &str)> = pairs
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-custom"))
            .collect();
        assert_eq!(custom, vec![&("X-Custom", "configured")], "{pairs:?}");

        let other = url::Url::parse("http://other.test/").expect("test URL must parse");
        assert!(headers_with_seed_host_headers(&config, &other, &headers).is_none());
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
    use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
    use chromiumoxide::cdp::browser_protocol::network::CookieParam;
    use chromiumoxide::cdp::browser_protocol::storage::{
        GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
    };
    use chromiumoxide::cdp::browser_protocol::target::{GetBrowserContextsParams, GetTargetsParams};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_stream::StreamExt;

    use super::{ACTION_GRACE, BrowserFirewall, BrowserOrigin, PageContext, TestDelays, UrlGate};

    /// How long a test gives a stop to finish, or interception to turn off, while the check still
    /// holds a refusal it must deliver first. A stop that does not wait for the refusal finishes
    /// within milliseconds; one that waits cannot finish until the test lets the refusal go, so the
    /// bound only decides how long a correct run waits.
    const STOP_BOUND: Duration = Duration::from_secs(1);

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
        let launched = match crate::browser_pool::apply_default_args(builder, &[]).build() {
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
    fn config() -> crate::types::CrawlConfig {
        crate::types::CrawlConfig::builder()
            .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
            .build()
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
        served_within(hits, Duration::from_secs(5)).await
    }

    /// Whether `hits` records a connection within `bound`.
    async fn served_within(hits: &Arc<AtomicUsize>, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            if hits.load(Ordering::SeqCst) > 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// How many browser contexts the browser has besides its default one.
    async fn context_count(browser: &Browser) -> usize {
        contexts(browser).await.len()
    }

    /// The browser contexts the browser has besides its default one.
    async fn contexts(browser: &Browser) -> Vec<BrowserContextId> {
        browser
            .execute(GetBrowserContextsParams::default())
            .await
            .map(|response| response.result.browser_context_ids)
            .expect("the browser must list its contexts")
    }

    /// Set the cookie `name=value` for `http://localhost/` in the context `context`, `None` being
    /// the browser's own. The cookie lives an hour: Chrome drops a session cookie of its own
    /// context when the last window closes, as it does for any browser session.
    async fn set_cookie(browser: &Browser, context: Option<BrowserContextId>, name: &str, value: &str) {
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs_f64() + 3600.0)
            .unwrap_or(0.0);
        let cookie = CookieParam {
            url: Some("http://localhost/".to_owned()),
            expires: Some(super::TimeSinceEpoch::new(expires)),
            ..CookieParam::new(name, value)
        };
        browser
            .execute(StorageSetCookiesParams {
                cookies: vec![cookie],
                browser_context_id: context,
            })
            .await
            .expect("the cookie must be set");
    }

    /// The names of the cookies in the context `context`, `None` being the browser's own.
    async fn cookie_names(browser: &Browser, context: Option<BrowserContextId>) -> Vec<String> {
        let mut names: Vec<String> = browser
            .execute(StorageGetCookiesParams {
                browser_context_id: context,
            })
            .await
            .expect("the cookies must be listed")
            .result
            .cookies
            .into_iter()
            .map(|cookie| cookie.name)
            .collect();
        names.sort();
        names
    }

    /// Whether the browser still lists the target `target`.
    async fn target_is_open(
        browser: &Browser,
        target: &chromiumoxide::cdp::browser_protocol::target::TargetId,
    ) -> bool {
        browser
            .execute(GetTargetsParams::default())
            .await
            .map(|response| {
                response
                    .result
                    .target_infos
                    .iter()
                    .any(|info| info.target_id == *target)
            })
            .unwrap_or(false)
    }

    /// Stopping the check must turn interception off. The listener is the only thing answering
    /// paused requests, so a stop that only aborts it leaves interception on with nothing behind
    /// it: every request it covers stays paused for good, and on a browser that outlives the stop
    /// -- one reached through `browser.endpoint` -- that is another client's tabs, permanently.
    ///
    /// ~keep The probe is a tab outside the check, in the browser's own context: the check's own
    /// ~keep pages are gone when the check stops, so none of them can tell a frozen browser from a
    /// ~keep working one. On a launched browser its requests are refused while interception is
    /// ~keep on, which is asserted first, or the probe could not tell the two apart. The tab is
    /// ~keep opened after the check starts: measured, a tab that had loaded a document before
    /// ~keep interception was turned on is not paused at all (5 of 5 requests reached). It stays
    /// ~keep on about:blank, since the check refuses its navigation too.
    ///
    /// ~keep The probe is a `fetch`, not a navigation. Chrome pre-connects for a navigation, so
    /// ~keep the listener accepts a connection even while the request itself is paused -- measured:
    /// ~keep 2 connections while `goto` hung for its full 3 s timeout. Counting connections would
    /// ~keep have reported a frozen browser as a working one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tab_outside_the_check_reaches_the_network_after_the_check_is_stopped() {
        let test_name = "a_tab_outside_the_check_reaches_the_network_after_the_check_is_stopped";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch) = watched_page(&browser, TestDelays::default()).await;
        // ~keep The tab stays on about:blank: on a launched browser the check refuses every
        // ~keep request of a page it does not watch, its navigation included.
        let other = browser.new_page("about:blank").await.expect("page");
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let (probe, probe_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!(
                "setInterval(() => fetch({probe:?}, {{ mode: 'no-cors' }}).catch(() => 0), 50); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let refused_while_watched = denied_hits.load(Ordering::SeqCst);
        let reached_while_on = probe_hits.load(Ordering::SeqCst);
        watch.close().await;
        firewall.stop().await;
        let reached = served(&probe_hits).await;
        drop((page, other));
        close(browser).await;

        assert_eq!(
            refused_while_watched, 0,
            "{test_name}: the watched page's request must have been refused while it was watched"
        );
        assert_eq!(
            reached_while_on, 0,
            "{test_name}: the tab's requests must be refused while the check runs, or it cannot tell \
             a frozen browser from a working one"
        );
        assert!(
            reached,
            "{test_name}: the tab must reach the network after the check is stopped; \
             interception was left on with no listener answering, so its requests are paused for good"
        );
    }

    /// A page the check did not open is refused a watch: it lives in the browser's own context,
    /// so nothing could take its pending requests with it when its watch ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_the_check_did_not_open_is_not_watched() {
        let test_name = "a_page_the_check_did_not_open_is_not_watched";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::Launched,
            PageContext::Isolated,
            TestDelays::default(),
        )
        .await
        .expect("the listener must start");
        let page = browser.new_page("about:blank").await.expect("page");
        let watched = firewall.handle().watch(&page, &config(), 0).await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        let error = watched.err().map(|error| error.to_string()).unwrap_or_default();
        assert!(
            error.contains("only watch a page it opened"),
            "{test_name}: a page opened outside the check must be refused, got {error:?}"
        );
    }

    /// A request refused after a slow DNS lookup still counts for the action that sent it: it
    /// is timed when the listener took its pause in, not when its verdict came.
    ///
    /// ~keep The verdict gate holds the fetch's verdict until the action's grace has ended, as a
    /// ~keep slow lookup would. The grace is fixed only once the listener holds the fetch, so a
    /// ~keep busy host that takes the pause in late cannot move it past the cutoff.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_after_a_slow_lookup_counts_for_the_action_that_sent_it() {
        let test_name = "a_refusal_after_a_slow_lookup_counts_for_the_action_that_sent_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(UrlGate {
            permits: tokio::sync::Semaphore::new(1),
            held: std::sync::Mutex::default(),
        });
        let delays = TestDelays {
            verdict_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.permits.acquire().await.expect("the gate is never closed").forget();
        let (url, _hits) = denied_listener().await;
        let started = Instant::now();
        let _ = page
            .evaluate(format!("fetch({url:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::lock(&gate.held).contains(&url) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let held = super::lock(&gate.held).contains(&url);
        let mut counting = Box::pin(watch.refusal_during(started, ACTION_GRACE));
        let _ = futures::poll!(&mut counting);
        tokio::time::sleep(ACTION_GRACE).await;
        gate.permits.add_permits(1);
        let judged = wait_for_refusal(&watch, &url).await;
        let refused = counting.await;
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            held && judged,
            "{test_name}: the check must hold the fetch before its verdict and then refuse it, or the test shows nothing"
        );
        assert!(
            refused.is_some_and(|(refused_url, _)| refused_url == url),
            "{test_name}: the refusal must count for the action"
        );
    }

    /// Wait up to ten seconds for the check to refuse `url` for `watch`'s page; `false` if it
    /// never did. A refusal is recorded at its verdict, before it is delivered.
    async fn wait_for_refusal(watch: &super::Watch, url: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::lock(&watch.page.refused_urls)
            .iter()
            .any(|refused| refused == url)
        {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        true
    }

    /// Wait up to thirty seconds for Chrome to pause a request for `url`; `false` if it never did.
    /// The bound only stops a hang.
    async fn wait_for_pause(
        paused: &mut chromiumoxide::listeners::EventStream<super::EventRequestPaused>,
        url: &str,
    ) -> bool {
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(event) = paused.next().await {
                if event.request.url == url {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false)
    }

    /// Start a check with `delays`, open a page with it, watch it, and give it a real origin.
    async fn watched_page(
        browser: &Arc<Browser>,
        delays: TestDelays,
    ) -> (BrowserFirewall, chromiumoxide::Page, super::Watch) {
        watched_page_in(browser, PageContext::Isolated, delays).await
    }

    /// Start a check with `delays` that opens its pages in `context`, open a page with it, watch
    /// it, and give it a real origin.
    async fn watched_page_in(
        browser: &Arc<Browser>,
        context: PageContext,
        delays: TestDelays,
    ) -> (BrowserFirewall, chromiumoxide::Page, super::Watch) {
        let firewall = BrowserFirewall::start_with(Arc::clone(browser), BrowserOrigin::Launched, context, delays)
            .await
            .expect("the listener must start");
        let page = firewall.handle().new_page().await.expect("the check must open a page");
        let watch = firewall
            .handle()
            .watch(&page, &config(), 0)
            .await
            .expect("the watch must start");
        open_blank_site(&page).await;
        (firewall, page, watch)
    }

    /// A watch that ends has failed every request of its page it refused: the page sees the
    /// refused `fetch` rejected by the time `park` returns.
    ///
    /// ~keep The delivery gate holds the refusal between its verdict and the `Fetch.failRequest`
    /// ~keep that delivers it, where a slow CDP round trip would hold it. A park that returns
    /// ~keep while the gate is shut has not waited, and the page still sees the fetch pending.
    /// ~keep The half second only gives such a park time to return; a park that waits cannot
    /// ~keep return before the gate opens, since its wait is bounded at `CLOSE_TIMEOUT`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_request_is_failed_before_its_watch_ends() {
        let test_name = "a_refused_request_is_failed_before_its_watch_ends";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            deliver_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.acquire().await.expect("the gate is never closed").forget();
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!(
                "window.__probe = 'pending'; fetch({denied:?}, {{ mode: 'no-cors' }}).then(() => \
                 window.__probe = 'sent', () => window.__probe = 'refused'); 1"
            ))
            .await;
        let judged = wait_for_refusal(&watch, &denied).await;
        let mut parking = Box::pin(watch.park());
        if tokio::time::timeout(Duration::from_millis(500), &mut parking)
            .await
            .is_err()
        {
            gate.add_permits(1);
            parking.await;
        }
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
        gate.add_permits(1);
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            judged,
            "{test_name}: the check must refuse the page's fetch before the park, or the test shows nothing"
        );
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
    /// refused request is failed rather than let through when interception turns off. Once the
    /// check has stopped, a tab outside the check reaches the network again.
    ///
    /// ~keep The delivery gate holds the refusal, so the stop must still be waiting when the
    /// ~keep bound ends. The request itself cannot show a stop that skips it: disposing the page's
    /// ~keep context fails the held request anyway.
    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_the_check_still_fails_a_refused_request() {
        let test_name = "stopping_the_check_still_fails_a_refused_request";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            deliver_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        // ~keep The tab stays on about:blank: on a launched browser the check refuses every
        // ~keep request of a page it does not watch, its navigation included.
        let other = browser.new_page("about:blank").await.expect("page");
        let (before, before_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!("fetch({before:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reached_while_on = before_hits.load(Ordering::SeqCst);
        gate.acquire().await.expect("the gate is never closed").forget();
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let judged = wait_for_refusal(&watch, &denied).await;
        let mut stopping = Box::pin(firewall.stop());
        let stopped_before_delivery = tokio::time::timeout(STOP_BOUND, &mut stopping).await.is_ok();
        gate.add_permits(1);
        if !stopped_before_delivery {
            tokio::time::timeout(Duration::from_secs(10), stopping)
                .await
                .expect("stopping the check must finish");
        }
        let reached = served(&denied_hits).await;
        let (after, after_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!("fetch({after:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let unchecked_after_stop = served(&after_hits).await;
        drop(watch);
        drop((page, other));
        close(browser).await;
        assert!(
            judged,
            "{test_name}: the check must refuse the request before the stop, or the test shows nothing"
        );
        assert!(
            !stopped_before_delivery,
            "{test_name}: the stop must wait for the refusal it is delivering"
        );
        assert_eq!(
            reached_while_on, 0,
            "{test_name}: the tab's request must be refused while the check runs, or the probe after \
             the stop shows nothing"
        );
        assert!(
            !reached,
            "{test_name}: a request refused while the check stopped must not reach the denied address"
        );
        assert!(
            unchecked_after_stop,
            "{test_name}: once the check has stopped, the browser's tabs must reach the network again"
        );
    }

    /// Dropping the check without stopping it still delivers the refusal it is sending, then
    /// turns interception off, so the browser is not left paused with nothing answering.
    ///
    /// ~keep The delivery gate holds the refusal until the check and its watch are dropped, so the
    /// ~keep listener's commands close while it is still delivering. A tab opened outside the
    /// ~keep check after it started is the probe: on a launched browser its requests belong to no
    /// ~keep watch, so they are refused until interception is off, and reach the network after.
    /// ~keep The probe must not get through while the gate still holds the refusal.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_check_delivers_its_refusals_and_turns_interception_off() {
        let test_name = "dropping_the_check_delivers_its_refusals_and_turns_interception_off";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            deliver_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        // ~keep The tab stays on about:blank: on a launched browser the check refuses every
        // ~keep request of a page it does not watch, its navigation included.
        let other = browser.new_page("about:blank").await.expect("page");
        let (probe, probe_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!(
                "setInterval(() => fetch({probe:?}, {{ mode: 'no-cors' }}).catch(() => 0), 50); 1"
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reached_while_on = probe_hits.load(Ordering::SeqCst);
        gate.acquire().await.expect("the gate is never closed").forget();
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let judged = wait_for_refusal(&watch, &denied).await;
        drop(firewall);
        drop(watch);
        let off_before_delivery = served_within(&probe_hits, STOP_BOUND).await;
        gate.add_permits(1);
        let reached_after_drop = served(&probe_hits).await;
        drop((page, other));
        close(browser).await;
        assert!(
            judged,
            "{test_name}: the check must refuse the request before it is dropped, or the test shows nothing"
        );
        assert_eq!(
            reached_while_on, 0,
            "{test_name}: another tab's requests must be refused while interception is on"
        );
        assert!(
            !off_before_delivery,
            "{test_name}: interception must stay on until the refusal the check is delivering has reached Chrome"
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
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::Launched,
            PageContext::Isolated,
            TestDelays::default(),
        )
        .await
        .expect("the listener must start");
        let page = firewall.handle().new_page().await.expect("the check must open a page");
        let watch = firewall
            .handle()
            .watch(&page, &config(), 0)
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

    /// A page opened while the check is stopping is refused a watch, and is dropped, with a
    /// context of its own, before the stop returns, even when dropping it outlasts the rest of
    /// the drain: not left unchecked once interception turns off, in either context the check
    /// opens in.
    ///
    /// ~keep Chrome still lists a closed target for a while after it answers the close, so the
    /// ~keep test reads the listener's record of the pages it dropped, not Chrome's target list.
    /// ~keep The delivery gate holds a refusal in the drain, so the stop cannot finish before
    /// ~keep the late page is opened and watched: the stop is sent before the late page's
    /// ~keep `Opened` on the same channel, and the gate opens only once the watch has been
    /// ~keep answered. Two 100 ms sleeps held this before and failed their setup in 13 of 20
    /// ~keep runs on a loaded host. A second gate holds the late page's drop until the watch has
    /// ~keep been answered and the refusal let go, so the drop is the last thing the drain waits
    /// ~keep for, and the late page cannot be dropped before its open returns.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_watched_while_the_check_stops_is_refused() {
        let test_name = "a_page_watched_while_the_check_stops_is_refused";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        for context in [PageContext::Isolated, PageContext::Shared] {
            let gate = Arc::new(tokio::sync::Semaphore::new(1));
            let drop_gate = Arc::new(tokio::sync::Semaphore::new(0));
            let dropped = Arc::new(std::sync::Mutex::default());
            let delays = TestDelays {
                drop_late_gate: Some(Arc::clone(&drop_gate)),
                deliver_gate: Some(Arc::clone(&gate)),
                dropped: Arc::clone(&dropped),
                ..TestDelays::default()
            };
            let before = context_count(&browser).await;
            let (firewall, page, watch) = watched_page_in(&browser, context, delays).await;
            gate.acquire().await.expect("the gate is never closed").forget();
            let (denied, _hits) = denied_listener().await;
            let _ = page
                .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
                .await;
            assert!(
                wait_for_refusal(&watch, &denied).await,
                "{test_name} ({context:?}): the check must refuse the page's fetch and hold the refusal"
            );
            let handle = firewall.handle();
            let mut stopping = Box::pin(firewall.stop());
            assert!(
                futures::poll!(&mut stopping).is_pending(),
                "{test_name} ({context:?}): the stop must wait for the held refusal"
            );
            let stopping = tokio::spawn(stopping);
            let late = handle.new_page().await;
            let opened_while_stopping = !stopping.is_finished();
            let watched = match &late {
                Ok(late) => handle.watch(late, &config(), 0).await.map(drop),
                Err(error) => Err(crate::error::CrawlError::browser_error(error.to_string())),
            };
            gate.add_permits(1);
            drop_gate.add_permits(1);
            let _ = stopping.await;
            let late_target = late.as_ref().ok().map(|late| late.target_id().clone());
            let late_dropped = late_target
                .as_ref()
                .is_some_and(|target| super::lock(&dropped).contains(target));
            let after = context_count(&browser).await;
            drop((watch, page, late));
            assert!(
                late_target.is_some() && opened_while_stopping,
                "{test_name} ({context:?}): the late page must open while the check stops, or the test shows nothing"
            );
            let refusal = watched.err().map(|error| error.to_string()).unwrap_or_default();
            assert!(
                refusal.contains("request interception stopped"),
                "{test_name} ({context:?}): a watch that starts while the check stops must fail as stopped, got {refusal:?}"
            );
            assert!(
                late_dropped,
                "{test_name} ({context:?}): a page opened while the check stops must be dropped before the stop returns"
            );
            assert_eq!(
                after, before,
                "{test_name} ({context:?}): a context of the late page's own must be disposed when the stop returns"
            );
        }
        close(browser).await;
    }

    /// A page whose main frame cannot be read is not watched: the redirect limit could not be
    /// applied to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_without_a_main_frame_is_not_watched() {
        let test_name = "a_page_without_a_main_frame_is_not_watched";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::Launched,
            PageContext::Isolated,
            TestDelays::default(),
        )
        .await
        .expect("the listener must start");
        let page = firewall.handle().new_page().await.expect("the check must open a page");
        let closed = page.clone();
        let _ = page.close().await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(closed.mainframe().await, Ok(Some(_))) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let frame_gone = !matches!(closed.mainframe().await, Ok(Some(_)));
        let watched = firewall.handle().watch(&closed, &config(), 0).await;
        firewall.stop().await;
        assert!(
            frame_gone,
            "{test_name}: the closed page must stop reporting a main frame, or the test shows nothing"
        );
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

    /// Stopping the check does not let a parked page's pending request out: the page is gone
    /// with its context before interception turns off, so a request Chrome paused that the
    /// listener has not taken in yet reaches nothing.
    ///
    /// ~keep The receive gate holds the pause in the listener, as a busy host does, until the park
    /// ~keep and the stop are sent: Chrome has paused the request and the listener has not
    /// ~keep counted it when the stop comes. Turning interception off then would continue the
    /// ~keep request (measured 14 leaks in 15 loaded runs for a stop with the page open,
    /// ~keep xberg-io/crawlberg#484), and no wait on what the listener has received can see it.
    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_the_check_does_not_release_a_parked_pages_pending_request() {
        let test_name = "stopping_the_check_does_not_release_a_parked_pages_pending_request";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            receive_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let received = Arc::clone(&delays.received);
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.acquire().await.expect("the gate is never closed").forget();
        let target = page.target_id().clone();
        let mut paused = browser
            .event_listener::<super::EventRequestPaused>()
            .await
            .expect("the test must see the paused requests");
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let denied_paused = wait_for_pause(&mut paused, &denied).await;
        // ~keep Only the denied request counts: the page's favicon can be taken in before the
        // ~keep gate shuts, and still be in flight here.
        let denied_taken_in = super::lock(&received).contains(&denied);
        let mut parking = Box::pin(watch.park());
        let park_held = futures::poll!(&mut parking).is_pending();
        let mut stopping = Box::pin(firewall.stop());
        let stop_held = futures::poll!(&mut stopping).is_pending();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10), async {
            parking.await;
            stopping.await;
        })
        .await
        .expect("stopping the check must finish");
        let reached = served(&denied_hits).await;
        let still_open = target_is_open(&browser, &target).await;
        drop(page);
        close(browser).await;
        assert!(
            denied_paused && !denied_taken_in && park_held && stop_held,
            "{test_name}: Chrome must pause the request and the listener hold it until the park and the stop are sent, \
             or the test shows nothing (paused {denied_paused}, taken in {denied_taken_in}, park held {park_held}, \
             stop held {stop_held})"
        );
        assert!(
            !reached,
            "{test_name}: the parked page's pending request must not reach the denied address once the check stops"
        );
        assert!(!still_open, "{test_name}: the parked page must be gone with the check");
    }

    /// A page of the check that its owner closes without a watch, as the session pool closes a
    /// parked page it evicts, takes its browser context with it.
    ///
    /// ~keep chromiumoxide 0.9.1 flushes a browser-level event to its subscribers only while its
    /// ~keep handler iterates a live target (`Handler::poll_next` polls the event listeners inside
    /// ~keep the per-target loop), so on an otherwise idle browser the `targetDestroyed` of the
    /// ~keep closed page can sit undelivered until the next target activity (measured: 0 events in
    /// ~keep 1 of 12 loaded runs, and the dispose always runs when the event arrives). A second
    /// ~keep page, evaluated on each poll, keeps a target active so the event is delivered; in
    /// ~keep production a launched browser's other pages, or the check's stop, do the same.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_closed_pages_context_is_disposed() {
        let test_name = "a_closed_pages_context_is_disposed";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::Launched,
            PageContext::Isolated,
            TestDelays::default(),
        )
        .await
        .expect("the listener must start");
        let before = context_count(&browser).await;
        let keepalive = firewall.handle().new_page().await.expect("the check must open a page");
        let page = firewall.handle().new_page().await.expect("the check must open a page");
        let with_page = context_count(&browser).await;
        let target = page.target_id().clone();
        let _ = page.close().await;
        let closed_at = Instant::now();
        let mut page_open = true;
        let mut after = with_page;
        let mut disposed_after = None;
        for _ in 0..200 {
            // ~keep Keep a target active so chromiumoxide delivers the closed page's destroy event.
            let _ = keepalive.evaluate("1").await;
            page_open = target_is_open(&browser, &target).await;
            after = context_count(&browser).await;
            if after == before + 1 && !page_open {
                disposed_after = Some(closed_at.elapsed());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        firewall.stop().await;
        let after_stop = context_count(&browser).await;
        drop(keepalive);
        close(browser).await;
        assert_eq!(
            with_page,
            before + 2,
            "{test_name}: the check must open each page in a browser context of its own"
        );
        assert!(
            !page_open,
            "{test_name}: the closed page must be gone within twenty seconds, or the test shows nothing"
        );
        assert_eq!(
            after,
            before + 1,
            "{test_name}: the closed page's browser context must be disposed within twenty seconds, \
             leaving only the keepalive page's (disposed after {disposed_after:?}; \
             {after_stop} contexts after the stop, {before} before the pages)"
        );
        assert_eq!(
            after_stop, before,
            "{test_name}: the stop must dispose the keepalive page's context too"
        );
    }

    /// Opening a page once the check has stopped fails, and leaves no browser context behind.
    #[tokio::test(flavor = "multi_thread")]
    async fn opening_a_page_after_the_check_stopped_fails_and_leaves_no_context() {
        let test_name = "opening_a_page_after_the_check_stopped_fails_and_leaves_no_context";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let firewall = BrowserFirewall::start_with(
            Arc::clone(&browser),
            BrowserOrigin::Launched,
            PageContext::Isolated,
            TestDelays::default(),
        )
        .await
        .expect("the listener must start");
        let handle = firewall.handle();
        firewall.stop().await;
        let before = context_count(&browser).await;
        let opened = handle.new_page().await;
        let after = context_count(&browser).await;
        let error = opened.err().map(|error| error.to_string()).unwrap_or_default();
        close(browser).await;
        assert!(
            error.contains("request interception stopped"),
            "{test_name}: a page opened after the stop must be refused, got {error:?}"
        );
        assert_eq!(
            after, before,
            "{test_name}: a refused page must leave no browser context behind"
        );
    }

    /// A page of a shared-context check lives in the browser's own context, as a
    /// `browser_profile` needs: no context is created, the page reads the browser's cookies, and
    /// the cookies and localStorage it writes are in the browser for the next page.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_shared_context_page_reads_and_writes_the_browsers_own_storage() {
        let test_name = "a_shared_context_page_reads_and_writes_the_browsers_own_storage";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        set_cookie(&browser, None, "saved", "1").await;
        let before = context_count(&browser).await;
        let (firewall, page, watch) = watched_page_in(&browser, PageContext::Shared, TestDelays::default()).await;
        let with_page = context_count(&browser).await;
        // ~keep localStorage is per origin, port included, so the next page must load this URL.
        let site = page.url().await.ok().flatten().expect("the page must have a URL");
        let read: String = page
            .evaluate("localStorage.setItem('k', 'v'); document.cookie = 'session=2; max-age=3600'; document.cookie")
            .await
            .ok()
            .and_then(|value| value.into_value::<String>().ok())
            .unwrap_or_default();
        watch.close().await;
        let closed = !target_is_open(&browser, page.target_id()).await;
        let in_browser = cookie_names(&browser, None).await;
        let next = firewall.handle().new_page().await.expect("the check must open a page");
        let next_watch = firewall
            .handle()
            .watch(&next, &config(), 0)
            .await
            .expect("the watch must start");
        next.goto(site).await.expect("the test page must load");
        let stored: String = next
            .evaluate("localStorage.getItem('k') || 'none'")
            .await
            .ok()
            .and_then(|value| value.into_value::<String>().ok())
            .unwrap_or_default();
        next_watch.close().await;
        firewall.stop().await;
        drop((page, next));
        close(browser).await;
        assert_eq!(
            with_page, before,
            "{test_name}: the page must live in the browser's own context, not one of its own"
        );
        assert!(
            read.contains("saved=1"),
            "{test_name}: the page must read the browser's cookies, read {read:?}"
        );
        assert!(closed, "{test_name}: closing the watch must close the page");
        assert_eq!(
            in_browser,
            vec!["saved".to_owned(), "session".to_owned()],
            "{test_name}: the cookie the page set must be in the browser when its watch ends"
        );
        assert_eq!(
            stored, "v",
            "{test_name}: the localStorage the page wrote must reach the next page"
        );
    }

    /// Stopping a shared-context check leaves interception on: a request Chrome paused that the
    /// listener has not taken in when the stop comes reaches nothing, and a tab outside the check
    /// still has its requests paused after the stop. The browser still closes.
    ///
    /// ~keep The receive gate holds the pause in the listener, as a busy host does, until the park
    /// ~keep and the stop are sent. A 500 ms delay held it before, and on a loaded host the
    /// ~keep listener still took the request in before the park (2 of 30 runs). The tab
    /// ~keep outside the check is the probe that the check does not disable: its request is
    /// ~keep paused, seen by this test's own listener, only while interception is on. A check
    /// ~keep that disables still passed the no-reach assertion (3 of 3 runs), so the probe is
    /// ~keep the assertion that catches it.
    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_a_shared_context_check_leaves_interception_on() {
        let test_name = "stopping_a_shared_context_check_leaves_interception_on";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            receive_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let received = Arc::clone(&delays.received);
        let (firewall, page, watch) = watched_page_in(&browser, PageContext::Shared, delays).await;
        gate.acquire().await.expect("the gate is never closed").forget();
        let target = page.target_id().clone();
        let other = browser.new_page("about:blank").await.expect("page");
        let mut paused = browser
            .event_listener::<super::EventRequestPaused>()
            .await
            .expect("the test must see the paused requests");
        let (denied, denied_hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let denied_paused = wait_for_pause(&mut paused, &denied).await;
        // ~keep Only the denied request counts: the page's favicon can be taken in before the
        // ~keep gate shuts, and still be in flight here.
        let denied_taken_in = super::lock(&received).contains(&denied);
        let mut parking = Box::pin(watch.park());
        let park_held = futures::poll!(&mut parking).is_pending();
        let mut stopping = Box::pin(firewall.stop());
        let stop_held = futures::poll!(&mut stopping).is_pending();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10), async {
            parking.await;
            stopping.await;
        })
        .await
        .expect("stopping the check must finish");
        let reached = served(&denied_hits).await;
        let still_open = target_is_open(&browser, &target).await;
        let (probe, probe_hits) = denied_listener().await;
        let _ = other
            .evaluate(format!("fetch({probe:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let probe_paused = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = paused.next().await {
                if event.request.url == probe {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        let probe_reached = served(&probe_hits).await;
        drop((page, other));
        let closed = tokio::time::timeout(Duration::from_secs(20), close(browser)).await;
        assert!(
            denied_paused && !denied_taken_in && park_held && stop_held,
            "{test_name}: Chrome must pause the request and the listener hold it until the park and the stop are sent, \
             or the test shows nothing (paused {denied_paused}, taken in {denied_taken_in}, park held {park_held}, \
             stop held {stop_held})"
        );
        assert!(
            !reached,
            "{test_name}: the parked page's pending request must not reach the denied address once the check stops"
        );
        assert!(!still_open, "{test_name}: the parked page must be gone with the check");
        assert!(
            probe_paused,
            "{test_name}: a request from a tab outside the check must still be paused after the stop"
        );
        assert!(
            !probe_reached,
            "{test_name}: a request paused after the stop must reach nothing"
        );
        assert!(
            closed.is_ok(),
            "{test_name}: the browser must close with interception left on"
        );
    }

    /// With the browser's cookies shared but not saved, a page starts with them and its own die
    /// with its context; isolated, it starts with none.
    #[tokio::test(flavor = "multi_thread")]
    async fn shared_cookies_reach_the_page_and_unsaved_ones_die_with_it() {
        let test_name = "shared_cookies_reach_the_page_and_unsaved_ones_die_with_it";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        set_cookie(&browser, None, "owner", "1").await;
        let mut seen = Vec::new();
        for sharing in [PageContext::Copied, PageContext::Isolated] {
            let firewall = BrowserFirewall::start_with(
                Arc::clone(&browser),
                BrowserOrigin::Launched,
                sharing,
                TestDelays::default(),
            )
            .await
            .expect("the listener must start");
            let before = contexts(&browser).await;
            let page = firewall.handle().new_page().await.expect("the check must open a page");
            let context = contexts(&browser)
                .await
                .into_iter()
                .find(|context| !before.contains(context))
                .expect("the page must live in a context of its own");
            let in_page = cookie_names(&browser, Some(context.clone())).await;
            set_cookie(&browser, Some(context), "session", "2").await;
            let watch = firewall
                .handle()
                .watch(&page, &config(), 0)
                .await
                .expect("the watch must start");
            watch.close().await;
            firewall.stop().await;
            drop(page);
            seen.push((sharing, in_page, cookie_names(&browser, None).await));
        }
        close(browser).await;
        assert_eq!(
            seen,
            vec![
                (PageContext::Copied, vec!["owner".to_owned()], vec!["owner".to_owned()]),
                (PageContext::Isolated, vec![], vec!["owner".to_owned()]),
            ],
            "{test_name}: (sharing, cookies in the page, cookies in the browser after the watch)"
        );
    }

    /// A watch dropped without being closed, as by a cancelled fetch, closes its page.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_watch_closes_its_page() {
        let test_name = "a_dropped_watch_closes_its_page";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let (firewall, page, watch) = watched_page(&browser, TestDelays::default()).await;
        let target = page.target_id().clone();
        let open_while_watched = target_is_open(&browser, &target).await;
        drop(watch);
        let mut still_open = true;
        for _ in 0..50 {
            still_open = target_is_open(&browser, &target).await;
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
    ///
    /// ~keep The delivery gate holds the watched page's refusal until the stop has taken it in,
    /// ~keep so the stop has an answer of its own to wait for. The page that keeps sending is a
    /// ~keep tab outside the check: the stop does not drop it, so its requests keep arriving
    /// ~keep until interception is off. It starts sending only once the stop has been taken in,
    /// ~keep and a second gate holds each of its answers until the test ends, so a stop that
    /// ~keep waited for the later answers could never finish. The page's favicon request is
    /// ~keep usually held with the refusal, since Chrome sends it after the page loads (22 of 23
    /// ~keep runs held both), so the count is at least one, not exactly one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_that_keeps_sending_does_not_hold_the_stop_off() {
        let test_name = "a_page_that_keeps_sending_does_not_hold_the_stop_off";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let later = Arc::new(UrlGate {
            permits: tokio::sync::Semaphore::new(0),
            held: std::sync::Mutex::default(),
        });
        let delays = TestDelays {
            deliver_gate: Some(Arc::clone(&gate)),
            unwatched_deliver_gate: Some(Arc::clone(&later)),
            ..TestDelays::default()
        };
        let dropped = Arc::clone(&delays.dropped);
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.acquire().await.expect("the gate is never closed").forget();
        let target = page.target_id().clone();
        let other = browser.new_page("about:blank").await.expect("page");
        let (denied, _hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let judged = wait_for_refusal(&watch, &denied).await;
        let mut stopping = Box::pin(firewall.stop());
        let _ = futures::poll!(&mut stopping);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::lock(&dropped).contains(&target) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let taken_in = super::lock(&dropped).contains(&target);
        let held = watch.page.in_flight.load(Ordering::Acquire);
        let later_before_stop = super::lock(&later.held).len();
        let _ = other
            .evaluate(format!(
                "setInterval(() => fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0), 5); 1"
            ))
            .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while super::lock(&later.held).is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let later_held = !super::lock(&later.held).is_empty();
        gate.add_permits(1);
        let stopped = tokio::time::timeout(Duration::from_secs(10), &mut stopping)
            .await
            .is_ok();
        later.permits.add_permits(1);
        drop((stopping, watch, page, other));
        close(browser).await;
        assert!(
            judged && taken_in && held >= 1 && later_before_stop == 0 && later_held,
            "{test_name}: the stop must take in the page's held refusal, and only an answer the other tab gets after \
             the stop must be held, or the test shows nothing (refused {judged}, stop taken in {taken_in}, \
             held {held}, held before the stop {later_before_stop}, held after {later_held})"
        );
        assert!(
            stopped,
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
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delays = TestDelays {
            receive_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.acquire().await.expect("the gate is never closed").forget();
        let (denied, _hits) = denied_listener().await;
        let sent = Instant::now();
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        // ~keep The receive gate holds the pause until the sender's grace has ended and the next
        // ~keep action has begun; the sender's count is read only once the refusal is recorded.
        let mut for_the_sender = Box::pin(watch.refusal_during(sent, ACTION_GRACE));
        let _ = futures::poll!(&mut for_the_sender);
        tokio::time::sleep(ACTION_GRACE).await;
        let next = Instant::now();
        gate.add_permits(1);
        let judged = wait_for_refusal(&watch, &denied).await;
        let for_the_sender = for_the_sender.await;
        let for_the_next = watch.refusal_during(next, ACTION_GRACE).await;
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            judged,
            "{test_name}: the check must refuse the page's fetch, or the test shows nothing"
        );
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
    ///
    /// ~keep The verdict gate holds the fetch before its verdict until the read has started, so
    /// ~keep a busy host cannot judge it first.
    #[cfg(feature = "browser")]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_refused_urls_include_a_request_still_being_judged() {
        let test_name = "the_refused_urls_include_a_request_still_being_judged";
        let Some(browser) = launch(test_name).await else {
            return;
        };
        let gate = Arc::new(UrlGate {
            permits: tokio::sync::Semaphore::new(1),
            held: std::sync::Mutex::default(),
        });
        let delays = TestDelays {
            verdict_gate: Some(Arc::clone(&gate)),
            ..TestDelays::default()
        };
        let (firewall, page, watch) = watched_page(&browser, delays).await;
        gate.permits.acquire().await.expect("the gate is never closed").forget();
        let (denied, _hits) = denied_listener().await;
        let _ = page
            .evaluate(format!("fetch({denied:?}, {{ mode: 'no-cors' }}).catch(() => 0); 1"))
            .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::lock(&gate.held).contains(&denied) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let held = super::lock(&gate.held).contains(&denied);
        let (waited, refused) = {
            let mut reading = Box::pin(watch.refused_urls());
            let first = futures::poll!(&mut reading);
            gate.permits.add_permits(1);
            match first {
                std::task::Poll::Ready(refused) => (false, refused),
                std::task::Poll::Pending => (true, reading.await),
            }
        };
        watch.close().await;
        firewall.stop().await;
        drop(page);
        close(browser).await;
        assert!(
            held,
            "{test_name}: the check must hold the fetch before its verdict, or the test shows nothing"
        );
        assert!(
            waited,
            "{test_name}: reading the refused URLs must wait for the request being judged"
        );
        assert_eq!(
            refused,
            [denied],
            "{test_name}: the request being judged must be listed"
        );
    }
}
